//! The dedicated MTA thread that owns every UI Automation COM object (spec §5, ADR 0002).
//!
//! Commands arrive over a bounded channel and carry owned data plus a oneshot reply. Live
//! elements stay in `slots`, keyed by `ElementKey { worker_epoch, slot }`.

use std::collections::{BTreeMap, HashSet};
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Accessibility::*;
use windows::core::Interface;
use winwright_contracts::backend::{
    ElementKey, InspectTarget, TreeRoot, UiActionOutcome, UiInspection, UiNode, UiPatternAction,
    UiProps, UiTree, UiTreeRequest,
};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::com::{ComApartment, platform};
use crate::patterns;
use crate::props::{cache_request, read_props, read_value, skip_children};

/// HRESULTs meaning "the element went away", which are expected mid-walk.
const UIA_E_ELEMENTNOTAVAILABLE: i32 = 0x8004_0201_u32 as i32;
const UIA_E_ELEMENTNOTENABLED: i32 = 0x8004_0200_u32 as i32;

const MAX_SLOTS: usize = 50_000;
const MAX_ANCESTORS: usize = 48;

pub struct Deadline {
    pub at: Instant,
    pub cancel: CancellationToken,
}

pub enum Command {
    CaptureTree {
        request: UiTreeRequest,
        deadline: Deadline,
        reply: oneshot::Sender<WinwrightResult<UiTree>>,
    },
    Inspect {
        target: InspectTarget,
        deadline: Deadline,
        reply: oneshot::Sender<WinwrightResult<UiInspection>>,
    },
    Release {
        keys: Vec<ElementKey>,
    },
    Refresh {
        key: ElementKey,
        reply: oneshot::Sender<WinwrightResult<UiProps>>,
    },
    Execute {
        key: ElementKey,
        action: UiPatternAction,
        reply: oneshot::Sender<WinwrightResult<UiActionOutcome>>,
    },
    /// Listener count changed; the worker reconciles after every command anyway.
    SyncEvents,
}

/// On-demand UIA event listening: attached only while `listeners > 0`.
pub struct EventState {
    pub tx: Arc<tokio::sync::watch::Sender<u64>>,
    pub listeners: Arc<AtomicUsize>,
    subscribed: bool,
}

impl EventState {
    pub fn new(tx: Arc<tokio::sync::watch::Sender<u64>>, listeners: Arc<AtomicUsize>) -> Self {
        Self {
            tx,
            listeners,
            subscribed: false,
        }
    }

    fn reconcile(&mut self, automation: &IUIAutomation, root: &IUIAutomationElement) {
        let want = self.listeners.load(Ordering::Acquire) > 0;
        if want && !self.subscribed {
            self.subscribed = crate::events::subscribe(automation, root, Arc::clone(&self.tx));
            tracing::debug!(subscribed = self.subscribed, "UIA events attached");
        } else if !want && self.subscribed {
            crate::events::unsubscribe(automation);
            self.subscribed = false;
            tracing::debug!("UIA events detached");
        }
    }
}

struct Worker {
    epoch: u64,
    automation: IUIAutomation,
    /// Element + its control-view children, for walking.
    cache_children: IUIAutomationCacheRequest,
    /// Element only, for inspect and ancestor chains.
    cache_element: IUIAutomationCacheRequest,
    walker: IUIAutomationTreeWalker,
    root: IUIAutomationElement,
    next_slot: u64,
    slots: BTreeMap<u64, IUIAutomationElement>,
}

