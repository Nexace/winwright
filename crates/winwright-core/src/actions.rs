//! Semantic action resolver (spec §11, §14, §33): resolve -> validate -> policy -> choose the
//! least invasive method -> execute -> verify -> report. Physical input only when UIA cannot.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use winwright_contracts::action::{
    ActionMethod, ActionResult, DesktopAction, ScrollDirection, WindowAction, WindowVisualState,
};
use winwright_contracts::backend::{
    InspectTarget, OperationContext, ScrollAmount, UiActionOutcome, UiPatternAction, UiProps,
    UiTreeRequest,
};
use winwright_contracts::element::{ControlRole, ExpandState, ToggleState, UiPattern};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::input::{InputBackend, Key, MouseButton, validate_chord};
use winwright_contracts::security::{ActionRisk, Capability, ProposedAction, TargetSummary};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::{classify_activation, is_sensitive, redacted_value};

use crate::engine::Engine;
use crate::find::Resolved;
use crate::session::Session;

/// How long an action may take to show an observable effect before we report "unverified".
const SETTLE: Duration = Duration::from_millis(600);
const SETTLE_POLL: Duration = Duration::from_millis(60);
const MAX_TEXT_CHARS: usize = 10_000;
const WHEEL_LINES_PER_STEP: i32 = 3;

/// Outcome of one dispatch before window/focus evidence is attached.
struct Step {
    method: ActionMethod,
    verified: bool,
    after: Option<UiProps>,
    text: Option<String>,
    warnings: Vec<String>,
}

impl Step {
    fn new(method: ActionMethod) -> Self {
        Self {
            method,
            verified: false,
            after: None,
            text: None,
            warnings: Vec::new(),
        }
    }
}

type WindowSet = HashMap<u64, String>;

fn window_label(title: &str, process: &str) -> String {
    if process.is_empty() {
        format!("{title:?}")
    } else {
        format!("{title:?} ({process})")
    }
}

