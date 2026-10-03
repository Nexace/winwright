//! Waits (spec §15): re-check the real UI state until a predicate holds. UIA events only
//! wake the loop early; they are never taken as proof. Bounded polling covers missed events.

use std::time::{Duration, Instant};

use winwright_contracts::backend::{OperationContext, UiNode, UiProps};
use winwright_contracts::element::ControlRole;
use winwright_contracts::ids::format_element_ref;
use winwright_contracts::snapshot::SnapshotTarget;
use winwright_contracts::wait::{WaitRequest, WaitResult, WaitState};
use winwright_contracts::window::WindowInfo;
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::redacted_value;

use crate::engine::Engine;
use crate::find::search_limits;
use crate::locator::{Matcher, Resolution, compile, find_matches, resolve_one};
use crate::session::Session;
use crate::snapshot::element_info;

const FIRST_INTERVAL: Duration = Duration::from_millis(50);
const MAX_INTERVAL: Duration = Duration::from_millis(500);

enum Check {
    Done {
        element: Option<Box<(UiProps, String)>>,
        window: Option<Box<WindowInfo>>,
    },
    Pending(String),
}

fn visible(p: &UiProps) -> bool {
    !p.offscreen && p.bounds.is_some()
}

fn value_matches(req: &WaitRequest, text: &str) -> bool {
    let want = req.value.as_deref().unwrap_or_default();
    Matcher::new(want, req.value_match, false)
        .map(|m| m.strength(text) > 0)
        .unwrap_or(false)
}

/// Evaluates an element-level state against one element's current properties.
fn element_state(req: &WaitRequest, p: &UiProps) -> Result<(), String> {
    let ok = match req.state {
        WaitState::Exists => true,
        WaitState::Visible => visible(p),
        WaitState::Hidden => !visible(p),
        WaitState::Enabled => p.enabled,
        WaitState::Disabled => !p.enabled,
        WaitState::Focused => p.focused,
        WaitState::Value => redacted_value(p).is_some_and(|v| value_matches(req, &v)),
        WaitState::Text => {
            value_matches(req, &p.name) || redacted_value(p).is_some_and(|v| value_matches(req, &v))
        }
        WaitState::Missing | WaitState::WindowOpen | WaitState::WindowClosed => false,
    };
    if ok {
        return Ok(());
    }
    let observed = match req.state {
        WaitState::Visible | WaitState::Hidden => if visible(p) {
            "visible"
        } else {
            "hidden/offscreen"
        }
        .to_owned(),
        WaitState::Enabled | WaitState::Disabled => {
            if p.enabled { "enabled" } else { "disabled" }.to_owned()
        }
        WaitState::Focused => "not focused".to_owned(),
        WaitState::Value | WaitState::Text => match redacted_value(p) {
            Some(v) => format!("name={:?} value={:?}", p.name, v),
            None => format!("name={:?} (no value)", p.name),
        },
        _ => "present".to_owned(),
    };
    Err(format!("{} is {observed}", p.label()))
}

