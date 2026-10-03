//! Engine tests against a stateful fake desktop: every backend call is simulated, so these
//! exercise resolution, policy, pattern choice, verification and ref bookkeeping without UI.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use winwright_contracts::action::{ActionMethod, DesktopAction, ElementTarget, WindowAction};
use winwright_contracts::backend::*;
use winwright_contracts::config::Config;
use winwright_contracts::element::{ControlRole, ExpandState, ToggleState, UiPattern};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::ids::SessionId;
use winwright_contracts::input::{InputBackend, Key, MouseButton};
use winwright_contracts::locator::{ElementLocator, FindRequest};
use winwright_contracts::snapshot::{SnapshotRequest, SnapshotTarget};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::{Engine, InspectRequest};

// ---------------------------------------------------------------- fake desktop

#[derive(Clone)]
struct El {
    role: ControlRole,
    name: String,
    automation_id: String,
    patterns: Vec<UiPattern>,
    value: Option<String>,
    read_only: bool,
    toggle: Option<ToggleState>,
    expand: Option<ExpandState>,
    selected: Option<bool>,
    enabled: bool,
    offscreen: bool,
    focused: bool,
    password: bool,
    pid: u32,
    children: Vec<i32>,
}

fn el(role: ControlRole, name: &str, id: &str, patterns: &[UiPattern]) -> El {
    El {
        role,
        name: name.into(),
        automation_id: id.into(),
        patterns: patterns.to_vec(),
        value: patterns.contains(&UiPattern::Value).then(String::new),
        read_only: false,
        toggle: patterns
            .contains(&UiPattern::Toggle)
            .then_some(ToggleState::Off),
        expand: patterns
            .contains(&UiPattern::ExpandCollapse)
            .then_some(ExpandState::Collapsed),
        selected: patterns
            .contains(&UiPattern::SelectionItem)
            .then_some(false),
        enabled: true,
        offscreen: false,
        focused: false,
        password: false,
        pid: 1,
        children: Vec::new(),
    }
}

const MAIN: u64 = 10;
const DIALOG: u64 = 20;
const TARGET: i32 = 12;
const STATUS: i32 = 16;
const COMBO: i32 = 7;
/// Another Winwright process (a CLI call next to the MCP server).
const OTHER_WINWRIGHT: u32 = 77;

struct State {
    els: BTreeMap<i32, El>,
    windows: Vec<WindowInfo>,
    roots: HashMap<u64, i32>,
    slots: HashMap<u64, i32>,
    next_slot: u64,
    next_rid: i32,
    released: Vec<ElementKey>,
    executed: Vec<String>,
    captured_roots: Vec<TreeRoot>,
    privileged: bool,
    /// Pattern calls cannot re-read the element (`props_after` is `None`).
    blind: bool,
    /// Toggle calls are accepted but the state does not change (yet).
    inert_toggle: bool,
    /// `props_after` shows the state from before the call (the provider updates a moment later).
    stale_readback: bool,
}

struct Fake {
    state: Mutex<State>,
    hang: bool,
}

fn window(hwnd: u64, title: &str, process: &str, foreground: bool) -> WindowInfo {
    WindowInfo {
        hwnd,
        title: title.into(),
        class_name: "C".into(),
        process_id: 1,
        process_name: process.into(),
        bounds: PhysicalRect::new(0, 0, 800, 600),
        minimized: false,
        maximized: false,
        foreground,
        topmost: false,
        owner_hwnd: None,
    }
}

impl Fake {
    fn new() -> Arc<Self> {
        use ControlRole as R;
        use UiPattern as P;
        let mut els = BTreeMap::new();
        els.insert(2, el(R::Text, "Name:", "100", &[]));
        els.insert(3, el(R::Edit, "Name:", "101", &[P::Value]));
        let mut pw = el(R::Edit, "Password:", "103", &[P::Value]);
        pw.password = true;
        els.insert(4, pw);
        els.insert(5, el(R::Button, "Submit", "110", &[P::Invoke]));
        els.insert(6, el(R::CheckBox, "Enable feature", "112", &[P::Toggle]));
        let mut combo = el(R::ComboBox, "Color:", "116", &[P::ExpandCollapse, P::Value]);
        combo.read_only = true;
        combo.children = vec![8, 9, 10];
        els.insert(COMBO, combo);
        for (rid, name) in [(8, "Red"), (9, "Green"), (10, "Blue")] {
            let mut item = el(R::ListItem, name, "", &[P::SelectionItem]);
            item.offscreen = true;
            els.insert(rid, item);
        }
        els.insert(11, el(R::Button, "Open Dialog", "120", &[P::Invoke]));
        els.insert(TARGET, el(R::Button, "Target", "124", &[P::Invoke]));
        els.insert(13, el(R::Button, "Recreate", "123", &[P::Invoke]));
        els.insert(14, el(R::Button, "Save", "s1", &[P::Invoke]));
        els.insert(15, el(R::Button, "Save", "s2", &[P::Invoke]));
        els.insert(STATUS, el(R::Text, "Ready", "140", &[]));
        let mut notes = el(R::Document, "Notes", "105", &[P::Text, P::Value]);
        notes.value = Some("hello notes".into());
        els.insert(17, notes);
        els.insert(18, el(R::RadioButton, "Small", "113", &[P::SelectionItem]));
        let mut disabled = el(R::Button, "Disabled Action", "111", &[P::Invoke]);
        disabled.enabled = false;
        els.insert(19, disabled);
        let mut root = el(R::Window, "Fixture", "", &[]);
        root.children = (2..=19).filter(|r| !(8..=10).contains(r)).collect();
        els.insert(1, root);
        // Dialog window, opened by "Open Dialog".
        let mut dialog = el(R::Dialog, "Fixture Dialog", "", &[]);
        dialog.children = vec![31];
        els.insert(30, dialog);
        els.insert(31, el(R::Button, "OK", "202", &[P::Invoke]));

        Arc::new(Self {
            state: Mutex::new(State {
                els,
                windows: vec![window(MAIN, "Fixture", "fixture.exe", true)],
                roots: HashMap::from([(MAIN, 1), (DIALOG, 30)]),
                slots: HashMap::new(),
                next_slot: 0,
                next_rid: 100,
                released: Vec::new(),
                executed: Vec::new(),
                captured_roots: Vec::new(),
                privileged: false,
                blind: false,
                inert_toggle: false,
                stale_readback: false,
            }),
            hang: false,
        })
    }

