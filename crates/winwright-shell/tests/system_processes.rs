//! Headless integration tests: `exec` runs harmless console commands with no window, and
//! `list` reads the process table. `launch` tests open real windows and are ignored.

use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use winwright_contracts::ErrorCode;
use winwright_contracts::backend::OperationContext;
use winwright_contracts::ids::SessionId;
use winwright_contracts::system::{ExecRequest, LaunchRequest, ProcessService};
use winwright_shell::{SystemProcesses, UNKNOWN_SESSION};

fn ctx(timeout: Duration) -> OperationContext {
    OperationContext::new(
        SessionId::parse("shell-tests").unwrap(),
        timeout,
        CancellationToken::new(),
    )
}

fn exec_request(program: &str, args: &[&str]) -> ExecRequest {
    ExecRequest {
        program: program.to_owned(),
        args: args.iter().map(|a| (*a).to_owned()).collect(),
        working_dir: None,
        timeout_ms: 30_000,
        max_output_bytes: 64 * 1024,
    }
}

#[tokio::test]
async fn exec_captures_stdout_and_exit_code() {
    let result = SystemProcesses::new()
        .exec(
            exec_request("cmd.exe", &["/c", "echo", "hello"]),
            &ctx(Duration::from_secs(30)),
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.trim(), "hello");
    assert_eq!(result.stderr, "");
    assert!(!result.timed_out);
    assert!(!result.truncated);
}

#[tokio::test]
async fn exec_captures_stderr_and_nonzero_exit() {
    let result = SystemProcesses::new()
        .exec(
            exec_request("cmd.exe", &["/c", "echo oops 1>&2 & exit /b 3"]),
            &ctx(Duration::from_secs(30)),
        )
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(3));
    assert_eq!(result.stderr.trim(), "oops");
}

#[tokio::test]
async fn exec_times_out_and_kills_the_child() {
    let mut request = exec_request("ping", &["-n", "10", "127.0.0.1"]);
    request.timeout_ms = 500;
    let started = Instant::now();
    let result = SystemProcesses::new()
        .exec(request, &ctx(Duration::from_secs(30)))
        .await
        .unwrap();
    assert!(result.timed_out);
    assert_eq!(result.exit_code, None);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(result.duration_ms >= 500, "{}", result.duration_ms);
}

#[tokio::test]
async fn exec_honors_an_earlier_context_deadline() {
    let result = SystemProcesses::new()
        .exec(
            exec_request("ping", &["-n", "10", "127.0.0.1"]),
            &ctx(Duration::from_millis(400)),
        )
        .await
        .unwrap();
    assert!(result.timed_out);
    assert!(result.duration_ms < 5_000, "{}", result.duration_ms);
}

#[tokio::test]
async fn exec_cancellation_kills_and_reports_cancelled() {
    let context = ctx(Duration::from_secs(30));
    let cancel = context.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    let started = Instant::now();
    let err = SystemProcesses::new()
        .exec(exec_request("ping", &["-n", "10", "127.0.0.1"]), &context)
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn exec_output_is_capped_while_the_child_keeps_running_to_completion() {
    // ~60 KB of output, far beyond the pipe buffer: the reader must keep draining past the cap.
    let mut request = exec_request(
        "cmd.exe",
        &["/c", "for /l %i in (1,1,5000) do @echo line %i"],
    );
    request.max_output_bytes = 100;
    let result = SystemProcesses::new()
        .exec(request, &ctx(Duration::from_secs(60)))
        .await
        .unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert!(result.truncated);
    assert_eq!(result.stdout.len(), 100);
    assert!(result.stdout.starts_with("line 1"));
}

#[tokio::test]
async fn exec_rejects_missing_programs_and_batch_files() {
    let context = ctx(Duration::from_secs(10));
    let missing = SystemProcesses::new()
        .exec(
            exec_request("winwright-no-such-program-4711", &[]),
            &context,
        )
        .await
        .unwrap_err();
    assert_eq!(missing.code(), ErrorCode::InvalidRequest);
    let batch = SystemProcesses::new()
        .exec(exec_request("build.bat", &[]), &context)
        .await
        .unwrap_err();
    assert_eq!(batch.code(), ErrorCode::InvalidRequest);
}

#[test]
fn list_contains_the_current_process() {
    let own = std::process::id();
    let processes = SystemProcesses::new().list().unwrap();
    assert!(processes.len() > 10);
    let me = processes
        .iter()
        .find(|p| p.process_id == own)
        .expect("current process listed");
    assert!(matches!(me.integrity.as_deref(), Some("medium" | "high")));
    assert!(me.path.is_some());
    assert_ne!(me.session_id, UNKNOWN_SESSION);
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn launch_starts_notepad_as_a_process() {
    let result = SystemProcesses::new()
        .launch(
            LaunchRequest {
                app: "notepad".to_owned(),
                args: Vec::new(),
                working_dir: None,
            },
            &ctx(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(result.method, "process");
    let pid = result.process_id.expect("pid");
    // Close only the instance this test started.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn launch_opens_a_folder_through_the_shell() {
    let result = SystemProcesses::new()
        .launch(
            LaunchRequest {
                app: std::env::temp_dir().to_string_lossy().into_owned(),
                args: Vec::new(),
                working_dir: None,
            },
            &ctx(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(result.method, "shell");
}

#[tokio::test]
#[ignore = "needs an interactive desktop"]
async fn launch_opens_a_settings_uri() {
    let result = SystemProcesses::new()
        .launch(
            LaunchRequest {
                app: "ms-settings:display".to_owned(),
                args: Vec::new(),
                working_dir: None,
            },
            &ctx(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    assert_eq!(result.method, "shell");
}
