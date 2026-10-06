//! Task memory: short reports of what was done, kept as markdown files (and copied to Notion
//! when configured) so any app that uses Winwright can recall earlier work.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::WinwrightResult;

/// Whether the conversation that wrote a report had read outside content (a web or Notion
/// page), which could carry instructions aimed at the assistant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OutsideContent {
    Yes,
    No,
    /// Written where nothing tracks it (most MCP clients).
    Unknown,
}

impl OutsideContent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "true",
            Self::No => "false",
            Self::Unknown => "unknown",
        }
    }
}

/// What the model asks to save.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemorySaveRequest {
    /// A few words naming the task ("Typed a note in Notepad").
    pub title: String,
    /// What the person asked and what was done, in a few sentences. Names and outcomes only:
    /// never passwords, secrets, or long copied text.
    pub summary: String,
    /// `done`, `partly done`, or `failed`.
    #[serde(default)]
    pub outcome: Option<String>,
    /// The app a lesson is about, as the person names it ("Microsoft Store", "Discord").
    #[serde(default)]
    pub app: Option<String>,
    /// When something failed in that app and then worked: what failed and what works, in one
    /// or two sentences. The next `app_launch` of the app returns it as a hint.
    #[serde(default)]
    pub lesson: Option<String>,
}

/// A report as the store writes it: the request plus what Winwright knows itself.
#[derive(Clone, Debug)]
pub struct NewReport {
    pub title: String,
    pub summary: String,
    pub outcome: String,
    /// Winwright tools run since the last report, as `tool ×count` labels.
    pub tools: Vec<String>,
    pub outside: OutsideContent,
    /// The app that asked (the MCP client's name), when known.
    pub source: Option<String>,
    /// The app a lesson is about, and the lesson (see [`MemorySaveRequest`]).
    pub app: Option<String>,
    pub lesson: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemorySaved {
    /// The report file.
    pub path: String,
    /// The Notion page, when the copy succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notion_url: Option<String>,
    /// Why the Notion copy failed; the file is saved either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notion_error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryRecallRequest {
    /// Words that must all appear in a report (case-insensitive). Omit for the newest reports.
    #[serde(default)]
    pub query: Option<String>,
    /// How many reports, newest first in the search, oldest first in the answer (default 5,
    /// at most 20).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// One stored report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredReport {
    /// File name (`2026-10-04-021326-open-notepad.md`); sorts by time.
    pub name: String,
    pub outside: OutsideContent,
    /// The markdown without its front matter.
    pub body: String,
}

/// Where reports live. Implemented by `winwright-memory`.
pub trait MemoryStore: Send + Sync {
    /// Writes the report, then copies it to Notion if configured (blocking: network).
    fn save(&self, report: &NewReport) -> WinwrightResult<MemorySaved>;
    /// Newest reports matching every word of `query`, newest first.
    fn recall(&self, query: &str, limit: usize) -> WinwrightResult<Vec<StoredReport>>;
    /// The newest lessons saved for `app`, newest first, never from a report written after
    /// outside content.
    fn lessons(&self, _app: &str, _limit: usize) -> WinwrightResult<Vec<String>> {
        Ok(Vec::new())
    }
}
