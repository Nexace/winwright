use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::PhysicalRect;

/// LLM-facing role. Mirrors UIA ControlType, plus `Dialog` (a Window that UIA flags as a
/// dialog) and `Unknown` for control types newer than this list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ControlRole {
    AppBar,
    Button,
    Calendar,
    CheckBox,
    ComboBox,
    Custom,
    DataGrid,
    DataItem,
    Dialog,
    Document,
    Edit,
    Group,
    Header,
    HeaderItem,
    Image,
    Link,
    List,
    ListItem,
    Menu,
    MenuBar,
    MenuItem,
    Pane,
    ProgressBar,
    RadioButton,
    ScrollBar,
    SemanticZoom,
    Separator,
    Slider,
    Spinner,
    SplitButton,
    StatusBar,
    Tab,
    TabItem,
    Table,
    Text,
    Thumb,
    TitleBar,
    ToolBar,
    ToolTip,
    Tree,
    TreeItem,
    #[default]
    Unknown,
    Window,
}

/// UIA_ButtonControlTypeId … UIA_AppBarControlTypeId are the contiguous range 50000..=50040.
const UIA_CONTROL_TYPES: [ControlRole; 41] = [
    ControlRole::Button,
    ControlRole::Calendar,
    ControlRole::CheckBox,
    ControlRole::ComboBox,
    ControlRole::Edit,
    ControlRole::Link,
    ControlRole::Image,
    ControlRole::ListItem,
    ControlRole::List,
    ControlRole::Menu,
    ControlRole::MenuBar,
    ControlRole::MenuItem,
    ControlRole::ProgressBar,
    ControlRole::RadioButton,
    ControlRole::ScrollBar,
    ControlRole::Slider,
    ControlRole::Spinner,
    ControlRole::StatusBar,
    ControlRole::Tab,
    ControlRole::TabItem,
    ControlRole::Text,
    ControlRole::ToolBar,
    ControlRole::ToolTip,
    ControlRole::Tree,
    ControlRole::TreeItem,
    ControlRole::Custom,
    ControlRole::Group,
    ControlRole::Thumb,
    ControlRole::DataGrid,
    ControlRole::DataItem,
    ControlRole::Document,
    ControlRole::SplitButton,
    ControlRole::Window,
    ControlRole::Pane,
    ControlRole::Header,
    ControlRole::HeaderItem,
    ControlRole::Table,
    ControlRole::TitleBar,
    ControlRole::Separator,
    ControlRole::SemanticZoom,
    ControlRole::AppBar,
];

impl ControlRole {
    pub const ALL: [ControlRole; 43] = {
        let mut all = [ControlRole::Unknown; 43];
        let mut i = 0;
        while i < UIA_CONTROL_TYPES.len() {
            all[i] = UIA_CONTROL_TYPES[i];
            i += 1;
        }
        all[41] = ControlRole::Dialog;
        all
    };

    pub fn from_uia(control_type_id: i32, is_dialog: bool) -> Self {
        let role = control_type_id
            .checked_sub(50_000)
            .and_then(|i| usize::try_from(i).ok())
            .and_then(|i| UIA_CONTROL_TYPES.get(i).copied())
            .unwrap_or(Self::Unknown);
        if role == Self::Window && is_dialog {
            Self::Dialog
        } else {
            role
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppBar => "AppBar",
            Self::Button => "Button",
            Self::Calendar => "Calendar",
            Self::CheckBox => "CheckBox",
            Self::ComboBox => "ComboBox",
            Self::Custom => "Custom",
            Self::DataGrid => "DataGrid",
            Self::DataItem => "DataItem",
            Self::Dialog => "Dialog",
            Self::Document => "Document",
            Self::Edit => "Edit",
            Self::Group => "Group",
            Self::Header => "Header",
            Self::HeaderItem => "HeaderItem",
            Self::Image => "Image",
            Self::Link => "Link",
            Self::List => "List",
            Self::ListItem => "ListItem",
            Self::Menu => "Menu",
            Self::MenuBar => "MenuBar",
            Self::MenuItem => "MenuItem",
            Self::Pane => "Pane",
            Self::ProgressBar => "ProgressBar",
            Self::RadioButton => "RadioButton",
            Self::ScrollBar => "ScrollBar",
            Self::SemanticZoom => "SemanticZoom",
            Self::Separator => "Separator",
            Self::Slider => "Slider",
            Self::Spinner => "Spinner",
            Self::SplitButton => "SplitButton",
            Self::StatusBar => "StatusBar",
            Self::Tab => "Tab",
            Self::TabItem => "TabItem",
            Self::Table => "Table",
            Self::Text => "Text",
            Self::Thumb => "Thumb",
            Self::TitleBar => "TitleBar",
            Self::ToolBar => "ToolBar",
            Self::ToolTip => "ToolTip",
            Self::Tree => "Tree",
            Self::TreeItem => "TreeItem",
            Self::Unknown => "Unknown",
            Self::Window => "Window",
        }
    }

