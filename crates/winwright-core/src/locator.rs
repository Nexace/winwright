//! Locator engine (spec §10, §65): compile an [`ElementLocator`] once, then match it against
//! captured UIA trees. All supplied predicates must match; ranking only orders matches and
//! never silently picks between equally strong ones.

use regex::{Regex, RegexBuilder};
use winwright_contracts::backend::UiNode;
use winwright_contracts::element::ControlRole;
use winwright_contracts::locator::{ElementLocator, MatchMode};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_security::{is_sensitive, redacted_value};

const REGEX_SIZE_LIMIT: usize = 1 << 20;

fn normalize(s: &str, case_sensitive: bool) -> String {
    let trimmed = s.trim();
    if case_sensitive {
        trimmed.to_owned()
    } else {
        trimmed.to_lowercase()
    }
}

/// Labels are compared without their trailing colon: `get_by_label("File name")` finds
/// `EDIT "File name:"`.
fn strip_label(s: &str) -> &str {
    s.trim().trim_end_matches(':').trim_end()
}

enum MatcherKind {
    Exact(String),
    Contains(String),
    Regex(Regex),
}

pub struct Matcher {
    kind: MatcherKind,
    case_sensitive: bool,
}

/// How well a string matched: 0 = no match, 1 = partial (contains/regex), 2 = equal.
type Strength = u32;

impl Matcher {
    pub fn new(pattern: &str, mode: MatchMode, case_sensitive: bool) -> WinwrightResult<Self> {
        let kind = match mode {
            MatchMode::Exact => MatcherKind::Exact(normalize(pattern, case_sensitive)),
            MatchMode::Contains => MatcherKind::Contains(normalize(pattern, case_sensitive)),
            MatchMode::Regex => MatcherKind::Regex(
                RegexBuilder::new(pattern)
                    .case_insensitive(!case_sensitive)
                    .size_limit(REGEX_SIZE_LIMIT)
                    .dfa_size_limit(REGEX_SIZE_LIMIT)
                    .build()
                    .map_err(|e| {
                        WinwrightError::invalid(format!("invalid regex {pattern:?}: {e}"))
                    })?,
            ),
        };
        Ok(Self {
            kind,
            case_sensitive,
        })
    }

    pub fn strength(&self, value: &str) -> Strength {
        match &self.kind {
            MatcherKind::Exact(p) => u32::from(normalize(value, self.case_sensitive) == *p) * 2,
            MatcherKind::Contains(p) => {
                let v = normalize(value, self.case_sensitive);
                if v == *p {
                    2
                } else if !p.is_empty() && v.contains(p.as_str()) {
                    1
                } else {
                    0
                }
            }
            MatcherKind::Regex(r) => u32::from(r.is_match(value)),
        }
    }
}

fn canonical_framework(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "winforms" | "winform" | "windowsforms" => "winform".into(),
        "uwp" | "xaml" | "winui" => "xaml".into(),
        "chromium" | "chrome" | "electron" | "edge" => "chrome".into(),
        other => other.to_owned(),
    }
}

pub struct CompiledLocator {
    role: Option<ControlRole>,
    name: Option<Matcher>,
    text: Option<Matcher>,
    automation_id: Option<Matcher>,
    class_name: Option<String>,
    framework: Option<String>,
    label: Option<Matcher>,
    ancestor: Option<Box<CompiledLocator>>,
    visible_only: bool,
    pub nth: Option<usize>,
    pub description: String,
}

fn describe(loc: &ElementLocator) -> String {
    let mut parts = Vec::new();
    if let Some(r) = &loc.role {
        parts.push(r.clone());
    }
    let mode = match loc.match_mode {
        MatchMode::Exact => "=",
        MatchMode::Contains => "~",
        MatchMode::Regex => "=~",
    };
    for (key, value) in [
        ("name", &loc.name),
        ("text", &loc.text),
        ("automationId", &loc.automation_id),
        ("label", &loc.label),
    ] {
        if let Some(v) = value {
            parts.push(format!("{key}{mode}{v:?}"));
        }
    }
    if let Some(c) = &loc.class_name {
        parts.push(format!("class={c:?}"));
    }
    if let Some(f) = &loc.framework_id {
        parts.push(format!("framework={f}"));
    }
    if let Some(n) = loc.nth {
        parts.push(format!("nth={n}"));
    }
    let own = parts.join(" ");
    match &loc.ancestor {
        Some(a) => format!("{} >> {own}", describe(a)),
        None => own,
    }
}

