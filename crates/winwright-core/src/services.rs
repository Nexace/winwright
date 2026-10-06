//! Engine entry points for capture, overlays, processes and files (spec §18, §21, §39, §40).
//! Every call is authorized first; risky capabilities are default-deny in the policy.

use winwright_contracts::backend::OperationContext;
use winwright_contracts::capture::{
    CaptureRequest, CaptureTarget, CapturedImage, Fit, ImageFormat, Mark, ScreenText,
    ScreenshotRequest, ScreenshotTarget,
};
use winwright_contracts::element::ControlRole;
use winwright_contracts::ids::parse_element_ref;
use winwright_contracts::overlay::{
    HighlightRequest, HighlightResult, OverlayId, OverlayRequest, OverlayService,
};
use winwright_contracts::security::{ActionRisk, Capability, ProposedAction, TargetSummary};
use winwright_contracts::snapshot::{SnapshotNode, SnapshotRequest, SnapshotTarget};
use winwright_contracts::system::{
    ExecRequest, ExecResult, FileOperation, FileResult, LaunchRequest, LaunchResult, ProcessInfo,
    SessionInfo, SessionOutput, SessionStart, WriteMode,
};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::{is_secret_path, program_capability, stricter, transfer_risk};

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::engine::Engine;
use crate::session::Session;

pub(crate) const DEFAULT_HIGHLIGHT_MS: u64 = 8_000;
/// Longest run `exec` accepts (the process backend enforces the same limit).
const MAX_EXEC_TIMEOUT_MS: u64 = 600_000;

/// Dialog/audit description of a file operation. Paths are shown; contents never are.
fn describe_file_op(op: &FileOperation) -> String {
    let overwrite = |o: &bool| if *o { " (overwrite)" } else { "" };
    match op {
        FileOperation::List { path, .. } => format!("List {}", path.display()),
        FileOperation::Metadata { path } => format!("Read metadata of {}", path.display()),
        FileOperation::Copy {
            from,
            to,
            overwrite: o,
        } => {
            format!(
                "Copy {} to {}{}",
                from.display(),
                to.display(),
                overwrite(o)
            )
        }
        FileOperation::Move {
            from,
            to,
            overwrite: o,
        } => {
            format!(
                "Move {} to {}{}",
                from.display(),
                to.display(),
                overwrite(o)
            )
        }
        FileOperation::Rename { path, new_name } => {
            format!("Rename {} to {new_name}", path.display())
        }
        FileOperation::Delete { path } => format!("Move {} to the Recycle Bin", path.display()),
        FileOperation::CreateDirectory { path } => format!("Create folder {}", path.display()),
        FileOperation::Search { root, pattern, .. } => {
            format!("Search {} for {pattern}", root.display())
        }
        FileOperation::KnownFolder { name } => format!("Resolve folder {name}"),
        FileOperation::Read { path, .. } => format!("Read {}", path.display()),
        FileOperation::Write {
            path,
            content,
            mode,
        } => {
            let chars = content.chars().count();
            match mode {
                WriteMode::Create => format!("Create {} ({chars} characters)", path.display()),
                WriteMode::Overwrite => format!(
                    "Replace {} with {chars} characters (the old file goes to the Recycle Bin)",
                    path.display()
                ),
                WriteMode::Append => format!("Add {chars} characters to {}", path.display()),
            }
        }
        FileOperation::Edit {
            path, old, count, ..
        } => format!(
            "Edit {}: replace {} characters{} (the old version goes to the Recycle Bin)",
            path.display(),
            old.chars().count(),
            if *count > 1 {
                format!(" in {count} places")
            } else {
                String::new()
            }
        ),
        FileOperation::Grep { root, pattern, .. } => {
            format!(
                "Search the text of files in {} for {pattern}",
                root.display()
            )
        }
    }
}
/// What running `requested` (resolved to `resolved`) needs: shell execution, with PowerShell
/// behind its own switch even when the shell is enabled.
fn shell_capability(requested: &str, resolved: &str) -> Capability {
    match stricter(program_capability(requested), program_capability(resolved)) {
        Capability::PowerShell => Capability::PowerShell,
        _ => Capability::Shell,
    }
}

/// Input for a session as a prompt shows it: whole up to the prompt limit, then cut, saying
/// how much is hidden.
fn shown_input(text: &str) -> String {
    let total = text.chars().count();
    if total <= MAX_PROMPT_ARGS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX_PROMPT_ARGS).collect();
    format!(
        "{kept}\u{2026} ({} more characters)",
        total - MAX_PROMPT_ARGS
    )
}

/// What a file operation needs and how risky it is.
/// A share (`\\server\share`): touching it hands the server the person's sign-in.
fn on_share(path: &std::path::Path) -> bool {
    let text = path.as_os_str().to_string_lossy();
    text.starts_with(r"\\") || text.starts_with("//")
}

