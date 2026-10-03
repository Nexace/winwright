//! Level-4 physical input (spec §4): pointer and keyboard synthesis through `SendInput`, used only
//! when semantic UI Automation patterns cannot do the job.
//!
//! # Coordinates and DPI
//! Every point is a physical virtual-desktop pixel and may be negative (monitors left of or above
//! the primary). Absolute moves use `MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE |
//! MOUSEEVENTF_VIRTUALDESK`, normalized against `SM_[XY]VIRTUALSCREEN` / `SM_C[XY]VIRTUALSCREEN`
//! so the first pixel maps to 0 and the last to 65535. Points off the virtual screen fail with
//! `INPUT_FAILED`.
//!
//! **The process must be Per-Monitor-V2 DPI aware** (the CLI calls
//! `winwright_win32::enable_per_monitor_dpi_awareness` at startup). Otherwise the virtual-screen
//! metrics are DPI-virtualized and absolute moves miss on scaled monitors (spec §43).
//!
//! # Design
//! Each operation is planned as pure `RawInput` batches (unit-tested in `plan`), then each batch is
//! handed to one `SendInput` call. Operations on one backend are serialized, and each checks its
//! [`OperationContext`] before every batch; nothing is retried blindly.
//!
//! # Held state and emergency release
//! Every key and button that `SendInput` actually inserted as "down" is tracked until its "up" is
//! inserted. A sequence that fails, is cancelled, times out, or whose future is dropped releases
//! what it pressed before returning. [`InputBackend::release_all`] is synchronous, never waits on
//! an operation, releases everything still held, and makes in-flight sequences stop with
//! `CANCELLED` instead of injecting further input.
//!
//! # Failure reporting
//! * Locked workstation, UAC secure desktop, or a non-interactive session: `OpenInputDesktop`
//!   fails and the operation returns `BACKEND_UNAVAILABLE` before sending anything.
//! * `SendInput` inserting fewer events than requested: `INPUT_FAILED` with `GetLastError`.
//! * UIPI is **not** reported: `SendInput` silently drops input aimed at a window of higher
//!   integrity, so callers must verify the outcome (spec §4, §67).
//!
//! # Logging
//! `tracing` events carry operation names and counts only, never typed text or keys (spec §54).

mod keys;
mod plan;
mod sys;

use std::collections::HashSet;
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use winwright_contracts::backend::{BackendFuture, OperationContext};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::input::{InputBackend, Key, MouseButton};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::keys::Layout;
use crate::plan::{Held, RawInput};

/// Pause between positioning the pointer and pressing, so the target sees the hover first.
const SETTLE: Duration = Duration::from_millis(15);
/// How long a chord stays down, so apps that poll key state still observe it.
const CHORD_HOLD: Duration = Duration::from_millis(20);
/// Pause between `type_text` batches, letting the target drain its input queue.
const TEXT_BATCH_GAP: Duration = Duration::from_millis(5);

/// [`InputBackend`] over Win32 `SendInput`. See the crate docs for the DPI-awareness requirement.
pub struct SendInputBackend {
    engine: Engine<sys::Win32>,
}

impl SendInputBackend {
    pub fn new() -> Self {
        Self {
            engine: Engine::new(sys::Win32),
        }
    }
}

impl Default for SendInputBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SendInputBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendInputBackend")
            .field("held", &self.engine.state().held.len())
            .finish_non_exhaustive()
    }
}

impl InputBackend for SendInputBackend {
    fn move_to<'a>(
        &'a self,
        point: PhysicalPoint,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()> {
        Box::pin(self.engine.move_to(point, ctx))
    }

    fn click<'a>(
        &'a self,
        point: PhysicalPoint,
        button: MouseButton,
        count: u32,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()> {
        Box::pin(self.engine.click(point, button, count, ctx))
    }

    fn drag<'a>(
        &'a self,
        from: PhysicalPoint,
        to: PhysicalPoint,
        button: MouseButton,
        duration: Duration,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()> {
        Box::pin(self.engine.drag(from, to, button, duration, ctx))
    }

    fn scroll<'a>(
        &'a self,
        point: PhysicalPoint,
        notches_x: i32,
        notches_y: i32,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()> {
        Box::pin(self.engine.scroll(point, notches_x, notches_y, ctx))
    }

