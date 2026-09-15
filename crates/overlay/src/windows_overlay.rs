//! Windows: boxes drawn with a layered window.
//!
//! # How it works
//!
//! A borderless window is created over the target rectangle, and
//! `WS_EX_LAYERED` plus a colour key makes everything but the border transparent.
//!
//! What the extended styles are for:
//!
//! - `WS_EX_LAYERED` — required to use colour-key transparency
//! - `WS_EX_TRANSPARENT` — lets mouse input pass through. Without it the box
//!   swallows clicks and the automated click right after lands on the box
//! - `WS_EX_NOACTIVATE` — showing it does not steal focus
//! - `WS_EX_TOOLWINDOW` — keeps it off the taskbar
//! - `WS_EX_TOPMOST` — puts it in front of the target window

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH,
    DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FW_SEMIBOLD, FillRect,
    GetTextExtentPoint32W, HBRUSH, InvalidateRect, NONANTIALIASED_QUALITY, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics,
    HWND_TOPMOST, LWA_COLORKEY, MSG, PM_REMOVE, PeekMessageW, RegisterClassExW, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SetLayeredWindowAttributes, SetWindowPos, ShowWindow, TranslateMessage,
    WM_DESTROY, WM_PAINT, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::{Mark, Overlay, OverlayError, ScreenBounds, Style, place_label};

/// The colour treated as transparent. Chosen so it does not collide with the
///
/// box colour: if the border happened to use the same value the border itself
/// would go transparent, so this is a magenta-ish value nobody uses in practice.
const COLOR_KEY: COLORREF = COLORREF(0x00FF_00FE);

const CLASS_NAME: PCWSTR = w!("MekikiOverlayWindow");
const MARKS_CLASS_NAME: PCWSTR = w!("MekikiPreviewOverlay");

/// Drawing information, and the way it reaches the window procedure.
///
/// Thread local rather than `static mut`. `wnd_proc` is called from the message
/// loop of the thread that created the window, so this both reaches it and
/// avoids unsafe.
/// Assumes only one overlay is shown at a time.
#[derive(Copy, Clone)]
struct PaintState {
    /// In COLORREF form (0x00BBGGRR).
    color: u32,
    thickness: u32,
    width: i32,
    height: i32,
}

thread_local! {
    static PAINT_STATE: std::cell::Cell<PaintState> = const {
        std::cell::Cell::new(PaintState {
            color: 0,
            thickness: 0,
            width: 0,
            height: 0,
        })
    };
}

struct Session {
    tx: Sender<SessionCmd>,
}

enum SessionCmd {
    Show {
        marks: Vec<Mark>,
        style: Style,
        reply: Sender<Result<(), OverlayError>>,
    },
    Hide,
    Shutdown,
}

struct MarksPaint {
    marks: Vec<Mark>,
    color: u32,
    thickness: u32,
    origin: (i32, i32),
    size: (i32, i32),
}

impl MarksPaint {
    fn empty() -> Self {
        Self {
            marks: Vec::new(),
            color: 0,
            thickness: 3,
            origin: (0, 0),
            size: (0, 0),
        }
    }
}

thread_local! {
    static MARKS_PAINT: RefCell<MarksPaint> = RefCell::new(MarksPaint::empty());
}

pub struct LayeredOverlay {
    class_registered: bool,
    session: Option<Session>,
}

// The window belongs to the thread that created it, but moving the struct itself is fine.
unsafe impl Send for LayeredOverlay {}

impl LayeredOverlay {
    pub fn new() -> Self {
        Self {
            class_registered: false,
            session: None,
        }
    }

    fn ensure_session(&mut self) -> Result<&Session, OverlayError> {
        if self.session.is_none() {
            let (tx, rx) = channel();
            std::thread::Builder::new()
                .name("mekiki-overlay".into())
                .spawn(move || session_loop(rx))
                .map_err(|e| OverlayError::Os(format!("cannot start the overlay thread: {e}")))?;
            self.session = Some(Session { tx });
        }
        Ok(self.session.as_ref().expect("created just above"))
    }

