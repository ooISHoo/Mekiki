//! Windows: capture via DXGI Desktop Duplication.
//!
//! Faster than GDI BitBlt, and free of tearing because the image comes straight
//! from the compositor. Should this ever go zero-copy (Phase 4-3), the
//! `ID3D11Texture2D` obtained here could be handed to wgpu directly. For now it
//! is read back to the CPU through a staging texture.
//!
//! # A quirk of Desktop Duplication
//!
//! `AcquireNextFrame` only returns a frame **when the screen changed**. On a
//! static screen it keeps timing out, so the last content that was obtained is
//! retained and returned. Implementing this without knowing that leaves capture
//! failing forever on a still screen.

use std::ptr;

use windows::Win32::Foundation::{E_ACCESSDENIED, E_INVALIDARG, HMODULE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTPUT_DESC, IDXGIAdapter1,
    IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::core::Interface;

use crate::{CaptureError, DisplayInfo, Frame, Rect, ScreenCapture};

/// How long a single `AcquireNextFrame` waits.
const ACQUIRE_TIMEOUT_MS: u32 = 20;

/// Retry count while no content has been obtained yet.
///
/// A completely static screen never sends an update, so we give up here and
/// fall back to GDI.
const INITIAL_RETRIES: u32 = 10;

fn os_err(e: windows::core::Error) -> CaptureError {
    CaptureError::Os(format!("{e}"))
}

/// Make the process per-monitor DPI aware.
///
/// Without this, on a scaled monitor the OS virtualises coordinates and bitmaps
/// behind our back, and the pixel coordinates of the captured image no longer
/// line up with mouse coordinates. This fails if it was already set (via a
/// manifest, say), which is fine to ignore.
fn ensure_dpi_aware() {
    unsafe {
        if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            log::debug!("DPI awareness is already set or cannot be set: {e}");
        }
    }
}

struct Output {
    output: IDXGIOutput1,
    /// The D3D11 device on **the adapter that owns this output**.
    ///
    /// This is per-output, not shared, because on a hybrid-GPU laptop the
    /// display is driven by the integrated GPU while the default adapter is the
    /// discrete one. `DuplicateOutput` has to run on the owning adapter's
    /// device, so creating one shared device on the default adapter returns
    /// `DXGI_ERROR_UNSUPPORTED` for an output the other adapter owns. Duplication
    /// and the staging texture must all use this device.
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dupl: Option<IDXGIOutputDuplication>,
    staging: Option<ID3D11Texture2D>,
    /// Whether we hold content that was last copied into staging.
    /// This is what gets returned when `AcquireNextFrame` times out on a still screen.
    has_content: bool,
    /// Why this output is captured with GDI instead of Desktop Duplication.
    ///
    /// `None` means duplication. Set permanently when duplication cannot be
    /// established at all (`DXGI_ERROR_UNSUPPORTED` — seen live on a
    /// hybrid-GPU laptop where the driver refuses it outright), when a lost
    /// session fails to re-establish, or when `MEKIKI_CAPTURE=gdi` forces it.
    /// GDI is slower and does not include the hardware cursor, but it returns
    /// the real desktop where duplication returns nothing.
    gdi_only: Option<String>,
}

pub struct DxgiCapture {
    displays: Vec<DisplayInfo>,
    outputs: Vec<Output>,
    /// Whether we ever fell back to GDI. For diagnostics.
    used_gdi_fallback: bool,
}

impl DxgiCapture {
    /// Whether the GDI fallback has been used so far.
    ///
    /// If it is always true, DXGI is not working, which is a lead worth chasing.
    pub fn used_gdi_fallback(&self) -> bool {
        self.used_gdi_fallback
    }
}

// D3D11 devices and contexts are not free-threaded, but moving ownership while
// keeping it confined to a single thread is fine.
// Declared explicitly to satisfy ScreenCapture: Send.
unsafe impl Send for DxgiCapture {}