    fn type_text<'a>(&'a self, text: &'a str, ctx: &'a OperationContext) -> BackendFuture<'a, ()> {
        Box::pin(self.engine.type_text(text, ctx))
    }

    fn press_keys<'a>(
        &'a self,
        keys: &'a [Key],
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()> {
        Box::pin(self.engine.press_keys(keys, ctx))
    }

    fn release_all(&self) -> WinwrightResult<()> {
        self.engine.release_all()
    }
}

/// The desktop as the engine sees it; [`sys::Win32`] in production, a recorder in tests.
trait Platform: Layout + Send + Sync {
    /// `BACKEND_UNAVAILABLE` when input cannot reach the user's desktop right now.
    fn check_input_desktop(&self) -> WinwrightResult<()>;
    fn virtual_screen(&self) -> WinwrightResult<PhysicalRect>;
    /// Injects `events` in order, reporting how many were inserted when not all were.
    fn send(&self, events: &[RawInput]) -> Result<(), Shortfall>;
}

/// `SendInput` inserted only the first `inserted` events.
struct Shortfall {
    inserted: usize,
    last_error: u32,
}

#[derive(Default)]
struct HeldState {
    held: HashSet<Held>,
    /// Bumped by `release_all`; sequences started under an older value stop injecting.
    stop_epoch: u64,
}

struct Engine<P> {
    platform: P,
    /// Guards `held` and every `SendInput` call, so a release can never interleave with a send.
    /// Never held across an await.
    state: Mutex<HeldState>,
    /// Serializes operations. `release_all` deliberately does not take it.
    turn: tokio::sync::Mutex<()>,
}

impl<P: Platform> Engine<P> {
    fn new(platform: P) -> Self {
        Self {
            platform,
            state: Mutex::new(HeldState::default()),
            turn: tokio::sync::Mutex::new(()),
        }
    }

    /// Poison-tolerant: the held set is always consistent between statements, and the release
    /// path must work even after a panic elsewhere.
    fn state(&self) -> MutexGuard<'_, HeldState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sends one batch and records the transitions of the events actually inserted, in the global
    /// held set and in the calling sequence's `pressed` set.
    fn send_locked(
        &self,
        state: &mut HeldState,
        events: &[RawInput],
        pressed: Option<&mut HashSet<Held>>,
    ) -> WinwrightResult<()> {
        if events.is_empty() {
            return Ok(());
        }
        let result = self.platform.send(events);
        let inserted = match &result {
            Ok(()) => events.len(),
            Err(shortfall) => shortfall.inserted.min(events.len()),
        };
        plan::track(&mut state.held, &events[..inserted]);
        if let Some(pressed) = pressed {
            plan::track(pressed, &events[..inserted]);
        }
        result.map_err(|shortfall| WinwrightError::InputFailed {
            reason: format!(
                "SendInput inserted {inserted} of {} events (GetLastError {}); input may be \
                 blocked by another thread or a desktop switch",
                events.len(),
                shortfall.last_error
            ),
        })
    }

    /// A sequence's own cleanup: releases what it pressed that is still held. Ignores the stop
    /// epoch, since releasing is always safe. Returns how many up events it sent.
    fn release_pressed(&self, pressed: &mut HashSet<Held>) -> WinwrightResult<usize> {
        let mut state = self.state();
        pressed.retain(|item| state.held.contains(item));
        let events = plan::plan_release(pressed.iter().copied(), &self.platform);
        self.send_locked(&mut state, &events, Some(pressed))?;
        Ok(events.len())
    }

    fn release_all(&self) -> WinwrightResult<()> {
        let mut state = self.state();
        state.stop_epoch = state.stop_epoch.wrapping_add(1);
        let events = plan::plan_release(state.held.iter().copied(), &self.platform);
        tracing::debug!(events = events.len(), "release_all");
        self.send_locked(&mut state, &events, None)
    }

