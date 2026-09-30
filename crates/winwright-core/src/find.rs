//! `find` and target resolution (spec §9, §10, §65).

use std::collections::HashSet;
use std::time::Instant;

use winwright_contracts::action::ElementTarget;
use winwright_contracts::backend::{
    ElementIdentity, ElementKey, OperationContext, TreeRoot, UiNode, UiProps, UiTree,
};
use winwright_contracts::ids::format_element_ref;
use winwright_contracts::locator::{FindRequest, FindResult};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::engine::{Engine, Limits, RAW_NODE_LIMIT, Root};
use crate::locator::{Match, Resolution, compile, find_matches, ranked, resolve_one};
use crate::refs::NewRef;
use crate::session::Session;
use crate::snapshot::{element_info, fingerprint_step};

/// Search captures go deep and wide: a locator must see what a snapshot would truncate.
pub(crate) fn search_limits(include_offscreen: bool) -> Limits {
    Limits {
        max_depth: 32,
        max_nodes: RAW_NODE_LIMIT,
        max_children: 1_000,
        include_offscreen,
    }
}

/// A target resolved to one live element, already recorded under a ref.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub reference: String,
    pub key: ElementKey,
    pub props: UiProps,
    pub window: Option<u64>,
}

impl Resolved {
    pub fn label(&self) -> String {
        self.props.label()
    }
}

fn fingerprint_of(m: &Match) -> u64 {
    let fp = m
        .ancestors
        .iter()
        .fold(0, |fp, a| fingerprint_step(fp, &a.props));
    fingerprint_step(fp, &m.node.props)
}

/// Readable path: named or interactive ancestors only (layout panes add nothing).
fn path_of(m: &Match) -> String {
    let mut parts: Vec<String> = m
        .ancestors
        .iter()
        .filter(|a| !a.props.name.trim().is_empty() || a.props.role.is_interactive())
        .map(|a| a.props.label())
        .collect();
    parts.push(m.node.props.label());
    parts.join(" > ")
}

fn window_of(m: &Match, trees: &[(UiTree, Option<u64>)]) -> Option<u64> {
    let root = m.ancestors.first().copied().unwrap_or(m.node);
    trees
        .iter()
        .find(|(t, _)| std::ptr::eq(&t.root, root))
        .and_then(|(_, w)| *w)
}

fn all_keys(node: &UiNode, out: &mut Vec<ElementKey>) {
    out.push(node.key);
    for c in &node.children {
        all_keys(c, out);
    }
}

impl Engine {
    /// Records matches under refs (keeping their slots) and returns the rest for release.
    fn record_matches(
        &self,
        session: &Session,
        matches: &[&Match],
        trees: &[(UiTree, Option<u64>)],
    ) -> (Vec<(u64, String)>, Vec<ElementKey>) {
        let now = Instant::now();
        let mut kept = HashSet::new();
        let mut release = Vec::new();
        let mut recorded = Vec::with_capacity(matches.len());
        {
            let mut state = session.state();
            let generation = state.generation;
            for m in matches {
                let props = &m.node.props;
                let up = state.refs.upsert(
                    NewRef {
                        identity: ElementIdentity::from_props(props, fingerprint_of(m)),
                        key: m.node.key,
                        bounds: props.bounds,
                        label: props.label(),
                        window: window_of(m, trees),
                    },
                    generation,
                    now,
                );
                kept.insert(m.node.key);
                release.extend(up.replaced_key);
                recorded.push((up.number, path_of(m)));
            }
        }
        let mut keys = Vec::new();
        for (t, _) in trees {
            all_keys(&t.root, &mut keys);
        }
        release.extend(keys.into_iter().filter(|k| !kept.contains(k)));
        (recorded, release)
    }

