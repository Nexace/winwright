//! Engine entry points for capture, overlays, processes and files (spec §18, §21, §39, §40).
//! Every call is authorized first; risky capabilities are default-deny in the policy.

use winwright_contracts::capture::{
    CaptureRequest, CaptureTarget, CapturedImage, ScreenshotRequest, ScreenshotTarget,
};
use winwright_contracts::overlay::{HighlightRequest, HighlightResult, OverlayId, OverlayRequest};
use winwright_contracts::security::{ActionRisk, Capability, ProposedAction, TargetSummary};
use winwright_contracts::system::{
    ExecRequest, ExecResult, FileOperation, FileResult, LaunchRequest, LaunchResult, ProcessInfo,
};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::engine::Engine;
use crate::session::Session;

const DEFAULT_HIGHLIGHT_MS: u64 = 8_000;
const DEFAULT_OVERLAY_COLOR: u32 = 0x00E0_4A2A;

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
    /// On-demand screenshot (never continuous). Element targets crop the element's bounds.
    pub async fn screenshot(
        &self,
        session: &Session,
        request: ScreenshotRequest,
    ) -> WinwrightResult<CapturedImage> {
        self.authorize(proposed(
            "desktop_screenshot",
            Capability::Capture,
            ActionRisk::ReadOnly,
            None,
        ))?;
        let capture = self
            .capture
            .as_deref()
            .ok_or_else(|| unavailable("capture"))?;
        let ctx = session.operation(self.timeout())?;
        let target = match &request.target {
            ScreenshotTarget::Active => CaptureTarget::Window(self.active_window()?.hwnd),
            ScreenshotTarget::Window(selector) => {
                CaptureTarget::Window(self.find_window(selector)?.hwnd)
            }
            ScreenshotTarget::Element { reference } => {
                let epoch = self.uia.worker_epoch();
                let key = session.state().refs.get_live(reference, epoch)?.key;
                let props = self.uia.refresh(key, &ctx).await?;
                let bounds = props.bounds.filter(|_| !props.offscreen).ok_or_else(|| {
                    WinwrightError::CaptureFailed {
                        reason: format!("{} is not on screen", props.label()),
                    }
                })?;
                CaptureTarget::Region(bounds)
            }
            ScreenshotTarget::Monitor(i) => CaptureTarget::Monitor(*i),
            ScreenshotTarget::Region(r) => CaptureTarget::Region(*r),
            ScreenshotTarget::Desktop => CaptureTarget::Desktop,
        };
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
        let overlay = self
            .overlay
            .as_deref()
            .ok_or_else(|| unavailable("overlay"))?;
        let ctx = session.operation(self.timeout())?;
        let resolved = self.resolve_target(session, &request.target, &ctx).await?;
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
        self.authorize(proposed(
            "app_launch",
            Capability::ProcessLaunch,
            ActionRisk::Normal,
            Some(request.app.clone()),
        ))?;
        let processes = self
            .processes
            .as_deref()
            .ok_or_else(|| unavailable("process"))?;
        let ctx = session.operation(self.timeout())?;
        processes.launch(request, &ctx).await
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
            FileOperation::Copy { overwrite, .. } | FileOperation::Move { overwrite, .. } => (
                Capability::FileWrite,
                if *overwrite {
                    ActionRisk::Destructive
                } else {
                    ActionRisk::Normal
                },
            ),
            FileOperation::Rename { .. } | FileOperation::CreateDirectory { .. } => {
                (Capability::FileWrite, ActionRisk::Normal)
            }
            FileOperation::Delete { .. } => (Capability::FileDelete, ActionRisk::Destructive),
        };
        self.authorize(proposed("filesystem_operation", capability, risk, None))?;
        let files = self.files.as_deref().ok_or_else(|| unavailable("files"))?;
        let ctx = session.operation(self.timeout())?;
        files.execute(op, &ctx).await
    }

    /// Typed process execution. Disabled unless the user enables `security.allowShell`, and
    /// even then every call needs confirmation.
    pub async fn exec(
        &self,
        session: &Session,
        request: ExecRequest,
    ) -> WinwrightResult<ExecResult> {
        self.authorize(proposed(
            "shell_execute",
            Capability::Shell,
            ActionRisk::Sensitive,
            Some(request.program.clone()),
        ))?;
        let processes = self
            .processes
            .as_deref()
            .ok_or_else(|| unavailable("process"))?;
        let ctx = session.operation(self.timeout())?;
        processes.exec(request, &ctx).await
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
    }
}
