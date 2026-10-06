//! What the tray menu's everyday items do: Settings (the person's own click only; no MCP tool
//! can change these) and Check this PC.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use winwright_contracts::WinwrightError;
use winwright_contracts::config::Config;
use winwright_overlay::{Settings, TrayHost};

/// The config file this process read (`--config`, else the default); Settings saves to it.
static CONFIG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn remember_config_path(explicit: Option<PathBuf>) {
    let _ = CONFIG_PATH.set(explicit.or_else(winwright_core::config::default_config_path));
}

fn config_path() -> Option<PathBuf> {
    CONFIG_PATH.get().cloned().flatten()
}

/// The config as the file says now (it may have changed since this process started).
fn current_config(path: &std::path::Path) -> Result<Config, WinwrightError> {
    if path.exists() {
        winwright_core::config::load_config(Some(path))
    } else {
        Ok(Config::default())
    }
}

fn message(err: WinwrightError) -> String {
    match err {
        WinwrightError::InvalidRequest { message } => message,
        other => other.to_string(),
    }
}

fn settings_of(config: &Config) -> Settings {
    Settings {
        theme: config.overlay.theme,
        confirmation_timeout_seconds: config.security.confirmation_timeout_seconds,
        allow_for_a_while: config.security.allow_for_a_while,
        audit: config.security.audit,
        task_done: config.notifications.task_done,
        update_check: config.updates.check,
    }
}

fn edits(s: &Settings) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("overlay.theme", json!(s.theme)),
        (
            "security.confirmationTimeoutSeconds",
            json!(s.confirmation_timeout_seconds),
        ),
        ("security.allowForAWhile", json!(s.allow_for_a_while)),
        ("security.audit", json!(s.audit)),
        ("notifications.taskDone", json!(s.task_done)),
        ("updates.check", json!(s.update_check)),
    ]
}

/// Opens the Settings window with what the config file says now. The theme applies to this
/// process's windows at once; everything else when the AI apps next start Winwright.
pub fn open_settings(tray: &TrayHost) {
    let Some(path) = config_path() else {
        tray.notify(
            "Settings unavailable",
            "Windows did not say where APPDATA is.",
        );
        return;
    };
    let config = match current_config(&path) {
        Ok(config) => config,
        Err(err) => {
            tray.notify("Settings could not open", &message(err));
            return;
        }
    };
    winwright_overlay::open_settings(
        settings_of(&config),
        Box::new(move |s| {
            winwright_core::config::save_settings(&path, &edits(s)).map_err(message)?;
            winwright_overlay::theme::set_choice(s.theme);
            Ok(())
        }),
    );
}

/// One check at a time.
static CHECKING: AtomicBool = AtomicBool::new(false);

/// Runs `winwright doctor` as a child process without a window, keeps what it printed in a
/// temp text file and opens that in Notepad. Doctor draws a small box on each monitor for a
/// moment: the notification says so.
pub fn check_this_pc(tray: &TrayHost) {
    if CHECKING.swap(true, Ordering::SeqCst) {
        tray.notify(
            "Already checking this PC",
            "The result opens in Notepad when it is done.",
        );
        return;
    }
    tray.notify(
        "Checking this PC",
        "Winwright shows a small box on each screen for a moment. The result opens in Notepad.",
    );
    let tray = tray.clone();
    let spawned = std::thread::Builder::new()
        .name("winwright-check".into())
        .spawn(move || {
            let report = run_doctor();
            CHECKING.store(false, Ordering::SeqCst);
            let file = std::env::temp_dir().join("winwright-check.txt");
            match std::fs::write(&file, report) {
                Ok(()) => crate::spawn_detached(Command::new("notepad.exe").arg(&file)),
                Err(err) => tray.notify("Check this PC", &format!("Cannot save the result: {err}")),
            }
        });
    if spawned.is_err() {
        CHECKING.store(false, Ordering::SeqCst);
    }
}

fn run_doctor() -> String {
    /// No console window for the child (Winwright itself may have none).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::env::current_exe().and_then(|exe| {
        let mut command = Command::new(exe);
        if let Some(path) = config_path().filter(|p| p.exists()) {
            command.arg("--config").arg(path);
        }
        command
            .arg("doctor")
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    });
    match output {
        Ok(out) => doctor_report(
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
            out.status.success(),
        ),
        Err(err) => format!("Winwright could not run its check: {err}\n"),
    }
}

/// What Notepad shows after a check: doctor's own lines, then a verdict.
fn doctor_report(stdout: &str, stderr: &str, passed: bool) -> String {
    let mut text = format!(
        "Winwright {}: Check this PC\n\n{}\n",
        env!("CARGO_PKG_VERSION"),
        stdout.trim_end()
    );
    if !stderr.trim().is_empty() {
        text.push_str(&format!("\nMessages:\n{}\n", stderr.trim_end()));
    }
    text.push_str(if passed {
        "\nEvery check passed.\n"
    } else {
        "\nSome checks did not pass: see the FAILED lines above.\n"
    });
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::config::ThemeChoice;

    #[test]
    fn doctor_output_is_kept_with_a_verdict() {
        let ok = doctor_report("  ok      UI Automation: 12 windows\n", "", true);
        assert!(ok.starts_with("Winwright ") && ok.contains("Check this PC"));
        assert!(ok.contains("  ok      UI Automation: 12 windows\n"));
        assert!(ok.ends_with("Every check passed.\n") && !ok.contains("Messages"));
        let failed = doctor_report(
            "  FAILED  monitor 2: overlay not seen\n",
            "warning: screen busy\n",
            false,
        );
        assert!(failed.contains("FAILED  monitor 2"));
        assert!(failed.contains("Messages:\nwarning: screen busy\n"));
        assert!(failed.ends_with("see the FAILED lines above.\n"));
    }

    #[test]
    fn settings_round_trip_through_the_config_file() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch")
            .join(format!("cli-settings-{}", std::process::id()));
        let path = dir.join("config.json");
        assert_eq!(
            settings_of(&current_config(&path).unwrap()),
            settings_of(&Config::default()),
            "no file yet: the defaults"
        );
        let wanted = Settings {
            theme: ThemeChoice::Light,
            confirmation_timeout_seconds: 120,
            allow_for_a_while: false,
            audit: false,
            task_done: false,
            update_check: true,
        };
        winwright_core::config::save_settings(&path, &edits(&wanted)).unwrap();
        assert_eq!(settings_of(&current_config(&path).unwrap()), wanted);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
