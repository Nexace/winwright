use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use winwright_contracts::WinwrightError;
use winwright_contracts::config::Config;
use winwright_contracts::element::ElementDetails;
use winwright_contracts::geometry::PhysicalPoint;
use winwright_contracts::ids::SessionId;
use winwright_contracts::snapshot::{SnapshotRequest, SnapshotTarget};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_core::{Engine, InspectRequest};

#[derive(Parser)]
#[command(
    name = "winwright",
    version,
    about = "Semantic Windows desktop automation"
)]
struct Cli {
    /// Config file (default: %APPDATA%\winwright\config.json).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
}

#[derive(Args)]
struct SnapshotArgs {
    /// Window whose title contains this text (case-insensitive).
    #[arg(long)]
    window: Option<String>,
    /// Window owned by this executable (e.g. notepad or notepad.exe).
    #[arg(long)]
    process: Option<String>,
    /// Window handle (decimal or 0x-prefixed hex).
    #[arg(long, value_parser = parse_u64)]
    hwnd: Option<u64>,
    /// Snapshot every visible window.
    #[arg(long, conflicts_with_all = ["window", "process", "hwnd"])]
    all_windows: bool,
    /// Include named non-interactive containers too.
    #[arg(long)]
    all: bool,
    /// Keep every node, including layout containers (debugging).
    #[arg(long)]
    raw: bool,
    #[arg(long)]
    no_text: bool,
    #[arg(long)]
    bounds: bool,
    #[arg(long)]
    patterns: bool,
    #[arg(long)]
    offscreen: bool,
    #[arg(long, default_value_t = 12)]
    max_depth: u32,
    #[arg(long)]
    max_nodes: Option<u32>,
    #[arg(long, default_value_t = 20)]
    max_list_items: u32,
    /// Include the structured node tree in JSON output.
    #[arg(long)]
    structured: bool,
}

#[derive(Args)]
#[group(multiple = false)]
struct InspectArgs {
    /// Element under the mouse cursor (default).
    #[arg(long)]
    under_cursor: bool,
    /// Element with keyboard focus.
    #[arg(long)]
    focused: bool,
    /// Element at physical screen coordinates X,Y.
    #[arg(long, value_parser = parse_point)]
    at: Option<PhysicalPoint>,
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

fn init_logging() {
    // stderr only: stdout is reserved for command output and, later, MCP frames.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("WINWRIGHT_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
}

fn print_json<T: serde::Serialize>(value: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("DTOs serialize")
    );
}

fn build_engine(config: Config) -> Result<Engine, WinwrightError> {
    let uia = winwright_uia::UiaBackend::start()?;
    Ok(Engine::new(
        config,
        Arc::new(winwright_win32::Win32Windows),
        Arc::new(uia),
    ))
}

fn print_windows(windows: &[WindowInfo]) {
    for w in windows {
        let mut flags = String::new();
        if w.minimized {
            flags.push_str(" (minimized)");
        }
        if w.topmost {
            flags.push_str(" (topmost)");
        }
        println!(
            "{} {:#010x} {:>6} {:<24} {:?}{flags}",
            if w.foreground { '*' } else { ' ' },
            w.hwnd,
            w.process_id,
            w.process_name,
            w.title,
        );
    }
}

fn print_details(d: &ElementDetails) {
    let e = &d.element;
    println!("{}  [{}]", d.element.path, e.reference);
    let row = |k: &str, v: String| {
        if !v.is_empty() {
            println!("  {k:<18} {v}");
        }
    };
    row("Role", format!("{:?} ({})", e.role, d.control_type_id));
    row("Name", e.name.clone());
    row("AutomationId", e.automation_id.clone());
    row("ClassName", e.class_name.clone());
    row("FrameworkId", e.framework.clone());
    row("Process", format!("{} ({})", d.process_name, d.process_id));
    row(
        "Bounds",
        e.bounds
            .map(|b| format!("[{}, {}, {}, {}]", b.left, b.top, b.right, b.bottom))
            .unwrap_or_default(),
    );
    row(
        "State",
        format!(
            "enabled={} focused={} offscreen={} focusable={}",
            e.enabled, e.focused, d.offscreen, d.keyboard_focusable
        ),
    );
    row("Patterns", format!("{:?}", e.patterns));
    row("Value", e.value.clone().unwrap_or_default());
    row("RuntimeId", format!("{:?}", d.runtime_id));
    row("HelpText", d.help_text.clone());
}

async fn run(cli: Cli) -> Result<(), WinwrightError> {
    let config = winwright_core::config::load_config(cli.config.as_deref())?;
    match cli.command {
        Command::Version => {
            let info = serde_json::json!({
                "name": "winwright",
                "version": env!("CARGO_PKG_VERSION"),
                "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            });
            if cli.json {
                println!("{info}");
            } else {
                println!("winwright {}", env!("CARGO_PKG_VERSION"));
            }
        }
        Command::Config => print_json(&config),
        Command::Windows => {
            let engine = build_engine(config)?;
            let windows = engine.list_windows()?;
            if cli.json {
                print_json(&windows);
            } else {
                print_windows(&windows);
            }
        }
        Command::Snapshot(args) => {
            let max_nodes = args
                .max_nodes
                .unwrap_or(config.automation.max_snapshot_nodes);
            let selector = WindowSelector {
                title: args.window,
                process: args.process,
                hwnd: args.hwnd,
            };
            let request = SnapshotRequest {
                target: if args.all_windows {
                    SnapshotTarget::AllWindows
                } else if selector.is_empty() {
                    SnapshotTarget::Active
                } else {
                    SnapshotTarget::Window(selector)
                },
                interactive_only: !args.all,
                include_text: !args.no_text,
                include_bounds: args.bounds,
                include_patterns: args.patterns,
                max_depth: args.max_depth,
                max_nodes,
                include_offscreen: args.offscreen,
                max_list_items: args.max_list_items,
                raw_debug: args.raw,
                structured: args.structured,
            };
            let engine = build_engine(config)?;
            let session = engine.session(&cli_session(), "cli")?;
            let snapshot = engine.snapshot(&session, request).await?;
            if cli.json {
                print_json(&snapshot);
            } else {
                print!("{}", snapshot.tree);
                for w in &snapshot.warnings {
                    eprintln!("warning: {w}");
                }
            }
        }
        Command::Inspect(args) => {
            let request = match (args.focused, args.at) {
                (true, _) => InspectRequest::Focused,
                (_, Some(p)) => InspectRequest::Point(p),
                _ => InspectRequest::UnderCursor,
            };
            let engine = build_engine(config)?;
            let session = engine.session(&cli_session(), "cli")?;
            let details = engine.inspect(&session, request).await?;
            if cli.json {
                print_json(&details);
            } else {
                print_details(&details);
            }
        }
    }
    Ok(())
}

/// One-shot CLI commands use a transient session; refs do not outlive the process until
/// `winwright serve` provides a persistent per-user engine.
fn cli_session() -> SessionId {
    SessionId::parse("cli").expect("valid id")
}

fn main() -> ExitCode {
    init_logging();
    // Must precede any HWND creation so every coordinate is physical pixels.
    winwright_win32::enable_per_monitor_dpi_awareness();
    let cli = Cli::parse();
    let json = cli.json;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("tokio runtime");
    match runtime.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&err.payload()).expect("payload serializes")
                );
            } else {
                eprintln!("error [{}]: {err}", err.code().as_str());
                if let Some(hint) = err.hint() {
                    eprintln!("hint: {hint}");
                }
            }
            ExitCode::FAILURE
        }
    }
}