    pub async fn find(
        &self,
        session: &Session,
        request: FindRequest,
    ) -> WinwrightResult<FindResult> {
        self.observe("desktop_find")?;
        let locator = request.locator();
        let compiled = compile(&locator)?;
        let ctx = session.operation(self.timeout())?;
        let captured = self
            .capture_scope(
                session,
                &request.scope,
                search_limits(!locator.visible_only),
                &ctx,
            )
            .await?;
        let roots: Vec<&UiNode> = captured.trees.iter().map(|(t, _)| &t.root).collect();
        let mut matches = find_matches(&compiled, &roots);
        let count = matches.len() as u32;
        let selected: Vec<&Match> = match compiled.nth {
            Some(n) => matches.get(n).into_iter().collect(),
            None => {
                ranked(&mut matches);
                matches.iter().take(request.limit.max(1) as usize).collect()
            }
        };
        let (recorded, release) = self.record_matches(session, &selected, &captured.trees);
        let result = FindResult {
            count,
            matches: selected
                .iter()
                .zip(recorded)
                .map(|(m, (number, path))| {
                    element_info(
                        &m.node.props,
                        format_element_ref(number),
                        path,
                        false,
                        false,
                    )
                })
                .collect(),
            warnings: captured.warnings,
        };
        self.release(release).await;
        Ok(result)
    }

    /// Resolves an action target to one live element (spec §9 steps 1–4).
    pub(crate) async fn resolve_target(
        &self,
        session: &Session,
        target: &ElementTarget,
        ctx: &OperationContext,
    ) -> WinwrightResult<Resolved> {
        target.validate().map_err(WinwrightError::invalid)?;
        match (&target.reference, &target.locator) {
            (Some(reference), _) => self.resolve_ref(session, reference, ctx).await,
            (None, Some(locator)) => {
                let compiled = compile(locator)?;
                let captured = self
                    .capture_scope(
                        session,
                        &target.scope,
                        search_limits(!locator.visible_only),
                        ctx,
                    )
                    .await?;
                let roots: Vec<&UiNode> = captured.trees.iter().map(|(t, _)| &t.root).collect();
                let matches = find_matches(&compiled, &roots);
                match resolve_one(&matches, compiled.nth) {
                    Resolution::One(i) => {
                        let (recorded, release) =
                            self.record_matches(session, &[&matches[i]], &captured.trees);
                        self.release(release).await;
                        let number = recorded[0].0;
                        Ok(Resolved {
                            reference: format_element_ref(number),
                            key: matches[i].node.key,
                            props: matches[i].node.props.clone(),
                            window: window_of(&matches[i], &captured.trees),
                        })
                    }
                    Resolution::NotFound => {
                        let mut keys = Vec::new();
                        for (t, _) in &captured.trees {
                            all_keys(&t.root, &mut keys);
                        }
                        self.release(keys).await;
                        Err(WinwrightError::ElementNotFound {
                            locator: compiled.description,
                        })
                    }
                    Resolution::Ambiguous(indices) => {
                        let tied: Vec<&Match> = indices.iter().map(|&i| &matches[i]).collect();
                        let (recorded, release) =
                            self.record_matches(session, &tied, &captured.trees);
                        self.release(release).await;
                        Err(WinwrightError::ElementAmbiguous {
                            locator: compiled.description,
                            matches: recorded
                                .iter()
                                .map(|(n, path)| format!("{} {path}", format_element_ref(*n)))
                                .collect(),
                        })
                    }
                }
            }
            (None, None) => unreachable!("validated above"),
        }
    }

    /// Exact element first; if gone or changed, re-resolve by identity in its window.
    async fn resolve_ref(
        &self,
        session: &Session,
        reference: &str,
        ctx: &OperationContext,
    ) -> WinwrightResult<Resolved> {
        let epoch = self.uia.worker_epoch();
        let entry = session.state().refs.get_live(reference, epoch)?.clone();
        match self.uia.refresh(entry.key, ctx).await {
            Ok(props) => {
                let identity =
                    ElementIdentity::from_props(&props, entry.identity.ancestor_fingerprint);
                if identity.same_element(&entry.identity) {
                    session.state().refs.rebind(
                        entry.number,
                        NewRef {
                            identity,
                            key: entry.key,
                            bounds: props.bounds,
                            label: props.label(),
                            window: entry.window,
                        },
                        entry.generation,
                        Instant::now(),
                    );
                    return Ok(Resolved {
                        reference: reference.to_owned(),
                        key: entry.key,
                        props,
                        window: entry.window,
                    });
                }
                tracing::debug!(reference, "ref points at a different element; re-resolving");
            }
            Err(WinwrightError::ElementStale { .. }) => {
                tracing::debug!(reference, "ref element is gone; re-resolving");
            }
            Err(other) => return Err(other),
        }
        self.reresolve(session, reference, &entry, ctx).await
    }

