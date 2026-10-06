//! Model-facing tool inputs: flat, forgiving, documented (doc comments become JSON-schema
//! descriptions). Each converts into an engine request with typed validation errors.

use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use winwright_contracts::WinwrightError;
use winwright_contracts::action::{
    DesktopAction, ElementTarget, ScreenPoint, ScrollDirection, WindowAction,
};
use winwright_contracts::capture::{ImageFormat, ScreenshotRequest, ScreenshotTarget};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::input::MouseButton;
use winwright_contracts::input::parse_chord;
use winwright_contracts::locator::{ElementLocator, FindRequest, MatchMode};
use winwright_contracts::overlay::{
    DEFAULT_SPOT_SIDE, GuideRequest, GuideStep, GuideTarget, GuideWait, HighlightRequest,
    OverlayStyle, RecordRequest, ScreenSpot, SpotHighlightRequest,
};
use winwright_contracts::snapshot::{SnapshotRequest, SnapshotTarget};

use winwright_contracts::system::{ExecRequest, FileOperation, LaunchRequest, SessionStart};
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

fn mouse_button(button: Option<ButtonInput>) -> MouseButton {
    match button {
        Some(ButtonInput::Right) => MouseButton::Right,
        Some(ButtonInput::Middle) => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

fn click_count(double_click: Option<bool>) -> u32 {
    if double_click == Some(true) { 2 } else { 1 }
}

impl ClickInput {
    pub fn action(&self) -> Result<DesktopAction> {
        Ok(DesktopAction::Click {
            target: self.target.required()?,
            button: mouse_button(self.button),
            click_count: click_count(self.double_click),
            force_physical: self.force_physical.unwrap_or(false),
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MouseOp {
    Move,
    Click,
    Drag,
    Scroll,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MouseInput {
    /// move (only the pointer), click, drag (to toX/toY), or scroll (the wheel).
    pub action: MouseOp,
    /// Pixels from the left of `window` (the pixels of its desktop_screenshot), or of the
    /// screen when no window is given.
    pub x: i32,
    /// Pixels from the top, like x.
    pub y: i32,
    /// Title (substring) of the window x/y are relative to.
    pub window: Option<String>,
    /// Drag end, in the same pixels as x/y.
    pub to_x: Option<i32>,
    pub to_y: Option<i32>,
    /// The `scale` a scaled-down desktop_screenshot reported: x/y (and toX/toY) are then that
    /// image's pixels. Default 1.
    pub scale: Option<f64>,
    /// Default left.
    pub button: Option<ButtonInput>,
    /// Double-click.
    pub double_click: Option<bool>,
    /// For scroll; default down.
    pub direction: Option<ScrollDirection>,
    /// For scroll: wheel notches, 3 lines each (default 3).
    pub amount: Option<u32>,
}

impl MouseInput {
    pub fn action(&self) -> Result<DesktopAction> {
        let scale = image_scale(self.scale)?;
        let point = |x: i32, y: i32| ScreenPoint {
            x: unscale(x, scale),
            y: unscale(y, scale),
            window: self.window.as_ref().map(|title| WindowSelector {
                title: Some(title.clone()),
                ..Default::default()
            }),
        };
        let at = point(self.x, self.y);
        let button = mouse_button(self.button);
        Ok(match self.action {
            MouseOp::Move => DesktopAction::MoveMouse { at },
            MouseOp::Click => DesktopAction::ClickAt {
                at,
                button,
                click_count: click_count(self.double_click),
            },
            MouseOp::Drag => match (self.to_x, self.to_y) {
                (Some(x), Some(y)) => DesktopAction::Drag {
                    from: at,
                    to: point(x, y),
                    button,
                },
                _ => return Err(WinwrightError::invalid("drag needs toX and toY")),
            },
            MouseOp::Scroll => DesktopAction::ScrollAt {
                at,
                direction: self.direction.unwrap_or(ScrollDirection::Down),
                amount: self.amount.unwrap_or(3),
            },
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
    /// `key` is accepted too: models reach for it for a single chord.
    #[serde(alias = "key")]
    pub keys: String,
    #[serde(flatten)]
    pub target: TargetFields,
}

impl PressInput {
    pub fn action(&self) -> Result<DesktopAction> {
        let keys = parse_chord(&self.keys).map_err(WinwrightError::invalid)?;
        Ok(DesktopAction::Press {
            target: self.target.optional()?,
            keys,
        })
    }
}

impl FocusInput {
    pub fn action(&self) -> Result<DesktopAction> {
        Ok(DesktopAction::Focus {
            target: self.target.required()?,
        })
    }
}

/// One step of desktop_batch: `do` names the tool, the other fields are that tool's.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "do", rename_all = "camelCase")]
pub enum BatchStep {
    Click(ClickInput),
    Fill(FillInput),
    Type(TypeInput),
    Press(PressInput),
    Select(SelectInput),
    Check(CheckInput),
    Expand(ExpandInput),
    Scroll(ScrollInput),
    Focus(FocusInput),
    ReadText(ReadTextInput),
    Mouse(MouseInput),
    WaitFor(WaitInput),
}

pub enum BatchCall {
    Act(Result<DesktopAction>),
    Wait(Result<WaitRequest>),
}

impl BatchStep {
    pub fn call(&self) -> BatchCall {
        BatchCall::Act(match self {
            Self::Click(i) => i.action(),
            Self::Fill(i) => i.action(),
            Self::Type(i) => i.action(),
            Self::Press(i) => i.action(),
            Self::Select(i) => i.action(),
            Self::Check(i) => i.action(),
            Self::Expand(i) => i.action(),
            Self::Scroll(i) => i.action(),
            Self::Focus(i) => i.action(),
            Self::ReadText(i) => i.action(),
            Self::Mouse(i) => i.action(),
            Self::WaitFor(i) => return BatchCall::Wait(i.request()),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchInput {
    /// 1 to 20 steps, run in order.
    pub steps: Vec<BatchStep>,
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
    /// Read the words on it with Windows OCR instead of returning the image: each line's text
    /// and box in screen pixels. For apps whose snapshot tree lacks their text.
    pub ocr: Option<bool>,
    /// With ocr: only the words or lines containing this text (any case), with the screen
    /// point to click (desktop_mouse without window or scale).
    pub find: Option<String>,
    /// For a window: draw each button, field and item's ref number on the image (12 = e12)
    /// and list them, so you can act by ref (desktop_click ref=e12) on what you see.
    pub marks: Option<bool>,
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
            fit: true,
            marks: self.marks == Some(true),
        })
    }
}

/// A screenshot's reported scale: in (0, 1], default 1.
fn image_scale(scale: Option<f64>) -> Result<f64> {
    match scale {
        None => Ok(1.0),
        Some(s) if s.is_finite() && s > 0.0 && s <= 1.0 => Ok(s),
        Some(s) => Err(WinwrightError::invalid(format!(
            "scale {s} must be in (0, 1]: pass the scale a desktop_screenshot reported"
        ))),
    }
}

/// A pixel of a scaled-down image to the physical pixel it shows.
fn unscale(v: i32, scale: f64) -> i32 {
    (f64::from(v) / scale).round() as i32
}

fn titled(window: &Option<String>) -> Option<WindowSelector> {
    window.as_ref().map(|title| WindowSelector {
        title: Some(title.clone()),
        ..Default::default()
    })
}

/// A spot centered on x/y in the pixels of `window`'s screenshot (or the screen), shown at
/// `scale` (from the screenshot; 1 when not scaled down).
fn spot(
    (x, y): (i32, i32),
    (width, height): (Option<u32>, Option<u32>),
    window: &Option<String>,
    scale: Option<f64>,
) -> Result<ScreenSpot> {
    let scale = image_scale(scale)?;
    let side = |v: Option<u32>| {
        v.map_or(DEFAULT_SPOT_SIDE, |v| {
            u32::try_from(unscale(v.min(i32::MAX as u32) as i32, scale)).unwrap_or(1)
        })
    };
    Ok(ScreenSpot {
        at: ScreenPoint {
            x: unscale(x, scale),
            y: unscale(y, scale),
            window: titled(window),
        },
        width: side(width),
        height: side(height),
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HighlightInput {
    #[serde(flatten)]
    pub target: TargetFields,
    /// Instead of an element: the center of a spot, in pixels of `window`'s desktop_screenshot
    /// (or of the screen without `window`), for what has no element.
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// The spot's size around x/y (default 48 each).
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// The `scale` a scaled-down desktop_screenshot reported, when x/y come from it.
    pub scale: Option<f64>,
    /// Text shown there, e.g. "Click here".
    pub caption: Option<String>,
    /// pointer (default: a pointer with the caption in a bubble), highlight (a box), arrow, or clickMarker.
    pub style: Option<OverlayStyle>,
    /// How long it stays (default 8000 ms).
    pub duration_ms: Option<u64>,
}

pub enum Highlight {
    Element(Box<HighlightRequest>),
    Spot(SpotHighlightRequest),
}

impl HighlightInput {
    pub fn request(&self) -> Result<Highlight> {
        let style = self.style.unwrap_or(OverlayStyle::Pointer);
        match (self.x, self.y) {
            (None, None) => Ok(Highlight::Element(Box::new(HighlightRequest {
                target: self.target.required()?,
                style,
                label: self.caption.clone(),
                step: None,
                color: None,
                duration_ms: self.duration_ms,
            }))),
            (Some(x), Some(y)) if self.target.reference.is_none() && !self.target.has_locator() => {
                Ok(Highlight::Spot(SpotHighlightRequest {
                    spot: spot(
                        (x, y),
                        (self.width, self.height),
                        &self.target.window,
                        self.scale,
                    )?,
                    style,
                    label: self.caption.clone(),
                    color: None,
                    duration_ms: self.duration_ms,
                }))
            }
            (Some(_), Some(_)) => Err(WinwrightError::invalid(
                "give either ref/locator fields or x/y, not both",
            )),
            _ => Err(WinwrightError::invalid("x and y go together")),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuideStepInput {
    /// What the person should do, e.g. "Click Develop" (at most 100 characters).
    pub caption: String,
    /// The element to point at (a ref from desktop_snapshot or desktop_find), or give x/y.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// The spot's center in pixels of `window`'s desktop_screenshot (or of the screen).
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// The spot's size (default 48 each): a click inside it does the step.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Title (substring) of the window x/y are relative to.
    pub window: Option<String>,
    /// The `scale` a scaled-down desktop_screenshot reported, when x/y come from it.
    pub scale: Option<f64>,
    /// click (default): wait for a click inside the spot. change: wait until the spot's pixels
    /// change, for a step done with the keyboard (name the keys in the caption).
    pub wait: Option<GuideWait>,
}

impl GuideStepInput {
    fn step(&self, n: usize) -> Result<GuideStep> {
        let target = match (&self.reference, self.x, self.y) {
            (Some(r), None, None) => {
                GuideTarget::Element(Box::new(ElementTarget::by_ref(r.clone())))
            }
            (None, Some(x), Some(y)) => GuideTarget::Spot(spot(
                (x, y),
                (self.width, self.height),
                &self.window,
                self.scale,
            )?),
            (Some(_), _, _) => {
                return Err(WinwrightError::invalid(format!(
                    "step {n}: give ref or x/y, not both"
                )));
            }
            _ => {
                return Err(WinwrightError::invalid(format!(
                    "step {n}: give ref, or both x and y"
                )));
            }
        };
        Ok(GuideStep {
            target,
            caption: self.caption.clone(),
            wait: self.wait.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GuideInput {
    /// 1 to 20 steps, shown one at a time. Give several only when all their spots are on
    /// screen now (not inside a menu that has yet to open).
    pub steps: Vec<GuideStepInput>,
    /// pointer (default), highlight, or arrow.
    pub style: Option<OverlayStyle>,
    /// How long to wait for the person in all (default 50000 ms, at most 300000; some apps end
    /// tool calls after 60 s).
    pub timeout_ms: Option<u64>,
    /// Also read each step's caption aloud with the Windows voice (default false).
    pub speak: Option<bool>,
}

impl GuideInput {
    pub fn request(&self) -> Result<GuideRequest> {
        Ok(GuideRequest {
            steps: self
                .steps
                .iter()
                .enumerate()
                .map(|(i, s)| s.step(i + 1))
                .collect::<Result<_>>()?,
            style: self.style.unwrap_or(OverlayStyle::Pointer),
            color: None,
            timeout_ms: self.timeout_ms.unwrap_or(50_000),
            speak: self.speak.unwrap_or(false),
        })
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecordInput {
    /// Stop after the person has been idle this long (default 15 s).
    pub idle_seconds: Option<u32>,
    /// Stop after this long in all (default 120 s, at most 600; some apps end tool calls
    /// after 60 s).
    pub max_seconds: Option<u32>,
}

impl RecordInput {
    pub fn request(&self) -> RecordRequest {
        RecordRequest {
            idle_seconds: self.idle_seconds.unwrap_or(15),
            max_seconds: self.max_seconds.unwrap_or(120),
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessListInput {
    /// Only processes whose name contains this (case-insensitive), e.g. "discord".
    pub name: Option<String>,
    /// At most this many lines (default 100).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TerminateInput {
    /// Process id from process_list.
    pub pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionAction {
    Start,
    Input,
    Read,
    List,
    Stop,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionInput {
    /// start a program, send it input, read its new output, list sessions, or stop one.
    pub action: SessionAction,
    /// For start: the program (cmd.exe, git, a full path; found in System32, Windows or PATH).
    pub program: Option<String>,
    /// For start: its arguments.
    pub args: Option<Vec<String>>,
    /// For start: the folder it runs in.
    pub working_dir: Option<String>,
    /// For input, read and stop: the session id that start returned.
    pub id: Option<u32>,
    /// For input: the text to send.
    pub text: Option<String>,
    /// For input: press Enter after the text (default true).
    pub enter: Option<bool>,
    /// For start, input and read: how long to wait for output, in ms (default 2000, max 30000).
    pub wait_ms: Option<u64>,
}

impl SessionInput {
    pub fn id(&self) -> Result<u32> {
        self.id
            .ok_or_else(|| WinwrightError::invalid("give the session id from start"))
    }

    pub fn start(&self) -> Result<SessionStart> {
        Ok(SessionStart {
            program: self
                .program
                .clone()
                .ok_or_else(|| WinwrightError::invalid("start needs a program"))?,
            args: self.args.clone().unwrap_or_default(),
            working_dir: self.working_dir.as_ref().map(PathBuf::from),
        })
    }

    /// The text to send, with Enter (`\r\n`) unless `enter` is false.
    pub fn input(&self) -> Result<String> {
        let mut text = self
            .text
            .clone()
            .ok_or_else(|| WinwrightError::invalid("input needs text"))?;
        if self.enter != Some(false) {
            text.push_str("\r\n");
        }
        Ok(text)
    }

    pub fn wait(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.wait_ms.unwrap_or(2_000).min(30_000))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchInput {
    /// A name as the Start menu shows it (Discord, Adobe Lightroom Classic), notepad.exe, msedge,
    /// a program path, a folder, or a document/image/media file. URIs:
    /// http(s), mailto, ms-settings, shell:<folder>. Other files (shortcuts, scripts): start
    /// the program that opens them with the file in `args`.
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
    fn batch_steps_are_the_single_tools_inputs_tagged_by_do() {
        let batch: BatchInput = serde_json::from_str(
            r#"{"steps": [
                {"do": "fill", "label": "Message", "value": "hi"},
                {"do": "press", "keys": "Enter"},
                {"do": "waitFor", "state": "exists", "name": "hi"},
                {"do": "click", "ref": "e3"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(batch.steps.len(), 4);
        assert!(matches!(
            batch.steps[0].call(),
            BatchCall::Act(Ok(DesktopAction::Fill { .. }))
        ));
        assert!(matches!(
            batch.steps[1].call(),
            BatchCall::Act(Ok(DesktopAction::Press { .. }))
        ));
        assert!(matches!(batch.steps[2].call(), BatchCall::Wait(Ok(_))));
        assert!(serde_json::from_str::<BatchInput>(r#"{"steps": [{"do": "explode"}]}"#).is_err());
    }

    #[test]
    fn recording_defaults_to_15_s_idle_and_2_minutes() {
        let empty: RecordInput = serde_json::from_str("{}").unwrap();
        let req = empty.request();
        assert_eq!((req.idle_seconds, req.max_seconds), (15, 120));
        let given: RecordInput =
            serde_json::from_str(r#"{"idleSeconds":5,"maxSeconds":60}"#).unwrap();
        assert_eq!(given.request().idle_seconds, 5);
    }

    #[test]
    fn guides_and_highlights_point_at_refs_or_spots() {
        let guide: GuideInput = serde_json::from_str(
            r#"{"steps":[{"caption":"Click Develop","ref":"e4"},
                {"caption":"Drag Exposure right","x":850,"y":420,"width":200,"window":"Lightroom"},
                {"caption":"Press Ctrl+Z","x":10,"y":20,"wait":"change"}]}"#,
        )
        .unwrap();
        let req = guide.request().unwrap();
        assert_eq!((req.style, req.timeout_ms), (OverlayStyle::Pointer, 50_000));
        assert_eq!(
            req.steps[0].target,
            GuideTarget::Element(Box::new(ElementTarget::by_ref("e4")))
        );
        let GuideTarget::Spot(slider) = &req.steps[1].target else {
            panic!("a spot")
        };
        assert_eq!((slider.at.x, slider.width, slider.height), (850, 200, 48));
        assert_eq!(
            slider.at.window.as_ref().unwrap().title.as_deref(),
            Some("Lightroom")
        );
        assert_eq!(req.steps[2].wait, GuideWait::Change);
        assert!(!req.speak, "speaking is opt-in");
        let spoken: GuideInput =
            serde_json::from_str(r#"{"steps":[{"caption":"x","ref":"e1"}],"speak":true}"#).unwrap();
        assert!(spoken.request().unwrap().speak);

        let both: GuideInput =
            serde_json::from_str(r#"{"steps":[{"caption":"x","ref":"e1","x":1,"y":2}]}"#).unwrap();
        assert!(both.request().unwrap_err().to_string().contains("step 1"));
        let half: GuideInput =
            serde_json::from_str(r#"{"steps":[{"caption":"x","x":1}]}"#).unwrap();
        assert!(half.request().is_err());

        let at: HighlightInput =
            serde_json::from_str(r#"{"x":5,"y":6,"caption":"Histogram"}"#).unwrap();
        let Highlight::Spot(spot) = at.request().unwrap() else {
            panic!("a spot")
        };
        assert_eq!((spot.style, spot.spot.width), (OverlayStyle::Pointer, 48));
        let element: HighlightInput = serde_json::from_str(r#"{"ref":"e2"}"#).unwrap();
        assert!(matches!(element.request().unwrap(), Highlight::Element(_)));
        let mixed: HighlightInput = serde_json::from_str(r#"{"ref":"e2","x":1,"y":1}"#).unwrap();
        assert!(mixed.request().is_err());
    }

    #[test]
    fn mouse_input_maps_to_point_actions() {
        let m: MouseInput = serde_json::from_str(
            r#"{"action":"click","x":10,"y":20,"window":"Paint","doubleClick":true}"#,
        )
        .unwrap();
        assert!(matches!(
            m.action().unwrap(),
            DesktopAction::ClickAt { ref at, click_count: 2, button: MouseButton::Left }
                if at.x == 10 && at.window.as_ref().and_then(|w| w.title.as_deref()) == Some("Paint")
        ));
        let m: MouseInput = serde_json::from_str(r#"{"action":"drag","x":0,"y":0}"#).unwrap();
        assert!(m.action().is_err(), "a drag needs its end");
        let m: MouseInput =
            serde_json::from_str(r#"{"action":"drag","x":0,"y":0,"toX":50,"toY":60}"#).unwrap();
        assert!(matches!(m.action().unwrap(), DesktopAction::Drag { ref to, .. } if to.y == 60));
        let m: MouseInput = serde_json::from_str(r#"{"action":"scroll","x":5,"y":5}"#).unwrap();
        assert!(matches!(
            m.action().unwrap(),
            DesktopAction::ScrollAt {
                direction: ScrollDirection::Down,
                amount: 3,
                ..
            }
        ));
    }

    #[test]
    fn press_takes_key_or_keys() {
        for json in [
            r#"{"keys":"ctrl+a"}"#,
            r#"{"key":"ctrl+a","window":"Notepad"}"#,
        ] {
            let p: PressInput = serde_json::from_str(json).unwrap();
            assert_eq!(p.keys, "ctrl+a", "{json}");
        }
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
    fn points_on_a_scaled_screenshot_map_back_to_physical_pixels() {
        let m: MouseInput = serde_json::from_str(
            r#"{"action":"drag","x":100,"y":50,"toX":678,"toY":424,"scale":0.5}"#,
        )
        .unwrap();
        let DesktopAction::Drag { from, to, .. } = m.action().unwrap() else {
            panic!()
        };
        assert_eq!((from.x, from.y, to.x, to.y), (200, 100, 1356, 848));
        for bad in ["0", "-1", "1.5"] {
            let m: MouseInput = serde_json::from_str(&format!(
                r#"{{"action":"click","x":1,"y":1,"scale":{bad}}}"#
            ))
            .unwrap();
            assert!(m.action().is_err(), "scale {bad}");
        }
        let h: HighlightInput =
            serde_json::from_str(r#"{"x":100,"y":100,"width":24,"scale":0.5}"#).unwrap();
        let Highlight::Spot(s) = h.request().unwrap() else {
            panic!()
        };
        assert_eq!((s.spot.at.x, s.spot.width, s.spot.height), (200, 48, 48));
        let s: ScreenshotInput = serde_json::from_str("{}").unwrap();
        assert!(
            s.request().unwrap().fit,
            "the model always gets a fitted image"
        );
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
