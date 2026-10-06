//! Teaching (Phase 22): pointing at spots given by pixels, and guides that point at each step
//! in turn and wait for the person to do it. Nothing here clicks or types for them.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use winwright_contracts::backend::{InspectTarget, OperationContext};
use winwright_contracts::capture::{CaptureRequest, CaptureTarget, ImageFormat};
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::overlay::{
    GuideClick, GuideOutcome, GuideRequest, GuideResult, GuideTarget, GuideWait, HighlightResult,
    OverlayId, OverlayRequest, OverlayService, PointerEvent, ScreenSpot, SpotHighlightRequest,
};
use winwright_contracts::security::{ActionRisk, Capability};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::engine::Engine;
use crate::find::Resolved;
use crate::services::{DEFAULT_HIGHLIGHT_MS, DEFAULT_OVERLAY_COLOR, proposed, unavailable};
use crate::session::Session;

const MAX_STEPS: usize = 20;
/// Overlays take 120 characters, "Not there: " included.
const MAX_CAPTION_CHARS: usize = 100;
const MAX_GUIDE_MS: u64 = 300_000;
const MAX_SPOT_SIDE: u32 = 4_000;
/// A release further than this from its press makes the click a drag.
const DRAG_PIXELS: i32 = 6;
/// A change step captures its area once the pointer has glided in and settled, then this often.
const CHANGE_SETTLE: Duration = Duration::from_millis(600);
const CHANGE_POLL: Duration = Duration::from_millis(400);
/// Clicks elsewhere a step takes before the guide stops for help.
const MAX_MISSES: u32 = 3;
/// After that the step stays this long, so the person sees where it was.
const MISSED_MS: u64 = 8_000;
/// A step not clicked for this long is shown again: the pointer glides in once more. The
/// same span as [`MISSED_MS`]: long enough to look around, short enough to still be waiting.
const REPLAY_AFTER: Duration = Duration::from_millis(MISSED_MS);
/// The pointer after a click elsewhere.
const MISS_COLOR: u32 = 0x00E5_484D;

/// `width` x `height` centered on `center`.
fn centered(center: PhysicalPoint, width: u32, height: u32) -> PhysicalRect {
    let w = width.clamp(1, MAX_SPOT_SIDE) as i32;
    let h = height.clamp(1, MAX_SPOT_SIDE) as i32;
    let (left, top) = (
        center.x.saturating_sub(w / 2),
        center.y.saturating_sub(h / 2),
    );
    PhysicalRect::new(left, top, left.saturating_add(w), top.saturating_add(h))
}

/// The step's caption after a click elsewhere.
fn not_there(caption: &str) -> String {
    format!("Not there: {caption}")
}

/// Where a press was released, when that makes it a drag.
fn dragged(press: PhysicalPoint, release: PhysicalPoint) -> Option<PhysicalPoint> {
    ((press.x - release.x).abs() > DRAG_PIXELS || (press.y - release.y).abs() > DRAG_PIXELS)
        .then_some(release)
}

fn validate(request: &GuideRequest) -> WinwrightResult<()> {
    if request.steps.is_empty() || request.steps.len() > MAX_STEPS {
        return Err(WinwrightError::invalid(format!(
            "a guide takes 1 to {MAX_STEPS} steps"
        )));
    }
    if request.timeout_ms == 0 {
        return Err(WinwrightError::invalid("timeoutMs must be positive"));
    }
    for (i, step) in request.steps.iter().enumerate() {
        let chars = step.caption.trim().chars().count();
        if chars == 0 || chars > MAX_CAPTION_CHARS {
            return Err(WinwrightError::invalid(format!(
                "step {}: the caption needs 1 to {MAX_CAPTION_CHARS} characters",
                i + 1
            )));
        }
    }
    Ok(())
}

/// The next press or release of the person's mouse; `None` once the guide's time is up.
async fn next_event(
    events: &mut UnboundedReceiver<PointerEvent>,
    ctx: &OperationContext,
) -> WinwrightResult<Option<PointerEvent>> {
    tokio::select! {
        event = events.recv() => Ok(event),
        () = ctx.cancel.cancelled() => Err(WinwrightError::Cancelled),
        () = tokio::time::sleep_until(ctx.deadline.into()) => Ok(None),
    }
}

