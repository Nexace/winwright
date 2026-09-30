//! Command-line shapes and their conversion into engine requests.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use winwright_contracts::action::{ElementTarget, ScrollDirection, WindowAction};
use winwright_contracts::geometry::PhysicalPoint;
use winwright_contracts::locator::{ElementLocator, FindRequest, MatchMode};
use winwright_contracts::snapshot::SnapshotTarget;
use winwright_contracts::window::WindowSelector;
use winwright_contracts::{WinwrightError, WinwrightResult};

#[derive(Parser)]
#[command(
    name = "winwright",
    version,
    about = "Semantic Windows desktop automation",
    after_help = "Refs such as e14 are only valid inside one engine: one-shot commands take \
                  locator flags (--role/--name/--label/...) instead."
)]
pub struct Cli {
    /// Config file (default: %APPDATA%\winwright\config.json).
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Print version and build information.
    Version,
    /// Print the effective configuration.
    Config,
    /// List visible top-level windows (foreground marked with *).
    Windows,
    /// Compact semantic snapshot of a window's UI Automation tree.
    Snapshot(SnapshotArgs),
    /// Inspect one element in detail.
    Inspect(InspectArgs),
    /// Find elements by semantic locator.
    Find(FindArgs),
    /// Click an element (InvokePattern first; physical click only as a fallback).
    Click(ClickArgs),
    /// Set an element's text (ValuePattern first).
    Fill(FillArgs),
    /// Type text with the keyboard into the focused element or a target.
    Type(TypeArgs),
    /// Move keyboard focus to an element.
    Focus(TargetArgs),
    /// Check a checkbox (or select a radio button).
    Check(TargetArgs),
    /// Uncheck a checkbox.
    Uncheck(TargetArgs),
    /// Toggle a checkbox or toggle button.
    Toggle(TargetArgs),
    /// Expand a tree item, combo box, or menu.
    Expand(TargetArgs),
    /// Collapse a tree item, combo box, or menu.
    Collapse(TargetArgs),
    /// Select an item, or an option inside a combo box / list / tab / tree.
    Select(SelectArgs),
    /// Scroll a container.
    Scroll(ScrollArgs),
    /// Press a key chord such as Ctrl+Shift+S, optionally after focusing a target.
    Press(PressArgs),
    /// Read an element's text (TextPattern, ValuePattern, then Name).
    Read(ReadArgs),
    /// Control a top-level window.
    Window(WindowArgs),
    /// Wait until an element or window reaches a state (no fixed sleeps).
    Wait(WaitArgs),
    /// Save an on-demand screenshot of a window, element region, monitor, or the desktop.
    Screenshot(ScreenshotArgs),
    /// Highlight an element with a click-through overlay (tutorial/debug).
    Highlight(HighlightArgs),
    /// Launch an app, shell URI (ms-settings:), or folder.
    Launch(LaunchArgs),
    /// List running processes.
    Processes,
    /// Serve MCP over stdio (launched by an AI client; exits when the client disconnects).
    Mcp,
    /// Show (or clear) the local audit log of actions Winwright performed.
    Audit(AuditArgs),
}

#[derive(Args)]
pub struct AuditArgs {
    /// How many recent events to show.
    #[arg(long, default_value_t = 50)]
    pub last: usize,
    /// Delete the audit log.
    #[arg(long)]
    pub clear: bool,
}

#[derive(Args, Clone, Default)]
pub struct ScopeArgs {
    /// Window whose title contains this text (case-insensitive).
    #[arg(long)]
    pub window: Option<String>,
    /// Window owned by this executable (e.g. notepad or notepad.exe).
    #[arg(long)]
    pub process: Option<String>,
    /// Window handle (decimal or 0x-prefixed hex).
    #[arg(long, value_parser = parse_u64)]
    pub hwnd: Option<u64>,
    /// Search every visible window.
    #[arg(long, conflicts_with_all = ["window", "process", "hwnd"])]
    pub all_windows: bool,
}

impl ScopeArgs {
    pub fn selector(&self) -> WindowSelector {
        WindowSelector {
            title: self.window.clone(),
            process: self.process.clone(),
            hwnd: self.hwnd,
        }
    }

