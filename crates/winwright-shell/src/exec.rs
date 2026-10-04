//! Typed execution: a program plus an argument vector, never a shell string. Output is captured
//! concurrently and capped per stream, time is bounded, and the whole process tree lives in a
//! kill-on-close job so nothing outlives the call.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use windows::Win32::Foundation::{ERROR_ELEVATION_REQUIRED, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::core::PCWSTR;
use winwright_contracts::backend::OperationContext;
use winwright_contracts::system::{ExecRequest, ExecResult};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::handle::OwnedHandle;
use crate::launch::{extension_of, on_network, remote_executable, search_executable};
use crate::{io_platform, reject_nul};

pub(crate) const MAX_TIMEOUT_MS: u64 = 600_000;
/// Upper bound for `max_output_bytes`, per stream.
pub(crate) const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// After a normal exit, how long descendants may keep the pipes open before they are killed.
const EXIT_GRACE: Duration = Duration::from_secs(2);
/// After a kill, how long to wait for the pipes to close.
const KILL_GRACE: Duration = Duration::from_millis(500);
pub(crate) const READ_CHUNK: usize = 8 * 1024;

enum Ending {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

/// The program `exec` runs: a path as given (made absolute against `workingDir`), else the
/// first `name.exe` in System32, the Windows folder, or `PATH`. Resolved here, not by the
/// process API, so what a confirmation shows is exactly what runs (that API would also try
/// the folder of winwright.exe first).
pub(crate) fn resolve_program(request: &ExecRequest) -> WinwrightResult<PathBuf> {
    validate(request)?;
    let program = request.program.trim();
    let path = if program.contains(['\\', '/', ':']) {
        let path = Path::new(program);
        let path = match &request.working_dir {
            Some(dir) if path.is_relative() => dir.join(path),
            _ => path.to_path_buf(),
        };
        std::path::absolute(&path).unwrap_or(path)
    } else {
        search_executable(program).ok_or_else(|| {
            WinwrightError::invalid(format!(
                "program {program} was not found in System32, the Windows folder, or PATH; \
                 pass its full path"
            ))
        })?
    };
    if on_network(&path) {
        return Err(remote_executable(&path));
    }
    Ok(path)
}

pub(crate) async fn exec(
    request: ExecRequest,
    ctx: &OperationContext,
) -> WinwrightResult<ExecResult> {
    let image = resolve_program(&request)?;
    ctx.check("exec")?;
    let program = program_label(&request.program);
    tracing::info!(program = %program, args = request.args.len(), "exec");

    let mut command = Command::new(&image);
    command
        .args(&request.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW.0)
        .kill_on_drop(true);
    if let Some(dir) = &request.working_dir {
        command.current_dir(dir);
    }

    let started = Instant::now();
    let deadline = (started + Duration::from_millis(request.timeout_ms)).min(ctx.deadline);
    let mut child = command.spawn().map_err(|e| spawn_error(&program, &e))?;
    let job = KillOnCloseJob::assign(&child);

    let stdout = Arc::new(Mutex::new(Capture::new(request.max_output_bytes)));
    let stderr = Arc::new(Mutex::new(Capture::new(request.max_output_bytes)));
    let mut readers = [
        tokio::spawn(pump(child.stdout.take(), Arc::clone(&stdout))),
        tokio::spawn(pump(child.stderr.take(), Arc::clone(&stderr))),
    ];

    let ending = tokio::select! {
        biased;
        status = child.wait() => Ending::Exited(status.map_err(|e| io_platform("wait", &e))?),
        () = ctx.cancel.cancelled() => Ending::Cancelled,
        () = tokio::time::sleep_until(deadline.into()) => Ending::TimedOut,
    };

    let exit_code = match ending {
        Ending::Exited(status) => {
            let grace = deadline
                .saturating_duration_since(Instant::now())
                .min(EXIT_GRACE);
            settle(&mut readers, grace, job.as_ref()).await;
            status.code()
        }
        Ending::TimedOut | Ending::Cancelled => {
            kill_tree(&mut child, job.as_ref()).await;
            settle(&mut readers, KILL_GRACE, job.as_ref()).await;
            None
        }
    };
    drop(job);

    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if matches!(ending, Ending::Cancelled) {
        tracing::info!(program = %program, duration_ms, "exec cancelled");
        return Err(WinwrightError::Cancelled);
    }
    let (stdout, stdout_truncated) = take(&stdout).finish();
    let (stderr, stderr_truncated) = take(&stderr).finish();
    let timed_out = matches!(ending, Ending::TimedOut);
    let truncated = stdout_truncated || stderr_truncated;
    tracing::info!(
        program = %program,
        exit_code = ?exit_code,
        timed_out,
        truncated,
        duration_ms,
        "exec finished"
    );
    Ok(ExecResult {
        exit_code,
        stdout,
        stderr,
        truncated,
        timed_out,
        duration_ms,
    })
}

fn validate(request: &ExecRequest) -> WinwrightResult<()> {
    if request.program.trim().is_empty() {
        return Err(WinwrightError::invalid("program is empty"));
    }
    reject_nul("program", &request.program)?;
    for arg in &request.args {
        reject_nul("exec arguments", arg)?;
    }
    if let Some(dir) = &request.working_dir
        && !dir.is_dir()
    {
        return Err(WinwrightError::invalid(format!(
            "workingDir {} is not an existing folder",
            dir.display()
        )));
    }
    if !(1..=MAX_TIMEOUT_MS).contains(&request.timeout_ms) {
        return Err(WinwrightError::invalid(format!(
            "timeoutMs must be between 1 and {MAX_TIMEOUT_MS}"
        )));
    }
    if request.max_output_bytes > MAX_OUTPUT_BYTES {
        return Err(WinwrightError::invalid(format!(
            "maxOutputBytes must be at most {MAX_OUTPUT_BYTES}"
        )));
    }
    // Rust runs batch files through `cmd.exe /c`, which re-parses the arguments.
    let name = program_label(&request.program);
    if matches!(extension_of(&name).as_deref(), Some("bat" | "cmd")) {
        return Err(WinwrightError::invalid(
            "batch files are interpreted by cmd.exe; pass cmd.exe as the program explicitly",
        ));
    }
    Ok(())
}

/// File name only: full paths can reveal user names, and arguments are never logged.
pub(crate) fn program_label(program: &str) -> String {
    Path::new(program)
        .file_name()
        .map_or_else(|| program.to_owned(), |n| n.to_string_lossy().into_owned())
}

pub(crate) fn spawn_error(program: &str, err: &std::io::Error) -> WinwrightError {
    if err.kind() == std::io::ErrorKind::NotFound {
        WinwrightError::invalid(format!("program {program} was not found"))
    } else if err.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED.0 as i32) {
        WinwrightError::ActionBlocked {
            reason: format!("{program} requires elevation; Winwright never requests elevation"),
        }
    } else {
        io_platform("spawn", err)
    }
}