impl DxgiCapture {
    pub fn new() -> Result<Self, CaptureError> {
        ensure_dpi_aware();

        // Devices are created per adapter, inside enumerate_outputs, so that
        // each output gets a device on the adapter that actually owns it.
        let (displays, outputs) = enumerate_outputs()?;
        if displays.is_empty() {
            return Err(CaptureError::NoDisplays);
        }

        log::info!("DXGI Desktop Duplication: {} display(s)", displays.len());
        for d in &displays {
            log::info!(
                "  [{}] {} {} {}",
                d.index,
                d.name,
                d.bounds,
                if d.is_primary { "(primary)" } else { "" }
            );
        }

        let mut capture = Self {
            displays,
            outputs,
            used_gdi_fallback: false,
        };

        // The explicit override, for a machine already known to be one where
        // duplication does not work: skip the failing attempt entirely.
        // Read here rather than in each frontend so the MCP server, the CLI
        // and the IDE all honour it the same way.
        if std::env::var("MEKIKI_CAPTURE").as_deref() == Ok("gdi") {
            log::info!("MEKIKI_CAPTURE=gdi: capturing every display with GDI");
            for output in &mut capture.outputs {
                output.gdi_only = Some("forced by MEKIKI_CAPTURE=gdi".to_string());
            }
        }

        Ok(capture)
    }

    /// Establish the duplication session if there is not one already.
    ///
    /// When the driver refuses duplication outright (`DXGI_ERROR_UNSUPPORTED`),
    /// the output is switched to GDI permanently instead of erroring: this is
    /// what a blind server looked like for three test rounds on a hybrid-GPU
    /// laptop where even the owning adapter's device gets `UNSUPPORTED`.
    /// The caller must check [`Output::gdi_only`] after this returns `Ok`.
    fn ensure_duplication(&mut self, index: usize) -> Result<(), CaptureError> {
        if self.outputs[index].dupl.is_some() || self.outputs[index].gdi_only.is_some() {
            return Ok(());
        }

        let device = self.outputs[index].device.clone();
        let dupl = match unsafe { self.outputs[index].output.DuplicateOutput(&device) } {
            Ok(dupl) => dupl,
            Err(e) if e.code() == DXGI_ERROR_UNSUPPORTED => {
                // Not transient: this machine will not duplicate this output,
                // ever. GDI returns the real desktop there, so use it and say
                // so, rather than reporting an error forever.
                log::warn!(
                    "display {index}: Desktop Duplication is unsupported ({e}); \
                     capturing it with GDI from now on (slower, no hardware cursor)"
                );
                self.outputs[index].gdi_only =
                    Some(format!("duplication unsupported: {}", e.code()));
                return Ok(());
            }
            Err(e) if e.code() == E_ACCESSDENIED => {
                // The secure desktop (a UAC dialog, the lock screen) is showing, etc.
                return Err(CaptureError::SessionLost(format!(
                    "cannot start duplication (the secure desktop may be showing): {e}"
                )));
            }
            Err(e) if e.code() == E_INVALIDARG => {
                // Only one duplication per output exists, even across
                // processes. Establishing a second one while another holds
                // it makes the NVIDIA driver return 0x80070057.
                return Err(CaptureError::Os(format!(
                    "cannot start screen duplication (another capture holds the same display): {e}"
                )));
            }
            Err(e) => return Err(os_err(e)),
        };

        self.outputs[index].dupl = Some(dupl);
        self.outputs[index].has_content = false;
        Ok(())
    }

    /// Discard the duplication session. The next call re-establishes it.
    fn drop_duplication(&mut self, index: usize) {
        self.outputs[index].dupl = None;
        self.outputs[index].has_content = false;
    }

    /// Create the staging texture if missing, or if the size changed.
    fn ensure_staging(
        &mut self,
        index: usize,
        width: u32,
        height: u32,
    ) -> Result<(), CaptureError> {
        if let Some(tex) = &self.outputs[index].staging {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { tex.GetDesc(&mut desc) };
            if desc.Width == width && desc.Height == height {
                return Ok(());
            }
        }

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };

        let mut tex: Option<ID3D11Texture2D> = None;
        unsafe {
            self.outputs[index]
                .device
                .CreateTexture2D(&desc, None, Some(&mut tex))
        }
        .map_err(os_err)?;