/// Short, redacted state summary for `ActionResult.after`.
fn summarize(p: &UiProps) -> Option<String> {
    let mut parts = Vec::new();
    match p.toggle_state {
        Some(ToggleState::On) => parts.push("checked".to_owned()),
        Some(ToggleState::Off) => parts.push("unchecked".to_owned()),
        Some(ToggleState::Indeterminate) => parts.push("mixed".to_owned()),
        None => {}
    }
    match p.expand_state {
        Some(ExpandState::Expanded) => parts.push("expanded".to_owned()),
        Some(ExpandState::Collapsed) => parts.push("collapsed".to_owned()),
        _ => {}
    }
    if p.selected == Some(true) {
        parts.push("selected".to_owned());
    }
    if let Some(v) = redacted_value(p) {
        let short: String = v.chars().take(80).collect();
        parts.push(format!("value={short:?}"));
    }
    if p.focused {
        parts.push("focused".to_owned());
    }
    if !p.enabled {
        parts.push("disabled".to_owned());
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Observable differences worth calling a state change.
fn changed(before: &UiProps, after: &UiProps) -> bool {
    before.name != after.name
        || before.enabled != after.enabled
        || before.toggle_state != after.toggle_state
        || before.expand_state != after.expand_state
        || before.selected != after.selected
        || before.value != after.value
        || before.focused != after.focused
        || before.offscreen != after.offscreen
        || before.bounds != after.bounds
}

fn ensure_enabled(r: &Resolved) -> WinwrightResult<()> {
    if r.props.enabled {
        Ok(())
    } else {
        Err(WinwrightError::invalid(format!(
            "{} is disabled",
            r.label()
        )))
    }
}

fn require(r: &Resolved, pattern: UiPattern) -> WinwrightResult<()> {
    if r.props.has_pattern(pattern) {
        Ok(())
    } else {
        Err(WinwrightError::UnsupportedPattern {
            element: r.label(),
            pattern: format!("{pattern:?}"),
        })
    }
}

impl Engine {
    fn input(&self) -> WinwrightResult<&dyn InputBackend> {
        self.input
            .as_deref()
            .ok_or_else(|| WinwrightError::BackendUnavailable {
                backend: "input".into(),
                reason: "physical input is not enabled in this engine".into(),
            })
    }

    fn window_set(&self) -> WindowSet {
        self.windows
            .list_windows()
            .map(|ws| {
                ws.into_iter()
                    .map(|w| (w.hwnd, window_label(&w.title, &w.process_name)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn proposed(&self, action: &DesktopAction, target: Option<&Resolved>) -> ProposedAction {
        let summary = target.map(|r| TargetSummary {
            process: Some(self.windows.process_name(r.props.process_id)).filter(|p| !p.is_empty()),
            window: r
                .window
                .and_then(|w| self.windows.window(w).ok().flatten())
                .map(|w| w.title),
            role: Some(r.props.role.as_str().to_owned()),
            name: Some(r.props.name.clone()).filter(|n| !n.is_empty()),
        });
        let activation = || {
            target.map_or(ActionRisk::Normal, |r| {
                classify_activation(&r.props.name, &r.props.automation_id)
            })
        };
        let sensitive_target = target.is_some_and(|r| is_sensitive(&r.props));
        let (capability, risk) = match action {
            DesktopAction::ReadText { .. } if sensitive_target => {
                (Capability::ReadSensitive, ActionRisk::Sensitive)
            }
            DesktopAction::ReadText { .. } => (Capability::Observe, ActionRisk::ReadOnly),
            DesktopAction::Focus { .. }
            | DesktopAction::Scroll { .. }
            | DesktopAction::ScrollIntoView { .. }
            | DesktopAction::Expand { .. }
            | DesktopAction::Collapse { .. } => (Capability::Interact, ActionRisk::Normal),
            DesktopAction::Fill { .. } | DesktopAction::TypeText { .. } if sensitive_target => {
                (Capability::Interact, ActionRisk::Sensitive)
            }
            DesktopAction::Fill { .. } | DesktopAction::TypeText { .. } => {
                (Capability::Interact, ActionRisk::Normal)
            }
            DesktopAction::Press { keys, .. } => {
                // Enter/Space on a focused control activates it.
                let activates = keys.iter().any(|k| matches!(k, Key::Enter | Key::Space));
                (
                    Capability::PhysicalInput,
                    if activates {
                        activation()
                    } else {
                        ActionRisk::Normal
                    },
                )
            }
            DesktopAction::Click { force_physical, .. } => (
                if *force_physical {
                    Capability::PhysicalInput
                } else {
                    Capability::Interact
                },
                activation(),
            ),
            DesktopAction::Select { .. }
            | DesktopAction::Check { .. }
            | DesktopAction::Uncheck { .. }
            | DesktopAction::Toggle { .. } => (Capability::Interact, activation()),
        };
        ProposedAction {
            tool: format!("desktop_{}", action.name()),
            capability,
            risk,
            target: summary,
        }
    }

    async fn pattern(
        &self,
        r: &Resolved,
        action: UiPatternAction,
        ctx: &OperationContext,
    ) -> WinwrightResult<UiActionOutcome> {
        ctx.check("action")?;
        self.uia.execute_pattern(r.key, action, ctx).await
    }

    /// Polls for any observable effect: target state, target disappearance, window changes.
    async fn settle(
        &self,
        r: Option<&Resolved>,
        before_windows: &WindowSet,
        ctx: &OperationContext,
    ) -> (bool, Option<UiProps>) {
        let deadline = Instant::now() + SETTLE.min(ctx.remaining());
        loop {
            let now_windows = self.window_set();
            if now_windows.len() != before_windows.len()
                || now_windows.keys().any(|k| !before_windows.contains_key(k))
            {
                return (true, None);
            }
            if let Some(r) = r {
                match self.uia.refresh(r.key, ctx).await {
                    Ok(now) if changed(&r.props, &now) => return (true, Some(now)),
                    Ok(_) => {}
                    Err(WinwrightError::ElementStale { .. }) => return (true, None),
                    Err(_) => return (false, None),
                }
            }
            if Instant::now() >= deadline || ctx.cancel.is_cancelled() {
                return (false, None);
            }
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    /// Execute an element-level action (spec §14).
    pub async fn execute(
        &self,
        session: &Session,
        action: DesktopAction,
    ) -> WinwrightResult<ActionResult> {
        let started = Instant::now();
        let ctx = session.operation(self.timeout())?;
        let resolved = match action.target() {
            Some(t) => Some(self.resolve_target(session, t, &ctx).await?),
            None => None,
        };
        if let (DesktopAction::ReadText { .. }, Some(r)) = (&action, &resolved)
            && is_sensitive(&r.props)
        {
            return Err(WinwrightError::SensitiveField { element: r.label() });
        }
        self.authorize(self.proposed(&action, resolved.as_ref()))?;
        if let Some(r) = &resolved
            && self.windows.is_more_privileged(r.props.process_id)
            && !matches!(action, DesktopAction::ReadText { .. })
        {
            return Err(WinwrightError::UipiBlocked {
                target: format!(
                    "{} in {}",
                    r.label(),
                    self.windows.process_name(r.props.process_id)
                ),
            });
        }
        let mutating = !matches!(action, DesktopAction::ReadText { .. });
        let _lease = if mutating {
            Some(self.lease.try_acquire(&session.id)?)
        } else {
            None
        };
        let before_windows = if mutating {
            self.window_set()
        } else {
            WindowSet::new()
        };
        tracing::debug!(action = ?action, target = resolved.as_ref().map(|r| r.label()), "executing");

        let r = resolved.as_ref();
        let mut step = match &action {
            DesktopAction::Click {
                button,
                click_count,
                force_physical,
                ..
            } => {
                let r = r.expect("click has a target");
                self.click(r, *button, *click_count, *force_physical, &ctx)
                    .await?
            }
            DesktopAction::Fill { text, clear, .. } => {
                self.fill(r.expect("fill has a target"), text, *clear, &ctx)
                    .await?
            }
            DesktopAction::TypeText { text, .. } => self.type_text(r, text, &ctx).await?,
            DesktopAction::Focus { .. } => self.focus(r.expect("target"), &ctx).await?,
            DesktopAction::Select { option, .. } => {
                self.select(session, r.expect("target"), option.as_deref(), &ctx)
                    .await?
            }
            DesktopAction::Check { .. } => {
                self.set_toggle(r.expect("target"), Some(true), &ctx)
                    .await?
            }
            DesktopAction::Uncheck { .. } => {
                self.set_toggle(r.expect("target"), Some(false), &ctx)
                    .await?
            }
            DesktopAction::Toggle { .. } => self.set_toggle(r.expect("target"), None, &ctx).await?,
            DesktopAction::Expand { .. } => self.expand(r.expect("target"), true, &ctx).await?,
            DesktopAction::Collapse { .. } => self.expand(r.expect("target"), false, &ctx).await?,
            DesktopAction::Scroll {
                direction, amount, ..
            } => {
                self.scroll(r.expect("target"), *direction, *amount, &ctx)
                    .await?
            }
            DesktopAction::ScrollIntoView { .. } => {
                self.scroll_into_view(r.expect("target"), &ctx).await?
            }
            DesktopAction::Press { keys, .. } => self.press(r, keys, &ctx).await?,
            DesktopAction::ReadText { max_chars, .. } => {
                self.read_text(r.expect("target"), *max_chars, &ctx).await?
            }
        };

        let mut result = ActionResult {
            success: true,
            executed: true,
            verified: step.verified,
            method: step.method,
            target: r.map(Resolved::label).unwrap_or_default(),
            reference: r.map(|r| r.reference.clone()),
            duration_ms: 0,
            after: None,
            text: step.text.take(),
            opened_windows: Vec::new(),
            closed_windows: Vec::new(),
            focus: None,
            warnings: std::mem::take(&mut step.warnings),
        };
        if mutating {
            if !step.verified && step.method != ActionMethod::NoOp {
                let (seen, after) = self.settle(r, &before_windows, &ctx).await;
                result.verified = seen;
                if after.is_some() {
                    step.after = after;
                }
                if !seen {
                    result
                        .warnings
                        .push("the action ran but no observable state change was detected".into());
                }
            }
            let after_windows = self.window_set();
            for (hwnd, label) in &after_windows {
                if !before_windows.contains_key(hwnd) {
                    result.opened_windows.push(label.clone());
                }
            }
            for (hwnd, label) in &before_windows {
                if !after_windows.contains_key(hwnd) {
                    result.closed_windows.push(label.clone());
                }
            }
            if !result.opened_windows.is_empty() || !result.closed_windows.is_empty() {
                result.verified = true;
                result
                    .warnings
                    .retain(|w| !w.starts_with("the action ran but no observable"));
            }
        }
        result.after = step.after.as_ref().and_then(summarize);
        result.duration_ms = started.elapsed().as_millis() as u64;
        Ok(result)
    }

    async fn physical_point(
        &self,
        r: &Resolved,
        ctx: &OperationContext,
    ) -> WinwrightResult<PhysicalPoint> {
        if r.props.offscreen && r.props.has_pattern(UiPattern::ScrollItem) {
            let _ = self.pattern(r, UiPatternAction::ScrollIntoView, ctx).await;
        }
        let outcome = self
            .pattern(r, UiPatternAction::ClickablePoint, ctx)
            .await?;
        let bounds: Option<PhysicalRect> = outcome
            .props_after
            .as_ref()
            .and_then(|p| p.bounds)
            .or(r.props.bounds);
        let point = outcome
            .point
            .or_else(|| bounds.map(|b| b.center()))
            .ok_or_else(|| WinwrightError::InputFailed {
                reason: format!("{} has no clickable point or bounds", r.label()),
            })?;
        // Revalidate: the element at that point must be the target or inside it.
        let hit = self.uia.inspect(InspectTarget::Point(point), ctx).await?;
        let target_rid = &r.props.runtime_id;
        let hit_target = !target_rid.is_empty()
            && (hit.props.runtime_id == *target_rid
                || hit.ancestors.iter().any(|a| a.runtime_id == *target_rid));
        self.release(vec![hit.key]).await;
        if !hit_target {
            return Err(WinwrightError::InputFailed {
                reason: format!(
                    "the point {},{} for {} is covered by {}",
                    point.x,
                    point.y,
                    r.label(),
                    hit.props.label()
                ),
            });
        }
        Ok(point)
    }

    async fn physical_click(
        &self,
        r: &Resolved,
        button: MouseButton,
        count: u32,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        let input = self.input()?;
        let point = self.physical_point(r, ctx).await?;
        input.click(point, button, count, ctx).await?;
        let mut step = Step::new(ActionMethod::PhysicalClick);
        step.warnings
            .push("used physical input (no suitable UIA pattern)".into());
        Ok(step)
    }

    async fn click(
        &self,
        r: &Resolved,
        button: MouseButton,
        count: u32,
        force_physical: bool,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        ensure_enabled(r)?;
        if !(1..=3).contains(&count) {
            return Err(WinwrightError::invalid("clickCount must be 1, 2 or 3"));
        }
        if force_physical || button != MouseButton::Left || count != 1 {
            return self.physical_click(r, button, count, ctx).await;
        }
        let p = &r.props;
        if p.has_pattern(UiPattern::Invoke) {
            let out = self.pattern(r, UiPatternAction::Invoke, ctx).await?;
            let mut step = Step::new(ActionMethod::InvokePattern);
            if let Some(after) = out.props_after {
                step.verified = changed(p, &after);
                step.after = Some(after);
            } else {
                step.verified = true; // the element vanished (e.g. its dialog closed)
            }
            return Ok(step);
        }
        if p.has_pattern(UiPattern::SelectionItem) {
            let out = self.pattern(r, UiPatternAction::Select, ctx).await?;
            let mut step = Step::new(ActionMethod::SelectionItemPattern);
            step.verified = out
                .props_after
                .as_ref()
                .is_some_and(|a| a.selected == Some(true));
            step.after = out.props_after;
            return Ok(step);
        }
        if p.has_pattern(UiPattern::Toggle)
            && matches!(
                p.role,
                ControlRole::CheckBox | ControlRole::Button | ControlRole::RadioButton
            )
        {
            let out = self.pattern(r, UiPatternAction::Toggle, ctx).await?;
            let mut step = Step::new(ActionMethod::TogglePattern);
            step.verified = out
                .props_after
                .as_ref()
                .is_some_and(|a| a.toggle_state != p.toggle_state);
            step.after = out.props_after;
            return Ok(step);
        }
        if p.has_pattern(UiPattern::ExpandCollapse) {
            let expand = p.expand_state != Some(ExpandState::Expanded);
            return self.expand(r, expand, ctx).await;
        }
        if self.input.is_some() {
            return self.physical_click(r, button, count, ctx).await;
        }
        Err(WinwrightError::UnsupportedPattern {
            element: r.label(),
            pattern: "Invoke/SelectionItem/Toggle/ExpandCollapse".into(),
        })
    }

    async fn fill(
        &self,
        r: &Resolved,
        text: &str,
        clear: bool,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        ensure_enabled(r)?;
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(WinwrightError::invalid(format!(
                "text is limited to {MAX_TEXT_CHARS} characters"
            )));
        }
        let p = &r.props;
        let sensitive = is_sensitive(p);
        let writable = p.has_pattern(UiPattern::Value) && p.value_read_only != Some(true);
        if writable && (clear || !sensitive) {
            let wanted = if clear {
                text.to_owned()
            } else {
                format!("{}{}", p.value.clone().unwrap_or_default(), text)
            };
            let out = self
                .pattern(r, UiPatternAction::SetValue(wanted.clone()), ctx)
                .await?;
            let mut step = Step::new(ActionMethod::ValuePattern);
            if sensitive {
                step.warnings
                    .push("value not read back: sensitive field".into());
                step.after = out.props_after;
                return Ok(step);
            }
            let readback = out.props_after.as_ref().and_then(|a| a.value.clone());
            if readback.as_deref() == Some(wanted.as_str()) {
                step.verified = true;
                step.after = out.props_after;
                return Ok(step);
            }
            // Read-back proves the semantic write did not take effect, so a physical retype
            // cannot double-apply it.
            if self.input.is_none() {
                step.warnings
                    .push("SetValue ran but the control reports a different value".into());
                step.after = out.props_after;
                return Ok(step);
            }
            tracing::debug!("SetValue not reflected; falling back to keyboard");
        }
        self.keyboard_fill(r, text, clear, ctx).await
    }

    async fn focus_target(&self, r: &Resolved, ctx: &OperationContext) -> WinwrightResult<()> {
        if let Some(w) = r.window
            && self.windows.foreground_window()?.map(|f| f.hwnd) != Some(w)
        {
            let _ = self.windows.focus_window(w);
        }
        let out = self.pattern(r, UiPatternAction::SetFocus, ctx).await?;
        if out.props_after.as_ref().is_some_and(|a| a.focused) {
            return Ok(());
        }
        // Some providers take a moment to report focus.
        tokio::time::sleep(Duration::from_millis(50)).await;
        match self.uia.refresh(r.key, ctx).await {
            Ok(p) if p.focused => Ok(()),
            _ => Err(WinwrightError::WindowNotFocused { window: r.label() }),
        }
    }

    async fn keyboard_fill(
        &self,
        r: &Resolved,
        text: &str,
        clear: bool,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        let input = self.input()?;
        self.focus_target(r, ctx).await?;
        if clear {
            input.press_keys(&[Key::Ctrl, Key::Char('a')], ctx).await?;
            input.press_keys(&[Key::Delete], ctx).await?;
        }
        input.type_text(text, ctx).await?;
        let mut step = Step::new(ActionMethod::PhysicalKeyboard);
        step.warnings.push("used physical keyboard input".into());
        if is_sensitive(&r.props) {
            step.warnings
                .push("value not read back: sensitive field".into());
            return Ok(step);
        }
        if let Ok(after) = self.uia.refresh(r.key, ctx).await {
            step.verified = match (&after.value, clear) {
                (Some(v), true) => v == text,
                (Some(v), false) => v.ends_with(text),
                (None, _) => false,
            };
            step.after = Some(after);
        }
        Ok(step)
    }

    async fn type_text(
        &self,
        r: Option<&Resolved>,
        text: &str,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(WinwrightError::invalid(format!(
                "text is limited to {MAX_TEXT_CHARS} characters"
            )));
        }
        let input = self.input()?;
        if let Some(r) = r {
            ensure_enabled(r)?;
            self.focus_target(r, ctx).await?;
        }
        input.type_text(text, ctx).await?;
        let mut step = Step::new(ActionMethod::PhysicalKeyboard);
        if let Some(r) = r
            && !is_sensitive(&r.props)
            && let Ok(after) = self.uia.refresh(r.key, ctx).await
        {
            step.verified = after.value.as_deref().is_some_and(|v| v.contains(text));
            step.after = Some(after);
        }
        Ok(step)
    }

    async fn focus(&self, r: &Resolved, ctx: &OperationContext) -> WinwrightResult<Step> {
        ensure_enabled(r)?;
        self.focus_target(r, ctx).await?;
        let mut step = Step::new(ActionMethod::SetFocus);
        step.verified = true;
        step.after = self.uia.refresh(r.key, ctx).await.ok();
        Ok(step)
    }

    async fn set_toggle(
        &self,
        r: &Resolved,
        want: Option<bool>,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        ensure_enabled(r)?;
        let p = &r.props;
        // Radio buttons are "checked" by selecting them.
        if p.role == ControlRole::RadioButton && !p.has_pattern(UiPattern::Toggle) {
            return match want {
                Some(true) | None => {
                    if p.selected == Some(true) {
                        let mut step = Step::new(ActionMethod::NoOp);
                        step.verified = true;
                        step.after = Some(p.clone());
                        return Ok(step);
                    }
                    require(r, UiPattern::SelectionItem)?;
                    let out = self.pattern(r, UiPatternAction::Select, ctx).await?;
                    let mut step = Step::new(ActionMethod::SelectionItemPattern);
                    step.verified = out
                        .props_after
                        .as_ref()
                        .is_some_and(|a| a.selected == Some(true));
                    step.after = out.props_after;
                    Ok(step)
                }
                Some(false) => Err(WinwrightError::invalid(
                    "a radio button cannot be unchecked directly; select another option",
                )),
            };
        }
        require(r, UiPattern::Toggle)?;
        let target_state = match want {
            Some(true) => Some(ToggleState::On),
            Some(false) => Some(ToggleState::Off),
            None => None,
        };
        if target_state.is_some() && p.toggle_state == target_state {
            let mut step = Step::new(ActionMethod::NoOp);
            step.verified = true;
            step.after = Some(p.clone());
            return Ok(step);
        }
        let mut current = p.toggle_state;
        let mut after = None;
        // Tri-state checkboxes may need two toggles to reach On/Off.
        let attempts = if target_state.is_some() { 3 } else { 1 };
        for _ in 0..attempts {
            let out = self.pattern(r, UiPatternAction::Toggle, ctx).await?;
            after = out.props_after;
            let now = after.as_ref().and_then(|a| a.toggle_state);
            if target_state.is_none() || now == target_state || now == current {
                break;
            }
            current = now;
        }
        let mut step = Step::new(ActionMethod::TogglePattern);
        let final_state = after.as_ref().and_then(|a| a.toggle_state);
        step.verified = match target_state {
            Some(t) => final_state == Some(t),
            None => final_state.is_some() && final_state != p.toggle_state,
        };
        step.after = after;
        Ok(step)
    }

    async fn expand(
        &self,
        r: &Resolved,
        expand: bool,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        ensure_enabled(r)?;
        require(r, UiPattern::ExpandCollapse)?;
        let want = if expand {
            ExpandState::Expanded
        } else {
            ExpandState::Collapsed
        };
        match r.props.expand_state {
            Some(ExpandState::LeafNode) => {
                return Err(WinwrightError::invalid(format!(
                    "{} has nothing to expand",
                    r.label()
                )));
            }
            Some(s) if s == want => {
                let mut step = Step::new(ActionMethod::NoOp);
                step.verified = true;
                step.after = Some(r.props.clone());
                return Ok(step);
            }
            _ => {}
        }
        let action = if expand {
            UiPatternAction::Expand
        } else {
            UiPatternAction::Collapse
        };
        let out = self.pattern(r, action, ctx).await?;
        let mut step = Step::new(ActionMethod::ExpandCollapsePattern);
        step.verified = out
            .props_after
            .as_ref()
            .is_some_and(|a| a.expand_state == Some(want));
        step.after = out.props_after;
        Ok(step)
    }

    async fn scroll_into_view(
        &self,
        r: &Resolved,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        require(r, UiPattern::ScrollItem)?;
        let out = self
            .pattern(r, UiPatternAction::ScrollIntoView, ctx)
            .await?;
        let mut step = Step::new(ActionMethod::ScrollItemPattern);
        step.verified = out.props_after.as_ref().is_some_and(|a| !a.offscreen);
        step.after = out.props_after;
        Ok(step)
    }

    async fn scroll(
        &self,
        r: &Resolved,
        direction: ScrollDirection,
        amount: u32,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        let amount = amount.clamp(1, 50);
        if r.props.has_pattern(UiPattern::Scroll) {
            let (h, v) = match direction {
                ScrollDirection::Up => (ScrollAmount::NoAmount, ScrollAmount::LargeDecrement),
                ScrollDirection::Down => (ScrollAmount::NoAmount, ScrollAmount::LargeIncrement),
                ScrollDirection::Left => (ScrollAmount::LargeDecrement, ScrollAmount::NoAmount),
                ScrollDirection::Right => (ScrollAmount::LargeIncrement, ScrollAmount::NoAmount),
            };
            let mut after = None;
            for _ in 0..amount {
                let out = self
                    .pattern(
                        r,
                        UiPatternAction::Scroll {
                            horizontal: h,
                            vertical: v,
                        },
                        ctx,
                    )
                    .await?;
                after = out.props_after;
            }
            let mut step = Step::new(ActionMethod::ScrollPattern);
            step.verified = after
                .as_ref()
                .and_then(|a| a.scroll_percent)
                .zip(r.props.scroll_percent)
                .is_some_and(|(a, b)| a != b);
            if !step.verified {
                step.warnings
                    .push("scroll position did not change (already at the end?)".into());
            }
            step.after = after;
            return Ok(step);
        }
        let input = self.input()?;
        let point =
            r.props
                .bounds
                .map(|b| b.center())
                .ok_or_else(|| WinwrightError::InputFailed {
                    reason: format!("{} has no bounds to scroll over", r.label()),
                })?;
        let lines = amount as i32 * WHEEL_LINES_PER_STEP;
        let (x, y) = match direction {
            ScrollDirection::Up => (0, -lines),
            ScrollDirection::Down => (0, lines),
            ScrollDirection::Left => (-lines, 0),
            ScrollDirection::Right => (lines, 0),
        };
        input.scroll(point, x, y, ctx).await?;
        let mut step = Step::new(ActionMethod::PhysicalScroll);
        step.warnings
            .push("used the mouse wheel (no ScrollPattern)".into());
        Ok(step)
    }

    async fn press(
        &self,
        r: Option<&Resolved>,
        keys: &[Key],
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        validate_chord(keys).map_err(WinwrightError::invalid)?;
        let input = self.input()?;
        if let Some(r) = r {
            ensure_enabled(r)?;
            self.focus_target(r, ctx).await?;
        }
        input.press_keys(keys, ctx).await?;
        Ok(Step::new(ActionMethod::PhysicalKeyboard))
    }

    async fn read_text(
        &self,
        r: &Resolved,
        max_chars: u32,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        if is_sensitive(&r.props) {
            return Err(WinwrightError::SensitiveField { element: r.label() });
        }
        let out = self
            .pattern(
                r,
                UiPatternAction::GetText {
                    max_chars: max_chars.clamp(1, 1_000_000),
                },
                ctx,
            )
            .await?;
        let (text, source) = out.text.unwrap_or_default();
        let mut step = Step::new(match source {
            "TextPattern" => ActionMethod::TextPattern,
            "ValuePattern" => ActionMethod::ValuePattern,
            _ => ActionMethod::Name,
        });
        step.verified = true;
        step.text = Some(text);
        Ok(step)
    }

    /// Selects an item: the target itself, or `option` inside a combo box / list / tab / tree.
    async fn select(
        &self,
        session: &Session,
        r: &Resolved,
        option: Option<&str>,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        ensure_enabled(r)?;
        let Some(option) = option else {
            require(r, UiPattern::SelectionItem)?;
            let out = self.pattern(r, UiPatternAction::Select, ctx).await?;
            let mut step = Step::new(ActionMethod::SelectionItemPattern);
            step.verified = out
                .props_after
                .as_ref()
                .is_some_and(|a| a.selected == Some(true));
            step.after = out.props_after;
            return Ok(step);
        };
        let was_collapsed = r.props.has_pattern(UiPattern::ExpandCollapse)
            && r.props.expand_state == Some(ExpandState::Collapsed);
        if was_collapsed {
            // Many combo boxes only materialize their items while open.
            let _ = self.pattern(r, UiPatternAction::Expand, ctx).await;
        }
        let item = self.find_item(session, r, option, ctx).await;
        let result = match item {
            Ok(item) => {
                if item.props.offscreen && item.props.has_pattern(UiPattern::ScrollItem) {
                    let _ = self
                        .pattern(&item, UiPatternAction::ScrollIntoView, ctx)
                        .await;
                }
                let (action, method) = if item.props.has_pattern(UiPattern::SelectionItem) {
                    (UiPatternAction::Select, ActionMethod::SelectionItemPattern)
                } else if item.props.has_pattern(UiPattern::Invoke) {
                    (UiPatternAction::Invoke, ActionMethod::InvokePattern)
                } else {
                    return Err(WinwrightError::UnsupportedPattern {
                        element: item.label(),
                        pattern: "SelectionItem/Invoke".into(),
                    });
                };
                let out = self.pattern(&item, action, ctx).await?;
                let mut step = Step::new(method);
                step.verified = out
                    .props_after
                    .as_ref()
                    .is_some_and(|a| a.selected == Some(true));
                self.release(vec![item.key]).await;
                Ok(step)
            }
            Err(WinwrightError::ElementNotFound { .. })
                if r.props.has_pattern(UiPattern::Value)
                    && r.props.value_read_only == Some(false) =>
            {
                let out = self
                    .pattern(r, UiPatternAction::SetValue(option.to_owned()), ctx)
                    .await?;
                let mut step = Step::new(ActionMethod::ValuePattern);
                step.verified =
                    out.props_after.as_ref().and_then(|a| a.value.as_deref()) == Some(option);
                Ok(step)
            }
            Err(e) => Err(e),
        };
        if was_collapsed {
            // Close the drop-down again if selecting did not already.
            if let Ok(now) = self.uia.refresh(r.key, ctx).await
                && now.expand_state == Some(ExpandState::Expanded)
            {
                let _ = self.pattern(r, UiPatternAction::Collapse, ctx).await;
            }
        }
        let mut step = result?;
        if let Ok(now) = self.uia.refresh(r.key, ctx).await {
            if !step.verified {
                step.verified = now.value.as_deref() == Some(option);
            }
            step.after = Some(now);
        }
        Ok(step)
    }

    /// Finds `option` among the target's descendants (exact name, else a unique contains).
    async fn find_item(
        &self,
        _session: &Session,
        container: &Resolved,
        option: &str,
        ctx: &OperationContext,
    ) -> WinwrightResult<Resolved> {
        let tree = self
            .uia
            .capture_tree(
                UiTreeRequest {
                    root: winwright_contracts::backend::TreeRoot::Element(container.key),
                    max_depth: 6,
                    max_nodes: 3_000,
                    max_children: 2_000,
                    include_offscreen: true,
                },
                ctx,
            )
            .await?;
        let mut all = Vec::new();
        let mut exact = Vec::new();
        let mut partial = Vec::new();
        fn walk<'a>(
            n: &'a winwright_contracts::backend::UiNode,
            depth: usize,
            all: &mut Vec<winwright_contracts::backend::ElementKey>,
            exact: &mut Vec<&'a winwright_contracts::backend::UiNode>,
            partial: &mut Vec<&'a winwright_contracts::backend::UiNode>,
            option: &str,
        ) {
            all.push(n.key);
            let item_role = matches!(
                n.props.role,
                ControlRole::ListItem
                    | ControlRole::TreeItem
                    | ControlRole::TabItem
                    | ControlRole::MenuItem
                    | ControlRole::DataItem
                    | ControlRole::RadioButton
            );
            if depth > 0 && item_role {
                let name = n.props.name.trim();
                if name.eq_ignore_ascii_case(option.trim()) {
                    exact.push(n);
                } else if name.to_lowercase().contains(&option.trim().to_lowercase()) {
                    partial.push(n);
                }
            }
            for c in &n.children {
                walk(c, depth + 1, all, exact, partial, option);
            }
        }
        walk(&tree.root, 0, &mut all, &mut exact, &mut partial, option);
        let pick = match (exact.len(), partial.len()) {
            (1, _) => Some(exact[0]),
            (0, 1) => Some(partial[0]),
            _ => None,
        };
        let ambiguous = exact.len() > 1 || (exact.is_empty() && partial.len() > 1);
        let picked = pick.map(|n| Resolved {
            reference: String::new(),
            key: n.key,
            props: n.props.clone(),
            window: container.window,
        });
        let keep = picked.as_ref().map(|p| p.key);
        self.release(all.into_iter().filter(|k| Some(*k) != keep).collect())
            .await;
        match picked {
            Some(p) => Ok(p),
            None if ambiguous => Err(WinwrightError::ElementAmbiguous {
                locator: format!("option {option:?} in {}", container.label()),
                matches: exact
                    .iter()
                    .chain(partial.iter())
                    .take(10)
                    .map(|n| n.props.label())
                    .collect(),
            }),
            None => Err(WinwrightError::ElementNotFound {
                locator: format!("option {option:?} in {}", container.label()),
            }),
        }
    }

    /// Top-level window control (spec §17).
    pub async fn window_action(
        &self,
        session: &Session,
        action: WindowAction,
    ) -> WinwrightResult<ActionResult> {
        let started = Instant::now();
        let ctx = session.operation(self.timeout())?;
        let window = self.find_window(action.selector())?;
        self.authorize(ProposedAction {
            tool: action.name().into(),
            capability: Capability::WindowControl,
            risk: ActionRisk::Normal,
            target: Some(TargetSummary {
                process: Some(window.process_name.clone()),
                window: Some(window.title.clone()),
                role: Some("Window".into()),
                name: None,
            }),
        })?;
        if self.windows.is_more_privileged(window.process_id) {
            return Err(WinwrightError::UipiBlocked {
                target: window_label(&window.title, &window.process_name),
            });
        }
        let _lease = self.lease.try_acquire(&session.id)?;
        let hwnd = window.hwnd;
        let mut warnings = Vec::new();
        let expected_bounds = match &action {
            WindowAction::Move { x, y, .. } => Some(PhysicalRect::new(
                *x,
                *y,
                x + window.bounds.width(),
                y + window.bounds.height(),
            )),
            WindowAction::Resize { width, height, .. } => Some(PhysicalRect::new(
                window.bounds.left,
                window.bounds.top,
                window.bounds.left + width,
                window.bounds.top + height,
            )),
            WindowAction::SetBounds { bounds, .. } => Some(*bounds),
            _ => None,
        };
        match &action {
            WindowAction::Focus { .. } => self.windows.focus_window(hwnd)?,
            WindowAction::Move { .. }
            | WindowAction::Resize { .. }
            | WindowAction::SetBounds { .. } => self
                .windows
                .set_window_bounds(hwnd, expected_bounds.expect("bounds action"))?,
            WindowAction::Minimize { .. } => self
                .windows
                .set_window_state(hwnd, WindowVisualState::Minimized)?,
            WindowAction::Maximize { .. } => self
                .windows
                .set_window_state(hwnd, WindowVisualState::Maximized)?,
            WindowAction::Restore { .. } => self
                .windows
                .set_window_state(hwnd, WindowVisualState::Normal)?,
            WindowAction::Close { .. } => self.windows.close_window(hwnd)?,
        }
        // Verify by polling the window's state.
        let deadline = Instant::now()
            + Duration::from_millis(if matches!(action, WindowAction::Close { .. }) {
                2_000
            } else {
                800
            });
        let verified = loop {
            let now = self.windows.window(hwnd)?;
            let ok = match (&action, &now) {
                (WindowAction::Close { .. }, None) => true,
                (WindowAction::Close { .. }, Some(_)) => false,
                (_, None) => false,
                (WindowAction::Focus { .. }, Some(w)) => w.foreground,
                (WindowAction::Minimize { .. }, Some(w)) => w.minimized,
                (WindowAction::Maximize { .. }, Some(w)) => w.maximized,
                (WindowAction::Restore { .. }, Some(w)) => !w.minimized && !w.maximized,
                (_, Some(w)) => expected_bounds.is_some_and(|e| {
                    (w.bounds.left - e.left).abs() <= 2
                        && (w.bounds.top - e.top).abs() <= 2
                        && (w.bounds.right - e.right).abs() <= 2
                        && (w.bounds.bottom - e.bottom).abs() <= 2
                }),
            };
            if ok || Instant::now() >= deadline || ctx.cancel.is_cancelled() {
                break ok;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        if !verified {
            warnings.push(match action {
                WindowAction::Close { .. } => {
                    "the window is still open (it may be asking to save changes)".to_owned()
                }
                _ => "the window did not reach the requested state".to_owned(),
            });
        }
        Ok(ActionResult {
            success: true,
            executed: true,
            verified,
            method: ActionMethod::WindowApi,
            target: format!(
                "Window {}",
                window_label(&window.title, &window.process_name)
            ),
            reference: None,
            duration_ms: started.elapsed().as_millis() as u64,
            after: None,
            text: None,
            opened_windows: Vec::new(),
            closed_windows: if matches!(action, WindowAction::Close { .. }) && verified {
                vec![window_label(&window.title, &window.process_name)]
            } else {
                Vec::new()
            },
            focus: None,
            warnings,
        })
    }
}
