//! Windows: enumerating top-level windows.
//!
//! The foundation of window scoping ([Rhai API architecture](../../../docs/architecture/rhai-api.md)).
//! Limiting the search area to one window is faster, and it also
//! **stops the same image in a background window from being matched**.

use std::mem::size_of;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, MAX_PATH, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{
    DI_NORMAL, DestroyIcon, DrawIconEx, EnumWindows, GCLP_HICON, GCLP_HICONSM, GetClassLongPtrW,
    GetClassNameW, GetForegroundWindow, GetLastActivePopup, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, HICON, ICON_BIG, ICON_SMALL, IsIconic, IsWindow,
    IsWindowVisible, SendMessageW, SetForegroundWindow, WM_GETICON,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

use crate::{CaptureError, Rect, WindowInfo};

/// The accumulator handed to the `EnumWindows` callback.
struct Collector {
    windows: Vec<WindowInfo>,
    /// Enumeration order. `EnumWindows` calls back from the top of the Z order,
    /// so this doubles as the rank.
    next_z: usize,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: enumerate() below passes a &mut Collector through lparam.
    let collector = unsafe { &mut *(lparam.0 as *mut Collector) };

    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return CONTINUE;
        }

        // A minimised window has a fixed off-screen rectangle and is
        // meaningless as a scope.
        if IsIconic(hwnd).as_bool() {
            return CONTINUE;
        }

        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            // Windows without a title are mostly tool windows and invisible shell elements.
            return CONTINUE;
        }

        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied <= 0 {
            return CONTINUE;
        }
        let title = String::from_utf16_lossy(&buf[..copied as usize]);

        let Some(bounds) = window_bounds(hwnd) else {
            return CONTINUE;
        };
        if bounds.is_empty() {
            return CONTINUE;
        }

        let pid = process_id(hwnd);

        collector.windows.push(WindowInfo {
            title,
            class_name: class_name(hwnd),
            exe: executable_name(pid),
            pid,
            bounds,
            z_order: collector.next_z,
            hwnd: hwnd.0 as isize,
        });
        collector.next_z += 1;
    }

    CONTINUE
}

/// The value an `EnumWindows` callback returns to keep enumerating.
const CONTINUE: BOOL = BOOL(1);

/// The window class name.
fn class_name(hwnd: HWND) -> String {
    // Class names are capped at 256 characters (per `RegisterClass`).
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

fn process_id(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The executable name (`notepad.exe`). Empty when it cannot be read.
///
/// Only the file name, not the full path. Writing `exe=notepad.exe` in a script
/// is enough, and requiring the path would break across machines.
///
/// **It is sometimes unavailable.** A process at a higher integrity level (an
/// app running as administrator) cannot be opened. In that case this is left
/// empty and the caller narrows down by other conditions.
fn executable_name(pid: u32) -> String {
    process_image_path(pid)
        .and_then(|path| {
            path.rsplit(['\\', '/'])
                .next()
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default()
}

/// The window's visible rectangle.
///
/// Since Windows 10, `GetWindowRect` includes the invisible border used for the
/// drop shadow, so it returns a rectangle a few pixels larger than what is seen.
/// Using that as the search area pulls in the edge of a neighbouring window.
/// The DWM extended frame bounds match the visible shape instead.
fn window_bounds(hwnd: HWND) -> Option<Rect> {
    let mut rect = RECT::default();

    let dwm_ok = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            size_of::<RECT>() as u32,
        )
    }
    .is_ok();

    if !dwm_ok {
        // Falls through here where DWM is disabled (classic theme, some remote sessions).
        unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    }

    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return None;
    }

    Some(Rect::new(rect.left, rect.top, width as u32, height as u32))
}

/// Bring the window matching the conditions to the front. Does not restore a minimised one.
///
/// Fails with [`CaptureError::ModalOpen`] when the window is disabled by a
/// modal dialog it owns. Activating the owner in that state reports success
/// while every keystroke lands in the dialog — and a message box treats bare
/// letters as button accelerators, so blind typing does not just vanish, it
/// presses buttons. The caller has to decide: target the dialog, or dismiss it.
pub fn activate(query: &crate::WindowQuery) -> Result<(), CaptureError> {
    let info = crate::find_window_by(query)?;
    activate_hwnd(info.hwnd, query.describe())
}

/// Bring an already resolved HWND to the front without consulting Z order.
pub fn activate_handle(hwnd: isize) -> Result<(), CaptureError> {
    let title = window_text(HWND(hwnd as *mut core::ffi::c_void));
    let description = if title.is_empty() {
        format!("hwnd=0x{:X}", hwnd as usize)
    } else {
        title
    };
    activate_hwnd(hwnd, description)
}

fn activate_hwnd(hwnd: isize, description: String) -> Result<(), CaptureError> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        if hwnd.is_invalid() || !IsWindow(Some(hwnd)).as_bool() {
            return Err(CaptureError::NoSuchWindow(description));
        }
        if !IsWindowEnabled(hwnd).as_bool() {
            // A disabled top-level window means a modal dialog owns the input.
            // GetLastActivePopup names the dialog when it can.
            let popup = GetLastActivePopup(hwnd);
            let dialog = if popup != hwnd && IsWindow(Some(popup)).as_bool() {
                window_text(popup)
            } else {
                String::new()
            };
            return Err(CaptureError::ModalOpen {
                window: description,
                dialog,
            });
        }
        if hwnd == GetForegroundWindow() {
            return Ok(());
        }
        if SetForegroundWindow(hwnd).as_bool() || hwnd == GetForegroundWindow() {
            return Ok(());
        }
    }
    Err(CaptureError::ActivateFailed(description))
}