    fn s(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

impl State {
    fn props(&self, rid: i32) -> UiProps {
        let e = &self.els[&rid];
        UiProps {
            control_type_id: e.role as i32,
            role: e.role,
            name: e.name.clone(),
            automation_id: e.automation_id.clone(),
            class_name: "Fake".into(),
            framework_id: "Win32".into(),
            process_id: e.pid,
            runtime_id: vec![42, rid],
            bounds: Some(PhysicalRect::new(rid * 20, 0, rid * 20 + 16, 16)),
            enabled: e.enabled,
            offscreen: e.offscreen,
            focused: e.focused,
            keyboard_focusable: true,
            is_password: e.password,
            patterns: e.patterns.clone(),
            value: if e.password { None } else { e.value.clone() },
            value_read_only: e
                .patterns
                .contains(&UiPattern::Value)
                .then_some(e.read_only),
            toggle_state: e.toggle,
            expand_state: e.expand,
            selected: e.selected,
            ..Default::default()
        }
    }

    fn slot(&mut self, rid: i32) -> ElementKey {
        self.next_slot += 1;
        self.slots.insert(self.next_slot, rid);
        ElementKey {
            worker_epoch: 1,
            slot: self.next_slot,
        }
    }

    fn rid(&self, key: ElementKey) -> WinwrightResult<i32> {
        let rid = *self
            .slots
            .get(&key.slot)
            .ok_or_else(|| WinwrightError::ElementStale {
                reference: format!("slot {}", key.slot),
                reason: "released".into(),
            })?;
        if !self.els.contains_key(&rid)
            || self.parent(rid).is_none() && !self.roots.values().any(|r| *r == rid)
        {
            return Err(WinwrightError::ElementStale {
                reference: format!("slot {}", key.slot),
                reason: "the element no longer exists".into(),
            });
        }
        Ok(rid)
    }

    fn parent(&self, rid: i32) -> Option<i32> {
        self.els
            .iter()
            .find(|(_, e)| e.children.contains(&rid))
            .map(|(p, _)| *p)
    }

    fn build(&mut self, rid: i32, depth: u32, req: &UiTreeRequest) -> UiNode {
        let key = self.slot(rid);
        let props = self.props(rid);
        let kids = self.els[&rid].children.clone();
        let mut children = Vec::new();
        if depth + 1 < req.max_depth {
            for c in &kids {
                if self.els[c].offscreen && !req.include_offscreen {
                    continue;
                }
                children.push(self.build(*c, depth + 1, req));
            }
        }
        UiNode {
            key,
            props,
            children_total: children.len() as u32,
            children,
        }
    }

    fn ancestors(&self, rid: i32) -> Vec<UiProps> {
        let mut chain = Vec::new();
        let mut cur = self.parent(rid);
        while let Some(p) = cur {
            chain.push(self.props(p));
            cur = self.parent(p);
        }
        chain.reverse();
        chain
    }
}

impl WindowBackend for Fake {
    fn list_windows(&self) -> WinwrightResult<Vec<WindowInfo>> {
        Ok(self.s().windows.clone())
    }
    fn foreground_window(&self) -> WinwrightResult<Option<WindowInfo>> {
        Ok(self.s().windows.iter().find(|w| w.foreground).cloned())
    }
    fn window(&self, hwnd: u64) -> WinwrightResult<Option<WindowInfo>> {
        Ok(self.s().windows.iter().find(|w| w.hwnd == hwnd).cloned())
    }
    fn cursor_position(&self) -> WinwrightResult<PhysicalPoint> {
        Ok(PhysicalPoint {
            x: 12 * 20 + 5,
            y: 5,
        })
    }
    fn process_name(&self, pid: u32) -> String {
        if pid == OTHER_WINWRIGHT {
            "winwright.exe".into()
        } else {
            "fixture.exe".into()
        }
    }
    fn focus_window(&self, hwnd: u64) -> WinwrightResult<()> {
        for w in &mut self.s().windows {
            w.foreground = w.hwnd == hwnd;
        }
        Ok(())
    }
    fn set_window_state(
        &self,
        hwnd: u64,
        state: winwright_contracts::action::WindowVisualState,
    ) -> WinwrightResult<()> {
        use winwright_contracts::action::WindowVisualState as V;
        for w in self.s().windows.iter_mut().filter(|w| w.hwnd == hwnd) {
            w.minimized = state == V::Minimized;
            w.maximized = state == V::Maximized;
        }
        Ok(())
    }
    fn set_window_bounds(&self, hwnd: u64, bounds: PhysicalRect) -> WinwrightResult<()> {
        for w in self.s().windows.iter_mut().filter(|w| w.hwnd == hwnd) {
            w.bounds = bounds;
        }
        Ok(())
    }
    fn close_window(&self, hwnd: u64) -> WinwrightResult<()> {
        let mut s = self.s();
        s.windows.retain(|w| w.hwnd != hwnd);
        Ok(())
    }
    fn is_more_privileged(&self, _pid: u32) -> bool {
        self.s().privileged
    }
}

impl UiAutomationBackend for Fake {
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
            let mut s = self.s();
            s.captured_roots.push(request.root);
            let rid = match request.root {
                TreeRoot::Window(h) => {
                    if !s.windows.iter().any(|w| w.hwnd == h) {
                        return Err(WinwrightError::WindowNotFound {
                            query: format!("{h}"),
                        });
                    }
                    s.roots[&h]
                }
                TreeRoot::Element(k) => s.rid(k)?,
                TreeRoot::Desktop => unreachable!("not used by these tests"),
            };
            let root = s.build(rid, 0, &request);
            Ok(UiTree {
                root,
                node_count: 1,
                truncated: false,
            })
        })
    }

    fn inspect<'a>(
        &'a self,
        target: InspectTarget,
        _ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiInspection> {
        Box::pin(async move {
            let mut s = self.s();
            let rid = match target {
                InspectTarget::Element(k) => s.rid(k)?,
                InspectTarget::Focused => *s
                    .els
                    .iter()
                    .find(|(_, e)| e.focused)
                    .map(|(r, _)| r)
                    .unwrap_or(&1),
                InspectTarget::Point(p) => {
                    let hit = s
                        .els
                        .keys()
                        .copied()
                        .filter(|r| *r != 1 && *r != 30)
                        .find(|r| s.props(*r).bounds.is_some_and(|b| b.contains(p)));
                    hit.unwrap_or(1)
                }
            };
            let key = s.slot(rid);
            Ok(UiInspection {
                key,
                props: s.props(rid),
                ancestors: s.ancestors(rid),
            })
        })
    }

    fn release<'a>(&'a self, keys: Vec<ElementKey>) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let mut s = self.s();
            for k in &keys {
                s.slots.remove(&k.slot);
            }
            s.released.extend(keys);
            Ok(())
        })
    }

    fn refresh<'a>(
        &'a self,
        key: ElementKey,
        _ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiProps> {
        Box::pin(async move {
            let s = self.s();
            let rid = s.rid(key)?;
            Ok(s.props(rid))
        })
    }

    fn execute_pattern<'a>(
        &'a self,
        key: ElementKey,
        action: UiPatternAction,
        _ctx: &'a OperationContext,
    ) -> BackendFuture<'a, UiActionOutcome> {
        Box::pin(async move {
            let mut s = self.s();
            let rid = s.rid(key)?;
            let before = s.props(rid);
            let label = before.label();
            s.executed.push(format!("{action:?} {label} #{rid}"));
            let unsupported = |p: &str| WinwrightError::UnsupportedPattern {
                element: label.clone(),
                pattern: p.into(),
            };
            let mut out = UiActionOutcome::default();
            match &action {
                UiPatternAction::Invoke => match s.els[&rid].name.as_str() {
                    "Open Dialog" | "More colors..." => {
                        for w in &mut s.windows {
                            w.foreground = false;
                        }
                        s.windows
                            .push(window(DIALOG, "Fixture Dialog", "fixture.exe", true));
                    }
                    "Recreate" => {
                        let old = s.els[&TARGET].clone();
                        let new_rid = s.next_rid;
                        s.next_rid += 1;
                        s.els.remove(&TARGET);
                        s.els.insert(new_rid, old);
                        let root = s.els.get_mut(&1).unwrap();
                        let pos = root.children.iter().position(|c| *c == TARGET).unwrap();
                        root.children[pos] = new_rid;
                    }
                    "Target" => s.els.get_mut(&STATUS).unwrap().name = "Target clicked".into(),
                    "Submit" => s.els.get_mut(&STATUS).unwrap().name = "Submitted".into(),
                    _ => {}
                },
                UiPatternAction::Toggle if s.inert_toggle => {}
                UiPatternAction::Toggle => {
                    let e = s.els.get_mut(&rid).unwrap();
                    e.toggle = Some(match e.toggle {
                        Some(ToggleState::On) => ToggleState::Off,
                        _ => ToggleState::On,
                    });
                }
                UiPatternAction::SetValue(v) => {
                    let e = s.els.get_mut(&rid).unwrap();
                    if !e.patterns.contains(&UiPattern::Value) || e.read_only {
                        return Err(unsupported("Value.SetValue"));
                    }
                    // An editable combo only accepts its own items.
                    let items = e.children.clone();
                    if rid == COMBO && !items.iter().any(|c| s.els[c].name == *v) {
                        return Err(unsupported("Value.SetValue (not an item)"));
                    }
                    s.els.get_mut(&rid).unwrap().value = Some(v.clone());
                }
                UiPatternAction::Expand | UiPatternAction::Collapse => {
                    let expanded = matches!(action, UiPatternAction::Expand);
                    let kids = {
                        let e = s.els.get_mut(&rid).unwrap();
                        e.expand = Some(if expanded {
                            ExpandState::Expanded
                        } else {
                            ExpandState::Collapsed
                        });
                        e.children.clone()
                    };
                    for c in kids {
                        s.els.get_mut(&c).unwrap().offscreen = !expanded;
                    }
                }
                UiPatternAction::Select => {
                    let parent = s.parent(rid).unwrap();
                    let siblings = s.els[&parent].children.clone();
                    for c in siblings {
                        if let Some(e) = s.els.get_mut(&c)
                            && e.selected.is_some()
                        {
                            e.selected = Some(c == rid);
                        }
                    }
                    let name = s.els[&rid].name.clone();
                    if parent == COMBO {
                        let combo = s.els.get_mut(&COMBO).unwrap();
                        combo.value = Some(name);
                        combo.expand = Some(ExpandState::Collapsed);
                    }
                }
                UiPatternAction::SetFocus => {
                    for (r, e) in s.els.iter_mut() {
                        e.focused = *r == rid;
                    }
                }
                UiPatternAction::GetText { .. } => {
                    let e = &s.els[&rid];
                    if e.password {
                        return Err(WinwrightError::SensitiveField { element: label });
                    }
                    out.text = Some(if e.patterns.contains(&UiPattern::Text) {
                        (e.value.clone().unwrap_or_default(), "TextPattern")
                    } else {
                        (e.name.clone(), "Name")
                    });
                }
                UiPatternAction::ClickablePoint => {
                    out.point = s.props(rid).bounds.map(|b| b.center());
                }
                UiPatternAction::ScrollIntoView => {
                    s.els.get_mut(&rid).unwrap().offscreen = false;
                }
                UiPatternAction::Scroll { .. } => return Err(unsupported("Scroll")),
            }
            out.props_after = if s.blind {
                None
            } else if s.stale_readback {
                Some(before)
            } else {
                s.els.contains_key(&rid).then(|| s.props(rid))
            };
            Ok(out)
        })
    }
}

