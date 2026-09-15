//! A debugging overlay that draws boxes on the screen.
//!
//! The implementation of `highlight()` from
//! [Rhai API architecture](../../../docs/architecture/rhai-api.md). It exists so you can see with
//! your own eyes what is currently matching; it plays no part in the automation
//! itself.
//!
//! The OS-dependent part is split the same way as in `mekiki-capture` /
//! `mekiki-input`: the [`Overlay`] trait is the boundary and the implementation
//! stays inside `windows_*.rs`.
//!
//! [`Overlay::show`] draws a single box and blocks (the script's `highlight()`).
//! [`Overlay::show_marks`] draws several boxes plus labels on a dedicated thread
//! and returns immediately (the IDE's match preview).

use std::fmt;
use std::time::Duration;

#[cfg(windows)]
mod windows_overlay;

#[derive(Debug)]
pub enum OverlayError {
    Unsupported(&'static str),
    Os(String),
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(p) => write!(f, "the overlay is not implemented for {p}"),
            Self::Os(e) => write!(f, "an OS API call failed: {e}"),
        }
    }
}

impl std::error::Error for OverlayError {}

/// Box colour (RGB).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    pub const RED: Color = Color(255, 0, 0);
    pub const GREEN: Color = Color(0, 200, 0);
}

/// How a box looks and how long it stays up.
#[derive(Copy, Clone, Debug)]
pub struct Style {
    pub color: Color,
    /// Border thickness in pixels. The box is drawn **outside** the rectangle,
    /// so it never covers the target.
    pub thickness: u32,
    pub duration: Duration,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: Color::RED,
            thickness: 3,
            duration: Duration::from_millis(1000),
        }
    }
}

/// One box drawn on the desktop. Coordinates are physical pixels in virtual
/// desktop space.
#[derive(Clone, Debug)]
pub struct Mark {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Text shown next to the box. In the match preview this is the score
    /// (`0.9823`).
    pub label: Option<String>,
}

/// The extent of the virtual screen, used to flip labels at the edges.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ScreenBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// The gap between the box and the score chip.
pub const LABEL_GAP: i32 = 3;

/// Format a similarity score to the same 4 decimals as the preview.
pub fn format_score(score: f32) -> String {
    format!("{score:.4}")
}

/// Top-left of the score chip in screen coordinates. By default it sits above
/// the box, left aligned, and flips at whichever edge it would overflow.
pub fn place_label(
    mark: &Mark,
    thickness: u32,
    chip_w: i32,
    chip_h: i32,
    screen: ScreenBounds,
) -> (i32, i32) {
    let t = thickness.max(1) as i32;
    let right = screen.x.saturating_add(screen.width);
    let bottom = screen.y.saturating_add(screen.height);

    let mut x = mark.x;
    let mut y = mark.y - t - LABEL_GAP - chip_h;

    if y < screen.y {
        y = mark.y + mark.height as i32 + t + LABEL_GAP;
    }
    if y + chip_h > bottom {
        y = bottom - chip_h;
    }
    if y < screen.y {
        y = screen.y;
    }

    if x + chip_w > right {
        x = right - chip_w;
    }
    if x < screen.x {
        x = screen.x;
    }
    (x, y)
}

pub trait Overlay: Send {
    /// Show a box around the given rectangle.
    ///
    /// **Blocks the caller for `style.duration`**, because the overlay window's
    /// messages have to keep being pumped. This trade-off is acceptable on the
    /// premise that it is only for debugging.
    fn show(
        &mut self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        style: Style,
    ) -> Result<(), OverlayError>;

    /// Show several boxes with labels. Does not block the caller.
    ///
    /// They disappear after `style.duration`, and a later call replaces them.
    /// An empty slice clears them. The default implementation is unsupported.
    fn show_marks(&mut self, marks: &[Mark], style: Style) -> Result<(), OverlayError> {
        let _ = (marks, style);
        Err(OverlayError::Unsupported("multi-box overlays"))
    }

    /// Immediately clear the boxes drawn by [`show_marks`].
    fn hide_marks(&mut self) -> Result<(), OverlayError> {
        Ok(())
    }

    fn backend_name(&self) -> &'static str;
}

/// Open the default backend for this platform.
pub fn open() -> Result<Box<dyn Overlay>, OverlayError> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows_overlay::LayeredOverlay::new()))
    }
    #[cfg(not(windows))]
    {
        Err(OverlayError::Unsupported("this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> ScreenBounds {
        ScreenBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }
    }

    fn mark(x: i32, y: i32, w: u32, h: u32) -> Mark {
        Mark {
            x,
            y,
            width: w,
            height: h,
            label: Some("0.9900".into()),
        }
    }

    #[test]
    fn score_uses_four_decimals() {
        assert_eq!(format_score(1.0), "1.0000");
        assert_eq!(format_score(0.9), "0.9000");
        assert_eq!(format_score(0.5).len(), 6);
    }

    #[test]
    fn label_sits_above_by_default() {
        let m = mark(100, 200, 80, 40);
        let (x, y) = place_label(&m, 3, 60, 18, screen());
        assert_eq!(x, 100);
        assert_eq!(y, 200 - 3 - LABEL_GAP - 18);
    }

    #[test]
    fn label_flips_below_near_top() {
        let m = mark(100, 5, 80, 40);
        let (_, y) = place_label(&m, 3, 60, 18, screen());
        assert_eq!(y, 5 + 40 + 3 + LABEL_GAP);
    }

    #[test]
    fn label_shifts_left_near_right_edge() {
        let m = mark(1900, 200, 20, 20);
        let (x, _) = place_label(&m, 3, 80, 18, screen());
        assert_eq!(x, 1920 - 80);
    }

    #[test]
    fn label_clamps_to_negative_origin() {
        let screen = ScreenBounds {
            x: -1920,
            y: 0,
            width: 3840,
            height: 1080,
        };
        let m = mark(-2000, 10, 40, 20);
        let (x, y) = place_label(&m, 3, 80, 18, screen);
        assert_eq!(x, -1920);
        assert!(y >= 0);
    }
}