pub fn run(
    epoch: u64,
    mut rx: mpsc::Receiver<Command>,
    ready: std::sync::mpsc::Sender<WinwrightResult<()>>,
    mut events: EventState,
) {
    // Drop order matters: `_apartment` is declared first so it is dropped after `worker`.
    let _apartment = match ComApartment::init_mta() {
        Ok(a) => a,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut worker = match Worker::new(epoch) {
        Ok(w) => w,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    tracing::debug!(epoch, "UIA worker ready");

    while let Some(command) = rx.blocking_recv() {
        match command {
            Command::CaptureTree {
                request,
                deadline,
                reply,
            } => {
                let result = worker.capture(&request, &deadline);
                if let Err(Ok(tree)) = reply.send(result) {
                    // Caller gave up (cancel/timeout): free the slots it will never use.
                    let mut keys = Vec::new();
                    collect_keys(&tree.root, &mut keys);
                    worker.release(&keys);
                }
            }
            Command::Inspect {
                target,
                deadline,
                reply,
            } => {
                let result = worker.inspect(target, &deadline);
                if let Err(Ok(inspection)) = reply.send(result) {
                    worker.release(&[inspection.key]);
                }
            }
            Command::Release { keys } => worker.release(&keys),
            Command::Refresh { key, reply } => {
                let _ = reply.send(worker.refresh(key));
            }
            Command::Execute { key, action, reply } => {
                let _ = reply.send(worker.execute(key, &action));
            }
            Command::SyncEvents => {}
        }
        events.reconcile(&worker.automation, &worker.root);
    }
    tracing::debug!(epoch, slots = worker.slots.len(), "UIA worker stopping");
    // Handlers must be removed on this thread before the apartment is torn down.
    events.listeners.store(0, Ordering::Release);
    events.reconcile(&worker.automation, &worker.root);
}

fn collect_keys(node: &UiNode, out: &mut Vec<ElementKey>) {
    out.push(node.key);
    for child in &node.children {
        collect_keys(child, out);
    }
}

fn is_gone(err: &windows::core::Error) -> bool {
    matches!(
        err.code().0,
        UIA_E_ELEMENTNOTAVAILABLE | UIA_E_ELEMENTNOTENABLED
    )
}

struct Walk<'a> {
    request: &'a UiTreeRequest,
    deadline: &'a Deadline,
    count: u32,
    truncated: bool,
    /// Runtime ids already captured. Some providers expose cycles (an expanded Win32 combo
    /// box lists its own top-level window as a child) or the same element twice.
    seen: HashSet<Vec<i32>>,
}

impl Walk<'_> {
    /// Cancellation aborts; the deadline only stops the walk and marks the tree truncated.
    fn should_stop(&mut self) -> WinwrightResult<bool> {
        if self.deadline.cancel.is_cancelled() {
            return Err(WinwrightError::Cancelled);
        }
        if self.count >= self.request.max_nodes || Instant::now() >= self.deadline.at {
            self.truncated = true;
            return Ok(true);
        }
        Ok(false)
    }
}

