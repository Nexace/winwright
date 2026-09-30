use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use winwright_contracts::WinwrightError;

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

fn run(cli: Cli) -> Result<(), WinwrightError> {
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
        Command::Config => {
            println!(
                "{}",
                serde_json::to_string_pretty(&config).expect("config serializes")
            );
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    init_logging();
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
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