pub fn compile(loc: &ElementLocator) -> WinwrightResult<CompiledLocator> {
    loc.validate().map_err(WinwrightError::invalid)?;
    compile_inner(loc)
}

fn compile_inner(loc: &ElementLocator) -> WinwrightResult<CompiledLocator> {
    let matcher = |value: &Option<String>| {
        value
            .as_deref()
            .map(|v| Matcher::new(v, loc.match_mode, loc.case_sensitive))
            .transpose()
    };
    let role = loc
        .role
        .as_deref()
        .map(|r| {
            ControlRole::parse(r).ok_or_else(|| {
                WinwrightError::invalid(format!(
                    "unknown role {r:?}; use a UIA control type such as Button, Edit, CheckBox, \
                     ComboBox, List, ListItem, MenuItem, Tab, TabItem, Tree, TreeItem, Text, \
                     Window, Dialog"
                ))
            })
        })
        .transpose()?;
    let label = loc
        .label
        .as_deref()
        .map(|v| {
            let pattern = if loc.match_mode == MatchMode::Regex {
                v
            } else {
                strip_label(v)
            };
            Matcher::new(pattern, loc.match_mode, loc.case_sensitive)
        })
        .transpose()?;
    Ok(CompiledLocator {
        role,
        name: matcher(&loc.name)?,
        text: matcher(&loc.text)?,
        automation_id: matcher(&loc.automation_id)?,
        class_name: loc
            .class_name
            .as_deref()
            .map(|c| c.trim().to_ascii_lowercase()),
        framework: loc.framework_id.as_deref().map(canonical_framework),
        label,
        ancestor: loc
            .ancestor
            .as_deref()
            .map(compile_inner)
            .transpose()?
            .map(Box::new),
        visible_only: loc.visible_only,
        nth: loc.nth,
        description: describe(loc),
    })
}

/// Roles whose accessible label usually lives in a separate text element.
fn is_labelable(role: ControlRole) -> bool {
    matches!(
        role,
        ControlRole::Edit
            | ControlRole::ComboBox
            | ControlRole::List
            | ControlRole::Spinner
            | ControlRole::Slider
            | ControlRole::Document
            | ControlRole::DataGrid
            | ControlRole::Tree
            | ControlRole::Custom
            | ControlRole::CheckBox
            | ControlRole::RadioButton
    )
}

/// Label inference order: LabeledBy, own name (Win32/XAML derive it from the label),
/// nearest preceding text sibling, then an enclosing named group.
fn label_strength(m: &Matcher, node: &UiNode, parent: Option<&UiNode>, index: usize) -> Strength {
    let p = &node.props;
    let mut best = p
        .labeled_by
        .as_deref()
        .map_or(0, |l| m.strength(strip_label(l)));
    if !is_labelable(p.role) {
        return best;
    }
    best = best.max(m.strength(strip_label(&p.name)));
    if let Some(parent) = parent {
        // The label is the closest preceding text, but only if no other control sits
        // between it and this one (otherwise it labels that control).
        for sibling in parent.children[..index].iter().rev() {
            let s = &sibling.props;
            if s.role == ControlRole::Text && !s.name.trim().is_empty() {
                best = best.max(m.strength(strip_label(&s.name)));
                break;
            }
            if s.role.is_interactive()
                || is_labelable(s.role)
                || s.patterns.iter().any(|x| x.is_actionable())
            {
                break;
            }
        }
        if parent.props.role == ControlRole::Group {
            best = best.max(m.strength(strip_label(&parent.props.name)));
        }
    }
    best
}

fn is_visible(node: &UiNode) -> bool {
    !node.props.offscreen && node.props.bounds.is_some()
}