#[derive(Default)]
struct FakeInput {
    log: Mutex<Vec<String>>,
}

impl InputBackend for FakeInput {
    fn move_to<'a>(&'a self, p: PhysicalPoint, _: &'a OperationContext) -> BackendFuture<'a, ()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("move {},{}", p.x, p.y));
        Box::pin(async { Ok(()) })
    }
    fn click<'a>(
        &'a self,
        p: PhysicalPoint,
        b: MouseButton,
        n: u32,
        _: &'a OperationContext,
    ) -> BackendFuture<'a, ()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("click {b:?} x{n} at {},{}", p.x, p.y));
        Box::pin(async { Ok(()) })
    }
    fn drag<'a>(
        &'a self,
        _: PhysicalPoint,
        _: PhysicalPoint,
        _: MouseButton,
        _: Duration,
        _: &'a OperationContext,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn scroll<'a>(
        &'a self,
        _: PhysicalPoint,
        x: i32,
        y: i32,
        _: &'a OperationContext,
    ) -> BackendFuture<'a, ()> {
        self.log.lock().unwrap().push(format!("wheel {x},{y}"));
        Box::pin(async { Ok(()) })
    }
    fn type_text<'a>(&'a self, text: &'a str, _: &'a OperationContext) -> BackendFuture<'a, ()> {
        self.log
            .lock()
            .unwrap()
            .push(format!("type chars={}", text.chars().count()));
        Box::pin(async { Ok(()) })
    }
    fn press_keys<'a>(&'a self, keys: &'a [Key], _: &'a OperationContext) -> BackendFuture<'a, ()> {
        let chord: Vec<String> = keys.iter().map(ToString::to_string).collect();
        self.log
            .lock()
            .unwrap()
            .push(format!("press {}", chord.join("+")));
        Box::pin(async { Ok(()) })
    }
    fn release_all(&self) -> WinwrightResult<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------- helpers

fn engine(fake: &Arc<Fake>) -> Engine {
    Engine::new(Config::default(), fake.clone(), fake.clone())
}

fn sid() -> SessionId {
    SessionId::parse("test").unwrap()
}

fn by(role: &str, name: &str) -> ElementTarget {
    ElementTarget::by_locator(
        ElementLocator {
            role: Some(role.into()),
            name: Some(name.into()),
            ..Default::default()
        },
        SnapshotTarget::Active,
    )
}

fn click(target: ElementTarget) -> DesktopAction {
    DesktopAction::Click {
        target,
        button: MouseButton::Left,
        click_count: 1,
        force_physical: false,
    }
}

async fn ref_of(
    engine: &Engine,
    session: &crate::session::Session,
    role: &str,
    name: &str,
) -> String {
    let found = engine
        .find(
            session,
            FindRequest {
                role: Some(role.into()),
                name: Some(name.into()),
                ..serde_json::from_str("{}").unwrap()
            },
        )
        .await
        .unwrap();
    assert_eq!(found.count, 1, "{role} {name}");
    found.matches[0].reference.clone()
}

// ---------------------------------------------------------------- snapshot / find / inspect

#[tokio::test]
async fn snapshot_renders_and_keeps_refs_stable() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let snap = engine
        .snapshot(&session, SnapshotRequest::default())
        .await
        .unwrap();
    assert_eq!(snap.generation, "s_1");
    assert!(
        snap.tree.starts_with("WINDOW \"Fixture\" [e1]\n"),
        "{}",
        snap.tree
    );
    assert!(
        snap.tree
            .contains("EDIT \"Password:\" value=\"[REDACTED]\" sensitive=true")
    );
    assert!(snap.tree.contains("CHECKBOX \"Enable feature\" unchecked"));
    assert!(snap.tree.contains("BUTTON \"Disabled Action\" disabled"));
    assert!(
        !snap.tree.contains("\"Red\""),
        "collapsed combo items are offscreen"
    );
    assert_eq!(snap.active_window.unwrap().reference, "e1");
    let again = engine
        .snapshot(&session, SnapshotRequest::default())
        .await
        .unwrap();
    assert_eq!(again.generation, "s_2");
    assert_eq!(again.tree, snap.tree);
}

