//! Live overlay and hotkey checks. They flash small overlays for well under a second, never
//! inject input, and never capture the screen. Run with:
//! `cargo test -p winwright-overlay -- --ignored --test-threads=1`

use std::ffi::c_void;
use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, POINT};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetForegroundWindow, GetWindowLongW, HTTRANSPARENT, IsWindow, IsWindowVisible,
    SendMessageW, WM_NCHITTEST, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WindowFromPoint,
};
use winwright_contracts::ErrorCode;
use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::input::parse_chord;
use winwright_contracts::overlay::{OverlayRequest, OverlayService, OverlayStyle};
use winwright_overlay::NativeUi;

fn hwnd(value: u64) -> HWND {
    HWND(value as usize as *mut c_void)
}

fn is_window(value: u64) -> bool {
    // SAFETY: IsWindow accepts any handle value.
    unsafe { IsWindow(Some(hwnd(value))) }.as_bool()
}

fn request(rect: [i32; 4], style: OverlayStyle) -> OverlayRequest {
    OverlayRequest {
        rect: PhysicalRect::from(rect),
        style,
        label: None,
        step: None,
        color: 0x00E0_4A2A,
        duration_ms: None,
    }
}

#[test]
#[ignore = "needs an interactive desktop"]
fn highlight_is_click_through_never_steals_focus_and_clears() {
    let ui = NativeUi::start().unwrap();
    // SAFETY: no arguments.
    let foreground = unsafe { GetForegroundWindow() };

    let mut req = request([200, 200, 420, 260], OverlayStyle::Highlight);
    req.label = Some("Winwright live test".into());
    req.step = Some(1);
    let id = ui.overlay.show(req).unwrap();
    let handle = ui
        .overlay
        .window_handle(id)
        .unwrap()
        .expect("overlay window");
    let overlay = hwnd(handle);

    // SAFETY: plain queries on a window this process owns.
    let (exstyle, visible) = unsafe {
        (
            GetWindowLongW(overlay, GWL_EXSTYLE) as u32,
            IsWindowVisible(overlay).as_bool(),
        )
    };
    let required = WS_EX_LAYERED.0
        | WS_EX_TRANSPARENT.0
        | WS_EX_TOPMOST.0
        | WS_EX_TOOLWINDOW.0
        | WS_EX_NOACTIVATE.0;
    assert_eq!(exstyle & required, required, "exstyle {exstyle:#x}");
    assert!(visible);

    // Click-through: hit testing says "transparent" and point lookups skip the overlay.
    let center = POINT { x: 310, y: 230 };
    let packed = ((center.y as u32 as isize) << 16) | (center.x as u32 as u16 as isize);
    // SAFETY: a synchronous hit-test query to our own window; no input is synthesized.
    let hit = unsafe { SendMessageW(overlay, WM_NCHITTEST, None, Some(LPARAM(packed))) };
    assert_eq!(hit.0, HTTRANSPARENT as isize);
    // SAFETY: plain query.
    assert_ne!(unsafe { WindowFromPoint(center) }, overlay);

    sleep(Duration::from_millis(300));
    // SAFETY: no arguments.
    assert_eq!(unsafe { GetForegroundWindow() }, foreground, "focus moved");

    ui.overlay.clear(Some(id)).unwrap();
    assert_eq!(ui.overlay.window_handle(id).unwrap(), None);
    assert!(!is_window(handle));
    // Idempotent.
    ui.overlay.clear(Some(id)).unwrap();
}

#[test]
#[ignore = "needs an interactive desktop"]
fn duration_auto_hides() {
    let ui = NativeUi::start().unwrap();
    let mut req = request([300, 300, 360, 330], OverlayStyle::ClickMarker);
    req.duration_ms = Some(250);
    let id = ui.overlay.show(req).unwrap();
    let handle = ui
        .overlay
        .window_handle(id)
        .unwrap()
        .expect("overlay window");
    sleep(Duration::from_millis(600));
    assert_eq!(ui.overlay.window_handle(id).unwrap(), None);
    assert!(!is_window(handle));
}

#[test]
#[ignore = "needs an interactive desktop"]
fn every_style_renders_and_clear_all_removes_them() {
    let ui = NativeUi::start().unwrap();
    let mut arrow = request([5, 400, 80, 430], OverlayStyle::Arrow);
    arrow.label = Some("Arrow flips right at the screen edge".into());
    let mut marker = request([500, 400, 560, 430], OverlayStyle::ClickMarker);
    marker.step = Some(12);
    let mut near_top = request([700, 0, 900, 30], OverlayStyle::Highlight);
    near_top.label = Some("Label goes below".into());
    near_top.color = 0x00FF_D700;

    let ids = [arrow, marker, near_top].map(|r| ui.overlay.show(r).unwrap());
    let handles = ids.map(|id| ui.overlay.window_handle(id).unwrap().expect("rendered"));

    let offscreen = request([-32000, -32000, -31840, -31972], OverlayStyle::Highlight);
    let hidden = ui.overlay.show(offscreen).unwrap();
    assert_eq!(ui.overlay.window_handle(hidden).unwrap(), None);

    sleep(Duration::from_millis(300));
    ui.overlay.clear(None).unwrap();
    for (id, handle) in ids.into_iter().zip(handles) {
        assert_eq!(ui.overlay.window_handle(id).unwrap(), None);
        assert!(!is_window(handle));
    }
}

