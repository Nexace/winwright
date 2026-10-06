//! What the tray menu's everyday items do: Settings (the person's own click only; no MCP tool
//! can change these).

use std::path::PathBuf;
use std::sync::OnceLock;

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

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::config::ThemeChoice;

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
