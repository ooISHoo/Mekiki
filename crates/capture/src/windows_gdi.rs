//! Windows: capture via GDI `BitBlt`.
//!
//! Slower than DXGI Desktop Duplication, but **it always returns a frame even
//!
//! when the screen is completely still**. Desktop Duplication only hands back
//! the desktop image when something changed since last time, so a fully static
//! screen yields nothing. "Cannot capture while the screen is not moving" is
//!
//! fatal for an automation tool, so this path is kept as insurance.
//!
//! It also works where DXGI is unavailable: remote desktop, some virtualised
//! environments, and setups where the app and the display span different GPUs.

use std::mem::size_of;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, HBITMAP, HDC,
    ReleaseDC, SRCCOPY, SelectObject,
};

use crate::{CaptureError, DisplayInfo, Frame, Rect};

/// A wrapper that guarantees the GDI handle is released.
///
/// This keeps an early return from leaking. A process gets 10000 GDI handles by
/// default, so leaking one per capture exhausts them quickly.
struct ScreenDc(HDC);

impl Drop for ScreenDc {
    fn drop(&mut self) {
        unsafe {
            ReleaseDC(Some(HWND::default()), self.0);
        }
    }
}

struct MemDc(HDC);

impl Drop for MemDc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct Bitmap(HBITMAP);

impl Drop for Bitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.0.into());
        }
    }
}

/// Grab a rectangle in virtual desktop coordinates as BGRA.
pub(crate) fn capture_rect(rect: Rect) -> Result<Frame, CaptureError> {
    if rect.is_empty() {
        return Err(CaptureError::Os("the rectangle is empty".into()));
    }

    let width = rect.width as i32;
    let height = rect.height as i32;

    unsafe {
        // The DC for HWND::default() (= NULL) covers the whole virtual desktop.
        let screen = ScreenDc(GetDC(Some(HWND::default())));
        if screen.0.is_invalid() {
            return Err(CaptureError::Os("GetDC failed".into()));
        }

        let mem = MemDc(CreateCompatibleDC(Some(screen.0)));
        if mem.0.is_invalid() {
            return Err(CaptureError::Os("CreateCompatibleDC failed".into()));
        }

        let bitmap = Bitmap(CreateCompatibleBitmap(screen.0, width, height));
        if bitmap.0.is_invalid() {
            return Err(CaptureError::Os("CreateCompatibleBitmap failed".into()));
        }

        let old = SelectObject(mem.0, bitmap.0.into());

        // CAPTUREBLT also includes layered windows (translucent tooltips and the like).
        BitBlt(
            mem.0,
            0,
            0,
            width,
            height,
            Some(screen.0),
            rect.x,
            rect.y,
            SRCCOPY | CAPTUREBLT,
        )
        .map_err(|e| CaptureError::Os(format!("BitBlt failed: {e}")))?;

        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative means top-down (rows from top to bottom). Omitting it
                // gives bottom-up and the image comes out vertically flipped.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bgra = vec![0u8; (width as usize) * (height as usize) * 4];
        let copied = GetDIBits(
            mem.0,
            bitmap.0,
            0,
            height as u32,
            Some(bgra.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );

        SelectObject(mem.0, old);

        if copied == 0 {
            return Err(CaptureError::Os("GetDIBits failed".into()));
        }

        // GDI leaves alpha at 0 even at 32bpp. Fill it in so the layers above
        // can assume opaque.
        for px in bgra.chunks_exact_mut(4) {
            px[3] = 255;
        }

        Ok(Frame {
            width: rect.width,
            height: rect.height,
            origin: (rect.x, rect.y),
            bgra,
        })
    }
}

/// A GDI-only backend, for environments where DXGI is unavailable.
pub struct GdiCapture {
    displays: Vec<DisplayInfo>,
}

impl GdiCapture {
    pub fn new(displays: Vec<DisplayInfo>) -> Result<Self, CaptureError> {
        if displays.is_empty() {
            return Err(CaptureError::NoDisplays);
        }
        Ok(Self { displays })
    }
}

impl crate::ScreenCapture for GdiCapture {
    fn displays(&self) -> &[DisplayInfo] {
        &self.displays
    }

    fn capture(&mut self, display: usize) -> Result<Frame, CaptureError> {
        let d = self
            .displays
            .get(display)
            .ok_or(CaptureError::NoSuchDisplay(display))?;
        capture_rect(d.bounds)
    }

    fn capture_rect(&mut self, rect: Rect) -> Result<Frame, CaptureError> {
        // GDI can grab an arbitrary rectangle directly, so there is no need to
        // capture everything and crop.
        capture_rect(rect)
    }

    fn backend_name(&self) -> &'static str {
        "windows/gdi-bitblt"
    }
}
