//! Turning captured frames into something safe to hand an agent.
//!
//! # Why downscaling is not optional
//!
//! A 4K screenshot is roughly 8 megapixels. Base64-encoded into a JSON-RPC
//! response it becomes several megabytes of context, and an agent that takes
//! three of them has spent its window on pictures of a screen it could have
//! read as text. `read_text` costs a fraction of that and returns coordinates
//! besides.
//!
//! So [`downscale_to_width`] runs by default and the tool description points at
//! `read_text` first. The agent can still ask for full resolution when it
//! genuinely needs to look at pixels.

use image::{ImageBuffer, Rgba, RgbaImage};

/// Encode a BGRA frame as PNG.
pub fn frame_to_png(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let img = to_rgba(bgra, width, height)?;
    encode_png(&img)
}

/// Encode a BGRA frame as PNG, shrinking it to at most `max_width` first.
///
/// Returns the PNG and the size actually encoded, which is what the agent needs
/// to map a pixel it sees back to a screen coordinate.
pub fn frame_to_png_scaled(
    bgra: &[u8],
    width: u32,
    height: u32,
    max_width: Option<u32>,
) -> Result<(Vec<u8>, u32, u32), String> {
    let img = to_rgba(bgra, width, height)?;
    let img = match max_width {
        Some(max) => downscale_to_width(img, max),
        None => img,
    };
    let (w, h) = (img.width(), img.height());
    Ok((encode_png(&img)?, w, h))
}

/// Shrink so the width is at most `max_width`, preserving aspect ratio.
///
/// Already-small images are returned untouched: upscaling would cost bytes and
/// add nothing.
pub fn downscale_to_width(img: RgbaImage, max_width: u32) -> RgbaImage {
    let max_width = max_width.max(1);
    if img.width() <= max_width {
        return img;
    }
    let height = ((img.height() as u64 * max_width as u64) / img.width() as u64).max(1) as u32;
    // Triangle rather than nearest: text stays legible when an agent does look
    // at the picture, which is the only reason to send one.
    image::imageops::resize(
        &img,
        max_width,
        height,
        image::imageops::FilterType::Triangle,
    )
}

fn to_rgba(bgra: &[u8], width: u32, height: u32) -> Result<RgbaImage, String> {
    let expected = width as usize * height as usize * 4;
    if bgra.len() < expected {
        return Err(format!(
            "the frame is short: {} bytes for {width}x{height} (expected {expected})",
            bgra.len()
        ));
    }
    let (pixels, _) = bgra[..expected].as_chunks::<4>();
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| [p[2], p[1], p[0], 255])
        .collect();
    ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba)
        .ok_or_else(|| "the pixel count does not match the dimensions".to_string())
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("cannot encode PNG: {e}"))?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .flat_map(|i| {
                let v = (i % 251) as u8;
                [v, v.wrapping_add(40), v.wrapping_add(80), 255]
            })
            .collect()
    }

    #[test]
    fn bgra_channels_land_in_the_right_order() {
        // One pixel: blue=10, green=20, red=30.
        let img = to_rgba(&[10, 20, 30, 255], 1, 1).unwrap();
        assert_eq!(img.get_pixel(0, 0).0, [30, 20, 10, 255]);
    }

    #[test]
    fn a_short_frame_is_reported_not_panicked() {
        let err = to_rgba(&[0, 0, 0, 255], 4, 4).unwrap_err();
        assert!(err.contains("short"), "{err}");
    }

    #[test]
    fn downscaling_preserves_aspect_ratio() {
        let img = to_rgba(&frame(800, 400), 800, 400).unwrap();
        let small = downscale_to_width(img, 200);
        assert_eq!((small.width(), small.height()), (200, 100));
    }

    #[test]
    fn small_images_are_left_alone() {
        let img = to_rgba(&frame(64, 32), 64, 32).unwrap();
        let same = downscale_to_width(img, 1280);
        assert_eq!((same.width(), same.height()), (64, 32), "must not upscale");
    }

    /// The reported size has to be the size actually encoded, or the agent maps
    /// pixels back to the wrong screen coordinates.
    #[test]
    fn scaled_encoding_reports_the_encoded_size() {
        let (png, w, h) = frame_to_png_scaled(&frame(640, 480), 640, 480, Some(320)).unwrap();
        assert_eq!((w, h), (320, 240));

        let decoded = image::load_from_memory(&png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (320, 240));
    }

    #[test]
    fn unscaled_encoding_keeps_the_original_size() {
        let (png, w, h) = frame_to_png_scaled(&frame(120, 80), 120, 80, None).unwrap();
        assert_eq!((w, h), (120, 80));
        assert!(!png.is_empty());
    }
}