    pub fn target(&self) -> SnapshotTarget {
        let selector = self.selector();
        if self.all_windows {
            SnapshotTarget::AllWindows
        } else if selector.is_empty() {
            SnapshotTarget::Active
        } else {
            SnapshotTarget::Window(selector)
        }
    }
}

#[derive(Args, Clone, Default)]
pub struct LocatorArgs {
    /// UIA role: Button, Edit, CheckBox, ComboBox, ListItem, MenuItem, TabItem, Window, ...
    #[arg(long)]
    pub role: Option<String>,
    /// Accessible name.
    #[arg(long)]
    pub name: Option<String>,
    /// Visible text (name or value).
    #[arg(long)]
    pub text: Option<String>,
    /// UIA AutomationId.
    #[arg(long = "id")]
    pub automation_id: Option<String>,
    /// Label of an input (LabeledBy, preceding text, or enclosing group).
    #[arg(long)]
    pub label: Option<String>,
    /// Class name.
    #[arg(long = "class")]
    pub class_name: Option<String>,
    /// UI framework: Win32, WPF, WinForm, XAML, Chrome, DirectUI.
    #[arg(long)]
    pub framework: Option<String>,
    /// Ancestor role (with --ancestor-name) the element must be inside.
    #[arg(long)]
    pub ancestor_role: Option<String>,
    #[arg(long)]
    pub ancestor_name: Option<String>,
    /// Substring matching instead of exact.
    #[arg(long, conflicts_with = "regex")]
    pub contains: bool,
    /// Regular-expression matching.
    #[arg(long)]
    pub regex: bool,
    #[arg(long)]
    pub case_sensitive: bool,
    /// Include offscreen/hidden elements.
    #[arg(long)]
    pub include_hidden: bool,
    /// Zero-based explicit index among matches (document order).
    #[arg(long)]
    pub nth: Option<usize>,
}

impl LocatorArgs {
    pub fn is_empty(&self) -> bool {
        self.role.is_none()
            && self.name.is_none()
            && self.text.is_none()
            && self.automation_id.is_none()
            && self.label.is_none()
            && self.class_name.is_none()
            && self.framework.is_none()
    }

    pub fn match_mode(&self) -> MatchMode {
        if self.regex {
            MatchMode::Regex
        } else if self.contains {
            MatchMode::Contains
        } else {
            MatchMode::Exact
        }
    }

    pub fn locator(&self) -> ElementLocator {
        let ancestor = (self.ancestor_role.is_some() || self.ancestor_name.is_some()).then(|| {
            Box::new(ElementLocator {
                role: self.ancestor_role.clone(),
                name: self.ancestor_name.clone(),
                match_mode: self.match_mode(),
                case_sensitive: self.case_sensitive,
                visible_only: false,
                ..Default::default()
            })
        });
        ElementLocator {
            role: self.role.clone(),
            name: self.name.clone(),
            text: self.text.clone(),
            automation_id: self.automation_id.clone(),
            class_name: self.class_name.clone(),
            framework_id: self.framework.clone(),
            label: self.label.clone(),
            ancestor,
            match_mode: self.match_mode(),
            case_sensitive: self.case_sensitive,
            visible_only: !self.include_hidden,
            nth: self.nth,
        }
    }
}

#[derive(Args, Clone, Default)]
pub struct TargetArgs {
    /// Element ref from a snapshot/find in the same engine (e.g. e14).
    pub reference: Option<String>,
    #[command(flatten)]
    pub locator: LocatorArgs,
    #[command(flatten)]
    pub scope: ScopeArgs,
}

impl TargetArgs {
    pub fn target(&self) -> WinwrightResult<ElementTarget> {
        match (&self.reference, self.locator.is_empty()) {
            (Some(r), true) => Ok(ElementTarget::by_ref(r.clone())),
            (None, false) => Ok(ElementTarget::by_locator(
                self.locator.locator(),
                self.scope.target(),
            )),
            (Some(_), false) => Err(WinwrightError::invalid(
                "give either a ref or locator flags, not both",
            )),
            (None, true) => Err(WinwrightError::invalid(
                "a target needs a ref (e14) or locator flags such as --role/--name/--label",
            )),
        }
    }

    pub fn optional_target(&self) -> WinwrightResult<Option<ElementTarget>> {
        if self.reference.is_none() && self.locator.is_empty() {
            Ok(None)
        } else {
            self.target().map(Some)
        }
    }
}