impl Engine {
    /// Waits until `request.state` holds, or fails with `TIMEOUT` describing what was last seen.
    pub async fn wait_for(
        &self,
        session: &Session,
        request: WaitRequest,
    ) -> WinwrightResult<WaitResult> {
        request.validate().map_err(WinwrightError::invalid)?;
        // A bad pattern is the caller's mistake, not a state that never arrives.
        if let Some(value) = &request.value {
            Matcher::new(value, request.value_match, false)?;
        }
        self.observe("desktop_wait_for")?;
        let timeout = request
            .timeout_ms
            .map(Duration::from_millis)
            .unwrap_or_else(|| self.timeout());
        let ctx = session.operation(timeout)?;
        let started = Instant::now();
        let mut events_rx = self.uia.events();
        let mut checks = 0u32;
        let mut events = 0u32;
        let mut interval = FIRST_INTERVAL;
        let mut last_seen: String;
        loop {
            if ctx.cancel.is_cancelled() {
                return Err(WinwrightError::Cancelled);
            }
            checks += 1;
            match self.check_wait(session, &request, &ctx).await {
                Ok(Check::Done { element, window }) => {
                    return Ok(WaitResult {
                        state: request.state,
                        elapsed_ms: started.elapsed().as_millis() as u64,
                        checks,
                        events,
                        element: element.map(|b| {
                            let (p, r) = *b;
                            element_info(&p, r, String::new(), false, false)
                        }),
                        window: window.map(|w| *w),
                    });
                }
                Ok(Check::Pending(seen)) => last_seen = seen,
                Err(WinwrightError::Cancelled) => return Err(WinwrightError::Cancelled),
                // The UI is changing under us: keep waiting and report it if time runs out.
                Err(
                    e @ (WinwrightError::WindowNotFound { .. }
                    | WinwrightError::ElementStale { .. }
                    | WinwrightError::Timeout { .. }
                    | WinwrightError::Platform { .. }),
                ) => last_seen = e.to_string(),
                Err(other) => return Err(other),
            }
            let remaining = ctx.remaining();
            if remaining.is_zero() {
                return Err(WinwrightError::Timeout {
                    operation: format!("wait_for {:?}; last seen: {last_seen}", request.state),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                });
            }
            let nap = interval.min(remaining);
            match events_rx.as_mut() {
                Some(sub) => {
                    tokio::select! {
                        changed = sub.rx.changed() => {
                            if changed.is_ok() {
                                events += 1;
                            } else {
                                events_rx = None;
                            }
                        }
                        () = tokio::time::sleep(nap) => {}
                        () = ctx.cancel.cancelled() => return Err(WinwrightError::Cancelled),
                    }
                }
                None => {
                    tokio::select! {
                        () = tokio::time::sleep(nap) => {}
                        () = ctx.cancel.cancelled() => return Err(WinwrightError::Cancelled),
                    }
                }
            }
            interval = (interval * 3 / 2).min(MAX_INTERVAL);
        }
    }

