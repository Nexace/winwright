//! Task memory: one short markdown report per task, so any app that uses Winwright can recall
//! earlier work. Reports live in `%USERPROFILE%\.winwright\reports` (not AppData, which apps
//! installed as packages see redirected to a private copy), shared with the assistant's bridge.
//! Each is also copied to Notion when a token and a parent page are configured.
//!
//! Reports hold names and summaries only: never typed text, field values, or file contents.

mod notion;
mod time;

use std::path::{Path, PathBuf};

use winwright_contracts::memory::{
    MemorySaved, MemoryStore, NewReport, OutsideContent, StoredReport,
};
use winwright_contracts::{WinwrightError, WinwrightResult};

const MAX_TITLE: usize = 80;
const MAX_SUMMARY: usize = 1_500;

pub struct Memory {
    dir: PathBuf,
    notion: Option<notion::Target>,
}

impl Memory {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, notion: None }
    }

    /// The reports folder and Notion copy from the environment: `WINWRIGHT_REPORTS_DIR`
    /// (default `%USERPROFILE%\.winwright\reports`), `WINWRIGHT_NOTION_TOKEN` and
    /// `WINWRIGHT_NOTION_PARENT` (the assistant's `JARVIS_NOTION_*` names work too).
    /// `WINWRIGHT_MEMORY=0` turns memory off.
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        if var("WINWRIGHT_MEMORY").as_deref() == Some("0") {
            return None;
        }
        let dir = var("WINWRIGHT_REPORTS_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                var("USERPROFILE")
                    .map(|home| PathBuf::from(home).join(".winwright").join("reports"))
            })?;
        Some(Self {
            dir,
            notion: notion::Target::from_env(var),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn copies_to_notion(&self) -> bool {
        self.notion.is_some()
    }
}

fn clip(text: &str, max: usize) -> String {
    let flat = text.trim();
    if flat.chars().count() <= max {
        flat.to_owned()
    } else {
        let mut cut: String = flat.chars().take(max - 1).collect();
        cut.push('…');
        cut
    }
}

/// `2026-10-04-021326-typed-a-note-in-notepad.md`.
fn file_name(stamp: &str, title: &str) -> String {
    let lower = title.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
        .collect();
    let mut slug: String = words.join("-").chars().take(40).collect();
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("task");
    }
    format!("{stamp}-{slug}.md")
}

/// The report file: front matter, then title, summary and the tools that ran.
fn report_markdown(r: &NewReport, utc: &str) -> String {
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut lines = vec![
        "---".to_owned(),
        format!("date: {utc}"),
        format!("outcome: {}", one_line(&clip(&r.outcome, 40))),
        format!("outsideContent: {}", r.outside.as_str()),
    ];
    if let Some(source) = &r.source {
        lines.push(format!("source: {}", one_line(&clip(source, 40))));
    }
    lines.push("---".into());
    lines.push(format!("# {}", one_line(&clip(&r.title, MAX_TITLE))));
    lines.push(String::new());
    lines.push(format!("**Summary:** {}", clip(&r.summary, MAX_SUMMARY)));
    lines.push(String::new());
    let tools = if r.tools.is_empty() {
        "none".to_owned()
    } else {
        r.tools.join(", ")
    };
    lines.push(format!("**Tools:** {tools}"));
    lines.push(String::new());
    lines.join("\n")
}

/// A report file read back: whether it followed outside content, and its body.
fn parse(name: String, text: &str) -> StoredReport {
    let text = text.replace("\r\n", "\n");
    let (front, body) = match text
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
    {
        Some((front, body)) => (front, body),
        None => ("", text.as_str()),
    };
    let outside = match front
        .lines()
        .find_map(|l| l.strip_prefix("outsideContent:"))
        .map(str::trim)
    {
        Some("false") => OutsideContent::No,
        Some("true") => OutsideContent::Yes,
        _ => OutsideContent::Unknown,
    };
    StoredReport {
        name,
        outside,
        body: body.trim().to_owned(),
    }
}

fn io_error(what: &str, path: &Path, err: std::io::Error) -> WinwrightError {
    WinwrightError::invalid(format!("{what} {}: {err}", path.display()))
}

