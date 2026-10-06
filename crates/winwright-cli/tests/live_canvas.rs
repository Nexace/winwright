//! Phase 7 acceptance (spec §50): the custom canvas fixture has no accessible controls, so
//! every action falls back to real mouse and keyboard input (SendInput). The canvas reports
//! each input it receives in its title.
//! Opt-in: it moves the real cursor and presses real keys for a few seconds:
//! `cargo test -p winwright-cli --test live_canvas -- --ignored --test-threads=1`

use std::sync::Arc;
use std::time::Duration;

use winwright_contracts::action::{
    ActionMethod, DesktopAction, ElementTarget, ScreenPoint, ScrollDirection,
};
use winwright_contracts::backend::WindowBackend;
use winwright_contracts::config::Config;
use winwright_contracts::input::{InputBackend, Key, MouseButton};
use winwright_contracts::locator::FindRequest;
use winwright_contracts::snapshot::SnapshotTarget;
use winwright_contracts::window::WindowSelector;
use winwright_core::Engine;
use winwright_test_support::{Fixture, FixtureProcess};

#[tokio::test]
#[ignore = "moves the real mouse and types into the canvas fixture for a few seconds"]
async fn phase7_canvas_is_driven_with_real_input() {
    winwright_win32::enable_per_monitor_dpi_awareness();
    let windows = winwright_win32::Win32Windows;
    let cursor = windows.cursor_position().unwrap();
    let fx = FixtureProcess::launch(Fixture::Canvas).expect("canvas launches");
    let input = Arc::new(winwright_input::SendInputBackend::new());
    let engine = Engine::new(
        Config::default(),
        Arc::new(winwright_win32::Win32Windows),
        Arc::new(winwright_uia::UiaBackend::start().expect("UIA worker")),
    )
    .with_input(input.clone());
    let session = engine
        .session(
            &winwright_contracts::ids::SessionId::parse("live").unwrap(),
            "test",
        )
        .unwrap();

    // UI Automation sees one empty window: that is the only thing to target.
    let mut find: FindRequest =
        serde_json::from_value(serde_json::json!({"role": "Window"})).unwrap();
    find.scope = SnapshotTarget::Window(WindowSelector {
        hwnd: Some(fx.hwnd),
        ..Default::default()
    });
    let found = engine.find(&session, find).await.unwrap();
    let canvas = found
        .matches
        .first()
        .expect("the canvas window")
        .reference
        .clone();
    let target = || ElementTarget::by_ref(canvas.clone());
    let event = |want: &str| {
        let title = fx.wait_for_title(|t| t.ends_with(want), Duration::from_secs(3));
        assert!(
            title.is_some(),
            "want {want:?}, title is {:?}",
            fx.window_title()
        );
    };

    // The window's centre lies in the green square.
    let clicked = engine
        .execute(
            &session,
            DesktopAction::Click {
                target: target(),
                button: MouseButton::Left,
                click_count: 1,
                force_physical: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(clicked.method, ActionMethod::PhysicalClick, "{clicked:?}");
    event(" - clicked green");

    for (button, count, want) in [
        (MouseButton::Right, 1, " - right-clicked green"),
        (MouseButton::Left, 2, " - double-clicked green"),
    ] {
        engine
            .execute(
                &session,
                DesktopAction::Click {
                    target: target(),
                    button,
                    click_count: count,
                    force_physical: false,
                },
            )
            .await
            .unwrap();
        event(want);
    }

    let typed = engine
        .execute(
            &session,
            DesktopAction::TypeText {
                target: Some(target()),
                text: "hi".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(typed.method, ActionMethod::PhysicalKeyboard, "{typed:?}");
    event(" - typed: hi");

    engine
        .execute(
            &session,
            DesktopAction::Press {
                target: Some(target()),
                keys: vec![Key::Ctrl, Key::Char('k')],
            },
        )
        .await
        .unwrap();
    event(" - chord ctrl+k");

    let scrolled = engine
        .execute(
            &session,
            DesktopAction::Scroll {
                target: target(),
                direction: ScrollDirection::Down,
                amount: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        scrolled.method,
        ActionMethod::PhysicalScroll,
        "{scrolled:?}"
    );
    let wheel = fx
        .wait_for_title(|t| t.contains(" - wheel -"), Duration::from_secs(3))
        .unwrap_or_else(|| panic!("no wheel event, title is {:?}", fx.window_title()));
    assert!(wheel.ends_with("-120"), "one notch down: {wheel}");

    // Nothing stays pressed, and the cursor goes back where the user left it.
    input.release_all().unwrap();
    let ctx = winwright_contracts::backend::OperationContext::new(
        winwright_contracts::ids::SessionId::parse("live").unwrap(),
        Duration::from_secs(5),
        tokio_util::sync::CancellationToken::new(),
    );
    input.move_to(cursor, &ctx).await.unwrap();
}

/// Phase 17: mouse input by position (`desktop_mouse`), in screen pixels and in pixels of the
/// window, the way a model aims from a screenshot.
#[tokio::test]
#[ignore = "moves the real mouse over the canvas fixture for a few seconds"]
async fn phase17_mouse_acts_by_position() {
    winwright_win32::enable_per_monitor_dpi_awareness();
    let windows = winwright_win32::Win32Windows;
    let cursor = windows.cursor_position().unwrap();
    let fx = FixtureProcess::launch(Fixture::Canvas).expect("canvas launches");
    let input = Arc::new(winwright_input::SendInputBackend::new());
    let engine = Engine::new(
        Config::default(),
        Arc::new(winwright_win32::Win32Windows),
        Arc::new(winwright_uia::UiaBackend::start().expect("UIA worker")),
    )
    .with_input(input.clone());
    let session = engine
        .session(
            &winwright_contracts::ids::SessionId::parse("live").unwrap(),
            "test",
        )
        .unwrap();
    let event = |want: &str| {
        let title = fx.wait_for_title(|t| t.ends_with(want), Duration::from_secs(3));
        assert!(
            title.is_some(),
            "want {want:?}, title is {:?}",
            fx.window_title()
        );
    };

    // The squares' centres: green holds the window's centre; red and blue sit 160 DIPs either
    // side (the client area is 520 DIPs wide, the window a few pixels more).
    let bounds = windows
        .window(fx.hwnd)
        .unwrap()
        .expect("canvas window")
        .bounds;
    let center = bounds.center();
    let side = (f64::from(bounds.width()) * 160.0 / 520.0) as i32;
    let screen = |x: i32| ScreenPoint {
        x,
        y: center.y,
        window: None,
    };
    let in_window = |x: i32| ScreenPoint {
        x: x - bounds.left,
        y: center.y - bounds.top,
        window: Some(WindowSelector {
            hwnd: Some(fx.hwnd),
            ..Default::default()
        }),
    };

    let clicked = engine
        .execute(
            &session,
            DesktopAction::ClickAt {
                at: screen(center.x),
                button: MouseButton::Left,
                click_count: 1,
            },
        )
        .await
        .unwrap();
    assert_eq!(clicked.method, ActionMethod::PhysicalClick, "{clicked:?}");
    assert!(
        clicked
            .target
            .ends_with(&format!(" at {},{}", center.x, center.y)),
        "the result says where: {clicked:?}"
    );
    event(" - clicked green");

    engine
        .execute(
            &session,
            DesktopAction::ClickAt {
                at: in_window(center.x),
                button: MouseButton::Right,
                click_count: 1,
            },
        )
        .await
        .unwrap();
    event(" - right-clicked green");

    let moved = engine
        .execute(
            &session,
            DesktopAction::MoveMouse {
                at: in_window(center.x - side),
            },
        )
        .await
        .unwrap();
    assert_eq!(moved.method, ActionMethod::PhysicalMove, "{moved:?}");
    assert!(moved.verified, "the pointer is on red: {moved:?}");

    let dragged = engine
        .execute(
            &session,
            DesktopAction::Drag {
                from: screen(center.x - side),
                to: screen(center.x + side),
                button: MouseButton::Left,
            },
        )
        .await
        .unwrap();
    assert_eq!(dragged.method, ActionMethod::PhysicalDrag, "{dragged:?}");
    event(" - dragged red to blue");

    engine
        .execute(
            &session,
            DesktopAction::ScrollAt {
                at: screen(center.x + side),
                direction: ScrollDirection::Down,
                amount: 1,
            },
        )
        .await
        .unwrap();
    event(" - wheel -120");

    // A point outside the window it is relative to is refused before any input.
    let outside = engine
        .execute(
            &session,
            DesktopAction::ClickAt {
                at: ScreenPoint {
                    x: bounds.width() + 50,
                    y: 10,
                    window: in_window(0).window,
                },
                button: MouseButton::Left,
                click_count: 1,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(outside.code().as_str(), "INVALID_REQUEST", "{outside}");

    input.release_all().unwrap();
    let ctx = winwright_contracts::backend::OperationContext::new(
        winwright_contracts::ids::SessionId::parse("live").unwrap(),
        Duration::from_secs(5),
        tokio_util::sync::CancellationToken::new(),
    );
    input.move_to(cursor, &ctx).await.unwrap();
}

/// A window the accessibility tree barely describes (the canvas draws its text itself) gets the
/// text OCR finds numbered in its marks legend, with screen points to click.
#[tokio::test]
#[ignore = "shows the canvas fixture and captures it for a few seconds"]
async fn marks_number_the_ocr_text_of_a_canvas() {
    use winwright_contracts::capture::{ScreenshotRequest, ScreenshotTarget};

    winwright_win32::enable_per_monitor_dpi_awareness();
    let fx = FixtureProcess::launch(Fixture::Canvas).expect("canvas launches");
    let engine = Engine::new(
        Config::default(),
        Arc::new(winwright_win32::Win32Windows),
        Arc::new(winwright_uia::UiaBackend::start().expect("UIA worker")),
    )
    .with_capture(Arc::new(
        winwright_capture::WgcCapture::start().expect("capture worker"),
    ));
    let session = engine
        .session(
            &winwright_contracts::ids::SessionId::parse("live").unwrap(),
            "test",
        )
        .unwrap();
    let image = engine
        .screenshot(
            &session,
            ScreenshotRequest {
                target: ScreenshotTarget::Window(WindowSelector {
                    hwnd: Some(fx.hwnd),
                    ..Default::default()
                }),
                marks: true,
                ..Default::default()
            },
        )
        .await
        .expect("a marked screenshot");
    let legend = image.legend.expect("a legend");
    assert!(
        legend.contains("found by OCR") && legend.to_lowercase().contains("wright"),
        "the drawn text is numbered: {legend}"
    );
}
