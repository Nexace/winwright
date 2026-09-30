//! Engine tests against fake backends: no desktop required.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use winwright_contracts::backend::*;
use winwright_contracts::config::Config;
use winwright_contracts::element::{ControlRole, UiPattern};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::ids::SessionId;
use winwright_contracts::snapshot::{SnapshotRequest, SnapshotTarget};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::{Engine, InspectRequest};

fn window(hwnd: u64, title: &str, process: &str, foreground: bool) -> WindowInfo {
    WindowInfo {
        hwnd,
        title: title.into(),
        class_name: "C".into(),
        process_id: hwnd as u32,
        process_name: process.into(),
        bounds: PhysicalRect::new(0, 0, 100, 100),
        minimized: false,
        maximized: false,
        foreground,
        topmost: false,
        owner_hwnd: None,
    }
}

struct FakeWindows(Vec<WindowInfo>);

impl WindowBackend for FakeWindows {
    fn list_windows(&self) -> WinwrightResult<Vec<WindowInfo>> {
        Ok(self.0.clone())
    }
    fn foreground_window(&self) -> WinwrightResult<Option<WindowInfo>> {
        Ok(self.0.iter().find(|w| w.foreground).cloned())
    }
    fn window(&self, hwnd: u64) -> WinwrightResult<Option<WindowInfo>> {
        Ok(self.0.iter().find(|w| w.hwnd == hwnd).cloned())
    }
    fn cursor_position(&self) -> WinwrightResult<PhysicalPoint> {
        Ok(PhysicalPoint { x: 5, y: 5 })
    }
    fn process_name(&self, _pid: u32) -> String {
        "fake.exe".into()
    }
}

#[derive(Default)]
struct FakeUia {
    next_slot: Mutex<u64>,
    released: Mutex<Vec<ElementKey>>,
    captured_roots: Mutex<Vec<TreeRoot>>,
    hang: bool,
}

impl FakeUia {
    fn key(&self) -> ElementKey {
        let mut n = self.next_slot.lock().unwrap();
        *n += 1;
        ElementKey {
            worker_epoch: 1,
            slot: *n,
        }
    }

    fn node(&self, role: ControlRole, name: &str, rid: i32, children: Vec<UiNode>) -> UiNode {
        UiNode {
            key: self.key(),
            props: UiProps {
                role,
                name: name.into(),
                runtime_id: vec![rid],
                process_id: 1,
                bounds: Some(PhysicalRect::new(0, 0, 10, 10)),
                enabled: true,
                patterns: if role == ControlRole::Button {
                    vec![UiPattern::Invoke]
                } else {
                    vec![]
                },
                ..Default::default()
            },
            children_total: children.len() as u32,
            children,
        }
    }
}

impl UiAutomationBackend for FakeUia {
    fn worker_epoch(&self) -> u64 {
        1
    }

    fn capture_tree<'a>(
        &'a self,
        request: UiTreeRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiTree> {
        Box::pin(async move {
            if self.hang {
                ctx.cancel.cancelled().await;
                return Err(WinwrightError::Cancelled);
            }
            self.captured_roots.lock().unwrap().push(request.root);
            let pane = self.node(
                ControlRole::Pane,
                "",
                3,
                vec![self.node(ControlRole::Button, "Save", 4, vec![])],
            );
            let root = self.node(ControlRole::Window, "Untitled - Notepad", 1, vec![pane]);
            Ok(UiTree {
                root,
                node_count: 3,
                truncated: false,
            })
        })
    }

    fn inspect<'a>(
        &'a self,
        _target: InspectTarget,
        _ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiInspection> {
        Box::pin(async move {
            let node = self.node(ControlRole::Button, "Save", 4, vec![]);
            let window = self.node(ControlRole::Window, "Untitled - Notepad", 1, vec![]);
            Ok(UiInspection {
                key: node.key,
                props: node.props,
                ancestors: vec![window.props],
            })
        })
    }

    fn release<'a>(&'a self, keys: Vec<ElementKey>) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            self.released.lock().unwrap().extend(keys);
            Ok(())
        })
    }
}

fn engine_with(windows: Vec<WindowInfo>, uia: Arc<FakeUia>) -> Engine {
    Engine::new(Config::default(), Arc::new(FakeWindows(windows)), uia)
}

fn sid() -> SessionId {
    SessionId::parse("test").unwrap()
}

