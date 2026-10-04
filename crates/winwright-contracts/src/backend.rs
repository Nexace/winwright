//! Backend traits implemented by platform adapters (spec §64).
//!
//! Everything crossing these traits is owned data. The UIA worker keeps live COM elements in
//! epoch-qualified slots and hands out [`ElementKey`]s instead.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::action::WindowVisualState;
use crate::element::{ControlRole, ExpandState, ToggleState, UiPattern};
use crate::error::{WinwrightError, WinwrightResult};
use crate::geometry::{PhysicalPoint, PhysicalRect};
use crate::ids::SessionId;
use crate::window::WindowInfo;

pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = WinwrightResult<T>> + Send + 'a>>;

/// Carried by every long operation: who asked, when it must finish, and how to stop it.
#[derive(Clone, Debug)]
pub struct OperationContext {
    pub session_id: SessionId,
    pub action_epoch: u64,
    /// When the operation started; timeouts report the time elapsed since then.
    pub started: Instant,
    pub deadline: Instant,
    pub cancel: CancellationToken,
}

impl OperationContext {
    pub fn new(session_id: SessionId, timeout: Duration, cancel: CancellationToken) -> Self {
        let started = Instant::now();
        Self {
            session_id,
            action_epoch: 0,
            started,
            deadline: started + timeout,
            cancel,
        }
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Call before dispatching work and between provider calls.
    pub fn check(&self, operation: &str) -> WinwrightResult<()> {
        if self.cancel.is_cancelled() {
            return Err(WinwrightError::Cancelled);
        }
        if self.remaining().is_zero() {
            return Err(WinwrightError::Timeout {
                operation: operation.to_owned(),
                elapsed_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
            });
        }
        Ok(())
    }
}

/// Internal element handle: a slot in the UIA worker, qualified by the worker's epoch so a
/// restarted worker can never resolve an old key. Not a COM pointer, never serialized.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ElementKey {
    pub worker_epoch: u64,
    pub slot: u64,
}

/// Owned, cached UIA properties of one element.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiProps {
    pub control_type_id: i32,
    pub role: ControlRole,
    pub name: String,
    pub automation_id: String,
    pub class_name: String,
    pub framework_id: String,
    pub help_text: String,
    pub process_id: u32,
    pub native_window_handle: Option<u64>,
    pub runtime_id: Vec<i32>,
    pub bounds: Option<PhysicalRect>,
    pub enabled: bool,
    pub offscreen: bool,
    pub focused: bool,
    pub keyboard_focusable: bool,
    pub is_password: bool,
    /// Name of the element referenced by UIA `LabeledBy`, when the provider sets it.
    pub labeled_by: Option<String>,
    pub patterns: Vec<UiPattern>,
    /// Never populated for password fields: the backend does not read them at all.
    pub value: Option<String>,
    pub value_read_only: Option<bool>,
    pub toggle_state: Option<ToggleState>,
    pub expand_state: Option<ExpandState>,
    pub selected: Option<bool>,
    /// `(horizontal, vertical)` scroll percent from `ScrollPattern`; -1 means not scrollable.
    pub scroll_percent: Option<(f64, f64)>,
}

impl UiProps {
    pub fn has_pattern(&self, pattern: UiPattern) -> bool {
        self.patterns.contains(&pattern)
    }

    /// `Button "Save"` style label for messages and paths.
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            self.role.as_str().to_owned()
        } else {
            format!("{} {:?}", self.role.as_str(), self.name)
        }
    }
}

/// What a ref remembers about its element, for validation and re-resolution (spec §9).
/// Bounds are deliberately excluded: elements move without becoming different elements.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ElementIdentity {
    pub process_id: u32,
    pub runtime_id: Vec<i32>,
    pub control_type_id: i32,
    pub automation_id: String,
    pub name: String,
    pub class_name: String,
    pub framework_id: String,
    /// Hash of the ancestor chain's role / name / AutomationId.
    pub ancestor_fingerprint: u64,
}

