//! Semantic action contracts (spec §11, §14, §33). The core resolves the target, applies
//! policy, picks the least invasive method, executes, verifies, and reports which method ran.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::PhysicalRect;
use crate::input::{Key, MouseButton};
use crate::locator::ElementLocator;
use crate::snapshot::SnapshotTarget;
use crate::window::WindowSelector;

/// Exactly one of `ref` (from a snapshot/find) or `locator` (resolved fresh in `scope`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ElementTarget {
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<ElementLocator>,
    /// Where a locator searches (default: active window). Ignored for refs.
    #[serde(default)]
    pub scope: SnapshotTarget,
}

impl ElementTarget {
    pub fn by_ref(reference: impl Into<String>) -> Self {
        Self {
            reference: Some(reference.into()),
            locator: None,
            scope: SnapshotTarget::Active,
        }
    }

    pub fn by_locator(locator: ElementLocator, scope: SnapshotTarget) -> Self {
        Self {
            reference: None,
            locator: Some(locator),
            scope,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match (&self.reference, &self.locator) {
            (Some(_), None) => Ok(()),
            (None, Some(l)) => l.validate(),
            (Some(_), Some(_)) => Err("give either ref or locator, not both".into()),
            (None, None) => Err("a target needs a ref or a locator".into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}
fn default_max_chars() -> u32 {
    4_000
}

/// Element-level actions. Serialized with an `action` tag, e.g. `{"action":"click",...}`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum DesktopAction {
    #[serde(rename_all = "camelCase")]
    Click {
        target: ElementTarget,
        #[serde(default)]
        button: MouseButton,
        #[serde(default = "one")]
        click_count: u32,
        /// Skip semantic patterns and click the element's point with real input.
        #[serde(default)]
        force_physical: bool,
    },
    Fill {
        target: ElementTarget,
        text: String,
        /// Replace existing content (default) instead of appending.
        #[serde(default = "yes")]
        clear: bool,
    },
    /// Types text with real keyboard input into `target` (focused first) or the focused control.
    #[serde(rename_all = "camelCase")]
    TypeText {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<ElementTarget>,
        text: String,
    },
    Focus {
        target: ElementTarget,
    },
    /// Selects `option` in a combo box / list, or selects the target item itself when
    /// `option` is omitted.
    Select {
        target: ElementTarget,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option: Option<String>,
    },
    Check {
        target: ElementTarget,
    },
    Uncheck {
        target: ElementTarget,
    },
    Toggle {
        target: ElementTarget,
    },
    Expand {
        target: ElementTarget,
    },
    Collapse {
        target: ElementTarget,
    },
    Scroll {
        target: ElementTarget,
        direction: ScrollDirection,
        /// Pages (semantic) or 3-line wheel notches (physical fallback).
        #[serde(default = "one")]
        amount: u32,
    },
    #[serde(rename_all = "camelCase")]
    ScrollIntoView {
        target: ElementTarget,
    },
    /// Presses a chord (`["Ctrl","Shift","S"]`) after focusing `target` if given.
    Press {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<ElementTarget>,
        keys: Vec<Key>,
    },
    #[serde(rename_all = "camelCase")]
    ReadText {
        target: ElementTarget,
        #[serde(default = "default_max_chars")]
        max_chars: u32,
    },
}

impl std::fmt::Debug for DesktopAction {
    /// Never prints typed text: it may be a secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self.name();
        match self {
            Self::Fill { text, .. } | Self::TypeText { text, .. } => {
                write!(f, "{name}(chars={})", text.chars().count())
            }
            _ => write!(f, "{name}"),
        }
    }
}

impl DesktopAction {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Click { .. } => "click",
            Self::Fill { .. } => "fill",
            Self::TypeText { .. } => "typeText",
            Self::Focus { .. } => "focus",
            Self::Select { .. } => "select",
            Self::Check { .. } => "check",
            Self::Uncheck { .. } => "uncheck",
            Self::Toggle { .. } => "toggle",
            Self::Expand { .. } => "expand",
            Self::Collapse { .. } => "collapse",
            Self::Scroll { .. } => "scroll",
            Self::ScrollIntoView { .. } => "scrollIntoView",
            Self::Press { .. } => "press",
            Self::ReadText { .. } => "readText",
        }
    }