fn file_risk(op: &FileOperation) -> (Capability, ActionRisk) {
    let (capability, risk) = file_risk_by_kind(op);
    let paths: Vec<&std::path::Path> = match op {
        FileOperation::Copy { from, to, .. } | FileOperation::Move { from, to, .. } => {
            vec![from, to]
        }
        FileOperation::KnownFolder { .. } => Vec::new(),
        FileOperation::Search { root, .. } | FileOperation::Grep { root, .. } => vec![root],
        FileOperation::List { path, .. }
        | FileOperation::Metadata { path }
        | FileOperation::Rename { path, .. }
        | FileOperation::Delete { path }
        | FileOperation::CreateDirectory { path }
        | FileOperation::Read { path, .. }
        | FileOperation::Write { path, .. }
        | FileOperation::Edit { path, .. } => vec![path],
    };
    if paths.into_iter().any(on_share) {
        (capability, ActionRisk::Destructive)
    } else {
        (capability, risk)
    }
}

fn file_risk_by_kind(op: &FileOperation) -> (Capability, ActionRisk) {
    match op {
        // A secret shown to the model cannot be taken back: always ask.
        FileOperation::Read { path, .. } | FileOperation::Grep { root: path, .. }
            if is_secret_path(path) =>
        {
            (Capability::FileRead, ActionRisk::Destructive)
        }
        FileOperation::Write { path, .. } | FileOperation::Edit { path, .. }
            if is_secret_path(path) =>
        {
            (Capability::FileWrite, ActionRisk::Destructive)
        }
        FileOperation::List { .. }
        | FileOperation::Metadata { .. }
        | FileOperation::Search { .. }
        | FileOperation::KnownFolder { .. }
        | FileOperation::Read { .. }
        | FileOperation::Grep { .. } => (Capability::FileRead, ActionRisk::ReadOnly),
        // A new file changes nothing that exists.
        FileOperation::Write {
            mode: WriteMode::Create,
            ..
        } => (Capability::FileWrite, ActionRisk::Normal),
        // The old version goes to the Recycle Bin, so this can be undone.
        FileOperation::Write { .. } | FileOperation::Edit { .. } => {
            (Capability::FileWrite, ActionRisk::Sensitive)
        }
        FileOperation::Copy { overwrite, .. } | FileOperation::Move { overwrite, .. }
            if *overwrite =>
        {
            (Capability::FileWrite, ActionRisk::Destructive)
        }
        // Into a secret place (`.ssh\authorized_keys`, `.aws\credentials`): as risky as writing it.
        FileOperation::Copy { to, .. } | FileOperation::Move { to, .. } if is_secret_path(to) => {
            (Capability::FileWrite, ActionRisk::Destructive)
        }
        FileOperation::Rename { path, new_name }
            if is_secret_path(path) || is_secret_path(&path.with_file_name(new_name)) =>
        {
            (Capability::FileWrite, ActionRisk::Destructive)
        }
        FileOperation::Copy { from, to, .. } => (Capability::FileWrite, transfer_risk(from, to)),
        // Moved or renamed things vanish from where the user (and their apps) expect them.
        FileOperation::Move { .. } | FileOperation::Rename { .. } => {
            (Capability::FileWrite, ActionRisk::Sensitive)
        }
        FileOperation::CreateDirectory { .. } => (Capability::FileWrite, ActionRisk::Normal),
        FileOperation::Delete { .. } => (Capability::FileDelete, ActionRisk::Destructive),
    }
}

/// Winwright teal blue.
/// Most numbered marks drawn on one screenshot.
const MAX_MARKS: usize = 200;

/// The elements worth a number on a screenshot: what can be clicked, typed in or picked, on
/// screen, with its ref number. Containers and plain text are left out.
fn marks_of(nodes: &[SnapshotNode], out: &mut Vec<Mark>) {
    use ControlRole as R;
    for node in nodes {
        if out.len() >= MAX_MARKS {
            return;
        }
        let e = &node.element;
        let actionable = matches!(
            e.role,
            R::Button
                | R::CheckBox
                | R::ComboBox
                | R::DataItem
                | R::Edit
                | R::HeaderItem
                | R::Link
                | R::ListItem
                | R::MenuItem
                | R::RadioButton
                | R::Slider
                | R::Spinner
                | R::SplitButton
                | R::TabItem
                | R::TreeItem
        );
        if let (true, true, Some(rect), Some(number)) = (
            actionable,
            e.visible,
            e.bounds.filter(|b| b.width() > 0 && b.height() > 0),
            parse_element_ref(&e.reference),
        ) {
            out.push(Mark {
                rect,
                number: u32::try_from(number).unwrap_or(u32::MAX),
            });
        }
        marks_of(&node.children, out);
    }
}

/// A window with fewer numbered elements than this gets its OCR text numbered too. A window's
/// own frame (menu, minimize, maximize, close) already makes four.
const FEW_MARKS: usize = 8;
/// Most OCR lines numbered on one screenshot.
const MAX_OCR_MARKS: usize = 60;
/// Longest OCR text a legend line shows.
const OCR_LEGEND_CHARS: usize = 60;

/// Numbers the lines OCR found (after the highest ref already drawn) and says where each is:
/// the legend text to append. Nothing is added when OCR found no text.
fn ocr_marks(text: &ScreenText, marks: &mut Vec<Mark>) -> String {
    let mut number = marks.iter().map(|m| m.number).max().unwrap_or(0);
    let mut legend = String::new();
    for line in text.lines.iter().filter(|l| !l.text.trim().is_empty()) {
        if legend.lines().count() >= MAX_OCR_MARKS || line.bounds.width() <= 0 {
            break;
        }
        number += 1;
        marks.push(Mark {
            rect: line.bounds,
            number,
        });
        let shown: String = line.text.chars().take(OCR_LEGEND_CHARS).collect();
        legend.push_str(&format!(
            "{number} \"{shown}\" click {},{}\n",
            line.bounds.left + line.bounds.width() / 2,
            line.bounds.top + line.bounds.height() / 2
        ));
    }
    if legend.is_empty() {
        return legend;
    }
    format!(
        "\nFew UI elements here, so these numbers mark text found by OCR instead of refs. Click \
         one with desktop_mouse using the screen x,y beside it (no window or scale):\n{legend}"
    )
}

