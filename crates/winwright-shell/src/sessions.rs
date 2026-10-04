//! Background sessions: a program whose output is kept for later reads and which can be sent
//! input (a build, a dev server, a REPL). Each runs in a kill-on-close job, so stopping a
//! session, or Winwright exiting, ends its whole process tree.

use std::collections::HashMap;
use std::fmt;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStdin, Command};
use tokio::sync::Notify;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use winwright_contracts::system::{ExecRequest, SessionInfo, SessionOutput, SessionStart};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::exec::{
    KillOnCloseJob, READ_CHUNK, incomplete_utf8_tail, program_label, resolve_program, spawn_error,
};

/// Sessions open at once, finished ones included until stopped.
pub(crate) const MAX_SESSIONS: usize = 8;
/// Unread output kept per session; the oldest goes first.
const MAX_UNREAD_BYTES: usize = 1024 * 1024;
/// Output comes in bursts: a read returns once it has paused this long.
const QUIET: Duration = Duration::from_millis(300);
/// Longest a read waits.
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(crate) struct Sessions {
    next: AtomicU32,
    live: Mutex<HashMap<u32, Arc<Running>>>,
}

impl fmt::Debug for Sessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sessions")
            .field("open", &self.table().len())
            .finish()
    }
}

struct Running {
    id: u32,
    program: String,
    pid: Option<u32>,
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    unread: Mutex<Unread>,
    /// A permit per change (output, a stream closing, the exit), so no wake-up is lost.
    changed: Notify,
    /// Output streams still open.
    open_streams: AtomicUsize,
    /// `Some(code)` once the process has exited (`code` is `None` when it was killed).
    exit: Mutex<Option<Option<i32>>>,
    kill: Notify,
    job: Option<KillOnCloseJob>,
}

impl Running {
    fn info(&self) -> SessionInfo {
        let exit = *lock(&self.exit);
        SessionInfo {
            id: self.id,
            program: self.program.clone(),
            process_id: self.pid,
            running: exit.is_none(),
            exit_code: exit.flatten(),
        }
    }

    /// Exited, and both output streams drained.
    fn finished(&self) -> bool {
        lock(&self.exit).is_some() && self.open_streams.load(Ordering::Acquire) == 0
    }

    fn end(&self) {
        if let Some(job) = &self.job {
            job.terminate();
        }
        self.kill.notify_one();
    }
}

/// Unread output, newest kept when it overflows.
#[derive(Default)]
struct Unread {
    bytes: Vec<u8>,
    dropped: bool,
}

