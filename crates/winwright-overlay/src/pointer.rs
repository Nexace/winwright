//! The person's own mouse clicks, for guides (native UI thread only): a low-level mouse hook
//! installed while someone watches and removed with the last watcher. It reports button
//! presses and releases only, skips injected input, and never sees the keyboard.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, LLMHF_INJECTED, MSLLHOOKSTRUCT, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_RBUTTONDOWN, WM_RBUTTONUP,
};
use winwright_contracts::WinwrightResult;
use winwright_contracts::geometry::PhysicalPoint;
use winwright_contracts::input::MouseButton;
use winwright_contracts::overlay::PointerEvent;

use crate::platform;

pub type Watcher = Arc<dyn Fn(PointerEvent) + Send + Sync>;

thread_local! {
    static WATCHERS: RefCell<Vec<(u64, Watcher)>> = const { RefCell::new(Vec::new()) };
    static HOOK: Cell<Option<HHOOK>> = const { Cell::new(None) };
}

/// The event for one hooked mouse message, or `None` for moves, wheels and injected input.
pub(crate) fn event(message: u32, info: &MSLLHOOKSTRUCT) -> Option<PointerEvent> {
    if info.flags & LLMHF_INJECTED != 0 {
        return None;
    }
    let (button, down) = match message {
        WM_LBUTTONDOWN => (MouseButton::Left, true),
        WM_LBUTTONUP => (MouseButton::Left, false),
        WM_RBUTTONDOWN => (MouseButton::Right, true),
        WM_RBUTTONUP => (MouseButton::Right, false),
        WM_MBUTTONDOWN => (MouseButton::Middle, true),
        WM_MBUTTONUP => (MouseButton::Middle, false),
        _ => return None,
    };
    Some(PointerEvent {
        point: PhysicalPoint {
            x: info.pt.x,
            y: info.pt.y,
        },
        button,
        down,
    })
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lParam points at this event's MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if let Some(event) = event(wparam.0 as u32, info) {
            crate::thread::guarded("pointer", || notify(event));
        }
    }
    // SAFETY: passes the event on unchanged; every hook must.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn notify(event: PointerEvent) {
    let watchers: Vec<Watcher> = WATCHERS.with(|w| {
        w.try_borrow()
            .map(|w| w.iter().map(|(_, f)| Arc::clone(f)).collect())
            .unwrap_or_default()
    });
    for watcher in watchers {
        watcher(event);
    }
}

pub fn watch(hinstance: HINSTANCE, id: u64, watcher: Watcher) -> WinwrightResult<()> {
    if HOOK.get().is_none() {
        // SAFETY: a global low-level hook; Windows calls `mouse_hook` on this thread, which
        // pumps messages.
        let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(hinstance), 0) }
            .map_err(|e| platform("SetWindowsHookExW", &e))?;
        HOOK.set(Some(hook));
    }
    WATCHERS.with(|w| w.borrow_mut().push((id, watcher)));
    Ok(())
}

pub fn unwatch(id: u64) {
    let empty = WATCHERS.with(|w| {
        let mut w = w.borrow_mut();
        w.retain(|(i, _)| *i != id);
        w.is_empty()
    });
    if empty {
        stop();
    }
}

/// Removes the hook and every watcher.
pub fn stop() {
    WATCHERS.with(|w| w.borrow_mut().clear());
    if let Some(hook) = HOOK.take() {
        // SAFETY: installed by this thread and removed once.
        let _ = unsafe { UnhookWindowsHookEx(hook) };
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{WM_MOUSEMOVE, WM_MOUSEWHEEL};

    use super::*;

    fn info(flags: u32) -> MSLLHOOKSTRUCT {
        MSLLHOOKSTRUCT {
            pt: POINT { x: -20, y: 300 },
            flags,
            ..Default::default()
        }
    }

    #[test]
    fn only_the_persons_own_button_presses_and_releases_count() {
        let press = event(WM_RBUTTONDOWN, &info(0)).unwrap();
        assert_eq!(
            press,
            PointerEvent {
                point: PhysicalPoint { x: -20, y: 300 },
                button: MouseButton::Right,
                down: true,
            }
        );
        let release = event(WM_LBUTTONUP, &info(0)).unwrap();
        assert_eq!((release.button, release.down), (MouseButton::Left, false));
        assert_eq!(
            event(WM_MBUTTONDOWN, &info(0)).unwrap().button,
            MouseButton::Middle
        );
        // Winwright's own clicks (and any other injected input) are not the person's.
        assert_eq!(event(WM_LBUTTONDOWN, &info(LLMHF_INJECTED)), None);
        assert_eq!(event(WM_LBUTTONDOWN, &info(LLMHF_INJECTED | 2)), None);
        assert_eq!(event(WM_MOUSEMOVE, &info(0)), None);
        assert_eq!(event(WM_MOUSEWHEEL, &info(0)), None);
    }
}