#[tokio::test]
async fn window_selection_is_never_guessed() {
    let fake = Fake::new();
    fake.s()
        .windows
        .push(window(11, "Second Fixture", "fixture.exe", false));
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let req = SnapshotRequest {
        target: SnapshotTarget::Window(WindowSelector {
            process: Some("fixture".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let err = engine.snapshot(&session, req).await.unwrap_err();
    assert_eq!(err.code().as_str(), "ELEMENT_AMBIGUOUS");
    assert_eq!(err.payload().matches.len(), 2);
    let missing = SnapshotRequest {
        target: SnapshotTarget::Window(WindowSelector {
            title: Some("Photoshop".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        engine
            .snapshot(&session, missing)
            .await
            .unwrap_err()
            .code()
            .as_str(),
        "WINDOW_NOT_FOUND"
    );
}

#[tokio::test]
async fn find_returns_refs_ranked_and_label_inference_works() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let saves = engine
        .find(
            &session,
            serde_json::from_str(r#"{"role":"Button","name":"Save"}"#).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saves.count, 2);
    assert_ne!(saves.matches[0].reference, saves.matches[1].reference);
    assert_eq!(
        saves.matches[0].path,
        "Window \"Fixture\" > Button \"Save\""
    );

    let by_label = engine
        .find(
            &session,
            serde_json::from_str(r#"{"label":"Name"}"#).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(by_label.count, 1);
    assert_eq!(by_label.matches[0].automation_id, "101");

    let offscreen = engine
        .find(
            &session,
            serde_json::from_str(r#"{"role":"ListItem","name":"Green","visibleOnly":false}"#)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        offscreen.count, 1,
        "visibleOnly=false searches collapsed items"
    );
}

#[tokio::test]
async fn inspect_assigns_a_ref_and_path() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let d = engine
        .inspect(&session, InspectRequest::UnderCursor)
        .await
        .unwrap();
    assert_eq!(d.element.name, "Target");
    assert_eq!(d.element.path, "Window \"Fixture\" > Button \"Target\"");
    let again = engine
        .inspect(&session, InspectRequest::Ref(d.element.reference.clone()))
        .await
        .unwrap();
    assert_eq!(again.element.reference, d.element.reference);
}

// ---------------------------------------------------------------- actions

#[tokio::test]
async fn invoke_that_opens_a_window_is_verified() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(&session, click(by("Button", "Open Dialog")))
        .await
        .unwrap();
    assert!(r.success && r.executed && r.verified, "{r:?}");
    assert_eq!(r.method, ActionMethod::InvokePattern);
    assert_eq!(r.opened_windows, ["\"Fixture Dialog\" (fixture.exe)"]);
    assert!(r.reference.is_some());
}

#[tokio::test]
async fn invoke_without_observable_change_is_executed_but_unverified() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let save = engine
        .find(
            &session,
            serde_json::from_str(r#"{"role":"Button","automationId":"s1"}"#).unwrap(),
        )
        .await
        .unwrap();
    let r = engine
        .execute(
            &session,
            click(ElementTarget::by_ref(&save.matches[0].reference)),
        )
        .await
        .unwrap();
    assert!(r.executed && !r.verified);
    assert!(r.warnings[0].contains("no observable state change"));
}

#[tokio::test]
async fn ambiguous_locator_reports_candidate_refs() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .execute(&session, click(by("Button", "Save")))
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "ELEMENT_AMBIGUOUS");
    let matches = err.payload().matches;
    assert_eq!(matches.len(), 2);
    assert!(matches.iter().all(|m| m.starts_with('e')), "{matches:?}");
    assert!(fake.s().executed.is_empty(), "nothing was clicked");
}

#[tokio::test]
async fn fill_uses_value_pattern_and_reads_back() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let name = ref_of(&engine, &session, "Edit", "Name:").await;
    let r = engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: ElementTarget::by_ref(&name),
                text: "notes.txt".into(),
                clear: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::ValuePattern);
    assert!(r.verified);
    assert_eq!(r.after.as_deref(), Some("value=\"notes.txt\""));
    let appended = engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: ElementTarget::by_ref(&name),
                text: ".bak".into(),
                clear: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(appended.after.as_deref(), Some("value=\"notes.txt.bak\""));
}

#[tokio::test]
async fn filling_a_password_needs_confirmation_and_reading_it_is_blocked() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let pw = ref_of(&engine, &session, "Edit", "Password:").await;
    let err = engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: ElementTarget::by_ref(&pw),
                text: "hunter2".into(),
                clear: true,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CONFIRMATION_REQUIRED");
    let err = engine
        .execute(
            &session,
            DesktopAction::ReadText {
                target: ElementTarget::by_ref(&pw),
                max_chars: 100,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "SENSITIVE_FIELD");
    assert!(fake.s().executed.is_empty());
}

#[tokio::test]
async fn sensitive_buttons_require_confirmation() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .execute(&session, click(by("Button", "Submit")))
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CONFIRMATION_REQUIRED");
    assert!(fake.s().executed.is_empty(), "Submit was not invoked");
    assert_eq!(fake.s().els[&STATUS].name, "Ready");
}

#[tokio::test]
async fn check_is_idempotent_and_verified() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let cb = ref_of(&engine, &session, "CheckBox", "Enable feature").await;
    let check = || DesktopAction::Check {
        target: ElementTarget::by_ref(&cb),
    };
    let first = engine.execute(&session, check()).await.unwrap();
    assert_eq!(first.method, ActionMethod::TogglePattern);
    assert!(first.verified);
    assert_eq!(first.after.as_deref(), Some("checked"));
    let second = engine.execute(&session, check()).await.unwrap();
    assert_eq!(second.method, ActionMethod::NoOp);
    let uncheck = engine
        .execute(
            &session,
            DesktopAction::Uncheck {
                target: ElementTarget::by_ref(&cb),
            },
        )
        .await
        .unwrap();
    assert_eq!(uncheck.after.as_deref(), Some("unchecked"));
    assert_eq!(
        fake.s()
            .executed
            .iter()
            .filter(|e| e.starts_with("Toggle"))
            .count(),
        2
    );
}

#[tokio::test]
async fn radio_check_selects() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(
            &session,
            DesktopAction::Check {
                target: by("RadioButton", "Small"),
            },
        )
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::SelectionItemPattern);
    assert!(r.verified);
}

#[tokio::test]
async fn select_option_in_collapsed_combo() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(
            &session,
            DesktopAction::Select {
                target: by("ComboBox", "Color:"),
                option: Some("green".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::SelectionItemPattern);
    assert!(r.verified, "{r:?}");
    let s = fake.s();
    assert_eq!(s.els[&COMBO].value.as_deref(), Some("Green"));
    assert_eq!(s.els[&COMBO].expand, Some(ExpandState::Collapsed));
    assert!(s.executed.iter().any(|e| e.starts_with("Expand")));
}

#[tokio::test]
async fn stale_ref_is_re_resolved_after_recreation() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let target = ref_of(&engine, &session, "Button", "Target").await;
    engine
        .execute(&session, click(by("Button", "Recreate")))
        .await
        .unwrap();
    assert!(!fake.s().els.contains_key(&TARGET), "old element is gone");
    let r = engine
        .execute(&session, click(ElementTarget::by_ref(&target)))
        .await
        .unwrap();
    assert_eq!(
        r.reference.as_deref(),
        Some(target.as_str()),
        "same ref keeps working"
    );
    assert_eq!(fake.s().els[&STATUS].name, "Target clicked");
    assert!(
        fake.s().executed.last().unwrap().contains("#100"),
        "the new element was invoked"
    );
}

#[tokio::test]
async fn read_text_prefers_text_pattern() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(
            &session,
            DesktopAction::ReadText {
                target: by("Document", "Notes"),
                max_chars: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::TextPattern);
    assert_eq!(r.text.as_deref(), Some("hello notes"));
}

#[tokio::test]
async fn disabled_and_elevated_targets_are_refused() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .execute(&session, click(by("Button", "Disabled Action")))
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "INVALID_REQUEST");
    assert!(err.to_string().contains("disabled"));
    fake.s().privileged = true;
    let err = engine
        .execute(&session, click(by("Button", "Target")))
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "UIPI_BLOCKED");
    assert!(fake.s().executed.is_empty());
}