        self.outputs[index].staging = tex;
        self.outputs[index].has_content = false;
        Ok(())
    }

    /// Copy one new frame into staging.
    ///
    /// `false` means there was no new frame (the screen is static).
    fn acquire_into_staging(&mut self, index: usize) -> Result<bool, CaptureError> {
        let retries = if self.outputs[index].has_content {
            1
        } else {
            INITIAL_RETRIES
        };

        for _ in 0..retries {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            let dupl = self.outputs[index]
                .dupl
                .as_ref()
                .expect("ensure_duplication has run")
                .clone();

            let acquired =
                unsafe { dupl.AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut info, &mut resource) };

            match acquired {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    self.drop_duplication(index);
                    return Err(CaptureError::SessionLost(format!("{e}")));
                }
                Err(e) => return Err(os_err(e)),
            }

            // Returning early past this point would skip ReleaseFrame and leave
            // every later AcquireNextFrame failing. Take the result first, then
            // always release.
            let result = (|| -> Result<(), CaptureError> {
                // A frame with LastPresentTime == 0 means only the pointer
                // information was updated and the desktop image was not.
                // The surface contents are not guaranteed then, and in practice
                // it comes back all zero (pure black). Using it just because
                // resource is non-None grabs a black image.
                if info.LastPresentTime == 0 {
                    return Ok(());
                }

                let Some(resource) = resource else {
                    return Ok(());
                };
                let src: ID3D11Texture2D = resource.cast().map_err(os_err)?;

                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { src.GetDesc(&mut desc) };
                self.ensure_staging(index, desc.Width, desc.Height)?;

                let out = &self.outputs[index];
                let staging = out.staging.as_ref().expect("created above");
                unsafe { out.context.CopyResource(staging, &src) };
                self.outputs[index].has_content = true;
                Ok(())
            })();

            unsafe {
                let _ = dupl.ReleaseFrame();
            }
            result?;

            if self.outputs[index].has_content {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Read the staging texture back to the CPU and turn it into a `Frame`.
    fn read_staging(&mut self, index: usize) -> Result<Frame, CaptureError> {
        let staging = self.outputs[index]
            .staging
            .as_ref()
            .ok_or(CaptureError::Timeout)?
            .clone();

        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { staging.GetDesc(&mut desc) };

        let context = self.outputs[index].context.clone();
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(os_err)?;
        }

        let width = desc.Width as usize;
        let height = desc.Height as usize;
        let row_bytes = width * 4;
        let pitch = mapped.RowPitch as usize;

        // RowPitch often differs from the real byte length of a row (for
        // alignment). Copy row by row to repack it tightly.
        let mut bgra = vec![0u8; row_bytes * height];
        unsafe {
            let src = mapped.pData as *const u8;
            for y in 0..height {
                ptr::copy_nonoverlapping(
                    src.add(y * pitch),
                    bgra.as_mut_ptr().add(y * row_bytes),
                    row_bytes,
                );
            }
            context.Unmap(&staging, 0);
        }

        let bounds = self.displays[index].bounds;
        Ok(Frame {
            width: desc.Width,
            height: desc.Height,
            origin: (bounds.x, bounds.y),
            bgra,
        })
    }
}

impl ScreenCapture for DxgiCapture {
    fn displays(&self) -> &[DisplayInfo] {
        &self.displays
    }

    fn capture(&mut self, display: usize) -> Result<Frame, CaptureError> {
        if display >= self.outputs.len() {
            return Err(CaptureError::NoSuchDisplay(display));
        }

        self.ensure_duplication(display)?;

        if self.outputs[display].gdi_only.is_some() {
            self.used_gdi_fallback = true;
            return crate::windows_gdi::capture_rect(self.displays[display].bounds);
        }

        let got_new = match self.acquire_into_staging(display) {
            Ok(v) => v,
            Err(CaptureError::SessionLost(e)) => {
                // It can be lost by a display configuration or mode change.
                // Re-establish it exactly once; if even that fails, duplication
                // is not coming back (an RDP session behaves like this), so
                // switch the output to GDI for good rather than failing every
                // call from here on.
                log::debug!("re-establishing the duplication session: {e}");
                let retried = self.ensure_duplication(display).and_then(|()| {
                    match self.outputs[display].gdi_only {
                        // ensure_duplication itself may have switched to GDI.
                        Some(_) => Ok(false),
                        None => self.acquire_into_staging(display),
                    }
                });
                match retried {
                    Ok(v) => v,
                    Err(CaptureError::SessionLost(e2)) => {
                        log::warn!(
                            "display {display}: the duplication session will not re-establish \
                             ({e2}); capturing it with GDI from now on"
                        );
                        self.outputs[display].gdi_only =
                            Some(format!("duplication kept failing: {e2}"));
                        false
                    }
                    Err(e2) => return Err(e2),
                }
            }
            Err(e) => return Err(e),
        };

        if self.outputs[display].gdi_only.is_some() {
            self.used_gdi_fallback = true;
            return crate::windows_gdi::capture_rect(self.displays[display].bounds);
        }

        if !got_new && !self.outputs[display].has_content {
            // On a completely static screen Desktop Duplication never hands
            // back a desktop image. Grab it with GDI instead.
            log::debug!("DXGI returned no content, falling back to GDI (the screen is static)");
            self.used_gdi_fallback = true;
            return crate::windows_gdi::capture_rect(self.displays[display].bounds);
        }

        self.read_staging(display)
    }