pub(crate) const DEFAULT_OVERLAY_COLOR: u32 = 0x0008_91B2;

/// Prompts show at most this many characters of arguments.
const MAX_PROMPT_ARGS: usize = 600;

/// ` with arguments "a" "b c"`, every argument, for a confirmation prompt (a very long list is
/// cut, saying how much is hidden).
fn with_args(args: &[String]) -> String {
    if args.is_empty() {
        return String::new();
    }
    let all = args
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(" ");
    let total = all.chars().count();
    if total <= MAX_PROMPT_ARGS {
        return format!(" with arguments {all}");
    }
    let shown: String = all.chars().take(MAX_PROMPT_ARGS).collect();
    format!(
        " with arguments {shown}\u{2026} ({} more characters)",
        total - MAX_PROMPT_ARGS
    )
}

/// How long app_launch waits for the app's window, and then for the app to finish starting:
/// keys sent the moment a window appears can land while it is still loading (Notepad
/// restoring its tabs garbled typed text that way).
/// Apps that check for updates first (Discord, Slack) take several seconds.
const LAUNCH_WINDOW_WAIT: Duration = Duration::from_secs(12);
const LAUNCH_SETTLE: Duration = Duration::from_millis(500);
/// An app that reuses a window it already had (a new tab) opens no new one: after this long
/// without one, its foreground window counts.
const LAUNCH_REUSE_AFTER: Duration = Duration::from_millis(1_500);

/// `C:\Windows\notepad.exe` -> `notepad`: what a window's process name is compared with.
fn program_stem(program: &str) -> String {
    let name = program
        .trim()
        .trim_matches('"')
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_owned()
}

/// Process names a launched app's windows may have: the program that started, and the name it
/// was asked for (a Start menu shortcut may start a launcher; Store apps have no program file).
struct LaunchedNames {
    program: String,
    asked: String,
}

impl LaunchedNames {
    fn new(shown: &str, asked: &str) -> Self {
        let squash = |s: &str| {
            s.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        };
        Self {
            program: program_stem(shown),
            asked: squash(&program_stem(asked)),
        }
    }

    fn owns(&self, process_name: &str) -> bool {
        let stem = program_stem(process_name);
        stem == self.program
            || (self.asked.len() >= 3
                && stem
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .contains(&self.asked))
    }
}

pub(crate) fn unavailable(backend: &str) -> WinwrightError {
    WinwrightError::BackendUnavailable {
        backend: backend.into(),
        reason: format!("{backend} is not enabled in this engine"),
    }
}

pub(crate) fn proposed(
    tool: &str,
    capability: Capability,
    risk: ActionRisk,
    name: Option<String>,
) -> ProposedAction {
    ProposedAction {
        tool: tool.into(),
        capability,
        risk,
        target: name.map(|n| TargetSummary {
            name: Some(n),
            ..Default::default()
        }),
    }
}

