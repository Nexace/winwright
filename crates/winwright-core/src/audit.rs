//! Local audit log (spec §27): one JSON line per state-changing call, never typed text or
//! field values. Size-capped with a single rotated backup so it cannot grow unbounded.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use winwright_contracts::action::ActionMethod;
use winwright_contracts::security::TargetSummary;

/// Rotate after this many bytes; one `.1` backup is kept (4 MB worst case on disk).
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent<'a> {
    /// Unix epoch milliseconds.
    pub timestamp_ms: u64,
    pub session: &'a str,
    pub tool: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<&'a TargetSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<ActionMethod>,
    /// `ok` or a stable error code such as `CONFIRMATION_REQUIRED`.
    pub result: &'a str,
    /// The user approved this action in a confirmation dialog.
    pub confirmation: bool,
    pub duration_ms: u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct AuditLog {
    path: PathBuf,
    max_bytes: u64,
    /// Serializes this process's writes. The file is opened per event, never held open,
    /// because other Winwright processes (a second MCP server, CLI calls) append to it and
    /// rotate it too; a held handle would keep writing into the rotated backup.
    lock: Mutex<()>,
}

impl AuditLog {
    /// `%LOCALAPPDATA%\winwright\audit.jsonl`.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("winwright").join("audit.jsonl"))
    }

    /// Opens lazily on the first write; nothing is created until something is recorded.
    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes: max_bytes.max(4 * 1024),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn backup(path: &Path) -> PathBuf {
        path.with_extension("1.jsonl")
    }

    /// Never fails the caller: audit problems are logged, the action result stands.
    pub fn record(&self, event: &AuditEvent) {
        let Ok(mut line) = serde_json::to_string(event) else {
            return;
        };
        line.push('\n');
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(err) = self.write(line.as_bytes()) {
            tracing::warn!(%err, path = %self.path.display(), "audit write failed");
        }
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size + bytes.len() as u64 > self.max_bytes && size > 0 {
            match std::fs::rename(&self.path, Self::backup(&self.path)) {
                // Another process rotated it first.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            }
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(bytes)?;
        file.flush()
    }

    /// Last `n` events, oldest first (spans the rotated backup when needed).
    pub fn tail(path: &Path, n: usize) -> std::io::Result<Vec<String>> {
        let mut lines = Vec::new();
        for p in [Self::backup(path), path.to_path_buf()] {
            match std::fs::read_to_string(&p) {
                Ok(text) => lines.extend(text.lines().map(str::to_owned)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let skip = lines.len().saturating_sub(n);
        Ok(lines.split_off(skip))
    }

    /// Deletes the log and its backup (user-initiated).
    pub fn clear(path: &Path) -> std::io::Result<()> {
        for p in [path.to_path_buf(), Self::backup(path)] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(tool: &str) -> AuditEvent<'_> {
        AuditEvent {
            timestamp_ms: 1,
            session: "s",
            tool,
            target: None,
            method: Some(ActionMethod::InvokePattern),
            result: "ok",
            confirmation: false,
            duration_ms: 3,
        }
    }

    #[test]
    fn writes_rotates_tails_and_clears() {
        let dir = crate::scratch_dir("audit");
        let path = dir.join("audit.jsonl");
        let log = AuditLog::new(path.clone(), 4 * 1024);
        for i in 0..200 {
            log.record(&event(if i % 2 == 0 {
                "desktop_click"
            } else {
                "desktop_fill"
            }));
        }
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size <= 4 * 1024, "active file stays under the cap: {size}");
        assert!(
            AuditLog::backup(&path).exists(),
            "rotation keeps one backup"
        );
        let tail = AuditLog::tail(&path, 3).unwrap();
        assert_eq!(tail.len(), 3);
        assert!(tail[2].contains("\"tool\":\"desktop_fill\""));
        assert!(!tail[0].contains("text"), "no free text is ever recorded");
        AuditLog::clear(&path).unwrap();
        assert!(!path.exists() && !AuditLog::backup(&path).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writers_sharing_one_log_rotate_together() {
        let dir = crate::scratch_dir("audit-shared");
        let path = dir.join("audit.jsonl");
        let cap = 4 * 1024;
        // An MCP server and a CLI call (or two MCP servers) append to the same file.
        let writers = [
            AuditLog::new(path.clone(), cap),
            AuditLog::new(path.clone(), cap),
        ];
        for i in 0..400u64 {
            writers[(i % 2) as usize].record(&AuditEvent {
                duration_ms: i,
                ..event("desktop_click")
            });
        }
        for p in [path.clone(), AuditLog::backup(&path)] {
            let size = std::fs::metadata(&p).unwrap().len();
            assert!(size <= cap, "{} stays under the cap: {size}", p.display());
        }
        let tail = AuditLog::tail(&path, 2).unwrap();
        assert!(
            tail[0].contains("\"durationMs\":398") && tail[1].contains("\"durationMs\":399"),
            "both writers' latest events are kept: {tail:?}"
        );
        drop(writers);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
