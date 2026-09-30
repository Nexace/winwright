//! Pure view models and text for the Inspector (tested without a window).

use winwright_contracts::element::{ElementDetails, ElementInfo};
use winwright_contracts::snapshot::SnapshotNode;

/// One tree row as the accessible item text: `Button "Save" [e12]`, with an id when there is
/// no name, plus state flags. The row is painted from [`RowView`].
pub fn tree_label(e: &ElementInfo) -> String {
    let mut s = e.role.as_str().to_owned();
    if !e.name.is_empty() {
        let name: String = e.name.chars().take(80).collect();
        s.push_str(&format!(" {name:?}"));
    } else if !e.automation_id.is_empty() {
        s.push_str(&format!(" id={:?}", e.automation_id));
    }
    s.push_str(&format!(" [{}]", e.reference));
    if !e.enabled {
        s.push_str(" disabled");
    }
    if !e.visible {
        s.push_str(" offscreen");
    }
    s
}

/// What a painted tree row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowView {
    pub role: String,
    pub name: String,
    pub automation_id: String,
    pub reference: String,
    pub enabled: bool,
    pub visible: bool,
    pub has_children: bool,
}

impl RowView {
    pub fn of(node: &SnapshotNode) -> Self {
        let e = &node.element;
        Self {
            role: e.role.as_str().to_owned(),
            name: e
                .name
                .chars()
                .take(120)
                .collect::<String>()
                .replace(['\r', '\n'], " "),
            automation_id: e.automation_id.clone(),
            reference: e.reference.clone(),
            enabled: e.enabled,
            visible: e.visible,
            has_children: !node.children.is_empty(),
        }
    }

    /// Right-hand tags: `offscreen · disabled`.
    pub fn tags(&self) -> String {
        let mut tags = Vec::new();
        if !self.visible {
            tags.push("offscreen");
        }
        if !self.enabled {
            tags.push("disabled");
        }
        tags.join(" \u{00B7} ")
    }
}

/// Whether `e` matches a lowercase filter (role, name, AutomationId, class, or ref).
pub fn matches(e: &ElementInfo, needle: &str) -> bool {
    needle.is_empty()
        || [
            e.role.as_str(),
            e.name.as_str(),
            e.automation_id.as_str(),
            e.class_name.as_str(),
            e.reference.as_str(),
        ]
        .iter()
        .any(|field| field.to_lowercase().contains(needle))
}

/// `nodes` pruned to the matches and their ancestors; `None` when nothing matches.
pub fn filter_nodes(nodes: &[SnapshotNode], needle: &str) -> Vec<SnapshotNode> {
    nodes
        .iter()
        .filter_map(|n| {
            let children = filter_nodes(&n.children, needle);
            (matches(&n.element, needle) || !children.is_empty()).then(|| SnapshotNode {
                element: n.element.clone(),
                children,
                ..n.clone()
            })
        })
        .collect()
}

pub fn count_nodes(nodes: &[SnapshotNode]) -> usize {
    nodes.iter().map(|n| 1 + count_nodes(&n.children)).sum()
}

/// A locator the CLI/MCP accept (`desktop_click` / `desktop_find` arguments).
pub fn locator_json(d: &ElementDetails, window: Option<&str>) -> String {
    locator_with(d, window, ":", ",")
}

/// The same locator, spaced for reading: `{"role": "Button", "name": "Save"}`.
pub fn locator_display(d: &ElementDetails, window: Option<&str>) -> String {
    locator_with(d, window, ": ", ", ")
}

/// JSON with keys in reading order (role, name, automationId, window); serde_json's map
/// would sort them alphabetically.
fn locator_with(d: &ElementDetails, window: Option<&str>, colon: &str, comma: &str) -> String {
    let e = &d.element;
    let mut fields = vec![("role", e.role.as_str())];
    if !e.name.is_empty() {
        fields.push(("name", e.name.as_str()));
    }
    if !e.automation_id.is_empty() {
        fields.push(("automationId", e.automation_id.as_str()));
    }
    if let Some(w) = window {
        fields.push(("window", w));
    }
    let quote = |s: &str| serde_json::Value::String(s.to_owned()).to_string();
    let body: Vec<String> = fields
        .into_iter()
        .map(|(k, v)| format!("{}{colon}{}", quote(k), quote(v)))
        .collect();
    format!("{{{}}}", body.join(comma))
}

/// Everything the details panel shows for one element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailView {
    pub role: String,
    pub title: String,
    pub subtitle: String,
    /// `(label, on)` state pills.
    pub states: Vec<(String, bool)>,
    pub sensitive: bool,
    pub sections: Vec<(String, Vec<(String, String)>)>,
    pub patterns: Vec<String>,
    pub locator: String,
}

