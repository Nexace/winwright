//! Snapshot compression (spec §8, §35): raw UIA tree -> compact, ref-annotated text.
//!
//! Pass 1 selects nodes (keep / flatten / prune) and applies budgets. Pass 2 assigns refs only
//! to emitted nodes and renders, so dropped containers never consume ref numbers.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;

use winwright_contracts::backend::{ElementIdentity, ElementKey, UiNode, UiProps, UiTree};
use winwright_contracts::element::{ControlRole, ElementInfo, ExpandState, ToggleState, UiPattern};
use winwright_contracts::snapshot::{CellValue, SnapshotNode, SnapshotRequest};
use winwright_security::{is_sensitive, redacted_value};

use crate::refs::RefTable;

const MAX_NAME_CHARS: usize = 120;
const MAX_VALUE_CHARS: usize = 120;
/// Non-list containers get a generous cap so one pathological pane cannot eat the budget.
const GENERIC_CHILD_CAP_FACTOR: u32 = 5;

pub fn fingerprint_step(parent: u64, props: &UiProps) -> u64 {
    let mut h = DefaultHasher::new();
    parent.hash(&mut h);
    props.control_type_id.hash(&mut h);
    props.class_name.hash(&mut h);
    props.automation_id.hash(&mut h);
    h.finish()
}

