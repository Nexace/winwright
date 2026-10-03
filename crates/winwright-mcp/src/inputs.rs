//! Model-facing tool inputs: flat, forgiving, documented (doc comments become JSON-schema
//! descriptions). Each converts into an engine request with typed validation errors.

use schemars::JsonSchema;
use serde::Deserialize;
use winwright_contracts::WinwrightError;
use winwright_contracts::action::{DesktopAction, ElementTarget, ScrollDirection, WindowAction};
use winwright_contracts::capture::{ImageFormat, ScreenshotRequest, ScreenshotTarget};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::input::MouseButton;
use winwright_contracts::locator::{ElementLocator, FindRequest, MatchMode};
use winwright_contracts::overlay::{HighlightRequest, OverlayStyle};
use winwright_contracts::snapshot::{SnapshotRequest, SnapshotTarget};
use winwright_contracts::system::{ExecRequest, FileOperation, LaunchRequest};
use winwright_contracts::wait::{WaitRequest, WaitState};
use winwright_contracts::window::WindowSelector;
use winwright_core::InspectRequest;

type Result<T> = std::result::Result<T, WinwrightError>;

fn title_scope(window: &Option<String>) -> SnapshotTarget {
    match window.as_deref() {
        Some("*") => SnapshotTarget::AllWindows,
        Some(title) => SnapshotTarget::Window(WindowSelector {
            title: Some(title.to_owned()),
            ..Default::default()
        }),
        None => SnapshotTarget::Active,
    }
}

/// Target an element by `ref` (preferred) or by locator fields.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TargetFields {
    /// Element ref from desktop_snapshot or desktop_find, e.g. "e12". Preferred over locators.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// UIA role, e.g. Button, Edit, CheckBox, ComboBox, List, ListItem, MenuItem, TabItem, TreeItem, Link, Text, Document, Window.
    pub role: Option<String>,
    /// Accessible name.
    pub name: Option<String>,
    /// Visible text (the element's name or value).
    pub text: Option<String>,
    /// UIA AutomationId.
    pub automation_id: Option<String>,
    /// Label of an input field, e.g. "File name".
    pub label: Option<String>,
    /// false = substring match. Default: exact (case-insensitive).
    pub exact: Option<bool>,
    /// Zero-based index among matches. Only use when several elements match.
    pub nth: Option<usize>,
    /// Title (substring) of the window to search in. Default: the active window. "*" = all windows.
    pub window: Option<String>,
}

impl TargetFields {
    fn has_locator(&self) -> bool {
        self.role.is_some()
            || self.name.is_some()
            || self.text.is_some()
            || self.automation_id.is_some()
            || self.label.is_some()
    }

    fn match_mode(&self) -> MatchMode {
        if self.exact == Some(false) {
            MatchMode::Contains
        } else {
            MatchMode::Exact
        }
    }

    fn locator(&self) -> ElementLocator {
        ElementLocator {
            role: self.role.clone(),
            name: self.name.clone(),
            text: self.text.clone(),
            automation_id: self.automation_id.clone(),
            label: self.label.clone(),
            match_mode: self.match_mode(),
            nth: self.nth,
            ..Default::default()
        }
    }

    pub fn required(&self) -> Result<ElementTarget> {
        match (&self.reference, self.has_locator()) {
            (Some(r), false) => Ok(ElementTarget::by_ref(r.clone())),
            (None, true) => Ok(ElementTarget::by_locator(
                self.locator(),
                title_scope(&self.window),
            )),
            (Some(_), true) => Err(WinwrightError::invalid(
                "give either ref or locator fields (role/name/text/automationId/label), not both",
            )),
            (None, false) => Err(WinwrightError::invalid(
                "a target needs ref (e.g. \"e12\") or locator fields such as role+name or label",
            )),
        }
    }

