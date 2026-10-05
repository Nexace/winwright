//! The engine every transport calls: windows, snapshots, inspection. Finding, actions and
//! waits live in sibling modules as further `impl Engine` blocks.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use winwright_contracts::action::ActionMethod;
use winwright_contracts::backend::{
    ElementIdentity, InspectTarget, OperationContext, TreeRoot, UiAutomationBackend, UiTree,
    UiTreeRequest, WindowBackend,
};
use winwright_contracts::capture::CaptureService;
use winwright_contracts::config::Config;
use winwright_contracts::element::ElementDetails;
use winwright_contracts::geometry::PhysicalPoint;
use winwright_contracts::ids::{SessionId, format_element_ref, format_generation};
use winwright_contracts::input::InputBackend;
use winwright_contracts::memory::MemoryStore;
use winwright_contracts::overlay::OverlayService;
use winwright_contracts::security::{
    ActionRisk, Capability, ConfirmationPrompt, Confirmer, PermissionDecision, ProposedAction,
    TargetSummary,
};
use winwright_contracts::snapshot::{
    DesktopSnapshot, SnapshotRequest, SnapshotTarget, WindowSummary,
};
use winwright_contracts::system::{FileService, ProcessService};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::Policy;

use crate::audit::{AuditEvent, AuditLog, now_ms};
use crate::lease::{ActionLease, LeaseGuard};
use crate::refs::NewRef;
use crate::session::{Session, SessionRegistry};
use crate::snapshot::{Compressor, element_info, fingerprint_step};

/// Snapshots older than this many generations may be pruned once past their TTL.
pub(crate) const KEEP_GENERATIONS: u64 = 3;
/// Raw nodes captured per emitted node, before filtering.
const RAW_NODE_FACTOR: u32 = 6;
pub(crate) const RAW_NODE_LIMIT: u32 = 5_000;
const MAX_ALL_WINDOWS: usize = 24;
/// Window sets whose last snapshot is remembered per session for diffs.
const MAX_REMEMBERED_SNAPSHOTS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectRequest {
    UnderCursor,
    Focused,
    Point(PhysicalPoint),
    Ref(String),
}

/// One capture root plus the top-level window it belongs to (if known).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Root {
    pub tree: TreeRoot,
    pub window: Option<u64>,
}

/// Raw capture limits for one call.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_depth: u32,
    pub max_nodes: u32,
    pub max_children: u32,
    pub include_offscreen: bool,
}

pub(crate) struct Captured {
    pub trees: Vec<(UiTree, Option<u64>)>,
    pub active: Option<WindowInfo>,
    pub warnings: Vec<String>,
}

pub struct Engine {
    pub(crate) config: Config,
    pub(crate) policy: Policy,
    pub(crate) windows: Arc<dyn WindowBackend>,
    pub(crate) uia: Arc<dyn UiAutomationBackend>,
    pub(crate) input: Option<Arc<dyn InputBackend>>,
    pub(crate) capture: Option<Arc<dyn CaptureService>>,
    pub(crate) overlay: Option<Arc<dyn OverlayService>>,
    pub(crate) processes: Option<Arc<dyn ProcessService>>,
    pub(crate) files: Option<Arc<dyn FileService>>,
    pub(crate) confirmer: Option<Arc<dyn Confirmer>>,
    pub(crate) audit: Option<Arc<AuditLog>>,
    pub(crate) sessions: SessionRegistry,
    pub(crate) lease: ActionLease,
    /// Confirmation dialogs open right now.
    pub(crate) confirming: AtomicUsize,
    pub(crate) taint: Taint,
    pub(crate) memory: Option<Arc<dyn MemoryStore>>,
    /// Tools run since the last memory report, for the next one.
    pub(crate) worked: Mutex<Vec<String>>,
}

