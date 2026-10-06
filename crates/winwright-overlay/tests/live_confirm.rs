//! The real confirmation dialog denies and closes itself when nobody answers, and only a
//! person's own click or key press can approve it.
//! Opt-in: `cargo test -p winwright-overlay --test live_confirm -- --ignored --test-threads=1 --skip manual_`
//! By hand (click "Allow once" yourself): `... --test live_confirm -- --ignored manual_`

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
use windows::Win32::UI::WindowsAndMessaging::{
    BM_CLICK, BN_CLICKED, FindWindowExW, FindWindowW, GetForegroundWindow, PostMessageW,
    SendMessageW, WM_COMMAND,
};
use windows::core::{PCWSTR, w};
use winwright_contracts::security::{Approval, ConfirmationPrompt, Confirmer};
use winwright_overlay::NativeConfirmer;

/// The open dialog and its "Allow once" button, once it appears.
fn find_dialog() -> (HWND, HWND) {
    let started = Instant::now();
    // SAFETY: looks up windows by class and title; no handle is dereferenced.
    unsafe {
        let dialog = loop {
            if let Ok(h) = FindWindowW(w!("WinwrightConfirm"), PCWSTR::null()) {
                break h;
            }
            assert!(started.elapsed() < Duration::from_secs(3), "no dialog");
            std::thread::sleep(Duration::from_millis(20));
        };
        // The buttons come just after the window: wait for them too.
        let allow = loop {
            if let Ok(b) = FindWindowExW(Some(dialog), None, w!("BUTTON"), w!("Allow once")) {
                break b;
            }
            assert!(started.elapsed() < Duration::from_secs(3), "no Allow button");
            std::thread::sleep(Duration::from_millis(20));
        };
        (dialog, allow)
    }
}

/// In front, as after a person switches to it: a test process may not take the foreground
/// itself, so this borrows the foreground thread's input state the way Winwright's own window
/// focusing does.
/// Retries for a moment: right after an earlier test's dialog closed, the foreground is still
/// moving and Windows refuses the switch.
fn bring_to_front(dialog: HWND) {
    let windows = winwright_win32::Win32Windows;
    let started = Instant::now();
    while winwright_contracts::backend::WindowBackend::focus_window(&windows, dialog.0 as u64)
        .is_err()
        && started.elapsed() < Duration::from_millis(1_500)
    {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The window in front before the test, handed back while our dialog still holds the
/// foreground (and so may give it away): once the dialog closes, Windows refuses the next test
/// the switch it needs.
fn foreground_now() -> isize {
    // SAFETY: reads the foreground window handle.
    unsafe { GetForegroundWindow() }.0 as isize
}

fn give_back(window: isize) {
    if window != 0 {
        bring_to_front(HWND(window as *mut std::ffi::c_void));
    }
}

fn prompt(reason: &str, timeout_ms: u64) -> ConfirmationPrompt {
    ConfirmationPrompt {
        summary: "Winwright self-test (no action will run)".into(),
        target: None,
        reason: reason.into(),
        timeout_ms,
        grant: None,
    }
}

/// One dialog, brought to the front once: Windows lets a test process take the foreground
/// only now and then, so arming and click origins are checked on the same dialog.
#[tokio::test]
#[ignore = "shows a dialog on the interactive desktop for about four seconds"]
async fn allow_arms_only_in_front_and_ignores_clicks_no_person_made() {
    let original = foreground_now();
    let checker = std::thread::spawn(move || {
        let (dialog, allow) = find_dialog();
        // SAFETY: reads window state; a stale handle only gives a wrong answer.
        let state = || unsafe {
            (
                GetForegroundWindow() == dialog,
                IsWindowEnabled(allow).as_bool(),
            )
        };
        std::thread::sleep(Duration::from_millis(1_200));
        let before = state();
        bring_to_front(dialog);
        std::thread::sleep(Duration::from_millis(1_200));
        let after = state();
        if after == (true, true) {
            // Armed and in front: only where a click comes from can refuse it now. This is
            // what another process, or UI Automation's Invoke on a Win32 button, can do.
            // SAFETY: messages the dialog this test opened; a handle that went stale in
            // between only makes a call fail.
            unsafe {
                SendMessageW(allow, BM_CLICK, None, None);
                let _ = PostMessageW(Some(allow), BM_CLICK, WPARAM(0), LPARAM(0));
                let _ = PostMessageW(
                    Some(dialog),
                    WM_COMMAND,
                    WPARAM(((BN_CLICKED as usize) << 16) | 100),
                    LPARAM(allow.0 as isize),
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        give_back(original);
        (before, after)
    });
    let approved = NativeConfirmer::new()
        .confirm(prompt(
            "automated test: arming, and clicks no person made",
            4_000,
        ))
        .await
        .unwrap();
    let ((was_active, was_enabled), (active, enabled)) = checker.join().unwrap();
    assert_eq!(
        was_enabled, was_active,
        "\"Allow once\" is enabled exactly when the dialog is active"
    );
    assert!(
        active && enabled,
        "the dialog never came to the front armed, so this run cannot test where clicks come          from; rerun while the desktop is free"
    );
    assert!(!approved, "a click no person made approved the action");
    assert!(
        was_active && was_enabled,
        "the dialog did not come to the front and arm by itself, so a person's first click \
         would only activate it"
    );
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
            grant: None,
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
        .confirm(prompt("automated test of the auto-deny timeout", 1_000))
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

#[tokio::test]
#[ignore = "shows a dialog on the interactive desktop for about four seconds"]
async fn the_ten_minute_button_arms_and_refuses_clicks_no_person_made_too() {
    let original = foreground_now();
    let checker = std::thread::spawn(move || {
        let started = Instant::now();
        let dialog = loop {
            // SAFETY: looks up a window by class; no handle is dereferenced.
            if let Ok(h) = unsafe { FindWindowW(w!("WinwrightConfirm"), PCWSTR::null()) } {
                break h;
            }
            assert!(started.elapsed() < Duration::from_secs(3), "no dialog");
            std::thread::sleep(Duration::from_millis(20));
        };
        // The dialog makes this button right after "Allow once": wait for it.
        let started = Instant::now();
        let button = loop {
            // SAFETY: looks up a child button by its text; no handle is dereferenced.
            if let Ok(b) = unsafe {
                FindWindowExW(
                    Some(dialog),
                    None,
                    w!("BUTTON"),
                    w!("Allow in Notepad for 10 minutes"),
                )
            } {
                break b;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "the dialog offers the ten-minute button"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        bring_to_front(dialog);
        std::thread::sleep(Duration::from_millis(1_200));
        // SAFETY: reads window state; a stale handle only gives a wrong answer.
        let armed = unsafe { IsWindowEnabled(button).as_bool() };
        if armed {
            // SAFETY: messages the dialog this test opened; a stale handle only fails a call.
            unsafe {
                SendMessageW(button, BM_CLICK, None, None);
                let _ = PostMessageW(
                    Some(dialog),
                    WM_COMMAND,
                    WPARAM(((BN_CLICKED as usize) << 16) | 101),
                    LPARAM(button.0 as isize),
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        give_back(original);
        armed
    });
    let mut p = prompt("automated test: the ten-minute button", 4_000);
    p.grant = Some("Notepad".into());
    let answer = NativeConfirmer::new().approve(p).await.unwrap();
    let armed = checker.join().unwrap();
    assert!(
        armed,
        "the button never armed in front; rerun while the desktop is free"
    );
    assert_eq!(
        answer,
        Approval::Denied,
        "a click no person made allowed for a while"
    );
}