    fn ensure_class(&mut self) -> Result<HINSTANCE, OverlayError> {
        let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
            .map_err(|e| OverlayError::Os(format!("GetModuleHandleW failed: {e}")))?
            .into();

        if self.class_registered {
            return Ok(instance);
        }

        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };

        // Returns 0 when already registered, which is fine from the second call on.
        unsafe { RegisterClassExW(&class) };
        self.class_registered = true;
        Ok(instance)
    }
}

impl Default for LayeredOverlay {
    fn default() -> Self {
        Self::new()
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => unsafe {
            let state = PAINT_STATE.with(|s| s.get());

            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            // Fill the whole surface with the colour key first (= make it transparent).
            let key_brush = CreateSolidBrush(COLOR_KEY);
            let full = windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: state.width,
                bottom: state.height,
            };
            FillRect(hdc, &full, key_brush);
            let _ = DeleteObject(key_brush.into());

            // Draw the border as four bands, leaving only the outline opaque.
            let border: HBRUSH = CreateSolidBrush(COLORREF(state.color));
            let t = state.thickness as i32;
            let bands = [
                windows::Win32::Foundation::RECT {
                    left: 0,
                    top: 0,
                    right: state.width,
                    bottom: t,
                },
                windows::Win32::Foundation::RECT {
                    left: 0,
                    top: state.height - t,
                    right: state.width,
                    bottom: state.height,
                },
                windows::Win32::Foundation::RECT {
                    left: 0,
                    top: 0,
                    right: t,
                    bottom: state.height,
                },
                windows::Win32::Foundation::RECT {
                    left: state.width - t,
                    top: 0,
                    right: state.width,
                    bottom: state.height,
                },
            ];
            for band in &bands {
                FillRect(hdc, band, border);
            }
            let _ = DeleteObject(border.into());

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        },
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

impl Overlay for LayeredOverlay {
    fn show(
        &mut self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        style: Style,
    ) -> Result<(), OverlayError> {
        if width == 0 || height == 0 {
            return Ok(());
        }

        let instance = self.ensure_class()?;

        // The border is drawn outside the rectangle; inside it would cover the target.
        let t = style.thickness.max(1);
        let outer_x = x - t as i32;
        let outer_y = y - t as i32;
        let outer_w = width + t * 2;
        let outer_h = height + t * 2;

        PAINT_STATE.with(|s| {
            s.set(PaintState {
                // COLORREF is 0x00BBGGRR, the reverse of RGB order.
                color: (style.color.2 as u32) << 16
                    | (style.color.1 as u32) << 8
                    | style.color.0 as u32,
                thickness: t,
                width: outer_w as i32,
                height: outer_h as i32,
            })
        });

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE,
                CLASS_NAME,
                PCWSTR::null(),
                WS_POPUP,
                outer_x,
                outer_y,
                outer_w as i32,
                outer_h as i32,
                None,
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|e| OverlayError::Os(format!("CreateWindowExW failed: {e}")))?;

        let result = (|| -> Result<(), OverlayError> {
            unsafe {
                SetLayeredWindowAttributes(hwnd, COLOR_KEY, 0, LWA_COLORKEY)
                    .map_err(|e| OverlayError::Os(format!("SetLayeredWindowAttributes: {e}")))?;

                // SW_SHOWNOACTIVATE and SWP_NOACTIVATE keep showing it from stealing focus.
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    outer_x,
                    outer_y,
                    outer_w as i32,
                    outer_h as i32,
                    SWP_NOACTIVATE,
                );
            }

            // Without pumping messages it freezes without ever painting.
            let deadline = Instant::now() + style.duration;
            while Instant::now() < deadline {
                let mut msg = MSG::default();
                unsafe {
                    while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        })();

        // Clean up even on failure. Leaving it behind strands a box on screen.
        unsafe {
            let _ = DestroyWindow(hwnd);
        }

        result
    }

