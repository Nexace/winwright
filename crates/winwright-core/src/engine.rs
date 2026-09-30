//! The engine every transport calls: windows, snapshots, inspection.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winwright_contracts::backend::{
    ElementIdentity, InspectTarget, TreeRoot, UiAutomationBackend, UiTree, UiTreeRequest,
    WindowBackend,
};
use winwright_contracts::config::Config;
use winwright_contracts::element::ElementDetails;
use winwright_contracts::geometry::PhysicalPoint;
use winwright_contracts::ids::{SessionId, format_element_ref, format_generation};
use winwright_contracts::security::{ActionRisk, Capability, PermissionDecision, ProposedAction};
use winwright_contracts::snapshot::{
    DesktopSnapshot, SnapshotRequest, SnapshotTarget, WindowSummary,
};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::Policy;

use crate::lease::ActionLease;
use crate::session::{Session, SessionRegistry};
use crate::snapshot::{Compressor, element_info, fingerprint_step};

/// Snapshots older than this many generations may be pruned once past their TTL.
const KEEP_GENERATIONS: u64 = 3;
/// Raw nodes captured per emitted node, before filtering.
const RAW_NODE_FACTOR: u32 = 6;
const RAW_NODE_LIMIT: u32 = 5_000;
const MAX_ALL_WINDOWS: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectRequest {
    UnderCursor,
    Focused,
    Point(PhysicalPoint),
    Ref(String),
}

pub struct Engine {
    config: Config,
    policy: Policy,
    windows: Arc<dyn WindowBackend>,
    uia: Arc<dyn UiAutomationBackend>,
    sessions: SessionRegistry,
    lease: ActionLease,
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
            sessions: SessionRegistry::default(),
            lease: ActionLease::default(),
        }
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

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.config.automation.default_timeout_ms)
    }

    fn ref_ttl(&self) -> Duration {
        Duration::from_secs(self.config.automation.reference_ttl_seconds)
    }

    fn authorize(&self, action: ProposedAction) -> WinwrightResult<()> {
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

    fn observe(&self, tool: &str) -> WinwrightResult<()> {
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

    pub async fn snapshot(
        &self,
        session: &Session,
        request: SnapshotRequest,
    ) -> WinwrightResult<DesktopSnapshot> {
        request.validate().map_err(WinwrightError::invalid)?;
        self.observe("desktop_snapshot")?;
        let ctx = session.operation(self.timeout())?;
        let epoch = self.uia.worker_epoch();

        let mut active = None;
        let roots: Vec<TreeRoot> = match &request.target {
            SnapshotTarget::Active => {
                let w = self.active_window()?;
                let root = TreeRoot::Window(w.hwnd);
                active = Some(w);
                vec![root]
            }
            SnapshotTarget::Window(selector) => {
                let w = self.find_window(selector)?;
                let root = TreeRoot::Window(w.hwnd);
                active = Some(w);
                vec![root]
            }
            SnapshotTarget::AllWindows => self
                .list_windows()?
                .iter()
                .filter(|w| !w.minimized)
                .take(MAX_ALL_WINDOWS)
                .map(|w| TreeRoot::Window(w.hwnd))
                .collect(),
            SnapshotTarget::Subtree { reference } => {
                let key = session.state().refs.get_live(reference, epoch)?.key;
                vec![TreeRoot::Element(key)]
            }
        };

        let raw_budget = request
            .max_nodes
            .saturating_mul(RAW_NODE_FACTOR)
            .clamp(request.max_nodes, RAW_NODE_LIMIT.max(request.max_nodes));
        let max_children = (request.max_list_items + 30).max(50);
        let mut trees: Vec<UiTree> = Vec::with_capacity(roots.len());
        let mut warnings = Vec::new();
        let mut used = 0u32;
        let started = Instant::now();
        for root in roots {
            let remaining = raw_budget.saturating_sub(used);
            if remaining == 0 {
                warnings.push("raw capture budget exhausted before all windows".into());
                break;
            }
            let tree_request = UiTreeRequest {
                root,
                max_depth: request.max_depth,
                max_nodes: remaining,
                max_children,
                include_offscreen: request.include_offscreen,
            };
            match self.uia.capture_tree(tree_request, &ctx).await {
                Ok(tree) => {
                    used += tree.node_count;
                    trees.push(tree);
                }
                Err(err)
                    if request.target == SnapshotTarget::AllWindows
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

        let now = Instant::now();
        let (snapshot, release) = {
            let mut state = session.state();
            state.generation += 1;
            let generation = state.generation;
            let mut compressor = Compressor::new(&request, generation, now);
            for tree in &trees {
                compressor.add_tree(tree, &mut state.refs);
            }
            let mut release = compressor.unreferenced_keys(&trees);
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
        if let Err(err) = self.uia.release(release).await {
            tracing::warn!(%err, "failed to release UIA slots");
        }
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
        let target = match &request {
            InspectRequest::UnderCursor => InspectTarget::Point(self.windows.cursor_position()?),
            InspectRequest::Focused => InspectTarget::Focused,
            InspectRequest::Point(p) => InspectTarget::Point(*p),
            InspectRequest::Ref(r) => {
                InspectTarget::Element(session.state().refs.get_live(r, epoch)?.key)
            }
        };
        let inspection = self.uia.inspect(target, &ctx).await?;

        let fingerprint = inspection.ancestors.iter().fold(0, fingerprint_step);
        let fingerprint = fingerprint_step(fingerprint, &inspection.props);
        let props = &inspection.props;
        let (number, replaced) = {
            let mut state = session.state();
            let generation = state.generation;
            let up = state.refs.upsert(
                ElementIdentity::from_props(props, fingerprint),
                inspection.key,
                props.bounds,
                props.label(),
                generation,
                Instant::now(),
            );
            (up.number, up.replaced_key)
        };
        if let Some(old) = replaced
            && let Err(err) = self.uia.release(vec![old]).await
        {
            tracing::warn!(%err, "failed to release UIA slot");
        }

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
