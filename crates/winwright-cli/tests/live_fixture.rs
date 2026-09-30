//! Phase 2/3 acceptance against the controlled Win32 fixture (spec §50, §52).
//! Opt-in: `cargo test -p winwright-cli --test live_fixture -- --ignored --test-threads=1`
//! Every action here is semantic (UIA patterns): no pointer or keyboard input is injected.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winwright_contracts::action::{ActionMethod, DesktopAction, ElementTarget, WindowAction};
use winwright_contracts::config::Config;
use winwright_contracts::element::ControlRole;
use winwright_contracts::input::MouseButton;
use winwright_contracts::locator::{ElementLocator, FindRequest, FindResult};
use winwright_contracts::snapshot::{SnapshotRequest, SnapshotTarget};
use winwright_contracts::window::WindowSelector;
use winwright_core::Engine;
use winwright_core::session::Session;
use winwright_test_support::{Fixture, FixtureProcess};

struct Harness {
    fx: FixtureProcess,
    engine: Engine,
    session: Arc<Session>,
}

fn start() -> Harness {
    winwright_win32::enable_per_monitor_dpi_awareness();
    let fx = FixtureProcess::launch(Fixture::Win32).expect("fixture launches");
    let engine = Engine::new(
        Config::default(),
        Arc::new(winwright_win32::Win32Windows),
        Arc::new(winwright_uia::UiaBackend::start().expect("UIA worker")),
    )
    .with_input(Arc::new(winwright_input::SendInputBackend::new()));
    let session = engine
        .session(
            &winwright_contracts::ids::SessionId::parse("live").unwrap(),
            "test",
        )
        .unwrap();
    Harness {
        fx,
        engine,
        session,
    }
}

impl Harness {
    fn scope(&self) -> SnapshotTarget {
        SnapshotTarget::Window(WindowSelector {
            hwnd: Some(self.fx.hwnd),
            ..Default::default()
        })
    }

    fn target(&self, role: &str, name: &str) -> ElementTarget {
        ElementTarget::by_locator(
            ElementLocator {
                role: Some(role.into()),
                name: Some(name.into()),
                ..Default::default()
            },
            self.scope(),
        )
    }

    fn by_id(&self, id: &str) -> ElementTarget {
        ElementTarget::by_locator(
            ElementLocator {
                automation_id: Some(id.into()),
                ..Default::default()
            },
            self.scope(),
        )
    }

    async fn find(&self, json: serde_json::Value) -> FindResult {
        let mut req: FindRequest = serde_json::from_value(json).unwrap();
        req.scope = self.scope();
        self.engine.find(&self.session, req).await.unwrap()
    }

    async fn status(&self) -> String {
        let found = self.find(serde_json::json!({"automationId": "140"})).await;
        found.matches[0].name.clone()
    }

