//! The real confirmation dialog denies and closes itself when nobody answers.
//! Opt-in: `cargo test -p winwright-overlay --test live_confirm -- --ignored`

use std::time::{Duration, Instant};

use winwright_contracts::security::{ConfirmationPrompt, Confirmer};
use winwright_overlay::NativeConfirmer;

#[tokio::test]
#[ignore = "shows a dialog on the interactive desktop for about a second"]
async fn unanswered_prompt_is_denied_and_closed() {
    let started = Instant::now();
    let approved = NativeConfirmer::new()
        .confirm(ConfirmationPrompt {
            summary: "Winwright self-test (no action will run)".into(),
            target: None,
            reason: "automated test of the auto-deny timeout".into(),
            timeout_ms: 1_000,
        })
        .await
        .unwrap();
    assert!(!approved, "silence must mean no");
    assert!(started.elapsed() < Duration::from_secs(5));
    // The dialog must be gone, not left waiting for a click.
    std::thread::sleep(Duration::from_millis(300));
    let pid = std::process::id();
    let still_open = winwright_win32::Win32Windows;
    let windows = winwright_contracts::backend::WindowBackend::list_windows(&still_open).unwrap();
    assert!(
        !windows
            .iter()
            .any(|w| w.process_id == pid && w.title.starts_with("Winwright: confirm")),
        "dialog was left open"
    );
}
