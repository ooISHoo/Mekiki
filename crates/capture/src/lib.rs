//! The screen capture abstraction and its per-OS backends.
//!
//! Phase 1-1 of the development plan. **Windows is the only target platform**,
//! and the implementation is DXGI Desktop Duplication (with a GDI fallback).
//!
//! # Why abstract at all when this is Windows only
//!
//! The [`ScreenCapture`] trait is the boundary with the OS-dependent part.
//! Concrete implementations stay inside `windows_*.rs` and nothing above
//! (`mekiki-core` and up) refers to them.
//!
//! Supporting other operating systems is out of scope, but the boundary is kept
//! deliberately.
//!
//! - If support is added later, an implementation can be dropped in without
//!   touching the upper layers.
//! - It still compiles on non-Windows ([`open`] returns `Unsupported`). Keeping
//!   that true in CI detects OS-specific details leaking upwards.
//!
//! As the plan intended, this starts with a naive CPU path. Going straight from
//! the GPU texture (zero copy) is deferred to Phase 4-3.
//!
//! # Coordinate system
//!
//! Everything is in **physical pixels in virtual desktop space**. The process
//! is per-monitor DPI aware, so these may not agree with logical pixels (after
//! DPI scaling). On a multi-monitor setup the origin is the top-left of the
//! primary monitor, and monitors to the left or above have negative
//! coordinates.

use std::fmt;

#[cfg(windows)]
mod windows_dxgi;
#[cfg(windows)]
mod windows_gdi;
#[cfg(windows)]
mod windows_window;

#[cfg(windows)]
pub use windows_gdi::GdiCapture;

/// A rectangle. The position is in virtual desktop coordinates and the size is
/// in physical pixels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> i32 {
        self.x + self.width as i32
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height as i32
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Intersection. `None` when they do not overlap.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            return None;
        }
        Some(Rect::new(x, y, (right - x) as u32, (bottom - y) as u32))
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}+{}+{}", self.width, self.height, self.x, self.y)
    }
}

/// Information about one top-level window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    /// The title bar string.
    ///
    /// **This is not an identifier.** It carries the name of the file being
    /// edited and unsaved markers such as `*`, so trying to pin down a window
    /// with it alone is fragile. For details and how other tools deal with it,
    /// see
    /// [window locator design](../../../docs/architecture/window-locators.md).
    pub title: String,
    /// The window class.
    ///
    /// Stable for plain Win32 applications (Notepad is always `Notepad`).
    /// There are two quirks though.
    ///
    /// - Chromium/Electron apps are all `Chrome_WidgetWin_1`, so a browser and
    ///   an Electron editor cannot be told apart.
    /// - WPF uses `HwndWrapper[App.exe;;<GUID>]`, where **the tail changes on
    ///   every launch**.
    ///
    /// Because of that, matching is by prefix ([`WindowQuery::class_name`]).
    pub class_name: String,
    /// The executable name (`notepad.exe`). Empty when it cannot be read.
    ///
    /// In practice this was the only thing that could separate Chromium-based
    /// applications.
    pub exe: String,
    /// The owning process ID.
    pub pid: u32,
    /// The visible rectangle (virtual desktop coordinates).
    pub bounds: Rect,
    /// Z order. 0 is frontmost.
    pub z_order: usize,
    /// The OS window handle, used to bring it to the front. 0 when unset.
    pub hwnd: isize,
}

/// Conditions for narrowing down a window.
///
/// Only what you specify becomes a condition, and they all combine with AND.
/// The shape follows AutoHotkey's `ahk_class` / `ahk_exe` and UiPath's
/// `<wnd app cls title>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowQuery {
    /// Title. `*` (zero or more characters) and `?` (exactly one) are allowed.
    ///
    /// **Without any wildcard it is treated as a substring match.** It is more
    /// natural for `title=Minutes` to mean "a window whose title contains
    /// Minutes"; when the position matters you can write
    /// `title=* - Notepad` instead.
    pub title: Option<String>,
    /// Exact title match. Mutually exclusive with `title`, and takes priority.
    pub title_exact: Option<String>,
    /// Window class. **Prefix match.**
    ///
    /// A WPF class name carries a per-launch GUID at the end, so an exact match
    /// is unusable. `class=HwndWrapper[App.exe` is enough.
    pub class_name: Option<String>,
    /// Executable name. Exact match, ignoring case.
    pub exe: Option<String>,
    pub pid: Option<u32>,
    /// Which one to take when several match (0 based, in Z order).
    pub index: usize,
}