#[derive(Args)]
pub struct SnapshotArgs {
    #[command(flatten)]
    pub scope: ScopeArgs,
    /// Include named non-interactive containers too.
    #[arg(long)]
    pub all: bool,
    /// Keep every node, including layout containers (debugging).
    #[arg(long)]
    pub raw: bool,
    #[arg(long)]
    pub no_text: bool,
    #[arg(long)]
    pub bounds: bool,
    #[arg(long)]
    pub patterns: bool,
    #[arg(long)]
    pub offscreen: bool,
    #[arg(long, default_value_t = 12)]
    pub max_depth: u32,
    #[arg(long)]
    pub max_nodes: Option<u32>,
    #[arg(long, default_value_t = 20)]
    pub max_list_items: u32,
    /// Include the structured node tree in JSON output.
    #[arg(long)]
    pub structured: bool,
    /// Only report changes since the previous snapshot in the same engine session.
    #[arg(long)]
    pub diff: bool,
}

#[derive(Args)]
#[group(multiple = false)]
pub struct InspectArgs {
    /// Element under the mouse cursor (default).
    #[arg(long)]
    pub under_cursor: bool,
    /// Element with keyboard focus.
    #[arg(long)]
    pub focused: bool,
    /// Element at physical screen coordinates X,Y.
    #[arg(long, value_parser = parse_point)]
    pub at: Option<PhysicalPoint>,
}

#[derive(Args)]
pub struct FindArgs {
    #[command(flatten)]
    pub locator: LocatorArgs,
    #[command(flatten)]
    pub scope: ScopeArgs,
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

impl FindArgs {
    pub fn request(&self) -> FindRequest {
        let l = self.locator.locator();
        FindRequest {
            scope: self.scope.target(),
            role: l.role,
            name: l.name,
            text: l.text,
            automation_id: l.automation_id,
            class_name: l.class_name,
            framework_id: l.framework_id,
            label: l.label,
            ancestor: l.ancestor,
            exact: None,
            match_mode: Some(l.match_mode),
            case_sensitive: l.case_sensitive,
            visible_only: l.visible_only,
            nth: l.nth,
            limit: self.limit,
        }
    }
}

#[derive(Args)]
pub struct ClickArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    #[arg(long, conflicts_with = "middle")]
    pub right: bool,
    #[arg(long)]
    pub middle: bool,
    #[arg(long)]
    pub double: bool,
    /// Click with real mouse input even if a UIA pattern exists.
    #[arg(long)]
    pub physical: bool,
}

#[derive(Args)]
pub struct FillArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// Text to set.
    #[arg(long)]
    pub value: String,
    /// Append instead of replacing.
    #[arg(long)]
    pub append: bool,
}

#[derive(Args)]
pub struct TypeArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    #[arg(long)]
    pub value: String,
}

#[derive(Args)]
pub struct SelectArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// Item to select inside the target; omit to select the target itself.
    #[arg(long)]
    pub option: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl From<Direction> for ScrollDirection {
    fn from(d: Direction) -> Self {
        match d {
            Direction::Up => ScrollDirection::Up,
            Direction::Down => ScrollDirection::Down,
            Direction::Left => ScrollDirection::Left,
            Direction::Right => ScrollDirection::Right,
        }
    }
}

#[derive(Args)]
pub struct ScrollArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    #[arg(long, value_enum, default_value = "down")]
    pub direction: Direction,
    #[arg(long, default_value_t = 1)]
    pub amount: u32,
}

#[derive(Args)]
pub struct PressArgs {
    /// Key chord, e.g. Ctrl+Shift+S, Enter, Alt+F4.
    pub keys: String,
    #[command(flatten)]
    pub target: TargetArgs,
}

#[derive(Args)]
pub struct ReadArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    #[arg(long, default_value_t = 4000)]
    pub max_chars: u32,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum WindowOp {
    Focus,
    Minimize,
    Maximize,
    Restore,
    Close,
    Move,
    Resize,
}

#[derive(Args)]
pub struct WindowArgs {
    #[arg(value_enum)]
    pub op: WindowOp,
    #[command(flatten)]
    pub scope: ScopeArgs,
    #[arg(long, allow_hyphen_values = true)]
    pub x: Option<i32>,
    #[arg(long, allow_hyphen_values = true)]
    pub y: Option<i32>,
    #[arg(long)]
    pub width: Option<i32>,
    #[arg(long)]
    pub height: Option<i32>,
}