    fn capture_rect(&mut self, rect: Rect) -> Result<Frame, CaptureError> {
        let display = self
            .displays
            .iter()
            .find(|d| d.bounds.intersect(&rect).is_some())
            .map(|d| d.index)
            .ok_or(CaptureError::NoDisplays)?;

        let frame = self.capture(display)?;
        frame.crop(rect).ok_or(CaptureError::NoDisplays)
    }

    fn backend_name(&self) -> &'static str {
        "windows/dxgi-desktop-duplication (+gdi fallback)"
    }

    fn diagnostics(&self) -> String {
        // Name the path per output: "the whole screen came from GDI" and "one
        // static frame fell back once" must not read the same, or nobody can
        // tell a permanently unsupported display from a quiet one.
        let outputs: Vec<String> = self
            .outputs
            .iter()
            .enumerate()
            .map(|(i, o)| match &o.gdi_only {
                Some(reason) => format!("[{i}] gdi ({reason})"),
                None => format!("[{i}] dxgi"),
            })
            .collect();
        format!(
            "{} / {} / static-screen GDI fallback: {}",
            self.backend_name(),
            outputs.join(", "),
            if self.used_gdi_fallback() {
                "used"
            } else {
                "not used"
            }
        )
    }
}

fn enumerate_outputs() -> Result<(Vec<DisplayInfo>, Vec<Output>), CaptureError> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(os_err)?;

    let mut displays = Vec::new();
    let mut outputs = Vec::new();

    let mut adapter_index = 0u32;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(os_err(e)),
        };
        adapter_index += 1;

        // A device on **this** adapter is required to duplicate an output it
        // owns. It is created lazily: an adapter with no desktop-attached
        // output never needs one, and skipping a failure here lets one usable
        // adapter carry the display even if the other cannot make a device.
        let mut adapter_device: Option<(ID3D11Device, ID3D11DeviceContext)> = None;

        let mut output_index = 0u32;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(os_err(e)),
            };
            output_index += 1;

            let desc: DXGI_OUTPUT_DESC = unsafe { output.GetDesc() }.map_err(os_err)?;

            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }

            let (device, context) = match &adapter_device {
                Some(dc) => dc.clone(),
                None => match create_device(&adapter) {
                    Ok(dc) => {
                        adapter_device = Some(dc.clone());
                        dc
                    }
                    Err(e) => {
                        // This adapter cannot back a capture device. Skip its
                        // outputs; another adapter may still drive the display.
                        log::warn!("no D3D11 device on adapter {}: {e}", adapter_index - 1);
                        break;
                    }
                },
            };

            let r = desc.DesktopCoordinates;
            let bounds = crate::Rect::new(
                r.left,
                r.top,
                (r.right - r.left) as u32,
                (r.bottom - r.top) as u32,
            );

            let name = String::from_utf16_lossy(
                &desc
                    .DeviceName
                    .iter()
                    .take_while(|&&c| c != 0)
                    .copied()
                    .collect::<Vec<u16>>(),
            );

            let output1: IDXGIOutput1 = output.cast().map_err(os_err)?;

            displays.push(DisplayInfo {
                index: displays.len(),
                name,
                bounds,
                // The primary monitor is the one at the virtual desktop origin.
                is_primary: bounds.x == 0 && bounds.y == 0,
            });
            outputs.push(Output {
                output: output1,
                device,
                context,
                dupl: None,
                staging: None,
                has_content: false,
                gdi_only: None,
            });
        }
    }

    Ok((displays, outputs))
}

/// Create a D3D11 device on a specific adapter.
///
/// An explicit adapter **requires** `D3D_DRIVER_TYPE_UNKNOWN`; passing
/// `HARDWARE` with an adapter returns `E_INVALIDARG`. `BGRA_SUPPORT` is needed
/// because Desktop Duplication's format is BGRA.
fn create_device(
    adapter: &IDXGIAdapter1,
) -> Result<(ID3D11Device, ID3D11DeviceContext), CaptureError> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
        .map_err(os_err)?;
    }
    let device = device.ok_or_else(|| CaptureError::Os("the D3D11 device is null".into()))?;
    let context = context.ok_or_else(|| CaptureError::Os("the D3D11 context is null".into()))?;
    Ok((device, context))
}