    fn show_marks(&mut self, marks: &[Mark], style: Style) -> Result<(), OverlayError> {
        if marks.is_empty() {
            return self.hide_marks();
        }
        let session = self.ensure_session()?;
        let (reply_tx, reply_rx) = channel();
        session
            .tx
            .send(SessionCmd::Show {
                marks: marks.to_vec(),
                style,
                reply: reply_tx,
            })
            .map_err(|_| OverlayError::Os("the overlay thread died".into()))?;
        reply_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| OverlayError::Os("the overlay is not responding".into()))?
    }

    fn hide_marks(&mut self) -> Result<(), OverlayError> {
        if let Some(session) = &self.session {
            let _ = session.tx.send(SessionCmd::Hide);
        }
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "windows/layered-window"
    }
}

impl Drop for LayeredOverlay {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let _ = session.tx.send(SessionCmd::Shutdown);
        }
    }
}

fn colorref(color: crate::Color) -> u32 {
    (color.2 as u32) << 16 | (color.1 as u32) << 8 | color.0 as u32
}

fn virtual_screen() -> ScreenBounds {
    unsafe {
        ScreenBounds {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    }
}

fn session_loop(rx: Receiver<SessionCmd>) {
    let mut hwnd = HWND::default();
    let mut deadline: Option<Instant> = None;

    loop {
        match rx.recv_timeout(Duration::from_millis(16)) {
            Ok(SessionCmd::Show {
                marks,
                style,
                reply,
            }) => {
                let result = show_on_thread(&mut hwnd, marks, style, &mut deadline);
                let _ = reply.send(result);
            }
            Ok(SessionCmd::Hide) => {
                hide_on_thread(hwnd, &mut deadline);
            }
            Ok(SessionCmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                if hwnd != HWND::default() {
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if deadline.is_some_and(|d| Instant::now() >= d) {
            hide_on_thread(hwnd, &mut deadline);
        }

        if hwnd != HWND::default() {
            let mut msg = MSG::default();
            unsafe {
                while PeekMessageW(&mut msg, Some(hwnd), 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

fn show_on_thread(
    hwnd: &mut HWND,
    marks: Vec<Mark>,
    style: Style,
    deadline: &mut Option<Instant>,
) -> Result<(), OverlayError> {
    let screen = virtual_screen();
    if screen.width <= 0 || screen.height <= 0 {
        return Err(OverlayError::Os(
            "cannot read the virtual screen size".into(),
        ));
    }

    MARKS_PAINT.with(|cell| {
        *cell.borrow_mut() = MarksPaint {
            marks,
            color: colorref(style.color),
            thickness: style.thickness.max(1),
            origin: (screen.x, screen.y),
            size: (screen.width, screen.height),
        };
    });

    if *hwnd == HWND::default() {
        *hwnd = create_marks_window(screen)?;
    } else {
        unsafe {
            let _ = SetWindowPos(
                *hwnd,
                Some(HWND_TOPMOST),
                screen.x,
                screen.y,
                screen.width,
                screen.height,
                SWP_NOACTIVATE,
            );
        }
    }

    unsafe {
        let _ = ShowWindow(*hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            *hwnd,
            Some(HWND_TOPMOST),
            screen.x,
            screen.y,
            screen.width,
            screen.height,
            SWP_NOACTIVATE,
        );
        let _ = InvalidateRect(Some(*hwnd), None, true);
    }

    *deadline = Some(Instant::now() + style.duration);
    Ok(())
}

fn hide_on_thread(hwnd: HWND, deadline: &mut Option<Instant>) {
    *deadline = None;
    if hwnd != HWND::default() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

fn create_marks_window(screen: ScreenBounds) -> Result<HWND, OverlayError> {
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map_err(|e| OverlayError::Os(format!("GetModuleHandleW failed: {e}")))?
        .into();

    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(marks_wnd_proc),
        hInstance: instance,
        lpszClassName: MARKS_CLASS_NAME,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&class) };

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            MARKS_CLASS_NAME,
            PCWSTR::null(),
            WS_POPUP,
            screen.x,
            screen.y,
            screen.width,
            screen.height,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|e| OverlayError::Os(format!("CreateWindowExW failed: {e}")))?;

    unsafe {
        SetLayeredWindowAttributes(hwnd, COLOR_KEY, 0, LWA_COLORKEY)
            .map_err(|e| OverlayError::Os(format!("SetLayeredWindowAttributes: {e}")))?;
    }
    Ok(hwnd)
}

unsafe extern "system" fn marks_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            MARKS_PAINT.with(|cell| paint_marks(hdc, &cell.borrow()));
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        },
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn paint_marks(hdc: windows::Win32::Graphics::Gdi::HDC, state: &MarksPaint) {
    use windows::Win32::Foundation::RECT;

    let key_brush = unsafe { CreateSolidBrush(COLOR_KEY) };
    let full = RECT {
        left: 0,
        top: 0,
        right: state.size.0,
        bottom: state.size.1,
    };
    unsafe {
        FillRect(hdc, &full, key_brush);
        let _ = DeleteObject(key_brush.into());
    }

    let border: HBRUSH = unsafe { CreateSolidBrush(COLORREF(state.color)) };
    let t = state.thickness as i32;
    let (ox, oy) = state.origin;
    let screen = ScreenBounds {
        x: ox,
        y: oy,
        width: state.size.0,
        height: state.size.1,
    };

    let font = unsafe {
        CreateFontW(
            16,
            0,
            0,
            0,
            FW_SEMIBOLD.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            NONANTIALIASED_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        )
    };
    let old_font = unsafe { SelectObject(hdc, font.into()) };
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00FF_FFFF));
    }

    for mark in &state.marks {
        if mark.width == 0 || mark.height == 0 {
            continue;
        }
        let left = mark.x - t - ox;
        let top = mark.y - t - oy;
        let right = mark.x + mark.width as i32 + t - ox;
        let bottom = mark.y + mark.height as i32 + t - oy;
        let bands = [
            RECT {
                left,
                top,
                right,
                bottom: top + t,
            },
            RECT {
                left,
                top: bottom - t,
                right,
                bottom,
            },
            RECT {
                left,
                top,
                right: left + t,
                bottom,
            },
            RECT {
                left: right - t,
                top,
                right,
                bottom,
            },
        ];
        for band in &bands {
            unsafe { FillRect(hdc, band, border) };
        }

        if let Some(label) = &mark.label {
            if !label.is_empty() {
                draw_label(hdc, mark, state.thickness, label, screen, ox, oy, border);
            }
        }
    }

    unsafe {
        SelectObject(hdc, old_font);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(border.into());
    }
}

fn draw_label(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    mark: &Mark,
    thickness: u32,
    label: &str,
    screen: ScreenBounds,
    ox: i32,
    oy: i32,
    chip_brush: HBRUSH,
) {
    use windows::Win32::Foundation::RECT;

    let mut wide: Vec<u16> = label.encode_utf16().collect();
    let mut size = SIZE::default();
    if !unsafe { GetTextExtentPoint32W(hdc, &wide, &mut size) }.as_bool() {
        return;
    }

    let chip_w = size.cx + 12;
    let chip_h = size.cy + 4;
    let (sx, sy) = place_label(mark, thickness, chip_w, chip_h, screen);
    let mut chip = RECT {
        left: sx - ox,
        top: sy - oy,
        right: sx - ox + chip_w,
        bottom: sy - oy + chip_h,
    };
    unsafe { FillRect(hdc, &chip, chip_brush) };
    unsafe {
        DrawTextW(
            hdc,
            &mut wide,
            &mut chip,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER,
        );
    }
}
