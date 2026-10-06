//! Learning by watching (Phase 26.10): the person does a task once while their own clicks are
//! recorded with the element under each, so the model can save the steps and teach them back
//! with `desktop_guide`. Keys are never seen: a field the person typed in is noted by its name
//! only, and never a password field.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::unbounded_channel;
use winwright_contracts::backend::{InspectTarget, UiInspection};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::input::MouseButton;
use winwright_contracts::overlay::{
    OverlayRequest, OverlayService, OverlayStyle, PointerEvent, RecordRequest, RecordResult,
    RecordStop, RecordedAction, RecordedStep,
};
use winwright_contracts::security::{ActionRisk, Capability};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::engine::Engine;
use crate::guide::Drawn;
use crate::services::proposed;
use crate::session::Session;

const MAX_SECONDS: u32 = 600;
const MAX_STEPS: usize = 50;
/// How often the focused field and the person's last input are looked at between clicks.
const LOOK_EVERY: Duration = Duration::from_secs(1);
/// The recording notice: red, like a recording light.
const RECORD_COLOR: u32 = 0x00E5_484D;

/// The idle time and the whole recording's limit (capped at 10 minutes).
fn limits(request: &RecordRequest) -> WinwrightResult<(Duration, Duration)> {
    if request.idle_seconds == 0 || request.max_seconds == 0 {
        return Err(WinwrightError::invalid(
            "idleSeconds and maxSeconds must be positive",
        ));
    }
    let max = request.max_seconds.min(MAX_SECONDS);
    Ok((
        Duration::from_secs(request.idle_seconds.min(max).into()),
        Duration::from_secs(max.into()),
    ))
}