impl WindowQuery {
    /// A query consisting only of a title substring match.
    pub fn title_contains(pattern: impl Into<String>) -> Self {
        Self {
            title: Some(pattern.into()),
            ..Default::default()
        }
    }

    /// Whether it carries no condition at all. Every window would match, so the
    /// caller rejects it.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.title_exact.is_none()
            && self.class_name.is_none()
            && self.exe.is_none()
            && self.pid.is_none()
    }

    /// Whether this window satisfies the conditions.
    pub fn matches(&self, w: &WindowInfo) -> bool {
        if let Some(t) = &self.title_exact
            && &w.title != t
        {
            return false;
        }
        if let Some(pattern) = &self.title
            && !title_matches(pattern, &w.title)
        {
            return false;
        }
        if let Some(c) = &self.class_name
            && !w.class_name.starts_with(c.as_str())
        {
            return false;
        }
        if let Some(e) = &self.exe
            && !w.exe.eq_ignore_ascii_case(e)
        {
            return false;
        }
        if let Some(p) = self.pid
            && w.pid != p
        {
            return false;
        }
        true
    }

    /// A human-readable description, used in error messages.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(t) = &self.title_exact {
            parts.push(format!("title(exact)={t}"));
        }
        if let Some(t) = &self.title {
            parts.push(format!("title={t}"));
        }
        if let Some(c) = &self.class_name {
            parts.push(format!("class={c}"));
        }
        if let Some(e) = &self.exe {
            parts.push(format!("exe={e}"));
        }
        if let Some(p) = self.pid {
            parts.push(format!("pid={p}"));
        }
        if self.index > 0 {
            parts.push(format!("index={}", self.index));
        }
        if parts.is_empty() {
            "(no conditions)".to_string()
        } else {
            parts.join(",")
        }
    }
}

/// Title matching.
///
/// A pattern without wildcards is a substring match; with them it must match
/// the whole string. The former is a substring match so that the long-standing
/// `window("Notepad")` spelling keeps working.
fn title_matches(pattern: &str, title: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return title.contains(pattern);
    }
    glob_match(
        &pattern.chars().collect::<Vec<_>>(),
        &title.chars().collect::<Vec<_>>(),
    )
}

/// Glob matching with `*` and `?` only.
///
/// Regular expressions are avoided both to keep the dependency list short and
/// because the rules shown to users should be explainable in two characters.
/// The implementation backtracks naively, which is fine for window titles.
fn glob_match(pattern: &[char], text: &[char]) -> bool {
    // Once the pattern is exhausted, it matches only if nothing is left.
    let Some((&head, rest)) = pattern.split_first() else {
        return text.is_empty();
    };

    match head {
        '*' => {
            // Match zero or more. Try each split point from the front.
            (0..=text.len()).any(|i| glob_match(rest, &text[i..]))
        }
        '?' => !text.is_empty() && glob_match(rest, &text[1..]),
        c => match text.split_first() {
            Some((&t, tail)) if t == c => glob_match(rest, tail),
            _ => false,
        },
    }
}

/// Enumerate visible top-level windows in Z order (frontmost first).
///
/// Minimised windows and windows without a title are excluded. The former have
/// a fixed off-screen rectangle and are meaningless as a scope; the latter are
/// mostly tool windows and invisible shell elements.
pub fn windows() -> Result<Vec<WindowInfo>, CaptureError> {
    #[cfg(windows)]
    {
        windows_window::enumerate()
    }
    #[cfg(not(windows))]
    {
        Err(CaptureError::Unsupported("this platform"))
    }
}

/// Look up a window by its conditions. On multiple matches, the
/// [`WindowQuery::index`]-th one.
pub fn find_window_by(query: &WindowQuery) -> Result<WindowInfo, CaptureError> {
    windows()?
        .into_iter()
        .filter(|w| query.matches(w))
        .nth(query.index)
        .ok_or_else(|| CaptureError::NoSuchWindow(query.describe()))
}

