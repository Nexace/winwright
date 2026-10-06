//! The thin teal frame around the window the AI is acting in, so the person sees where it
//! works. It stays up across a quick run of actions and goes a moment after the last one.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::overlay::{OverlayId, OverlayRequest, OverlayService, OverlayStyle};

use crate::services::DEFAULT_OVERLAY_COLOR;

/// How long the frame stays after the last action.
const LINGER: Duration = Duration::from_millis(1_500);
/// Upper bound for one frame, should an action hang.
const MAX_SHOWN_MS: u64 = 120_000;
/// The highlight draws its border just outside the rect it is given; a maximized window's
/// edges are the screen's, where that border would be off-screen. Inset so it lies inside.
const INSET: i32 = 10;

/// `bounds` shrunk by [`INSET`] on every side (unchanged when too small for that).
fn inside(bounds: PhysicalRect) -> PhysicalRect {
    if bounds.width() <= 4 * INSET || bounds.height() <= 4 * INSET {
        return bounds;
    }
    PhysicalRect::new(
        bounds.left + INSET,
        bounds.top + INSET,
        bounds.right - INSET,
        bounds.bottom - INSET,
    )
}

#[derive(Default)]
pub(crate) struct Working {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    shown: Option<OverlayId>,
    /// Bumped by every action, so only the last one's linger hides the frame.
    generation: u64,
}

/// Held while an action runs; hides the frame [`LINGER`] after the last one ends.
pub(crate) struct Shown {
    working: Arc<Working>,
    overlay: Arc<dyn OverlayService>,
    generation: u64,
}

impl Working {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Frames `bounds`. The new frame is drawn before the old one goes, so nothing flickers.
    pub(crate) fn show(
        self: &Arc<Self>,
        overlay: &Arc<dyn OverlayService>,
        bounds: PhysicalRect,
    ) -> Shown {
        let id = overlay
            .show(OverlayRequest {
                rect: inside(bounds),
                style: OverlayStyle::Highlight,
                label: None,
                step: None,
                steps: None,
                color: DEFAULT_OVERLAY_COLOR,
                duration_ms: Some(MAX_SHOWN_MS),
            })
            .ok();
        let mut state = self.state();
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        let old = std::mem::replace(&mut state.shown, id);
        drop(state);
        if let Some(old) = old {
            let _ = overlay.clear(Some(old));
        }
        Shown {
            working: Arc::clone(self),
            overlay: Arc::clone(overlay),
            generation,
        }
    }

    /// Takes the frame down at once (before a screenshot, so it is not in the picture).
    pub(crate) fn hide_now(&self, overlay: &dyn OverlayService) {
        let shown = self.state().shown.take();
        if let Some(id) = shown {
            let _ = overlay.clear(Some(id));
        }
    }

    fn hide_if_last(&self, overlay: &dyn OverlayService, generation: u64) {
        let mut state = self.state();
        if state.generation != generation {
            return; // Another action came since: it keeps the frame.
        }
        if let Some(id) = state.shown.take() {
            drop(state);
            let _ = overlay.clear(Some(id));
        }
    }
}

impl Drop for Shown {
    fn drop(&mut self) {
        let (working, overlay, generation) = (
            Arc::clone(&self.working),
            Arc::clone(&self.overlay),
            self.generation,
        );
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    tokio::time::sleep(LINGER).await;
                    working.hide_if_last(overlay.as_ref(), generation);
                });
            }
            Err(_) => working.hide_if_last(overlay.as_ref(), generation),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::WinwrightResult;

    #[derive(Default)]
    struct Frames(Mutex<(u64, Vec<u64>)>);

    impl OverlayService for Frames {
        fn show(&self, _: OverlayRequest) -> WinwrightResult<OverlayId> {
            let mut f = self.0.lock().unwrap();
            f.0 += 1;
            let id = f.0;
            f.1.push(id);
            Ok(OverlayId(id))
        }

        fn clear(&self, id: Option<OverlayId>) -> WinwrightResult<()> {
            let mut f = self.0.lock().unwrap();
            f.1.retain(|&x| id.is_some_and(|id| id.0 != x));
            Ok(())
        }
    }

    fn up(frames: &Frames) -> Vec<u64> {
        frames.0.lock().unwrap().1.clone()
    }

    #[test]
    fn the_frame_sits_inside_the_windows_edge() {
        let maximized = PhysicalRect::new(0, 0, 1920, 1152);
        assert_eq!(inside(maximized), PhysicalRect::new(10, 10, 1910, 1142));
        let tiny = PhysicalRect::new(0, 0, 30, 30);
        assert_eq!(inside(tiny), tiny);
    }

    #[tokio::test(start_paused = true)]
    async fn the_frame_stays_through_a_run_of_actions_and_goes_after_the_last() {
        let frames = Arc::new(Frames::default());
        let overlay: Arc<dyn OverlayService> = frames.clone();
        let working = Arc::new(Working::default());
        let rect = PhysicalRect::new(0, 0, 100, 100);
        let first = working.show(&overlay, rect);
        drop(first);
        let second = working.show(&overlay, rect);
        assert_eq!(up(&frames), vec![2], "one frame at a time");
        tokio::time::sleep(LINGER * 2).await;
        assert_eq!(up(&frames), vec![2], "the first action's linger leaves it");
        drop(second);
        tokio::time::sleep(LINGER / 2).await;
        assert_eq!(up(&frames), vec![2]);
        tokio::time::sleep(LINGER).await;
        assert!(up(&frames).is_empty(), "gone after the last action");
        let _third = working.show(&overlay, rect);
        working.hide_now(overlay.as_ref());
        assert!(up(&frames).is_empty(), "screenshots take it down at once");
    }
}
