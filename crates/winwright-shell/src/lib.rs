//! Level-1 process operations (spec §3, §39): launch apps, list processes, and run typed
//! executions (a program plus an argument list, never a shell string).
//!
//! Permission policy (allow / confirm / deny) is applied by the core engine. This crate only
//! enforces hard invariants: no elevation, no NUL smuggling, no implicit `cmd.exe`, bounded
//! time and output, and no process left behind by `exec`.

mod exec;
mod handle;
mod launch;
mod processes;
mod sessions;
mod shortcut;

pub use processes::UNKNOWN_SESSION;

use std::sync::Arc;
use std::time::Duration;

use winwright_contracts::backend::{BackendFuture, OperationContext};
use winwright_contracts::system::{
    ExecRequest, ExecResult, LaunchRequest, LaunchResult, ProcessInfo, ProcessService, SessionInfo,
    SessionOutput, SessionStart,
};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::sessions::Sessions;

/// [`ProcessService`] backed by `CreateProcessW`, `ShellExecuteExW`, Toolhelp snapshots, and
/// Tokio child processes, with background sessions that end with it.
#[derive(Clone, Debug, Default)]
pub struct SystemProcesses {
    sessions: Arc<Sessions>,
}

impl SystemProcesses {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProcessService for SystemProcesses {
    fn launch<'a>(
        &'a self,
        request: LaunchRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, LaunchResult> {
        Box::pin(async move {
            ctx.check("launch")?;
            let plan = launch::plan(&request)?;
            tracing::info!(
                target_kind = plan.kind(),
                target = %plan.log_label(),
                args = request.args.len(),
                "launch"
            );
            let result = tokio::task::spawn_blocking(move || plan.run())
                .await
                .map_err(|e| worker_failed("launch", &e))??;
            tracing::info!(method = %result.method, pid = ?result.process_id, "launched");
            Ok(result)
        })
    }

    fn list(&self) -> WinwrightResult<Vec<ProcessInfo>> {
        processes::list()
    }

    fn exec<'a>(
        &'a self,
        request: ExecRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ExecResult> {
        Box::pin(exec::exec(request, ctx))
    }

    fn resolve_launch(&self, request: &LaunchRequest) -> WinwrightResult<String> {
        launch::plan(request).map(|plan| plan.target_text())
    }

    fn resolve_program(&self, request: &ExecRequest) -> WinwrightResult<String> {
        exec::resolve_program(request).map(|path| path.display().to_string())
    }

    fn can_terminate(&self, pid: u32, name: &str) -> WinwrightResult<()> {
        processes::can_terminate(pid, name)
    }

    fn terminate(&self, pid: u32, name: &str) -> WinwrightResult<()> {
        tracing::info!(pid, "terminate");
        processes::terminate(pid, name)
    }

    fn session_start(&self, request: SessionStart) -> WinwrightResult<SessionInfo> {
        self.sessions.start(&request)
    }

    fn session_input<'a>(&'a self, id: u32, text: String) -> BackendFuture<'a, ()> {
        Box::pin(async move { self.sessions.input(id, &text).await })
    }

    fn session_read<'a>(&'a self, id: u32, wait: Duration) -> BackendFuture<'a, SessionOutput> {
        Box::pin(self.sessions.read(id, wait))
    }

    fn session_list(&self) -> Vec<SessionInfo> {
        self.sessions.list()
    }

    fn session_stop(&self, id: u32) -> WinwrightResult<SessionInfo> {
        self.sessions.stop(id)
    }

    fn stop_all_sessions(&self) {
        self.sessions.stop_all();
    }
}

fn worker_failed(operation: &str, err: &tokio::task::JoinError) -> WinwrightError {
    WinwrightError::ActionOutcomeUnknown {
        operation: operation.to_owned(),
        reason: format!("worker task failed: {err}"),
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

/// Rejects strings that would be silently truncated at an interior NUL by Win32.
pub(crate) fn reject_nul(what: &str, value: &str) -> WinwrightResult<()> {
    if value.contains('\0') {
        return Err(WinwrightError::invalid(format!(
            "{what} must not contain NUL characters"
        )));
    }
    Ok(())
}