#[tokio::test]
async fn keys_without_a_target_are_refused_for_an_elevated_foreground() {
    let fake = Fake::new();
    fake.s().privileged = true;
    let input = Arc::new(FakeInput::default());
    let engine = engine(&fake).with_input(input.clone());
    let session = engine.session(&sid(), "test").unwrap();
    for action in [
        DesktopAction::Press {
            target: None,
            keys: vec![Key::Ctrl, Key::Char('s')],
        },
        DesktopAction::TypeText {
            target: None,
            text: "hello".into(),
        },
    ] {
        let err = engine.execute(&session, action).await.unwrap_err();
        assert_eq!(err.code().as_str(), "UIPI_BLOCKED");
    }
    assert!(input.log.lock().unwrap().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn physical_paths_need_an_input_backend() {
    let fake = Fake::new();
    let bare = engine(&fake);
    let session = bare.session(&sid(), "test").unwrap();
    let press = DesktopAction::Press {
        target: None,
        keys: vec![Key::Ctrl, Key::Shift, Key::Char('s')],
    };
    assert_eq!(
        bare.execute(&session, press.clone())
            .await
            .unwrap_err()
            .code()
            .as_str(),
        "BACKEND_UNAVAILABLE"
    );

    let input = Arc::new(FakeInput::default());
    let with_input = engine(&fake).with_input(input.clone());
    let session = with_input.session(&sid(), "test").unwrap();
    let r = with_input.execute(&session, press).await.unwrap();
    assert_eq!(r.method, ActionMethod::PhysicalKeyboard);
    let r = with_input
        .execute(
            &session,
            DesktopAction::Click {
                target: by("Button", "Target"),
                button: MouseButton::Right,
                click_count: 1,
                force_physical: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::PhysicalClick);
    let log = input.log.lock().unwrap().clone();
    assert_eq!(log, ["press Ctrl+Shift+s", "click Right x1 at 248,8"]);
}

#[tokio::test]
async fn window_close_is_verified() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .window_action(
            &session,
            WindowAction::Close {
                window: WindowSelector {
                    title: Some("Fixture".into()),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap();
    assert!(r.verified);
    assert_eq!(r.method, ActionMethod::WindowApi);
    assert_eq!(r.closed_windows.len(), 1);
}

// ---------------------------------------------------------------- lifecycle

#[tokio::test]
async fn cancel_interrupts_a_hung_capture_and_is_terminal() {
    let fake = Arc::new(Fake {
        hang: true,
        ..Arc::into_inner(Fake::new()).unwrap()
    });
    let engine = Arc::new(engine(&fake));
    let session = engine.session(&sid(), "test").unwrap();
    let task = {
        let engine = Arc::clone(&engine);
        let session = Arc::clone(&session);
        tokio::spawn(async move { engine.snapshot(&session, SnapshotRequest::default()).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    session.cancel();
    assert_eq!(
        task.await.unwrap().unwrap_err().code().as_str(),
        "CANCELLED"
    );
    assert_eq!(
        engine
            .snapshot(&session, SnapshotRequest::default())
            .await
            .unwrap_err()
            .code()
            .as_str(),
        "CANCELLED"
    );
}

#[tokio::test]
async fn invalid_requests_fail_before_touching_backends() {
    let fake = Fake::new();
    let engine = engine(&fake);
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
        .execute(
            &session,
            DesktopAction::Focus {
                target: ElementTarget {
                    reference: None,
                    locator: None,
                    scope: SnapshotTarget::Active,
                },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "INVALID_REQUEST");
    assert!(fake.s().captured_roots.is_empty());
}

// ---------------------------------------------------------------- waits (phase 4)

fn wait(json: serde_json::Value) -> winwright_contracts::wait::WaitRequest {
    serde_json::from_value(json).unwrap()
}

#[tokio::test]
async fn wait_for_window_open_reports_what_it_saw_on_timeout() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "window-open", "window": {"title": "Fixture Dialog"}, "timeoutMs": 150})),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "TIMEOUT");
    assert!(err.to_string().contains("no window matches"), "{err}");

    engine
        .execute(&session, click(by("Button", "Open Dialog")))
        .await
        .unwrap();
    let ok = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "window-open", "window": {"title": "Fixture Dialog"}, "timeoutMs": 1000})),
        )
        .await
        .unwrap();
    assert_eq!(ok.window.unwrap().hwnd, DIALOG);
    assert_eq!(ok.checks, 1);
}

#[tokio::test]
async fn wait_for_element_states() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let visible = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "visible", "locator": {"role": "Button", "name": "Target"}})),
        )
        .await
        .unwrap();
    let target_ref = visible.element.unwrap().reference;
    assert!(target_ref.starts_with('e'));

    let err = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "enabled", "locator": {"name": "Disabled Action"}, "timeoutMs": 150})),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("is disabled"), "{err}");

    // The old Target disappears when it is recreated: a ref wait sees it go missing.
    engine
        .execute(&session, click(by("Button", "Recreate")))
        .await
        .unwrap();
    let gone = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "missing", "ref": target_ref, "timeoutMs": 500})),
        )
        .await
        .unwrap();
    assert_eq!(gone.state, winwright_contracts::wait::WaitState::Missing);

    engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: by("Edit", "Name:"),
                text: "Ada Lovelace".into(),
                clear: true,
            },
        )
        .await
        .unwrap();
    let value = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "value", "locator": {"role": "Edit", "name": "Name:"}, "value": "ada", "match": "contains"})),
        )
        .await
        .unwrap();
    assert_eq!(
        value.element.unwrap().value.as_deref(),
        Some("Ada Lovelace")
    );
}