impl MemoryStore for Memory {
    fn save(&self, report: &NewReport) -> WinwrightResult<MemorySaved> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| io_error("cannot create the reports folder", &self.dir, e))?;
        let now = time::now();
        let markdown = report_markdown(report, &now.utc);
        let name = file_name(&now.local_stamp, &report.title);
        // Never overwrite: two reports in the same second get a number.
        let mut path = self.dir.join(&name);
        for n in 2.. {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(markdown.as_bytes())
                        .map_err(|e| io_error("cannot write", &path, e))?;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => {
                    path = self.dir.join(name.replace(".md", &format!("-{n}.md")));
                }
                Err(e) => return Err(io_error("cannot write", &path, e)),
            }
        }
        let (notion_url, notion_error) = match &self.notion {
            Some(target) => match notion::create_page(target, &markdown) {
                Ok(url) => (Some(url), None),
                Err(err) => {
                    tracing::warn!(%err, "Notion copy failed");
                    (None, Some(err))
                }
            },
            None => (None, None),
        };
        Ok(MemorySaved {
            path: path.display().to_string(),
            notion_url,
            notion_error,
        })
    }

    fn recall(&self, query: &str, limit: usize) -> WinwrightResult<Vec<StoredReport>> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut names: Vec<String> = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.ends_with(".md"))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error("cannot read", &self.dir, e)),
        };
        names.sort_unstable_by(|a, b| b.cmp(a));
        let mut found = Vec::new();
        for name in names {
            if found.len() >= limit {
                break;
            }
            let Ok(text) = std::fs::read_to_string(self.dir.join(&name)) else {
                continue;
            };
            let lower = text.to_lowercase();
            if words.iter().all(|w| lower.contains(w.as_str())) {
                found.push(parse(name, &text));
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(title: &str, summary: &str, outside: OutsideContent) -> NewReport {
        NewReport {
            title: title.into(),
            summary: summary.into(),
            outcome: "done".into(),
            tools: vec![
                "winwright app_launch".into(),
                "winwright desktop_type ×2".into(),
            ],
            outside,
            source: Some("codex".into()),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "winwright-memory-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn names_are_time_then_slug() {
        assert_eq!(
            file_name("2026-10-04-021326", "Typed a note in Notepad!"),
            "2026-10-04-021326-typed-a-note-in-notepad.md"
        );
        assert_eq!(
            file_name("2026-10-04-021326", "¿¿"),
            "2026-10-04-021326-task.md"
        );
    }

    #[test]
    fn a_report_round_trips_and_keeps_its_outside_content_mark() {
        let md = report_markdown(
            &report("Open Notepad", "Typed a greeting.", OutsideContent::Unknown),
            "2026-10-04T08:48:19Z",
        );
        assert!(md.contains("outsideContent: unknown\nsource: codex\n---\n# Open Notepad"));
        assert!(md.contains("**Tools:** winwright app_launch, winwright desktop_type ×2"));
        let back = parse("x.md".into(), &md);
        assert_eq!(back.outside, OutsideContent::Unknown);
        assert!(back.body.starts_with("# Open Notepad"));
        // The bridge's reports say true/false.
        let bridge = "---\ndate: d\noutsideContent: false\n---\n# Hi\n\n**Asked:** hi";
        assert_eq!(parse("y.md".into(), bridge).outside, OutsideContent::No);
    }

    #[test]
    fn saving_never_overwrites_and_recall_finds_by_words_newest_first() {
        let dir = temp_dir("save");
        let memory = Memory::new(dir.clone());
        assert!(memory.recall("", 5).unwrap().is_empty(), "no folder yet");
        let a = memory
            .save(&report(
                "Weather in Pune",
                "Sunny, 31 C.",
                OutsideContent::Yes,
            ))
            .unwrap();
        let b = memory
            .save(&report(
                "Weather in Pune",
                "Rain later.",
                OutsideContent::No,
            ))
            .unwrap();
        assert_ne!(a.path, b.path, "same second, same title: a second file");
        assert!(
            a.notion_url.is_none() && a.notion_error.is_none(),
            "Notion off"
        );
        memory
            .save(&report("Open Notepad", "Typed hello.", OutsideContent::No))
            .unwrap();
        let pune = memory.recall("PUNE weather", 5).unwrap();
        assert_eq!(pune.len(), 2);
        assert!(memory.recall("", 1).unwrap().len() == 1);
        assert!(memory.recall("nothing-like-this", 5).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_text_is_clipped() {
        let md = report_markdown(
            &report(&"t".repeat(500), &"s".repeat(10_000), OutsideContent::No),
            "d",
        );
        assert!(md.chars().count() < MAX_TITLE + MAX_SUMMARY + 400);
    }
}