    async fn check_wait(
        &self,
        session: &Session,
        req: &WaitRequest,
        ctx: &OperationContext,
    ) -> WinwrightResult<Check> {
        if req.state.is_window_state() {
            let selector = req.window.as_ref().expect("validated");
            let found = self
                .windows
                .list_windows()?
                .into_iter()
                .find(|w| selector.matches(w));
            return Ok(match (req.state, found) {
                (WaitState::WindowOpen, Some(w)) => Check::Done {
                    element: None,
                    window: Some(Box::new(w)),
                },
                (WaitState::WindowOpen, None) => {
                    Check::Pending(format!("no window matches {}", selector.describe()))
                }
                (_, None) => Check::Done {
                    element: None,
                    window: None,
                },
                (_, Some(w)) => Check::Pending(format!("window {:?} is still open", w.title)),
            });
        }

        if let Some(reference) = &req.reference {
            let entry = session
                .state()
                .refs
                .get_live(reference, self.uia.worker_epoch())?
                .clone();
            return Ok(match self.uia.refresh(entry.key, ctx).await {
                Ok(props) if req.state == WaitState::Missing => {
                    Check::Pending(format!("{} still exists", props.label()))
                }
                Ok(props) => match element_state(req, &props) {
                    Ok(()) => Check::Done {
                        element: Some(Box::new((props, reference.clone()))),
                        window: None,
                    },
                    Err(seen) => Check::Pending(seen),
                },
                Err(WinwrightError::ElementStale { .. })
                    if matches!(req.state, WaitState::Missing | WaitState::Hidden) =>
                {
                    Check::Done {
                        element: None,
                        window: None,
                    }
                }
                Err(WinwrightError::ElementStale { .. }) => {
                    Check::Pending(format!("{reference} no longer exists"))
                }
                Err(e) => return Err(e),
            });
        }

        let mut locator = req.locator.clone().expect("validated");
        // Presence states must see hidden elements too.
        if matches!(
            req.state,
            WaitState::Exists | WaitState::Missing | WaitState::Hidden
        ) {
            locator.visible_only = false;
        }
        let compiled = compile(&locator)?;
        // A window/dialog may appear as a new top-level window, not inside the active one.
        let scope = match (
            &req.scope,
            locator.role.as_deref().and_then(ControlRole::parse),
        ) {
            (SnapshotTarget::Active, Some(ControlRole::Window | ControlRole::Dialog)) => {
                SnapshotTarget::AllWindows
            }
            (scope, _) => scope.clone(),
        };
        let captured = self
            .capture_scope(session, &scope, search_limits(!locator.visible_only), ctx)
            .await?;
        let roots: Vec<&UiNode> = captured.trees.iter().map(|(t, _)| &t.root).collect();
        let mut matches = find_matches(&compiled, &roots);
        // `nth` narrows every state to that one match (document order), presence states too.
        if let Some(n) = compiled.nth {
            matches = matches.into_iter().nth(n).into_iter().collect();
        }
        let describe = || compiled.description.clone();
        // Decide which match (if any) satisfies the state, or why it is still pending.
        let decision: Result<Option<usize>, String> = match req.state {
            WaitState::Exists => matches
                .first()
                .map(|_| Some(0))
                .ok_or_else(|| format!("no element matches {}", describe())),
            WaitState::Visible => match matches.iter().position(|m| visible(&m.node.props)) {
                Some(i) => Ok(Some(i)),
                None if matches.is_empty() => Err(format!("no element matches {}", describe())),
                None => Err(format!("{} exists but is not visible", describe())),
            },
            WaitState::Missing => {
                if matches.is_empty() {
                    Ok(None)
                } else {
                    Err(format!(
                        "{} element(s) still match {}",
                        matches.len(),
                        describe()
                    ))
                }
            }
            WaitState::Hidden => {
                if matches.iter().any(|m| visible(&m.node.props)) {
                    Err(format!("{} is still visible", describe()))
                } else {
                    Ok(None)
                }
            }
            _ => match resolve_one(&matches, None) {
                Resolution::One(i) => element_state(req, &matches[i].node.props).map(|()| Some(i)),
                Resolution::NotFound => Err(format!("no element matches {}", describe())),
                Resolution::Ambiguous(ix) => {
                    let labels = ix
                        .iter()
                        .take(10)
                        .map(|&i| matches[i].node.props.label())
                        .collect();
                    self.release_trees(captured.trees).await;
                    return Err(WinwrightError::ElementAmbiguous {
                        locator: describe(),
                        matches: labels,
                    });
                }
            },
        };
        match decision {
            Err(seen) => {
                self.release_trees(captured.trees).await;
                Ok(Check::Pending(seen))
            }
            Ok(None) => {
                self.release_trees(captured.trees).await;
                Ok(Check::Done {
                    element: None,
                    window: None,
                })
            }
            Ok(Some(i)) => {
                let m = &matches[i];
                let props = m.node.props.clone();
                let (recorded, release) = self.record_matches(session, &[m], &captured.trees);
                self.release(release).await;
                Ok(Check::Done {
                    element: Some(Box::new((props, format_element_ref(recorded[0].0)))),
                    window: None,
                })
            }
        }
    }

    async fn release_trees(&self, trees: Vec<(winwright_contracts::backend::UiTree, Option<u64>)>) {
        fn all(n: &UiNode, out: &mut Vec<winwright_contracts::backend::ElementKey>) {
            out.push(n.key);
            for c in &n.children {
                all(c, out);
            }
        }
        let mut keys = Vec::new();
        for (t, _) in &trees {
            all(&t.root, &mut keys);
        }
        self.release(keys).await;
    }
}
