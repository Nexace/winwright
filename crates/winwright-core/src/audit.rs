//! Local audit log (spec §27): one JSON line per state-changing call, never typed text or
//! field values. Size-capped with a single rotated backup so it cannot grow unbounded.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
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

/// One logged event, as far as the readable list needs it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Logged {
    timestamp_ms: u64,
    tool: String,
    #[serde(default)]
    target: Option<TargetSummary>,
    result: String,
    #[serde(default)]
    confirmation: bool,
}

/// What a person calls each tool, and whether the element's name may follow (never for typing,
/// keys or scrolling, so nothing that could be content shows up beside a verb).
const VERBS: &[(&str, &str, bool)] = &[
    ("desktop_click", "clicked", true),
    ("desktop_clickAt", "clicked", true),
    ("desktop_fill", "typed text", false),
    ("desktop_typeText", "typed text", false),
    ("desktop_press", "pressed keys", false),
    ("desktop_focus", "focused", true),
    ("desktop_select", "selected", true),
    ("desktop_check", "checked", true),
    ("desktop_uncheck", "unchecked", true),
    ("desktop_toggle", "switched", true),
    ("desktop_expand", "expanded", true),
    ("desktop_collapse", "collapsed", true),
    ("desktop_scroll", "scrolled", false),
    ("desktop_scrollAt", "scrolled", false),
    ("desktop_scrollIntoView", "scrolled to", true),
    ("desktop_readText", "read the text of", true),
    ("desktop_moveMouse", "moved the pointer", false),
    ("desktop_drag", "dragged", true),
    ("desktop_screenshot", "took a screenshot", false),
    ("window_focus", "brought the window to the front", false),
    ("window_move", "moved the window", false),
    ("window_resize", "resized the window", false),
    ("window_set_bounds", "moved the window", false),
    ("window_minimize", "minimized the window", false),
    ("window_maximize", "maximized the window", false),
    ("window_restore", "restored the window", false),
    ("window_close", "closed the window", false),
];

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAY_MS: u64 = 86_400_000;

/// `lines` (oldest first, as `AuditLog::tail` returns them) as a list a person can read: newest
/// first under a heading per day, `10:42  Discord  clicked "Send"  (allowed)`. `to_local` turns
/// Unix epoch milliseconds into local-time milliseconds. Shows only what the log holds, which
/// never includes typed text or values; lines it cannot read are skipped.
pub fn readable(lines: &[String], to_local: impl Fn(u64) -> u64) -> String {
    let mut out = String::new();
    let mut day = None;
    for event in lines
        .iter()
        .rev()
        .filter_map(|l| serde_json::from_str::<Logged>(l).ok())
    {
        let local = to_local(event.timestamp_ms);
        if day != Some(local / DAY_MS) {
            if day.is_some() {
                out.push('\n');
            }
            day = Some(local / DAY_MS);
            out.push_str(&day_heading(local / DAY_MS));
            out.push('\n');
        }
        let minutes = local % DAY_MS / 60_000;
        let (app, action) = describe(&event);
        out.push_str(&format!(
            "  {:02}:{:02}  {}  {}  ({})\n",
            minutes / 60,
            minutes % 60,
            fit(&app, 20),
            fit(&action, 40),
            outcome(&event.result, event.confirmation)
        ));
    }
    out
}

/// `Tuesday 6 October 2026` for a count of days since 1970-01-01 (a Thursday).
fn day_heading(days: u64) -> String {
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{} {d} {} {y}",
        WEEKDAYS[((days + 4) % 7) as usize],
        MONTHS[(m - 1) as usize]
    )
}

/// The app (process, else window) and what was done to it.
fn describe(event: &Logged) -> (String, String) {
    let target = event.target.clone().unwrap_or_default();
    let text = |s: &Option<String>| s.as_deref().map(printable).filter(|s| !s.is_empty());
    let name = text(&target.name);
    let program = |s: String| {
        let file = s.rsplit(['\\', '/']).next().unwrap_or_default().to_owned();
        strip_exe(&file).to_owned()
    };
    let app = text(&target.process)
        .map(|p| strip_exe(&p).to_owned())
        .or_else(|| text(&target.window));
    let (app, action) = match event.tool.as_str() {
        "app_launch" => (name, "opened".to_owned()),
        "process_terminate" => (name.map(program), "ended the program".to_owned()),
        "shell_execute" => (name.map(program), "ran a command".to_owned()),
        "process_session" => (name.map(program), "background program".to_owned()),
        "filesystem_operation" => (
            Some("Files".to_owned()),
            name.unwrap_or_else(|| "file operation".to_owned()),
        ),
        "memory_save" => (
            Some("Winwright".to_owned()),
            "saved a task report".to_owned(),
        ),
        "memory_recall" => (
            Some("Winwright".to_owned()),
            "read earlier task reports".to_owned(),
        ),
        "desktop_screenshot" => (app.or(name), "took a screenshot".to_owned()),
        tool => match VERBS.iter().find(|(t, ..)| *t == tool) {
            Some((_, verb, named)) => match name.filter(|_| *named) {
                Some(n) => (app, format!("{verb} \"{}\"", clip(&n, 40))),
                None => (app, (*verb).to_owned()),
            },
            None => (app, tool.replace('_', " ")),
        },
    };
    (app.unwrap_or_else(|| "-".to_owned()), action)
}

fn outcome(result: &str, confirmation: bool) -> &'static str {
    match result {
        "ok" if confirmation => "asked, you allowed",
        "ok" => "allowed",
        "ACTION_BLOCKED" | "CONFIRMATION_REQUIRED" => "denied",
        "CANCELLED" => "stopped",
        _ if confirmation => "you allowed, it failed",
        _ => "failed",
    }
}