/// Look up a window by a title substring. On multiple matches, the frontmost.
pub fn find_window(title_contains: &str) -> Result<WindowInfo, CaptureError> {
    find_window_by(&WindowQuery::title_contains(title_contains))
}

/// Look up a window by exact title. On multiple matches, the frontmost.
pub fn find_window_exact(title: &str) -> Result<WindowInfo, CaptureError> {
    find_window_by(&WindowQuery {
        title_exact: Some(title.to_string()),
        ..Default::default()
    })
}

/// A window's application icon (BGRA, 32px per side). `None` if unavailable.
pub fn window_icon_bgra(hwnd: isize) -> Option<(u32, u32, Vec<u8>)> {
    #[cfg(windows)]
    {
        windows_window::icon_bgra(hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        None
    }
}

/// Bring the window matching the conditions to the front. Does not restore a
/// minimised window.
pub fn activate_window(query: &WindowQuery) -> Result<(), CaptureError> {
    #[cfg(windows)]
    {
        windows_window::activate(query)
    }
    #[cfg(not(windows))]
    {
        let _ = query;
        Err(CaptureError::Unsupported("this platform"))
    }
}

/// Bring a previously resolved native window to the front.
///
/// Unlike [`activate_window`], this does not enumerate windows again. It is for
/// a script `Region` that already owns a stable HWND and must keep addressing
/// the same window even after activation changes the Z order.
pub fn activate_window_handle(hwnd: isize) -> Result<(), CaptureError> {
    #[cfg(windows)]
    {
        windows_window::activate_handle(hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        Err(CaptureError::Unsupported("this platform"))
    }
}

/// Information about one display.
#[derive(Clone, Debug)]
pub struct DisplayInfo {
    /// The index within [`ScreenCapture::displays`]. Also the ID passed to
    /// `capture`.
    pub index: usize,
    pub name: String,
    /// Position and size in virtual desktop coordinates.
    pub bounds: Rect,
    pub is_primary: bool,
}

/// One captured frame.
///
/// Pixels are BGRA8, tightly packed (no row padding). Both Windows DXGI and
/// macOS hand back BGRA, so that is the common format here.
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Where the top-left of this frame sits in virtual desktop coordinates.
    pub origin: (i32, i32),
    /// BGRA8, `width * height * 4` bytes.
    pub bgra: Vec<u8>,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("origin", &self.origin)
            .field("bytes", &self.bgra.len())
            .finish()
    }
}

impl Frame {
    pub fn bounds(&self) -> Rect {
        Rect::new(self.origin.0, self.origin.1, self.width, self.height)
    }

    /// Convert to `0.0..=1.0` greyscale using the BT.601 luma formula.
    ///
    /// The coefficients match OpenCV `cvtColor(BGR2GRAY)`. As long as the
    /// golden tests are anchored to OpenCV, matching here keeps the scores from
    /// drifting.
    ///
    /// # Why this is parallelised
    ///
    /// At 4K this walks 8 million pixels. Profiling showed 15ms single
    /// threaded, **close to 30% of the whole search**. The computation is
    /// independent per pixel, so splitting by row parallelises directly.
    pub fn to_luma_f32(&self) -> Vec<f32> {
        use rayon::prelude::*;

        let width = self.width as usize;
        let height = self.height as usize;
        let mut out = vec![0.0f32; width * height];

        out.par_chunks_mut(width)
            .zip(self.bgra.par_chunks(width * 4))
            .for_each(|(row_out, row_bgra)| {
                for (dst, px) in row_out.iter_mut().zip(row_bgra.chunks_exact(4)) {
                    let b = f32::from(px[0]);
                    let g = f32::from(px[1]);
                    let r = f32::from(px[2]);
                    *dst = (0.114 * b + 0.587 * g + 0.299 * r) / 255.0;
                }
            });

        out
    }

