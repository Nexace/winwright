//! Screen capture contracts (spec §18). Implemented by `winwright-capture`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::WinwrightResult;
use crate::backend::{BackendFuture, OperationContext};
use crate::geometry::{PhysicalPoint, PhysicalRect};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
}

/// What to capture. Regions are physical virtual-desktop pixels and may span monitors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CaptureTarget {
    /// Every monitor composed into one image covering the virtual desktop bounds.
    Desktop,
    /// Index into [`CaptureService::monitors`].
    Monitor(u32),
    /// A top-level window, including occluded content where the OS allows.
    Window(u64),
    Region(PhysicalRect),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRequest {
    pub target: CaptureTarget,
    #[serde(default)]
    pub format: ImageFormat,
    /// JPEG quality 1..=100; ignored for PNG.
    #[serde(default = "default_quality")]
    pub quality: u8,
}

fn default_quality() -> u8 {
    85
}

/// Owned encoded image. No live graphics objects ever leave the capture crate.
#[derive(Clone, PartialEq, Eq)]
pub struct CapturedImage {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    /// Physical desktop position of the image's top-left pixel.
    pub origin: PhysicalPoint,
    /// Effective DPI of the monitor holding the image's top-left pixel.
    pub dpi: u32,
    /// Unix epoch milliseconds when the frame was taken.
    pub timestamp_ms: u64,
}

impl std::fmt::Debug for CapturedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never dump pixel data into logs.
        f.debug_struct("CapturedImage")
            .field("format", &self.format)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("origin", &self.origin)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl CapturedImage {
    /// Desktop rectangle the image covers.
    pub fn bounds(&self) -> PhysicalRect {
        PhysicalRect::new(
            self.origin.x,
            self.origin.y,
            self.origin.x + self.width as i32,
            self.origin.y + self.height as i32,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub index: u32,
    /// Device name such as `\\.\DISPLAY1`.
    pub name: String,
    pub bounds: PhysicalRect,
    pub work_area: PhysicalRect,
    pub dpi: u32,
    /// 100 at 96 DPI, 125 at 120 DPI, ...
    pub scale_percent: u32,
    pub primary: bool,
}

pub trait CaptureService: Send + Sync {
    fn monitors(&self) -> WinwrightResult<Vec<MonitorInfo>>;

    fn capture<'a>(
        &'a self,
        request: CaptureRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, CapturedImage>;
}
