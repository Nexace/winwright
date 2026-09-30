//! Real-input tests. They move the real cursor and press real keys, so they are opt-in:
//! `cargo test -p winwright-input --test live_input -- --ignored --test-threads=1`
//! on an unlocked interactive session with nothing important focused. Fixture-app scenarios
//! (click, drag, scroll, type into a canvas) belong to the fixture suites.

use std::time::Duration;

use tokio_util::sync::CancellationToken;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
use winwright_contracts::backend::{OperationContext, WindowBackend};
use winwright_contracts::geometry::PhysicalPoint;
use winwright_contracts::ids::SessionId;
use winwright_contracts::input::{InputBackend, Key};
use winwright_input::SendInputBackend;
use winwright_win32::Win32Windows;

fn ctx() -> OperationContext {
    OperationContext::new(
        SessionId::parse("live").unwrap(),
        Duration::from_secs(10),
        CancellationToken::new(),
    )
}

#[tokio::test]
#[ignore = "injects real input"]
async fn move_to_lands_on_exact_primary_monitor_pixels() {
    winwright_win32::enable_per_monitor_dpi_awareness();
    let windows = Win32Windows;
    let input = SendInputBackend::new();
    let original = windows.cursor_position().unwrap();
    // SAFETY: GetSystemMetrics only reads system-wide values.
    let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    // The primary monitor spans (0, 0)..(cx, cy); the virtual screen may extend further,
    // including to negative coordinates, which exercises the normalization offset.
    let targets = [
        (0, 0),
        (1, 1),
        (cx - 1, cy - 1),
        (cx / 2, cy / 2),
        (cx / 3, cy - 2),
        (37, 29),
    ];
    let c = ctx();
    for (x, y) in targets {
        let target = PhysicalPoint { x, y };
        input.move_to(target, &c).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(windows.cursor_position().unwrap(), target);
    }
    input.move_to(original, &c).await.unwrap();
}

#[tokio::test]
#[ignore = "injects real input"]
async fn chord_leaves_nothing_held_and_release_all_is_idempotent() {
    let input = SendInputBackend::new();
    input.press_keys(&[Key::Shift], &ctx()).await.unwrap();
    // SAFETY: reads the asynchronous key state; no pointers involved.
    let shift_down = unsafe { GetAsyncKeyState(i32::from(VK_SHIFT.0)) } < 0;
    assert!(!shift_down, "Shift was released");
    input.release_all().unwrap();
    input.release_all().unwrap();
}
