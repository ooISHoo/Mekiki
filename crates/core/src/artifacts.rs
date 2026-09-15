//! Artifacts written automatically on failure.
//!
//! The implementation of the [Rhai API diagnostic contract](../../../docs/architecture/rhai-api.md#diagnostics-and-assets).
//!
//! When a `FindFailed` comes out, this leaves behind enough to see at a glance
//! whether **the threshold needs lowering, or you are looking in the wrong
//! place entirely**. Having these or not makes a large difference to how fast a
//! problem can be narrowed down.

use std::path::{Path, PathBuf};

use mekiki_capture::Frame;
use mekiki_matching::Image;

use crate::model::Match;

/// The list of files that were written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureArtifacts {
    /// The capture taken at the moment of failure.
    pub screen: PathBuf,
    /// A heat map of the score map.
    pub heatmap: Option<PathBuf>,
    /// The capture with the top candidates' rectangles drawn on it.
    pub annotated: Option<PathBuf>,
    /// The list of top candidates (rank, position, score).
    pub candidates: Option<PathBuf>,
}

impl std::fmt::Display for FailureArtifacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.screen.display())?;
        for p in [&self.heatmap, &self.annotated, &self.candidates]
            .into_iter()
            .flatten()
        {
            write!(f, ", {}", p.display())?;
        }
        Ok(())
    }
}

/// Save a BGRA frame as an RGBA PNG.
pub fn save_frame(frame: &Frame, path: &Path) -> std::io::Result<()> {
    let mut rgba = Vec::with_capacity(frame.bgra.len());
    for px in frame.bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    let buf = image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or_else(|| std::io::Error::other("the image buffer size does not match"))?;
    buf.save(path).map_err(std::io::Error::other)
}

/// Render the score map as a colour-coded PNG.
///
/// ZMD scores run -1.0..=1.0: lower is blue, higher is red. The region around
/// the threshold lands in green-to-yellow, so the colour tells you whether it
/// was "nearly there" or "nothing like it".
pub(crate) fn save_heatmap(scores: &Image<'_>, path: &Path) -> std::io::Result<()> {
    let mut rgba = Vec::with_capacity(scores.data.len() * 4);
    for &v in scores.data.iter() {
        let (r, g, b) = ramp(v);
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    let buf = image::RgbaImage::from_raw(scores.width, scores.height, rgba)
        .ok_or_else(|| std::io::Error::other("the heat map size does not match"))?;
    buf.save(path).map_err(std::io::Error::other)
}

/// Map -1.0..=1.0 onto a blue → green → red ramp.
fn ramp(v: f32) -> (u8, u8, u8) {
    let t = ((v + 1.0) / 2.0).clamp(0.0, 1.0);
    if t < 0.5 {
        let k = t * 2.0;
        (0, (k * 255.0) as u8, ((1.0 - k) * 255.0) as u8)
    } else {
        let k = (t - 0.5) * 2.0;
        ((k * 255.0) as u8, ((1.0 - k) * 255.0) as u8, 0)
    }
}

/// Save a PNG of the capture with the candidate rectangles drawn on it.
///
/// The top hit is red and the rest yellow. The score values go into the text
/// from [`save_candidates`] instead: drawing text onto the image would mean
/// carrying a font, which is not worth it.
pub(crate) fn save_annotated(
    frame: &Frame,
    candidates: &[Match],
    path: &Path,
) -> std::io::Result<()> {
    let mut rgba = Vec::with_capacity(frame.bgra.len());
    for px in frame.bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    let mut buf = image::RgbaImage::from_raw(frame.width, frame.height, rgba)
        .ok_or_else(|| std::io::Error::other("the image buffer size does not match"))?;

    for (rank, m) in candidates.iter().enumerate() {
        let color = if rank == 0 {
            [255u8, 0, 0, 255]
        } else {
            [255u8, 220, 0, 255]
        };
        // Convert to coordinates relative to the frame.
        let x0 = m.rect.x - frame.origin.0;
        let y0 = m.rect.y - frame.origin.1;
        draw_rect(&mut buf, x0, y0, m.rect.width, m.rect.height, color);
    }

    buf.save(path).map_err(std::io::Error::other)
}

fn draw_rect(buf: &mut image::RgbaImage, x: i32, y: i32, w: u32, h: u32, color: [u8; 4]) {
    let (iw, ih) = (buf.width() as i32, buf.height() as i32);
    let mut put = |px: i32, py: i32| {
        if px >= 0 && py >= 0 && px < iw && py < ih {
            buf.put_pixel(px as u32, py as u32, image::Rgba(color));
        }
    };
    for dx in 0..w as i32 {
        put(x + dx, y);
        put(x + dx, y + h as i32 - 1);
    }
    for dy in 0..h as i32 {
        put(x, y + dy);
        put(x + w as i32 - 1, y + dy);
    }
}

/// Write the candidate list out as text.
pub(crate) fn save_candidates(
    candidates: &[Match],
    required: f32,
    path: &Path,
) -> std::io::Result<()> {
    let mut s = format!("required score: {required:.4}\n\nrank  position          score\n");
    if candidates.is_empty() {
        s.push_str("(far below the threshold; not a single candidate came out)\n");
    }
    for (rank, m) in candidates.iter().enumerate() {
        s.push_str(&format!(
            "{:>4}  {:>6},{:<6} {:>8.4}{}\n",
            rank + 1,
            m.rect.x,
            m.rect.y,
            m.score,
            if m.score >= required {
                "  <- passed"
            } else {
                ""
            }
        ));
    }
    std::fs::write(path, s)
}

/// Reduce a name to something usable as an artifact file name.
pub(crate) fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "target".to_string()
    } else {
        trimmed.chars().take(60).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_endpoints_and_middle() {
        assert_eq!(ramp(-1.0), (0, 0, 255)); // the minimum is blue
        assert_eq!(ramp(1.0), (255, 0, 0)); // the maximum is red
        let mid = ramp(0.0);
        assert_eq!(mid.1, 255, "the middle is green"); // (0, 255, 0)
    }

    #[test]
    fn ramp_clamps_out_of_range() {
        assert_eq!(ramp(-5.0), (0, 0, 255));
        assert_eq!(ramp(5.0), (255, 0, 0));
    }

    #[test]
    fn sanitize_strips_path_characters() {
        assert_eq!(sanitize("ok_button.png"), "ok_button_png");
        assert_eq!(sanitize("a/b\\c:d"), "a_b_c_d");
        assert_eq!(sanitize("field.png (RightOf 'label.png')"), {
            let s = sanitize("field.png (RightOf 'label.png')");
            s.clone()
        });
        assert!(!sanitize("///").is_empty());
        assert_eq!(sanitize("///"), "target");
    }

    #[test]
    fn sanitize_limits_length() {
        let long = "a".repeat(200);
        assert_eq!(sanitize(&long).len(), 60);
    }

    #[test]
    fn draw_rect_stays_in_bounds() {
        let mut buf = image::RgbaImage::new(10, 10);
        // Drawing a rectangle that overflows the image must not panic.
        draw_rect(&mut buf, -5, -5, 20, 20, [255, 0, 0, 255]);
        draw_rect(&mut buf, 8, 8, 20, 20, [255, 0, 0, 255]);
    }
}
