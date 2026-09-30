//! Playwright-style waits (spec §15). Event-accelerated, poll-guaranteed, never fixed sleeps.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::element::ElementInfo;
use crate::locator::{ElementLocator, MatchMode};
use crate::snapshot::SnapshotTarget;
use crate::window::{WindowInfo, WindowSelector};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WaitState {
    /// At least one element matches (visible or not).
    Exists,
    /// No element matches any more.
    Missing,
    Visible,
    Hidden,
    Enabled,
    Disabled,
    Focused,
    /// The element's value matches `value`.
    Value,
    /// The element's name or value matches `value`.
    Text,
    /// A top-level window matching `window` is open.
    WindowOpen,
    /// No top-level window matches `window`.
    WindowClosed,
}

impl WaitState {
    pub fn is_window_state(self) -> bool {
        matches!(self, Self::WindowOpen | Self::WindowClosed)
    }
}

/// Element waits take `ref` or `locator` (+ `scope`); window waits take `window`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitRequest {
    pub state: WaitState,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<ElementLocator>,
    #[serde(default)]
    pub scope: SnapshotTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowSelector>,
    /// Expected value/text for the `value` and `text` states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// How `value` is compared (default: exact, case-insensitive).
    #[serde(default, rename = "match")]
    pub value_match: MatchMode,
    /// Defaults to the configured `defaultTimeoutMs`. Max 10 minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl WaitRequest {
    pub const MAX_TIMEOUT_MS: u64 = 600_000;

    pub fn validate(&self) -> Result<(), String> {
        if self
            .timeout_ms
            .is_some_and(|t| t == 0 || t > Self::MAX_TIMEOUT_MS)
        {
            return Err(format!("timeoutMs must be 1..={}", Self::MAX_TIMEOUT_MS));
        }
        if self.state.is_window_state() {
            return match &self.window {
                Some(w) if !w.is_empty() => Ok(()),
                _ => Err("window-open/window-closed waits need `window`".into()),
            };
        }
        match (&self.reference, &self.locator) {
            (Some(_), None) => {}
            (None, Some(l)) => l.validate()?,
            _ => return Err("element waits need exactly one of `ref` or `locator`".into()),
        }
        if matches!(self.state, WaitState::Value | WaitState::Text) && self.value.is_none() {
            return Err("value/text waits need `value`".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WaitResult {
    pub state: WaitState,
    pub elapsed_ms: u64,
    /// Predicate evaluations performed.
    pub checks: u32,
    /// UI Automation events that woke the wait early.
    pub events: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element: Option<ElementInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_forms_and_validation() {
        let w: WaitRequest = serde_json::from_str(
            r#"{"state":"visible","locator":{"role":"Text","name":"Export complete"},"timeoutMs":10000}"#,
        )
        .unwrap();
        assert!(w.validate().is_ok());
        let w: WaitRequest =
            serde_json::from_str(r#"{"state":"window-open","window":{"title":"Save As"}}"#)
                .unwrap();
        assert!(w.validate().is_ok());
        let w: WaitRequest = serde_json::from_str(r#"{"state":"window-open"}"#).unwrap();
        assert!(w.validate().is_err());
        let w: WaitRequest = serde_json::from_str(r#"{"state":"value","ref":"e3"}"#).unwrap();
        assert!(w.validate().is_err(), "value wait without value");
        let w: WaitRequest =
            serde_json::from_str(r#"{"state":"enabled","ref":"e3","timeoutMs":0}"#).unwrap();
        assert!(w.validate().is_err());
    }
}