async fn kill_tree(child: &mut Child, job: Option<&KillOnCloseJob>) {
    if let Some(job) = job {
        job.terminate();
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(KILL_GRACE, child.wait()).await;
}

/// Waits for both readers to hit EOF. Descendants that still hold the pipes after `grace` are
/// killed with the job; readers that still do not finish are aborted (captured bytes are kept).
async fn settle(readers: &mut [JoinHandle<()>; 2], grace: Duration, job: Option<&KillOnCloseJob>) {
    if join_all(readers, grace).await {
        return;
    }
    if let Some(job) = job {
        job.terminate();
    }
    if !join_all(readers, KILL_GRACE).await {
        for reader in readers.iter() {
            reader.abort();
        }
    }
}

async fn join_all(readers: &mut [JoinHandle<()>; 2], limit: Duration) -> bool {
    tokio::time::timeout(limit, async {
        for reader in readers.iter_mut() {
            if !reader.is_finished() {
                let _ = reader.await;
            }
        }
    })
    .await
    .is_ok()
}

/// Reads to EOF, keeping at most the capture limit and discarding the rest so the child never
/// blocks on a full pipe.
async fn pump<R: AsyncRead + Unpin>(stream: Option<R>, sink: Arc<Mutex<Capture>>) {
    let Some(mut stream) = stream else { return };
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => sink
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(&chunk[..n]),
        }
    }
}