/// Whether the conversation has read untrusted content (a web or Notion page), which could
/// carry instructions aimed at the assistant. A client that tracks this creates `file` when it
/// happens (`winwright mcp --taint-file`); once seen, the taint holds for this process until the user re-enables Winwright,
/// so removing the file does not undo it.
#[derive(Debug, Default)]
pub(crate) struct Taint {
    file: Option<PathBuf>,
    seen: AtomicBool,
}

impl Taint {
    pub(crate) fn is_set(&self) -> bool {
        if self.seen.load(Ordering::SeqCst) {
            return true;
        }
        let present = self.file.as_deref().is_some_and(Path::exists);
        if present {
            self.seen.store(true, Ordering::SeqCst);
        }
        present
    }

    /// Whether the client tells this process when the conversation read outside content
    /// (with `--taint-file`; most MCP clients do not).
    pub(crate) fn tracked(&self) -> bool {
        self.file.is_some()
    }

    /// Outside content arrived through Winwright itself (a recalled report).
    pub(crate) fn latch(&self) {
        self.seen.store(true, Ordering::SeqCst);
    }

    /// The user re-enabled Winwright: forget the taint (the bridge marks it again on the next
    /// untrusted read).
    pub(crate) fn clear(&self) {
        if let Some(file) = &self.file {
            let _ = std::fs::remove_file(file);
        }
        self.seen.store(false, Ordering::SeqCst);
    }
}

/// Counts one open confirmation dialog for as long as it lives.
struct Confirming<'a>(&'a AtomicUsize);