    /// Waits for this backend's turn (cancellable), then checks the input desktop.
    async fn begin<'a>(
        &'a self,
        op: &'static str,
        ctx: &'a OperationContext,
    ) -> WinwrightResult<Sequence<'a, P>> {
        ctx.check(op)?;
        let turn = tokio::select! {
            turn = self.turn.lock() => turn,
            () = ctx.cancel.cancelled() => return Err(WinwrightError::Cancelled),
            () = tokio::time::sleep(ctx.remaining()) => {
                return Err(WinwrightError::Timeout {
                    operation: op.to_owned(),
                    elapsed_ms: u64::try_from(ctx.started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                });
            }
        };
        ctx.check(op)?;
        self.platform.check_input_desktop()?;
        let epoch = self.state().stop_epoch;
        Ok(Sequence {
            engine: self,
            ctx,
            op,
            epoch,
            pressed: HashSet::new(),
            _turn: turn,
        })
    }

    async fn move_to(&self, point: PhysicalPoint, ctx: &OperationContext) -> WinwrightResult<()> {
        let mut seq = self.begin("move_to", ctx).await?;
        let target = plan::plan_move(point, self.platform.virtual_screen()?)?;
        tracing::debug!("move_to");
        seq.send(&[target])
    }

    async fn click(
        &self,
        point: PhysicalPoint,
        button: MouseButton,
        count: u32,
        ctx: &OperationContext,
    ) -> WinwrightResult<()> {
        let presses = plan::plan_click(button, count)?;
        let mut seq = self.begin("click", ctx).await?;
        let target = plan::plan_move(point, self.platform.virtual_screen()?)?;
        tracing::debug!(?button, count, "click");
        seq.send(&[target])?;
        seq.pause(SETTLE).await?;
        seq.send(&presses)
    }

    async fn drag(
        &self,
        from: PhysicalPoint,
        to: PhysicalPoint,
        button: MouseButton,
        duration: Duration,
        ctx: &OperationContext,
    ) -> WinwrightResult<()> {
        let duration = plan::drag_duration(duration)?;
        let mut seq = self.begin("drag", ctx).await?;
        let virt = self.platform.virtual_screen()?;
        let start = plan::plan_move(from, virt)?;
        let (path, step) = plan::drag_path(from, to, duration);
        let moves = path
            .into_iter()
            .map(|p| plan::plan_move(p, virt))
            .collect::<WinwrightResult<Vec<_>>>()?;
        tracing::debug!(?button, moves = moves.len(), "drag");
        seq.send(&[start])?;
        seq.pause(SETTLE).await?;
        seq.send(&[RawInput::Button { button, down: true }])?;
        // From here every early return (cancel, deadline, release_all, send failure) drops `seq`,
        // which releases the button before the error reaches the caller.
        for next in moves {
            seq.pause(step).await?;
            seq.send(&[next])?;
        }
        seq.pause(SETTLE).await?;
        seq.send(&[RawInput::Button {
            button,
            down: false,
        }])
    }

    async fn scroll(
        &self,
        point: PhysicalPoint,
        notches_x: i32,
        notches_y: i32,
        ctx: &OperationContext,
    ) -> WinwrightResult<()> {
        let wheel = plan::plan_scroll(notches_x, notches_y);
        let mut seq = self.begin("scroll", ctx).await?;
        let target = plan::plan_move(point, self.platform.virtual_screen()?)?;
        tracing::debug!(events = wheel.len(), "scroll");
        seq.send(&[target])?;
        if wheel.is_empty() {
            return Ok(());
        }
        seq.pause(SETTLE).await?;
        seq.send(&wheel)
    }

    async fn type_text(&self, text: &str, ctx: &OperationContext) -> WinwrightResult<()> {
        let batches = plan::plan_text(text, &self.platform)?;
        let mut seq = self.begin("type_text", ctx).await?;
        tracing::debug!(
            chars = text.chars().count(),
            batches = batches.len(),
            "type_text"
        );
        for (i, batch) in batches.iter().enumerate() {
            if i > 0 {
                seq.pause(TEXT_BATCH_GAP).await?;
            }
            seq.send(batch)?;
        }
        Ok(())
    }

    async fn press_keys(&self, keys: &[Key], ctx: &OperationContext) -> WinwrightResult<()> {
        let chord = plan::plan_chord(keys, &self.platform)?;
        let mut seq = self.begin("press_keys", ctx).await?;
        tracing::debug!(keys = chord.press.len(), "press_keys");
        seq.send(&chord.press)?;
        // Not cancellable: once pressed, the release must follow.
        tokio::time::sleep(CHORD_HOLD).await;
        seq.send(&chord.release)
    }
}