/// Invisible bidi marks (Explorer dates are full of them) cost tokens and carry no meaning.
fn is_bidi_mark(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars().filter(|c| !is_bidi_mark(*c));
    let mut out: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// Rows whose read-only cells are folded into the row's own line.
fn is_row(role: ControlRole) -> bool {
    matches!(
        role,
        ControlRole::ListItem | ControlRole::DataItem | ControlRole::TreeItem
    )
}

/// A leaf grid/table cell with nothing to act on besides its value.
fn is_cell(node: &UiNode) -> bool {
    let p = &node.props;
    matches!(p.role, ControlRole::Edit | ControlRole::Text)
        && node.children.is_empty()
        && (p.has_pattern(UiPattern::GridItem) || p.has_pattern(UiPattern::TableItem))
        && !p.patterns.iter().any(|x| {
            matches!(
                x,
                UiPattern::Invoke
                    | UiPattern::Toggle
                    | UiPattern::ExpandCollapse
                    | UiPattern::SelectionItem
            )
        })
        && p.value_read_only != Some(false)
        && !is_sensitive(p)
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Roles whose children are homogeneous items and get `maxListItems` truncation.
fn is_list_like(role: ControlRole) -> bool {
    matches!(
        role,
        ControlRole::List
            | ControlRole::Tree
            | ControlRole::TreeItem
            | ControlRole::DataGrid
            | ControlRole::Table
            | ControlRole::Menu
            | ControlRole::ComboBox
            | ControlRole::Tab
    )
}

fn is_actionable(props: &UiProps) -> bool {
    props.patterns.iter().any(|p| p.is_actionable())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Decision {
    Keep,
    /// Kept only if something below it is emitted (named containers).
    KeepIfChildren,
    Flatten,
    Prune,
}

fn decide(node: &UiNode, is_root: bool, parent_name: &str, req: &SnapshotRequest) -> Decision {
    let p = &node.props;
    if is_root {
        return Decision::Keep;
    }
    if p.offscreen && !req.include_offscreen {
        return Decision::Prune;
    }
    if p.bounds.is_none() && !req.include_offscreen && !req.raw_debug {
        return if node.children.is_empty() {
            Decision::Prune
        } else {
            Decision::Flatten
        };
    }
    if req.raw_debug {
        return Decision::Keep;
    }
    let named = !p.name.trim().is_empty();
    match p.role {
        ControlRole::ScrollBar | ControlRole::Thumb | ControlRole::Separator => Decision::Prune,
        ControlRole::TitleBar => Decision::Flatten,
        ControlRole::Image => {
            if is_actionable(p) {
                Decision::Keep
            } else {
                Decision::Prune
            }
        }
        ControlRole::Text => {
            if req.include_text && named && p.name != parent_name {
                Decision::Keep
            } else {
                Decision::Prune
            }
        }
        role if role.is_interactive() => Decision::Keep,
        _ if is_actionable(p) => Decision::Keep,
        _ if named && !req.interactive_only => Decision::Keep,
        _ if named => Decision::KeepIfChildren,
        _ => Decision::Flatten,
    }
}

struct Sel<'a> {
    node: &'a UiNode,
    fingerprint: u64,
    cells: Vec<CellValue>,
    children: Vec<Sel<'a>>,
    /// `(total, shown)` when the child list was truncated.
    child_count: Option<(u32, u32)>,
}

/// Accumulates one snapshot across one or more captured trees.
pub struct Compressor<'r> {
    req: &'r SnapshotRequest,
    generation: u64,
    now: Instant,
    pub text: String,
    pub nodes: Vec<SnapshotNode>,
    pub emitted: u32,
    pub truncated: bool,
    kept: HashSet<ElementKey>,
    replaced: Vec<ElementKey>,
    /// Ref of the first emitted root, used as the active window ref.
    pub first_root_ref: Option<String>,
}

impl<'r> Compressor<'r> {
    pub fn new(req: &'r SnapshotRequest, generation: u64, now: Instant) -> Self {
        Self {
            req,
            generation,
            now,
            text: String::new(),
            nodes: Vec::new(),
            emitted: 0,
            truncated: false,
            kept: HashSet::new(),
            replaced: Vec::new(),
            first_root_ref: None,
        }
    }

    pub fn add_tree(&mut self, tree: &UiTree, refs: &mut RefTable) {
        self.truncated |= tree.truncated;
        let mut budget = self.req.max_nodes.saturating_sub(self.emitted);
        let mut sels = Vec::new();
        self.select(&tree.root, true, "", 0, &mut budget, &mut sels);
        for sel in &sels {
            let node = self.render(sel, 0, "", refs);
            if self.first_root_ref.is_none() {
                self.first_root_ref = Some(node.element.reference.clone());
            }
            if self.req.structured {
                self.nodes.push(node);
            }
        }
    }

    /// Worker slots not referenced by any emitted node, plus slots replaced by ref reuse.
    pub fn unreferenced_keys(&self, trees: &[UiTree]) -> Vec<ElementKey> {
        fn walk(node: &UiNode, kept: &HashSet<ElementKey>, out: &mut Vec<ElementKey>) {
            if !kept.contains(&node.key) {
                out.push(node.key);
            }
            for c in &node.children {
                walk(c, kept, out);
            }
        }
        let mut out = self.replaced.clone();
        for t in trees {
            walk(&t.root, &self.kept, &mut out);
        }
        out
    }

    fn select<'a>(
        &mut self,
        node: &'a UiNode,
        is_root: bool,
        parent_name: &str,
        parent_fp: u64,
        budget: &mut u32,
        out: &mut Vec<Sel<'a>>,
    ) {
        if *budget == 0 {
            self.truncated = true;
            return;
        }
        let decision = decide(node, is_root, parent_name, self.req);
        if decision == Decision::Prune {
            return;
        }
        let fingerprint = fingerprint_step(parent_fp, &node.props);
        let own_name = if node.props.name.is_empty() {
            parent_name
        } else {
            node.props.name.as_str()
        };

        if decision == Decision::Flatten {
            for child in &node.children {
                self.select(child, false, own_name, fingerprint, budget, out);
            }
            return;
        }

        // Reserve this node's slot before children so the budget is honest.
        *budget -= 1;
        let cap = if is_list_like(node.props.role) {
            self.req.max_list_items
        } else {
            self.req
                .max_list_items
                .saturating_mul(GENERIC_CHILD_CAP_FACTOR)
        };
        let fold_cells = is_row(node.props.role) && !self.req.raw_debug;
        let mut cells = Vec::new();
        let mut children = Vec::new();
        let mut list_truncated = false;
        for child in &node.children {
            if fold_cells && is_cell(child) {
                let value = child.props.value.as_deref().unwrap_or_default();
                if !value.is_empty() && value != node.props.name {
                    cells.push(CellValue {
                        name: truncate(&child.props.name, MAX_NAME_CHARS),
                        value: truncate(value, MAX_VALUE_CHARS),
                    });
                }
                continue;
            }
            if children.len() as u32 >= cap {
                list_truncated = true;
                break;
            }
            self.select(child, false, own_name, fingerprint, budget, &mut children);
        }
        if children.len() as u32 > cap {
            children.truncate(cap as usize);
            list_truncated = true;
        }
        let provider_total = node.children_total.max(node.children.len() as u32);
        let backend_capped = node.children_total > node.children.len() as u32;
        let child_count = (list_truncated || backend_capped).then(|| {
            let shown = children.len() as u32;
            (provider_total.max(shown), shown)
        });

        if decision == Decision::KeepIfChildren && children.is_empty() {
            *budget += 1;
            return;
        }
        out.push(Sel {
            node,
            fingerprint,
            cells,
            children,
            child_count,
        });
    }

    fn render(
        &mut self,
        sel: &Sel,
        depth: usize,
        parent_path: &str,
        refs: &mut RefTable,
    ) -> SnapshotNode {
        let props = &sel.node.props;
        let identity = ElementIdentity::from_props(props, sel.fingerprint);
        let upserted = refs.upsert(
            identity,
            sel.node.key,
            props.bounds,
            props.label(),
            self.generation,
            self.now,
        );
        self.kept.insert(sel.node.key);
        self.replaced.extend(upserted.replaced_key);
        self.emitted += 1;

        let reference = winwright_contracts::ids::format_element_ref(upserted.number);
        let path = if parent_path.is_empty() {
            props.label()
        } else {
            format!("{parent_path} > {}", props.label())
        };
        let info = element_info(
            props,
            reference,
            if self.req.structured {
                path.clone()
            } else {
                String::new()
            },
            self.req.include_bounds,
            self.req.include_patterns,
        );
        render_line(&mut self.text, depth, props, &info, sel, self.req);

        let children = sel
            .children
            .iter()
            .map(|c| self.render(c, depth + 1, &path, refs))
            .collect();
        SnapshotNode {
            element: info,
            cells: sel.cells.clone(),
            children,
            child_count: sel.child_count.map(|(total, _)| total),
        }
    }
}