#[tokio::test]
async fn snapshot_active_window_and_reuse_refs() {
    let uia = Arc::new(FakeUia::default());
    let engine = engine_with(
        vec![
            window(10, "Untitled - Notepad", "notepad.exe", true),
            window(20, "Calculator", "CalculatorApp.exe", false),
        ],
        uia.clone(),
    );
    let session = engine.session(&sid(), "test").unwrap();
    let snap = engine
        .snapshot(&session, SnapshotRequest::default())
        .await
        .unwrap();
    assert_eq!(snap.generation, "s_1");
    assert_eq!(
        snap.tree,
        "WINDOW \"Untitled - Notepad\" [e1]\n  BUTTON \"Save\" [e2]\n"
    );
    let active = snap.active_window.unwrap();
    assert_eq!(
        (active.reference.as_str(), active.process.as_str()),
        ("e1", "notepad.exe")
    );
    assert_eq!(uia.captured_roots.lock().unwrap()[0], TreeRoot::Window(10));
    assert_eq!(
        uia.released.lock().unwrap().len(),
        1,
        "the unnamed pane is released"
    );

    let again = engine
        .snapshot(&session, SnapshotRequest::default())
        .await
        .unwrap();
    assert_eq!(again.generation, "s_2");
    assert_eq!(again.tree, snap.tree, "same elements keep their refs");
    // Second snapshot: pane released, plus the two superseded slots of e1/e2.
    assert_eq!(uia.released.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn window_selection_is_never_guessed() {
    let uia = Arc::new(FakeUia::default());
    let engine = engine_with(
        vec![
            window(10, "notes - Notepad", "notepad.exe", true),
            window(11, "todo - Notepad", "notepad.exe", false),
        ],
        uia,
    );
    let session = engine.session(&sid(), "test").unwrap();
    let by_process = SnapshotRequest {
        target: SnapshotTarget::Window(WindowSelector {
            process: Some("notepad".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = engine.snapshot(&session, by_process).await.unwrap_err();
    assert_eq!(err.code().as_str(), "ELEMENT_AMBIGUOUS");
    assert_eq!(err.payload().matches.len(), 2);

    let by_title = SnapshotRequest {
        target: SnapshotTarget::Window(WindowSelector {
            title: Some("todo".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    engine.snapshot(&session, by_title).await.unwrap();

    let missing = SnapshotRequest {
        target: SnapshotTarget::Window(WindowSelector {
            title: Some("Photoshop".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = engine.snapshot(&session, missing).await.unwrap_err();
    assert_eq!(err.code().as_str(), "WINDOW_NOT_FOUND");
}

#[tokio::test]
async fn subtree_uses_session_refs_and_rejects_foreign_refs() {
    let uia = Arc::new(FakeUia::default());
    let engine = engine_with(vec![window(10, "N", "n.exe", true)], uia.clone());
    let a = engine.session(&sid(), "test").unwrap();
    let b = engine
        .session(&SessionId::parse("other").unwrap(), "test")
        .unwrap();
    engine
        .snapshot(&a, SnapshotRequest::default())
        .await
        .unwrap();

    let subtree = SnapshotRequest {
        target: SnapshotTarget::Subtree {
            reference: "e2".into(),
        },
        ..Default::default()
    };
    engine.snapshot(&a, subtree.clone()).await.unwrap();
    assert!(matches!(
        uia.captured_roots.lock().unwrap()[1],
        TreeRoot::Element(_)
    ));
    let err = engine.snapshot(&b, subtree).await.unwrap_err();
    assert_eq!(
        err.code().as_str(),
        "INVALID_REQUEST",
        "refs are per session"
    );
}

#[tokio::test]
async fn cancel_interrupts_a_hung_capture() {
    let uia = Arc::new(FakeUia {
        hang: true,
        ..Default::default()
    });
    let engine = Arc::new(engine_with(vec![window(10, "N", "n.exe", true)], uia));
    let session = engine.session(&sid(), "test").unwrap();
    let task = {
        let engine = Arc::clone(&engine);
        let session = Arc::clone(&session);
        tokio::spawn(async move { engine.snapshot(&session, SnapshotRequest::default()).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    session.cancel();
    let err = task.await.unwrap().unwrap_err();
    assert_eq!(err.code().as_str(), "CANCELLED");
    let err = engine
        .snapshot(&session, SnapshotRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CANCELLED", "cancel is terminal");
}

#[tokio::test]
async fn inspect_assigns_a_ref_and_path() {
    let uia = Arc::new(FakeUia::default());
    let engine = engine_with(vec![window(10, "N", "n.exe", true)], uia);
    let session = engine.session(&sid(), "test").unwrap();
    let details = engine
        .inspect(&session, InspectRequest::UnderCursor)
        .await
        .unwrap();
    assert_eq!(details.element.reference, "e1");
    assert_eq!(
        details.element.path,
        "Window \"Untitled - Notepad\" > Button \"Save\""
    );
    assert_eq!(details.element.patterns, vec![UiPattern::Invoke]);
    assert_eq!(details.process_name, "fake.exe");
    let again = engine
        .inspect(&session, InspectRequest::Ref("e1".into()))
        .await
        .unwrap();
    assert_eq!(again.element.reference, "e1");
}

#[tokio::test]
async fn invalid_requests_fail_before_touching_backends() {
    let uia = Arc::new(FakeUia::default());
    let engine = engine_with(vec![], uia.clone());
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .snapshot(
            &session,
            SnapshotRequest {
                max_depth: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "INVALID_REQUEST");
    let err = engine
        .snapshot(&session, SnapshotRequest::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.code().as_str(),
        "WINDOW_NOT_FOUND",
        "no foreground window"
    );
    assert!(uia.captured_roots.lock().unwrap().is_empty());
}