    async fn wait_status(&self, want: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let s = self.status().await;
            if s == want || Instant::now() > deadline {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn run(&self, action: DesktopAction) -> winwright_contracts::action::ActionResult {
        self.engine.execute(&self.session, action).await.unwrap()
    }
}

fn click(target: ElementTarget) -> DesktopAction {
    DesktopAction::Click {
        target,
        button: MouseButton::Left,
        click_count: 1,
        force_physical: false,
    }
}

#[tokio::test]
#[ignore = "launches the fixture app"]
async fn phase2_locators_resolve_fixture_controls() {
    let h = start();
    let submit = h
        .find(serde_json::json!({"role": "Button", "name": "Submit"}))
        .await;
    assert_eq!(submit.count, 1);
    assert_eq!(submit.matches[0].automation_id, "110");

    let name = h
        .find(serde_json::json!({"role": "Edit", "label": "Name"}))
        .await;
    assert_eq!(name.count, 1);
    assert_eq!(name.matches[0].automation_id, "101");

    let combo = h.find(serde_json::json!({"automationId": "116"})).await;
    assert_eq!(combo.matches[0].role, ControlRole::ComboBox);

    let far_item = h
        .find(serde_json::json!({"role": "ListItem", "name": "Item 150", "visibleOnly": false}))
        .await;
    assert_eq!(
        far_item.count, 1,
        "offscreen list items are searchable when asked"
    );

    let disabled = h.find(serde_json::json!({"name": "Disabled Action"})).await;
    assert!(!disabled.matches[0].enabled);

    let dialogish = h
        .find(serde_json::json!({"role": "Button", "name": "dialog", "exact": false}))
        .await;
    assert_eq!(dialogish.matches[0].name, "Open Dialog");

    let snapshot = h
        .engine
        .snapshot(
            &h.session,
            SnapshotRequest {
                target: h.scope(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        snapshot
            .tree
            .contains("EDIT \"Password:\" value=\"[REDACTED]\" sensitive=true"),
        "{}",
        snapshot.tree
    );
}

#[tokio::test]
#[ignore = "launches the fixture app"]
async fn phase3_invoke_fill_toggle_select() {
    let h = start();

    let r = h
        .run(DesktopAction::Fill {
            target: ElementTarget::by_locator(
                ElementLocator {
                    role: Some("Edit".into()),
                    label: Some("Name".into()),
                    ..Default::default()
                },
                h.scope(),
            ),
            text: "Ada".into(),
            clear: true,
        })
        .await;
    assert_eq!(r.method, ActionMethod::ValuePattern);
    assert!(r.verified, "{r:?}");

    let r = h.run(click(h.target("Button", "Target"))).await;
    assert_eq!(r.method, ActionMethod::InvokePattern);
    assert_eq!(h.wait_status("Target clicked 1").await, "Target clicked 1");

    let r = h
        .run(DesktopAction::Check {
            target: h.target("CheckBox", "Enable feature"),
        })
        .await;
    assert!(r.verified, "{r:?}");
    assert_eq!(h.wait_status("Feature: on").await, "Feature: on");
    let again = h
        .run(DesktopAction::Check {
            target: h.target("CheckBox", "Enable feature"),
        })
        .await;
    assert_eq!(again.method, ActionMethod::NoOp);

    let r = h
        .run(DesktopAction::Check {
            target: h.target("RadioButton", "Large"),
        })
        .await;
    assert!(r.verified, "{r:?}");

    let r = h
        .run(DesktopAction::Select {
            target: h.by_id("116"),
            option: Some("Green".into()),
        })
        .await;
    assert!(r.verified, "combo value should read Green: {r:?}");

    let r = h
        .run(DesktopAction::Select {
            target: h.by_id("118"),
            option: Some("Item 150".into()),
        })
        .await;
    assert!(r.verified, "{r:?}");

    let r = h
        .run(DesktopAction::Select {
            target: h.by_id("130"),
            option: Some("Advanced".into()),
        })
        .await;
    assert!(r.verified, "{r:?}");

    let r = h
        .run(DesktopAction::Expand {
            target: h.target("TreeItem", "Root"),
        })
        .await;
    assert!(r.verified, "{r:?}");
    let child = h
        .find(serde_json::json!({"role": "TreeItem", "name": "Child A"}))
        .await;
    assert_eq!(child.count, 1, "expanding Root reveals Child A");
}

#[tokio::test]
#[ignore = "launches the fixture app"]
async fn phase3_policy_and_sensitive_fields() {
    let h = start();
    let err = h
        .engine
        .execute(&h.session, click(h.target("Button", "Submit")))
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CONFIRMATION_REQUIRED");
    assert_eq!(h.status().await, "Ready", "Submit did not run");

    let err = h
        .engine
        .execute(
            &h.session,
            DesktopAction::ReadText {
                target: h.by_id("103"),
                max_chars: 100,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "SENSITIVE_FIELD");

    let err = h
        .engine
        .execute(&h.session, click(h.target("Button", "Disabled Action")))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("disabled"), "{err}");
}

#[tokio::test]
#[ignore = "launches the fixture app"]
async fn phase3_dialog_round_trip_and_read_text() {
    let h = start();
    let r = h.run(click(h.target("Button", "Open Dialog"))).await;
    assert!(r.verified, "{r:?}");
    assert!(
        r.opened_windows
            .iter()
            .any(|w| w.contains("Fixture Dialog")),
        "{r:?}"
    );

    let dialog = SnapshotTarget::Window(WindowSelector {
        title: Some("Fixture Dialog".into()),
        ..Default::default()
    });
    let r = h
        .run(DesktopAction::Fill {
            target: ElementTarget::by_locator(
                ElementLocator {
                    role: Some("Edit".into()),
                    label: Some("Value".into()),
                    ..Default::default()
                },
                dialog.clone(),
            ),
            text: "42".into(),
            clear: true,
        })
        .await;
    assert!(r.verified, "{r:?}");
    let r = h
        .run(click(ElementTarget::by_locator(
            ElementLocator {
                role: Some("Button".into()),
                name: Some("OK".into()),
                ..Default::default()
            },
            dialog,
        )))
        .await;
    assert!(
        r.closed_windows
            .iter()
            .any(|w| w.contains("Fixture Dialog")),
        "{r:?}"
    );
    assert_eq!(h.wait_status("Dialog: 42").await, "Dialog: 42");

    h.run(DesktopAction::Fill {
        target: h.by_id("105"),
        text: "line one\r\nline two".into(),
        clear: true,
    })
    .await;
    let r = h
        .run(DesktopAction::ReadText {
            target: h.by_id("105"),
            max_chars: 1000,
        })
        .await;
    assert!(
        r.text.as_deref().unwrap_or_default().contains("line two"),
        "{r:?}"
    );
}

#[tokio::test]
#[ignore = "launches the fixture app"]
async fn phase3_stale_ref_re_resolves_after_recreation() {
    let h = start();
    let target = h
        .find(serde_json::json!({"role": "Button", "name": "Target"}))
        .await;
    let reference = target.matches[0].reference.clone();
    h.run(click(h.target("Button", "Recreate"))).await;
    assert!(h.wait_status("Recreated 1").await.starts_with("Recreated"));
    let r = h.run(click(ElementTarget::by_ref(&reference))).await;
    assert_eq!(r.reference.as_deref(), Some(reference.as_str()));
    assert_eq!(h.wait_status("Target clicked 1").await, "Target clicked 1");
}

#[tokio::test]
#[ignore = "launches the fixture app"]
async fn window_control_round_trip() {
    let h = start();
    let window = WindowSelector {
        hwnd: Some(h.fx.hwnd),
        ..Default::default()
    };
    let r = h
        .engine
        .window_action(
            &h.session,
            WindowAction::Move {
                window: window.clone(),
                x: 60,
                y: 70,
            },
        )
        .await
        .unwrap();
    assert!(r.verified, "{r:?}");
    let r = h
        .engine
        .window_action(
            &h.session,
            WindowAction::Resize {
                window: window.clone(),
                width: 900,
                height: 720,
            },
        )
        .await
        .unwrap();
    assert!(r.verified, "{r:?}");
    for action in [
        WindowAction::Minimize {
            window: window.clone(),
        },
        WindowAction::Restore {
            window: window.clone(),
        },
        WindowAction::Maximize {
            window: window.clone(),
        },
        WindowAction::Restore {
            window: window.clone(),
        },
    ] {
        let r = h.engine.window_action(&h.session, action).await.unwrap();
        assert!(r.verified, "{r:?}");
    }
    let r = h
        .engine
        .window_action(&h.session, WindowAction::Close { window })
        .await
        .unwrap();
    assert!(r.verified, "{r:?}");
}

#[tokio::test]
#[ignore = "launches the fixture app"]
async fn phase4_waits_replace_sleeps() {
    use winwright_contracts::wait::{WaitRequest, WaitState};
    let h = start();
    let wait = |state,
                locator: Option<ElementLocator>,
                window: Option<WindowSelector>,
                value: Option<&str>| WaitRequest {
        state,
        reference: None,
        locator,
        scope: h.scope(),
        window,
        value: value.map(Into::into),
        value_match: Default::default(),
        timeout_ms: Some(5_000),
    };
    let button = |name: &str| ElementLocator {
        role: Some("Button".into()),
        name: Some(name.into()),
        ..Default::default()
    };

    h.run(click(h.target("Button", "Add Delayed"))).await;
    let appeared = h
        .engine
        .wait_for(
            &h.session,
            wait(
                WaitState::Visible,
                Some(button("Delayed Button")),
                None,
                None,
            ),
        )
        .await
        .unwrap();
    assert!(
        appeared.elapsed_ms >= 1_000,
        "the button is created after 1.5 s: {appeared:?}"
    );
    let delayed_ref = appeared.element.unwrap().reference;
    h.run(click(ElementTarget::by_ref(&delayed_ref))).await;
    let status = h
        .engine
        .wait_for(
            &h.session,
            wait(
                WaitState::Text,
                Some(ElementLocator {
                    automation_id: Some("140".into()),
                    ..Default::default()
                }),
                None,
                Some("Delayed clicked"),
            ),
        )
        .await
        .unwrap();
    assert!(status.checks >= 1);

    let dialog = WindowSelector {
        title: Some("Fixture Dialog".into()),
        ..Default::default()
    };
    h.run(click(h.target("Button", "Open Dialog"))).await;
    h.engine
        .wait_for(
            &h.session,
            wait(WaitState::WindowOpen, None, Some(dialog.clone()), None),
        )
        .await
        .unwrap();
    h.run(click(ElementTarget::by_locator(
        button("Cancel"),
        SnapshotTarget::Window(dialog.clone()),
    )))
    .await;
    h.engine
        .wait_for(
            &h.session,
            wait(WaitState::WindowClosed, None, Some(dialog), None),
        )
        .await
        .unwrap();

    // A diff snapshot after one change reports only that change.
    let snap = |diff| SnapshotRequest {
        target: h.scope(),
        diff,
        ..Default::default()
    };
    h.engine.snapshot(&h.session, snap(false)).await.unwrap();
    h.run(DesktopAction::Check {
        target: h.target("CheckBox", "Enable feature"),
    })
    .await;
    let d = h.engine.snapshot(&h.session, snap(true)).await.unwrap();
    let diff = d.diff.expect("diff against the previous snapshot");
    let changed = diff
        .lines()
        .find(|l| l.starts_with("~ CHECKBOX \"Enable feature\" unchecked"))
        .unwrap_or_else(|| panic!("checkbox change missing: {diff}"));
    assert!(changed.contains(" checked ["), "{diff}");
    assert!(d.tree.is_empty());
}
