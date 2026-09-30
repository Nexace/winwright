mod args;

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use winwright_contracts::WinwrightError;
use winwright_contracts::action::{ActionResult, DesktopAction};
use winwright_contracts::config::Config;
use winwright_contracts::element::ElementDetails;
use winwright_contracts::ids::SessionId;
use winwright_contracts::input::{MouseButton, parse_chord};
use winwright_contracts::locator::FindResult;
use winwright_contracts::snapshot::SnapshotRequest;
use winwright_contracts::window::WindowInfo;
use winwright_core::{Engine, InspectRequest};

use crate::args::{Cli, Command};

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

/// Optional backends, started only for the commands that need them.
#[derive(Clone, Copy, Default)]
struct Extras {
    capture: bool,
    overlay: bool,
    system: bool,
}

fn build_engine(config: Config, extras: Extras) -> Result<Engine, WinwrightError> {
    let uia = winwright_uia::UiaBackend::start()?;
    let mut engine = Engine::new(
        config,
        Arc::new(winwright_win32::Win32Windows),
        Arc::new(uia),
    )
    .with_input(Arc::new(winwright_input::SendInputBackend::new()));
    if extras.capture {
        engine = engine.with_capture(Arc::new(winwright_capture::WgcCapture::start()?));
    }
    if extras.overlay {
        engine = engine.with_overlay(Arc::new(winwright_overlay::NativeOverlay::start()?));
    }
    if extras.system {
        engine = engine
            .with_processes(Arc::new(winwright_shell::SystemProcesses::new()))
            .with_files(Arc::new(winwright_files::LocalFiles::new()));
    }
    Ok(engine)
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

fn print_found(found: &FindResult) {
    for m in &found.matches {
        let mut line = format!("{:<5} {}", m.reference, m.path);
        if !m.automation_id.is_empty() {
            line.push_str(&format!("  id={:?}", m.automation_id));
        }
        if !m.enabled {
            line.push_str("  disabled");
        }
        println!("{line}");
    }
    if found.count as usize > found.matches.len() {
        println!("... {} more", found.count as usize - found.matches.len());
    }
    if found.count == 0 {
        eprintln!("no matches");
    }
    for w in &found.warnings {
        eprintln!("warning: {w}");
    }
}

fn print_action(r: &ActionResult) {
    let mut line = format!(
        "{} {:?} {}",
        if r.verified { "ok" } else { "done" },
        r.method,
        r.target
    );
    if let Some(reference) = &r.reference {
        line.push_str(&format!(" [{reference}]"));
    }
    line.push_str(&format!(
        " {} {} ms",
        if r.verified { "verified" } else { "unverified" },
        r.duration_ms
    ));
    println!("{line}");
    if let Some(after) = &r.after {
        println!("  after: {after}");
    }
    for w in &r.opened_windows {
        println!("  opened: {w}");
    }
    for w in &r.closed_windows {
        println!("  closed: {w}");
    }
    if let Some(text) = &r.text {
        println!("{text}");
    }
    for w in &r.warnings {
        eprintln!("warning: {w}");
    }
}

/// One-shot CLI commands use a transient session; refs do not outlive the process until
/// `winwright serve` provides a persistent per-user engine.
fn cli_session() -> SessionId {
    SessionId::parse("cli").expect("valid id")
}

async fn act(engine: &Engine, action: DesktopAction, json: bool) -> Result<(), WinwrightError> {
    let session = engine.session(&cli_session(), "cli")?;
    let result = engine.execute(&session, action).await?;
    if json {
        print_json(&result);
    } else {
        print_action(&result);
    }
    Ok(())
}

async fn run(cli: Cli) -> Result<(), WinwrightError> {
    let config = winwright_core::config::load_config(cli.config.as_deref())?;
    let json = cli.json;
    let command = match cli.command {
        Command::Version => {
            let info = serde_json::json!({
                "name": "winwright",
                "version": env!("CARGO_PKG_VERSION"),
                "target": format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
            });
            if json {
                println!("{info}");
            } else {
                println!("winwright {}", env!("CARGO_PKG_VERSION"));
            }
            return Ok(());
        }
        Command::Config => {
            print_json(&config);
            return Ok(());
        }
        other => other,
    };

    let max_nodes = config.automation.max_snapshot_nodes;
    let extras = Extras {
        capture: matches!(command, Command::Screenshot(_)),
        overlay: matches!(command, Command::Highlight(_)),
        system: matches!(command, Command::Launch(_) | Command::Processes),
    };
    let engine = build_engine(config, extras)?;
    match command {
        Command::Version | Command::Config => unreachable!("handled above"),
        Command::Windows => {
            let windows = engine.list_windows()?;
            if json {
                print_json(&windows);
            } else {
                print_windows(&windows);
            }
        }
        Command::Snapshot(a) => {
            let request = SnapshotRequest {
                target: a.scope.target(),
                interactive_only: !a.all,
                include_text: !a.no_text,
                include_bounds: a.bounds,
                include_patterns: a.patterns,
                max_depth: a.max_depth,
                max_nodes: a.max_nodes.unwrap_or(max_nodes),
                include_offscreen: a.offscreen,
                max_list_items: a.max_list_items,
                raw_debug: a.raw,
                structured: a.structured,
                diff: a.diff,
            };
            let session = engine.session(&cli_session(), "cli")?;
            let snapshot = engine.snapshot(&session, request).await?;
            if json {
                print_json(&snapshot);
            } else {
                print!("{}", snapshot.tree);
                if let Some(diff) = &snapshot.diff {
                    print!("{diff}");
                }
                for w in &snapshot.warnings {
                    eprintln!("warning: {w}");
                }
            }
        }
        Command::Inspect(a) => {
            let request = match (a.focused, a.at) {
                (true, _) => InspectRequest::Focused,
                (_, Some(p)) => InspectRequest::Point(p),
                _ => InspectRequest::UnderCursor,
            };
            let session = engine.session(&cli_session(), "cli")?;
            let details = engine.inspect(&session, request).await?;
            if json {
                print_json(&details);
            } else {
                print_details(&details);
            }
        }
        Command::Find(a) => {
            let session = engine.session(&cli_session(), "cli")?;
            let found = engine.find(&session, a.request()).await?;
            if json {
                print_json(&found);
            } else {
                print_found(&found);
            }
        }
        Command::Click(a) => {
            let button = if a.right {
                MouseButton::Right
            } else if a.middle {
                MouseButton::Middle
            } else {
                MouseButton::Left
            };
            let action = DesktopAction::Click {
                target: a.target.target()?,
                button,
                click_count: if a.double { 2 } else { 1 },
                force_physical: a.physical,
            };
            act(&engine, action, json).await?;
        }
        Command::Fill(a) => {
            let action = DesktopAction::Fill {
                target: a.target.target()?,
                text: a.value,
                clear: !a.append,
            };
            act(&engine, action, json).await?;
        }
        Command::Type(a) => {
            let action = DesktopAction::TypeText {
                target: a.target.optional_target()?,
                text: a.value,
            };
            act(&engine, action, json).await?;
        }
        Command::Focus(t) => {
            act(
                &engine,
                DesktopAction::Focus {
                    target: t.target()?,
                },
                json,
            )
            .await?
        }
        Command::Check(t) => {
            act(
                &engine,
                DesktopAction::Check {
                    target: t.target()?,
                },
                json,
            )
            .await?
        }
        Command::Uncheck(t) => {
            act(
                &engine,
                DesktopAction::Uncheck {
                    target: t.target()?,
                },
                json,
            )
            .await?
        }
        Command::Toggle(t) => {
            act(
                &engine,
                DesktopAction::Toggle {
                    target: t.target()?,
                },
                json,
            )
            .await?
        }
        Command::Expand(t) => {
            act(
                &engine,
                DesktopAction::Expand {
                    target: t.target()?,
                },
                json,
            )
            .await?
        }
        Command::Collapse(t) => {
            act(
                &engine,
                DesktopAction::Collapse {
                    target: t.target()?,
                },
                json,
            )
            .await?
        }
        Command::Select(a) => {
            let action = DesktopAction::Select {
                target: a.target.target()?,
                option: a.option,
            };
            act(&engine, action, json).await?;
        }
        Command::Scroll(a) => {
            let action = DesktopAction::Scroll {
                target: a.target.target()?,
                direction: a.direction.into(),
                amount: a.amount,
            };
            act(&engine, action, json).await?;
        }
        Command::Press(a) => {
            let action = DesktopAction::Press {
                target: a.target.optional_target()?,
                keys: parse_chord(&a.keys).map_err(WinwrightError::invalid)?,
            };
            act(&engine, action, json).await?;
        }
        Command::Read(a) => {
            let action = DesktopAction::ReadText {
                target: a.target.target()?,
                max_chars: a.max_chars,
            };
            act(&engine, action, json).await?;
        }
        Command::Window(a) => {
            let session = engine.session(&cli_session(), "cli")?;
            let result = engine.window_action(&session, a.action()?).await?;
            if json {
                print_json(&result);
            } else {
                print_action(&result);
            }
        }
        Command::Wait(a) => {
            let session = engine.session(&cli_session(), "cli")?;
            let result = engine.wait_for(&session, a.request()?).await?;
            if json {
                print_json(&result);
            } else {
                let what = result
                    .element
                    .as_ref()
                    .map(|e| format!(" {:?} {:?} [{}]", e.role, e.name, e.reference))
                    .or_else(|| {
                        result
                            .window
                            .as_ref()
                            .map(|w| format!(" window {:?}", w.title))
                    })
                    .unwrap_or_default();
                println!(
                    "ok {:?}{what} after {} ms ({} checks, {} events)",
                    result.state, result.elapsed_ms, result.checks, result.events
                );
            }
        }
        Command::Screenshot(a) => {
            let session = engine.session(&cli_session(), "cli")?;
            let image = engine.screenshot(&session, a.request()).await?;
            std::fs::write(&a.out, &image.bytes).map_err(|e| {
                WinwrightError::invalid(format!("cannot write {}: {e}", a.out.display()))
            })?;
            let info = serde_json::json!({
                "path": a.out.display().to_string(),
                "width": image.width,
                "height": image.height,
                "origin": [image.origin.x, image.origin.y],
                "dpi": image.dpi,
                "bytes": image.bytes.len(),
            });
            if json {
                print_json(&info);
            } else {
                println!(
                    "saved {} ({}x{} at {},{}; {} dpi)",
                    a.out.display(),
                    image.width,
                    image.height,
                    image.origin.x,
                    image.origin.y,
                    image.dpi
                );
            }
        }
        Command::Highlight(a) => {
            use winwright_contracts::overlay::{HighlightRequest, OverlayStyle};
            let session = engine.session(&cli_session(), "cli")?;
            let result = engine
                .highlight(
                    &session,
                    HighlightRequest {
                        target: a.target.target()?,
                        style: match a.style {
                            args::OverlayStyleArg::Highlight => OverlayStyle::Highlight,
                            args::OverlayStyleArg::Arrow => OverlayStyle::Arrow,
                            args::OverlayStyleArg::ClickMarker => OverlayStyle::ClickMarker,
                        },
                        label: a.caption,
                        step: None,
                        color: None,
                        duration_ms: Some(a.duration_ms.max(1)),
                    },
                )
                .await?;
            if json {
                print_json(&result);
            } else {
                println!("highlighting {} [{}]", result.target, result.reference);
            }
            // Overlays live on this process's UI thread: stay alive while one is shown.
            tokio::time::sleep(std::time::Duration::from_millis(a.duration_ms)).await;
            engine.clear_overlays(None)?;
        }
        Command::Launch(a) => {
            let session = engine.session(&cli_session(), "cli")?;
            let result = engine
                .launch_app(
                    &session,
                    winwright_contracts::system::LaunchRequest {
                        app: a.app,
                        args: a.args,
                        working_dir: None,
                    },
                )
                .await?;
            if json {
                print_json(&result);
            } else {
                match result.process_id {
                    Some(pid) => println!("launched ({}) pid {pid}", result.method),
                    None => println!("launched ({})", result.method),
                }
            }
        }
        Command::Processes => {
            let list = engine.process_list()?;
            if json {
                print_json(&list);
            } else {
                for p in &list {
                    println!(
                        "{:>6} {:<8} {}",
                        p.process_id,
                        p.integrity.as_deref().unwrap_or("-"),
                        p.name
                    );
                }
            }
        }
    }
    Ok(())
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
                for m in err.payload().matches {
                    eprintln!("  candidate: {m}");
                }
            }
            ExitCode::FAILURE
        }
    }
}