impl Worker {
    fn new(epoch: u64) -> WinwrightResult<Self> {
        // SAFETY: COM is initialized on this thread (MTA); all interfaces stay on it.
        unsafe {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                    .or_else(|_| CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))
                    .map_err(|e| WinwrightError::BackendUnavailable {
                        backend: "UIAutomation".into(),
                        reason: format!("CoCreateInstance failed: {e}"),
                    })?;
            if let Ok(a2) = automation.cast::<IUIAutomation2>() {
                // Bound how long one hung provider can stall the worker.
                let _ = a2.SetConnectionTimeout(2_000);
                let _ = a2.SetTransactionTimeout(5_000);
            }
            let cache_children = cache_request(
                &automation,
                TreeScope(TreeScope_Element.0 | TreeScope_Children.0),
            )?;
            let cache_element = cache_request(&automation, TreeScope_Element)?;
            let walker = automation
                .ControlViewWalker()
                .map_err(|e| platform("ControlViewWalker", &e))?;
            let root = automation
                .GetRootElement()
                .map_err(|e| platform("GetRootElement", &e))?;
            Ok(Self {
                epoch,
                automation,
                cache_children,
                cache_element,
                walker,
                root,
                next_slot: 0,
                slots: BTreeMap::new(),
            })
        }
    }

    fn store(&mut self, el: IUIAutomationElement) -> ElementKey {
        self.next_slot += 1;
        self.slots.insert(self.next_slot, el);
        while self.slots.len() > MAX_SLOTS {
            self.slots.pop_first();
        }
        ElementKey {
            worker_epoch: self.epoch,
            slot: self.next_slot,
        }
    }

    fn slot(&self, key: ElementKey) -> WinwrightResult<&IUIAutomationElement> {
        if key.worker_epoch != self.epoch {
            return Err(WinwrightError::ElementStale {
                reference: format!("slot {}", key.slot),
                reason: "the automation worker restarted".into(),
            });
        }
        self.slots
            .get(&key.slot)
            .ok_or_else(|| WinwrightError::ElementStale {
                reference: format!("slot {}", key.slot),
                reason: "the element handle was released".into(),
            })
    }

    fn release(&mut self, keys: &[ElementKey]) {
        for key in keys.iter().filter(|k| k.worker_epoch == self.epoch) {
            self.slots.remove(&key.slot);
        }
    }

    /// Fresh properties for a stored element; the slot keeps the updated element.
    fn fresh(&mut self, key: ElementKey) -> WinwrightResult<(IUIAutomationElement, UiProps)> {
        let el = self.slot(key)?.clone();
        // SAFETY: COM call on an element owned by this thread.
        let fresh = unsafe { el.BuildUpdatedCache(&self.cache_element) }
            .map_err(|e| patterns::action_error("refresh", &format!("slot {}", key.slot), &e))?;
        let mut props = read_props(&fresh);
        read_value(&fresh, &mut props);
        self.slots.insert(key.slot, fresh.clone());
        Ok((fresh, props))
    }

    fn refresh(&mut self, key: ElementKey) -> WinwrightResult<UiProps> {
        self.fresh(key).map(|(_, props)| props)
    }

    fn execute(
        &mut self,
        key: ElementKey,
        action: &UiPatternAction,
    ) -> WinwrightResult<UiActionOutcome> {
        let (el, props) = self.fresh(key)?;
        let output = patterns::execute(&el, &props, action)?;
        // Read-only operations report the state we already have; mutating ones re-read.
        let props_after = match action {
            UiPatternAction::GetText { .. } | UiPatternAction::ClickablePoint => Some(props),
            // SAFETY: COM call on an element owned by this thread.
            _ => unsafe { el.BuildUpdatedCache(&self.cache_element) }
                .ok()
                .map(|updated| {
                    let mut p = read_props(&updated);
                    read_value(&updated, &mut p);
                    self.slots.insert(key.slot, updated);
                    p
                }),
        };
        Ok(UiActionOutcome {
            props_after,
            text: output.text,
            point: output.point,
        })
    }

    fn capture(&mut self, request: &UiTreeRequest, deadline: &Deadline) -> WinwrightResult<UiTree> {
        if deadline.cancel.is_cancelled() {
            return Err(WinwrightError::Cancelled);
        }
        // SAFETY: COM calls on interfaces owned by this thread.
        let root = unsafe {
            match request.root {
                TreeRoot::Window(hwnd) => self
                    .automation
                    .ElementFromHandleBuildCache(
                        HWND(hwnd as usize as *mut c_void),
                        &self.cache_children,
                    )
                    .map_err(|e| {
                        tracing::debug!(%e, hwnd, "ElementFromHandle failed");
                        WinwrightError::WindowNotFound {
                            query: format!("hwnd={hwnd:#x}"),
                        }
                    })?,
                TreeRoot::Element(key) => self
                    .slot(key)?
                    .BuildUpdatedCache(&self.cache_children)
                    .map_err(|e| {
                    if is_gone(&e) {
                        WinwrightError::ElementStale {
                            reference: format!("slot {}", key.slot),
                            reason: "the element no longer exists".into(),
                        }
                    } else {
                        platform("BuildUpdatedCache", &e)
                    }
                })?,
                TreeRoot::Desktop => self
                    .automation
                    .GetRootElementBuildCache(&self.cache_children)
                    .map_err(|e| platform("GetRootElementBuildCache", &e))?,
            }
        };
        let mut walk = Walk {
            request,
            deadline,
            count: 0,
            truncated: false,
            seen: HashSet::new(),
        };
        let root = self.walk(root, 0, &mut walk)?;
        Ok(UiTree {
            root,
            node_count: walk.count,
            truncated: walk.truncated,
        })
    }

    /// `el` must carry cached properties (and cached children, if it will be descended).
    fn walk(
        &mut self,
        el: IUIAutomationElement,
        depth: u32,
        walk: &mut Walk,
    ) -> WinwrightResult<UiNode> {
        let mut props = read_props(&el);
        read_value(&el, &mut props);
        walk.count += 1;
        if !props.runtime_id.is_empty() {
            walk.seen.insert(props.runtime_id.clone());
        }
        let descend = !skip_children(props.role);
        let key = self.store(el.clone());
        let mut node = UiNode {
            key,
            props,
            children: Vec::new(),
            children_total: 0,
        };
        if !descend {
            return Ok(node);
        }
        // SAFETY: reads the client-side cache of a live element owned by this thread.
        let Some(children) = (unsafe { el.GetCachedChildren() }).ok() else {
            return Ok(node);
        };
        // SAFETY: as above.
        let len = unsafe { children.Length() }.unwrap_or(0).max(0) as u32;
        node.children_total = len;
        if len == 0 {
            return Ok(node);
        }
        if depth + 1 >= walk.request.max_depth {
            walk.truncated = true;
            return Ok(node);
        }
        let child_is_leaf_level = depth + 2 >= walk.request.max_depth;
        // Children deliberately filtered out (offscreen, vanished) do not count as "more".
        let mut filtered = 0;
        for i in 0..len.min(walk.request.max_children) {
            if walk.should_stop()? {
                break;
            }
            // SAFETY: index is within `Length()`; the array is owned by this thread.
            let Ok(child) = (unsafe { children.GetElement(i as i32) }) else {
                filtered += 1;
                continue;
            };
            let child_props = read_props(&child);
            if child_props.offscreen && !walk.request.include_offscreen {
                filtered += 1;
                continue;
            }
            if !child_props.runtime_id.is_empty() && walk.seen.contains(&child_props.runtime_id) {
                filtered += 1;
                continue;
            }
            let child = if child_is_leaf_level || skip_children(child_props.role) {
                child
            } else {
                // SAFETY: one cross-process call caching the child's own children.
                match unsafe { child.BuildUpdatedCache(&self.cache_children) } {
                    Ok(c) => c,
                    Err(e) if is_gone(&e) => {
                        filtered += 1;
                        continue;
                    }
                    Err(e) => {
                        tracing::debug!(%e, "BuildUpdatedCache failed; keeping cached child");
                        child
                    }
                }
            };
            node.children.push(self.walk(child, depth + 1, walk)?);
        }
        node.children_total = len - filtered;
        if len > walk.request.max_children {
            walk.truncated = true;
        }
        Ok(node)
    }

    fn inspect(
        &mut self,
        target: InspectTarget,
        deadline: &Deadline,
    ) -> WinwrightResult<UiInspection> {
        if deadline.cancel.is_cancelled() {
            return Err(WinwrightError::Cancelled);
        }
        // SAFETY: COM calls on interfaces owned by this thread.
        let el = unsafe {
            match target {
                InspectTarget::Point(p) => self
                    .automation
                    .ElementFromPointBuildCache(POINT { x: p.x, y: p.y }, &self.cache_element)
                    .map_err(|e| platform("ElementFromPoint", &e))?,
                InspectTarget::Focused => self
                    .automation
                    .GetFocusedElementBuildCache(&self.cache_element)
                    .map_err(|e| platform("GetFocusedElement", &e))?,
                InspectTarget::Element(key) => self
                    .slot(key)?
                    .BuildUpdatedCache(&self.cache_element)
                    .map_err(|e| platform("BuildUpdatedCache", &e))?,
            }
        };
        let mut props = read_props(&el);
        read_value(&el, &mut props);

        let mut ancestors = Vec::new();
        let mut current = el.clone();
        while ancestors.len() < MAX_ANCESTORS && Instant::now() < deadline.at {
            // SAFETY: COM calls on interfaces owned by this thread.
            let parent = match unsafe {
                self.walker
                    .GetParentElementBuildCache(&current, &self.cache_element)
            } {
                Ok(p) => p,
                Err(_) => break,
            };
            // SAFETY: as above.
            let is_root = unsafe { self.automation.CompareElements(&parent, &self.root) }
                .map(|b| b.as_bool())
                .unwrap_or(true);
            if is_root {
                break;
            }
            ancestors.push(read_props(&parent));
            current = parent;
        }
        ancestors.reverse();
        let key = self.store(el);
        Ok(UiInspection {
            key,
            props,
            ancestors,
        })
    }
}
