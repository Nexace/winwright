//! Level-1 file operations (spec §3, §40) without driving Explorer.
//!
//! Hard invariants enforced here, before and independent of the core's permission policy:
//! - every path is absolute, normalized, and canonicalized (links and 8.3 names resolved);
//!   device/namespace paths, reserved device names, and alternate data streams are refused;
//! - writes, moves, renames, deletes, and creates never touch protected locations
//!   (see [`LocalFiles::protected_locations`]);
//! - deletion only ever moves items to the Recycle Bin, never deletes permanently;
//! - copies, moves, and renames never replace an existing item unless `overwrite` says so;
//! - a text file that is replaced or edited goes to the Recycle Bin first.

mod glob;
mod ops;
mod path;
mod shell;
mod text;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;

use winwright_contracts::WinwrightError;
use winwright_contracts::backend::{BackendFuture, OperationContext};
use winwright_contracts::system::{FileOperation, FileResult, FileService};

use crate::path::Protected;

/// [`FileService`] over the local filesystem and the Windows shell.
#[derive(Clone, Debug)]
pub struct LocalFiles {
    protected: Arc<Protected>,
}

impl LocalFiles {
    /// Resolves the protected locations once: the Windows folder, Program Files (all
    /// architectures), ProgramData, the user's Startup folder, and the user profile.
    pub fn new() -> Self {
        Self {
            protected: Arc::new(shell::system_protected()),
        }
    }

    /// Protected trees followed by the user profile, for diagnostics.
    pub fn protected_locations(&self) -> Vec<PathBuf> {
        self.protected.locations()
    }
}

impl Default for LocalFiles {
    fn default() -> Self {
        Self::new()
    }
}

impl FileService for LocalFiles {
    fn execute<'a>(
        &'a self,
        op: FileOperation,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, FileResult> {
        Box::pin(async move {
            ctx.check("file operation")?;
            let kind = operation_name(&op);
            let protected = Arc::clone(&self.protected);
            let worker_ctx = ctx.clone();
            tracing::debug!(op = kind, "file operation");
            let result = tokio::task::spawn_blocking(move || ops::run(op, &protected, &worker_ctx))
                .await
                .map_err(|e| WinwrightError::ActionOutcomeUnknown {
                    operation: kind.to_owned(),
                    reason: format!("worker task failed: {e}"),
                })?;
            if let Err(err) = &result {
                tracing::debug!(
                    op = kind,
                    code = err.code().as_str(),
                    "file operation refused"
                );
            }
            result
        })
    }
}

fn operation_name(op: &FileOperation) -> &'static str {
    match op {
        FileOperation::List { .. } => "list",
        FileOperation::Metadata { .. } => "metadata",
        FileOperation::Copy { .. } => "copy",
        FileOperation::Move { .. } => "move",
        FileOperation::Rename { .. } => "rename",
        FileOperation::Delete { .. } => "delete",
        FileOperation::CreateDirectory { .. } => "createDirectory",
        FileOperation::Search { .. } => "search",
        FileOperation::KnownFolder { .. } => "knownFolder",
        FileOperation::Read { .. } => "read",
        FileOperation::Write { .. } => "write",
        FileOperation::Edit { .. } => "edit",
        FileOperation::Grep { .. } => "grep",
    }
}

pub(crate) fn platform(operation: &str, err: &windows::core::Error) -> WinwrightError {
    WinwrightError::Platform {
        operation: operation.to_owned(),
        hresult: err.code().0,
    }
}

/// `HRESULT_FROM_WIN32` for an OS error code carried by `std::io::Error`.
pub(crate) fn io_platform(operation: &str, err: &std::io::Error) -> WinwrightError {
    let hresult = err.raw_os_error().map_or(0x8000_4005_u32 as i32, |code| {
        windows::core::HRESULT::from_win32(code as u32).0
    });
    WinwrightError::Platform {
        operation: operation.to_owned(),
        hresult,
    }
}

/// NUL-terminated UTF-16 copy of `value` for `PCWSTR` parameters.
pub(crate) fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
