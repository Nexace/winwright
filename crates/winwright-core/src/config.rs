use std::path::{Path, PathBuf};

use winwright_contracts::config::Config;
use winwright_contracts::{WinwrightError, WinwrightResult};

/// `%APPDATA%\winwright\config.json`.
pub fn default_config_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("winwright").join("config.json"))
}

/// Loads an explicit path (must exist), else the default path if present, else defaults.
pub fn load_config(explicit: Option<&Path>) -> WinwrightResult<Config> {
    let (path, required) = match explicit {
        Some(p) => (p.to_path_buf(), true),
        None => match default_config_path() {
            Some(p) => (p, false),
            None => return Ok(Config::default()),
        },
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(e) => {
            return Err(WinwrightError::invalid(format!(
                "cannot read config {}: {e}",
                path.display()
            )));
        }
    };
    let config: Config = serde_json::from_str(&text)
        .map_err(|e| WinwrightError::invalid(format!("invalid config {}: {e}", path.display())))?;
    check_values(&config)
        .map_err(|e| WinwrightError::invalid(format!("invalid config {}: {e}", path.display())))?;
    Ok(config)
}

/// Writes `edits` (dotted camelCase keys such as `security.audit`) into the config file at
/// `path`, keeping every other key as it is. Refuses a result Winwright could not load, and
/// replaces the file in one rename, so a crash never leaves half a file.
pub fn save_settings(path: &Path, edits: &[(&str, serde_json::Value)]) -> WinwrightResult<()> {
    let fail = |what: String| WinwrightError::invalid(format!("{what} ({})", path.display()));
    let mut root = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| fail(format!("the config file is not valid JSON: {e}")))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(fail(format!("cannot read the config file: {e}"))),
    };
    for (key, value) in edits {
        let mut node = &mut root;
        for part in key.split('.') {
            let serde_json::Value::Object(map) = node else {
                return Err(fail(format!(
                    "{key} is inside a value that is not a section"
                )));
            };
            node = map.entry(part).or_insert_with(|| serde_json::json!({}));
        }
        *node = value.clone();
    }
    let config: Config = serde_json::from_value(root.clone())
        .map_err(|e| fail(format!("the settings would not load: {e}")))?;
    check_values(&config).map_err(fail)?;
    let mut text = serde_json::to_string_pretty(&root).expect("JSON values serialize");
    text.push('\n');
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| fail(format!("cannot create the folder: {e}")))?;
    }
    std::fs::write(&temp, text).map_err(|e| fail(format!("cannot write the settings: {e}")))?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        fail(format!("cannot replace the config file: {e}"))
    })
}

