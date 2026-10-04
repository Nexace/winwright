//! Level-1 system operations (spec §39, §40): processes and files, without UI automation.
//! Implemented by `winwright-shell` and `winwright-files`. Policy is applied by the core.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::WinwrightResult;
use crate::backend::{BackendFuture, OperationContext};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LaunchRequest {
    /// Executable name or path (`notepad.exe`), an allowlisted URI (`ms-settings:display`), or
    /// a folder or document/image/media path to open with its default handler.
    pub app: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LaunchResult {
    /// `None` when the OS reused an existing process or a URI handler started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    /// How it was started: `process` (CreateProcess) or `shell` (ShellExecuteEx).
    pub method: String,
    /// The window it opened, once it appeared and had a moment to get ready for input.
    /// `None` when no window showed up in time (a background program, or a slow start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<crate::window::WindowInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub process_id: u32,
    pub parent_process_id: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub session_id: u32,
    /// Integrity level when queryable: `low`, `medium`, `high`, `system`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
}

/// Typed process execution: an executable plus an argument list, never a shell string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecRequest {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    #[serde(default = "default_exec_timeout")]
    pub timeout_ms: u64,
    /// Output is truncated to this many bytes per stream.
    #[serde(default = "default_exec_output")]
    pub max_output_bytes: usize,
}

fn default_exec_timeout() -> u64 {
    30_000
}
fn default_exec_output() -> usize {
    64 * 1024
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    /// `None` if the process was killed at the timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
    pub duration_ms: u64,
}