/// The window's title text, or an empty string.
fn window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let read = GetWindowTextW(hwnd, &mut buf);
        String::from_utf16_lossy(&buf[..read.max(0) as usize])
    }
}

const ICON_PX: i32 = 32;

/// Draw the icon from the window or its executable and return it as BGRA.
pub fn icon_bgra(hwnd: isize) -> Option<(u32, u32, Vec<u8>)> {
    let hwnd = HWND(hwnd as *mut core::ffi::c_void);
    if hwnd.is_invalid() {
        return None;
    }
    let (icon, owned) = icon_for_window(hwnd)?;
    let pixels = rasterize_icon(icon);
    if owned {
        let _ = unsafe { DestroyIcon(icon) };
    }
    pixels
}

fn icon_for_window(hwnd: HWND) -> Option<(HICON, bool)> {
    unsafe {
        for size in [ICON_SMALL, ICON_BIG] {
            let result = SendMessageW(
                hwnd,
                WM_GETICON,
                Some(WPARAM(size as usize)),
                Some(LPARAM(0)),
            );
            if result.0 != 0 {
                return Some((HICON(result.0 as *mut core::ffi::c_void), false));
            }
        }
        for index in [GCLP_HICONSM, GCLP_HICON] {
            let value = GetClassLongPtrW(hwnd, index);
            if value != 0 {
                return Some((HICON(value as *mut core::ffi::c_void), false));
            }
        }
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let path = process_image_path(pid)?;
    let mut wide: Vec<u16> = path.encode_utf16().collect();
    wide.push(0);
    let mut info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            Default::default(),
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }
    Some((info.hIcon, true))
}

fn rasterize_icon(icon: HICON) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            return None;
        }
        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            ReleaseDC(None, screen);
            return None;
        }
        let bitmap = CreateCompatibleBitmap(screen, ICON_PX, ICON_PX);
        if bitmap.is_invalid() {
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return None;
        }
        let old = SelectObject(mem, bitmap.into());
        let drawn = DrawIconEx(mem, 0, 0, icon, ICON_PX, ICON_PX, 0, None, DI_NORMAL).is_ok();

        let mut bgra = vec![0u8; (ICON_PX * ICON_PX * 4) as usize];
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: ICON_PX,
                biHeight: -ICON_PX,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let copied = if drawn {
            GetDIBits(
                mem,
                bitmap,
                0,
                ICON_PX as u32,
                Some(bgra.as_mut_ptr().cast()),
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(mem, old);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);

        if copied == 0 {
            return None;
        }
        Some((ICON_PX as u32, ICON_PX as u32, bgra))
    }
}

fn process_image_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; MAX_PATH as usize];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            Default::default(),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_ok();
    let _ = unsafe { CloseHandle(handle) };
    if !ok {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Enumerate visible top-level windows in Z order (frontmost first).
pub fn enumerate() -> Result<Vec<WindowInfo>, CaptureError> {
    let mut collector = Collector {
        windows: Vec::new(),
        next_z: 0,
    };

    if let Err(error) = unsafe { EnumWindows(Some(enum_proc), LPARAM(&raw mut collector as isize)) }
    {
        // EnumWindows documents a zero return as either callback cancellation
        // or failure. Some restricted/non-interactive desktops return zero
        // while leaving last-error at SUCCESS. That means "nothing visible",
        // not an actionable OS error with the absurd text "operation completed
        // successfully". Real access failures retain their HRESULT.
        if error.code().0 != 0 {
            return Err(CaptureError::Os(format!("EnumWindows failed: {error}")));
        }
    }

    Ok(collector.windows)
}
