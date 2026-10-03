//! Semantic action resolver (spec §11, §14, §33): resolve -> validate -> policy -> choose the
//! least invasive method -> execute -> verify -> report. Physical input only when UIA cannot.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use winwright_contracts::action::{
    ActionMethod, ActionResult, DesktopAction, ScrollDirection, WindowAction, WindowVisualState,
};
use winwright_contracts::backend::{
    ElementKey, InspectTarget, OperationContext, ScrollAmount, TreeRoot, UiActionOutcome, UiNode,
    UiPatternAction, UiProps, UiTreeRequest,
};
use winwright_contracts::element::{ControlRole, ExpandState, ToggleState, UiPattern};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::input::{InputBackend, Key, MouseButton, validate_chord};
use winwright_contracts::security::{ActionRisk, Capability, ProposedAction, TargetSummary};

/// One-line description shown in confirmation dialogs (never includes typed text).
/// `focused` names the element untargeted keys reach, when known.
fn describe(
    action: &DesktopAction,
    target: Option<&Resolved>,
    focused: Option<&UiProps>,
) -> String {
    let on = match (target, focused) {
        (Some(r), _) => r.label(),
        (None, Some(p)) => format!("{} (focused)", p.label()),
        (None, None) => "the focused window".to_owned(),
    };
    match action {
        DesktopAction::Click { click_count, .. } if *click_count > 1 => {
            format!("Double-click {on}")
        }
        DesktopAction::Click { .. } => format!("Click {on}"),
        DesktopAction::Fill { text, .. } => {
            format!("Enter {} characters into {on}", text.chars().count())
        }
        DesktopAction::TypeText { text, .. } => {
            format!("Type {} characters into {on}", text.chars().count())
        }
        DesktopAction::Focus { .. } => format!("Focus {on}"),
        DesktopAction::Select {
            option: Some(o), ..
        } => format!("Select {o:?} in {on}"),
        DesktopAction::Select { option: None, .. } => format!("Select {on}"),
        DesktopAction::Check { .. } => format!("Check {on}"),
        DesktopAction::Uncheck { .. } => format!("Uncheck {on}"),
        DesktopAction::Toggle { .. } => format!("Toggle {on}"),
        DesktopAction::Expand { .. } => format!("Expand {on}"),
        DesktopAction::Collapse { .. } => format!("Collapse {on}"),
        DesktopAction::Scroll { .. } | DesktopAction::ScrollIntoView { .. } => {
            format!("Scroll {on}")
        }
        DesktopAction::Press { keys, .. } => {
            let chord: Vec<String> = keys.iter().map(ToString::to_string).collect();
            format!("Press {} in {on}", chord.join("+"))
        }
        DesktopAction::ReadText { .. } => format!("Read text of {on}"),
    }
}
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::{
    classify_activation, classify_submit, command_capability, console_capability, is_affirmative,
    is_launcher, is_sensitive, redacted_value, stricter,
};

use crate::engine::Engine;
use crate::find::Resolved;
use crate::session::Session;

/// How long an action may take to show an observable effect before we report "unverified".
const SETTLE: Duration = Duration::from_millis(600);
const SETTLE_POLL: Duration = Duration::from_millis(60);
const MAX_TEXT_CHARS: usize = 10_000;

/// Outcome of one dispatch before window/focus evidence is attached.
struct Step {
    method: ActionMethod,
    verified: bool,
    after: Option<UiProps>,
    text: Option<String>,
    warnings: Vec<String>,
    /// The exact state the target must reach; re-checked briefly because some providers
    /// (Win32 radio buttons, list boxes) update a moment after the call returns.
    expect: Option<Expect>,
    /// The target as this step left it just before its input (e.g. after focusing it), so the
    /// step's own preparation is never mistaken for an effect of the action.
    baseline: Option<UiProps>,
}