fn take(capture: &Mutex<Capture>) -> Capture {
    std::mem::take(&mut *capture.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Bounded output buffer for one stream.
#[derive(Debug, Default)]
pub(crate) struct Capture {
    buf: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl Capture {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            buf: Vec::with_capacity(limit.min(READ_CHUNK)),
            limit,
            truncated: false,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        let room = self.limit.saturating_sub(self.buf.len());
        let keep = room.min(chunk.len());
        self.buf.extend_from_slice(&chunk[..keep]);
        if keep < chunk.len() {
            self.truncated = true;
        }
    }

    /// Lossy UTF-8 text; a multi-byte character cut by the limit is dropped, not mangled.
    pub(crate) fn finish(self) -> (String, bool) {
        let end = if self.truncated {
            self.buf.len() - incomplete_utf8_tail(&self.buf)
        } else {
            self.buf.len()
        };
        (
            String::from_utf8_lossy(&self.buf[..end]).into_owned(),
            self.truncated,
        )
    }
}

/// Length of an incomplete UTF-8 sequence at the end of `bytes` (0 when it ends cleanly).
pub(crate) fn incomplete_utf8_tail(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(4) {
        let byte = bytes[bytes.len() - back];
        if byte & 0xC0 == 0x80 {
            continue; // continuation byte: keep looking for the lead byte
        }
        let needed = match byte {
            0xF0.. => 4,
            0xE0.. => 3,
            0xC0.. => 2,
            _ => 1,
        };
        return if needed > back { back } else { 0 };
    }
    0
}

/// Job object that kills every process in it when terminated or when its handle closes.
pub(crate) struct KillOnCloseJob(OwnedHandle);

impl KillOnCloseJob {
    /// Best effort: without a job, timeouts still kill the direct child.
    pub(crate) fn assign(child: &Child) -> Option<Self> {
        let process = HANDLE(child.raw_handle()?);
        // SAFETY: no security attributes and an unnamed job; the handle is owned below.
        let raw = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.ok()?;
        let job = Self(OwnedHandle::new(raw)?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is a fully initialized JOBOBJECT_EXTENDED_LIMIT_INFORMATION and the
        // length passed matches its size.
        let configured = unsafe {
            SetInformationJobObject(
                job.0.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        // SAFETY: `process` is the child's live process handle, owned by `child`, which
        // outlives this call.
        let assigned =
            configured.and_then(|()| unsafe { AssignProcessToJobObject(job.0.0, process) });
        match assigned {
            Ok(()) => Some(job),
            Err(e) => {
                tracing::debug!(hresult = e.code().0, "exec child not placed in a job");
                None
            }
        }
    }

    pub(crate) fn terminate(&self) {
        // SAFETY: the job handle is live for `self`'s lifetime.
        let _ = unsafe { TerminateJobObject(self.0.0, 1) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(program: &str) -> ExecRequest {
        ExecRequest {
            program: program.to_owned(),
            args: Vec::new(),
            working_dir: None,
            timeout_ms: 1_000,
            max_output_bytes: 1_024,
        }
    }

    #[test]
    fn capture_keeps_the_prefix_and_flags_overflow() {
        let mut capture = Capture::new(5);
        capture.push(b"abc");
        capture.push(b"defgh");
        capture.push(b"ijk");
        assert_eq!(capture.finish(), ("abcde".to_owned(), true));

        let mut exact = Capture::new(3);
        exact.push(b"abc");
        assert_eq!(exact.finish(), ("abc".to_owned(), false));

        let mut zero = Capture::new(0);
        zero.push(b"x");
        assert_eq!(zero.finish(), (String::new(), true));
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let text = "héllo ✓";
        for limit in 0..text.len() {
            let mut capture = Capture::new(limit);
            capture.push(text.as_bytes());
            let (out, truncated) = capture.finish();
            assert!(truncated);
            assert!(!out.contains('\u{FFFD}'), "limit {limit}: {out:?}");
            assert!(text.starts_with(&out), "limit {limit}: {out:?}");
        }
        // Invalid bytes that were not cut by the limit are still decoded lossily.
        let mut capture = Capture::new(16);
        capture.push(&[b'a', 0xFF, b'b']);
        assert_eq!(capture.finish(), ("a\u{FFFD}b".to_owned(), false));
    }

    #[test]
    fn incomplete_tails_are_measured() {
        assert_eq!(incomplete_utf8_tail(b""), 0);
        assert_eq!(incomplete_utf8_tail(b"abc"), 0);
        assert_eq!(incomplete_utf8_tail("é".as_bytes()), 0);
        assert_eq!(incomplete_utf8_tail(&"é".as_bytes()[..1]), 1);
        assert_eq!(incomplete_utf8_tail(&"✓".as_bytes()[..2]), 2);
        assert_eq!(incomplete_utf8_tail(&"😀".as_bytes()[..3]), 3);
    }

    #[test]
    fn requests_are_validated() {
        assert!(validate(&request("cmd.exe")).is_ok());
        let invalid = |req: ExecRequest| validate(&req).unwrap_err().code();
        use winwright_contracts::ErrorCode::InvalidRequest;

        assert_eq!(invalid(request("")), InvalidRequest);
        assert_eq!(invalid(request("cmd\0.exe")), InvalidRequest);
        assert_eq!(invalid(request(r"C:\tools\build.bat")), InvalidRequest);
        assert_eq!(invalid(request("deploy.CMD")), InvalidRequest);
        let mut req = request("cmd.exe");
        req.timeout_ms = 0;
        assert_eq!(invalid(req.clone()), InvalidRequest);
        req.timeout_ms = MAX_TIMEOUT_MS + 1;
        assert_eq!(invalid(req), InvalidRequest);
        let mut req = request("cmd.exe");
        req.max_output_bytes = MAX_OUTPUT_BYTES + 1;
        assert_eq!(invalid(req), InvalidRequest);
        let mut req = request("cmd.exe");
        req.args = vec!["a\0b".to_owned()];
        assert_eq!(invalid(req), InvalidRequest);
        let mut req = request("cmd.exe");
        req.working_dir = Some(r"C:\winwright\definitely\missing".into());
        assert_eq!(invalid(req), InvalidRequest);
    }

    #[test]
    fn program_label_hides_directories() {
        assert_eq!(program_label(r"C:\Users\someone\bin\tool.exe"), "tool.exe");
        assert_eq!(program_label("git"), "git");
    }
}