    async fn reresolve(
        &self,
        session: &Session,
        reference: &str,
        entry: &crate::refs::RefEntry,
        ctx: &OperationContext,
    ) -> WinwrightResult<Resolved> {
        let stale = |reason: &str| WinwrightError::ElementStale {
            reference: reference.to_owned(),
            reason: reason.to_owned(),
        };
        let Some(window) = entry.window else {
            return Err(stale("the element is gone and its window is unknown"));
        };
        if self.windows.window(window)?.is_none() {
            return Err(stale("the element's window has closed"));
        }
        let root = Root {
            tree: TreeRoot::Window(window),
            window: Some(window),
        };
        let (trees, _) = self
            .capture_roots(vec![root], search_limits(true), ctx)
            .await?;
        let want = &entry.identity;
        let roots: Vec<&UiNode> = trees.iter().map(|(t, _)| &t.root).collect();
        let mut candidates: Vec<(u64, &UiNode, Vec<&UiNode>)> = Vec::new();
        fn walk<'a>(
            node: &'a UiNode,
            stack: &mut Vec<&'a UiNode>,
            want: &ElementIdentity,
            out: &mut Vec<(u64, &'a UiNode, Vec<&'a UiNode>)>,
        ) {
            let p = &node.props;
            if p.process_id == want.process_id
                && p.control_type_id == want.control_type_id
                && p.automation_id == want.automation_id
                && p.class_name == want.class_name
                && p.framework_id == want.framework_id
                && p.name == want.name
            {
                let fp = stack.iter().fold(0, |fp, a| fingerprint_step(fp, &a.props));
                out.push((fingerprint_step(fp, p), node, stack.clone()));
            }
            stack.push(node);
            for c in &node.children {
                walk(c, stack, want, out);
            }
            stack.pop();
        }
        for r in &roots {
            walk(r, &mut Vec::new(), want, &mut candidates);
        }
        // Prefer candidates in the same structural position.
        let same_place: Vec<_> = candidates
            .iter()
            .filter(|(fp, _, _)| *fp == want.ancestor_fingerprint)
            .collect();
        let pool: Vec<_> = if same_place.is_empty() {
            candidates.iter().collect()
        } else {
            same_place
        };
        let chosen = match pool.len() {
            0 => None,
            1 => Some(pool[0]),
            n => {
                let mut keys = Vec::new();
                for (t, _) in &trees {
                    all_keys(&t.root, &mut keys);
                }
                self.release(keys).await;
                return Err(WinwrightError::ElementAmbiguous {
                    locator: format!("{reference} ({})", entry.label),
                    matches: vec![format!("{n} elements now match the original identity")],
                });
            }
        };
        let mut release = Vec::new();
        for (t, _) in &trees {
            all_keys(&t.root, &mut release);
        }
        let Some((fp, node, _)) = chosen else {
            self.release(release).await;
            return Err(stale(
                "no element with the original identity exists any more",
            ));
        };
        let props = node.props.clone();
        let key = node.key;
        release.retain(|k| *k != key);
        let replaced = session.state().refs.rebind(
            entry.number,
            NewRef {
                identity: ElementIdentity::from_props(&props, *fp),
                key,
                bounds: props.bounds,
                label: props.label(),
                window: Some(window),
            },
            entry.generation,
            Instant::now(),
        );
        release.extend(replaced);
        self.release(release).await;
        tracing::debug!(reference, "re-resolved stale ref");
        Ok(Resolved {
            reference: reference.to_owned(),
            key,
            props,
            window: Some(window),
        })
    }
}
