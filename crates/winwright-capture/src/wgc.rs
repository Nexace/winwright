//! Windows.Graphics.Capture single-frame grabs with explicit D3D11 and frame-pool ownership.
//! Every object here lives and dies on the capture worker thread.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{HRESULT, Interface};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::com::{platform, refused};
use crate::raster::{Bgra, LocalRect, MAX_EDGE, clamp_to_frame, copy_rows, raw_len};
use crate::worker::Deadline;

/// Upper bound on waiting for the first frame, independent of the caller's deadline.
const FIRST_FRAME_CAP: Duration = Duration::from_secs(3);
/// Longest single wait between frame-pool polls, so cancellation is noticed promptly.
const POLL_SLICE: Duration = Duration::from_millis(20);

fn unavailable(reason: impl Into<String>) -> WinwrightError {
    WinwrightError::BackendUnavailable {
        backend: "Windows.Graphics.Capture".into(),
        reason: reason.into(),
    }
}

pub struct Wgc {
    d3d: ID3D11Device,
    context: ID3D11DeviceContext,
    device: IDirect3DDevice,
    interop: IGraphicsCaptureItemInterop,
}

fn create_d3d(
    driver: D3D_DRIVER_TYPE,
) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    // SAFETY: all out pointers are valid Options owned by this frame; no adapter or software
    // module is passed.
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    match (device, context) {
        (Some(d), Some(c)) => Ok((d, c)),
        _ => Err(windows::Win32::Foundation::E_POINTER.into()),
    }
}