impl<'a> Confirming<'a> {
    fn open(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for Confirming<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Engine {
    pub fn new(
        config: Config,
        windows: Arc<dyn WindowBackend>,
        uia: Arc<dyn UiAutomationBackend>,
    ) -> Self {
        Self {
            policy: Policy::new(config.security.clone()),
            config,
            windows,
            uia,
            input: None,
            capture: None,
            overlay: None,
            processes: None,
            files: None,
            confirmer: None,
            audit: None,
            sessions: SessionRegistry::default(),
            lease: ActionLease::default(),
            confirming: AtomicUsize::new(0),
            taint: Taint::default(),
            memory: None,
            worked: Mutex::new(Vec::new()),
        }
    }

    pub fn with_input(mut self, input: Arc<dyn InputBackend>) -> Self {
        self.input = Some(input);
        self
    }

    pub fn with_capture(mut self, capture: Arc<dyn CaptureService>) -> Self {
        self.capture = Some(capture);
        self
    }

    pub fn with_overlay(mut self, overlay: Arc<dyn OverlayService>) -> Self {
        self.overlay = Some(overlay);
        self
    }

    pub fn with_processes(mut self, processes: Arc<dyn ProcessService>) -> Self {
        self.processes = Some(processes);
        self
    }

    pub fn with_files(mut self, files: Arc<dyn FileService>) -> Self {
        self.files = Some(files);
        self
    }

    /// Trusted local approval UI. Without one, confirmations fail with `CONFIRMATION_REQUIRED`.
    pub fn with_confirmer(mut self, confirmer: Arc<dyn Confirmer>) -> Self {
        self.confirmer = Some(confirmer);
        self
    }

    /// The file whose existence means this conversation has read untrusted content (see
    /// `Taint`); passed as `winwright mcp --taint-file`.
    pub fn with_taint_file(mut self, file: PathBuf) -> Self {
        self.taint = Taint {
            file: Some(file),
            seen: AtomicBool::new(false),
        };
        self
    }

    /// Task reports any app can save and recall (memory_save / memory_recall).
    pub fn with_memory(mut self, memory: Arc<dyn MemoryStore>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_audit(mut self, audit: AuditLog) -> Self {
        self.audit = Some(Arc::new(audit));
        self
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn sessions(&self) -> &SessionRegistry {
        &self.sessions
    }

    pub fn lease(&self) -> &ActionLease {
        &self.lease
    }

    pub fn session(&self, id: &SessionId, owner: &str) -> WinwrightResult<Arc<Session>> {
        self.sessions.get_or_create(id, owner)
    }

    pub(crate) fn timeout(&self) -> Duration {
        Duration::from_millis(self.config.automation.default_timeout_ms)
    }

    pub(crate) fn ref_ttl(&self) -> Duration {
        Duration::from_secs(self.config.automation.reference_ttl_seconds)
    }

    /// After an emergency stop every entry point refuses, read-only ones included, until the
    /// user re-enables Winwright. Session-bound calls also fail through their cancelled session.
    pub(crate) fn ensure_running(&self) -> WinwrightResult<()> {
        if self.sessions.is_stopped() {
            return Err(WinwrightError::Cancelled);
        }
        Ok(())
    }

    pub(crate) fn authorize(&self, action: ProposedAction) -> WinwrightResult<()> {
        self.ensure_running()?;
        let verdict = self.policy.evaluate(&action);
        match verdict.decision {
            PermissionDecision::Allow => Ok(()),
            PermissionDecision::Confirm => Err(WinwrightError::ConfirmationRequired {
                reason: verdict.reason,
            }),
            PermissionDecision::Deny => Err(WinwrightError::ActionBlocked {
                reason: verdict.reason,
            }),
        }
    }

    /// Policy check for state-changing calls. `Confirm` asks the human through the trusted
    /// confirmer while holding the action lease, so nothing (not even this engine) can act on
    /// the desktop, or press Enter on the dialog, until the user decides. Returns whether the
    /// user approved a confirmation.
    pub(crate) async fn permit(
        &self,
        session: &Session,
        action: ProposedAction,
        summary: String,
        lease: &mut Option<LeaseGuard>,
    ) -> WinwrightResult<bool> {
        self.ensure_running()?;
        let mut verdict = self.policy.evaluate(&action);
        // After untrusted content, what policy would allow needs a person's yes instead.
        if verdict.decision == PermissionDecision::Allow
            && action.risk != ActionRisk::ReadOnly
            && self.taint.is_set()
        {
            verdict.decision = PermissionDecision::Confirm;
            verdict.reason = "this conversation has read web or Notion content, so desktop \
                              changes need your approval until a new conversation"
                .into();
        }
        match verdict.decision {
            PermissionDecision::Allow => Ok(false),
            PermissionDecision::Deny => Err(WinwrightError::ActionBlocked {
                reason: verdict.reason,
            }),
            PermissionDecision::Confirm => {
                let Some(confirmer) = self.confirmer.as_deref() else {
                    return Err(WinwrightError::ConfirmationRequired {
                        reason: format!("{} ({summary})", verdict.reason),
                    });
                };
                if lease.is_none() {
                    *lease = Some(self.lease.try_acquire(&session.id)?);
                }
                let prompt = ConfirmationPrompt {
                    summary: summary.clone(),
                    target: action.target,
                    reason: verdict.reason,
                    timeout_ms: self
                        .config
                        .security
                        .confirmation_timeout_seconds
                        .clamp(5, 600)
                        * 1000,
                };
                let cancel = session.operation(Duration::from_secs(600))?.cancel;
                // Nothing may draw over the dialog or label it while the user decides.
                let _open = Confirming::open(&self.confirming);
                if let Some(overlay) = self.overlay.as_deref() {
                    let _ = overlay.clear(None);
                }
                let timeout = Duration::from_millis(prompt.timeout_ms);
                let asked = Instant::now();
                let approved = tokio::select! {
                    answer = confirmer.confirm(prompt) => answer?,
                    () = cancel.cancelled() => return Err(WinwrightError::Cancelled),
                };
                if approved {
                    // The summary can hold arguments (tokens, paths): never logged.
                    tracing::info!(tool = %action.tool, "user approved");
                    Ok(true)
                } else if asked.elapsed() + Duration::from_millis(500) >= timeout {
                    Err(WinwrightError::ActionBlocked {
                        reason: format!("nobody answered the confirmation in time: {summary}"),
                    })
                } else {
                    Err(WinwrightError::ActionBlocked {
                        reason: format!("the user declined: {summary}"),
                    })
                }
            }
        }
    }

    /// Brings a window forward on a blocking thread: Windows may refuse at first, and the retries
    /// wait, which must not hold up the server's single async thread.
    pub(crate) async fn focus_window_off_thread(&self, hwnd: u64) -> WinwrightResult<()> {
        let windows = Arc::clone(&self.windows);
        tokio::task::spawn_blocking(move || windows.focus_window(hwnd))
            .await
            .map_err(|e| WinwrightError::BackendUnavailable {
                backend: "windows".into(),
                reason: format!("focusing a window failed: {e}"),
            })?
    }

    /// Overlays are refused while a confirmation dialog is open: one could cover it, or label
    /// its buttons to steer the answer.
    pub(crate) fn ensure_no_confirmation_open(&self) -> WinwrightResult<()> {
        if self.confirming.load(Ordering::SeqCst) > 0 {
            return Err(WinwrightError::ActionBlocked {
                reason: "a confirmation is waiting for the user; overlays are off until it is \
                         answered"
                    .into(),
            });
        }
        Ok(())
    }

    /// Refuses to touch Winwright's own windows (its confirmation dialogs above all), including
    /// those of other Winwright processes: a CLI call must not answer the MCP server's dialog.
    pub(crate) fn guard_self(&self, process_id: u32, what: &str) -> WinwrightResult<()> {
        if process_id == std::process::id()
            || self
                .windows
                .process_name(process_id)
                .eq_ignore_ascii_case("winwright.exe")
        {
            return Err(WinwrightError::ActionBlocked {
                reason: format!("{what} belongs to Winwright itself and cannot be automated"),
            });
        }
        Ok(())
    }

    /// Records one state-changing call in the local audit log (if enabled).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record<T>(
        &self,
        session: &Session,
        tool: &str,
        target: Option<&TargetSummary>,
        method: Option<ActionMethod>,
        result: &WinwrightResult<T>,
        confirmation: bool,
        started: Instant,
    ) {
        if !tool.starts_with("memory_") {
            let mut worked = self.worked.lock().unwrap_or_else(PoisonError::into_inner);
            if worked.len() < 500 {
                worked.push(tool.to_owned());
            }
        }
        let Some(audit) = self.audit.as_deref() else {
            return;
        };
        let code = match result {
            Ok(_) => "ok",
            Err(e) => e.code().as_str(),
        };
        audit.record(&AuditEvent {
            timestamp_ms: now_ms(),
            session: session.id.as_str(),
            tool,
            target,
            method,
            result: code,
            confirmation,
            duration_ms: started.elapsed().as_millis() as u64,
        });
    }

    pub(crate) fn observe(&self, tool: &str) -> WinwrightResult<()> {
        self.authorize(ProposedAction {
            tool: tool.into(),
            capability: Capability::Observe,
            risk: ActionRisk::ReadOnly,
            target: None,
        })
    }

    pub fn list_windows(&self) -> WinwrightResult<Vec<WindowInfo>> {
        self.observe("desktop_windows")?;
        self.windows.list_windows()
    }

    pub fn active_window(&self) -> WinwrightResult<WindowInfo> {
        self.observe("desktop_active_window")?;
        self.windows
            .foreground_window()?
            .ok_or_else(|| WinwrightError::WindowNotFound {
                query: "foreground window".into(),
            })
    }

    /// Exactly one window must match; several matches are reported, never guessed between.
    pub fn find_window(&self, selector: &WindowSelector) -> WinwrightResult<WindowInfo> {
        self.ensure_running()?;
        if selector.is_empty() {
            return Err(WinwrightError::invalid("window selector is empty"));
        }
        if let Some(hwnd) = selector.hwnd {
            return self
                .windows
                .window(hwnd)?
                .filter(|w| selector.matches(w))
                .ok_or_else(|| WinwrightError::WindowNotFound {
                    query: selector.describe(),
                });
        }
        let mut matches: Vec<WindowInfo> = self
            .list_windows()?
            .into_iter()
            .filter(|w| selector.matches(w))
            .collect();
        match matches.len() {
            0 => Err(WinwrightError::WindowNotFound {
                query: selector.describe(),
            }),
            1 => Ok(matches.remove(0)),
            _ => Err(WinwrightError::ElementAmbiguous {
                locator: format!("window {}", selector.describe()),
                matches: matches
                    .iter()
                    .map(|w| format!("hwnd={:#x} {:?} ({})", w.hwnd, w.title, w.process_name))
                    .collect(),
            }),
        }
    }

    /// Resolves a snapshot/find scope into capture roots.
    pub(crate) fn resolve_roots(
        &self,
        session: &Session,
        target: &SnapshotTarget,
    ) -> WinwrightResult<(Vec<Root>, Option<WindowInfo>)> {
        Ok(match target {
            SnapshotTarget::Active => {
                let w = self.active_window()?;
                let root = Root {
                    tree: TreeRoot::Window(w.hwnd),
                    window: Some(w.hwnd),
                };
                (vec![root], Some(w))
            }
            SnapshotTarget::Window(selector) => {
                let w = self.find_window(selector)?;
                let root = Root {
                    tree: TreeRoot::Window(w.hwnd),
                    window: Some(w.hwnd),
                };
                (vec![root], Some(w))
            }
            SnapshotTarget::AllWindows => (
                self.list_windows()?
                    .iter()
                    .filter(|w| !w.minimized)
                    .take(MAX_ALL_WINDOWS)
                    .map(|w| Root {
                        tree: TreeRoot::Window(w.hwnd),
                        window: Some(w.hwnd),
                    })
                    .collect(),
                None,
            ),
            SnapshotTarget::Subtree { reference } => {
                let state = session.state();
                let entry = state.refs.get_live(reference, self.uia.worker_epoch())?;
                (
                    vec![Root {
                        tree: TreeRoot::Element(entry.key),
                        window: entry.window,
                    }],
                    None,
                )
            }
        })
    }

    /// Captures every root within a shared raw-node budget. In multi-window captures a window
    /// that vanished mid-capture becomes a warning instead of failing the whole call.
    pub(crate) async fn capture_roots(
        &self,
        roots: Vec<Root>,
        limits: Limits,
        ctx: &OperationContext,
    ) -> WinwrightResult<(Vec<(UiTree, Option<u64>)>, Vec<String>)> {
        let tolerant = roots.len() > 1;
        let mut trees = Vec::with_capacity(roots.len());
        let mut warnings = Vec::new();
        let mut used = 0u32;
        let started = Instant::now();
        for root in roots {
            let remaining = limits.max_nodes.saturating_sub(used);
            if remaining == 0 {
                warnings.push("raw capture budget exhausted before all windows".into());
                break;
            }
            let request = UiTreeRequest {
                root: root.tree,
                max_depth: limits.max_depth,
                max_nodes: remaining,
                max_children: limits.max_children,
                include_offscreen: limits.include_offscreen,
            };
            match self.uia.capture_tree(request, ctx).await {
                Ok(tree) => {
                    used += tree.node_count;
                    trees.push((tree, root.window));
                }
                Err(err)
                    if tolerant
                        && matches!(
                            err,
                            WinwrightError::WindowNotFound { .. } | WinwrightError::Platform { .. }
                        ) =>
                {
                    warnings.push(format!("skipped a window: {err}"));
                }
                Err(err) => {
                    // The windows already captured hold worker slots nobody will use.
                    let mut keys = Vec::new();
                    for (tree, _) in &trees {
                        crate::find::all_keys(&tree.root, &mut keys);
                    }
                    self.release(keys).await;
                    return Err(err);
                }
            }
        }
        tracing::debug!(
            raw_nodes = used,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "captured UIA trees"
        );
        Ok((trees, warnings))
    }

    /// Resolve + capture in one step.
    pub(crate) async fn capture_scope(
        &self,
        session: &Session,
        target: &SnapshotTarget,
        limits: Limits,
        ctx: &OperationContext,
    ) -> WinwrightResult<Captured> {
        let (roots, active) = self.resolve_roots(session, target)?;
        let (trees, warnings) = self.capture_roots(roots, limits, ctx).await?;
        Ok(Captured {
            trees,
            active,
            warnings,
        })
    }

    pub(crate) async fn release(&self, keys: Vec<winwright_contracts::backend::ElementKey>) {
        if keys.is_empty() {
            return;
        }
        if let Err(err) = self.uia.release(keys).await {
            tracing::warn!(%err, "failed to release UIA slots");
        }
    }

    pub async fn snapshot(
        &self,
        session: &Session,
        request: SnapshotRequest,
    ) -> WinwrightResult<DesktopSnapshot> {
        request.validate().map_err(WinwrightError::invalid)?;
        self.observe("desktop_snapshot")?;
        let ctx = session.operation(self.timeout())?;
        let epoch = self.uia.worker_epoch();
        let limits = Limits {
            max_depth: request.max_depth,
            max_nodes: request
                .max_nodes
                .saturating_mul(RAW_NODE_FACTOR)
                .clamp(request.max_nodes, RAW_NODE_LIMIT.max(request.max_nodes)),
            max_children: request.max_list_items.saturating_add(30).max(50),
            include_offscreen: request.include_offscreen,
        };
        let Captured {
            trees,
            active,
            mut warnings,
        } = self
            .capture_scope(session, &request.target, limits, &ctx)
            .await?;

        let now = Instant::now();
        let scope_key = {
            let mut windows: Vec<String> = trees
                .iter()
                .map(|(_, w)| w.map_or("?".to_owned(), |h| format!("{h:#x}")))
                .collect();
            windows.sort();
            match &request.target {
                SnapshotTarget::Subtree { reference } => {
                    format!("sub:{reference}:{}", windows.join(","))
                }
                _ => format!("win:{}", windows.join(",")),
            }
        };
        let (snapshot, release) = {
            let mut state = session.state();
            state.generation += 1;
            let generation = state.generation;
            let mut compressor =
                Compressor::new(&request, generation, now).with_raw_child_cap(limits.max_children);
            for (tree, window) in &trees {
                compressor.add_tree(tree, &mut state.refs, *window);
            }
            let lines = crate::diff::lines(&compressor.text);
            let diff = if request.diff {
                state.snapshots.get(&scope_key).map(|(previous, old)| {
                    crate::diff::render(
                        &format_generation(*previous),
                        &format_generation(generation),
                        old,
                        &lines,
                    )
                    .0
                })
            } else {
                None
            };
            if request.diff && diff.is_none() {
                warnings.push(
                    "no earlier snapshot of these windows to diff against; sent the full tree"
                        .into(),
                );
            }
            if state.snapshots.len() >= MAX_REMEMBERED_SNAPSHOTS
                && !state.snapshots.contains_key(&scope_key)
                && let Some(oldest) = state
                    .snapshots
                    .iter()
                    .min_by_key(|(_, (g, _))| *g)
                    .map(|(k, _)| k.clone())
            {
                state.snapshots.remove(&oldest);
            }
            state.snapshots.insert(scope_key, (generation, lines));
            let plain: Vec<UiTree> = trees.into_iter().map(|(t, _)| t).collect();
            let mut release = compressor.unreferenced_keys(&plain);
            release.extend(state.refs.prune(
                generation,
                KEEP_GENERATIONS,
                now,
                self.ref_ttl(),
                epoch,
            ));
            if compressor.truncated {
                warnings.push(
                    "snapshot truncated: raise maxNodes/maxDepth/maxListItems or snapshot a subtree"
                        .into(),
                );
            }
            let snapshot = DesktopSnapshot {
                session: session.id.to_string(),
                generation: format_generation(generation),
                active_window: active.as_ref().map(|w| WindowSummary {
                    reference: compressor.first_root_ref.clone().unwrap_or_default(),
                    title: w.title.clone(),
                    process: w.process_name.clone(),
                }),
                tree: if diff.is_some() {
                    String::new()
                } else {
                    compressor.text
                },
                node_count: compressor.emitted,
                truncated: compressor.truncated,
                warnings,
                nodes: request.structured.then_some(compressor.nodes),
                diff,
            };
            (snapshot, release)
        };
        self.release(release).await;
        Ok(snapshot)
    }

    pub async fn inspect(
        &self,
        session: &Session,
        request: InspectRequest,
    ) -> WinwrightResult<ElementDetails> {
        self.observe("desktop_inspect")?;
        let ctx = session.operation(self.timeout())?;
        let epoch = self.uia.worker_epoch();
        let (target, known_window) = match &request {
            InspectRequest::UnderCursor => {
                (InspectTarget::Point(self.windows.cursor_position()?), None)
            }
            InspectRequest::Focused => (InspectTarget::Focused, None),
            InspectRequest::Point(p) => (InspectTarget::Point(*p), None),
            InspectRequest::Ref(r) => {
                let state = session.state();
                let entry = state.refs.get_live(r, epoch)?;
                (InspectTarget::Element(entry.key), entry.window)
            }
        };
        let inspection = self.uia.inspect(target, &ctx).await?;
        let (reference, _) = self.remember(session, &inspection, known_window).await;
        let props = &inspection.props;

        let ancestors: Vec<String> = inspection.ancestors.iter().map(|a| a.label()).collect();
        let mut path = ancestors.join(" > ");
        if !path.is_empty() {
            path.push_str(" > ");
        }
        path.push_str(&props.label());
        Ok(ElementDetails {
            element: element_info(props, reference, path, true, true),
            control_type_id: props.control_type_id,
            process_id: props.process_id,
            process_name: self.windows.process_name(props.process_id),
            native_window_handle: props.native_window_handle,
            runtime_id: props.runtime_id.clone(),
            keyboard_focusable: props.keyboard_focusable,
            offscreen: props.offscreen,
            help_text: props.help_text.clone(),
            ancestors,
        })
    }

    /// Gives an inspected element a ref in `session` (the same one when it already has one)
    /// and returns it with the element's top-level window.
    pub(crate) async fn remember(
        &self,
        session: &Session,
        inspection: &winwright_contracts::backend::UiInspection,
        known_window: Option<u64>,
    ) -> (String, Option<u64>) {
        let fingerprint = inspection.ancestors.iter().fold(0, fingerprint_step);
        let fingerprint = fingerprint_step(fingerprint, &inspection.props);
        let props = &inspection.props;
        // The outermost ancestor is the top-level window when it has a native handle.
        let window = known_window.or_else(|| {
            inspection
                .ancestors
                .first()
                .unwrap_or(props)
                .native_window_handle
        });
        let (number, replaced) = {
            let mut state = session.state();
            let generation = state.generation;
            let up = state.refs.upsert(
                NewRef {
                    identity: ElementIdentity::from_props(props, fingerprint),
                    key: inspection.key,
                    bounds: props.bounds,
                    label: props.label(),
                    window,
                },
                generation,
                Instant::now(),
            );
            (up.number, up.replaced_key)
        };
        self.release(replaced.into_iter().collect()).await;
        (format_element_ref(number), window)
    }
}