#[test]
#[ignore = "needs an interactive desktop"]
fn pointer_renders_and_the_click_watch_comes_and_goes() {
    let ui = NativeUi::start().unwrap();
    let mut req = request([600, 300, 700, 340], OverlayStyle::Pointer);
    req.label = Some("Click here".into());
    let id = ui.overlay.show(req).unwrap();
    let handle = ui
        .overlay
        .window_handle(id)
        .unwrap()
        .expect("pointer window");
    assert!(is_window(handle));
    // It glides in from the cursor and settles with its tip just right of and below the
    // target's center (650, 320), the window starting a few pixels up-left of the tip.
    // Physical pixels, as Winwright itself works (a DPI-unaware caller sees scaled ones).
    // SAFETY: affects only this test thread.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    sleep(Duration::from_millis(500));
    let landed = ui.overlay.window_rect(id).unwrap().expect("pointer window");
    assert!(
        (630..=680).contains(&landed.left) && (300..=335).contains(&landed.top),
        "{landed:?}"
    );

    let first = ui.overlay.watch_pointer(Box::new(|_| {})).unwrap();
    let second = ui.overlay.watch_pointer(Box::new(|_| {})).unwrap();
    drop(first);
    drop(second);
    // The hook went with the last watcher and comes back for the next one.
    drop(ui.overlay.watch_pointer(Box::new(|_| {})).unwrap());

    sleep(Duration::from_millis(300));
    ui.overlay.clear(None).unwrap();
    assert_eq!(ui.overlay.window_handle(id).unwrap(), None);
    assert!(!is_window(handle));
}

#[test]
#[ignore = "needs an interactive desktop"]
fn shutdown_destroys_overlays_and_rejects_later_shows() {
    let ui = NativeUi::start().unwrap();
    let id = ui
        .overlay
        .show(request([240, 520, 300, 560], OverlayStyle::Highlight))
        .unwrap();
    let handle = ui
        .overlay
        .window_handle(id)
        .unwrap()
        .expect("overlay window");
    ui.hotkeys.shutdown();
    assert!(!is_window(handle));
    let err = ui
        .overlay
        .show(request([0, 0, 10, 10], OverlayStyle::Highlight))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::BackendUnavailable);
    // The cancellation path must never fail.
    ui.overlay.clear(None).unwrap();
}

#[test]
#[ignore = "needs an interactive desktop"]
fn hotkey_registration_reports_conflicts() {
    let ui = NativeUi::start().unwrap();
    let chord = parse_chord("Ctrl+Alt+Shift+F12").unwrap();

    let id = ui.hotkeys.register(&chord, Box::new(|| {})).unwrap();

    let again = ui.hotkeys.register(&chord, Box::new(|| {})).unwrap_err();
    assert_eq!(again.code(), ErrorCode::InvalidRequest);
    let text = again.to_string();
    assert!(
        text.contains("Ctrl+Alt+Shift+F12") && text.contains("already registered"),
        "{text}"
    );

    // A second native UI thread hits the OS-level conflict.
    let other = NativeUi::start().unwrap();
    let os = other.hotkeys.register(&chord, Box::new(|| {})).unwrap_err();
    assert_eq!(os.code(), ErrorCode::InvalidRequest);
    let text = os.to_string();
    assert!(
        text.contains("Ctrl+Alt+Shift+F12") && text.contains("another application"),
        "{text}"
    );

    ui.hotkeys.unregister(id).unwrap();
    ui.hotkeys.unregister(id).unwrap();
    // Freed: the other instance can take it now.
    let taken = other.hotkeys.register(&chord, Box::new(|| {})).unwrap();
    other.hotkeys.unregister(taken).unwrap();

    let stop = ui
        .hotkeys
        .register_emergency_stop(None, Box::new(|| {}))
        .unwrap();
    ui.hotkeys.unregister(stop).unwrap();

    let bad = ui
        .hotkeys
        .register_emergency_stop(Some("Escape"), Box::new(|| {}))
        .unwrap_err();
    assert_eq!(bad.code(), ErrorCode::InvalidRequest);
}
