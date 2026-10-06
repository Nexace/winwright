//! Overlay and hotkey contracts (spec §21, §22). Implemented by `winwright-overlay`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::action::{ElementTarget, ScreenPoint};
use crate::geometry::{PhysicalPoint, PhysicalRect};
use crate::input::{Key, MouseButton};
use crate::{WinwrightError, WinwrightResult};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OverlayStyle {
    /// Border around the rectangle.
    #[default]
    Highlight,
    /// Arrow pointing at the rectangle from outside it.
    Arrow,
    /// Small filled circle at the rectangle's center (click feedback).
    ClickMarker,
    /// A pointer at the rectangle's center with the label in a bubble beside it (teaching).
    /// It draws no step badge.
    Pointer,
}

fn default_color() -> u32 {
    0x0008_91B2
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OverlayRequest {
    /// Physical virtual-desktop pixels.
    pub rect: PhysicalRect,
    #[serde(default)]
    pub style: OverlayStyle,
    /// Text drawn next to the rectangle (e.g. "Click this"). Max 120 chars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Numbered badge for tutorial steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    /// 0xRRGGBB.
    #[serde(default = "default_color")]
    pub color: u32,
    /// Auto-hide after this many milliseconds; `None` keeps it until cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OverlayId(pub u64);

/// Model-facing highlight (spec §21): the engine resolves `target` to bounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HighlightRequest {
    pub target: crate::action::ElementTarget,
    #[serde(default)]
    pub style: OverlayStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// Defaults to 8 s so forgotten highlights disappear; `0` is rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HighlightResult {
    pub overlay: OverlayId,
    #[serde(rename = "ref")]
    pub reference: String,
    pub target: String,
    pub rect: PhysicalRect,
}

/// An area given by pixels: `width` x `height` centered on `at`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScreenSpot {
    pub at: ScreenPoint,
    #[serde(default = "default_spot_side")]
    pub width: u32,
    #[serde(default = "default_spot_side")]
    pub height: u32,
}

pub const DEFAULT_SPOT_SIDE: u32 = 48;

fn default_spot_side() -> u32 {
    DEFAULT_SPOT_SIDE
}

/// A highlight of a spot given by pixels, for what has no UI element (canvases, custom UIs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpotHighlightRequest {
    pub spot: ScreenSpot,
    #[serde(default)]
    pub style: OverlayStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// What a guide step points at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum GuideTarget {
    Element(Box<ElementTarget>),
    Spot(ScreenSpot),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum GuideWait {
    /// The person clicks inside the step's area.
    #[default]
    Click,
    /// The pixels of the step's area change (a step done with the keyboard).
    Change,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuideStep {
    pub target: GuideTarget,
    pub caption: String,
    #[serde(default)]
    pub wait: GuideWait,
}

/// Teaching: each step is pointed at in turn and waits for the person to do it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuideRequest {
    pub steps: Vec<GuideStep>,
    pub style: OverlayStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// For the whole guide.
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum GuideOutcome {
    /// Every step was done.
    Done,
    /// The person clicked outside the current step's area; the guide stopped there.
    ClickedElsewhere,
    TimedOut,
}

/// A click the person made during a guide.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuideClick {
    /// The step shown at the time (1-based).
    pub step: u32,
    pub button: MouseButton,
    /// Where the button went down, in the step's pixels (its window's screenshot when the
    /// step gave a window, else the screen).
    pub x: i32,
    pub y: i32,
    /// Where it was released, when the person dragged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drag_to: Option<PhysicalPoint>,
    pub inside: bool,
    /// The element under the press.
    pub element: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuideResult {
    /// Steps done.
    pub completed: u32,
    pub steps: u32,
    pub outcome: GuideOutcome,
    pub clicks: Vec<GuideClick>,
    /// The window of the last step shown, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A mouse button the person pressed or released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerEvent {
    /// Physical virtual-desktop pixels.
    pub point: PhysicalPoint,
    pub button: MouseButton,
    pub down: bool,
}

pub type PointerCallback = Box<dyn Fn(PointerEvent) + Send + Sync>;

/// Watching stops when this drops.
pub struct PointerWatch(Option<Box<dyn FnOnce() + Send>>);

impl PointerWatch {
    pub fn new(stop: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(stop)))
    }
}

impl Drop for PointerWatch {
    fn drop(&mut self) {
        if let Some(stop) = self.0.take() {
            stop();
        }
    }
}

/// Native, click-through, never-activating overlays on their own message-loop thread.
pub trait OverlayService: Send + Sync {
    fn show(&self, request: OverlayRequest) -> WinwrightResult<OverlayId>;
    /// Removes one overlay, or all when `id` is `None`.
    fn clear(&self, id: Option<OverlayId>) -> WinwrightResult<()>;

    /// Calls `on_event` for every mouse button the person presses or releases until the
    /// returned watch drops. Injected input (Winwright's own clicks among it) is skipped and
    /// the keyboard is never watched. `on_event` runs on the overlay thread: return at once.
    fn watch_pointer(&self, on_event: PointerCallback) -> WinwrightResult<PointerWatch> {
        drop(on_event);
        Err(WinwrightError::BackendUnavailable {
            backend: "overlay".into(),
            reason: "this overlay service cannot watch the pointer".into(),
        })
    }
}

/// A global hotkey bound with `RegisterHotKey`. Conflicts are reported, never ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HotkeyBinding {
    pub keys: Vec<Key>,
}