    /// Case-insensitive role parsing for locators; accepts `Hyperlink` as an alias of `Link`.
    pub fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("hyperlink") {
            return Some(Self::Link);
        }
        Self::ALL
            .into_iter()
            .find(|r| r.as_str().eq_ignore_ascii_case(value))
    }

    /// `Window` in a locator also matches dialogs; every other role matches only itself.
    pub fn matches(self, actual: ControlRole) -> bool {
        self == actual || (self == Self::Window && actual == Self::Dialog)
    }

    /// Roles the model can act on directly (spec §35 include list).
    pub fn is_interactive(self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::Calendar
                | Self::CheckBox
                | Self::ComboBox
                | Self::DataGrid
                | Self::DataItem
                | Self::Dialog
                | Self::Document
                | Self::Edit
                | Self::HeaderItem
                | Self::Link
                | Self::List
                | Self::ListItem
                | Self::Menu
                | Self::MenuBar
                | Self::MenuItem
                | Self::RadioButton
                | Self::Slider
                | Self::Spinner
                | Self::SplitButton
                | Self::Tab
                | Self::TabItem
                | Self::Table
                | Self::Tree
                | Self::TreeItem
                | Self::Window
        )
    }

    /// Upper-case tag used in the compact text snapshot (`BUTTON "Save" [e6]`).
    pub fn tag(self) -> String {
        self.as_str().to_ascii_uppercase()
    }
}

/// UI Automation control patterns surfaced to callers (names only, never interfaces).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum UiPattern {
    Invoke,
    Selection,
    SelectionItem,
    Value,
    RangeValue,
    Scroll,
    ScrollItem,
    ExpandCollapse,
    Toggle,
    Text,
    Window,
    Transform,
    Grid,
    GridItem,
    Table,
    TableItem,
    LegacyIAccessible,
    VirtualizedItem,
    ItemContainer,
    Drag,
    DropTarget,
    TextEdit,
}

impl UiPattern {
    /// Patterns implying the element accepts a semantic action.
    pub fn is_actionable(self) -> bool {
        matches!(
            self,
            Self::Invoke
                | Self::SelectionItem
                | Self::Value
                | Self::RangeValue
                | Self::ExpandCollapse
                | Self::Toggle
                | Self::TextEdit
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToggleState {
    Off,
    On,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ExpandState {
    Collapsed,
    Expanded,
    PartiallyExpanded,
    LeafNode,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One element as the model sees it (spec §7). No native objects, ever.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ElementInfo {
    #[serde(rename = "ref")]
    pub reference: String,
    pub role: ControlRole,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub automation_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub class_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub framework: String,
    pub enabled: bool,
    pub visible: bool,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<PhysicalRect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patterns: Vec<UiPattern>,
    /// Already redacted to `[REDACTED]` for sensitive fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub sensitive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toggle_state: Option<ToggleState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expand_state: Option<ExpandState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

/// Detailed view for `inspect` (spec §29 inspector fields).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ElementDetails {
    #[serde(flatten)]
    pub element: ElementInfo,
    pub control_type_id: i32,
    pub process_id: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub process_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_window_handle: Option<u64>,
    pub runtime_id: Vec<i32>,
    pub keyboard_focusable: bool,
    pub offscreen: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub help_text: String,
    /// Outermost first: `Window "Notepad" > Pane > Document "Text editor"`.
    pub ancestors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uia_ids_map_to_roles() {
        assert_eq!(ControlRole::from_uia(50000, false), ControlRole::Button);
        assert_eq!(ControlRole::from_uia(50005, false), ControlRole::Link);
        assert_eq!(ControlRole::from_uia(50032, false), ControlRole::Window);
        assert_eq!(ControlRole::from_uia(50032, true), ControlRole::Dialog);
        assert_eq!(ControlRole::from_uia(50040, false), ControlRole::AppBar);
        assert_eq!(ControlRole::from_uia(50041, false), ControlRole::Unknown);
        assert_eq!(ControlRole::from_uia(-1, false), ControlRole::Unknown);
    }

    #[test]
    fn every_role_parses_from_its_name() {
        for role in ControlRole::ALL {
            assert_eq!(ControlRole::parse(role.as_str()), Some(role));
            assert_eq!(
                serde_json::to_string(&role).unwrap(),
                format!("\"{}\"", role.as_str())
            );
        }
        assert_eq!(ControlRole::parse("button"), Some(ControlRole::Button));
        assert_eq!(ControlRole::parse("Hyperlink"), Some(ControlRole::Link));
        assert_eq!(ControlRole::parse("nope"), None);
    }

    #[test]
    fn window_role_matches_dialogs() {
        assert!(ControlRole::Window.matches(ControlRole::Dialog));
        assert!(!ControlRole::Dialog.matches(ControlRole::Window));
        assert!(!ControlRole::Button.matches(ControlRole::Edit));
    }

    #[test]
    fn element_info_uses_wire_names() {
        let e = ElementInfo {
            reference: "e42".into(),
            role: ControlRole::Button,
            name: "Save".into(),
            automation_id: "SaveButton".into(),
            class_name: String::new(),
            framework: "WPF".into(),
            enabled: true,
            visible: true,
            focused: false,
            bounds: Some(PhysicalRect::new(1240, 820, 1324, 858)),
            patterns: vec![UiPattern::Invoke],
            value: None,
            sensitive: false,
            toggle_state: None,
            expand_state: None,
            selected: None,
            path: String::new(),
        };
        let json = serde_json::to_value(&e).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "ref": "e42", "role": "Button", "name": "Save",
                "automationId": "SaveButton", "framework": "WPF",
                "enabled": true, "visible": true, "focused": false,
                "bounds": [1240, 820, 1324, 858], "patterns": ["Invoke"]
            })
        );
        let back: ElementInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, e);
    }
}
