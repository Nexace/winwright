//! The engine every transport calls: windows, snapshots, inspection. Finding, actions and
//! waits live in sibling modules as further `impl Engine` blocks.

use std::sync::Arc;
use std::time::{Duration, Instant};

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
use winwright_contracts::overlay::OverlayService;
use winwright_contracts::security::{ActionRisk, Capability, PermissionDecision, ProposedAction};
use winwright_contracts::snapshot::{
    DesktopSnapshot, SnapshotRequest, SnapshotTarget, WindowSummary,
};
use winwright_contracts::system::{FileService, ProcessService};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::Policy;

use crate::lease::ActionLease;
use crate::refs::NewRef;
use crate::session::{Session, SessionRegistry};
use crate::snapshot::{Compressor, element_info, fingerprint_step};

/// Snapshots older than this many generations may be pruned once past their TTL.
pub(crate) const KEEP_GENERATIONS: u64 = 3;
/// Raw nodes captured per emitted node, before filtering.
const RAW_NODE_FACTOR: u32 = 6;
pub(crate) const RAW_NODE_LIMIT: u32 = 5_000;
const MAX_ALL_WINDOWS: usize = 24;

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
    pub(crate) sessions: SessionRegistry,
    pub(crate) lease: ActionLease,
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
            sessions: SessionRegistry::default(),
            lease: ActionLease::default(),
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

    pub(crate) fn authorize(&self, action: ProposedAction) -> WinwrightResult<()> {
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
                Err(err) => return Err(err),
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
            max_children: (request.max_list_items + 30).max(50),
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
        let (snapshot, release) = {
            let mut state = session.state();
            state.generation += 1;
            let generation = state.generation;
            let mut compressor = Compressor::new(&request, generation, now);
            for (tree, window) in &trees {
                compressor.add_tree(tree, &mut state.refs, *window);
            }
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
                    "snapshot truncated: raise maxNodes/maxDepth or snapshot a subtree".into(),
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
                tree: compressor.text,
                node_count: compressor.emitted,
                truncated: compressor.truncated,
                warnings,
                nodes: request.structured.then_some(compressor.nodes),
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

        let ancestors: Vec<String> = inspection.ancestors.iter().map(|a| a.label()).collect();
        let mut path = ancestors.join(" > ");
        if !path.is_empty() {
            path.push_str(" > ");
        }
        path.push_str(&props.label());
        Ok(ElementDetails {
            element: element_info(props, format_element_ref(number), path, true, true),
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
}