#[tokio::test]
async fn wait_is_cancellable_and_validates_input() {
    let fake = Fake::new();
    let engine = Arc::new(engine(&fake));
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .wait_for(&session, wait(serde_json::json!({"state": "window-open"})))
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "INVALID_REQUEST");
    let task = {
        let engine = Arc::clone(&engine);
        let session = Arc::clone(&session);
        tokio::spawn(async move {
            engine
                .wait_for(
                    &session,
                    wait(serde_json::json!({"state": "visible", "locator": {"name": "Never"}, "timeoutMs": 60000})),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(80)).await;
    session.cancel();
    assert_eq!(
        task.await.unwrap().unwrap_err().code().as_str(),
        "CANCELLED"
    );
}

// ---------------------------------------------------------------- safety services

#[tokio::test]
async fn emergency_stop_halts_everything_until_rearmed() {
    let fake = Fake::new();
    let input = Arc::new(FakeInput::default());
    let engine = engine(&fake).with_input(input);
    let session = engine.session(&sid(), "test").unwrap();
    engine.emergency_stop();
    assert_eq!(
        engine
            .execute(&session, click(by("Button", "Target")))
            .await
            .unwrap_err()
            .code()
            .as_str(),
        "CANCELLED"
    );
    assert!(
        engine.session(&sid(), "test").is_err(),
        "no new sessions while stopped"
    );
    engine.rearm();
    let fresh = engine.session(&sid(), "test").unwrap();
    engine
        .execute(&fresh, click(by("Button", "Target")))
        .await
        .unwrap();
}

#[tokio::test]
async fn risky_system_operations_are_gated_by_policy() {
    use winwright_contracts::system::{ExecRequest, FileOperation};
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .file_operation(
            &session,
            FileOperation::Delete {
                path: r"C:\Users\x\Desktop\a.txt".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CONFIRMATION_REQUIRED");
    let exec: ExecRequest =
        serde_json::from_str(r#"{"program":"cmd.exe","args":["/c","echo","hi"]}"#).unwrap();
    let err = engine.exec(&session, exec).await.unwrap_err();
    assert_eq!(
        err.code().as_str(),
        "ACTION_BLOCKED",
        "shell is off by default"
    );
    let err = engine
        .file_operation(
            &session,
            FileOperation::List {
                path: "C:\\".into(),
                include_hidden: false,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.code().as_str(),
        "BACKEND_UNAVAILABLE",
        "reads pass policy"
    );
}

#[tokio::test]
async fn diff_snapshot_sends_only_changes() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let first = engine
        .snapshot(
            &session,
            SnapshotRequest {
                diff: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        first.diff.is_none() && !first.tree.is_empty(),
        "nothing to diff against yet"
    );
    engine
        .execute(
            &session,
            DesktopAction::Check {
                target: by("CheckBox", "Enable feature"),
            },
        )
        .await
        .unwrap();
    let second = engine
        .snapshot(
            &session,
            SnapshotRequest {
                diff: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(second.tree.is_empty());
    let diff = second.diff.unwrap();
    assert!(diff.starts_with("DIFF s_1 -> s_2\n"), "{diff}");
    assert!(
        diff.contains(
            "~ CHECKBOX \"Enable feature\" unchecked -> CHECKBOX \"Enable feature\" checked"
        ),
        "{diff}"
    );
    assert_eq!(diff.lines().count(), 2, "only the checkbox changed: {diff}");
}

// ---------------------------------------------------------------- phase 8: confirmations, audit

struct FakeConfirmer {
    answer: bool,
    prompts: Mutex<Vec<winwright_contracts::security::ConfirmationPrompt>>,
}

impl winwright_contracts::security::Confirmer for FakeConfirmer {
    fn confirm<'a>(
        &'a self,
        prompt: winwright_contracts::security::ConfirmationPrompt,
    ) -> BackendFuture<'a, bool> {
        self.prompts.lock().unwrap().push(prompt);
        let answer = self.answer;
        Box::pin(async move { Ok(answer) })
    }
}

fn confirmer(answer: bool) -> Arc<FakeConfirmer> {
    Arc::new(FakeConfirmer {
        answer,
        prompts: Mutex::new(Vec::new()),
    })
}

#[tokio::test]
async fn approved_confirmation_runs_the_action_once() {
    let fake = Fake::new();
    let yes = confirmer(true);
    let engine = engine(&fake).with_confirmer(yes.clone());
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(&session, click(by("Button", "Submit")))
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::InvokePattern);
    assert_eq!(fake.s().els[&STATUS].name, "Submitted");
    let prompts = yes.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].summary, "Click Button \"Submit\"");
    assert!(prompts[0].reason.contains("send, submit"));
}

#[tokio::test]
async fn declined_confirmation_blocks_and_nothing_runs() {
    let fake = Fake::new();
    let engine = engine(&fake).with_confirmer(confirmer(false));
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .execute(&session, click(by("Button", "Submit")))
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "ACTION_BLOCKED");
    assert!(err.to_string().contains("declined"), "{err}");
    assert!(fake.s().executed.is_empty());
    assert!(
        engine.lease().holder().is_none(),
        "lease released after the dialog"
    );
}

#[tokio::test]
async fn fill_prompt_never_contains_the_typed_text() {
    let fake = Fake::new();
    let yes = confirmer(true);
    let engine = engine(&fake).with_confirmer(yes.clone());
    let session = engine.session(&sid(), "test").unwrap();
    engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: by("Edit", "Password:"),
                text: "hunter2".into(),
                clear: true,
            },
        )
        .await
        .unwrap();
    let prompts = yes.prompts.lock().unwrap();
    assert_eq!(
        prompts[0].summary,
        "Enter 7 characters into Edit \"Password:\""
    );
    assert!(!format!("{:?}", prompts[0]).contains("hunter2"));
}

#[tokio::test]
async fn winwright_never_automates_its_own_windows() {
    let fake = Fake::new();
    let mut own = window(99, "Winwright: confirm action", "winwright.exe", true);
    own.process_id = std::process::id();
    {
        let mut s = fake.s();
        for w in &mut s.windows {
            w.foreground = false;
        }
        s.windows.push(own);
    }
    let engine = engine(&fake).with_input(Arc::new(FakeInput::default()));
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .window_action(
            &session,
            WindowAction::Close {
                window: WindowSelector {
                    hwnd: Some(99),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "ACTION_BLOCKED");
    // Keys go to the foreground window, which is Winwright's dialog here.
    let err = engine
        .execute(
            &session,
            DesktopAction::Press {
                target: None,
                keys: vec![Key::Enter],
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "ACTION_BLOCKED");
    assert!(err.to_string().contains("Winwright's own windows"), "{err}");
}

#[tokio::test]
async fn audit_log_records_outcomes_without_text() {
    let fake = Fake::new();
    let dir = crate::scratch_dir("engine-audit");
    let path = dir.join("audit.jsonl");
    let engine =
        engine(&fake)
            .with_confirmer(confirmer(false))
            .with_audit(crate::audit::AuditLog::new(
                path.clone(),
                crate::audit::DEFAULT_MAX_BYTES,
            ));
    let session = engine.session(&sid(), "test").unwrap();
    engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: by("Edit", "Name:"),
                text: "top secret words".into(),
                clear: true,
            },
        )
        .await
        .unwrap();
    let _ = engine
        .execute(&session, click(by("Button", "Submit")))
        .await;
    // Reads are not audited.
    engine
        .execute(
            &session,
            DesktopAction::ReadText {
                target: by("Document", "Notes"),
                max_chars: 10,
            },
        )
        .await
        .unwrap();
    let lines = crate::audit::AuditLog::tail(&path, 10).unwrap();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].contains("\"tool\":\"desktop_fill\"") && lines[0].contains("\"result\":\"ok\"")
    );
    assert!(lines[0].contains("\"method\":\"ValuePattern\""));
    assert!(lines[1].contains("\"result\":\"ACTION_BLOCKED\""));
    assert!(
        !lines.join("\n").contains("secret"),
        "typed text never reaches the log"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- regressions

#[tokio::test]
async fn emergency_stop_refuses_session_less_calls_until_rearmed() {
    let fake = Fake::new();
    let engine = engine(&fake);
    engine.emergency_stop();
    let main = WindowSelector {
        hwnd: Some(MAIN),
        ..Default::default()
    };
    for (what, err) in [
        (
            "list_windows",
            engine.list_windows().map(|_| ()).unwrap_err(),
        ),
        (
            "active_window",
            engine.active_window().map(|_| ()).unwrap_err(),
        ),
        (
            "find_window",
            engine.find_window(&main).map(|_| ()).unwrap_err(),
        ),
        (
            "process_list",
            engine.process_list().map(|_| ()).unwrap_err(),
        ),
        ("clear_overlays", engine.clear_overlays(None).unwrap_err()),
    ] {
        assert_eq!(err.code().as_str(), "CANCELLED", "{what}");
    }
    engine.rearm();
    assert_eq!(engine.list_windows().unwrap().len(), 1);
    assert_eq!(engine.find_window(&main).unwrap().hwnd, MAIN);
}

#[tokio::test]
async fn toggle_is_never_repeated_without_a_fresh_reading() {
    let fake = Fake::new();
    {
        let mut s = fake.s();
        s.blind = true;
        s.inert_toggle = true;
    }
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(
            &session,
            DesktopAction::Check {
                target: by("CheckBox", "Enable feature"),
            },
        )
        .await
        .unwrap();
    assert!(!r.verified, "{r:?}");
    let toggles = fake
        .s()
        .executed
        .iter()
        .filter(|e| e.starts_with("Toggle"))
        .count();
    assert_eq!(toggles, 1, "a toggle that may still land is not repeated");
}

#[tokio::test]
async fn fill_waits_for_a_late_value_instead_of_retyping_it() {
    let fake = Fake::new();
    fake.s().els.get_mut(&3).unwrap().value = Some("notes.txt".into());
    fake.s().stale_readback = true;
    let input = Arc::new(FakeInput::default());
    let engine = engine(&fake).with_input(input.clone());
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: by("Edit", "Name:"),
                text: ".bak".into(),
                clear: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::ValuePattern, "{r:?}");
    assert!(r.verified);
    assert!(input.log.lock().unwrap().is_empty(), "nothing was retyped");
    assert_eq!(fake.s().els[&3].value.as_deref(), Some("notes.txt.bak"));
}