pub fn detail_view(d: &ElementDetails, locator: String) -> DetailView {
    let e = &d.element;
    let title = if e.name.is_empty() {
        "(no name)".to_owned()
    } else {
        e.name.clone()
    };
    let mut subtitle = vec![e.reference.clone()];
    if !e.framework.is_empty() {
        subtitle.push(e.framework.clone());
    }
    subtitle.push(if d.process_name.is_empty() {
        format!("pid {}", d.process_id)
    } else {
        format!("{} (pid {})", d.process_name, d.process_id)
    });
    let row = |k: &str, v: String| (!v.is_empty()).then(|| (k.to_owned(), v));
    let identity: Vec<_> = [
        row("AutomationId", e.automation_id.clone()),
        row("Class", e.class_name.clone()),
        row(
            "Control type",
            format!("{} ({})", e.role.as_str(), d.control_type_id),
        ),
        row(
            "Runtime id",
            d.runtime_id
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join("."),
        ),
        row(
            "Window handle",
            d.native_window_handle
                .map(|h| format!("0x{h:X}"))
                .unwrap_or_default(),
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    let placement: Vec<_> = [
        row(
            "Bounds",
            e.bounds
                .map(|b| {
                    format!(
                        "{} \u{00D7} {} at ({}, {})",
                        b.width(),
                        b.height(),
                        b.left,
                        b.top
                    )
                })
                .unwrap_or_default(),
        ),
        row("Path", e.path.replace(" > ", "  \u{203A}  ")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let value = if e.sensitive {
        "hidden (sensitive field, never read)".to_owned()
    } else {
        e.value.clone().unwrap_or_default()
    };
    let state: Vec<_> = [
        row("Value", value),
        row(
            "Toggle",
            e.toggle_state.map(|t| format!("{t:?}")).unwrap_or_default(),
        ),
        row(
            "Expand",
            e.expand_state.map(|t| format!("{t:?}")).unwrap_or_default(),
        ),
        row(
            "Selected",
            e.selected
                .map(|t| if t { "yes" } else { "no" }.to_owned())
                .unwrap_or_default(),
        ),
        row("Help text", d.help_text.clone()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let sections = [
        ("Identity", identity),
        ("Placement", placement),
        ("State", state),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .map(|(t, rows)| (t.to_owned(), rows))
    .collect();
    DetailView {
        role: e.role.as_str().to_owned(),
        title,
        subtitle: subtitle.join("  \u{00B7}  "),
        states: vec![
            ("Enabled".into(), e.enabled),
            ("Visible".into(), e.visible),
            ("Focused".into(), e.focused),
            ("Keyboard focusable".into(), d.keyboard_focusable),
        ],
        sensitive: e.sensitive,
        sections,
        patterns: e.patterns.iter().map(|p| format!("{p:?}")).collect(),
        locator,
    }
}

/// All properties as plain text ("Copy all properties").
pub fn details(d: &ElementDetails, locator: &str) -> String {
    let e = &d.element;
    let mut out = String::new();
    let mut row = |k: &str, v: String| {
        if !v.is_empty() {
            out.push_str(&format!("{k:<16} {v}\n"));
        }
    };
    row(
        "Element",
        format!("{} {:?}  [{}]", e.role.as_str(), e.name, e.reference),
    );
    row("Path", e.path.clone());
    row("AutomationId", e.automation_id.clone());
    row("ClassName", e.class_name.clone());
    row("Framework", e.framework.clone());
    row(
        "Process",
        format!("{} (pid {})", d.process_name, d.process_id),
    );
    row(
        "Bounds",
        e.bounds
            .map(|b| {
                format!(
                    "[{}, {}, {}, {}]  {}x{}",
                    b.left,
                    b.top,
                    b.right,
                    b.bottom,
                    b.width(),
                    b.height()
                )
            })
            .unwrap_or_default(),
    );
    row(
        "State",
        format!(
            "enabled={} visible={} focused={} keyboard-focusable={}",
            e.enabled, e.visible, e.focused, d.keyboard_focusable
        ),
    );
    row(
        "Patterns",
        e.patterns
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    row("Value", e.value.clone().unwrap_or_default());
    if e.sensitive {
        row("Sensitive", "yes (value is never read)".into());
    }
    row(
        "Toggle",
        e.toggle_state.map(|t| format!("{t:?}")).unwrap_or_default(),
    );
    row(
        "Expand",
        e.expand_state.map(|t| format!("{t:?}")).unwrap_or_default(),
    );
    row(
        "Selected",
        e.selected.map(|t| t.to_string()).unwrap_or_default(),
    );
    row("ControlType", d.control_type_id.to_string());
    row("RuntimeId", format!("{:?}", d.runtime_id));
    row("HelpText", d.help_text.clone());
    row("Locator", locator.to_owned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::element::{ControlRole, UiPattern};
    use winwright_contracts::geometry::PhysicalRect;

    fn element() -> ElementInfo {
        ElementInfo {
            reference: "e12".into(),
            role: ControlRole::Button,
            name: "Save".into(),
            automation_id: "btnSave".into(),
            class_name: "Button".into(),
            framework: "Win32".into(),
            enabled: true,
            visible: true,
            focused: false,
            bounds: Some(PhysicalRect::new(10, 20, 110, 50)),
            patterns: vec![UiPattern::Invoke],
            value: None,
            sensitive: false,
            toggle_state: None,
            expand_state: None,
            selected: None,
            path: "Window \"Notepad\" > Button \"Save\"".into(),
        }
    }

    fn details_of(e: ElementInfo) -> ElementDetails {
        ElementDetails {
            element: e,
            control_type_id: 50000,
            process_id: 42,
            process_name: "notepad.exe".into(),
            native_window_handle: None,
            runtime_id: vec![42, 7],
            keyboard_focusable: true,
            offscreen: false,
            help_text: String::new(),
            ancestors: vec![],
        }
    }

    fn node(e: ElementInfo, children: Vec<SnapshotNode>) -> SnapshotNode {
        SnapshotNode {
            element: e,
            cells: vec![],
            children,
            child_count: None,
        }
    }

    #[test]
    fn labels_and_locators() {
        let mut e = element();
        assert_eq!(tree_label(&e), "Button \"Save\" [e12]");
        e.name.clear();
        e.enabled = false;
        assert_eq!(tree_label(&e), "Button id=\"btnSave\" [e12] disabled");
        let json = locator_json(&details_of(element()), Some("Untitled - Notepad"));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["role"], "Button");
        assert_eq!(v["name"], "Save");
        assert_eq!(v["automationId"], "btnSave");
        assert_eq!(v["window"], "Untitled - Notepad");
        let shown = locator_display(&details_of(element()), None);
        assert_eq!(
            shown,
            r#"{"role": "Button", "name": "Save", "automationId": "btnSave"}"#
        );
        let parsed: serde_json::Value = serde_json::from_str(&shown).unwrap();
        assert_eq!(
            parsed["name"], "Save",
            "the display form is still valid JSON"
        );
    }

    #[test]
    fn details_list_the_inspector_fields() {
        let text = details(&details_of(element()), "{}");
        for field in [
            "Element",
            "Path",
            "AutomationId",
            "Bounds",
            "Patterns",
            "RuntimeId",
            "Process",
        ] {
            assert!(text.contains(field), "{field} missing:\n{text}");
        }
        assert!(text.contains("100x30"));
    }

    #[test]
    fn detail_view_groups_properties() {
        let v = detail_view(&details_of(element()), "{}".into());
        assert_eq!(v.role, "Button");
        assert_eq!(v.title, "Save");
        assert!(v.subtitle.contains("e12") && v.subtitle.contains("notepad.exe (pid 42)"));
        let titles: Vec<_> = v.sections.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(
            titles,
            ["Identity", "Placement"],
            "empty State section is dropped"
        );
        let placement = &v.sections[1].1;
        assert!(
            placement
                .iter()
                .any(|(k, v)| k == "Bounds" && v == "100 \u{00D7} 30 at (10, 20)")
        );
        assert!(
            placement
                .iter()
                .any(|(k, v)| k == "Path" && v.contains('\u{203A}'))
        );
        assert_eq!(v.patterns, ["Invoke"]);
        assert!(v.states.contains(&("Keyboard focusable".into(), true)));

        let mut secret = element();
        secret.sensitive = true;
        secret.value = Some("[REDACTED]".into());
        let v = detail_view(&details_of(secret), String::new());
        assert!(v.sensitive);
        let state = &v.sections.iter().find(|s| s.0 == "State").unwrap().1;
        assert!(
            state[0].1.starts_with("hidden"),
            "never shows a secret value"
        );
    }

    #[test]
    fn filtering_keeps_matches_and_their_ancestors() {
        let mut save = element();
        save.reference = "e2".into();
        let mut open = element();
        open.name = "Open".into();
        open.automation_id = "btnOpen".into();
        open.reference = "e3".into();
        let mut root = element();
        root.role = ControlRole::Window;
        root.name = "Notepad".into();
        root.automation_id.clear();
        root.reference = "e1".into();
        let tree = vec![node(root, vec![node(save, vec![]), node(open, vec![])])];
        assert_eq!(count_nodes(&tree), 3);
        let hit = filter_nodes(&tree, "open");
        assert_eq!(count_nodes(&hit), 2, "match plus its window");
        assert_eq!(hit[0].children[0].element.name, "Open");
        assert!(filter_nodes(&tree, "nothing-like-this").is_empty());
        assert_eq!(count_nodes(&filter_nodes(&tree, "")), 3);
        assert!(matches(&tree[0].element, "window"), "role matches");
        let row = RowView::of(&tree[0]);
        assert!(row.has_children);
        assert!(row.tags().is_empty());
    }
}