impl Unread {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > MAX_UNREAD_BYTES {
            let cut = self.bytes.len() - MAX_UNREAD_BYTES;
            self.bytes.drain(..cut);
            self.dropped = true;
        }
    }

    /// The unread text; a character still arriving stays for the next read.
    fn take(&mut self) -> (String, bool) {
        let end = self.bytes.len() - incomplete_utf8_tail(&self.bytes);
        let text = String::from_utf8_lossy(&self.bytes[..end]).into_owned();
        self.bytes.drain(..end);
        (text, std::mem::take(&mut self.dropped))
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Sessions {
    fn table(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Arc<Running>>> {
        lock(&self.live)
    }

    fn get(&self, id: u32) -> WinwrightResult<Arc<Running>> {
        self.table().get(&id).cloned().ok_or_else(|| {
            WinwrightError::invalid(format!(
                "no session {id}: list sessions to see the open ones"
            ))
        })
    }

    /// Starts `request` as a new session. Must run inside the Tokio runtime.
    pub(crate) fn start(&self, request: &SessionStart) -> WinwrightResult<SessionInfo> {
        let exec = ExecRequest {
            program: request.program.clone(),
            args: request.args.clone(),
            working_dir: request.working_dir.clone(),
            timeout_ms: 1,
            max_output_bytes: 0,
        };
        let image = resolve_program(&exec)?;
        {
            let mut table = self.table();
            // Finished sessions whose output was all read make room first.
            table.retain(|_, s| !(s.finished() && lock(&s.unread).bytes.is_empty()));
            if table.len() >= MAX_SESSIONS {
                return Err(WinwrightError::invalid(format!(
                    "{MAX_SESSIONS} sessions are open: stop one first"
                )));
            }
        }
        let program = program_label(&request.program);
        let mut command = Command::new(&image);
        command
            .args(&request.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW.0)
            .kill_on_drop(true);
        if let Some(dir) = &request.working_dir {
            command.current_dir(dir);
        }
        let mut child = command.spawn().map_err(|e| spawn_error(&program, &e))?;
        let job = KillOnCloseJob::assign(&child);
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let session = Arc::new(Running {
            id,
            program,
            pid: child.id(),
            stdin: tokio::sync::Mutex::new(child.stdin.take()),
            unread: Mutex::default(),
            changed: Notify::new(),
            open_streams: AtomicUsize::new(2),
            exit: Mutex::new(None),
            kill: Notify::new(),
            job,
        });
        tokio::spawn(pump(child.stdout.take(), Arc::clone(&session)));
        tokio::spawn(pump(child.stderr.take(), Arc::clone(&session)));
        let waiter = Arc::clone(&session);
        tokio::spawn(async move {
            let status = tokio::select! {
                status = child.wait() => status.ok(),
                () = waiter.kill.notified() => {
                    let _ = child.start_kill();
                    child.wait().await.ok()
                }
            };
            *lock(&waiter.exit) = Some(status.and_then(|s| s.code()));
            waiter.changed.notify_one();
        });
        tracing::info!(id, program = %session.program, "session started");
        let info = session.info();
        self.table().insert(id, session);
        Ok(info)
    }

    pub(crate) async fn input(&self, id: u32, text: &str) -> WinwrightResult<()> {
        let session = self.get(id)?;
        let mut stdin = session.stdin.lock().await;
        let closed = || {
            WinwrightError::invalid(format!(
                "session {id} takes no more input: {} has exited",
                session.program
            ))
        };
        let pipe = stdin.as_mut().ok_or_else(closed)?;
        let written = async {
            pipe.write_all(text.as_bytes()).await?;
            pipe.flush().await
        }
        .await;
        if written.is_err() {
            *stdin = None;
            return Err(closed());
        }
        Ok(())
    }

    pub(crate) async fn read(&self, id: u32, wait: Duration) -> WinwrightResult<SessionOutput> {
        let session = self.get(id)?;
        let deadline = Instant::now() + wait.min(MAX_WAIT);
        loop {
            if session.finished() {
                break;
            }
            let pending = !lock(&session.unread).bytes.is_empty();
            let left = deadline.saturating_duration_since(Instant::now());
            // Before any output, wait for some; after, only until it pauses.
            let limit = if pending { QUIET.min(left) } else { left };
            if limit.is_zero()
                || tokio::time::timeout(limit, session.changed.notified())
                    .await
                    .is_err()
            {
                break;
            }
        }
        let (output, dropped) = lock(&session.unread).take();
        Ok(SessionOutput {
            session: session.info(),
            output,
            dropped,
        })
    }

    pub(crate) fn list(&self) -> Vec<SessionInfo> {
        let mut all: Vec<SessionInfo> = self.table().values().map(|s| s.info()).collect();
        all.sort_by_key(|s| s.id);
        all
    }

    pub(crate) fn stop(&self, id: u32) -> WinwrightResult<SessionInfo> {
        let session = self.get(id)?;
        session.end();
        self.table().remove(&id);
        tracing::info!(id, "session stopped");
        // Its tree has been ended; the exit is recorded a moment later.
        Ok(SessionInfo {
            running: false,
            ..session.info()
        })
    }

    pub(crate) fn stop_all(&self) {
        for (_, session) in self.table().drain() {
            session.end();
        }
    }
}

/// Copies a stream into the session's unread output until it closes.
async fn pump<R: AsyncRead + Unpin>(stream: Option<R>, session: Arc<Running>) {
    if let Some(mut stream) = stream {
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    lock(&session.unread).push(&chunk[..n]);
                    session.changed.notify_one();
                }
            }
        }
    }
    session.open_streams.fetch_sub(1, Ordering::AcqRel);
    session.changed.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unread_output_keeps_the_newest_and_whole_characters() {
        let mut unread = Unread::default();
        unread.push(b"hello ");
        unread.push(&"wörld".as_bytes()[..2]);
        assert_eq!(unread.take(), ("hello w".to_owned(), false));
        unread.push(&"wörld".as_bytes()[2..]);
        assert_eq!(unread.take(), ("örld".to_owned(), false));
        unread.push(&vec![b'a'; MAX_UNREAD_BYTES + 10]);
        let (text, dropped) = unread.take();
        assert!(dropped);
        assert_eq!(text.len(), MAX_UNREAD_BYTES);
        assert_eq!(unread.take(), (String::new(), false));
    }

    fn start(sessions: &Sessions, program: &str, args: &[&str]) -> SessionInfo {
        sessions
            .start(&SessionStart {
                program: program.to_owned(),
                args: args.iter().map(|a| (*a).to_owned()).collect(),
                working_dir: None,
            })
            .unwrap()
    }

    #[tokio::test]
    async fn a_session_takes_input_and_keeps_output_for_reading() {
        let sessions = Sessions::default();
        let info = start(&sessions, "cmd.exe", &["/q", "/k", "echo ready"]);
        assert!(info.running && info.process_id.is_some());
        let first = sessions
            .read(info.id, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(first.output.contains("ready"), "{first:?}");
        sessions
            .input(info.id, "echo winwright-%COMPUTERNAME:~0,0%session\r\n")
            .await
            .unwrap();
        let echoed = sessions
            .read(info.id, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(echoed.output.contains("winwright-session"), "{echoed:?}");
        sessions.input(info.id, "exit 7\r\n").await.unwrap();
        let last = sessions
            .read(info.id, Duration::from_secs(10))
            .await
            .unwrap();
        assert!(!last.session.running, "{last:?}");
        assert_eq!(last.session.exit_code, Some(7));
        let err = sessions.input(info.id, "echo late\r\n").await.unwrap_err();
        assert!(err.to_string().contains("has exited"), "{err}");
        sessions.stop(info.id).unwrap();
        assert!(sessions.list().is_empty());
    }

    #[tokio::test]
    async fn stopping_a_session_ends_its_process_tree() {
        let sessions = Sessions::default();
        let info = start(&sessions, "PING.EXE", &["-n", "60", "127.0.0.1"]);
        let pid = info.process_id.unwrap();
        assert_eq!(sessions.list().len(), 1);
        let stopped = sessions.stop(info.id).unwrap();
        assert_eq!(stopped.id, info.id);
        assert!(!stopped.running, "{stopped:?}");
        // The process is gone within a moment.
        let gone = async {
            while crate::processes::list()
                .unwrap()
                .iter()
                .any(|p| p.process_id == pid)
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), gone)
            .await
            .expect("ping ended");
        let missing = sessions.read(info.id, Duration::ZERO).await.unwrap_err();
        assert!(missing.to_string().contains("no session"), "{missing}");
    }
}
