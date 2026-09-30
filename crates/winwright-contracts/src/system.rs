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
    /// Executable name or path (`notepad.exe`), a shell URI (`ms-settings:display`), or a
    /// folder/file path to open with its default handler.
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "op", deny_unknown_fields)]
pub enum FileOperation {
    List {
        path: PathBuf,
        #[serde(default)]
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
        #[serde(default = "default_search_limit")]
        max_results: usize,
    },
    /// Resolves a known folder: `Desktop`, `Documents`, `Downloads`, `Pictures`, ...
    KnownFolder {
        name: String,
    },
}

fn default_search_limit() -> usize {
    200
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
}

pub trait FileService: Send + Sync {
    fn execute<'a>(
        &'a self,
        op: FileOperation,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, FileResult>;
}