    pub fn optional(&self) -> Result<Option<ElementTarget>> {
        if self.reference.is_none() && !self.has_locator() {
            Ok(None)
        } else {
            self.required().map(Some)
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInput {
    /// Window title (substring) to snapshot; "*" for all windows. Default: the active window.
    pub window: Option<String>,
    /// Snapshot only this element's subtree.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// true: return only changes since your previous snapshot of the same window(s).
    pub diff: Option<bool>,
    /// true: include named non-interactive containers too.
    pub all: Option<bool>,
    pub include_bounds: Option<bool>,
    pub include_offscreen: Option<bool>,
    /// Default: the configured automation.maxSnapshotNodes (500).
    pub max_nodes: Option<u32>,
    /// Default 12.
    pub max_depth: Option<u32>,
}

impl SnapshotInput {
    /// `default_max_nodes` is the configured `automation.maxSnapshotNodes`.
    pub fn request(&self, default_max_nodes: u32) -> SnapshotRequest {
        let defaults = SnapshotRequest::default();
        SnapshotRequest {
            target: match &self.reference {
                Some(r) => SnapshotTarget::Subtree {
                    reference: r.clone(),
                },
                None => title_scope(&self.window),
            },
            interactive_only: !self.all.unwrap_or(false),
            include_bounds: self.include_bounds.unwrap_or(false),
            include_offscreen: self.include_offscreen.unwrap_or(false),
            max_nodes: self.max_nodes.unwrap_or(default_max_nodes),
            max_depth: self.max_depth.unwrap_or(defaults.max_depth),
            diff: self.diff.unwrap_or(false),
            ..defaults
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Include offscreen/hidden elements.
    pub include_hidden: Option<bool>,
    /// Maximum matches to return (default 20).
    pub limit: Option<u32>,
}

impl FindInput {
    pub fn request(&self) -> Result<FindRequest> {
        if self.target.reference.is_some() {
            return Err(WinwrightError::invalid(
                "desktop_find takes locator fields, not ref",
            ));
        }
        let l = self.target.locator();
        Ok(FindRequest {
            scope: title_scope(&self.target.window),
            role: l.role,
            name: l.name,
            text: l.text,
            automation_id: l.automation_id,
            class_name: None,
            framework_id: None,
            label: l.label,
            ancestor: None,
            exact: None,
            match_mode: Some(l.match_mode),
            case_sensitive: false,
            visible_only: !self.include_hidden.unwrap_or(false),
            nth: l.nth,
            limit: self.limit.unwrap_or(20).clamp(1, 200),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InspectInput {
    /// Element ref to inspect.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Physical screen X (with y). Omit ref and x/y to inspect the focused element.
    pub x: Option<i32>,
    /// Physical screen Y (with x).
    pub y: Option<i32>,
}

impl InspectInput {
    pub fn request(&self) -> Result<InspectRequest> {
        match (&self.reference, self.x, self.y) {
            (Some(r), None, None) => Ok(InspectRequest::Ref(r.clone())),
            (None, Some(x), Some(y)) => Ok(InspectRequest::Point(PhysicalPoint { x, y })),
            (None, None, None) => Ok(InspectRequest::Focused),
            (Some(_), _, _) => Err(WinwrightError::invalid("give either ref or x/y, not both")),
            (None, _, _) => Err(WinwrightError::invalid("x and y must be given together")),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ButtonInput {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClickInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Default left. Right/middle use real mouse input.
    pub button: Option<ButtonInput>,
    /// Double-click with real mouse input.
    pub double_click: Option<bool>,
    /// Click with real mouse input even when a UIA pattern exists.
    pub force_physical: Option<bool>,
}

impl ClickInput {
    pub fn action(&self) -> Result<DesktopAction> {
        Ok(DesktopAction::Click {
            target: self.target.required()?,
            button: match self.button {
                Some(ButtonInput::Right) => MouseButton::Right,
                Some(ButtonInput::Middle) => MouseButton::Middle,
                _ => MouseButton::Left,
            },
            click_count: if self.double_click == Some(true) {
                2
            } else {
                1
            },
            force_physical: self.force_physical.unwrap_or(false),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FillInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Text to put in the field.
    pub value: String,
    /// true: append to the existing text instead of replacing it.
    pub append: Option<bool>,
}

impl FillInput {
    pub fn action(&self) -> Result<DesktopAction> {
        Ok(DesktopAction::Fill {
            target: self.target.required()?,
            text: self.value.clone(),
            clear: !self.append.unwrap_or(false),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TypeInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Text to type. "\n" presses Enter.
    pub value: String,
}

impl TypeInput {
    pub fn action(&self) -> Result<DesktopAction> {
        Ok(DesktopAction::TypeText {
            target: self.target.optional()?,
            text: self.value.clone(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PressInput {
    /// Key chord, e.g. "Ctrl+S", "Enter", "Alt+F4", "Ctrl+Shift+Tab". Use "Plus" for +.
    pub keys: String,
    #[serde(flatten)]
    pub target: TargetFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelectInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Item to select inside the target (combo box, list, tab strip, tree). Omit to select the target itself.
    pub option: Option<String>,
}

impl SelectInput {
    pub fn action(&self) -> Result<DesktopAction> {
        Ok(DesktopAction::Select {
            target: self.target.required()?,
            option: self.option.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CheckState {
    Check,
    Uncheck,
    Toggle,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CheckInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Default check.
    pub state: Option<CheckState>,
}

impl CheckInput {
    pub fn action(&self) -> Result<DesktopAction> {
        let target = self.target.required()?;
        Ok(match self.state.unwrap_or(CheckState::Check) {
            CheckState::Check => DesktopAction::Check { target },
            CheckState::Uncheck => DesktopAction::Uncheck { target },
            CheckState::Toggle => DesktopAction::Toggle { target },
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExpandInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// false collapses. Default true.
    pub expand: Option<bool>,
}

impl ExpandInput {
    pub fn action(&self) -> Result<DesktopAction> {
        let target = self.target.required()?;
        Ok(if self.expand.unwrap_or(true) {
            DesktopAction::Expand { target }
        } else {
            DesktopAction::Collapse { target }
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScrollInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Default down.
    pub direction: Option<ScrollDirection>,
    /// Pages to scroll (default 1).
    pub amount: Option<u32>,
    /// true: scroll the target element itself into view instead.
    pub into_view: Option<bool>,
}

impl ScrollInput {
    pub fn action(&self) -> Result<DesktopAction> {
        let target = self.target.required()?;
        Ok(if self.into_view == Some(true) {
            DesktopAction::ScrollIntoView { target }
        } else {
            DesktopAction::Scroll {
                target,
                direction: self.direction.unwrap_or(ScrollDirection::Down),
                amount: self.amount.unwrap_or(1),
            }
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FocusInput {
    #[serde(flatten)]
    pub target: TargetFields,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Default 4000.
    pub max_chars: Option<u32>,
}

impl ReadTextInput {
    pub fn action(&self) -> Result<DesktopAction> {
        Ok(DesktopAction::ReadText {
            target: self.target.required()?,
            max_chars: self.max_chars.unwrap_or(4_000),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WaitInput {
    /// exists, missing, visible, hidden, enabled, disabled, focused, value, text, window-open, window-closed.
    pub state: WaitState,
    /// Element target (ref or locator). For window-open/window-closed, `window` is the window title to wait for.
    #[serde(flatten)]
    pub target: TargetFields,
    /// Expected value for the `value`/`text` states (exact=false for substring).
    pub value: Option<String>,
    /// Default: the configured automation.defaultTimeoutMs (10000). Max 600000.
    pub timeout_ms: Option<u64>,
}

impl WaitInput {
    pub fn request(&self) -> Result<WaitRequest> {
        let value_match = self.target.match_mode();
        if self.state.is_window_state() {
            let title = self.target.window.clone().ok_or_else(|| {
                WinwrightError::invalid(
                    "window-open/window-closed need `window` (a title substring)",
                )
            })?;
            return Ok(WaitRequest {
                state: self.state,
                reference: None,
                locator: None,
                scope: SnapshotTarget::Active,
                window: Some(WindowSelector {
                    title: Some(title),
                    ..Default::default()
                }),
                value: None,
                value_match,
                timeout_ms: self.timeout_ms,
            });
        }
        let target = self.target.required()?;
        Ok(WaitRequest {
            state: self.state,
            reference: target.reference,
            locator: target.locator,
            scope: target.scope,
            window: None,
            value: self.value.clone(),
            value_match,
            timeout_ms: self.timeout_ms,
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WindowOp {
    Focus,
    Move,
    Resize,
    Minimize,
    Maximize,
    Restore,
    Close,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WindowInput {
    pub action: WindowOp,
    /// Window title (substring).
    pub window: Option<String>,
    /// Executable name, e.g. notepad.exe.
    pub process: Option<String>,
    /// Window handle from desktop_windows.
    pub hwnd: Option<u64>,
    /// For move: physical x.
    pub x: Option<i32>,
    /// For move: physical y.
    pub y: Option<i32>,
    /// For resize.
    pub width: Option<i32>,
    /// For resize.
    pub height: Option<i32>,
}

impl WindowInput {
    pub fn action(&self) -> Result<WindowAction> {
        let window = WindowSelector {
            title: self.window.clone(),
            process: self.process.clone(),
            hwnd: self.hwnd,
        };
        if window.is_empty() {
            return Err(WinwrightError::invalid(
                "name the window with window, process, or hwnd",
            ));
        }
        let need = |v: Option<i32>, field: &str| {
            v.ok_or_else(|| WinwrightError::invalid(format!("{field} is required for this action")))
        };
        Ok(match self.action {
            WindowOp::Focus => WindowAction::Focus { window },
            WindowOp::Minimize => WindowAction::Minimize { window },
            WindowOp::Maximize => WindowAction::Maximize { window },
            WindowOp::Restore => WindowAction::Restore { window },
            WindowOp::Close => WindowAction::Close { window },
            WindowOp::Move => WindowAction::Move {
                window,
                x: need(self.x, "x")?,
                y: need(self.y, "y")?,
            },
            WindowOp::Resize => WindowAction::Resize {
                window,
                width: need(self.width, "width")?,
                height: need(self.height, "height")?,
            },
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotInput {
    /// Capture this element's on-screen bounds.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Window title (substring). Default: the active window.
    pub window: Option<String>,
    /// Monitor index.
    pub monitor: Option<u32>,
    /// Physical region [x, y, width, height].
    pub region: Option<[i32; 4]>,
    /// Every monitor in one image.
    pub desktop: Option<bool>,
    /// Default jpeg (smaller); png for exact pixels.
    pub format: Option<ImageFormat>,
    /// JPEG quality 1-100 (default 80).
    pub quality: Option<u8>,
}

impl ScreenshotInput {
    pub fn request(&self) -> Result<ScreenshotRequest> {
        let targets = [
            self.reference.is_some(),
            self.window.is_some(),
            self.monitor.is_some(),
            self.region.is_some(),
            self.desktop == Some(true),
        ];
        if targets.into_iter().filter(|&t| t).count() > 1 {
            return Err(WinwrightError::invalid(
                "give only one of ref, window, monitor, region, or desktop",
            ));
        }
        let target = if let Some(r) = &self.reference {
            ScreenshotTarget::Element {
                reference: r.clone(),
            }
        } else if let Some(m) = self.monitor {
            ScreenshotTarget::Monitor(m)
        } else if let Some([x, y, w, h]) = self.region {
            if w <= 0 || h <= 0 {
                return Err(WinwrightError::invalid(
                    "region width and height must be positive",
                ));
            }
            let (Some(right), Some(bottom)) = (x.checked_add(w), y.checked_add(h)) else {
                return Err(WinwrightError::invalid(
                    "region extends past the coordinate range",
                ));
            };
            ScreenshotTarget::Region(PhysicalRect::new(x, y, right, bottom))
        } else if self.desktop == Some(true) {
            ScreenshotTarget::Desktop
        } else if let Some(title) = &self.window {
            ScreenshotTarget::Window(WindowSelector {
                title: Some(title.clone()),
                ..Default::default()
            })
        } else {
            ScreenshotTarget::Active
        };
        Ok(ScreenshotRequest {
            target,
            format: self.format.unwrap_or(ImageFormat::Jpeg),
            quality: Some(self.quality.unwrap_or(80)),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HighlightInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Text shown next to the element, e.g. "Click here".
    pub caption: Option<String>,
    /// highlight (default), arrow, or clickMarker.
    pub style: Option<OverlayStyle>,
    /// How long it stays (default 8000 ms).
    pub duration_ms: Option<u64>,
}

impl HighlightInput {
    pub fn request(&self) -> Result<HighlightRequest> {
        Ok(HighlightRequest {
            target: self.target.required()?,
            style: self.style.unwrap_or_default(),
            label: self.caption.clone(),
            step: None,
            color: None,
            duration_ms: self.duration_ms,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchInput {
    /// notepad.exe, ms-settings:display, or a folder/file path.
    pub app: String,
    /// Arguments for an executable.
    pub args: Option<Vec<String>>,
}

impl LaunchInput {
    pub fn request(&self) -> LaunchRequest {
        LaunchRequest {
            app: self.app.clone(),
            args: self.args.clone().unwrap_or_default(),
            working_dir: None,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileInput {
    /// The operation, tagged by `op`: list, metadata, copy, move, rename, delete, createDirectory, search, knownFolder.
    pub operation: FileOperation,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecInput {
    /// Program to run (no shell), e.g. git.exe.
    pub program: String,
    pub args: Option<Vec<String>>,
    /// Default 30000.
    pub timeout_ms: Option<u64>,
}

impl ExecInput {
    pub fn request(&self) -> ExecRequest {
        ExecRequest {
            program: self.program.clone(),
            args: self.args.clone().unwrap_or_default(),
            working_dir: None,
            timeout_ms: self.timeout_ms.unwrap_or(30_000),
            max_output_bytes: 64 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattened_targets_parse_from_model_json() {
        let c: ClickInput = serde_json::from_str(r#"{"ref":"e12"}"#).unwrap();
        assert!(
            matches!(c.action().unwrap(), DesktopAction::Click { ref target, click_count: 1, .. } if target.reference.as_deref() == Some("e12"))
        );
        let f: FillInput =
            serde_json::from_str(r#"{"label":"File name","value":"notes.txt","window":"Save As"}"#)
                .unwrap();
        let DesktopAction::Fill {
            target,
            text,
            clear,
        } = f.action().unwrap()
        else {
            panic!()
        };
        assert_eq!((text.as_str(), clear), ("notes.txt", true));
        assert!(matches!(target.scope, SnapshotTarget::Window(_)));
        let bad: FocusInput = serde_json::from_str(r#"{}"#).unwrap();
        assert!(bad.target.required().is_err());
    }

    #[test]
    fn wait_input_window_states_use_window_title() {
        let w: WaitInput =
            serde_json::from_str(r#"{"state":"window-open","window":"Save As"}"#).unwrap();
        let req = w.request().unwrap();
        assert_eq!(req.window.unwrap().title.as_deref(), Some("Save As"));
        let w: WaitInput = serde_json::from_str(r#"{"state":"window-closed"}"#).unwrap();
        assert!(w.request().is_err());
    }

    #[test]
    fn screenshot_defaults_to_jpeg_of_active_window() {
        let s: ScreenshotInput = serde_json::from_str("{}").unwrap();
        let r = s.request().unwrap();
        assert_eq!(r.target, ScreenshotTarget::Active);
        assert_eq!(r.format, ImageFormat::Jpeg);
    }

    #[test]
    fn inspect_needs_both_coordinates_and_one_target() {
        let i: InspectInput = serde_json::from_str(r#"{"x":10,"y":-20}"#).unwrap();
        assert_eq!(
            i.request().unwrap(),
            InspectRequest::Point(PhysicalPoint { x: 10, y: -20 })
        );
        let i: InspectInput = serde_json::from_str("{}").unwrap();
        assert_eq!(i.request().unwrap(), InspectRequest::Focused);
        let i: InspectInput = serde_json::from_str(r#"{"ref":"e4"}"#).unwrap();
        assert_eq!(i.request().unwrap(), InspectRequest::Ref("e4".into()));
        for bad in [r#"{"x":10}"#, r#"{"y":10}"#, r#"{"ref":"e1","x":1,"y":2}"#] {
            let i: InspectInput = serde_json::from_str(bad).unwrap();
            assert!(i.request().is_err(), "{bad}");
        }
    }

    #[test]
    fn screenshot_region_is_checked_without_overflow() {
        let s: ScreenshotInput = serde_json::from_str(r#"{"region":[-1920,10,800,600]}"#).unwrap();
        assert_eq!(
            s.request().unwrap().target,
            ScreenshotTarget::Region(PhysicalRect::new(-1920, 10, -1120, 610))
        );
        let s: ScreenshotInput =
            serde_json::from_str(r#"{"region":[2147483000,0,1000,10]}"#).unwrap();
        assert_eq!(
            s.request().unwrap_err().code(),
            winwright_contracts::ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn screenshot_takes_exactly_one_target() {
        for bad in [
            r#"{"window":"Notepad","region":[0,0,10,10]}"#,
            r#"{"ref":"e3","monitor":0}"#,
            r#"{"desktop":true,"window":"Notepad"}"#,
        ] {
            let s: ScreenshotInput = serde_json::from_str(bad).unwrap();
            assert!(s.request().is_err(), "{bad}");
        }
        let s: ScreenshotInput =
            serde_json::from_str(r#"{"window":"Notepad","desktop":false}"#).unwrap();
        assert!(matches!(
            s.request().unwrap().target,
            ScreenshotTarget::Window(_)
        ));
    }

    #[test]
    fn snapshot_node_budget_defaults_to_the_configured_one() {
        let s: SnapshotInput = serde_json::from_str("{}").unwrap();
        assert_eq!(s.request(1_200).max_nodes, 1_200);
        let s: SnapshotInput = serde_json::from_str(r#"{"maxNodes":50}"#).unwrap();
        assert_eq!(s.request(1_200).max_nodes, 50);
    }

    #[test]
    fn every_input_schema_is_an_object() {
        fn is_object<T: JsonSchema>() -> bool {
            let schema = schemars::schema_for!(T);
            schema.get("type").and_then(|t| t.as_str()) == Some("object")
        }
        assert!(is_object::<SnapshotInput>());
        assert!(is_object::<ClickInput>());
        assert!(is_object::<FillInput>());
        assert!(is_object::<WaitInput>());
        assert!(is_object::<ScreenshotInput>());
        assert!(is_object::<FileInput>());
        assert!(is_object::<WindowInput>());
    }
}