impl Wgc {
    /// Requires COM/WinRT (MTA) on the calling thread.
    pub fn new() -> WinwrightResult<Self> {
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err(unavailable(
                "screen capture is not supported on this system",
            ));
        }
        let (d3d, context) = create_d3d(D3D_DRIVER_TYPE_HARDWARE)
            .or_else(|e| {
                tracing::debug!(%e, "hardware D3D11 device unavailable; using WARP");
                create_d3d(D3D_DRIVER_TYPE_WARP)
            })
            .map_err(|e| unavailable(format!("cannot create a D3D11 device: {e}")))?;
        let dxgi: IDXGIDevice = d3d.cast().map_err(|e| platform("IDXGIDevice", &e))?;
        // SAFETY: `dxgi` is a live DXGI device owned by this thread.
        let device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .and_then(|inspectable| inspectable.cast())
            .map_err(|e| platform("CreateDirect3D11DeviceFromDXGIDevice", &e))?;
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| unavailable(format!("capture item factory unavailable: {e}")))?;
        Ok(Self {
            d3d,
            context,
            device,
            interop,
        })
    }

    pub fn window_item(&self, hwnd: HWND) -> WinwrightResult<GraphicsCaptureItem> {
        // SAFETY: `hwnd` was validated by the caller; the interop only reads it.
        unsafe { self.interop.CreateForWindow(hwnd) }
            .map_err(|e| refused("GraphicsCaptureItem::CreateForWindow", &e))
    }

    pub fn monitor_item(&self, monitor: HMONITOR) -> WinwrightResult<GraphicsCaptureItem> {
        // SAFETY: `monitor` comes from a fresh EnumDisplayMonitors on this thread.
        unsafe { self.interop.CreateForMonitor(monitor) }
            .map_err(|e| refused("GraphicsCaptureItem::CreateForMonitor", &e))
    }

    /// Captures one frame of `item` and reads back `crop` (capture-local pixels; the whole
    /// frame when `None`). The crop happens on the GPU, so small regions of large monitors only
    /// read back the pixels they need.
    pub fn grab(
        &self,
        item: &GraphicsCaptureItem,
        crop: Option<LocalRect>,
        deadline: &Deadline,
    ) -> WinwrightResult<Bgra> {
        let size = item
            .Size()
            .map_err(|e| refused("GraphicsCaptureItem::Size", &e))?;
        let edge_ok = |v: i32| v > 0 && v as u32 <= MAX_EDGE;
        if !edge_ok(size.Width) || !edge_ok(size.Height) {
            return Err(WinwrightError::CaptureFailed {
                reason: format!(
                    "capture target size {}x{} is empty or exceeds {MAX_EDGE}px",
                    size.Width, size.Height
                ),
            });
        }
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &self.device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            1,
            size,
        )
        .map_err(|e| platform("Direct3D11CaptureFramePool::CreateFreeThreaded", &e))?;
        let mut capture = Capture {
            pool,
            arrived: None,
            session: None,
        };
        let (signal, arrivals) = mpsc::sync_channel::<()>(1);
        capture.arrived = Some(
            capture
                .pool
                .FrameArrived(&TypedEventHandler::new(move |_, _| {
                    let _ = signal.try_send(());
                    Ok(())
                }))
                .map_err(|e| platform("Direct3D11CaptureFramePool::FrameArrived", &e))?,
        );
        let session = capture
            .pool
            .CreateCaptureSession(item)
            .map_err(|e| refused("CreateCaptureSession", &e))?;
        capture.session = Some(session.clone());
        if let Err(e) = session.SetIsCursorCaptureEnabled(false) {
            tracing::debug!(%e, "cannot disable cursor capture");
        }
        if let Err(e) = session.SetIsBorderRequired(false) {
            tracing::debug!(%e, "capture border cannot be disabled; a yellow border may show");
        }
        session
            .StartCapture()
            .map_err(|e| refused("GraphicsCaptureSession::StartCapture", &e))?;

        let frame = capture.first_frame(&arrivals, deadline)?;
        self.read_frame(&frame.0, crop)
        // `frame` then `capture` drop here: frame, session, and pool are closed in that order.
    }

    fn read_frame(
        &self,
        frame: &Direct3D11CaptureFrame,
        crop: Option<LocalRect>,
    ) -> WinwrightResult<Bgra> {
        let surface = frame
            .Surface()
            .map_err(|e| platform("Direct3D11CaptureFrame::Surface", &e))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|e| platform("IDirect3DDxgiInterfaceAccess", &e))?;
        // SAFETY: the surface is a live D3D11 texture owned by the open frame.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|e| platform("IDirect3DDxgiInterfaceAccess::GetInterface", &e))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` is a valid out pointer.
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(WinwrightError::CaptureFailed {
                reason: format!("unexpected frame pixel format {}", desc.Format.0),
            });
        }
        // The pool texture can be larger than the content (e.g. the window shrank).
        let content = frame
            .ContentSize()
            .map_err(|e| platform("Direct3D11CaptureFrame::ContentSize", &e))?;
        let width = desc.Width.min(content.Width.max(0) as u32);
        let height = desc.Height.min(content.Height.max(0) as u32);
        let full = LocalRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let region = clamp_to_frame(crop.unwrap_or(full), width, height).ok_or_else(|| {
            WinwrightError::CaptureFailed {
                reason: format!("requested pixels lie outside the {width}x{height} frame"),
            }
        })?;
        raw_len(region.width, region.height)?;
        self.read_back(&texture, region)
    }

    /// Copies `region` of `texture` into a CPU-readable staging texture and reads it out.
    fn read_back(&self, texture: &ID3D11Texture2D, region: LocalRect) -> WinwrightResult<Bgra> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: region.width,
            Height: region.height,
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
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` describes a valid staging texture; the out pointer is a local Option.
        unsafe { self.d3d.CreateTexture2D(&desc, None, Some(&mut staging)) }
            .map_err(|e| platform("ID3D11Device::CreateTexture2D", &e))?;
        let staging = staging.ok_or_else(|| WinwrightError::Platform {
            operation: "ID3D11Device::CreateTexture2D".into(),
            hresult: windows::Win32::Foundation::E_POINTER.0,
        })?;
        let src_box = D3D11_BOX {
            left: region.x,
            top: region.y,
            front: 0,
            right: region.x + region.width,
            bottom: region.y + region.height,
            back: 1,
        };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures belong to this device and share the BGRA format; the box lies
        // inside the source (clamped by the caller) and matches the staging size.
        unsafe {
            self.context
                .CopySubresourceRegion(&staging, 0, 0, 0, 0, texture, 0, Some(&src_box));
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| platform("ID3D11DeviceContext::Map", &e))?;
        }
        let pitch = mapped.RowPitch as usize;
        let row = region.width as usize * 4;
        let len = pitch * (region.height as usize - 1) + row;
        let pixels = if mapped.pData.is_null() || pitch < row {
            None
        } else {
            // SAFETY: a mapped staging texture exposes `RowPitch * (Height - 1) + Width * 4`
            // readable bytes at `pData` until Unmap, which happens right after this copy.
            let src = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) };
            Some(copy_rows(src, pitch, region.width, region.height))
        };
        // SAFETY: balances the successful Map above.
        unsafe { self.context.Unmap(&staging, 0) };
        let pixels = pixels.ok_or_else(|| WinwrightError::CaptureFailed {
            reason: format!("mapped frame has an invalid layout (row pitch {pitch})"),
        })?;
        Ok(Bgra {
            width: region.width,
            height: region.height,
            pixels,
        })
    }
}

/// An active frame pool + session, closed (and the handler removed) on drop.
struct Capture {
    pool: Direct3D11CaptureFramePool,
    arrived: Option<i64>,
    session: Option<GraphicsCaptureSession>,
}

/// Returns the frame's buffer to the pool on drop.
struct Frame(Direct3D11CaptureFrame);

impl Drop for Frame {
    fn drop(&mut self) {
        let _ = self.0.Close();
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let _ = session.Close();
        }
        if let Some(token) = self.arrived.take() {
            let _ = self.pool.RemoveFrameArrived(token);
        }
        let _ = self.pool.Close();
    }
}

impl Capture {
    /// Waits for the first frame, woken by `FrameArrived` and bounded by the caller's deadline,
    /// cancellation, and [`FIRST_FRAME_CAP`].
    fn first_frame(
        &self,
        arrivals: &mpsc::Receiver<()>,
        deadline: &Deadline,
    ) -> WinwrightResult<Frame> {
        let started = Instant::now();
        let limit = deadline.at.min(started + FIRST_FRAME_CAP);
        loop {
            match self.pool.TryGetNextFrame() {
                Ok(frame) => return Ok(Frame(frame)),
                // A null frame surfaces as an "empty" error with HRESULT 0: none yet.
                Err(e) if e.code() == HRESULT(0) => {}
                Err(e) => return Err(platform("Direct3D11CaptureFramePool::TryGetNextFrame", &e)),
            }
            if deadline.cancel.is_cancelled() {
                return Err(WinwrightError::Cancelled);
            }
            let now = Instant::now();
            if now >= limit {
                return Err(WinwrightError::Timeout {
                    operation: "waiting for the first capture frame".into(),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
            let _ = arrivals.recv_timeout((limit - now).min(POLL_SLICE));
        }
    }
}