impl CompiledLocator {
    /// Score of `node` under this locator, or `None` if any predicate fails.
    /// `ancestors` is the chain from the capture root down to the node's parent.
    fn score(&self, node: &UiNode, ancestors: &[&UiNode], index: usize) -> Option<u32> {
        let p = &node.props;
        if self.visible_only && !is_visible(node) {
            return None;
        }
        let mut score = 0;
        if let Some(role) = self.role {
            if !role.matches(p.role) {
                return None;
            }
            score += if role == p.role { 4 } else { 3 };
        }
        if let Some(m) = &self.automation_id {
            let s = m.strength(&p.automation_id);
            if s == 0 {
                return None;
            }
            score += s * 4;
        }
        if let Some(m) = &self.name {
            let s = m.strength(&p.name);
            if s == 0 {
                return None;
            }
            score += s * 2;
        }
        if let Some(m) = &self.text {
            let value = if is_sensitive(p) {
                None
            } else {
                redacted_value(p)
            };
            let s = m.strength(&p.name).max(value.map_or(0, |v| m.strength(&v)));
            if s == 0 {
                return None;
            }
            score += s;
        }
        if let Some(c) = &self.class_name {
            if !p.class_name.eq_ignore_ascii_case(c) {
                return None;
            }
            score += 1;
        }
        if let Some(f) = &self.framework {
            if canonical_framework(&p.framework_id) != *f {
                return None;
            }
            score += 1;
        }
        if let Some(m) = &self.label {
            let s = label_strength(m, node, ancestors.last().copied(), index);
            if s == 0 {
                return None;
            }
            score += s * 2;
        }
        if let Some(anc) = &self.ancestor {
            let found = (0..ancestors.len()).rev().any(|i| {
                let parent_index = if i == 0 {
                    0
                } else {
                    ancestors[i - 1]
                        .children
                        .iter()
                        .position(|c| std::ptr::eq(c, ancestors[i]))
                        .unwrap_or(0)
                };
                anc.score(ancestors[i], &ancestors[..i], parent_index)
                    .is_some()
            });
            if !found {
                return None;
            }
            score += 1;
        }
        Some(score)
    }
}

pub struct Match<'a> {
    pub node: &'a UiNode,
    /// Capture root first, parent last.
    pub ancestors: Vec<&'a UiNode>,
    pub score: u32,
    /// Document order across all searched roots.
    pub order: usize,
}

/// Every node under `roots` (roots included) that satisfies the locator, in document order.
pub fn find_matches<'a>(loc: &CompiledLocator, roots: &[&'a UiNode]) -> Vec<Match<'a>> {
    fn walk<'a>(
        loc: &CompiledLocator,
        node: &'a UiNode,
        index: usize,
        stack: &mut Vec<&'a UiNode>,
        order: &mut usize,
        out: &mut Vec<Match<'a>>,
    ) {
        if let Some(score) = loc.score(node, stack, index) {
            out.push(Match {
                node,
                ancestors: stack.clone(),
                score,
                order: *order,
            });
        }
        *order += 1;
        stack.push(node);
        for (i, child) in node.children.iter().enumerate() {
            walk(loc, child, i, stack, order, out);
        }
        stack.pop();
    }
    let mut out = Vec::new();
    let mut order = 0;
    for root in roots {
        walk(loc, root, 0, &mut Vec::new(), &mut order, &mut out);
    }
    out
}

/// Picks the single target of an action. `nth` selects by document order; otherwise the
/// strictly strongest match wins and ties are reported as ambiguity (indices into `matches`).
pub enum Resolution {
    One(usize),
    NotFound,
    Ambiguous(Vec<usize>),
}

pub fn resolve_one(matches: &[Match], nth: Option<usize>) -> Resolution {
    if let Some(n) = nth {
        return if n < matches.len() {
            Resolution::One(n)
        } else {
            Resolution::NotFound
        };
    }
    let Some(best) = matches.iter().map(|m| m.score).max() else {
        return Resolution::NotFound;
    };
    let top: Vec<usize> = (0..matches.len())
        .filter(|&i| matches[i].score == best)
        .collect();
    if top.len() == 1 {
        Resolution::One(top[0])
    } else {
        Resolution::Ambiguous(top)
    }
}