fn value_hash(value: &str) -> u64 {
    let mut h = DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

/// A step for the element `hit` describes.
fn step(
    hit: &UiInspection,
    action: RecordedAction,
    button: Option<MouseButton>,
    at: Option<PhysicalPoint>,
) -> RecordedStep {
    let props = &hit.props;
    RecordedStep {
        action,
        button,
        // The outermost ancestor is the top-level window.
        window: hit.ancestors.first().unwrap_or(props).name.clone(),
        role: props.role.as_str().to_owned(),
        name: props.name.clone(),
        automation_id: props.automation_id.clone(),
        at,
    }
}

/// The focused field as last seen; its value only as a hash, so the text is never kept.
struct Field {
    id: (u32, Vec<i32>),
    first: u64,
    last: u64,
    typed: RecordedStep,
}

/// The focused element as a field to watch: one with a value that is not a password.
fn field(hit: &UiInspection) -> Option<Field> {
    let props = &hit.props;
    if props.is_password {
        return None;
    }
    let value = props.value.as_deref()?;
    let hash = value_hash(value);
    let mut typed = step(hit, RecordedAction::Typed, None, None);
    // Some fields name themselves by their text: then the name would tell what was typed.
    if props.name == value {
        typed.name.clear();
    }
    Some(Field {
        id: (props.process_id, props.runtime_id.clone()),
        first: hash,
        last: hash,
        typed,
    })
}

/// Notices a focused field whose value changed: the person typed in it.
#[derive(Default)]
struct Typing {
    field: Option<Field>,
}

impl Typing {
    /// Takes in what has focus now; when the focus left a field that changed, its step.
    fn see(&mut self, focused: Option<Field>) -> Option<RecordedStep> {
        if let (Some(field), Some(now)) = (self.field.as_mut(), focused.as_ref())
            && field.id == now.id
        {
            field.last = now.last;
            return None;
        }
        std::mem::replace(&mut self.field, focused)
            .filter(|f| f.first != f.last)
            .map(|f| f.typed)
    }

    /// The focused field's step when it changed since the last time, which starts over.
    fn flush(&mut self) -> Option<RecordedStep> {
        let field = self.field.as_mut()?;
        (field.first != field.last).then(|| {
            field.first = field.last;
            field.typed.clone()
        })
    }
}

impl Engine {
    /// Learns by watching: after the person agrees, records their own clicks (and, by name only,
    /// the fields they typed in) until they stop for the idle time, the time is up, or 50 steps.
    pub async fn record_clicks(
        &self,
        session: &Session,
        request: RecordRequest,
    ) -> WinwrightResult<RecordResult> {
        let started = Instant::now();
        let (idle, limit) = limits(&request)?;
        let overlay = self.overlay_service()?;
        let action = proposed(
            "desktop_record",
            Capability::WatchPerson,
            ActionRisk::Sensitive,
            None,
        );
        let summary = format!(
            "Watch your clicks (never your keys) to learn this task, until you stop for {} s \
             (at most {} s)",
            idle.as_secs(),
            limit.as_secs()
        );
        let mut confirmed = false;
        let result = async {
            let mut lease = None;
            confirmed = self.permit(session, action, summary, &mut lease).await?;
            // Nothing acts for the person while they show the task; the lease is not needed.
            drop(lease);
            self.watch_clicks(session, overlay, idle, limit).await
        }
        .await;
        self.record(
            session,
            "desktop_record",
            None,
            None,
            &result,
            confirmed,
            started,
        );
        result
    }

    async fn watch_clicks(
        &self,
        session: &Session,
        overlay: &dyn OverlayService,
        idle: Duration,
        limit: Duration,
    ) -> WinwrightResult<RecordResult> {
        let started = Instant::now();
        let ctx = session.operation(limit)?;
        let (tx, mut events) = unbounded_channel::<PointerEvent>();
        let _watch = overlay.watch_pointer(Box::new(move |event| {
            let _ = tx.send(event);
        }))?;
        let _notice = self.recording_notice(overlay, idle, limit);
        let mut steps = Vec::new();
        let mut typing = Typing::default();
        let mut active = Instant::now();
        let mut input = self.windows.last_input_ms();
        let stopped = loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else {
                        return Err(WinwrightError::BackendUnavailable {
                            backend: "overlay".into(),
                            reason: "the click watcher stopped".into(),
                        });
                    };
                    if event.down {
                        active = Instant::now();
                        // What was typed before this click comes first.
                        steps.extend(typing.see(self.focused_field(session).await));
                        steps.extend(typing.flush());
                        steps.extend(self.clicked(session, event).await);
                    }
                }
                () = tokio::time::sleep(LOOK_EVERY) => {
                    // Keys and mouse moves keep the recording going too (never which key).
                    let now = self.windows.last_input_ms();
                    if now != input {
                        input = now;
                        active = Instant::now();
                    }
                    steps.extend(typing.see(self.focused_field(session).await));
                }
                () = ctx.cancel.cancelled() => return Err(WinwrightError::Cancelled),
                () = tokio::time::sleep_until(ctx.deadline.into()) => break RecordStop::TimeLimit,
            }
            if steps.len() >= MAX_STEPS {
                break RecordStop::StepLimit;
            }
            if active.elapsed() >= idle {
                break RecordStop::Idle;
            }
        };
        // Typing just before the end counts too.
        steps.extend(typing.see(self.focused_field(session).await));
        steps.extend(typing.flush());
        steps.truncate(MAX_STEPS);
        Ok(RecordResult {
            steps,
            stopped,
            seconds: started.elapsed().as_secs() as u32,
        })
    }

    /// "Recording your clicks" at the top of the main screen while it lasts; `None` when it
    /// cannot be shown (the recording goes on without it).
    fn recording_notice<'a>(
        &self,
        overlay: &'a dyn OverlayService,
        idle: Duration,
        limit: Duration,
    ) -> Option<Drawn<'a>> {
        self.ensure_no_confirmation_open().ok()?;
        let monitors = self.capture.as_deref()?.monitors().ok()?;
        let work = monitors
            .iter()
            .find(|m| m.primary)
            .or(monitors.first())?
            .work_area;
        let x = work.center().x;
        // A recording light, its label just above it.
        let rect = PhysicalRect::new(x - 10, work.top + 40, x + 10, work.top + 60);
        let id = overlay
            .show(OverlayRequest {
                rect,
                style: OverlayStyle::ClickMarker,
                label: Some(format!(
                    "Recording your clicks \u{2014} stops after {} s idle",
                    idle.as_secs()
                )),
                step: None,
                steps: None,
                color: RECORD_COLOR,
                duration_ms: Some(limit.as_millis() as u64 + 1_000),
            })
            .ok()?;
        Some(Drawn { overlay, id })
    }

    /// The focused element as a field to watch.
    async fn focused_field(&self, session: &Session) -> Option<Field> {
        let ctx = session.operation(self.timeout()).ok()?;
        let hit = self.uia.inspect(InspectTarget::Focused, &ctx).await.ok()?;
        self.release(vec![hit.key]).await;
        field(&hit)
    }

    /// The step for a click: the element under it. Winwright's own windows are left out.
    async fn clicked(&self, session: &Session, event: PointerEvent) -> Option<RecordedStep> {
        let ctx = session.operation(self.timeout()).ok()?;
        let hit = self
            .uia
            .inspect(InspectTarget::Point(event.point), &ctx)
            .await
            .ok()?;
        self.release(vec![hit.key]).await;
        self.guard_self(hit.props.process_id, "it").ok()?;
        Some(step(
            &hit,
            RecordedAction::Click,
            Some(event.button),
            Some(event.point),
        ))
    }
}