    /// Crop a rectangle out of the frame. `rect` is in virtual desktop
    /// coordinates.
    ///
    /// When it extends past the frame, only the intersection is returned.
    /// `None` when there is no overlap.
    pub fn crop(&self, rect: Rect) -> Option<Frame> {
        let clipped = self.bounds().intersect(&rect)?;
        let x0 = (clipped.x - self.origin.0) as usize;
        let y0 = (clipped.y - self.origin.1) as usize;
        let w = clipped.width as usize;
        let h = clipped.height as usize;

        let mut bgra = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            let start = ((y0 + row) * self.width as usize + x0) * 4;
            bgra.extend_from_slice(&self.bgra[start..start + w * 4]);
        }

        Some(Frame {
            width: clipped.width,
            height: clipped.height,
            origin: (clipped.x, clipped.y),
            bgra,
        })
    }
}

#[derive(Debug)]
pub enum CaptureError {
    /// No backend is implemented for this platform.
    Unsupported(&'static str),
    /// No display exists at the given index.
    NoSuchDisplay(usize),
    /// No window matches the conditions.
    NoSuchWindow(String),
    /// The OS refused to bring the window to the front.
    ActivateFailed(String),
    /// The window exists but a modal dialog it owns holds the input.
    ///
    /// Activating the owner would report success while every keystroke lands
    /// in the dialog — and in a message box, letters are button accelerators.
    /// Failing loudly here is what turns that from a silent misfire into a
    /// diagnosable one.
    ModalOpen {
        /// The window that was asked for.
        window: String,
        /// The dialog that actually holds the input. Empty when its title
        /// could not be read.
        dialog: String,
    },
    /// Not a single display was found.
    NoDisplays,
    /// The capture session became invalid, e.g. because the display
    /// configuration changed. Calling again retries establishing it.
    SessionLost(String),
    /// An OS API call failed.
    Os(String),
    /// No new frame arrived within the time limit.
    Timeout,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(p) => write!(f, "screen capture is not implemented for {p}"),
            Self::NoSuchDisplay(i) => write!(f, "display {i} does not exist"),
            Self::NoSuchWindow(t) => write!(f, "no window matches '{t}'"),
            Self::ActivateFailed(t) => write!(f, "cannot bring '{t}' to the front"),
            Self::ModalOpen { window, dialog } => {
                if dialog.is_empty() {
                    write!(
                        f,
                        "'{window}' is blocked by a modal dialog; target the dialog instead, or dismiss it"
                    )
                } else {
                    write!(
                        f,
                        "'{window}' is blocked by a modal dialog '{dialog}'; target the dialog instead, or dismiss it"
                    )
                }
            }
            Self::NoDisplays => write!(f, "no display found"),
            Self::SessionLost(e) => write!(f, "the capture session was lost: {e}"),
            Self::Os(e) => write!(f, "an OS API call failed: {e}"),
            Self::Timeout => write!(f, "no new frame arrived"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// A screen capture backend.
///
/// Implementations carry internal state (a D3D device, a duplication session
/// and so on), so reuse one instead of creating it per call.
pub trait ScreenCapture: Send {
    fn displays(&self) -> &[DisplayInfo];

    /// Capture one display in full.
    fn capture(&mut self, display: usize) -> Result<Frame, CaptureError>;

    /// Capture a rectangle in virtual desktop coordinates.
    ///
    /// The default implementation grabs the whole display and crops. A backend
    /// that can capture a sub-region directly may override this.
    fn capture_rect(&mut self, rect: Rect) -> Result<Frame, CaptureError> {
        let display = self
            .displays()
            .iter()
            .find(|d| d.bounds.intersect(&rect).is_some())
            .map(|d| d.index)
            .ok_or(CaptureError::NoDisplays)?;

        let frame = self.capture(display)?;
        frame.crop(rect).ok_or(CaptureError::NoDisplays)
    }

    /// Backend name, for logs and diagnostics.
    fn backend_name(&self) -> &'static str;

    /// The backend's state in a human-readable form.
    ///
    /// A situation like "DXGI opened but every frame actually falls back to
    /// GDI" is invisible from the name alone. Use this when performance is not
    /// what you expected.
    fn diagnostics(&self) -> String {
        self.backend_name().to_string()
    }
}

/// Open the default backend for this platform.
///
/// Returns [`CaptureError::Unsupported`] on anything but Windows. To add
/// support, add a module for that platform, implement [`ScreenCapture`] and
/// swap the branch here. No change is needed in the upper layers.
pub fn open() -> Result<Box<dyn ScreenCapture>, CaptureError> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows_dxgi::DxgiCapture::new()?))
    }
    #[cfg(target_os = "macos")]
    {
        // This would be ScreenCaptureKit. Out of scope for now.
        Err(CaptureError::Unsupported("macOS"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // This would be PipeWire (Wayland) / XShm (X11). Out of scope for now.
        Err(CaptureError::Unsupported("Linux"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(title: &str, class_name: &str, exe: &str, pid: u32, z: usize) -> WindowInfo {
        WindowInfo {
            title: title.into(),
            class_name: class_name.into(),
            exe: exe.into(),
            pid,
            bounds: Rect::new(0, 0, 100, 100),
            z_order: z,
            hwnd: 0,
        }
    }

    /// A pattern without wildcards is a substring match, so that the
    /// long-standing `window("Notepad")` spelling keeps working.
    #[test]
    fn plain_title_pattern_is_a_substring_match() {
        let w = win("*minutes.txt - Notepad", "Notepad", "Notepad.exe", 10, 0);
        assert!(WindowQuery::title_contains("Notepad").matches(&w));
        assert!(WindowQuery::title_contains("minutes").matches(&w));
        assert!(!WindowQuery::title_contains("TextEditor").matches(&w));
    }

    /// A pattern containing wildcards must match the whole string.
    #[test]
    fn wildcard_title_anchors_the_whole_string() {
        let w = win("Untitled - Notepad", "Notepad", "Notepad.exe", 10, 0);
        assert!(WindowQuery::title_contains("* - Notepad").matches(&w));
        assert!(WindowQuery::title_contains("Untitled*").matches(&w));
        // `?` is exactly one character. "Untitled" is eight.
        assert!(WindowQuery::title_contains("???????? - Notepad").matches(&w));
        assert!(!WindowQuery::title_contains("????????? - Notepad").matches(&w));
        // Anchored, so a different tail does not match.
        assert!(!WindowQuery::title_contains("* - TextEditor").matches(&w));
    }

    #[test]
    fn glob_handles_consecutive_stars_and_empty() {
        let w = win("abc", "C", "a.exe", 1, 0);
        assert!(WindowQuery::title_contains("*").matches(&w));
        assert!(WindowQuery::title_contains("**abc**").matches(&w));
        assert!(WindowQuery::title_contains("a*c").matches(&w));
        assert!(!WindowQuery::title_contains("a*d").matches(&w));
    }

    /// Class names match by prefix, because WPF appends a per-launch GUID.
    #[test]
    fn class_name_matches_by_prefix() {
        let wpf = win(
            "Vector Editor",
            "HwndWrapper[Designer.exe;;7ba151fe-b64c-4a2d-90cb-da73fd81370e]",
            "Designer.exe",
            20,
            0,
        );
        let q = WindowQuery {
            class_name: Some("HwndWrapper[Designer.exe".into()),
            ..Default::default()
        };
        assert!(q.matches(&wpf), "did not match a class name with a GUID");
    }

    /// Executable names are matched ignoring case.
    ///
    /// In practice the OS reports `Notepad.exe` while users usually write
    /// `notepad.exe`. A mismatch here would be hard to explain.
    #[test]
    fn exe_match_ignores_case() {
        let w = win("Untitled - Notepad", "Notepad", "Notepad.exe", 10, 0);
        let q = WindowQuery {
            exe: Some("notepad.exe".into()),
            ..Default::default()
        };
        assert!(q.matches(&w));
    }

    /// Chromium-based apps share a class name, so only the executable can
    /// separate them.
    #[test]
    fn executable_separates_chromium_apps() {
        let browser = win(
            "A page - Browser",
            "Chrome_WidgetWin_1",
            "browser.exe",
            1,
            0,
        );
        let editor = win("main.rs - Editor", "Chrome_WidgetWin_1", "Editor.exe", 2, 1);

        let q = WindowQuery {
            class_name: Some("Chrome_WidgetWin_1".into()),
            ..Default::default()
        };
        assert!(
            q.matches(&browser) && q.matches(&editor),
            "the class alone cannot separate them"
        );

        let q = WindowQuery {
            exe: Some("editor.exe".into()),
            ..Default::default()
        };
        assert!(!q.matches(&browser) && q.matches(&editor));
    }

    /// Conditions combine with AND. Missing any one of them fails the match.
    #[test]
    fn conditions_are_combined_with_and() {
        let w = win("Untitled - Notepad", "Notepad", "Notepad.exe", 10, 0);
        let q = WindowQuery {
            title: Some("Notepad".into()),
            exe: Some("notepad.exe".into()),
            class_name: Some("Notepad".into()),
            ..Default::default()
        };
        assert!(q.matches(&w));

        let q = WindowQuery {
            exe: Some("texteditor.exe".into()),
            ..q
        };
        assert!(!q.matches(&w));
    }

    #[test]
    fn title_exact_requires_the_whole_string() {
        let w = win("Untitled - Notepad", "Notepad", "Notepad.exe", 10, 0);
        let q = WindowQuery {
            title_exact: Some("Untitled - Notepad".into()),
            ..Default::default()
        };
        assert!(q.matches(&w));

        let q = WindowQuery {
            title_exact: Some("Notepad".into()),
            ..Default::default()
        };
        assert!(!q.matches(&w), "it is behaving as a substring match");
    }

    #[test]
    fn empty_query_is_detected() {
        assert!(WindowQuery::default().is_empty());
        assert!(!WindowQuery::title_contains("x").is_empty());
        // An index on its own is not a condition.
        assert!(
            WindowQuery {
                index: 3,
                ..Default::default()
            }
            .is_empty()
        );
    }

    #[test]
    fn describe_lists_the_given_conditions() {
        let q = WindowQuery {
            exe: Some("notepad.exe".into()),
            index: 1,
            ..Default::default()
        };
        let d = q.describe();
        assert!(d.contains("exe=notepad.exe"), "{d}");
        assert!(d.contains("index=1"), "{d}");
    }

    fn frame(w: u32, h: u32, origin: (i32, i32)) -> Frame {
        // Embed the coordinates in the pixel values so the crop position can be
        // verified.
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                bgra.extend_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        Frame {
            width: w,
            height: h,
            origin,
            bgra,
        }
    }

    #[test]
    fn rect_intersection() {
        let a = Rect::new(0, 0, 100, 100);
        assert_eq!(
            a.intersect(&Rect::new(50, 50, 100, 100)),
            Some(Rect::new(50, 50, 50, 50))
        );
        assert_eq!(a.intersect(&Rect::new(200, 0, 10, 10)), None);
        // Merely touching along an edge does not count as intersecting.
        assert_eq!(a.intersect(&Rect::new(100, 0, 10, 10)), None);
    }

    #[test]
    fn rect_with_negative_origin() {
        // A monitor to the left has negative coordinates.
        let left = Rect::new(-1920, 0, 1920, 1080);
        assert!(left.contains(-1000, 500));
        assert!(!left.contains(0, 500));
        assert_eq!(left.right(), 0);
    }

    #[test]
    fn crop_uses_virtual_desktop_coordinates() {
        let f = frame(10, 10, (100, 200));
        let c = f.crop(Rect::new(103, 204, 4, 3)).unwrap();
        assert_eq!((c.width, c.height), (4, 3));
        assert_eq!(c.origin, (103, 204));
        // The top-left pixel should be (3, 4) within the frame.
        assert_eq!(&c.bgra[0..2], &[3, 4]);
    }

    #[test]
    fn crop_clips_to_frame() {
        let f = frame(10, 10, (0, 0));
        let c = f.crop(Rect::new(8, 8, 100, 100)).unwrap();
        assert_eq!((c.width, c.height), (2, 2));
        assert!(f.crop(Rect::new(50, 50, 5, 5)).is_none());
    }

    #[test]
    fn luma_matches_bt601() {
        let f = Frame {
            width: 1,
            height: 1,
            origin: (0, 0),
            bgra: vec![0, 0, 255, 255], // pure red
        };
        let luma = f.to_luma_f32();
        assert!((luma[0] - 0.299).abs() < 1e-6, "{}", luma[0]);
    }
}