/// Matches ordered for display: strongest first, then document order.
pub fn ranked(matches: &mut [Match]) {
    matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.order.cmp(&b.order)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::backend::{ElementKey, UiProps};
    use winwright_contracts::element::UiPattern;
    use winwright_contracts::geometry::PhysicalRect;

    fn node(role: ControlRole, name: &str, children: Vec<UiNode>) -> UiNode {
        UiNode {
            key: ElementKey {
                worker_epoch: 1,
                slot: 0,
            },
            props: UiProps {
                role,
                name: name.into(),
                bounds: Some(PhysicalRect::new(0, 0, 10, 10)),
                enabled: true,
                framework_id: "Win32".into(),
                class_name: match role {
                    ControlRole::Button => "Button".into(),
                    ControlRole::Edit => "Edit".into(),
                    _ => String::new(),
                },
                ..Default::default()
            },
            children_total: children.len() as u32,
            children,
        }
    }

    fn with_id(mut n: UiNode, id: &str) -> UiNode {
        n.props.automation_id = id.into();
        n
    }

    fn fixture() -> UiNode {
        let form = node(
            ControlRole::Group,
            "Login",
            vec![
                node(ControlRole::Text, "User:", vec![]),
                with_id(node(ControlRole::Edit, "", vec![]), "user"),
                node(ControlRole::Text, "Password:", vec![]),
                {
                    let mut pw = with_id(node(ControlRole::Edit, "", vec![]), "pw");
                    pw.props.is_password = true;
                    pw.props.patterns = vec![UiPattern::Value];
                    pw.props.value = Some("hunter2".into());
                    pw
                },
            ],
        );
        let toolbar = node(
            ControlRole::ToolBar,
            "Tools",
            vec![
                with_id(node(ControlRole::Button, "Save", vec![]), "btnSave"),
                node(ControlRole::Button, "Save As", vec![]),
                {
                    let mut hidden = node(ControlRole::Button, "Save", vec![]);
                    hidden.props.offscreen = true;
                    hidden
                },
            ],
        );
        let file_name = {
            let mut e = node(ControlRole::Edit, "File name:", vec![]);
            e.props.patterns = vec![UiPattern::Value];
            e.props.value = Some("report.docx".into());
            e
        };
        node(
            ControlRole::Dialog,
            "Save As",
            vec![
                form,
                toolbar,
                file_name,
                node(ControlRole::Text, "Export complete", vec![]),
            ],
        )
    }

    fn loc(f: impl FnOnce(&mut ElementLocator)) -> CompiledLocator {
        let mut l = ElementLocator::default();
        f(&mut l);
        compile(&l).unwrap()
    }

    fn names(ms: &[Match]) -> Vec<String> {
        ms.iter()
            .map(|m| format!("{:?} {}", m.node.props.role, m.node.props.name))
            .collect()
    }

    #[test]
    fn role_and_exact_name() {
        let root = fixture();
        let l = loc(|l| {
            l.role = Some("button".into());
            l.name = Some("save".into());
        });
        let ms = find_matches(&l, &[&root]);
        assert_eq!(names(&ms), ["Button Save"], "offscreen Save is skipped");
    }

    #[test]
    fn visible_only_false_includes_offscreen() {
        let root = fixture();
        let l = loc(|l| {
            l.role = Some("Button".into());
            l.name = Some("Save".into());
            l.visible_only = false;
        });
        let ms = find_matches(&l, &[&root]);
        assert_eq!(ms.len(), 2);
        assert!(matches!(resolve_one(&ms, None), Resolution::Ambiguous(ref v) if v.len() == 2));
        assert!(matches!(resolve_one(&ms, Some(1)), Resolution::One(1)));
        assert!(matches!(resolve_one(&ms, Some(2)), Resolution::NotFound));
    }

    #[test]
    fn contains_ranks_exact_equality_higher() {
        let root = fixture();
        let l = loc(|l| {
            l.role = Some("Button".into());
            l.name = Some("Save".into());
            l.match_mode = MatchMode::Contains;
        });
        let mut ms = find_matches(&l, &[&root]);
        assert_eq!(ms.len(), 2);
        let Resolution::One(i) = resolve_one(&ms, None) else {
            panic!("exact equality should win")
        };
        assert_eq!(ms[i].node.props.name, "Save");
        ranked(&mut ms);
        assert_eq!(names(&ms), ["Button Save", "Button Save As"]);
    }

    #[test]
    fn window_role_matches_dialog_root() {
        let root = fixture();
        let l = loc(|l| {
            l.role = Some("Window".into());
            l.name = Some("Save As".into());
        });
        assert_eq!(names(&find_matches(&l, &[&root])), ["Dialog Save As"]);
    }

    #[test]
    fn automation_id_and_class() {
        let root = fixture();
        let l = loc(|l| {
            l.automation_id = Some("btnSave".into());
            l.class_name = Some("button".into());
        });
        assert_eq!(names(&find_matches(&l, &[&root])), ["Button Save"]);
    }

    #[test]
    fn label_inference_sources() {
        let root = fixture();
        // Preceding text sibling.
        let l = loc(|l| l.label = Some("User".into()));
        let ms = find_matches(&l, &[&root]);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].node.props.automation_id, "user");
        // Own name with trailing colon stripped (Win32 / spec example).
        let l = loc(|l| {
            l.role = Some("Edit".into());
            l.label = Some("File name".into());
        });
        assert_eq!(names(&find_matches(&l, &[&root])), ["Edit File name:"]);
        // LabeledBy.
        let mut root = fixture();
        root.children[2].props.labeled_by = Some("Target file".into());
        let l = loc(|l| l.label = Some("Target file".into()));
        assert_eq!(find_matches(&l, &[&root]).len(), 1);
        // Enclosing group applies to every input inside it.
        let l = loc(|l| {
            l.role = Some("Edit".into());
            l.label = Some("Login".into());
        });
        assert_eq!(find_matches(&l, &[&root]).len(), 2);
    }

    #[test]
    fn text_matches_name_or_value_but_never_secrets() {
        let root = fixture();
        let l = loc(|l| l.text = Some("Export complete".into()));
        assert_eq!(names(&find_matches(&l, &[&root])), ["Text Export complete"]);
        let l = loc(|l| l.text = Some("report.docx".into()));
        assert_eq!(names(&find_matches(&l, &[&root])), ["Edit File name:"]);
        let l = loc(|l| l.text = Some("hunter2".into()));
        assert!(
            find_matches(&l, &[&root]).is_empty(),
            "password values are not searchable"
        );
    }

    #[test]
    fn sibling_label_stops_at_the_next_control() {
        let root = node(
            ControlRole::Window,
            "W",
            vec![
                node(ControlRole::Text, "Name:", vec![]),
                with_id(node(ControlRole::Edit, "", vec![]), "a"),
                with_id(node(ControlRole::Edit, "", vec![]), "b"),
                node(ControlRole::CheckBox, "", vec![]),
            ],
        );
        let l = loc(|l| l.label = Some("Name".into()));
        let ms = find_matches(&l, &[&root]);
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].node.props.automation_id, "a");
    }

    #[test]
    fn ancestor_scoping() {
        let root = fixture();
        let l = loc(|l| {
            l.role = Some("Edit".into());
            l.ancestor = Some(Box::new(ElementLocator {
                role: Some("Group".into()),
                name: Some("Login".into()),
                ..Default::default()
            }));
        });
        assert_eq!(find_matches(&l, &[&root]).len(), 2);
        let l = loc(|l| {
            l.role = Some("Button".into());
            l.ancestor = Some(Box::new(ElementLocator {
                name: Some("Login".into()),
                ..Default::default()
            }));
        });
        assert!(find_matches(&l, &[&root]).is_empty());
    }

    #[test]
    fn regex_and_case_sensitivity() {
        let root = fixture();
        let l = loc(|l| {
            l.name = Some("^save( as)?$".into());
            l.match_mode = MatchMode::Regex;
        });
        assert_eq!(find_matches(&l, &[&root]).len(), 3, "dialog + two buttons");
        let l = loc(|l| {
            l.name = Some("save".into());
            l.case_sensitive = true;
        });
        assert!(find_matches(&l, &[&root]).is_empty());
    }

    #[test]
    fn framework_aliases() {
        let root = fixture();
        let l = loc(|l| {
            l.role = Some("Button".into());
            l.framework_id = Some("WIN32".into());
        });
        assert_eq!(find_matches(&l, &[&root]).len(), 2);
        let l = loc(|l| {
            l.role = Some("Button".into());
            l.framework_id = Some("WPF".into());
        });
        assert!(find_matches(&l, &[&root]).is_empty());
    }

    #[test]
    fn invalid_locators_are_typed_errors() {
        let bad_role = ElementLocator {
            role: Some("Buton".into()),
            ..Default::default()
        };
        assert_eq!(
            compile(&bad_role).err().unwrap().code().as_str(),
            "INVALID_REQUEST"
        );
        let bad_regex = ElementLocator {
            name: Some("(".into()),
            match_mode: MatchMode::Regex,
            ..Default::default()
        };
        assert_eq!(
            compile(&bad_regex).err().unwrap().code().as_str(),
            "INVALID_REQUEST"
        );
        assert!(
            compile(&ElementLocator::default()).is_err(),
            "no predicates"
        );
    }

    #[test]
    fn description_is_readable() {
        let l = loc(|l| {
            l.role = Some("Button".into());
            l.name = Some("Save".into());
            l.ancestor = Some(Box::new(ElementLocator {
                role: Some("Window".into()),
                name: Some("Save As".into()),
                ..Default::default()
            }));
        });
        assert_eq!(
            l.description,
            "Window name=\"Save As\" >> Button name=\"Save\""
        );
    }
}