impl Step {
    fn new(method: ActionMethod) -> Self {
        Self {
            method,
            verified: false,
            after: None,
            text: None,
            warnings: Vec::new(),
            expect: None,
            baseline: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Expect {
    Selected,
    Toggle(ToggleState),
    ToggleChangedFrom(Option<ToggleState>),
    Expand(ExpandState),
    Visible,
    /// What a combo box shows (trimmed, ASCII case-insensitive).
    Value(String),
    /// The field's exact value after a keyboard fill.
    ExactValue(String),
    /// Typed text arrived: the value changed and now contains it.
    Typed {
        before: Option<String>,
        text: String,
    },
}

impl Expect {
    fn met(&self, p: &UiProps) -> bool {
        match self {
            Self::Selected => p.selected == Some(true),
            Self::Toggle(t) => p.toggle_state == Some(*t),
            Self::ToggleChangedFrom(before) => {
                p.toggle_state.is_some() && p.toggle_state != *before
            }
            Self::Expand(e) => p.expand_state == Some(*e),
            Self::Visible => !p.offscreen,
            Self::Value(v) => p
                .value
                .as_deref()
                .is_some_and(|x| x.trim().eq_ignore_ascii_case(v)),
            Self::ExactValue(v) => p.value.as_deref() == Some(v.as_str()),
            Self::Typed { before, text } => {
                p.value != *before
                    && p.value
                        .as_deref()
                        .is_some_and(|v| v.contains(text.as_str()))
            }
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

/// Text fields, where Delete edits text instead of deleting the item.
fn is_text_input(p: &UiProps) -> bool {
    matches!(p.role, ControlRole::Edit | ControlRole::Document)
        || (p.has_pattern(UiPattern::Value) && p.value_read_only == Some(false))
}

/// Risk of Enter reaching `receiver`: it activates a control, and submits a text field (in a
/// message box, that sends the message).
fn enter_risk(receiver: Option<&UiProps>) -> ActionRisk {
    receiver.map_or(ActionRisk::Normal, |p| {
        if is_text_input(p) {
            classify_submit(&p.name, &p.automation_id)
        } else {
            classify_activation(&p.name, &p.automation_id)
        }
    })
}

/// Typed `\n` and `\r` are Enter presses and `\t` is Tab (winwright-input), so typed text can
/// submit the field it goes into or, after a Tab, whatever control has focus by then.
fn typed_risk(text: &str, receiver: Option<&UiProps>) -> ActionRisk {
    match text.rfind(['\n', '\r']) {
        None => ActionRisk::Normal,
        Some(last_enter) if text[..last_enter].contains('\t') => ActionRisk::Sensitive,
        Some(_) => enter_risk(receiver),
    }
}

/// The lines typed text submits, one per line break; text after the last break is only typed.
/// The first line is judged both alone (typing replaced a selected value, as in an address bar)
/// and after the field's current value.
fn submitted_lines(value: Option<&str>, text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.split(['\n', '\r']).map(str::to_owned).collect();
    lines.pop();
    if let (Some(first), Some(value)) = (lines.first(), value.filter(|v| !v.is_empty())) {
        let joined = format!("{value}{first}");
        lines.push(joined);
    }
    lines
}

/// Window edges beyond this are refused; real virtual desktops are far smaller.
const MAX_WINDOW_COORD: i64 = 1 << 20;

/// Validated window bounds from a position and size (model input: never overflow or invert).
fn window_rect(left: i32, top: i32, width: i64, height: i64) -> WinwrightResult<PhysicalRect> {
    let (l, t) = (i64::from(left), i64::from(top));
    let (r, b) = (l + width, t + height);
    if width <= 0 || height <= 0 || [l, t, r, b].iter().any(|v| v.abs() > MAX_WINDOW_COORD) {
        return Err(WinwrightError::invalid(format!(
            "window bounds at {left},{top} sized {width}x{height} are out of range"
        )));
    }
    // In range of i32 by the check above.
    Ok(PhysicalRect::new(left, top, r as i32, b as i32))
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

    /// Risk of what a dialog asks when the action says yes to it ("Delete 3 files?", then
    /// "Yes"): its title and text, never its other buttons. Only owned windows and standard
    /// dialogs count, so unrelated text in a main window cannot make "OK" look risky. `None`
    /// when the action does not answer a dialog.
    async fn dialog_risk(
        &self,
        action: &DesktopAction,
        target: Option<&Resolved>,
        focused: Option<&UiProps>,
        ctx: &OperationContext,
    ) -> Option<ActionRisk> {
        let activates = match action {
            DesktopAction::Click { .. } => true,
            DesktopAction::Press { keys, .. } => {
                keys.iter().any(|k| matches!(k, Key::Enter | Key::Space))
            }
            _ => false,
        };
        let receiver = target.map(|r| &r.props).or(focused);
        if !activates || !receiver.is_some_and(|p| is_affirmative(&p.name)) {
            return None;
        }
        let window = match target {
            Some(r) => r.window.and_then(|w| self.windows.window(w).ok().flatten()),
            None => self.windows.foreground_window().ok().flatten(),
        };
        let window = window.filter(|w| w.owner_hwnd.is_some() || w.class_name == "#32770")?;
        let mut text = vec![window.title.clone()];
        let request = UiTreeRequest {
            root: TreeRoot::Window(window.hwnd),
            max_depth: 6,
            max_nodes: 300,
            max_children: 100,
            include_offscreen: false,
        };
        // Without the tree the title still says a lot ("Delete File").
        if let Ok(tree) = self.uia.capture_tree(request, ctx).await {
            fn walk(n: &UiNode, text: &mut Vec<String>, keys: &mut Vec<ElementKey>) {
                keys.push(n.key);
                if n.props.role == ControlRole::Text {
                    text.push(n.props.name.clone());
                }
                for c in &n.children {
                    walk(c, text, keys);
                }
            }
            let mut keys = Vec::new();
            walk(&tree.root, &mut text, &mut keys);
            self.release(keys).await;
        }
        Some(classify_activation(&text.join("\n"), ""))
    }

    /// `focused` is the element keys without a target would reach; `dialog` is the risk of
    /// what an affirmative button's dialog asks.
    fn proposed(
        &self,
        action: &DesktopAction,
        target: Option<&Resolved>,
        focused: Option<&UiProps>,
        dialog: Option<ActionRisk>,
    ) -> ProposedAction {
        let summary = target
            .map(|r| TargetSummary {
                process: Some(self.windows.process_name(r.props.process_id))
                    .filter(|p| !p.is_empty()),
                window: r
                    .window
                    .and_then(|w| self.windows.window(w).ok().flatten())
                    .map(|w| w.title),
                role: Some(r.props.role.as_str().to_owned()),
                name: Some(r.props.name.clone()).filter(|n| !n.is_empty()),
            })
            .or_else(|| {
                focused.map(|p| TargetSummary {
                    process: Some(self.windows.process_name(p.process_id))
                        .filter(|p| !p.is_empty()),
                    window: None,
                    role: Some(p.role.as_str().to_owned()),
                    name: Some(p.name.clone()).filter(|n| !n.is_empty()),
                })
            });
        let activation = || {
            target
                .map(|r| &r.props)
                .or(focused)
                .map_or(ActionRisk::Normal, |p| {
                    classify_activation(&p.name, &p.automation_id)
                })
        };
        let sensitive_target = target.is_some_and(|r| is_sensitive(&r.props));
        let receiver = target.map(|r| &r.props).or(focused);
        // Enter that runs a command line is shell execution, however the text got there: in a
        // terminal, or in the Run box, Start search, or an address bar holding a shell command.
        let process = receiver.map(|p| self.windows.process_name(p.process_id));
        let runs = |lines: &[String]| -> Option<Capability> {
            let process = process.as_deref().filter(|_| !lines.is_empty())?;
            if let Some(shell) = console_capability(process) {
                return Some(shell);
            }
            if !(is_launcher(process) && receiver.is_some_and(is_text_input)) {
                return None;
            }
            let command = lines
                .iter()
                .map(|l| command_capability(l))
                .fold(Capability::ProcessLaunch, stricter);
            matches!(command, Capability::Shell | Capability::PowerShell).then_some(command)
        };
        let value = receiver.and_then(|p| p.value.as_deref());
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
            // Fill types too when the field has no settable value.
            DesktopAction::Fill { text, .. } | DesktopAction::TypeText { text, .. } => (
                runs(&submitted_lines(value, text)).unwrap_or(Capability::Interact),
                typed_risk(text, receiver).max(if sensitive_target {
                    ActionRisk::Sensitive
                } else {
                    ActionRisk::Normal
                }),
            ),
            DesktopAction::Press { keys, .. } => {
                let has = |key| keys.contains(&key);
                let in_text = receiver.is_some_and(is_text_input);
                // Win chords open Run, the Start search and system menus, which start anything;
                // Ctrl+Enter and Alt+Enter send in most mail and chat apps, whatever has focus.
                let mut risk =
                    if has(Key::Win) || (has(Key::Enter) && (has(Key::Ctrl) || has(Key::Alt))) {
                        ActionRisk::Sensitive
                    } else {
                        ActionRisk::Normal
                    };
                if has(Key::Enter) {
                    risk = risk.max(enter_risk(receiver));
                }
                // Space activates a focused control; in a text field it is just a space.
                if has(Key::Space) && !in_text {
                    risk = risk.max(activation());
                }
                // Delete outside a text field deletes the item itself (a file in Explorer).
                if has(Key::Delete) && !in_text {
                    risk = risk.max(ActionRisk::Destructive);
                }
                // Win+R opens the Run box and Win+X a menu with Terminal and Run: both run
                // any command, so they are judged as PowerShell.
                let capability = if has(Key::Win) && (has(Key::Char('r')) || has(Key::Char('x'))) {
                    Capability::PowerShell
                } else if has(Key::Enter) {
                    runs(&[value.unwrap_or_default().to_owned()])
                        .unwrap_or(Capability::PhysicalInput)
                } else {
                    Capability::PhysicalInput
                };
                (capability, risk)
            }
            DesktopAction::Click { force_physical, .. } => (
                if *force_physical {
                    Capability::PhysicalInput
                } else {
                    Capability::Interact
                },
                activation(),
            ),
            // The option is what gets selected or invoked (a "Delete" menu item, a "Buy" choice).
            DesktopAction::Select { option, .. } => (
                Capability::Interact,
                activation().max(
                    option
                        .as_deref()
                        .map_or(ActionRisk::Normal, |o| classify_activation(o, "")),
                ),
            ),
            DesktopAction::Check { .. }
            | DesktopAction::Uncheck { .. }
            | DesktopAction::Toggle { .. } => (Capability::Interact, activation()),
        };
        ProposedAction {
            tool: format!("desktop_{}", action.name()),
            capability,
            // No dialog context leaves the risk as it is (a read stays ReadOnly).
            risk: dialog.map_or(risk, |d| risk.max(d)),
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

    /// Re-reads the target until `pred` holds or the settle window passes.
    async fn poll_until(
        &self,
        r: &Resolved,
        ctx: &OperationContext,
        pred: impl Fn(&UiProps) -> bool,
    ) -> Option<UiProps> {
        let deadline = Instant::now() + SETTLE.min(ctx.remaining());
        loop {
            if let Ok(props) = self.uia.refresh(r.key, ctx).await
                && pred(&props)
            {
                return Some(props);
            }
            if Instant::now() >= deadline || ctx.cancel.is_cancelled() {
                return None;
            }
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    /// Polls for any observable effect: target state, target disappearance, window changes.
    /// The target is compared with `baseline` when the step prepared it, else as resolved.
    async fn settle(
        &self,
        r: Option<&Resolved>,
        baseline: Option<&UiProps>,
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
                    Ok(now) if changed(baseline.unwrap_or(&r.props), &now) => {
                        return (true, Some(now));
                    }
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

    /// Execute an element-level action (spec §14). Every action is audited, reading text too.
    pub async fn execute(
        &self,
        session: &Session,
        action: DesktopAction,
    ) -> WinwrightResult<ActionResult> {
        let started = Instant::now();
        let tool = format!("desktop_{}", action.name());
        let mut target = None;
        let mut confirmed = false;
        let result = self
            .execute_inner(session, action, &mut target, &mut confirmed)
            .await;
        let method = result.as_ref().ok().map(|r| r.method);
        self.record(
            session,
            &tool,
            target.as_ref(),
            method,
            &result,
            confirmed,
            started,
        );
        result
    }

    async fn execute_inner(
        &self,
        session: &Session,
        action: DesktopAction,
        audit_target: &mut Option<TargetSummary>,
        confirmed: &mut bool,
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
        if let Some(r) = &resolved {
            self.guard_self(r.props.process_id, &r.label())?;
        }
        if resolved.is_none()
            && matches!(
                action,
                DesktopAction::Press { .. } | DesktopAction::TypeText { .. }
            )
        {
            // Keys go to the foreground window: never let them answer Winwright's own dialogs.
            if let Some(fg) = self.windows.foreground_window()? {
                if fg.process_id == std::process::id()
                    || fg.process_name.eq_ignore_ascii_case("winwright.exe")
                {
                    return Err(WinwrightError::ActionBlocked {
                        reason: "keyboard input to Winwright's own windows is blocked".into(),
                    });
                }
                // Windows silently drops input to a more privileged window.
                if self.windows.is_more_privileged(fg.process_id) {
                    return Err(WinwrightError::UipiBlocked {
                        target: window_label(&fg.title, &fg.process_name),
                    });
                }
            }
        }
        let mutating = !matches!(action, DesktopAction::ReadText { .. });
        let mut lease = if mutating {
            Some(self.lease.try_acquire(&session.id)?)
        } else {
            None
        };
        // Enter/Space/Delete (or a typed line break) without a target act on whatever has
        // focus: judge that element.
        let acts_on_focus = match &action {
            DesktopAction::Press { keys, .. } => keys
                .iter()
                .any(|k| matches!(k, Key::Enter | Key::Space | Key::Delete)),
            DesktopAction::TypeText { text, .. } => text.contains(['\n', '\r']),
            _ => false,
        };
        let focused = if resolved.is_none() && acts_on_focus {
            match self.uia.inspect(InspectTarget::Focused, &ctx).await {
                Ok(hit) => {
                    self.release(vec![hit.key]).await;
                    Some(hit.props)
                }
                Err(_) => None,
            }
        } else {
            None
        };
        let dialog = self
            .dialog_risk(&action, resolved.as_ref(), focused.as_ref(), &ctx)
            .await;
        let proposed = self.proposed(&action, resolved.as_ref(), focused.as_ref(), dialog);
        *audit_target = proposed.target.clone();
        let summary = describe(&action, resolved.as_ref(), focused.as_ref());
        *confirmed = self.permit(session, proposed, summary, &mut lease).await?;
        // A confirmation may have taken a while: give the action its own full deadline.
        let ctx = if *confirmed {
            session.operation(self.timeout())?
        } else {
            ctx
        };
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

        if !step.verified
            && let (Some(expect), Some(r)) = (step.expect.clone(), r)
            && let Some(props) = self.poll_until(r, &ctx, |p| expect.met(p)).await
        {
            step.verified = true;
            step.after = Some(props);
            step.warnings
                .retain(|w| !w.starts_with("the combo box still shows"));
        }
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
        // Invoke and physical input can only be verified by side effects; every other method
        // checked the control's own state and that answer stands. So does a precise
        // expectation (a combo's value, a typed field's text) whatever the method was.
        let evidence_based = step.expect.is_none()
            && matches!(
                step.method,
                ActionMethod::InvokePattern
                    | ActionMethod::PhysicalClick
                    | ActionMethod::PhysicalKeyboard
                    | ActionMethod::PhysicalScroll
            );
        if mutating {
            if evidence_based && !step.verified {
                let (seen, after) = self
                    .settle(r, step.baseline.as_ref(), &before_windows, &ctx)
                    .await;
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
            if evidence_based
                && (!result.opened_windows.is_empty() || !result.closed_windows.is_empty())
            {
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

    /// The point to click, plus the target's state just before the click (after any scrolling).
    async fn physical_point(
        &self,
        r: &Resolved,
        ctx: &OperationContext,
    ) -> WinwrightResult<(PhysicalPoint, UiProps)> {
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
        Ok((
            point,
            outcome.props_after.unwrap_or_else(|| r.props.clone()),
        ))
    }

    async fn physical_click(
        &self,
        r: &Resolved,
        button: MouseButton,
        count: u32,
        ctx: &OperationContext,
    ) -> WinwrightResult<Step> {
        let input = self.input()?;
        let (point, before) = self.physical_point(r, ctx).await?;
        input.click(point, button, count, ctx).await?;
        let mut step = Step::new(ActionMethod::PhysicalClick);
        // Scrolling the target into view is not an effect of the click.
        step.baseline = Some(before);
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
            step.expect = Some(Expect::Selected);
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
            step.expect = Some(Expect::ToggleChangedFrom(p.toggle_state));
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
            let shows_wanted = |p: &UiProps| p.value.as_deref() == Some(wanted.as_str());
            let landed = match &out.props_after {
                Some(after) if shows_wanted(after) => Some(after.clone()),
                // Some providers report the new value a moment later; retyping it before then
                // would apply the text twice.
                _ => self.poll_until(r, ctx, shows_wanted).await,
            };
            if let Some(after) = landed {
                step.verified = true;
                step.after = Some(after);
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

    /// Focuses the target and returns its state with focus.
    async fn focus_target(&self, r: &Resolved, ctx: &OperationContext) -> WinwrightResult<UiProps> {
        if let Some(w) = r.window
            && self.windows.foreground_window()?.map(|f| f.hwnd) != Some(w)
        {
            let _ = self.windows.focus_window(w);
        }
        let out = self.pattern(r, UiPatternAction::SetFocus, ctx).await?;
        if let Some(after) = out.props_after.filter(|a| a.focused) {
            return Ok(after);
        }
        // Some providers take a moment to report focus.
        tokio::time::sleep(Duration::from_millis(50)).await;
        match self.uia.refresh(r.key, ctx).await {
            Ok(p) if p.focused => Ok(p),
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
        let focused = self.focus_target(r, ctx).await?;
        if clear {
            input.press_keys(&[Key::Ctrl, Key::Char('a')], ctx).await?;
            input.press_keys(&[Key::Delete], ctx).await?;
        }
        input.type_text(text, ctx).await?;
        let mut step = Step::new(ActionMethod::PhysicalKeyboard);
        step.warnings.push("used physical keyboard input".into());
        let wanted = if clear {
            text.to_owned()
        } else {
            format!("{}{text}", focused.value.as_deref().unwrap_or_default())
        };
        step.baseline = Some(focused);
        if is_sensitive(&r.props) {
            step.warnings
                .push("value not read back: sensitive field".into());
            return Ok(step);
        }
        if let Ok(after) = self.uia.refresh(r.key, ctx).await {
            // A readable value is the whole truth: the field must hold exactly the result.
            if after.value.is_some() {
                let expect = Expect::ExactValue(wanted);
                step.verified = expect.met(&after);
                step.expect = Some(expect);
            }
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
        let mut step = Step::new(ActionMethod::PhysicalKeyboard);
        if let Some(r) = r {
            ensure_enabled(r)?;
            step.baseline = Some(self.focus_target(r, ctx).await?);
        }
        input.type_text(text, ctx).await?;
        if let Some(r) = r
            && !is_sensitive(&r.props)
            && let Ok(after) = self.uia.refresh(r.key, ctx).await
        {
            if after.value.is_some() {
                let expect = Expect::Typed {
                    before: step.baseline.as_ref().and_then(|b| b.value.clone()),
                    text: text.to_owned(),
                };
                step.verified = expect.met(&after);
                step.expect = Some(expect);
            }
            step.after = Some(after);
        }
        Ok(step)
    }

    async fn focus(&self, r: &Resolved, ctx: &OperationContext) -> WinwrightResult<Step> {
        ensure_enabled(r)?;
        let focused = self.focus_target(r, ctx).await?;
        let mut step = Step::new(ActionMethod::SetFocus);
        step.verified = true;
        step.after = Some(focused);
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
                    step.expect = Some(Expect::Selected);
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
            // Never toggle again on a stale read: wait for this toggle to land first.
            let before = current;
            after = match self.poll_until(r, ctx, |p| p.toggle_state != before).await {
                Some(props) => Some(props),
                None => out.props_after,
            };
            let now = after.as_ref().and_then(|a| a.toggle_state);
            // No reading at all is no reading: a toggle that lands late must not be undone.
            if target_state.is_none() || now.is_none() || now == target_state || now == current {
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
        step.expect = Some(match target_state {
            Some(t) => Expect::Toggle(t),
            None => Expect::ToggleChangedFrom(p.toggle_state),
        });
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
        step.expect = Some(Expect::Expand(want));
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
        step.expect = Some(Expect::Visible);
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
        // One step is one wheel notch (3 lines with Windows' default setting).
        let notches = i32::try_from(amount).unwrap_or(i32::MAX);
        let (x, y) = match direction {
            ScrollDirection::Up => (0, -notches),
            ScrollDirection::Down => (0, notches),
            ScrollDirection::Left => (-notches, 0),
            ScrollDirection::Right => (notches, 0),
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
        let mut step = Step::new(ActionMethod::PhysicalKeyboard);
        if let Some(r) = r {
            ensure_enabled(r)?;
            step.baseline = Some(self.focus_target(r, ctx).await?);
        }
        input.press_keys(keys, ctx).await?;
        Ok(step)
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
            step.expect = Some(Expect::Selected);
            return Ok(step);
        };
        if option.trim().is_empty() {
            // An empty option would "contain"-match every item.
            return Err(WinwrightError::invalid("option is empty"));
        }
        let is_combo = r.props.role == ControlRole::ComboBox;
        let was_collapsed = r.props.has_pattern(UiPattern::ExpandCollapse)
            && r.props.expand_state == Some(ExpandState::Collapsed);
        // A classic drop-down may need Enter to commit; its window must be in front before the
        // list opens, because activating it later would close the list.
        if is_combo
            && self.input.is_some()
            && let Some(window) = r.window
            && self.windows.foreground_window()?.map(|w| w.hwnd) != Some(window)
        {
            let _ = self.windows.focus_window(window);
        }
        if was_collapsed {
            // Many combo boxes only materialize their items while open.
            let _ = self.pattern(r, UiPatternAction::Expand, ctx).await;
        }
        let chosen = self.choose_option(session, r, option, is_combo, ctx).await;
        // Success or not, a drop-down this action opened does not stay open.
        self.collapse_if_opened(r, was_collapsed, ctx).await;
        let (mut step, expected) = chosen?;
        let shows_expected = |p: &UiProps| {
            p.value
                .as_deref()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case(&expected))
        };
        let now = self.uia.refresh(r.key, ctx).await?;
        if is_combo {
            step.verified = shows_expected(&now);
            step.expect = Some(Expect::Value(expected.clone()));
            if !step.verified {
                step.warnings.push(format!(
                    "the combo box still shows {:?}, not {expected:?}",
                    now.value.as_deref().unwrap_or_default()
                ));
            }
        } else if !step.verified {
            step.verified = shows_expected(&now);
        }
        step.after = Some(now);
        Ok(step)
    }

    /// Selects or invokes `option` (or writes it into an editable container) and commits a
    /// classic drop-down whose list is still open. Returns the step and the text the container
    /// must show afterwards (combo boxes are verified by value).
    async fn choose_option(
        &self,
        session: &Session,
        r: &Resolved,
        option: &str,
        is_combo: bool,
        ctx: &OperationContext,
    ) -> WinwrightResult<(Step, String)> {
        let (mut step, expected) = match self.find_item(session, r, option, ctx).await {
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
                    self.release(vec![item.key]).await;
                    return Err(WinwrightError::UnsupportedPattern {
                        element: item.label(),
                        pattern: "SelectionItem/Invoke".into(),
                    });
                };
                let out = self.pattern(&item, action, ctx).await;
                self.release(vec![item.key]).await;
                let out = out?;
                let mut step = Step::new(method);
                // For lists/tabs/trees the item's own state is the truth; a combo box is
                // checked by its value.
                step.verified = !is_combo
                    && out
                        .props_after
                        .as_ref()
                        .is_some_and(|a| a.selected == Some(true));
                (step, item.props.name.trim().to_owned())
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
                (step, option.trim().to_owned())
            }
            Err(e) => return Err(e),
        };
        if is_combo {
            let now = self.uia.refresh(r.key, ctx).await?;
            if now.expand_state == Some(ExpandState::Expanded) {
                // Classic Win32 drop-downs only highlight the item on Select and revert when
                // the list is closed; Enter commits it (spec §11 keyboard fallback). Only while
                // this combo's list is open, focused, and its window is in front, so the key
                // cannot reach anything else. Modern combos close themselves on Select.
                let foreground = self.windows.foreground_window()?.map(|w| w.hwnd);
                let safe = now.focused && foreground.is_some() && foreground == r.window;
                tracing::debug!(
                    safe,
                    focused = now.focused,
                    "drop-down still open after Select"
                );
                if let Some(input) = self.input.as_deref()
                    && safe
                {
                    input.press_keys(&[Key::Enter], ctx).await?;
                    step.warnings
                        .push("committed the drop-down choice with Enter".into());
                }
            }
        }
        Ok((step, expected))
    }

    /// Closes a drop-down this action opened, if it is still open.
    async fn collapse_if_opened(&self, r: &Resolved, opened: bool, ctx: &OperationContext) {
        if opened
            && let Ok(now) = self.uia.refresh(r.key, ctx).await
            && now.expand_state == Some(ExpandState::Expanded)
        {
            let _ = self.pattern(r, UiPatternAction::Collapse, ctx).await;
        }
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
        // The same element can surface twice through provider quirks; count it once.
        let dedupe = |v: &mut Vec<&winwright_contracts::backend::UiNode>| {
            let mut seen = std::collections::HashSet::new();
            v.retain(|n| n.props.runtime_id.is_empty() || seen.insert(n.props.runtime_id.clone()));
        };
        dedupe(&mut exact);
        dedupe(&mut partial);
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

    /// Top-level window control (spec §17). Audited.
    pub async fn window_action(
        &self,
        session: &Session,
        action: WindowAction,
    ) -> WinwrightResult<ActionResult> {
        let started = Instant::now();
        let tool = action.name();
        let mut target = None;
        let mut confirmed = false;
        let result = self
            .window_action_inner(session, action, &mut target, &mut confirmed)
            .await;
        let method = result.as_ref().ok().map(|r| r.method);
        self.record(
            session,
            tool,
            target.as_ref(),
            method,
            &result,
            confirmed,
            started,
        );
        result
    }

    async fn window_action_inner(
        &self,
        session: &Session,
        action: WindowAction,
        audit_target: &mut Option<TargetSummary>,
        confirmed: &mut bool,
    ) -> WinwrightResult<ActionResult> {
        let started = Instant::now();
        let ctx = session.operation(self.timeout())?;
        let window = self.find_window(action.selector())?;
        let label = window_label(&window.title, &window.process_name);
        self.guard_self(window.process_id, &format!("Window {label}"))?;
        if self.windows.is_more_privileged(window.process_id) {
            return Err(WinwrightError::UipiBlocked { target: label });
        }
        let size = |r: &PhysicalRect| {
            (
                i64::from(r.right) - i64::from(r.left),
                i64::from(r.bottom) - i64::from(r.top),
            )
        };
        let expected_bounds = match &action {
            WindowAction::Move { x, y, .. } => {
                let (w, h) = size(&window.bounds);
                Some(window_rect(*x, *y, w, h)?)
            }
            WindowAction::Resize { width, height, .. } => Some(window_rect(
                window.bounds.left,
                window.bounds.top,
                i64::from(*width),
                i64::from(*height),
            )?),
            WindowAction::SetBounds { bounds, .. } => {
                let (w, h) = size(bounds);
                Some(window_rect(bounds.left, bounds.top, w, h)?)
            }
            _ => None,
        };
        let mut lease = Some(self.lease.try_acquire(&session.id)?);
        let proposed = ProposedAction {
            tool: action.name().into(),
            capability: Capability::WindowControl,
            risk: ActionRisk::Normal,
            target: Some(TargetSummary {
                process: Some(window.process_name.clone()),
                window: Some(window.title.clone()),
                role: Some("Window".into()),
                name: None,
            }),
        };
        *audit_target = proposed.target.clone();
        let verb = action
            .name()
            .trim_start_matches("window_")
            .replace('_', " ");
        *confirmed = self
            .permit(
                session,
                proposed,
                format!("{verb} window {label}"),
                &mut lease,
            )
            .await?;
        let hwnd = window.hwnd;
        let mut warnings = Vec::new();
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
