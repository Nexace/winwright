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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRequest {
    pub target: CaptureTarget,
    #[serde(default)]
    pub format: ImageFormat,
    /// JPEG quality 1..=100; ignored for PNG.
    #[serde(default = "default_quality")]
    pub quality: u8,
    /// Scale the image down to fit (never up); `None` keeps every physical pixel.
    #[serde(default)]
    pub fit: Option<Fit>,
    /// Numbered boxes to draw on the image (set of marks).
    #[serde(default)]
    pub marks: Vec<Mark>,
}

/// A numbered box drawn on a screenshot, in physical desktop pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Mark {
    pub rect: PhysicalRect,
    pub number: u32,
}

fn default_quality() -> u8 {
    85
}

/// Largest image to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Fit {
    pub max_edge: u32,
    pub max_pixels: u64,
}

impl Fit {
    /// What a vision model sees without scaling it further itself (Claude: 1568 px on the long
    /// edge and about 1.15 megapixels), so a point it reads off the image maps back exactly.
    pub const MODEL: Self = Self {
        max_edge: 1568,
        max_pixels: 1_150_000,
    };

    /// The size `width` x `height` fits to: same aspect, never larger, at least 1x1.
    pub fn size(self, width: u32, height: u32) -> (u32, u32) {
        let (w, h) = (f64::from(width), f64::from(height));
        let by_edge = f64::from(self.max_edge) / w.max(h);
        let by_area = (self.max_pixels as f64 / (w * h)).sqrt();
        let s = by_edge.min(by_area).min(1.0);
        if s >= 1.0 {
            return (width, height);
        }
        let side = |v: f64| ((v * s).floor() as u32).max(1);
        (side(w), side(h))
    }
}

/// Owned encoded image. No live graphics objects ever leave the capture crate.
#[derive(Clone, PartialEq, Eq)]
pub struct CapturedImage {
    pub bytes: Vec<u8>,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    /// The captured physical pixels' size: `width`/`height` unless the image was fitted.
    pub physical_width: u32,
    pub physical_height: u32,
    /// Physical desktop position of the image's top-left pixel.
    pub origin: PhysicalPoint,
    /// Effective DPI of the monitor holding the image's top-left pixel.
    pub dpi: u32,
    /// Unix epoch milliseconds when the frame was taken.
    pub timestamp_ms: u64,
    /// For a marked screenshot: the snapshot whose refs the drawn numbers name (mark 12 is
    /// ref e12). Filled in by the engine.
    pub legend: Option<String>,
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
            self.origin.x + self.physical_width as i32,
            self.origin.y + self.physical_height as i32,
        )
    }

    /// Image pixels per physical pixel: 1.0 unless the image was fitted.
    pub fn scale(&self) -> f64 {
        f64::from(self.width) / f64::from(self.physical_width.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::Fit;

    #[test]
    fn fitting_keeps_the_aspect_and_never_enlarges() {
        assert_eq!(Fit::MODEL.size(800, 600), (800, 600));
        let (w, h) = Fit::MODEL.size(1920, 1200);
        assert!(u64::from(w) * u64::from(h) <= 1_150_000, "{w}x{h}");
        assert_eq!((w, h), (1356, 847));
        assert_eq!(
            Fit::MODEL.size(4000, 300).0,
            1568,
            "a wide strip is bound by its edge"
        );
        assert_eq!(
            Fit::MODEL.size(100_000, 1),
            (1568, 1),
            "never below one pixel"
        );
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

/// Model-facing screenshot target (spec §18). Resolved by the engine into a [`CaptureTarget`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ScreenshotTarget {
    /// The foreground window.
    #[default]
    Active,
    Window(crate::window::WindowSelector),
    /// An element's bounds (by ref), cropped from the screen.
    Element {
        #[serde(rename = "ref")]
        reference: String,
    },
    Monitor(u32),
    Region(PhysicalRect),
    Desktop,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScreenshotRequest {
    #[serde(default)]
    pub target: ScreenshotTarget,
    #[serde(default)]
    pub format: ImageFormat,
    /// JPEG quality 1..=100 (default 85).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
    /// Scale down to what a vision model sees in full ([`Fit::MODEL`]).
    #[serde(default)]
    pub fit: bool,
    /// Draw each interactive element's ref number on a window's image (set of marks).
    #[serde(default)]
    pub marks: bool,
}

/// Text read off the screen by OCR. Boxes are physical desktop pixels.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenText {
    /// The recognizer's language, e.g. `en-US`.
    pub language: String,
    pub lines: Vec<TextLine>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextLine {
    pub text: String,
    pub bounds: PhysicalRect,
    pub words: Vec<TextWord>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextWord {
    pub text: String,
    pub bounds: PhysicalRect,
}

pub trait CaptureService: Send + Sync {
    fn monitors(&self) -> WinwrightResult<Vec<MonitorInfo>>;

    fn capture<'a>(
        &'a self,
        request: CaptureRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, CapturedImage>;

    /// Reads the text in the captured pixels with OCR (`format`, `quality` and `fit` unused).
    fn read_text<'a>(
        &'a self,
        _request: CaptureRequest,
        _ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ScreenText> {
        Box::pin(async {
            Err(crate::WinwrightError::BackendUnavailable {
                backend: "OCR".into(),
                reason: "this capture backend cannot read text".into(),
            })
        })
    }
}
