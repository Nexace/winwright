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
    serde_json::from_str(&text)
        .map_err(|e| WinwrightError::invalid(format!("invalid config {}: {e}", path.display())))
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
        let dir = std::env::temp_dir().join(format!("winwright-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"automation":{"referenceTtlSeconds":5}}"#).unwrap();
        let cfg = load_config(Some(&path)).unwrap();
        assert_eq!(cfg.automation.reference_ttl_seconds, 5);
        std::fs::write(&path, r#"{"automation":{"bogus":1}}"#).unwrap();
        assert!(load_config(Some(&path)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
