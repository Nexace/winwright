//! The real confirmation dialog denies and closes itself when nobody answers, and only a
//! person's own click or key press can approve it.
//! Opt-in: `cargo test -p winwright-overlay --test live_confirm -- --ignored --test-threads=1 --skip manual_`
//! By hand (click "Allow once" yourself): `... --test live_confirm -- --ignored manual_`

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_CLICK, BN_CLICKED, FindWindowExW, FindWindowW, PostMessageW, SendMessageW, WM_COMMAND,
};
use windows::core::{PCWSTR, w};
use winwright_contracts::security::{ConfirmationPrompt, Confirmer};
use winwright_overlay::NativeConfirmer;

#[tokio::test]
#[ignore = "shows a dialog on the interactive desktop for about three seconds"]
async fn clicks_no_person_made_cannot_allow() {
    let clicker = std::thread::spawn(|| {
        let started = Instant::now();
        // SAFETY: finds and messages the dialog this test opened; a handle that went stale
        // in between only makes a call fail.
        unsafe {
            let dialog = loop {
                if let Ok(h) = FindWindowW(w!("WinwrightConfirm"), PCWSTR::null()) {
                    break h;
                }
                assert!(started.elapsed() < Duration::from_secs(3), "no dialog");
                std::thread::sleep(Duration::from_millis(20));
            };
            let allow = FindWindowExW(Some(dialog), None, w!("BUTTON"), w!("Allow once")).unwrap();
            // Past the arm delay, so only where the click came from can refuse it.
            std::thread::sleep(Duration::from_millis(1_200));
            // What another process, or UI Automation's Invoke on a Win32 button, can do.
            SendMessageW(allow, BM_CLICK, None, None);
            let _ = PostMessageW(Some(allow), BM_CLICK, WPARAM(0), LPARAM(0));
            let _ = PostMessageW(
                Some(dialog),
                WM_COMMAND,
                WPARAM(((BN_CLICKED as usize) << 16) | 100),
                LPARAM(allow.0 as isize),
            );
        }
    });
    let approved = NativeConfirmer::new()
        .confirm(ConfirmationPrompt {
            summary: "Winwright self-test (no action will run)".into(),
            target: None,
            reason: "automated test: programmatic clicks must not approve".into(),
            timeout_ms: 3_000,
        })
        .await
        .unwrap();
    clicker.join().unwrap();
    assert!(!approved, "a click no person made approved the action");
}

#[tokio::test]
#[ignore = "manual: a person must click \"Allow once\" within 20 s"]
async fn manual_a_real_click_allows() {
    let approved = NativeConfirmer::new()
        .confirm(ConfirmationPrompt {
            summary: "Winwright self-test: click \"Allow once\" (no action will run)".into(),
            target: None,
            reason: "manual test that a real click still approves".into(),
            timeout_ms: 20_000,
        })
        .await
        .unwrap();
    assert!(approved, "a real click on \"Allow once\" did not approve");
}

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