/// Wire view of one element. Values are redacted for sensitive fields.
pub fn element_info(
    props: &UiProps,
    reference: String,
    path: String,
    include_bounds: bool,
    include_patterns: bool,
) -> ElementInfo {
    let sensitive = is_sensitive(props)
        && (props.role == ControlRole::Edit
            || props.has_pattern(winwright_contracts::element::UiPattern::Value));
    ElementInfo {
        reference,
        role: props.role,
        name: props.name.clone(),
        automation_id: props.automation_id.clone(),
        class_name: props.class_name.clone(),
        framework: props.framework_id.clone(),
        enabled: props.enabled,
        visible: !props.offscreen,
        focused: props.focused,
        bounds: if include_bounds { props.bounds } else { None },
        patterns: if include_patterns {
            props.patterns.clone()
        } else {
            Vec::new()
        },
        value: redacted_value(props).map(|v| truncate(&v, MAX_VALUE_CHARS)),
        sensitive,
        toggle_state: props.toggle_state,
        expand_state: props.expand_state,
        selected: props.selected,
        path,
    }
}

fn render_line(
    out: &mut String,
    depth: usize,
    props: &UiProps,
    info: &ElementInfo,
    sel: &Sel,
    req: &SnapshotRequest,
) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    out.push_str(&props.role.tag());
    if !props.name.is_empty() {
        let _ = write!(out, " {}", quote(&truncate(&props.name, MAX_NAME_CHARS)));
    } else if !props.automation_id.is_empty() {
        let _ = write!(
            out,
            " id={}",
            quote(&truncate(&props.automation_id, MAX_NAME_CHARS))
        );
    }
    if let Some(v) = &info.value {
        let _ = write!(out, " value={}", quote(v));
    }
    if info.sensitive {
        out.push_str(" sensitive=true");
    }
    if !props.enabled {
        out.push_str(" disabled");
    }
    if props.focused {
        out.push_str(" focused");
    }
    match props.toggle_state {
        Some(ToggleState::On) => out.push_str(" checked"),
        Some(ToggleState::Off) => out.push_str(" unchecked"),
        Some(ToggleState::Indeterminate) => out.push_str(" mixed"),
        None => {}
    }
    match props.expand_state {
        Some(ExpandState::Expanded) => out.push_str(" expanded"),
        Some(ExpandState::Collapsed) => out.push_str(" collapsed"),
        Some(ExpandState::PartiallyExpanded) => out.push_str(" partially-expanded"),
        Some(ExpandState::LeafNode) | None => {}
    }
    if props.selected == Some(true) {
        out.push_str(" selected");
    }
    if props.offscreen {
        out.push_str(" offscreen");
    }
    if req.include_bounds
        && let Some(b) = props.bounds
    {
        let _ = write!(out, " @[{},{},{},{}]", b.left, b.top, b.right, b.bottom);
    }
    if req.include_patterns && !props.patterns.is_empty() {
        let names: Vec<String> = props.patterns.iter().map(|p| format!("{p:?}")).collect();
        let _ = write!(out, " {{{}}}", names.join(","));
    }
    let _ = write!(out, " [{}]", info.reference);
    for cell in &sel.cells {
        let _ = write!(out, " {}={}", cell.name, quote(&cell.value));
    }
    if let Some((total, shown)) = sel.child_count {
        let _ = write!(out, " children={total} showing={shown}");
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::element::UiPattern;
    use winwright_contracts::geometry::PhysicalRect;

    struct B {
        next: u64,
    }

    impl B {
        fn node(&mut self, role: ControlRole, name: &str, children: Vec<UiNode>) -> UiNode {
            self.next += 1;
            let patterns = match role {
                ControlRole::Button => vec![UiPattern::Invoke],
                ControlRole::Edit => vec![UiPattern::Value],
                _ => vec![],
            };
            UiNode {
                key: ElementKey {
                    worker_epoch: 1,
                    slot: self.next,
                },
                props: UiProps {
                    role,
                    control_type_id: 50000 + self.next as i32,
                    name: name.into(),
                    runtime_id: vec![42, self.next as i32],
                    process_id: 7,
                    bounds: Some(PhysicalRect::new(0, 0, 10, 10)),
                    enabled: true,
                    patterns,
                    ..Default::default()
                },
                children_total: children.len() as u32,
                children,
            }
        }
    }

    fn tree(root: UiNode) -> UiTree {
        UiTree {
            root,
            node_count: 0,
            truncated: false,
        }
    }

    fn compress(
        t: &UiTree,
        req: &SnapshotRequest,
        refs: &mut RefTable,
    ) -> (String, Vec<ElementKey>) {
        let mut c = Compressor::new(req, 1, Instant::now());
        c.add_tree(t, refs);
        let released = c.unreferenced_keys(std::slice::from_ref(t));
        (c.text, released)
    }

    fn save_as_dialog() -> UiTree {
        let mut b = B { next: 0 };
        let label = b.node(ControlRole::Text, "File name:", vec![]);
        let edit = b.node(ControlRole::Edit, "File name:", vec![]);
        let save_text = b.node(ControlRole::Text, "Save", vec![]);
        let save = b.node(ControlRole::Button, "Save", vec![save_text]);
        let cancel = b.node(ControlRole::Button, "Cancel", vec![]);
        let layout = b.node(ControlRole::Pane, "", vec![label, edit, save, cancel]);
        let scroll = b.node(ControlRole::ScrollBar, "Vertical", vec![]);
        let empty_named = b.node(ControlRole::Pane, "Decoration", vec![]);
        let window = b.node(
            ControlRole::Dialog,
            "Save As",
            vec![layout, scroll, empty_named],
        );
        tree(window)
    }

    #[test]
    fn compact_tree_matches_spec_shape() {
        let t = save_as_dialog();
        let mut refs = RefTable::default();
        let (text, _) = compress(&t, &SnapshotRequest::default(), &mut refs);
        assert_eq!(
            text,
            "DIALOG \"Save As\" [e1]\n\
             \x20 TEXT \"File name:\" [e2]\n\
             \x20 EDIT \"File name:\" [e3]\n\
             \x20 BUTTON \"Save\" [e4]\n\
             \x20 BUTTON \"Cancel\" [e5]\n"
        );
    }

    #[test]
    fn unreferenced_slots_are_released() {
        let t = save_as_dialog();
        let mut refs = RefTable::default();
        let (_, released) = compress(&t, &SnapshotRequest::default(), &mut refs);
        // layout pane, "Save" text under the button, scrollbar, empty named pane.
        assert_eq!(released.len(), 4);
        assert_eq!(refs.len(), 5);
    }

    #[test]
    fn refs_are_stable_across_snapshots() {
        let t = save_as_dialog();
        let mut refs = RefTable::default();
        let (first, _) = compress(&t, &SnapshotRequest::default(), &mut refs);
        let (second, _) = compress(&t, &SnapshotRequest::default(), &mut refs);
        assert_eq!(first, second);
    }

    #[test]
    fn sensitive_values_are_redacted_and_states_rendered() {
        let mut b = B { next: 0 };
        let mut pw = b.node(ControlRole::Edit, "Password", vec![]);
        pw.props.is_password = true;
        pw.props.value = Some("hunter2".into());
        let mut user = b.node(ControlRole::Edit, "User", vec![]);
        user.props.value = Some("amogh".into());
        let mut check = b.node(ControlRole::CheckBox, "Remember me", vec![]);
        check.props.toggle_state = Some(ToggleState::On);
        let mut ok = b.node(ControlRole::Button, "OK", vec![]);
        ok.props.enabled = false;
        let t = tree(b.node(ControlRole::Window, "Sign in", vec![pw, user, check, ok]));
        let mut refs = RefTable::default();
        let (text, _) = compress(&t, &SnapshotRequest::default(), &mut refs);
        assert!(!text.contains("hunter2"));
        assert!(text.contains("EDIT \"Password\" value=\"[REDACTED]\" sensitive=true [e2]"));
        assert!(text.contains("EDIT \"User\" value=\"amogh\" [e3]"));
        assert!(text.contains("CHECKBOX \"Remember me\" checked [e4]"));
        assert!(text.contains("BUTTON \"OK\" disabled [e5]"));
    }

    #[test]
    fn huge_lists_are_truncated_with_counts() {
        let mut b = B { next: 0 };
        let items: Vec<UiNode> = (0..384)
            .map(|i| b.node(ControlRole::ListItem, &format!("file{i}.txt"), vec![]))
            .collect();
        let list = b.node(ControlRole::List, "Files", items);
        let t = tree(b.node(ControlRole::Window, "Explorer", vec![list]));
        let mut refs = RefTable::default();
        let (text, _) = compress(&t, &SnapshotRequest::default(), &mut refs);
        assert!(
            text.contains("LIST \"Files\" [e2] children=384 showing=20"),
            "{text}"
        );
        assert_eq!(text.lines().count(), 22);
    }

    #[test]
    fn grid_cells_fold_into_rows() {
        let mut b = B { next: 0 };
        let mut cell = |name: &str, value: &str| {
            let mut c = b.node(ControlRole::Edit, name, vec![]);
            c.props.patterns = vec![UiPattern::Value, UiPattern::GridItem];
            c.props.value = Some(value.into());
            c
        };
        let cells = vec![
            cell("Name", "a.txt"),
            cell("Date modified", "\u{200E}9/\u{200E}30/2026"),
            cell("Size", ""),
        ];
        let row = b.node(ControlRole::ListItem, "a.txt", cells);
        let list = b.node(ControlRole::List, "Items", vec![row]);
        let t = tree(b.node(ControlRole::Window, "E", vec![list]));
        let mut refs = RefTable::default();
        let (text, released) = compress(&t, &SnapshotRequest::default(), &mut refs);
        assert_eq!(
            text,
            "WINDOW \"E\" [e1]\n  LIST \"Items\" [e2]\n    LISTITEM \"a.txt\" [e3] Date modified=\"9/30/2026\"\n"
        );
        assert_eq!(released.len(), 3, "folded cells hold no slots");
    }

    #[test]
    fn node_budget_truncates() {
        let mut b = B { next: 0 };
        let buttons: Vec<UiNode> = (0..50)
            .map(|i| b.node(ControlRole::Button, &format!("b{i}"), vec![]))
            .collect();
        let t = tree(b.node(ControlRole::Window, "W", buttons));
        let req = SnapshotRequest {
            max_nodes: 10,
            ..Default::default()
        };
        let mut refs = RefTable::default();
        let mut c = Compressor::new(&req, 1, Instant::now());
        c.add_tree(&t, &mut refs);
        assert_eq!(c.emitted, 10);
        assert!(c.truncated);
    }

    #[test]
    fn offscreen_and_zero_size_are_omitted_unless_requested() {
        let mut b = B { next: 0 };
        let mut hidden = b.node(ControlRole::Button, "Hidden", vec![]);
        hidden.props.offscreen = true;
        let mut zero = b.node(ControlRole::Button, "Zero", vec![]);
        zero.props.bounds = None;
        let t = tree(b.node(ControlRole::Window, "W", vec![hidden, zero]));
        let mut refs = RefTable::default();
        let (text, _) = compress(&t, &SnapshotRequest::default(), &mut refs);
        assert_eq!(text, "WINDOW \"W\" [e1]\n");
        let req = SnapshotRequest {
            include_offscreen: true,
            ..Default::default()
        };
        let (text, _) = compress(&t, &req, &mut refs);
        assert!(text.contains("BUTTON \"Hidden\" offscreen"));
        assert!(text.contains("BUTTON \"Zero\""));
    }

    #[test]
    fn structured_nodes_carry_paths() {
        let t = save_as_dialog();
        let req = SnapshotRequest {
            structured: true,
            ..Default::default()
        };
        let mut refs = RefTable::default();
        let mut c = Compressor::new(&req, 1, Instant::now());
        c.add_tree(&t, &mut refs);
        let root = &c.nodes[0];
        assert_eq!(root.children.len(), 4);
        assert_eq!(
            root.children[2].element.path,
            "Dialog \"Save As\" > Button \"Save\""
        );
    }
}