fn strip_exe(name: &str) -> &str {
    match name.len().checked_sub(4) {
        Some(i) if name.is_char_boundary(i) && name[i..].eq_ignore_ascii_case(".exe") => &name[..i],
        _ => name,
    }
}

/// Window titles and names as plain one-line text: no control or direction-changing characters.
fn printable(text: &str) -> String {
    let bidi = |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    text.chars()
        .filter(|c| !bidi(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// At most `width` characters, the last one an ellipsis when cut.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('\u{2026}');
    cut
}

/// Clipped, then padded to exactly `width` characters, so columns line up in Notepad.
fn fit(text: &str, width: usize) -> String {
    let clipped = clip(text, width);
    let pad = width - clipped.chars().count();
    format!("{clipped}{}", " ".repeat(pad))
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

    /// 2026-10-06 (a Tuesday) at `hh:mm` UTC.
    fn at(hh: u64, mm: u64) -> u64 {
        1_791_244_800_000 + (hh * 60 + mm) * 60_000
    }

    fn logged(
        timestamp_ms: u64,
        tool: &str,
        target: Option<&TargetSummary>,
        result: &str,
        confirmation: bool,
    ) -> String {
        serde_json::to_string(&AuditEvent {
            timestamp_ms,
            session: "s",
            tool,
            target,
            method: None,
            result,
            confirmation,
            duration_ms: 5,
        })
        .unwrap()
    }

    fn element(process: &str, window: &str, role: &str, name: &str) -> TargetSummary {
        TargetSummary {
            process: Some(process.into()),
            window: Some(window.into()),
            role: Some(role.into()),
            name: Some(name.into()),
        }
    }

    #[test]
    fn recent_activity_reads_newest_first_with_day_headings() {
        let send = element("Discord.exe", "#general - Discord", "Button", "Send");
        let field = element(
            "Discord.exe",
            "#general - Discord",
            "Edit",
            "Message #general",
        );
        let lines = vec![
            logged(
                at(23, 59) - DAY_MS,
                "desktop_click",
                Some(&send),
                "ok",
                false,
            ),
            logged(at(10, 41), "desktop_typeText", Some(&field), "ok", false),
            logged(at(10, 42), "desktop_click", Some(&send), "ok", true),
            "not json".to_owned(),
            logged(
                at(10, 43),
                "window_close",
                Some(&send),
                "ACTION_BLOCKED",
                false,
            ),
        ];
        let text = readable(&lines, |ms| ms);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], "Tuesday 6 October 2026");
        assert!(rows[1].starts_with("  10:43  Discord  ") && rows[1].ends_with("(denied)"));
        assert!(rows[1].contains("closed the window"));
        assert!(rows[2].contains("clicked \"Send\"") && rows[2].ends_with("(asked, you allowed)"));
        assert!(rows[3].contains("typed text") && rows[3].ends_with("(allowed)"));
        assert!(
            !rows[3].contains("Message"),
            "typing never names the field: {}",
            rows[3]
        );
        assert_eq!(rows[4], "");
        assert_eq!(rows[5], "Monday 5 October 2026");
        assert!(rows[6].starts_with("  23:59  "));
        assert_eq!(rows.len(), 7, "the unreadable line is skipped: {text}");
        // Columns line up: every action starts at the same place.
        let col = |r: &str| {
            r.find("  closed")
                .or(r.find("  clicked"))
                .or(r.find("  typed"))
        };
        assert_eq!(col(rows[1]), col(rows[2]));
        assert_eq!(col(rows[2]), col(rows[3]));
    }

    #[test]
    fn recent_activity_uses_local_time_and_plain_words() {
        let five_thirty = |ms: u64| ms + 330 * 60_000;
        let app = TargetSummary {
            name: Some("Discord".into()),
            ..Default::default()
        };
        let file = TargetSummary {
            name: Some("Read C:\\notes.txt".into()),
            ..Default::default()
        };
        let shell = TargetSummary {
            name: Some("C:\\Windows\\System32\\cmd.exe".into()),
            ..Default::default()
        };
        let lines = vec![
            logged(at(20, 0), "app_launch", Some(&app), "ok", false),
            logged(at(20, 1), "filesystem_operation", Some(&file), "ok", false),
            logged(at(20, 2), "shell_execute", Some(&shell), "TIMEOUT", true),
            logged(at(20, 3), "memory_save", None, "ok", false),
            logged(at(20, 4), "desktop_press", None, "CANCELLED", false),
        ];
        let text = readable(&lines, five_thirty);
        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows[0], "Wednesday 7 October 2026", "past midnight locally");
        assert!(rows[1].starts_with("  01:34  -  ") && rows[1].ends_with("(stopped)"));
        assert!(rows[2].contains("Winwright") && rows[2].contains("saved a task report"));
        assert!(rows[3].contains("cmd ") && rows[3].ends_with("(you allowed, it failed)"));
        assert!(rows[4].contains("Files") && rows[4].contains("Read C:\\notes.txt"));
        assert!(rows[5].starts_with("  01:30  Discord") && rows[5].contains("opened"));
    }

    #[test]
    fn titles_cannot_break_the_layout() {
        let sneaky = element(
            "app.exe",
            "w",
            "Button",
            "Line one\nLine two \u{202E}desrever and a very long name that keeps going on",
        );
        let text = readable(
            &[logged(
                at(9, 5),
                "desktop_click",
                Some(&sneaky),
                "ok",
                false,
            )],
            |ms| ms,
        );
        let row = text.lines().nth(1).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(!row.contains('\u{202E}'));
        assert!(row.contains('\u{2026}'), "long names are clipped: {row}");
        assert!(readable(&[], |ms| ms).is_empty());
    }
}