impl ElementIdentity {
    pub fn from_props(props: &UiProps, ancestor_fingerprint: u64) -> Self {
        Self {
            process_id: props.process_id,
            runtime_id: props.runtime_id.clone(),
            control_type_id: props.control_type_id,
            automation_id: props.automation_id.clone(),
            name: props.name.clone(),
            class_name: props.class_name.clone(),
            framework_id: props.framework_id.clone(),
            ancestor_fingerprint,
        }
    }

    /// Same native element: runtime ids agree and the static properties did not change.
    /// Name is excluded because labels legitimately change (`Play` → `Pause`).
    pub fn same_element(&self, other: &Self) -> bool {
        !self.runtime_id.is_empty()
            && self.runtime_id == other.runtime_id
            && self.process_id == other.process_id
            && self.control_type_id == other.control_type_id
            && self.automation_id == other.automation_id
            && self.class_name == other.class_name
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiNode {
    pub key: ElementKey,
    pub props: UiProps,
    pub children: Vec<UiNode>,
    /// Children the provider reported; larger than `children.len()` when capture was capped.
    pub children_total: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiTree {
    pub root: UiNode,
    pub node_count: u32,
    /// True when the node budget or depth limit stopped the walk.
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRoot {
    Window(u64),
    Element(ElementKey),
    Desktop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiTreeRequest {
    pub root: TreeRoot,
    pub max_depth: u32,
    /// Raw capture budget; the core filters this down to the snapshot's `maxNodes`.
    pub max_nodes: u32,
    /// Children fetched per parent before truncation.
    pub max_children: u32,
    pub include_offscreen: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectTarget {
    Point(PhysicalPoint),
    Focused,
    Element(ElementKey),
}

#[derive(Clone, Debug, PartialEq)]
pub struct UiInspection {
    pub key: ElementKey,
    pub props: UiProps,
    /// Outermost first, excluding the desktop root.
    pub ancestors: Vec<UiProps>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAmount {
    LargeDecrement,
    SmallDecrement,
    NoAmount,
    LargeIncrement,
    SmallIncrement,
}

/// One control-pattern operation executed on the UIA worker.
#[derive(Clone, PartialEq)]
pub enum UiPatternAction {
    Invoke,
    /// `SelectionItemPattern.Select`.
    Select,
    Toggle,
    Expand,
    Collapse,
    SetValue(String),
    ScrollIntoView,
    Scroll {
        horizontal: ScrollAmount,
        vertical: ScrollAmount,
    },
    SetFocus,
    /// `TextPattern` document text, falling back to `ValuePattern` then `Name`.
    GetText {
        max_chars: u32,
    },
    ClickablePoint,
}

impl std::fmt::Debug for UiPatternAction {
    /// Never prints a value being set: it may be a secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SetValue(v) => write!(f, "SetValue(chars={})", v.chars().count()),
            Self::Invoke => f.write_str("Invoke"),
            Self::Select => f.write_str("Select"),
            Self::Toggle => f.write_str("Toggle"),
            Self::Expand => f.write_str("Expand"),
            Self::Collapse => f.write_str("Collapse"),
            Self::ScrollIntoView => f.write_str("ScrollIntoView"),
            Self::Scroll {
                horizontal,
                vertical,
            } => write!(f, "Scroll({horizontal:?}, {vertical:?})"),
            Self::SetFocus => f.write_str("SetFocus"),
            Self::GetText { max_chars } => write!(f, "GetText({max_chars})"),
            Self::ClickablePoint => f.write_str("ClickablePoint"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiActionOutcome {
    /// Fresh properties after the operation; `None` when the element disappeared.
    pub props_after: Option<UiProps>,
    /// For `GetText`: the text and which source provided it (`TextPattern`/`ValuePattern`/`Name`).
    pub text: Option<(String, &'static str)>,
    /// For `ClickablePoint`: the provider's clickable point, if it has one.
    pub point: Option<PhysicalPoint>,
}

/// A live UI-change counter; dropping it lets the backend stop listening.
pub struct EventSubscription {
    pub rx: tokio::sync::watch::Receiver<u64>,
    release: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl EventSubscription {
    pub fn new(
        rx: tokio::sync::watch::Receiver<u64>,
        release: impl FnOnce() + Send + Sync + 'static,
    ) -> Self {
        Self {
            rx,
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// Top-level window enumeration and control. Fast Win32 calls that never block on hung apps.
pub trait WindowBackend: Send + Sync {
    fn list_windows(&self) -> WinwrightResult<Vec<WindowInfo>>;
    fn foreground_window(&self) -> WinwrightResult<Option<WindowInfo>>;
    fn window(&self, hwnd: u64) -> WinwrightResult<Option<WindowInfo>>;
    fn cursor_position(&self) -> WinwrightResult<PhysicalPoint>;
    fn process_name(&self, pid: u32) -> String;

    /// When the person last used the keyboard or mouse (a tick count in milliseconds; never
    /// which key). `None` when unknown.
    fn last_input_ms(&self) -> Option<u64> {
        None
    }

    /// Restores if minimized and brings to the foreground. Fails with `WINDOW_NOT_FOCUSED`
    /// when Windows refuses the foreground change.
    fn focus_window(&self, hwnd: u64) -> WinwrightResult<()>;
    fn set_window_state(&self, hwnd: u64, state: WindowVisualState) -> WinwrightResult<()>;
    /// Moves/resizes so the *visible frame* (DWM bounds) equals `bounds`.
    fn set_window_bounds(&self, hwnd: u64, bounds: PhysicalRect) -> WinwrightResult<()>;
    /// Posts `WM_CLOSE`; the application may still prompt (e.g. unsaved changes).
    fn close_window(&self, hwnd: u64) -> WinwrightResult<()>;
    /// True when `pid` runs at a higher integrity level than this process (UIPI blocks input).
    fn is_more_privileged(&self, pid: u32) -> bool;
}

/// Thread-safe mailbox proxy to the dedicated MTA UIA worker.
pub trait UiAutomationBackend: Send + Sync {
    /// Changes whenever the worker restarts; keys from older epochs are dead.
    fn worker_epoch(&self) -> u64;

    fn capture_tree<'a>(
        &'a self,
        request: UiTreeRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiTree>;

    fn inspect<'a>(
        &'a self,
        target: InspectTarget,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiInspection>;

    /// Drops worker slots the caller no longer references.
    fn release<'a>(&'a self, keys: Vec<ElementKey>) -> BackendFuture<'a, ()>;

    /// Starts listening for UI changes (window opened/closed, focus moved) for as long as the
    /// returned subscription lives. Listening costs cross-process event traffic, so backends
    /// attach only while at least one subscription exists. Waits use the counter only to wake
    /// early and always re-check real state. `None` when the backend has no event source.
    fn events(&self) -> Option<EventSubscription> {
        None
    }

    /// Re-reads the element's properties. `ELEMENT_STALE` when it no longer exists.
    fn refresh<'a>(
        &'a self,
        key: ElementKey,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiProps>;

    /// Runs one control-pattern operation. Providers can block; a timeout after dispatch is
    /// reported as `ACTION_OUTCOME_UNKNOWN`, never retried.
    fn execute_pattern<'a>(
        &'a self,
        key: ElementKey,
        action: UiPatternAction,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiActionOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_report_the_real_elapsed_time() {
        let ctx = OperationContext::new(
            SessionId::parse("t").unwrap(),
            Duration::from_millis(10),
            CancellationToken::new(),
        );
        std::thread::sleep(Duration::from_millis(30));
        let Err(WinwrightError::Timeout { elapsed_ms, .. }) = ctx.check("desktop_click") else {
            panic!("expected a timeout");
        };
        assert!(elapsed_ms >= 30, "elapsed_ms={elapsed_ms}");
    }

    #[test]
    fn set_value_debug_hides_text() {
        let a = UiPatternAction::SetValue("hunter2".into());
        assert_eq!(format!("{a:?}"), "SetValue(chars=7)");
    }
}