impl WindowArgs {
    pub fn action(&self) -> WinwrightResult<WindowAction> {
        let window = self.scope.selector();
        if window.is_empty() {
            return Err(WinwrightError::invalid(
                "name the window with --window, --process or --hwnd",
            ));
        }
        let need = |v: Option<i32>, flag: &str| {
            v.ok_or_else(|| WinwrightError::invalid(format!("{flag} is required")))
        };
        Ok(match self.op {
            WindowOp::Focus => WindowAction::Focus { window },
            WindowOp::Minimize => WindowAction::Minimize { window },
            WindowOp::Maximize => WindowAction::Maximize { window },
            WindowOp::Restore => WindowAction::Restore { window },
            WindowOp::Close => WindowAction::Close { window },
            WindowOp::Move => WindowAction::Move {
                window,
                x: need(self.x, "--x")?,
                y: need(self.y, "--y")?,
            },
            WindowOp::Resize => WindowAction::Resize {
                window,
                width: need(self.width, "--width")?,
                height: need(self.height, "--height")?,
            },
        })
    }
}

fn parse_u64(s: &str) -> Result<u64, String> {
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    };
    r.map_err(|e| e.to_string())
}

fn parse_point(s: &str) -> Result<PhysicalPoint, String> {
    let (x, y) = s.split_once(',').ok_or("expected X,Y")?;
    Ok(PhysicalPoint {
        x: x.trim().parse().map_err(|e| format!("{e}"))?,
        y: y.trim().parse().map_err(|e| format!("{e}"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn click_by_locator_builds_target() {
        let cli = Cli::try_parse_from([
            "winwright",
            "click",
            "--role",
            "Button",
            "--name",
            "Save",
            "--window",
            "Notepad",
        ])
        .unwrap();
        let Command::Click(c) = cli.command else {
            panic!()
        };
        let t = c.target.target().unwrap();
        assert!(t.reference.is_none());
        assert_eq!(t.locator.unwrap().name.as_deref(), Some("Save"));
        assert!(matches!(t.scope, SnapshotTarget::Window(_)));
    }

    #[test]
    fn ref_and_locator_conflict() {
        let cli = Cli::try_parse_from(["winwright", "focus", "e3", "--name", "x"]).unwrap();
        let Command::Focus(t) = cli.command else {
            panic!()
        };
        assert!(t.target().is_err());
    }

    #[test]
    fn press_takes_chord_then_optional_target() {
        let cli = Cli::try_parse_from(["winwright", "press", "Ctrl+Shift+S"]).unwrap();
        let Command::Press(p) = cli.command else {
            panic!()
        };
        assert_eq!(p.keys, "Ctrl+Shift+S");
        assert!(p.target.optional_target().unwrap().is_none());
    }

    #[test]
    fn window_move_accepts_negative_coordinates() {
        let cli = Cli::try_parse_from([
            "winwright",
            "window",
            "move",
            "--window",
            "Fixture",
            "--x",
            "-1500",
            "--y",
            "20",
        ])
        .unwrap();
        let Command::Window(w) = cli.command else {
            panic!()
        };
        assert!(matches!(
            w.action().unwrap(),
            WindowAction::Move {
                x: -1500,
                y: 20,
                ..
            }
        ));
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum WaitStateArg {
    Exists,
    Missing,
    Visible,
    Hidden,
    Enabled,
    Disabled,
    Focused,
    Value,
    Text,
    WindowOpen,
    WindowClosed,
}

#[derive(Args)]
pub struct WaitArgs {
    #[arg(value_enum)]
    pub state: WaitStateArg,
    /// Element ref/locator; for window-open/window-closed use --window/--process/--hwnd.
    #[command(flatten)]
    pub target: TargetArgs,
    /// Expected value/text for the `value` and `text` states.
    #[arg(long)]
    pub value: Option<String>,
    #[arg(long)]
    pub timeout_ms: Option<u64>,
}

impl WaitArgs {
    pub fn request(&self) -> WinwrightResult<winwright_contracts::wait::WaitRequest> {
        use winwright_contracts::wait::{WaitRequest, WaitState};
        let state = match self.state {
            WaitStateArg::Exists => WaitState::Exists,
            WaitStateArg::Missing => WaitState::Missing,
            WaitStateArg::Visible => WaitState::Visible,
            WaitStateArg::Hidden => WaitState::Hidden,
            WaitStateArg::Enabled => WaitState::Enabled,
            WaitStateArg::Disabled => WaitState::Disabled,
            WaitStateArg::Focused => WaitState::Focused,
            WaitStateArg::Value => WaitState::Value,
            WaitStateArg::Text => WaitState::Text,
            WaitStateArg::WindowOpen => WaitState::WindowOpen,
            WaitStateArg::WindowClosed => WaitState::WindowClosed,
        };
        let value_match = self.target.locator.match_mode();
        if state.is_window_state() {
            return Ok(WaitRequest {
                state,
                reference: None,
                locator: None,
                scope: SnapshotTarget::Active,
                window: Some(self.target.scope.selector()),
                value: None,
                value_match,
                timeout_ms: self.timeout_ms,
            });
        }
        let target = self.target.target()?;
        Ok(WaitRequest {
            state,
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

#[derive(Args)]
pub struct ScreenshotArgs {
    /// Output file (.png or .jpg).
    #[arg(long, default_value = "winwright-screenshot.png")]
    pub out: PathBuf,
    #[command(flatten)]
    pub scope: ScopeArgs,
    /// Monitor index (see --json output of a desktop capture for bounds).
    #[arg(long, conflicts_with_all = ["window", "process", "hwnd", "region", "desktop"])]
    pub monitor: Option<u32>,
    /// Physical desktop region X,Y,WIDTH,HEIGHT.
    #[arg(long, value_parser = parse_region, conflicts_with_all = ["window", "process", "hwnd", "desktop"])]
    pub region: Option<winwright_contracts::geometry::PhysicalRect>,
    /// Every monitor composed into one image.
    #[arg(long, conflicts_with_all = ["window", "process", "hwnd"])]
    pub desktop: bool,
    #[arg(long, default_value_t = 85)]
    pub quality: u8,
}

impl ScreenshotArgs {
    pub fn request(&self) -> winwright_contracts::capture::ScreenshotRequest {
        use winwright_contracts::capture::{ImageFormat, ScreenshotRequest, ScreenshotTarget};
        let selector = self.scope.selector();
        let target = if let Some(m) = self.monitor {
            ScreenshotTarget::Monitor(m)
        } else if let Some(r) = self.region {
            ScreenshotTarget::Region(r)
        } else if self.desktop {
            ScreenshotTarget::Desktop
        } else if !selector.is_empty() {
            ScreenshotTarget::Window(selector)
        } else {
            ScreenshotTarget::Active
        };
        let jpeg = self
            .out
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
        ScreenshotRequest {
            target,
            format: if jpeg {
                ImageFormat::Jpeg
            } else {
                ImageFormat::Png
            },
            quality: Some(self.quality),
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum OverlayStyleArg {
    Highlight,
    Arrow,
    ClickMarker,
}

#[derive(Args)]
pub struct HighlightArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    /// Text shown next to the highlight (e.g. "Click this").
    #[arg(long)]
    pub caption: Option<String>,
    #[arg(long, value_enum, default_value = "highlight")]
    pub style: OverlayStyleArg,
    /// How long to keep it on screen; this command waits that long before exiting.
    #[arg(long, default_value_t = 3000)]
    pub duration_ms: u64,
}

#[derive(Args)]
pub struct LaunchArgs {
    /// Executable, shell URI (ms-settings:display), or folder/file path.
    pub app: String,
    /// Arguments passed to an executable.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

fn parse_region(s: &str) -> Result<winwright_contracts::geometry::PhysicalRect, String> {
    let parts: Vec<i32> = s
        .split(',')
        .map(|p| p.trim().parse::<i32>().map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let [x, y, w, h] = parts[..] else {
        return Err("expected X,Y,WIDTH,HEIGHT".into());
    };
    if w <= 0 || h <= 0 {
        return Err("width and height must be positive".into());
    }
    Ok(winwright_contracts::geometry::PhysicalRect::new(
        x,
        y,
        x + w,
        y + h,
    ))
}
