//! Pure text rendering for the Inspector (tested without a window).

use winwright_contracts::element::{ElementDetails, ElementInfo};

/// One tree row: `Button "Save" [e12]`, with an id when there is no name, plus state flags.
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

/// A locator the CLI/MCP accept (`desktop_click` / `desktop_find` arguments).
pub fn locator_json(d: &ElementDetails, window: Option<&str>) -> String {
    let e = &d.element;
    let mut obj = serde_json::Map::new();
    obj.insert("role".into(), e.role.as_str().into());
    if !e.name.is_empty() {
        obj.insert("name".into(), e.name.clone().into());
    }
    if !e.automation_id.is_empty() {
        obj.insert("automationId".into(), e.automation_id.clone().into());
    }
    if let Some(w) = window {
        obj.insert("window".into(), w.into());
    }
    serde_json::Value::Object(obj).to_string()
}

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
}