/// Values the format accepts but Winwright cannot work with: a 0 ms timeout fails every call,
/// a 0 s reference lifetime expires every ref at once, and the server only ever listens on
/// loopback.
fn check_values(c: &Config) -> Result<(), String> {
    let ranges = [
        (
            "automation.defaultTimeoutMs",
            c.automation.default_timeout_ms,
            100..=600_000,
        ),
        (
            "automation.referenceTtlSeconds",
            c.automation.reference_ttl_seconds,
            1..=3_600,
        ),
        (
            "automation.maxSnapshotNodes",
            u64::from(c.automation.max_snapshot_nodes),
            10..=20_000,
        ),
        (
            "security.confirmationTimeoutSeconds",
            c.security.confirmation_timeout_seconds,
            5..=600,
        ),
    ];
    for (name, value, range) in ranges {
        if !range.contains(&value) {
            return Err(format!(
                "{name} is {value}; it must be between {} and {}",
                range.start(),
                range.end()
            ));
        }
    }
    if c.server.http_port == 0 {
        return Err("server.httpPort must not be 0".into());
    }
    if !matches!(
        c.server.http_host.as_str(),
        "127.0.0.1" | "::1" | "localhost"
    ) {
        return Err(format!(
            "server.httpHost is {:?}; only loopback (127.0.0.1, ::1, localhost) is allowed",
            c.server.http_host
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_missing_file_is_an_error() {
        let err =
            load_config(Some(Path::new(r"C:\definitely\missing\winwright.json"))).unwrap_err();
        assert_eq!(err.code().as_str(), "INVALID_REQUEST");
    }

    #[test]
    fn explicit_file_is_parsed() {
        let dir = crate::scratch_dir("cfg");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"automation":{"referenceTtlSeconds":5}}"#).unwrap();
        let cfg = load_config(Some(&path)).unwrap();
        assert_eq!(cfg.automation.reference_ttl_seconds, 5);
        std::fs::write(&path, r#"{"automation":{"bogus":1}}"#).unwrap();
        assert!(load_config(Some(&path)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_are_saved_beside_every_other_key() {
        use serde_json::json;
        let dir = crate::scratch_dir("cfg-save");
        let path = dir.join("winwright").join("config.json");
        // A missing file (and folder) is created.
        save_settings(&path, &[("updates.check", json!(true))]).unwrap();
        assert!(load_config(Some(&path)).unwrap().updates.check);
        std::fs::write(
            &path,
            r#"{"security":{"allowShell":true,"confirmationMode":"relaxed"},"automation":{"maxSnapshotNodes":800}}"#,
        )
        .unwrap();
        save_settings(
            &path,
            &[
                ("security.confirmationTimeoutSeconds", json!(90)),
                ("security.allowForAWhile", json!(false)),
                ("overlay.theme", json!("dark")),
                ("notifications.taskDone", json!(false)),
            ],
        )
        .unwrap();
        let cfg = load_config(Some(&path)).unwrap();
        assert!(
            cfg.security.allow_shell,
            "keys the window does not show are kept"
        );
        assert_eq!(cfg.automation.max_snapshot_nodes, 800);
        assert_eq!(cfg.security.confirmation_timeout_seconds, 90);
        assert!(!cfg.security.allow_for_a_while && !cfg.notifications.task_done);
        assert_eq!(
            cfg.overlay.theme,
            winwright_contracts::config::ThemeChoice::Dark
        );
        assert!(!dir.join("winwright").join("config.json.tmp").exists());
        // A value Winwright could not load is refused and the file stays as it was.
        let before = std::fs::read_to_string(&path).unwrap();
        let err =
            save_settings(&path, &[("security.confirmationTimeoutSeconds", json!(1))]).unwrap_err();
        assert!(
            err.to_string().contains("confirmationTimeoutSeconds"),
            "{err}"
        );
        assert!(save_settings(&path, &[("security.audit", json!("yes"))]).is_err());
        assert!(save_settings(&path, &[("security.audit.deep", json!(true))]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        std::fs::write(&path, "{ not json").unwrap();
        assert!(save_settings(&path, &[("updates.check", json!(true))]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unusable_values_are_rejected_with_the_allowed_range() {
        assert_eq!(check_values(&Config::default()), Ok(()));
        let dir = crate::scratch_dir("cfg-values");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        for (json, field) in [
            (
                r#"{"automation":{"defaultTimeoutMs":0}}"#,
                "defaultTimeoutMs",
            ),
            (
                r#"{"automation":{"referenceTtlSeconds":0}}"#,
                "referenceTtlSeconds",
            ),
            (
                r#"{"automation":{"maxSnapshotNodes":1000000}}"#,
                "maxSnapshotNodes",
            ),
            (
                r#"{"security":{"confirmationTimeoutSeconds":1}}"#,
                "confirmationTimeoutSeconds",
            ),
            (r#"{"server":{"httpHost":"0.0.0.0"}}"#, "httpHost"),
        ] {
            std::fs::write(&path, json).unwrap();
            let err = load_config(Some(&path)).unwrap_err();
            assert!(err.to_string().contains(field), "{json}: {err}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