#[tokio::test]
async fn keyboard_typing_is_not_verified_by_focus_or_old_text() {
    let fake = Fake::new();
    {
        let mut s = fake.s();
        let name = s.els.get_mut(&3).unwrap();
        name.read_only = true;
        name.value = Some("banana".into());
    }
    let input = Arc::new(FakeInput::default());
    let engine = engine(&fake)
        .with_input(input.clone())
        .with_confirmer(confirmer(true));
    let session = engine.session(&sid(), "test").unwrap();
    // The fake keyboard changes nothing: "banana" already ends with and contains "na".
    let fill = engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: by("Edit", "Name:"),
                text: "na".into(),
                clear: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(fill.method, ActionMethod::PhysicalKeyboard);
    assert!(!fill.verified, "{fill:?}");
    fake.s().els.get_mut(&3).unwrap().focused = false;
    let typed = engine
        .execute(
            &session,
            DesktopAction::TypeText {
                target: Some(by("Edit", "Name:")),
                text: "an".into(),
            },
        )
        .await
        .unwrap();
    assert!(!typed.verified, "{typed:?}");
    // A password cannot be read back; focusing it is no evidence the text arrived.
    let secret = engine
        .execute(
            &session,
            DesktopAction::Fill {
                target: by("Edit", "Password:"),
                text: "hunter2".into(),
                clear: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(secret.method, ActionMethod::PhysicalKeyboard);
    assert!(!secret.verified, "{secret:?}");
}

#[tokio::test]
async fn enter_on_a_focused_send_style_button_needs_confirmation() {
    let fake = Fake::new();
    let input = Arc::new(FakeInput::default());
    let engine = engine(&fake).with_input(input.clone());
    let session = engine.session(&sid(), "test").unwrap();
    engine
        .execute(
            &session,
            DesktopAction::Focus {
                target: by("Button", "Submit"),
            },
        )
        .await
        .unwrap();
    let err = engine
        .execute(
            &session,
            DesktopAction::Press {
                target: None,
                keys: vec![Key::Enter],
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CONFIRMATION_REQUIRED");
    assert!(
        err.to_string()
            .contains("Press Enter in Button \"Submit\" (focused)"),
        "the prompt names what Enter would activate: {err}"
    );
    assert!(input.log.lock().unwrap().is_empty(), "Enter was not sent");
}

#[tokio::test]
async fn combo_select_is_judged_by_its_value_not_side_effects() {
    let fake = Fake::new();
    {
        let mut s = fake.s();
        let mut item = el(
            ControlRole::ListItem,
            "More colors...",
            "",
            &[UiPattern::Invoke],
        );
        item.offscreen = true;
        s.els.insert(40, item);
        s.els.get_mut(&COMBO).unwrap().children.push(40);
    }
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(
            &session,
            DesktopAction::Select {
                target: by("ComboBox", "Color:"),
                option: Some("More colors...".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::InvokePattern);
    assert_eq!(r.opened_windows.len(), 1, "{r:?}");
    assert!(!r.verified, "the combo still shows no color: {r:?}");
    assert!(
        r.warnings.iter().any(|w| w.contains("still shows")),
        "{r:?}"
    );
}

#[tokio::test]
async fn failed_combo_select_closes_the_list_it_opened() {
    let fake = Fake::new();
    fake.s().els.get_mut(&COMBO).unwrap().read_only = false;
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .execute(
            &session,
            DesktopAction::Select {
                target: by("ComboBox", "Color:"),
                option: Some("Purple".into()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "UNSUPPORTED_PATTERN");
    assert_eq!(fake.s().els[&COMBO].expand, Some(ExpandState::Collapsed));
    // An empty option would match every item by "contains".
    let err = engine
        .execute(
            &session,
            DesktopAction::Select {
                target: by("ComboBox", "Color:"),
                option: Some(" ".into()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "INVALID_REQUEST");
}

#[tokio::test]
async fn window_geometry_is_validated() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let window = || WindowSelector {
        hwnd: Some(MAIN),
        ..Default::default()
    };
    for action in [
        WindowAction::Move {
            window: window(),
            x: i32::MAX,
            y: 0,
        },
        WindowAction::Resize {
            window: window(),
            width: -5,
            height: 100,
        },
        WindowAction::SetBounds {
            window: window(),
            bounds: PhysicalRect::new(100, 100, 50, 300),
        },
    ] {
        let err = engine.window_action(&session, action).await.unwrap_err();
        assert_eq!(err.code().as_str(), "INVALID_REQUEST", "{err}");
    }
    assert_eq!(
        fake.s().windows[0].bounds,
        PhysicalRect::new(0, 0, 800, 600),
        "nothing moved"
    );
    let r = engine
        .window_action(
            &session,
            WindowAction::Move {
                window: window(),
                x: -1920,
                y: 40,
            },
        )
        .await
        .unwrap();
    assert!(r.verified);
}

#[tokio::test]
async fn waits_honor_nth() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let second = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "visible", "locator": {"role": "Button", "name": "Save", "nth": 1}})),
        )
        .await
        .unwrap();
    assert_eq!(second.element.unwrap().automation_id, "s2");
    let err = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "exists", "locator": {"role": "Button", "name": "Save", "nth": 2}, "timeoutMs": 150})),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "TIMEOUT", "there is no third Save");
    engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "missing", "locator": {"role": "Button", "name": "Save", "nth": 2}, "timeoutMs": 150})),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn launching_a_shell_is_gated_like_shell_execute() {
    use winwright_contracts::system::{ExecRequest, LaunchRequest};
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    for app in [
        "cmd.exe",
        "CMD",
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
        "pwsh",
        "mshta.exe",
    ] {
        let launch: LaunchRequest =
            serde_json::from_value(serde_json::json!({"app": app, "args": ["/c", "x"]})).unwrap();
        let err = engine.launch_app(&session, launch).await.unwrap_err();
        assert_eq!(err.code().as_str(), "ACTION_BLOCKED", "{app}");
    }
    let notepad: LaunchRequest = serde_json::from_str(r#"{"app":"notepad.exe"}"#).unwrap();
    assert_eq!(
        engine
            .launch_app(&session, notepad)
            .await
            .unwrap_err()
            .code()
            .as_str(),
        "BACKEND_UNAVAILABLE",
        "ordinary apps pass the policy"
    );

    // Enabling the shell does not enable PowerShell.
    let mut config = Config::default();
    config.security.allow_shell = true;
    let engine = Engine::new(config, fake.clone(), fake.clone());
    let session = engine.session(&sid(), "test").unwrap();
    let ps: ExecRequest =
        serde_json::from_str(r#"{"program":"powershell.exe","args":["-c","x"]}"#).unwrap();
    assert_eq!(
        engine.exec(&session, ps).await.unwrap_err().code().as_str(),
        "ACTION_BLOCKED"
    );
    let cmd: ExecRequest = serde_json::from_str(r#"{"program":"cmd.exe","args":["/c"]}"#).unwrap();
    assert_eq!(
        engine
            .exec(&session, cmd)
            .await
            .unwrap_err()
            .code()
            .as_str(),
        "CONFIRMATION_REQUIRED"
    );
}

#[tokio::test]
async fn selecting_a_risky_option_needs_confirmation() {
    let fake = Fake::new();
    {
        let mut s = fake.s();
        let mut menu = el(ControlRole::Menu, "Actions", "", &[]);
        menu.children = vec![42];
        s.els.insert(41, menu);
        s.els.insert(
            42,
            el(
                ControlRole::MenuItem,
                "Delete account",
                "",
                &[UiPattern::Invoke],
            ),
        );
        s.els.get_mut(&1).unwrap().children.push(41);
    }
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .execute(
            &session,
            DesktopAction::Select {
                target: by("Menu", "Actions"),
                option: Some("Delete account".into()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "CONFIRMATION_REQUIRED");
    assert!(fake.s().executed.is_empty(), "nothing was invoked");
}

#[tokio::test]
async fn delete_key_outside_text_needs_confirmation() {
    let fake = Fake::new();
    let input = Arc::new(FakeInput::default());
    let engine = engine(&fake).with_input(input.clone());
    let session = engine.session(&sid(), "test").unwrap();
    let press = |target: Option<ElementTarget>| DesktopAction::Press {
        target,
        keys: vec![Key::Shift, Key::Delete],
    };
    for target in [Some(by("Button", "Target")), None] {
        let err = engine.execute(&session, press(target)).await.unwrap_err();
        assert_eq!(err.code().as_str(), "CONFIRMATION_REQUIRED");
    }
    assert!(input.log.lock().unwrap().is_empty());
    engine
        .execute(&session, press(Some(by("Edit", "Name:"))))
        .await
        .unwrap();
    assert_eq!(*input.log.lock().unwrap(), ["press Shift+Delete"]);
}

#[tokio::test]
async fn wait_rejects_a_bad_value_pattern_up_front() {
    let fake = Fake::new();
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let err = engine
        .wait_for(
            &session,
            wait(serde_json::json!({"state": "value", "locator": {"role": "Edit", "name": "Name:"}, "value": "(", "match": "regex", "timeoutMs": 150})),
        )
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "INVALID_REQUEST", "{err}");
}

#[tokio::test]
async fn scrolling_a_target_into_view_does_not_verify_a_physical_click() {
    let fake = Fake::new();
    {
        let mut s = fake.s();
        let target = s.els.get_mut(&TARGET).unwrap();
        target.offscreen = true;
        target.patterns.push(UiPattern::ScrollItem);
    }
    let input = Arc::new(FakeInput::default());
    let engine = engine(&fake).with_input(input.clone());
    let session = engine.session(&sid(), "test").unwrap();
    let r = engine
        .execute(
            &session,
            DesktopAction::Click {
                target: ElementTarget::by_locator(
                    ElementLocator {
                        role: Some("Button".into()),
                        name: Some("Target".into()),
                        visible_only: false,
                        ..Default::default()
                    },
                    SnapshotTarget::Active,
                ),
                button: MouseButton::Right,
                click_count: 1,
                force_physical: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(r.method, ActionMethod::PhysicalClick);
    assert_eq!(input.log.lock().unwrap().len(), 1, "the click was sent");
    assert!(!r.verified, "the click itself changed nothing: {r:?}");
}

/// Records the time budget each `exec` call was given.
#[derive(Default)]
struct FakeProcesses {
    exec_budget: Mutex<Option<Duration>>,
}

impl winwright_contracts::system::ProcessService for FakeProcesses {
    fn launch<'a>(
        &'a self,
        _: winwright_contracts::system::LaunchRequest,
        _: &'a OperationContext,
    ) -> BackendFuture<'a, winwright_contracts::system::LaunchResult> {
        Box::pin(async { Err(WinwrightError::invalid("not used")) })
    }
    fn list(&self) -> WinwrightResult<Vec<winwright_contracts::system::ProcessInfo>> {
        Ok(Vec::new())
    }
    fn exec<'a>(
        &'a self,
        _: winwright_contracts::system::ExecRequest,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, winwright_contracts::system::ExecResult> {
        *self.exec_budget.lock().unwrap() = Some(ctx.remaining());
        Box::pin(async { Err(WinwrightError::invalid("fake exec")) })
    }
}

#[tokio::test]
async fn exec_gets_the_time_it_asked_for() {
    let fake = Fake::new();
    let mut config = Config::default();
    config.security.allow_shell = true;
    let processes = Arc::new(FakeProcesses::default());
    let engine = Engine::new(config, fake.clone(), fake.clone())
        .with_processes(processes.clone())
        .with_confirmer(confirmer(true));
    let session = engine.session(&sid(), "test").unwrap();
    let request = serde_json::from_str(r#"{"program":"tool.exe","timeoutMs":60000}"#).unwrap();
    let _ = engine.exec(&session, request).await;
    let budget = processes.exec_budget.lock().unwrap().unwrap();
    assert!(
        budget >= Duration::from_secs(59),
        "a 60 s run is not cut at the 10 s default: {budget:?}"
    );
}

#[tokio::test]
async fn other_winwright_processes_are_never_automated() {
    let fake = Fake::new();
    {
        let mut s = fake.s();
        let mut dialog = el(ControlRole::Dialog, "Winwright: confirm action", "", &[]);
        dialog.pid = OTHER_WINWRIGHT;
        dialog.children = vec![51];
        s.els.insert(50, dialog);
        let mut yes = el(ControlRole::Button, "Yes", "", &[UiPattern::Invoke]);
        yes.pid = OTHER_WINWRIGHT;
        s.els.insert(51, yes);
        s.roots.insert(98, 50);
        let mut w = window(98, "Winwright: confirm action", "winwright.exe", false);
        w.process_id = OTHER_WINWRIGHT;
        s.windows.push(w);
    }
    let engine = engine(&fake);
    let session = engine.session(&sid(), "test").unwrap();
    let dialog = || WindowSelector {
        hwnd: Some(98),
        ..Default::default()
    };
    let yes = ElementTarget::by_locator(
        ElementLocator {
            role: Some("Button".into()),
            name: Some("Yes".into()),
            ..Default::default()
        },
        SnapshotTarget::Window(dialog()),
    );
    let err = engine.execute(&session, click(yes)).await.unwrap_err();
    assert_eq!(err.code().as_str(), "ACTION_BLOCKED", "{err}");
    let err = engine
        .window_action(&session, WindowAction::Close { window: dialog() })
        .await
        .unwrap_err();
    assert_eq!(err.code().as_str(), "ACTION_BLOCKED", "{err}");
    assert!(fake.s().executed.is_empty(), "the dialog was not answered");
    assert_eq!(fake.s().windows.len(), 2, "the dialog was not closed");
}
