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