/// Waits `pause`; false when the guide's time runs out first.
async fn wait(pause: Duration, ctx: &OperationContext) -> WinwrightResult<bool> {
    tokio::select! {
        () = tokio::time::sleep(pause) => Ok(true),
        () = ctx.cancel.cancelled() => Err(WinwrightError::Cancelled),
        () = tokio::time::sleep_until(ctx.deadline.into()) => Ok(false),
    }
}

/// What the person did while a step waited.
#[derive(Debug)]
enum Press<T> {
    Clicked(T),
    /// No press within the idle time given.
    Idle,
    /// The guide's time ran out.
    TimeUp,
}

/// The person's next press and, once it is released, where (when they dragged). With `idle`,
/// gives up after that long without a press. A press still held when time runs out answers
/// on its own.
async fn next_press(
    events: &mut UnboundedReceiver<PointerEvent>,
    idle: Option<Duration>,
    ctx: &OperationContext,
) -> WinwrightResult<Press<(PointerEvent, Option<PhysicalPoint>)>> {
    let idle_at = idle.map(|d| std::time::Instant::now() + d);
    let press = loop {
        let event = tokio::select! {
            event = next_event(events, ctx) => event?,
            () = tokio::time::sleep_until(idle_at.unwrap_or(ctx.deadline).into()),
                if idle_at.is_some() => return Ok(Press::Idle),
        };
        match event {
            None => return Ok(Press::TimeUp),
            Some(event) if event.down => break event,
            // The release of a press made before this step.
            Some(_) => {}
        }
    };
    while let Some(event) = next_event(events, ctx).await? {
        if !event.down && event.button == press.button {
            return Ok(Press::Clicked((press, dragged(press.point, event.point))));
        }
    }
    Ok(Press::Clicked((press, None)))
}

/// What a click means for the step on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Done,
    /// Marked "Not there"; the step keeps waiting.
    Missed,
    /// One miss too many: the guide stops for help.
    GiveUp,
}

fn verdict(inside: bool, misses: &mut u32) -> Verdict {
    if inside {
        return Verdict::Done;
    }
    *misses += 1;
    if *misses >= MAX_MISSES {
        Verdict::GiveUp
    } else {
        Verdict::Missed
    }
}

/// Decides when a keyboard step's area has changed. Frames seen before the person touches
/// anything are its normal life (a blinking caret, a looping animation) and never count; after
/// their input, a frame not seen before counts once it shows on two captures running.
struct ChangeWatch {
    normal: HashSet<u64>,
    input_at_start: Option<u64>,
    pending: bool,
}

impl ChangeWatch {
    fn new(first: u64, input_at_start: Option<u64>) -> Self {
        Self {
            normal: HashSet::from([first]),
            input_at_start,
            pending: false,
        }
    }

    /// Whether the step is done after this frame. `input` is the person's last input time,
    /// `None` when unknown (then any new frame may count).
    fn see(&mut self, frame: u64, input: Option<u64>) -> bool {
        if input.is_some() && input == self.input_at_start {
            self.normal.insert(frame);
            self.pending = false;
            return false;
        }
        if self.normal.contains(&frame) {
            self.pending = false;
            return false;
        }
        if self.pending {
            return true;
        }
        self.pending = true;
        false
    }
}

fn frame_hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// A step on screen: its area, and the origin its pixels are counted from.
struct Shown {
    rect: PhysicalRect,
    origin: PhysicalPoint,
    window: Option<u64>,
}

impl Shown {
    fn local(&self, p: PhysicalPoint) -> PhysicalPoint {
        PhysicalPoint {
            x: p.x - self.origin.x,
            y: p.y - self.origin.y,
        }
    }
}

/// Removes a step's overlay when dropped, however the guide ends.
pub(crate) struct Drawn<'a> {
    pub(crate) overlay: &'a dyn OverlayService,
    pub(crate) id: OverlayId,
}

impl Drop for Drawn<'_> {
    fn drop(&mut self) {
        let _ = self.overlay.clear(Some(self.id));
    }
}

/// Stops the voice when dropped, however the guide ends.
struct Hush<'a>(&'a dyn OverlayService);

impl Drop for Hush<'_> {
    fn drop(&mut self) {
        self.0.hush();
    }
}