/// One operation's exclusive turn. Remembers what it pressed and releases it on drop, so every
/// exit path, including a dropped future, leaves nothing held down.
struct Sequence<'a, P: Platform> {
    engine: &'a Engine<P>,
    ctx: &'a OperationContext,
    op: &'static str,
    epoch: u64,
    pressed: HashSet<Held>,
    _turn: tokio::sync::MutexGuard<'a, ()>,
}

impl<P: Platform> Sequence<'_, P> {
    /// Sends one batch unless `release_all` ran since this sequence began.
    fn send(&mut self, events: &[RawInput]) -> WinwrightResult<()> {
        let engine = self.engine;
        let mut state = engine.state();
        if state.stop_epoch != self.epoch {
            return Err(WinwrightError::Cancelled);
        }
        engine.send_locked(&mut state, events, Some(&mut self.pressed))
    }

    /// Sleeps up to `duration` (cut short by cancellation or the deadline), then checks `ctx`.
    async fn pause(&self, duration: Duration) -> WinwrightResult<()> {
        tokio::select! {
            () = tokio::time::sleep(duration.min(self.ctx.remaining())) => {}
            () = self.ctx.cancel.cancelled() => {}
        }
        self.ctx.check(self.op)
    }
}

impl<P: Platform> Drop for Sequence<'_, P> {
    fn drop(&mut self) {
        if self.pressed.is_empty() {
            return;
        }
        let engine = self.engine;
        match engine.release_pressed(&mut self.pressed) {
            Ok(0) => {}
            Ok(released) => tracing::debug!(
                op = self.op,
                released,
                "released input of an interrupted sequence"
            ),
            Err(err) => tracing::warn!(
                op = self.op,
                %err,
                "could not release input of an interrupted sequence"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Instant;

    use tokio_util::sync::CancellationToken;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_SHIFT};
    use winwright_contracts::ids::SessionId;

    use super::*;
    use crate::keys::UsLayout;

    /// Records every inserted batch instead of touching the desktop.
    struct Recorder {
        virt: PhysicalRect,
        locked: bool,
        /// Per-call insert limits, consumed front to back; empty means "insert everything".
        limits: Mutex<VecDeque<usize>>,
        /// Cancelled right after the first send, to interrupt a sequence between batches.
        cancel_after_send: Mutex<Option<CancellationToken>>,
        log: Mutex<Vec<Vec<RawInput>>>,
    }

    impl Default for Recorder {
        fn default() -> Self {
            Self {
                virt: PhysicalRect::new(-1920, -120, 1920, 1080),
                locked: false,
                limits: Mutex::default(),
                cancel_after_send: Mutex::default(),
                log: Mutex::default(),
            }
        }
    }

    impl Layout for Recorder {
        fn scan_code(&self, vk: u16) -> u16 {
            UsLayout.scan_code(vk)
        }
        fn vk_key_scan(&self, unit: u16) -> i16 {
            UsLayout.vk_key_scan(unit)
        }
    }

    impl Platform for Recorder {
        fn check_input_desktop(&self) -> WinwrightResult<()> {
            if self.locked {
                return Err(WinwrightError::BackendUnavailable {
                    backend: "input".into(),
                    reason: "locked".into(),
                });
            }
            Ok(())
        }

        fn virtual_screen(&self) -> WinwrightResult<PhysicalRect> {
            Ok(self.virt)
        }

        fn send(&self, events: &[RawInput]) -> Result<(), Shortfall> {
            let limit = self.limits.lock().unwrap().pop_front();
            let inserted = limit.map_or(events.len(), |n| n.min(events.len()));
            self.log.lock().unwrap().push(events[..inserted].to_vec());
            if let Some(token) = self.cancel_after_send.lock().unwrap().take() {
                token.cancel();
            }
            if inserted == events.len() {
                Ok(())
            } else {
                Err(Shortfall {
                    inserted,
                    last_error: 5,
                })
            }
        }
    }

    fn engine() -> Engine<Recorder> {
        Engine::new(Recorder::default())
    }

    fn ctx(timeout: Duration) -> OperationContext {
        OperationContext::new(
            SessionId::parse("test").unwrap(),
            timeout,
            CancellationToken::new(),
        )
    }

    fn long_ctx() -> OperationContext {
        ctx(Duration::from_secs(30))
    }

    fn batches(engine: &Engine<Recorder>) -> Vec<Vec<RawInput>> {
        engine.platform.log.lock().unwrap().clone()
    }

    fn events(engine: &Engine<Recorder>) -> Vec<RawInput> {
        batches(engine).concat()
    }

    fn held(engine: &Engine<Recorder>) -> HashSet<Held> {
        engine.state().held.clone()
    }

    fn pt(x: i32, y: i32) -> PhysicalPoint {
        PhysicalPoint { x, y }
    }

    const LEFT_DOWN: RawInput = RawInput::Button {
        button: MouseButton::Left,
        down: true,
    };
    const LEFT_UP: RawInput = RawInput::Button {
        button: MouseButton::Left,
        down: false,
    };

    fn button_ups(events: &[RawInput]) -> usize {
        events.iter().filter(|e| **e == LEFT_UP).count()
    }

    #[tokio::test]
    async fn waiting_for_a_turn_times_out_with_the_real_elapsed_time() {
        let engine = engine();
        let _busy = engine.turn.lock().await;
        let err = engine
            .click(
                pt(1, 1),
                MouseButton::Left,
                1,
                &ctx(Duration::from_millis(60)),
            )
            .await
            .unwrap_err();
        let WinwrightError::Timeout { elapsed_ms, .. } = err else {
            panic!("{err:?}");
        };
        assert!(elapsed_ms >= 50, "elapsed_ms={elapsed_ms}");
        assert!(events(&engine).is_empty());
    }

    #[tokio::test]
    async fn click_moves_then_sends_all_presses_in_one_batch() {
        let engine = engine();
        engine
            .click(pt(-5, 7), MouseButton::Left, 2, &long_ctx())
            .await
            .unwrap();
        let virt = engine.platform.virt;
        assert_eq!(
            batches(&engine),
            [
                vec![plan::plan_move(pt(-5, 7), virt).unwrap()],
                vec![LEFT_DOWN, LEFT_UP, LEFT_DOWN, LEFT_UP],
            ]
        );
        assert!(held(&engine).is_empty());
    }

    #[tokio::test]
    async fn validation_failures_send_nothing() {
        let engine = engine();
        let c = long_ctx();
        for count in [0, 4] {
            let err = engine
                .click(pt(0, 0), MouseButton::Left, count, &c)
                .await
                .unwrap_err();
            assert_eq!(err.code().as_str(), "INVALID_REQUEST");
        }
        let err = engine.move_to(pt(1920, 0), &c).await.unwrap_err();
        assert_eq!(err.code().as_str(), "INPUT_FAILED");
        let err = engine
            .drag(pt(0, 0), pt(5000, 0), MouseButton::Left, Duration::ZERO, &c)
            .await
            .unwrap_err();
        assert_eq!(err.code().as_str(), "INPUT_FAILED");
        let err = engine
            .press_keys(&[Key::Char('a'), Key::Ctrl], &c)
            .await
            .unwrap_err();
        assert_eq!(err.code().as_str(), "INVALID_REQUEST");
        assert!(events(&engine).is_empty());
    }

    #[tokio::test]
    async fn locked_desktop_is_backend_unavailable() {
        let engine = Engine::new(Recorder {
            locked: true,
            ..Recorder::default()
        });
        let err = engine.move_to(pt(0, 0), &long_ctx()).await.unwrap_err();
        assert_eq!(err.code().as_str(), "BACKEND_UNAVAILABLE");
        assert!(events(&engine).is_empty());
    }

    #[tokio::test]
    async fn expired_or_cancelled_context_sends_nothing() {
        let engine = engine();
        let expired = ctx(Duration::ZERO);
        let err = engine.move_to(pt(0, 0), &expired).await.unwrap_err();
        assert_eq!(err.code().as_str(), "TIMEOUT");
        let cancelled = long_ctx();
        cancelled.cancel.cancel();
        let err = engine.type_text("hello", &cancelled).await.unwrap_err();
        assert_eq!(err.code().as_str(), "CANCELLED");
        assert!(events(&engine).is_empty());
    }

    #[tokio::test]
    async fn drag_presses_interpolates_and_releases_at_target() {
        let engine = engine();
        let (from, to) = (pt(-100, 0), pt(100, 50));
        engine
            .drag(from, to, MouseButton::Left, Duration::ZERO, &long_ctx())
            .await
            .unwrap();
        let virt = engine.platform.virt;
        let log = events(&engine);
        assert_eq!(log[0], plan::plan_move(from, virt).unwrap());
        assert_eq!(log[1], LEFT_DOWN);
        assert_eq!(*log.last().unwrap(), LEFT_UP);
        let moves = &log[2..log.len() - 1];
        assert!(
            moves.len() >= 9,
            "at least 8 intermediate moves plus the target"
        );
        assert!(moves.iter().all(|e| matches!(e, RawInput::Move { .. })));
        assert_eq!(*moves.last().unwrap(), plan::plan_move(to, virt).unwrap());
        assert!(held(&engine).is_empty());
    }

    #[tokio::test]
    async fn cancelled_drag_releases_the_button_before_returning() {
        let engine = engine();
        let c = long_ctx();
        let started = Instant::now();
        let (result, ()) = tokio::join!(
            engine.drag(
                pt(0, 0),
                pt(500, 500),
                MouseButton::Left,
                Duration::from_secs(2),
                &c
            ),
            async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                c.cancel.cancel();
            }
        );
        assert_eq!(result.unwrap_err().code().as_str(), "CANCELLED");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancel is prompt"
        );
        let log = events(&engine);
        assert_eq!(*log.last().unwrap(), LEFT_UP);
        assert_eq!(button_ups(&log), 1);
        assert!(held(&engine).is_empty());
    }

    #[tokio::test]
    async fn drag_past_its_deadline_releases_the_button() {
        let engine = engine();
        let err = engine
            .drag(
                pt(0, 0),
                pt(500, 500),
                MouseButton::Left,
                Duration::from_secs(2),
                &ctx(Duration::from_millis(150)),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code().as_str(), "TIMEOUT");
        let log = events(&engine);
        assert!(log.contains(&LEFT_DOWN));
        assert_eq!(*log.last().unwrap(), LEFT_UP);
        assert!(held(&engine).is_empty());
    }

    #[tokio::test]
    async fn dropped_drag_future_releases_the_button() {
        let engine = engine();
        let c = long_ctx();
        let outcome = tokio::time::timeout(
            Duration::from_millis(80),
            engine.drag(
                pt(0, 0),
                pt(500, 500),
                MouseButton::Left,
                Duration::from_secs(2),
                &c,
            ),
        )
        .await;
        assert!(outcome.is_err(), "the outer timeout dropped the future");
        let log = events(&engine);
        assert!(log.contains(&LEFT_DOWN));
        assert_eq!(*log.last().unwrap(), LEFT_UP);
        assert!(held(&engine).is_empty());
    }

    #[tokio::test]
    async fn release_all_stops_an_in_flight_sequence() {
        let engine = engine();
        let c = long_ctx();
        let (result, released) = tokio::join!(
            engine.drag(
                pt(0, 0),
                pt(500, 500),
                MouseButton::Left,
                Duration::from_secs(2),
                &c
            ),
            async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                engine.release_all()
            }
        );
        released.unwrap();
        assert_eq!(result.unwrap_err().code().as_str(), "CANCELLED");
        let log = events(&engine);
        assert_eq!(
            *log.last().unwrap(),
            LEFT_UP,
            "nothing injected after release"
        );
        assert_eq!(button_ups(&log), 1);
        assert!(held(&engine).is_empty());
        // The stop only affects sequences that were already running.
        engine.move_to(pt(0, 0), &c).await.unwrap();
    }

    #[tokio::test]
    async fn partial_insert_is_input_failed_and_releases_what_went_down() {
        let engine = engine();
        engine.platform.limits.lock().unwrap().push_back(1);
        let err = engine
            .press_keys(&[Key::Ctrl, Key::Shift, Key::Char('s')], &long_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code().as_str(), "INPUT_FAILED");
        assert!(err.to_string().contains("inserted 1 of 3"), "{err}");
        assert_eq!(
            batches(&engine),
            [
                vec![RawInput::key(VK_CONTROL.0, true, &UsLayout)],
                vec![RawInput::key(VK_CONTROL.0, false, &UsLayout)],
            ]
        );
        assert!(held(&engine).is_empty());
    }

    #[tokio::test]
    async fn chord_presses_then_releases_in_reverse() {
        let engine = engine();
        engine
            .press_keys(&[Key::Ctrl, Key::Char('+')], &long_ctx())
            .await
            .unwrap();
        let k = |vk: u16, down| RawInput::key(vk, down, &UsLayout);
        assert_eq!(
            batches(&engine),
            [
                vec![k(VK_CONTROL.0, true), k(VK_SHIFT.0, true), k(0xBB, true)],
                vec![k(0xBB, false), k(VK_SHIFT.0, false), k(VK_CONTROL.0, false)],
            ]
        );
        assert!(held(&engine).is_empty());
    }

    #[tokio::test]
    async fn type_text_checks_cancellation_between_batches() {
        let engine = engine();
        let c = long_ctx();
        *engine.platform.cancel_after_send.lock().unwrap() = Some(c.cancel.clone());
        let err = engine.type_text(&"x".repeat(100), &c).await.unwrap_err();
        assert_eq!(err.code().as_str(), "CANCELLED");
        let sent = batches(&engine);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].len(), 64, "one full batch of 32 characters");
        assert!(held(&engine).is_empty());
    }

    #[tokio::test]
    async fn type_text_sends_every_batch() {
        let engine = engine();
        engine
            .type_text(&"y".repeat(70), &long_ctx())
            .await
            .unwrap();
        let lens: Vec<usize> = batches(&engine).iter().map(Vec::len).collect();
        assert_eq!(lens, [64, 64, 12]);
    }

    #[tokio::test]
    async fn scroll_moves_then_wheels() {
        let engine = engine();
        let c = long_ctx();
        engine.scroll(pt(10, 10), 1, 3, &c).await.unwrap();
        let virt = engine.platform.virt;
        assert_eq!(
            batches(&engine),
            [
                vec![plan::plan_move(pt(10, 10), virt).unwrap()],
                vec![
                    RawInput::Wheel {
                        horizontal: false,
                        delta: -360
                    },
                    RawInput::Wheel {
                        horizontal: true,
                        delta: 120
                    },
                ],
            ]
        );
        engine.scroll(pt(10, 10), 0, 0, &c).await.unwrap();
        assert_eq!(batches(&engine).len(), 3, "zero notches only moves");
    }

    #[test]
    fn release_all_releases_everything_in_order_even_after_poisoning() {
        let engine = engine();
        engine.state().held.extend([
            Held::Button(MouseButton::Left),
            Held::Key(VK_CONTROL.0),
            Held::Key(u16::from(b'A')),
        ]);
        std::thread::scope(|s| {
            let poisoner = s.spawn(|| {
                let _guard = engine.state.lock().unwrap();
                panic!("poison the held-state mutex");
            });
            assert!(poisoner.join().is_err());
        });
        assert!(engine.state.is_poisoned());
        engine.release_all().unwrap();
        assert_eq!(
            batches(&engine),
            [vec![
                RawInput::key(u16::from(b'A'), false, &UsLayout),
                RawInput::key(VK_CONTROL.0, false, &UsLayout),
                LEFT_UP,
            ]]
        );
        assert!(held(&engine).is_empty());
        assert_eq!(engine.state().stop_epoch, 1);
        // Nothing held: nothing sent.
        engine.release_all().unwrap();
        assert_eq!(batches(&engine).len(), 1);
    }

    #[test]
    fn failed_release_all_keeps_unreleased_entries_for_a_retry() {
        let engine = engine();
        engine
            .state()
            .held
            .extend([Held::Key(VK_SHIFT.0), Held::Button(MouseButton::Right)]);
        engine.platform.limits.lock().unwrap().push_back(1);
        let err = engine.release_all().unwrap_err();
        assert_eq!(err.code().as_str(), "INPUT_FAILED");
        assert_eq!(
            held(&engine),
            HashSet::from([Held::Button(MouseButton::Right)])
        );
        engine.release_all().unwrap();
        assert!(held(&engine).is_empty());
    }
}