impl Engine {
    /// The window a launch opened: a new titled top-level window (the launched program's first,
    /// then the one in front), or after a while the front window if it belongs to the launched
    /// program. Waits a moment more once found, so the app is ready for input; a window gone by
    /// then (an updater's splash) is passed over for the next one.
    async fn launched_window(
        &self,
        before: &HashSet<u64>,
        program: &LaunchedNames,
        ctx: &OperationContext,
    ) -> Option<WindowInfo> {
        let started = Instant::now();
        let deadline = started + LAUNCH_WINDOW_WAIT.min(ctx.remaining());
        let mut gone = HashSet::new();
        loop {
            let windows = self.windows.list_windows().unwrap_or_default();
            let mut opened: Vec<&WindowInfo> = windows
                .iter()
                .filter(|w| {
                    !before.contains(&w.hwnd)
                        && !gone.contains(&w.hwnd)
                        && !w.title.is_empty()
                        && !w.minimized
                })
                .collect();
            opened.sort_by_key(|w| (!program.owns(&w.process_name), !w.foreground));
            let reused = || {
                (started.elapsed() >= LAUNCH_REUSE_AFTER)
                    .then(|| {
                        windows
                            .iter()
                            .find(|w| w.foreground && program.owns(&w.process_name))
                    })
                    .flatten()
            };
            if let Some(found) = opened.first().copied().or_else(reused) {
                let hwnd = found.hwnd;
                tokio::time::sleep(LAUNCH_SETTLE.min(ctx.remaining())).await;
                if let Some(window) = self.windows.window(hwnd).ok().flatten() {
                    return Some(window);
                }
                gone.insert(hwnd);
            }
            if Instant::now() >= deadline || ctx.cancel.is_cancelled() {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// On-demand screenshot (never continuous). Element targets crop the element's bounds.
    /// Audited: a screenshot can carry anything on screen.
    pub async fn screenshot(
        &self,
        session: &Session,
        request: ScreenshotRequest,
    ) -> WinwrightResult<CapturedImage> {
        let started = Instant::now();
        let mut what = None;
        let result = self.screenshot_inner(session, request, &mut what).await;
        let target = what.map(|name| TargetSummary {
            name: Some(name),
            ..Default::default()
        });
        self.record(
            session,
            "desktop_screenshot",
            target.as_ref(),
            None,
            &result,
            false,
            started,
        );
        result
    }

    async fn screenshot_inner(
        &self,
        session: &Session,
        request: ScreenshotRequest,
        what: &mut Option<String>,
    ) -> WinwrightResult<CapturedImage> {
        self.authorize(proposed(
            "desktop_screenshot",
            Capability::Capture,
            ActionRisk::ReadOnly,
            None,
        ))?;
        let ctx = session.operation(self.timeout())?;
        let (target, label) = self.capture_target(session, &request.target, &ctx).await?;
        *what = Some(label);
        // Winwright's own frame is not part of what the model should see.
        if let Some(overlay) = self.overlay.as_deref() {
            self.working.hide_now(overlay);
        }
        let (marks, legend) = if request.marks {
            let CaptureTarget::Window(hwnd) = target else {
                return Err(WinwrightError::invalid(
                    "marks number a window's elements: screenshot the active window or a window",
                ));
            };
            // The refs drawn are live: the model acts on them like a snapshot's.
            let snap = self
                .snapshot(
                    session,
                    SnapshotRequest {
                        target: SnapshotTarget::Window(WindowSelector {
                            hwnd: Some(hwnd),
                            ..Default::default()
                        }),
                        include_bounds: true,
                        structured: true,
                        ..Default::default()
                    },
                )
                .await?;
            let mut marks = Vec::new();
            marks_of(snap.nodes.as_deref().unwrap_or_default(), &mut marks);
            let mut legend = format!(
                "Numbers on the image are refs (12 = e12): act with desktop_click ref=e12 etc.\n{}",
                snap.tree
            );
            // A window the accessibility tree barely describes (a game, a canvas): number the
            // text OCR finds too, so the model can still point by number.
            if marks.len() < FEW_MARKS
                && let Some(capture) = self.capture.as_deref()
                && let Ok(text) = capture
                    .read_text(
                        CaptureRequest {
                            target,
                            format: ImageFormat::Png,
                            quality: 85,
                            fit: None,
                            marks: Vec::new(),
                        },
                        &ctx,
                    )
                    .await
            {
                legend.push_str(&ocr_marks(&text, &mut marks));
            }
            (marks, Some(legend))
        } else {
            (Vec::new(), None)
        };
        let capture = self
            .capture
            .as_deref()
            .ok_or_else(|| unavailable("capture"))?;
        let mut image = capture
            .capture(
                CaptureRequest {
                    target,
                    format: request.format,
                    quality: request.quality.unwrap_or(85).clamp(1, 100),
                    fit: request.fit.then_some(Fit::MODEL),
                    marks,
                },
                &ctx,
            )
            .await?;
        image.legend = legend;
        Ok(image)
    }

    /// Reads the text on screen with OCR (for apps without an accessibility tree). Audited like
    /// a screenshot.
    pub async fn screen_text(
        &self,
        session: &Session,
        target: ScreenshotTarget,
    ) -> WinwrightResult<ScreenText> {
        let started = Instant::now();
        let mut what = None;
        let result = async {
            self.authorize(proposed(
                "desktop_ocr",
                Capability::Capture,
                ActionRisk::ReadOnly,
                None,
            ))?;
            let ctx = session.operation(self.timeout())?;
            let (target, label) = self.capture_target(session, &target, &ctx).await?;
            what = Some(label);
            let capture = self
                .capture
                .as_deref()
                .ok_or_else(|| unavailable("capture"))?;
            let request = CaptureRequest {
                target,
                format: ImageFormat::Png,
                quality: 85,
                fit: None,
                marks: Vec::new(),
            };
            capture.read_text(request, &ctx).await
        }
        .await;
        let target = what.map(|name| TargetSummary {
            name: Some(name),
            ..Default::default()
        });
        self.record(
            session,
            "desktop_ocr",
            target.as_ref(),
            None,
            &result,
            false,
            started,
        );
        result
    }

    /// What a screenshot or text read of `target` captures, and a label for the audit log.
    async fn capture_target(
        &self,
        session: &Session,
        target: &ScreenshotTarget,
        ctx: &OperationContext,
    ) -> WinwrightResult<(CaptureTarget, String)> {
        let window = |w: WindowInfo| {
            (
                CaptureTarget::Window(w.hwnd),
                format!("window {:?}", w.title),
            )
        };
        Ok(match target {
            ScreenshotTarget::Active => window(self.active_window()?),
            ScreenshotTarget::Window(selector) => window(self.find_window(selector)?),
            ScreenshotTarget::Element { reference } => {
                let epoch = self.uia.worker_epoch();
                let key = session.state().refs.get_live(reference, epoch)?.key;
                let props = self.uia.refresh(key, ctx).await?;
                let bounds = props.bounds.filter(|_| !props.offscreen).ok_or_else(|| {
                    WinwrightError::CaptureFailed {
                        reason: format!("{} is not on screen", props.label()),
                    }
                })?;
                (CaptureTarget::Region(bounds), props.label())
            }
            ScreenshotTarget::Monitor(i) => (CaptureTarget::Monitor(*i), format!("monitor {i}")),
            ScreenshotTarget::Region(r) => (
                CaptureTarget::Region(*r),
                format!("region {},{} {}x{}", r.left, r.top, r.width(), r.height()),
            ),
            ScreenshotTarget::Desktop => (CaptureTarget::Desktop, "desktop".to_owned()),
        })
    }

    /// Highlights an element for tutorial/debug mode (spec §21). Overlays never take focus.
    pub async fn highlight(
        &self,
        session: &Session,
        request: HighlightRequest,
    ) -> WinwrightResult<HighlightResult> {
        self.observe("overlay_highlight")?;
        if request.duration_ms == Some(0) {
            return Err(WinwrightError::invalid("durationMs must be positive"));
        }
        self.ensure_no_confirmation_open()?;
        let ctx = session.operation(self.timeout())?;
        let resolved = self.resolve_target(session, &request.target, &ctx).await?;
        // A label drawn on Winwright's own dialog could steer the user's answer.
        self.guard_self(resolved.props.process_id, &resolved.label())?;
        // Again, right before drawing: a confirmation may have opened while resolving.
        self.ensure_no_confirmation_open()?;
        let overlay = self.overlay_service()?;
        let rect = resolved
            .props
            .bounds
            .filter(|_| !resolved.props.offscreen)
            .ok_or_else(|| {
                WinwrightError::invalid(format!("{} is not on screen", resolved.label()))
            })?;
        let id = overlay.show(OverlayRequest {
            rect,
            style: request.style,
            label: request.label,
            step: request.step,
            steps: None,
            color: request.color.unwrap_or(DEFAULT_OVERLAY_COLOR),
            duration_ms: Some(request.duration_ms.unwrap_or(DEFAULT_HIGHLIGHT_MS)),
        })?;
        Ok(HighlightResult {
            overlay: id,
            reference: resolved.reference.clone(),
            target: resolved.label(),
            rect,
        })
    }

    pub(crate) fn overlay_service(&self) -> WinwrightResult<&dyn OverlayService> {
        self.overlay.as_deref().ok_or_else(|| {
            // Turned off by the user is a decision, not a fault.
            if self.config.overlay.enabled {
                unavailable("overlay")
            } else {
                WinwrightError::ActionBlocked {
                    reason: "overlays are disabled in config (overlay.enabled)".into(),
                }
            }
        })
    }

    pub fn clear_overlays(&self, id: Option<OverlayId>) -> WinwrightResult<()> {
        self.ensure_running()?;
        match self.overlay.as_deref() {
            Some(o) => o.clear(id),
            None => Ok(()),
        }
    }

    /// Launches an app, URI, or folder (Level 1: prefer this over clicking through the shell).
    pub async fn launch_app(
        &self,
        session: &Session,
        request: LaunchRequest,
    ) -> WinwrightResult<LaunchResult> {
        let started = Instant::now();
        // The prompt names what would really start, and policy judges that too: a bare name
        // can resolve (App Paths) to another program.
        let resolved = match self.processes.as_deref() {
            Some(p) => p.resolve_launch(&request),
            None => Ok(request.app.clone()),
        };
        let shown = resolved.as_deref().unwrap_or(&request.app);
        // Launching an interpreter with arguments is shell execution and is gated like it.
        let action = proposed(
            "app_launch",
            stricter(program_capability(&request.app), program_capability(shown)),
            ActionRisk::Normal,
            Some(request.app.clone()),
        );
        let target = action.target.clone();
        // Arguments a shortcut adds are shown too: the person approves the whole command line.
        let args = match self.processes.as_deref() {
            Some(p) => p
                .launch_args(&request)
                .unwrap_or_else(|_| request.args.clone()),
            None => request.args.clone(),
        };
        let summary = format!("Launch {shown}{}", with_args(&args));
        let program = LaunchedNames::new(shown, &request.app);
        let mut lease = None;
        let mut confirmed = false;
        let result = async {
            resolved?;
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            // Taken after the person answered: windows they opened meanwhile are not the app's.
            let before: HashSet<u64> = self
                .windows
                .list_windows()
                .map(|ws| ws.into_iter().map(|w| w.hwnd).collect())
                .unwrap_or_default();
            let processes = self
                .processes
                .as_deref()
                .ok_or_else(|| unavailable("process"))?;
            let ctx = session.operation(self.timeout())?;
            let mut launched = processes.launch(request, &ctx).await?;
            launched.window = self.launched_window(&before, &program, &ctx).await;
            // What the person asked to open comes to the front (restored if minimized).
            if let Some(hwnd) = launched
                .window
                .as_ref()
                .filter(|w| !w.foreground)
                .map(|w| w.hwnd)
                && self.focus_window_off_thread(hwnd).await.is_ok()
                && let Some(now) = self.windows.window(hwnd).ok().flatten()
            {
                launched.window = Some(now);
            }
            Ok(launched)
        }
        .await;
        self.record(
            session,
            "app_launch",
            target.as_ref(),
            None,
            &result,
            confirmed,
            started,
        );
        result
    }

    pub fn process_list(&self) -> WinwrightResult<Vec<ProcessInfo>> {
        self.observe("process_list")?;
        self.processes
            .as_deref()
            .ok_or_else(|| unavailable("process"))?
            .list()
    }

    /// Ends a process once the person agrees; the prompt names the program and its path.
    /// Windows' own processes, services and Winwright are refused before anyone is asked.
    pub async fn process_terminate(
        &self,
        session: &Session,
        pid: u32,
    ) -> WinwrightResult<ProcessInfo> {
        let started = Instant::now();
        let mut target = None;
        let mut lease = None;
        let mut confirmed = false;
        let result = async {
            self.ensure_running()?;
            let processes = self
                .processes
                .clone()
                .ok_or_else(|| unavailable("process"))?;
            let info = processes
                .list()?
                .into_iter()
                .find(|p| p.process_id == pid)
                .ok_or_else(|| {
                    WinwrightError::invalid(format!(
                        "no process has id {pid}: list processes again"
                    ))
                })?;
            self.guard_self(pid, &info.name)?;
            processes.can_terminate(pid, &info.name)?;
            let action = proposed(
                "process_terminate",
                Capability::ProcessTerminate,
                ActionRisk::Destructive,
                Some(info.name.clone()),
            );
            target.clone_from(&action.target);
            let summary = format!(
                "End {} (process {pid}{}); anything unsaved in it is lost",
                info.name,
                info.path
                    .as_deref()
                    .map(|p| format!(", {p}"))
                    .unwrap_or_default()
            );
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            let name = info.name.clone();
            tokio::task::spawn_blocking(move || processes.terminate(pid, &name))
                .await
                .map_err(|e| WinwrightError::ActionOutcomeUnknown {
                    operation: "terminate".into(),
                    reason: format!("worker task failed: {e}"),
                })??;
            Ok(info)
        }
        .await;
        self.record(
            session,
            "process_terminate",
            target.as_ref(),
            None,
            &result,
            confirmed,
            started,
        );
        result
    }

    /// Typed file operations. Delete always goes to the Recycle Bin and needs confirmation.
    pub async fn file_operation(
        &self,
        session: &Session,
        op: FileOperation,
    ) -> WinwrightResult<FileResult> {
        let (capability, risk) = file_risk(&op);
        let summary = describe_file_op(&op);
        let action = proposed(
            "filesystem_operation",
            capability,
            risk,
            Some(summary.clone()),
        );
        let started = Instant::now();
        let target = action.target.clone();
        if risk == ActionRisk::ReadOnly {
            // Audited too: listings and searches reveal what is on disk.
            let result = async {
                self.authorize(action)?;
                let files = self.files.as_deref().ok_or_else(|| unavailable("files"))?;
                let ctx = session.operation(self.timeout())?;
                files.execute(op, &ctx).await
            }
            .await;
            self.record(
                session,
                "filesystem_operation",
                target.as_ref(),
                None,
                &result,
                false,
                started,
            );
            return result;
        }
        let mut lease = None;
        let mut confirmed = false;
        let result = async {
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            let files = self.files.as_deref().ok_or_else(|| unavailable("files"))?;
            let ctx = session.operation(self.timeout())?;
            files.execute(op, &ctx).await
        }
        .await;
        self.record(
            session,
            "filesystem_operation",
            target.as_ref(),
            None,
            &result,
            confirmed,
            started,
        );
        result
    }

    /// Typed process execution. Disabled unless the user enables `security.allowShell`, and
    /// even then every call needs confirmation.
    pub async fn exec(
        &self,
        session: &Session,
        request: ExecRequest,
    ) -> WinwrightResult<ExecResult> {
        let started = Instant::now();
        let resolved = match self.processes.as_deref() {
            Some(p) => p.resolve_program(&request),
            None => Ok(request.program.clone()),
        };
        let shown = resolved.as_deref().unwrap_or(&request.program);
        let capability = shell_capability(&request.program, shown);
        let action = proposed(
            "shell_execute",
            capability,
            ActionRisk::Sensitive,
            Some(request.program.clone()),
        );
        let target = action.target.clone();
        let summary = format!("Run {shown}{}", with_args(&request.args));
        let mut lease = None;
        let mut confirmed = false;
        let result = async {
            resolved?;
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            let processes = self
                .processes
                .as_deref()
                .ok_or_else(|| unavailable("process"))?;
            // The run gets the time it asked for (the backend validates the range), not the
            // engine's default.
            let wanted = Duration::from_millis(request.timeout_ms.min(MAX_EXEC_TIMEOUT_MS));
            let ctx = session.operation(wanted.max(self.timeout()))?;
            processes.exec(request, &ctx).await
        }
        .await;
        self.record(
            session,
            "shell_execute",
            target.as_ref(),
            None,
            &result,
            confirmed,
            started,
        );
        result
    }

    /// Starts a background session. Gated like `shell_execute`: off unless the user enabled the
    /// shell, and every start asks.
    pub async fn session_start(
        &self,
        session: &Session,
        request: SessionStart,
    ) -> WinwrightResult<SessionInfo> {
        let started = Instant::now();
        let exec = ExecRequest {
            program: request.program.clone(),
            args: request.args.clone(),
            working_dir: request.working_dir.clone(),
            timeout_ms: MAX_EXEC_TIMEOUT_MS,
            max_output_bytes: 0,
        };
        let resolved = match self.processes.as_deref() {
            Some(p) => p.resolve_program(&exec),
            None => Ok(request.program.clone()),
        };
        let shown = resolved.as_deref().unwrap_or(&request.program);
        let action = proposed(
            "process_session",
            shell_capability(&request.program, shown),
            ActionRisk::Sensitive,
            Some(request.program.clone()),
        );
        let target = action.target.clone();
        let summary = format!(
            "Start {shown}{} in the background",
            with_args(&request.args)
        );
        let mut lease = None;
        let mut confirmed = false;
        let result = async {
            resolved?;
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            self.processes
                .as_deref()
                .ok_or_else(|| unavailable("process"))?
                .session_start(request)
        }
        .await;
        self.record(
            session,
            "process_session",
            target.as_ref(),
            None,
            &result,
            confirmed,
            started,
        );
        result
    }

    /// Sends `text` to a session. Input to a running program is a command: it asks every
    /// time, and the prompt shows the text.
    pub async fn session_input(
        &self,
        session: &Session,
        id: u32,
        text: String,
    ) -> WinwrightResult<()> {
        let started = Instant::now();
        let mut target = None;
        let mut lease = None;
        let mut confirmed = false;
        let result = async {
            self.ensure_running()?;
            let processes = self
                .processes
                .as_deref()
                .ok_or_else(|| unavailable("process"))?;
            let info = processes
                .session_list()
                .into_iter()
                .find(|s| s.id == id)
                .ok_or_else(|| {
                    WinwrightError::invalid(format!(
                        "no session {id}: list sessions to see the open ones"
                    ))
                })?;
            let action = proposed(
                "process_session",
                shell_capability(&info.program, &info.program),
                ActionRisk::Sensitive,
                Some(info.program.clone()),
            );
            target.clone_from(&action.target);
            let summary = format!(
                "Send to {} (session {id}): {}",
                info.program,
                shown_input(&text)
            );
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            processes.session_input(id, text).await
        }
        .await;
        self.record(
            session,
            "process_session",
            target.as_ref(),
            None,
            &result,
            confirmed,
            started,
        );
        result
    }

    /// A session's output since the previous read (audited, like other reads).
    pub async fn session_read(
        &self,
        session: &Session,
        id: u32,
        wait: Duration,
    ) -> WinwrightResult<SessionOutput> {
        let started = Instant::now();
        let result = async {
            self.observe("process_session")?;
            self.processes
                .as_deref()
                .ok_or_else(|| unavailable("process"))?
                .session_read(id, wait)
                .await
        }
        .await;
        self.record(
            session,
            "process_session",
            None,
            None,
            &result,
            false,
            started,
        );
        result
    }

    pub fn session_list(&self) -> WinwrightResult<Vec<SessionInfo>> {
        self.observe("process_session")?;
        Ok(self
            .processes
            .as_deref()
            .ok_or_else(|| unavailable("process"))?
            .session_list())
    }

    /// Ends a session Winwright itself started (the user agreed to it); this does not ask.
    pub async fn session_stop(&self, session: &Session, id: u32) -> WinwrightResult<SessionInfo> {
        let started = Instant::now();
        let action = proposed(
            "process_session",
            Capability::Interact,
            ActionRisk::Normal,
            None,
        );
        let mut lease = None;
        let mut confirmed = false;
        let summary = format!("Stop background session {id}");
        let result = async {
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            self.processes
                .as_deref()
                .ok_or_else(|| unavailable("process"))?
                .session_stop(id)
        }
        .await;
        self.record(
            session,
            "process_session",
            None,
            None,
            &result,
            confirmed,
            started,
        );
        result
    }

    /// Emergency stop (spec §22, §44): cancel every session and queued operation, release any
    /// synthesized keys/buttons, and hide overlays. Independent of queues; never blocks.
    /// The engine stays stopped until [`Engine::rearm`] is called from trusted local UI.
    pub fn emergency_stop(&self) {
        self.sessions.cancel_all();
        // Every "allow for a while" ends with the stop.
        self.grants.clear();
        // Background programs the AI started end too.
        if let Some(processes) = self.processes.as_deref() {
            processes.stop_all_sessions();
        }
        if let Some(input) = self.input.as_deref()
            && let Err(err) = input.release_all()
        {
            tracing::error!(%err, "emergency stop could not release all input");
        }
        if let Some(overlay) = self.overlay.as_deref() {
            let _ = overlay.clear(None);
            overlay.hush();
        }
        tracing::warn!("emergency stop: all sessions cancelled");
    }

    /// Re-enables the engine after an emergency stop. User-initiated only; not exposed to models.
    pub fn rearm(&self) {
        self.sessions.rearm();
        self.taint.clear();
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn launched_windows_belong_to_the_program_or_the_name_asked_for() {
        let shortcut =
            super::LaunchedNames::new(r"C:\Users\a\AppData\Local\Discord\Update.exe", "Discord");
        assert!(shortcut.owns("Discord.exe") && shortcut.owns("Update.exe"));
        assert!(!shortcut.owns("explorer.exe"));
        let store = super::LaunchedNames::new(
            r"shell:AppsFolder\5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App",
            "WhatsApp",
        );
        assert!(store.owns("WhatsApp.exe") && store.owns("WhatsApp.Root.exe"));
        let spaced = super::LaunchedNames::new(r"C:\x\lightroom.exe", "Adobe Lightroom");
        assert!(spaced.owns("lightroom.exe") && !spaced.owns("notepad.exe"));
    }

    use super::*;
    use winwright_contracts::capture::TextLine;
    use winwright_contracts::geometry::PhysicalRect;

    #[test]
    fn ocr_lines_are_numbered_after_the_refs_and_say_where_to_click() {
        let line = |text: &str, left: i32, top: i32| TextLine {
            text: text.into(),
            bounds: PhysicalRect::new(left, top, left + 100, top + 20),
            words: Vec::new(),
        };
        let text = ScreenText {
            language: "en-US".into(),
            lines: vec![
                line("Play", 10, 10),
                line("  ", 10, 40),
                line("Settings", 10, 70),
            ],
        };
        let ui = PhysicalRect::new(0, 0, 30, 30);
        let mut marks = vec![Mark {
            rect: ui,
            number: 7,
        }];
        let legend = ocr_marks(&text, &mut marks);
        assert_eq!(
            marks.iter().map(|m| m.number).collect::<Vec<_>>(),
            [7, 8, 9],
            "blank lines get no number; numbers continue after the refs"
        );
        assert!(legend.contains("8 \"Play\" click 60,20\n"), "{legend}");
        assert!(legend.contains("9 \"Settings\" click 60,80\n"), "{legend}");
        assert_eq!(ocr_marks(&ScreenText::default(), &mut marks), "");
    }

    fn op(json: &str) -> FileOperation {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn every_program_run_is_shell_execution_and_powershell_keeps_its_switch() {
        assert_eq!(shell_capability("ping.exe", "ping.exe"), Capability::Shell);
        assert_eq!(
            shell_capability("cmd", r"C:\Windows\System32\cmd.exe"),
            Capability::Shell
        );
        assert_eq!(
            shell_capability("pwsh", r"C:\pwsh\pwsh.exe"),
            Capability::PowerShell
        );
        // A harmless-looking name that resolves to PowerShell is judged as PowerShell.
        assert_eq!(
            shell_capability(
                "tool",
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            ),
            Capability::PowerShell
        );
    }

    #[test]
    fn session_input_is_shown_whole_up_to_the_prompt_limit() {
        assert_eq!(shown_input("dir\r\n"), "dir\r\n");
        let long = "x".repeat(MAX_PROMPT_ARGS + 5);
        let shown = shown_input(&long);
        assert!(shown.ends_with("(5 more characters)"), "{shown}");
    }

    #[test]
    fn secret_destinations_and_shares_always_ask() {
        let risk = |json: &str| super::file_risk(&serde_json::from_str(json).unwrap()).1;
        for json in [
            r#"{"op":"copy","from":"C:\\Users\\a\\Documents\\work\\k","to":"C:\\Users\\a\\.ssh\\authorized_keys"}"#,
            r#"{"op":"move","from":"C:\\Users\\a\\Documents\\work\\k","to":"C:\\Users\\a\\.claude\\settings.json"}"#,
            r#"{"op":"rename","path":"C:\\Users\\a\\Documents\\x.txt","newName":".env"}"#,
            r#"{"op":"read","path":"\\\\server\\share\\notes.txt"}"#,
            r#"{"op":"list","path":"\\\\server\\share"}"#,
        ] {
            assert_eq!(risk(json), ActionRisk::Destructive, "{json}");
        }
        assert_eq!(
            risk(r#"{"op":"read","path":"C:\\Users\\a\\Documents\\notes.txt"}"#),
            ActionRisk::ReadOnly
        );
    }

    #[test]
    fn text_file_risks() {
        let cases = [
            (
                r#"{"op":"read","path":"C:\\code\\main.rs"}"#,
                ActionRisk::ReadOnly,
            ),
            (
                r#"{"op":"grep","root":"C:\\code","pattern":"fn"}"#,
                ActionRisk::ReadOnly,
            ),
            (
                r#"{"op":"read","path":"C:\\code\\.env"}"#,
                ActionRisk::Destructive,
            ),
            (
                r#"{"op":"grep","root":"C:\\Users\\a\\.ssh","pattern":"x"}"#,
                ActionRisk::Destructive,
            ),
            (
                r#"{"op":"write","path":"C:\\code\\new.txt","content":"x"}"#,
                ActionRisk::Normal,
            ),
            (
                r#"{"op":"write","path":"C:\\code\\a.txt","content":"x","mode":"overwrite"}"#,
                ActionRisk::Sensitive,
            ),
            (
                r#"{"op":"write","path":"C:\\code\\a.txt","content":"x","mode":"append"}"#,
                ActionRisk::Sensitive,
            ),
            (
                r#"{"op":"edit","path":"C:\\code\\a.txt","old":"a","new":"b"}"#,
                ActionRisk::Sensitive,
            ),
            (
                r#"{"op":"write","path":"C:\\Users\\a\\.ssh\\authorized_keys","content":"k"}"#,
                ActionRisk::Destructive,
            ),
            (
                r#"{"op":"edit","path":"C:\\code\\.env.local","old":"a","new":"b"}"#,
                ActionRisk::Destructive,
            ),
        ];
        for (json, want) in cases {
            assert_eq!(file_risk(&op(json)).1, want, "{json}");
        }
    }
}