impl Engine {
    /// A spot's screen area, the origin of its pixels, and the element under its center (with
    /// a ref). Winwright's own windows are refused.
    async fn spot(
        &self,
        session: &Session,
        spot: &ScreenSpot,
        ctx: &OperationContext,
    ) -> WinwrightResult<(PhysicalRect, PhysicalPoint, Resolved)> {
        let center = self.screen_point(&spot.at)?;
        let hit = self.uia.inspect(InspectTarget::Point(center), ctx).await?;
        let (reference, window) = self.remember(session, &hit, None).await;
        self.guard_self(hit.props.process_id, &hit.props.label())?;
        let origin = PhysicalPoint {
            x: center.x - spot.at.x,
            y: center.y - spot.at.y,
        };
        let hit = Resolved {
            reference,
            key: hit.key,
            props: hit.props,
            window,
        };
        Ok((centered(center, spot.width, spot.height), origin, hit))
    }

    /// Points at a spot given by pixels (spec §21 for what has no UI element).
    pub async fn highlight_spot(
        &self,
        session: &Session,
        request: SpotHighlightRequest,
    ) -> WinwrightResult<HighlightResult> {
        self.observe("overlay_highlight")?;
        if request.duration_ms == Some(0) {
            return Err(WinwrightError::invalid("durationMs must be positive"));
        }
        self.ensure_no_confirmation_open()?;
        let ctx = session.operation(self.timeout())?;
        let (rect, _, hit) = self.spot(session, &request.spot, &ctx).await?;
        self.ensure_no_confirmation_open()?;
        let id = self.overlay_service()?.show(OverlayRequest {
            rect,
            style: request.style,
            label: request.label,
            step: None,
            steps: None,
            color: request.color.unwrap_or(DEFAULT_OVERLAY_COLOR),
            duration_ms: Some(request.duration_ms.unwrap_or(DEFAULT_HIGHLIGHT_MS)),
        })?;
        Ok(HighlightResult {
            overlay: id,
            reference: hit.reference.clone(),
            target: format!(
                "{} at {},{}",
                hit.label(),
                request.spot.at.x,
                request.spot.at.y
            ),
            rect,
        })
    }