#[cfg(test)]
mod tests {
    use winwright_contracts::backend::{ElementKey, UiProps};
    use winwright_contracts::element::ControlRole;

    use super::*;

    fn props(name: &str, role: ControlRole, id: i32) -> UiProps {
        UiProps {
            name: name.into(),
            role,
            process_id: 7,
            runtime_id: vec![id],
            ..Default::default()
        }
    }

    fn hit(props: UiProps) -> UiInspection {
        UiInspection {
            key: ElementKey {
                worker_epoch: 1,
                slot: 1,
            },
            props,
            ancestors: vec![props_window()],
        }
    }

    fn props_window() -> UiProps {
        props("Untitled - Notepad", ControlRole::Window, 1)
    }

    fn edit(id: i32, value: &str) -> Option<Field> {
        let mut p = props("Search", ControlRole::Edit, id);
        p.value = Some(value.into());
        field(&hit(p))
    }

    #[test]
    fn steps_name_the_window_the_element_and_the_click() {
        let mut p = props("Save", ControlRole::Button, 5);
        p.automation_id = "SaveButton".into();
        let at = PhysicalPoint { x: 40, y: 50 };
        let s = step(
            &hit(p),
            RecordedAction::Click,
            Some(MouseButton::Left),
            Some(at),
        );
        assert_eq!(
            (s.window.as_str(), s.role.as_str(), s.name.as_str()),
            ("Untitled - Notepad", "Button", "Save")
        );
        assert_eq!((s.automation_id.as_str(), s.at), ("SaveButton", Some(at)));
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"automationId\":\"SaveButton\""), "{json}");
    }

    #[test]
    fn typing_is_noted_by_field_name_never_by_text() {
        let mut t = Typing::default();
        assert_eq!(t.see(edit(2, "")), None);
        assert_eq!(t.see(edit(2, "h")), None);
        assert_eq!(t.see(edit(2, "hello")), None);
        // The focus moves on: the field it left was typed in.
        let typed = t.see(edit(3, "")).unwrap();
        assert_eq!(typed.action, RecordedAction::Typed);
        assert_eq!((typed.name.as_str(), typed.at), ("Search", None));
        let json = serde_json::to_string(&typed).unwrap();
        assert!(!json.contains("hello"), "{json}");
        // Nothing typed in the new field: nothing to note.
        assert_eq!(t.see(None), None);
    }

    #[test]
    fn a_click_flushes_typing_once_and_unchanged_fields_say_nothing() {
        let mut t = Typing::default();
        t.see(edit(2, "a"));
        assert_eq!(t.flush(), None);
        t.see(edit(2, "ab"));
        assert!(t.flush().is_some());
        // Counted once; only new typing counts again.
        assert_eq!(t.flush(), None);
        assert_eq!(t.see(edit(4, "x")), None);
    }

    #[test]
    fn password_fields_and_fields_named_by_their_text_give_nothing_away() {
        let mut secret = props("Password", ControlRole::Edit, 9);
        secret.is_password = true;
        secret.value = Some("hunter2".into());
        assert!(field(&hit(secret)).is_none());
        // No value at all (a button has focus): nothing to watch.
        assert!(field(&hit(props("OK", ControlRole::Button, 3))).is_none());
        let mut named = props("draft text", ControlRole::Edit, 6);
        named.value = Some("draft text".into());
        let f = field(&hit(named)).unwrap();
        assert_eq!((f.typed.name.as_str(), f.typed.role.as_str()), ("", "Edit"));
    }

    #[test]
    fn limits_are_positive_and_capped() {
        let req = |idle, max| RecordRequest {
            idle_seconds: idle,
            max_seconds: max,
        };
        let (idle, max) = limits(&req(15, 120)).unwrap();
        assert_eq!((idle.as_secs(), max.as_secs()), (15, 120));
        let (idle, max) = limits(&req(900, 5_000)).unwrap();
        assert_eq!((idle.as_secs(), max.as_secs()), (600, 600));
        assert!(limits(&req(0, 120)).is_err());
        assert!(limits(&req(15, 0)).is_err());
    }
}
