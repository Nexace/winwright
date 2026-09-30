//! Overlay and hotkey contracts (spec §21, §22). Implemented by `winwright-overlay`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::WinwrightResult;
use crate::geometry::PhysicalRect;
use crate::input::Key;

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
}

fn default_color() -> u32 {
    0x00E0_4A2A
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

/// Native, click-through, never-activating overlays on their own message-loop thread.
pub trait OverlayService: Send + Sync {
    fn show(&self, request: OverlayRequest) -> WinwrightResult<OverlayId>;
    /// Removes one overlay, or all when `id` is `None`.
    fn clear(&self, id: Option<OverlayId>) -> WinwrightResult<()>;
}

/// A global hotkey bound with `RegisterHotKey`. Conflicts are reported, never ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HotkeyBinding {
    pub keys: Vec<Key>,
}