    /// Teaches: points at each step in turn and waits until the person clicks inside it (or,
    /// for a keyboard step, until its pixels change). A click elsewhere marks the step "Not
    /// there" and keeps waiting; the third one stops the guide so the model can help.
    pub async fn guide(
        &self,
        session: &Session,
        request: GuideRequest,
    ) -> WinwrightResult<GuideResult> {
        self.observe("desktop_guide")?;
        validate(&request)?;
        if request.steps.iter().any(|s| s.wait == GuideWait::Change) {
            self.authorize(proposed(
                "desktop_guide",
                Capability::Capture,
                ActionRisk::ReadOnly,
                None,
            ))?;
        }
        let overlay = self.overlay_service()?;
        // A step left on screen by the previous guide.
        let _ = overlay.clear(None);
        let ctx = session.operation(Duration::from_millis(request.timeout_ms.min(MAX_GUIDE_MS)))?;
        let (tx, mut events) = unbounded_channel();
        let _watch = overlay.watch_pointer(Box::new(move |event| {
            let _ = tx.send(event);
        }))?;
        let total = request.steps.len();
        let mut result = GuideResult {
            completed: 0,
            steps: total as u32,
            outcome: GuideOutcome::Done,
            clicks: Vec::new(),
            window: None,
            warnings: Vec::new(),
        };
        let color = request.color.unwrap_or(DEFAULT_OVERLAY_COLOR);
        let mut speaking = request.speak;
        let _hush = speaking.then_some(Hush(overlay));
        let mut missed = None;
        'steps: for (i, step) in request.steps.iter().enumerate() {
            self.ensure_no_confirmation_open()?;
            let shown = self.guide_target(session, &step.target, &ctx).await?;
            result.window = shown.window.or(result.window);
            self.ensure_no_confirmation_open()?;
            let caption = step.caption.trim();
            let number = (total > 1).then_some(i as u32 + 1);
            let show = |label: String, color: u32| {
                overlay.show(OverlayRequest {
                    rect: shown.rect,
                    style: request.style,
                    label: Some(label),
                    step: number,
                    steps: number.map(|_| total as u32),
                    color,
                    duration_ms: Some((ctx.remaining().as_millis() as u64).max(1)),
                })
            };
            let mut drawn = Drawn {
                overlay,
                id: show(caption.to_owned(), color)?,
            };
            if speaking && let Err(err) = overlay.speak(caption) {
                result
                    .warnings
                    .push(format!("captions are not read aloud: {err}"));
                speaking = false;
            }
            match step.wait {
                GuideWait::Click => {
                    let mut misses = 0;
                    // "Show me again": once per stretch without a click.
                    let mut replayed = false;
                    let mut marked = false;
                    loop {
                        let idle = (!replayed).then_some(REPLAY_AFTER);
                        let click = match self
                            .next_click(session, &mut events, &shown, i + 1, idle, &ctx)
                            .await?
                        {
                            Press::Clicked(click) => click,
                            Press::Idle => {
                                // Shown anew, the pointer glides in again from the cursor.
                                let (label, color) = if marked {
                                    (not_there(caption), MISS_COLOR)
                                } else {
                                    (caption.to_owned(), color)
                                };
                                let old = drawn.id;
                                drawn.id = show(label, color)?;
                                let _ = overlay.clear(Some(old));
                                replayed = true;
                                continue;
                            }
                            Press::TimeUp => {
                                result.outcome = GuideOutcome::TimedOut;
                                break 'steps;
                            }
                        };
                        replayed = false;
                        let inside = click.inside;
                        result.clicks.push(click);
                        match verdict(inside, &mut misses) {
                            Verdict::Done => break,
                            Verdict::GiveUp => {
                                result.outcome = GuideOutcome::ClickedElsewhere;
                                missed = Some((shown.rect, number, caption));
                                break 'steps;
                            }
                            Verdict::Missed => {
                                // The same step, marked, waits for the right click.
                                let _ = overlay.clear(Some(drawn.id));
                                drawn.id = show(not_there(caption), MISS_COLOR)?;
                                marked = true;
                            }
                        }
                    }
                }
                GuideWait::Change => {
                    if !self.next_change(shown.rect, &ctx).await? {
                        result.outcome = GuideOutcome::TimedOut;
                        break;
                    }
                    // Clicks made while a key was awaited answer no step.
                    while events.try_recv().is_ok() {}
                }
            }
            result.completed += 1;
        }
        if let Some((rect, step, caption)) = missed {
            let _ = overlay.show(OverlayRequest {
                rect,
                style: request.style,
                label: Some(not_there(caption)),
                step,
                steps: step.map(|_| total as u32),
                color: MISS_COLOR,
                duration_ms: Some(MISSED_MS),
            });
        }
        Ok(result)
    }

    async fn guide_target(
        &self,
        session: &Session,
        target: &GuideTarget,
        ctx: &OperationContext,
    ) -> WinwrightResult<Shown> {
        match target {
            GuideTarget::Element(target) => {
                let r = self.resolve_target(session, target, ctx).await?;
                self.guard_self(r.props.process_id, &r.label())?;
                let rect = r
                    .props
                    .bounds
                    .filter(|_| !r.props.offscreen)
                    .ok_or_else(|| {
                        WinwrightError::invalid(format!("{} is not on screen", r.label()))
                    })?;
                Ok(Shown {
                    rect,
                    origin: PhysicalPoint { x: 0, y: 0 },
                    window: r.window,
                })
            }
            GuideTarget::Spot(spot) => {
                let (rect, origin, hit) = self.spot(session, spot, ctx).await?;
                Ok(Shown {
                    rect,
                    origin,
                    window: hit.window,
                })
            }
        }
    }

    /// The person's next press and where it was released, as [`next_press`] waits for it.
    async fn next_click(
        &self,
        session: &Session,
        events: &mut UnboundedReceiver<PointerEvent>,
        shown: &Shown,
        step: usize,
        idle: Option<Duration>,
        ctx: &OperationContext,
    ) -> WinwrightResult<Press<GuideClick>> {
        let (press, drag_to) = match next_press(events, idle, ctx).await? {
            Press::Clicked(click) => click,
            Press::Idle => return Ok(Press::Idle),
            Press::TimeUp => return Ok(Press::TimeUp),
        };
        let at = shown.local(press.point);
        Ok(Press::Clicked(GuideClick {
            step: step as u32,
            button: press.button,
            x: at.x,
            y: at.y,
            drag_to: drag_to.map(|p| shown.local(p)),
            inside: shown.rect.contains(press.point),
            element: self.label_at(session, press.point).await,
        }))
    }

    async fn label_at(&self, session: &Session, point: PhysicalPoint) -> String {
        let Ok(ctx) = session.operation(self.timeout()) else {
            return "unknown".into();
        };
        match self.uia.inspect(InspectTarget::Point(point), &ctx).await {
            Ok(hit) => {
                let label = hit.props.label();
                self.release(vec![hit.key]).await;
                label
            }
            Err(_) => "unknown".into(),
        }
    }

    /// Whether the pixels of `rect` change, as [`ChangeWatch`] judges it, before the guide's
    /// time is up.
    async fn next_change(
        &self,
        rect: PhysicalRect,
        ctx: &OperationContext,
    ) -> WinwrightResult<bool> {
        let capture = self
            .capture
            .as_deref()
            .ok_or_else(|| unavailable("capture"))?;
        let shot = || {
            capture.capture(
                CaptureRequest {
                    fit: None,
                    marks: Vec::new(),
                    target: CaptureTarget::Region(rect),
                    format: ImageFormat::Png,
                    quality: 100,
                },
                ctx,
            )
        };
        if !wait(CHANGE_SETTLE, ctx).await? {
            return Ok(false);
        }
        let input_at_start = self.windows.last_input_ms();
        let mut watch = ChangeWatch::new(frame_hash(&shot().await?.bytes), input_at_start);
        loop {
            if !wait(CHANGE_POLL, ctx).await? {
                return Ok(false);
            }
            match shot().await {
                Ok(now) => {
                    if watch.see(frame_hash(&now.bytes), self.windows.last_input_ms()) {
                        return Ok(true);
                    }
                }
                Err(WinwrightError::Timeout { .. }) => return Ok(false),
                Err(other) => return Err(other),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use winwright_contracts::action::ScreenPoint;
    use winwright_contracts::overlay::{GuideStep, OverlayStyle};

    use super::*;

    fn step(caption: &str) -> GuideStep {
        GuideStep {
            target: GuideTarget::Spot(ScreenSpot {
                at: ScreenPoint {
                    x: 10,
                    y: 10,
                    window: None,
                },
                width: 48,
                height: 48,
            }),
            caption: caption.into(),
            wait: GuideWait::Click,
        }
    }

    fn request(steps: Vec<GuideStep>) -> GuideRequest {
        GuideRequest {
            steps,
            style: OverlayStyle::Pointer,
            color: None,
            timeout_ms: 1_000,
            speak: false,
        }
    }

    #[test]
    fn spots_are_centered_and_kept_sane() {
        let c = PhysicalPoint { x: 100, y: 50 };
        assert_eq!(centered(c, 48, 20), PhysicalRect::new(76, 40, 124, 60));
        assert_eq!(centered(c, 0, 0), PhysicalRect::new(100, 50, 101, 51));
        assert_eq!(centered(c, u32::MAX, 1).width(), 4_000);
    }

    #[test]
    fn misses_are_marked_and_drags_need_a_real_move() {
        assert_eq!(not_there("Click +"), "Not there: Click +");
        let longest = not_there(&"x".repeat(MAX_CAPTION_CHARS));
        assert!(longest.chars().count() <= 120);
        let press = PhysicalPoint { x: 100, y: 100 };
        assert_eq!(dragged(press, PhysicalPoint { x: 104, y: 97 }), None);
        let far = PhysicalPoint { x: 160, y: 100 };
        assert_eq!(dragged(press, far), Some(far));
    }

    fn press(x: i32, down: bool) -> PointerEvent {
        PointerEvent {
            point: PhysicalPoint { x, y: 0 },
            button: winwright_contracts::input::MouseButton::Left,
            down,
        }
    }

    fn ctx(ms: u64) -> OperationContext {
        OperationContext::new(
            winwright_contracts::ids::SessionId::parse("guide-test").unwrap(),
            Duration::from_millis(ms),
            tokio_util::sync::CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn presses_pair_with_their_release_and_stale_releases_are_skipped() {
        let (tx, mut rx) = unbounded_channel();
        // A release left over from the step before, then a click, then a drag.
        for e in [press(5, false), press(10, true), press(12, false)] {
            tx.send(e).unwrap();
        }
        tx.send(press(100, true)).unwrap();
        tx.send(press(300, false)).unwrap();
        let ctx = ctx(1_000);
        let Press::Clicked((p, drag)) = next_press(&mut rx, None, &ctx).await.unwrap() else {
            panic!("no click");
        };
        assert_eq!((p.point.x, drag), (10, None));
        let Press::Clicked((p, drag)) = next_press(&mut rx, None, &ctx).await.unwrap() else {
            panic!("no drag");
        };
        assert_eq!((p.point.x, drag.map(|d| d.x)), (100, Some(300)));
        // Nothing more: time runs out.
        let left = next_press(&mut rx, None, &ctx).await.unwrap();
        assert!(matches!(left, Press::TimeUp));
    }

    #[tokio::test]
    async fn a_press_still_held_when_time_runs_out_answers_alone_and_a_stop_cancels() {
        let (tx, mut rx) = unbounded_channel();
        tx.send(press(7, true)).unwrap();
        let Press::Clicked((p, drag)) = next_press(&mut rx, None, &ctx(200)).await.unwrap() else {
            panic!("no click");
        };
        assert_eq!((p.point.x, drag), (7, None));
        let stopped = ctx(5_000);
        stopped.cancel.cancel();
        let err = next_press(&mut rx, None, &stopped).await.unwrap_err();
        assert!(matches!(err, WinwrightError::Cancelled));
    }

    #[tokio::test]
    async fn a_step_left_alone_goes_idle_but_a_held_press_is_never_cut_short() {
        let (tx, mut rx) = unbounded_channel();
        let ctx = ctx(5_000);
        let quiet = Some(Duration::from_millis(50));
        assert!(matches!(
            next_press(&mut rx, quiet, &ctx).await.unwrap(),
            Press::Idle
        ));
        // A stale release does not count as a click, so the step still goes idle.
        tx.send(press(5, false)).unwrap();
        assert!(matches!(
            next_press(&mut rx, quiet, &ctx).await.unwrap(),
            Press::Idle
        ));
        // Once pressed, the release is awaited past the idle time.
        tx.send(press(9, true)).unwrap();
        let late = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            late.send(press(9, false)).unwrap();
        });
        let Press::Clicked((p, drag)) = next_press(&mut rx, quiet, &ctx).await.unwrap() else {
            panic!("the held press was dropped");
        };
        assert_eq!((p.point.x, drag), (9, None));
    }

    #[test]
    fn wrong_clicks_are_marked_until_the_third_stops_the_guide() {
        let mut misses = 0;
        assert_eq!(verdict(false, &mut misses), Verdict::Missed);
        assert_eq!(verdict(false, &mut misses), Verdict::Missed);
        assert_eq!(verdict(true, &mut misses), Verdict::Done);
        let mut misses = 0;
        assert_eq!(verdict(false, &mut misses), Verdict::Missed);
        assert_eq!(verdict(false, &mut misses), Verdict::Missed);
        assert_eq!(verdict(false, &mut misses), Verdict::GiveUp);
    }

    #[test]
    fn a_blinking_caret_never_counts_as_the_change() {
        // Caret on (1) and off (2) while the person does nothing: learned as normal.
        let mut w = ChangeWatch::new(1, Some(100));
        assert!(!w.see(2, Some(100)));
        assert!(!w.see(1, Some(100)));
        // They press a key (input time moves): the caret keeps blinking, nothing counts.
        assert!(!w.see(2, Some(900)));
        assert!(!w.see(1, Some(900)));
        // The text changes (3) and stays: counted on its second capture.
        assert!(!w.see(3, Some(900)));
        assert!(w.see(3, Some(900)));
    }

    #[test]
    fn a_one_frame_glitch_does_not_count_and_unknown_input_still_works() {
        let mut w = ChangeWatch::new(1, None);
        assert!(!w.see(4, None));
        assert!(!w.see(1, None));
        assert!(!w.see(5, None));
        assert!(w.see(6, None));
    }

    #[test]
    fn guides_need_steps_with_short_captions() {
        validate(&request(vec![step("Click Develop")])).unwrap();
        assert!(validate(&request(vec![])).is_err());
        assert!(validate(&request(vec![step("x"); MAX_STEPS + 1])).is_err());
        assert!(validate(&request(vec![step("  ")])).is_err());
        assert!(validate(&request(vec![step(&"é".repeat(MAX_CAPTION_CHARS + 1))])).is_err());
        let mut zero = request(vec![step("ok")]);
        zero.timeout_ms = 0;
        assert!(validate(&zero).is_err());
    }
}