/// Tagged by `op`; field names are camelCase (`includeHidden`, `newName`, `maxResults`), and
/// the earlier snake_case spellings are still accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "op",
    deny_unknown_fields
)]
pub enum FileOperation {
    List {
        path: PathBuf,
        #[serde(default, alias = "include_hidden")]
        include_hidden: bool,
    },
    Metadata {
        path: PathBuf,
    },
    Copy {
        from: PathBuf,
        to: PathBuf,
        #[serde(default)]
        overwrite: bool,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        #[serde(default)]
        overwrite: bool,
    },
    Rename {
        path: PathBuf,
        #[serde(alias = "new_name")]
        new_name: String,
    },
    /// Moves to the Recycle Bin; never a permanent delete.
    Delete {
        path: PathBuf,
    },
    CreateDirectory {
        path: PathBuf,
    },
    Search {
        root: PathBuf,
        /// Case-insensitive glob on file names, e.g. `*.png`.
        pattern: String,
        #[serde(default = "default_search_limit", alias = "max_results")]
        max_results: usize,
    },
    /// Resolves a known folder: `Desktop`, `Documents`, `Downloads`, `Pictures`, ...
    KnownFolder {
        name: String,
    },
    /// Lines of a text file: from line `offset` (0-based; negative counts from the end), at
    /// most `length` of them.
    Read {
        path: PathBuf,
        #[serde(default)]
        offset: i64,
        #[serde(default = "default_read_lines")]
        length: usize,
    },
    /// Creates a text file (UTF-8), or with `mode` replaces or appends to one. A replaced file
    /// goes to the Recycle Bin first.
    Write {
        path: PathBuf,
        content: String,
        #[serde(default)]
        mode: WriteMode,
    },
    /// Replaces `old` with `new` in a text file. `old` must occur exactly `count` times
    /// (default 1); the previous version goes to the Recycle Bin.
    Edit {
        path: PathBuf,
        old: String,
        new: String,
        #[serde(default = "one")]
        count: usize,
    },
    /// Lines matching a regular expression in the text files under `root` (only names matching
    /// `glob` when given). Secret files and folders are skipped.
    Grep {
        root: PathBuf,
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        glob: Option<String>,
        #[serde(default)]
        ignore_case: bool,
        #[serde(default = "default_search_limit", alias = "max_results")]
        max_results: usize,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WriteMode {
    /// A new file; fails if one exists.
    #[default]
    Create,
    /// Replace the file (the old one goes to the Recycle Bin), or create it.
    Overwrite,
    /// Add to the end of the file, or create it.
    Append,
}

fn default_search_limit() -> usize {
    200
}
fn default_read_lines() -> usize {
    1_000
}
fn one() -> usize {
    1
}

/// One line found by `grep`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TextMatch {
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
    /// The line, cut to a few hundred characters.
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<u64>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum FileResult {
    Entries {
        entries: Vec<FileEntry>,
        truncated: bool,
    },
    Entry {
        entry: FileEntry,
    },
    Path {
        path: PathBuf,
    },
    Done,
    /// Lines `first_line`.. (0-based) of a file of `total_lines` lines.
    #[serde(rename_all = "camelCase")]
    Text {
        path: PathBuf,
        text: String,
        first_line: usize,
        total_lines: usize,
        truncated: bool,
    },
    Matches {
        matches: Vec<TextMatch>,
        truncated: bool,
    },
}

pub trait ProcessService: Send + Sync {
    fn launch<'a>(
        &'a self,
        request: LaunchRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, LaunchResult>;
    fn list(&self) -> WinwrightResult<Vec<ProcessInfo>>;
    fn exec<'a>(
        &'a self,
        request: ExecRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ExecResult>;

    /// What `launch` would start, without starting it: the resolved program path, or the URI,
    /// folder or file the shell opens. Fails as `launch` would.
    fn resolve_launch(&self, request: &LaunchRequest) -> WinwrightResult<String> {
        Ok(request.app.clone())
    }

    /// The program `exec` would run, without running it. Fails as `exec` would.
    fn resolve_program(&self, request: &ExecRequest) -> WinwrightResult<String> {
        Ok(request.program.clone())
    }
}

pub trait FileService: Send + Sync {
    fn execute<'a>(
        &'a self,
        op: FileOperation,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, FileResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_operation_fields_are_camel_case() {
        let op: FileOperation =
            serde_json::from_str(r#"{"op":"rename","path":"a.txt","newName":"b.txt"}"#).unwrap();
        assert!(matches!(op, FileOperation::Rename { ref new_name, .. } if new_name == "b.txt"));
        let op: FileOperation = serde_json::from_str(
            r#"{"op":"search","root":"docs","pattern":"*.png","maxResults":5}"#,
        )
        .unwrap();
        assert!(matches!(op, FileOperation::Search { max_results: 5, .. }));
        let json = serde_json::to_value(FileOperation::List {
            path: "docs".into(),
            include_hidden: true,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op": "list", "path": "docs", "includeHidden": true})
        );
        // Earlier snake_case spellings keep working; typos are still rejected.
        let op: FileOperation =
            serde_json::from_str(r#"{"op":"list","path":"docs","include_hidden":true}"#).unwrap();
        assert!(matches!(
            op,
            FileOperation::List {
                include_hidden: true,
                ..
            }
        ));
        assert!(
            serde_json::from_str::<FileOperation>(r#"{"op":"list","path":"docs","hidden":true}"#)
                .is_err()
        );
        let op: FileOperation =
            serde_json::from_str(r#"{"op":"write","path":"a.txt","content":"hi","mode":"append"}"#)
                .unwrap();
        assert!(matches!(
            op,
            FileOperation::Write {
                mode: WriteMode::Append,
                ..
            }
        ));
        let op: FileOperation =
            serde_json::from_str(r#"{"op":"edit","path":"a.txt","old":"x","new":"y"}"#).unwrap();
        assert!(matches!(op, FileOperation::Edit { count: 1, .. }));
        let op: FileOperation =
            serde_json::from_str(r#"{"op":"read","path":"a.txt","offset":-20}"#).unwrap();
        assert!(matches!(
            op,
            FileOperation::Read {
                offset: -20,
                length: 1_000,
                ..
            }
        ));
        let schema = serde_json::to_string(&schemars::schema_for!(FileOperation)).unwrap();
        for field in ["includeHidden", "newName", "maxResults", "ignoreCase"] {
            assert!(schema.contains(field), "{field} missing from {schema}");
        }
    }
}