    pub fn target(&self) -> Option<&ElementTarget> {
        match self {
            Self::Click { target, .. }
            | Self::Fill { target, .. }
            | Self::Focus { target }
            | Self::Select { target, .. }
            | Self::Check { target }
            | Self::Uncheck { target }
            | Self::Toggle { target }
            | Self::Expand { target }
            | Self::Collapse { target }
            | Self::Scroll { target, .. }
            | Self::ScrollIntoView { target }
            | Self::ReadText { target, .. } => Some(target),
            Self::TypeText { target, .. } | Self::Press { target, .. } => target.as_ref(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ActionMethod {
    InvokePattern,
    SelectionItemPattern,
    TogglePattern,
    ExpandCollapsePattern,
    ValuePattern,
    RangeValuePattern,
    ScrollPattern,
    ScrollItemPattern,
    SetFocus,
    TextPattern,
    Name,
    PhysicalClick,
    PhysicalKeyboard,
    PhysicalScroll,
    WindowApi,
    /// Nothing needed doing (e.g. `check` on an already-checked box).
    NoOp,
}

/// Every action reports what ran and whether its effect was observed (spec §14, §33).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionResult {
    /// The action executed without error. See `verified` for whether its effect was seen.
    pub success: bool,
    pub executed: bool,
    pub verified: bool,
    pub method: ActionMethod,
    /// `Button "Save"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub target: String,
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub duration_ms: u64,
    /// Short post-action state, e.g. `checked`, `value="notes.txt"`, `expanded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Text read by `readText`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opened_windows: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_windows: Vec<String>,
    /// Focused element after the action, when it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WindowVisualState {
    Normal,
    Minimized,
    Maximized,
}

/// Top-level window control (spec §17).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum WindowAction {
    Focus {
        window: WindowSelector,
    },
    Move {
        window: WindowSelector,
        x: i32,
        y: i32,
    },
    Resize {
        window: WindowSelector,
        width: i32,
        height: i32,
    },
    SetBounds {
        window: WindowSelector,
        bounds: PhysicalRect,
    },
    Minimize {
        window: WindowSelector,
    },
    Maximize {
        window: WindowSelector,
    },
    Restore {
        window: WindowSelector,
    },
    Close {
        window: WindowSelector,
    },
}

impl WindowAction {
    pub fn selector(&self) -> &WindowSelector {
        match self {
            Self::Focus { window }
            | Self::Move { window, .. }
            | Self::Resize { window, .. }
            | Self::SetBounds { window, .. }
            | Self::Minimize { window }
            | Self::Maximize { window }
            | Self::Restore { window }
            | Self::Close { window } => window,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Focus { .. } => "window_focus",
            Self::Move { .. } => "window_move",
            Self::Resize { .. } => "window_resize",
            Self::SetBounds { .. } => "window_set_bounds",
            Self::Minimize { .. } => "window_minimize",
            Self::Maximize { .. } => "window_maximize",
            Self::Restore { .. } => "window_restore",
            Self::Close { .. } => "window_close",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_round_trip_with_tag() {
        let json = r#"{"action":"click","target":{"ref":"e6"},"clickCount":1}"#;
        let a: DesktopAction = serde_json::from_str(json).unwrap();
        assert!(matches!(
            a,
            DesktopAction::Click { ref target, button: MouseButton::Left, click_count: 1, force_physical: false }
                if target.reference.as_deref() == Some("e6")
        ));
        let json = r#"{"action":"press","keys":["Ctrl","Shift","S"]}"#;
        let a: DesktopAction = serde_json::from_str(json).unwrap();
        assert!(matches!(a, DesktopAction::Press { target: None, ref keys } if keys.len() == 3));
        let json = r#"{"action":"fill","target":{"locator":{"role":"Edit","label":"File name"}},"text":"notes.txt"}"#;
        let a: DesktopAction = serde_json::from_str(json).unwrap();
        assert_eq!(a.name(), "fill");
        assert!(a.target().unwrap().validate().is_ok());
    }

    #[test]
    fn debug_never_prints_text() {
        let a = DesktopAction::Fill {
            target: ElementTarget::by_ref("e1"),
            text: "hunter2".into(),
            clear: true,
        };
        assert_eq!(format!("{a:?}"), "fill(chars=7)");
    }

    #[test]
    fn targets_need_exactly_one_selector() {
        let mut t = ElementTarget::by_ref("e1");
        assert!(t.validate().is_ok());
        t.locator = Some(ElementLocator {
            name: Some("x".into()),
            ..Default::default()
        });
        assert!(t.validate().is_err());
        t.reference = None;
        assert!(t.validate().is_ok());
        t.locator = None;
        assert!(t.validate().is_err());
    }
}
