//! Engine entry points for capture, overlays, processes and files (spec §18, §21, §39, §40).
//! Every call is authorized first; risky capabilities are default-deny in the policy.

use winwright_contracts::backend::OperationContext;
use winwright_contracts::capture::{
    CaptureRequest, CaptureTarget, CapturedImage, ScreenshotRequest, ScreenshotTarget,
};
use winwright_contracts::overlay::{HighlightRequest, HighlightResult, OverlayId, OverlayRequest};
use winwright_contracts::security::{ActionRisk, Capability, ProposedAction, TargetSummary};
use winwright_contracts::system::{
    ExecRequest, ExecResult, FileOperation, FileResult, LaunchRequest, LaunchResult, ProcessInfo,
};
use winwright_contracts::window::WindowInfo;
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::{program_capability, stricter, transfer_risk};

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::engine::Engine;
use crate::session::Session;

const DEFAULT_HIGHLIGHT_MS: u64 = 8_000;
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
    }
}
const DEFAULT_OVERLAY_COLOR: u32 = 0x00E0_4A2A;

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
const LAUNCH_WINDOW_WAIT: Duration = Duration::from_secs(5);
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

fn unavailable(backend: &str) -> WinwrightError {
    WinwrightError::BackendUnavailable {
        backend: backend.into(),
        reason: format!("{backend} is not enabled in this engine"),
    }
}

fn proposed(
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
    /// The window a launch opened: a new titled top-level window (the one in front, when
    /// several appeared), or after a while the front window if it belongs to the launched
    /// program. Waits a moment more once found, so the app is ready for input.
    async fn launched_window(
        &self,
        before: &HashSet<u64>,
        program: &str,
        ctx: &OperationContext,
    ) -> Option<WindowInfo> {
        let started = Instant::now();
        let deadline = started + LAUNCH_WINDOW_WAIT.min(ctx.remaining());
        loop {
            let windows = self.windows.list_windows().unwrap_or_default();
            let mut opened: Vec<&WindowInfo> = windows
                .iter()
                .filter(|w| !before.contains(&w.hwnd) && !w.title.is_empty() && !w.minimized)
                .collect();
            opened.sort_by_key(|w| !w.foreground);
            let reused = || {
                (started.elapsed() >= LAUNCH_REUSE_AFTER)
                    .then(|| {
                        windows
                            .iter()
                            .find(|w| w.foreground && program_stem(&w.process_name) == program)
                    })
                    .flatten()
            };
            if let Some(found) = opened.first().copied().or_else(reused) {
                let hwnd = found.hwnd;
                tokio::time::sleep(LAUNCH_SETTLE.min(ctx.remaining())).await;
                return self.windows.window(hwnd).ok().flatten();
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
        let window = |w: WindowInfo| {
            (
                CaptureTarget::Window(w.hwnd),
                format!("window {:?}", w.title),
            )
        };
        let (target, label) = match &request.target {
            ScreenshotTarget::Active => window(self.active_window()?),
            ScreenshotTarget::Window(selector) => window(self.find_window(selector)?),
            ScreenshotTarget::Element { reference } => {
                let epoch = self.uia.worker_epoch();
                let key = session.state().refs.get_live(reference, epoch)?.key;
                let props = self.uia.refresh(key, &ctx).await?;
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
        };
        *what = Some(label);
        let capture = self
            .capture
            .as_deref()
            .ok_or_else(|| unavailable("capture"))?;
        capture
            .capture(
                CaptureRequest {
                    target,
                    format: request.format,
                    quality: request.quality.unwrap_or(85).clamp(1, 100),
                },
                &ctx,
            )
            .await
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
        let overlay = self.overlay.as_deref().ok_or_else(|| {
            // Turned off by the user is a decision, not a fault.
            if self.config.overlay.enabled {
                unavailable("overlay")
            } else {
                WinwrightError::ActionBlocked {
                    reason: "overlays are disabled in config (overlay.enabled)".into(),
                }
            }
        })?;
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
        let summary = format!("Launch {shown}{}", with_args(&request.args));
        let program = program_stem(shown);
        let before: HashSet<u64> = self
            .windows
            .list_windows()
            .map(|ws| ws.into_iter().map(|w| w.hwnd).collect())
            .unwrap_or_default();
        let mut lease = None;
        let mut confirmed = false;
        let result = async {
            resolved?;
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            let processes = self
                .processes
                .as_deref()
                .ok_or_else(|| unavailable("process"))?;
            let ctx = session.operation(self.timeout())?;
            let mut launched = processes.launch(request, &ctx).await?;
            launched.window = self.launched_window(&before, &program, &ctx).await;
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

    /// Typed file operations. Delete always goes to the Recycle Bin and needs confirmation.
    pub async fn file_operation(
        &self,
        session: &Session,
        op: FileOperation,
    ) -> WinwrightResult<FileResult> {
        let (capability, risk) = match &op {
            FileOperation::List { .. }
            | FileOperation::Metadata { .. }
            | FileOperation::Search { .. }
            | FileOperation::KnownFolder { .. } => (Capability::FileRead, ActionRisk::ReadOnly),
            FileOperation::Copy { overwrite, .. } | FileOperation::Move { overwrite, .. }
                if *overwrite =>
            {
                (Capability::FileWrite, ActionRisk::Destructive)
            }
            FileOperation::Copy { from, to, .. } => {
                (Capability::FileWrite, transfer_risk(from, to))
            }
            // Moved or renamed things vanish from where the user (and their apps) expect them.
            FileOperation::Move { .. } | FileOperation::Rename { .. } => {
                (Capability::FileWrite, ActionRisk::Sensitive)
            }
            FileOperation::CreateDirectory { .. } => (Capability::FileWrite, ActionRisk::Normal),
            FileOperation::Delete { .. } => (Capability::FileDelete, ActionRisk::Destructive),
        };
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
        // PowerShell stays behind its own switch even when the shell is enabled.
        let capability = match stricter(
            program_capability(&request.program),
            program_capability(shown),
        ) {
            Capability::PowerShell => Capability::PowerShell,
            _ => Capability::Shell,
        };
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

    /// Emergency stop (spec §22, §44): cancel every session and queued operation, release any
    /// synthesized keys/buttons, and hide overlays. Independent of queues; never blocks.
    /// The engine stays stopped until [`Engine::rearm`] is called from trusted local UI.
    pub fn emergency_stop(&self) {
        self.sessions.cancel_all();
        if let Some(input) = self.input.as_deref()
            && let Err(err) = input.release_all()
        {
            tracing::error!(%err, "emergency stop could not release all input");
        }
        if let Some(overlay) = self.overlay.as_deref() {
            let _ = overlay.clear(None);
        }
        tracing::warn!("emergency stop: all sessions cancelled");
    }

    /// Re-enables the engine after an emergency stop. User-initiated only; not exposed to models.
    pub fn rearm(&self) {
        self.sessions.rearm();
        self.taint.clear();
    }
}
