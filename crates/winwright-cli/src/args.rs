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

    fn match_mode(&self) -> MatchMode {
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
