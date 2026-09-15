//! Windows clipboard text access.
//!
//! The same boundary rules as input injection apply: this module holds
//! **primitives only** and text is `CF_UNICODETEXT`; other clipboard formats
//! are out of scope. The one piece of policy kept here is the open retry,
//! because "another process briefly holds the clipboard" is a property of the
//! OS API, not of any caller.

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

use crate::{InputError, Result};

const CF_UNICODETEXT: u32 = 13;

/// `OpenClipboard` fails transiently while another process holds the
/// clipboard open, so a few short retries are part of the primitive.
fn open_clipboard() -> Result<()> {
    let mut last = None;
    for _ in 0..5 {
        match unsafe { OpenClipboard(None) } {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    Err(InputError::Os(format!(
        "cannot open the clipboard: {}",
        last.expect("at least one attempt ran")
    )))
}

/// Read the clipboard text. An empty or non-text clipboard is `Ok("")`.
pub fn clipboard_text() -> Result<String> {
    open_clipboard()?;
    let result = read_unicode_text();
    let _ = unsafe { CloseClipboard() };
    result
}

fn read_unicode_text() -> Result<String> {
    let handle = match unsafe { GetClipboardData(CF_UNICODETEXT) } {
        // No text on the clipboard is absence, not an error.
        Err(_) => return Ok(String::new()),
        Ok(h) if h.is_invalid() => return Ok(String::new()),
        Ok(h) => h,
    };
    let hglobal = HGLOBAL(handle.0);
    let ptr = unsafe { GlobalLock(hglobal) } as *const u16;
    if ptr.is_null() {
        return Err(InputError::Os("cannot lock the clipboard memory".into()));
    }
    // SAFETY: CF_UNICODETEXT is NUL-terminated by contract.
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) });
    let _ = unsafe { GlobalUnlock(hglobal) };
    Ok(text)
}

/// Replace the clipboard contents with text.
pub fn set_clipboard_text(text: &str) -> Result<()> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);

    open_clipboard()?;
    let result = write_unicode_text(&wide);
    let _ = unsafe { CloseClipboard() };
    result
}

fn write_unicode_text(wide: &[u16]) -> Result<()> {
    unsafe { EmptyClipboard() }
        .map_err(|e| InputError::Os(format!("cannot clear the clipboard: {e}")))?;

    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) }
        .map_err(|e| InputError::Os(format!("cannot allocate clipboard memory: {e}")))?;
    let ptr = unsafe { GlobalLock(hglobal) } as *mut u16;
    if ptr.is_null() {
        let _ = unsafe { GlobalFree(Some(hglobal)) };
        return Err(InputError::Os("cannot lock the clipboard memory".into()));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(hglobal);
    }

    // On success the system owns the memory; it is freed only on failure.
    if let Err(e) = unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hglobal.0))) } {
        let _ = unsafe { GlobalFree(Some(hglobal)) };
        return Err(InputError::Os(format!("cannot set the clipboard: {e}")));
    }
    Ok(())
}
