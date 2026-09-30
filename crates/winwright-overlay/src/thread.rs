//! The native UI thread: one Win32 message loop that owns every overlay HWND, a hidden
//! message-only host window, the auto-hide timers, and all global hotkey registrations
//! (spec §21, §22). State lives in a thread-local; nothing here is touched from other threads
//! except through the command channel plus a `PostMessageW` wake-up.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};

use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, ERROR_HOTKEY_ALREADY_REGISTERED, GetLastError, HINSTANCE, HWND,
    LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VkKeyScanW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HTTRANSPARENT,
    HWND_MESSAGE, KillTimer, MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage, RegisterClassExW,
    SetTimer, USER_TIMER_MAXIMUM, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_HOTKEY,
    WM_MOUSEACTIVATE, WM_NCHITTEST, WM_TIMER, WNDCLASSEXW, WNDPROC,
};
use windows::core::{HRESULT, PCWSTR, w};
use winwright_contracts::input::Key;
use winwright_contracts::overlay::{OverlayId, OverlayRequest};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::keys::{self, Chord};
use crate::render::{self, OVERLAY_CLASS};
use crate::{HotkeyId, platform};

/// Posted to the host window whenever commands are queued.
pub const WM_APP_WAKE: u32 = WM_APP + 1;
const HOST_CLASS: PCWSTR = w!("WinwrightNativeUiHost");
/// Beyond this many live overlays the oldest is destroyed.
const MAX_OVERLAYS: usize = 64;
/// `RegisterHotKey` ids 0x0000..=0xBFFF belong to applications.
const MAX_HOTKEY_ID: i32 = 0xBFFF;

pub type Callback = Arc<dyn Fn() + Send + Sync>;

pub enum Command {
    Show {
        id: OverlayId,
        request: OverlayRequest,
    },
    Clear {
        id: Option<OverlayId>,
    },
    WindowHandle {
        id: OverlayId,
        reply: SyncSender<Option<u64>>,
    },
    Register {
        keys: Vec<Key>,
        callback: Callback,
        reply: SyncSender<WinwrightResult<HotkeyId>>,
    },
    Unregister {
        id: HotkeyId,
        reply: SyncSender<()>,
    },
    /// Adds or updates the tray icon; `reply` only for the first (adding) call.
    TraySet {
        state: crate::tray::TrayState,
        callback: Option<crate::tray::TrayCallback>,
        reply: Option<SyncSender<WinwrightResult<()>>>,
    },
    TrayBalloon {
        title: String,
        body: String,
    },
    TrayRemove,
    Shutdown,
}

/// Lock-free flags shared between the handles and the UI thread.
#[derive(Default)]
pub struct Signals {
    /// A wake message is queued and not yet handled, so senders need not post another.
    pub wake_pending: AtomicBool,
    /// `Show` commands queued but not yet handled (bounds the mailbox, spec §45).
    pub pending_shows: AtomicUsize,
}

struct Overlay {
    hwnd: HWND,
    timer: bool,
}

struct Hotkey {
    chord: Chord,
    callback: Callback,
}

struct UiState {
    rx: Receiver<Command>,
    signals: Arc<Signals>,
    host: HWND,
    hinstance: HINSTANCE,
    overlays: BTreeMap<u64, Overlay>,
    hotkeys: BTreeMap<i32, Hotkey>,
    next_hotkey: i32,
}

thread_local! {
    static STATE: RefCell<Option<UiState>> = const { RefCell::new(None) };
}

/// Thread body: declare PMv2 awareness, create the host window, report readiness (the host
/// HWND as `usize`), then pump messages until shutdown.
pub fn run(
    rx: Receiver<Command>,
    signals: Arc<Signals>,
    ready: SyncSender<WinwrightResult<usize>>,
) {
    // SAFETY: affects only this thread; it must precede every window it creates (spec §43).
    let previous =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if previous.0.is_null() {
        tracing::warn!("Per-Monitor-V2 DPI awareness unavailable; overlay sizes may be off");
    }
    match setup(rx, signals) {
        Ok(host) => {
            let _ = ready.send(Ok(host.0 as usize));
        }
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    }
    drop(ready);
    tracing::debug!("native UI thread ready");

    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out-parameter; `None` reads every message of this thread.
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        match result.0 {
            0 => break,
            -1 => {
                tracing::error!("GetMessageW failed; native UI thread stopping");
                break;
            }
            _ => {}
        }
        // SAFETY: `msg` was just filled by GetMessageW.
        unsafe { DispatchMessageW(&msg) };
    }
    teardown(false);
    tracing::debug!("native UI thread stopped");
}

fn setup(rx: Receiver<Command>, signals: Arc<Signals>) -> WinwrightResult<HWND> {
    // SAFETY: `None` asks for the executable's module handle; nothing is freed.
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map_err(|e| platform("GetModuleHandleW", &e))?
        .into();
    register_class(hinstance, HOST_CLASS, Some(host_proc))?;
    register_class(hinstance, OVERLAY_CLASS, Some(overlay_proc))?;
    // SAFETY: the class is registered above; HWND_MESSAGE makes a message-only window, which
    // is never visible and receives no broadcasts.
    let host = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            HOST_CLASS,
            w!("Winwright native UI"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| platform("CreateWindowExW(host)", &e))?;
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(UiState {
            rx,
            signals,
            host,
            hinstance,
            overlays: BTreeMap::new(),
            hotkeys: BTreeMap::new(),
            next_hotkey: 1,
        });
    });
    Ok(host)
}

pub(crate) fn register_class(
    hinstance: HINSTANCE,
    name: PCWSTR,
    proc: WNDPROC,
) -> WinwrightResult<()> {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: proc,
        hInstance: hinstance,
        lpszClassName: name,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialized and `name` is a static string.
    if unsafe { RegisterClassExW(&class) } != 0 {
        return Ok(());
    }
    // SAFETY: reads this thread's last-error value.
    let error = unsafe { GetLastError() };
    // Classes are per process; another `NativeUi` may have registered it already.
    if error == ERROR_CLASS_ALREADY_EXISTS {
        return Ok(());
    }
    Err(WinwrightError::Platform {
        operation: "RegisterClassExW".into(),
        hresult: HRESULT::from_win32(error.0).0,
    })
}

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Belt and braces with WS_EX_TRANSPARENT: never a hit-test target, never activated.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // SAFETY: forwards the unmodified message to the default procedure.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn host_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_APP_WAKE => guarded("wake", || on_wake(hwnd)),
        WM_TIMER => guarded("timer", || on_timer(hwnd, wparam.0 as u64)),
        WM_HOTKEY => guarded("hotkey", || on_hotkey(wparam.0 as i32)),
        // SAFETY: forwards the unmodified message to the default procedure.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

/// A panic must not unwind into user32 (that aborts the process): log it, keep the loop alive.
fn guarded(what: &'static str, f: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        tracing::error!(what, "native UI handler panicked");
    }
}

fn with_state<R>(f: impl FnOnce(&mut UiState) -> R) -> Option<R> {
    STATE.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

enum Drain {
    Idle,
    Shutdown,
    Busy,
    Stopped,
}

fn on_wake(host: HWND) {
    let outcome = STATE.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            return Drain::Busy;
        };
        let Some(state) = guard.as_mut() else {
            return Drain::Stopped;
        };
        // Clear before draining: a command sent after this point posts a fresh wake.
        state.signals.wake_pending.store(false, Ordering::SeqCst);
        state.drain()
    });
    match outcome {
        Drain::Idle | Drain::Stopped => {}
        Drain::Shutdown => teardown(true),
        Drain::Busy => {
            // Re-entered while the state is borrowed (a nested modal loop); retry later.
            // SAFETY: `host` is this thread's live window.
            let _ = unsafe { PostMessageW(Some(host), WM_APP_WAKE, WPARAM(0), LPARAM(0)) };
        }
    }
}

fn on_timer(host: HWND, id: u64) {
    // SAFETY: stops the (periodic) timer on this thread's host window; auto-hide is one-shot.
    let _ = unsafe { KillTimer(Some(host), id as usize) };
    with_state(|state| state.remove(id));
}

fn on_hotkey(id: i32) {
    // Clone the callback out so it runs with the state released (it may call show/clear).
    let Some((label, callback)) = with_state(|state| {
        state
            .hotkeys
            .get(&id)
            .map(|h| (h.chord.label.clone(), h.callback.clone()))
    })
    .flatten() else {
        return;
    };
    tracing::info!(id, chord = %label, "global hotkey pressed");
    callback();
}

/// Takes the state out and destroys everything it owns, on this thread.
fn teardown(post_quit: bool) {
    let state = STATE.with(|cell| cell.try_borrow_mut().ok().and_then(|mut g| g.take()));
    if let Some(state) = state {
        state.destroy();
    }
    if post_quit {
        // SAFETY: no pointers; ends this thread's message loop.
        unsafe { PostQuitMessage(0) };
    }
}

/// `VkKeyScanW` low byte for the UI thread's keyboard layout.
fn scan_char(c: char) -> Option<u8> {
    let mut units = [0u16; 2];
    let [unit] = c.encode_utf16(&mut units) else {
        return None;
    };
    // SAFETY: no pointers.
    let packed = unsafe { VkKeyScanW(*unit) };
    (packed != -1).then_some((packed as u16 & 0xFF) as u8)
}

fn register_error(chord: &Chord, err: &windows::core::Error) -> WinwrightError {
    WinwrightError::invalid(
        if err.code() == HRESULT::from_win32(ERROR_HOTKEY_ALREADY_REGISTERED.0) {
            format!(
                "hotkey {} is already registered by another application; choose another binding",
                chord.label
            )
        } else {
            format!(
                "cannot register hotkey {} ({}); choose another binding",
                chord.label,
                err.message()
            )
        },
    )
}

impl UiState {
    fn drain(&mut self) -> Drain {
        loop {
            match self.rx.try_recv() {
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return Drain::Shutdown,
                Ok(command) => self.handle(command),
                Err(TryRecvError::Empty) => return Drain::Idle,
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Show { id, request } => {
                self.signals.pending_shows.fetch_sub(1, Ordering::AcqRel);
                self.show(id, &request);
            }
            Command::Clear { id: Some(id) } => self.remove(id.0),
            Command::Clear { id: None } => self.clear_all(),
            Command::WindowHandle { id, reply } => {
                let hwnd = self.overlays.get(&id.0).map(|o| o.hwnd.0 as usize as u64);
                let _ = reply.send(hwnd);
            }
            Command::Register {
                keys,
                callback,
                reply,
            } => {
                let _ = reply.send(self.register(&keys, callback));
            }
            Command::Unregister { id, reply } => {
                self.unregister(id);
                let _ = reply.send(());
            }
            Command::TraySet {
                state,
                callback,
                reply,
            } => {
                let result = crate::tray::set(self.hinstance, state, callback);
                match reply {
                    Some(reply) => {
                        let _ = reply.send(result);
                    }
                    None => {
                        if let Err(err) = result {
                            tracing::warn!(%err, "tray update failed");
                        }
                    }
                }
            }
            Command::TrayBalloon { title, body } => crate::tray::balloon(&title, &body),
            Command::TrayRemove => crate::tray::remove(),
            Command::Shutdown => {}
        }
    }

    fn show(&mut self, id: OverlayId, request: &OverlayRequest) {
        let hwnd = match render::create_overlay(self.hinstance, request) {
            Ok(Some(hwnd)) => hwnd,
            Ok(None) => {
                tracing::debug!(id = id.0, "overlay has no on-screen area; nothing shown");
                return;
            }
            Err(err) => {
                tracing::warn!(id = id.0, %err, "overlay render failed");
                return;
            }
        };
        let timer = request.duration_ms.is_some_and(|ms| {
            let elapse = ms.min(u64::from(USER_TIMER_MAXIMUM)) as u32;
            // SAFETY: `host` is this thread's live window; without a TIMERPROC the expiry
            // arrives as WM_TIMER with `id` in wParam.
            let ok = unsafe { SetTimer(Some(self.host), id.0 as usize, elapse, None) } != 0;
            if !ok {
                tracing::warn!(id = id.0, "SetTimer failed; overlay stays until cleared");
            }
            ok
        });
        self.overlays.insert(id.0, Overlay { hwnd, timer });
        while self.overlays.len() > MAX_OVERLAYS {
            let Some((&oldest, _)) = self.overlays.first_key_value() else {
                break;
            };
            self.remove(oldest);
        }
    }

    fn remove(&mut self, id: u64) {
        let Some(overlay) = self.overlays.remove(&id) else {
            return;
        };
        // SAFETY: the timer and window were created by this thread and are destroyed once.
        unsafe {
            if overlay.timer {
                let _ = KillTimer(Some(self.host), id as usize);
            }
            let _ = DestroyWindow(overlay.hwnd);
        }
    }

    fn clear_all(&mut self) {
        while let Some((&id, _)) = self.overlays.first_key_value() {
            self.remove(id);
        }
    }

    fn register(&mut self, keys: &[Key], callback: Callback) -> WinwrightResult<HotkeyId> {
        let chord = keys::chord(keys, scan_char).map_err(WinwrightError::invalid)?;
        if self
            .hotkeys
            .values()
            .any(|h| h.chord.modifiers == chord.modifiers && h.chord.vk == chord.vk)
        {
            return Err(WinwrightError::invalid(format!(
                "hotkey {} is already registered by this Winwright instance; choose another binding",
                chord.label
            )));
        }
        if self.next_hotkey > MAX_HOTKEY_ID {
            return Err(WinwrightError::invalid(
                "hotkey ids are exhausted; restart the native UI thread",
            ));
        }
        let id = self.next_hotkey;
        let modifiers = HOT_KEY_MODIFIERS(chord.modifiers | MOD_NOREPEAT.0);
        // SAFETY: `host` is this thread's live window; WM_HOTKEY for `id` is posted to it.
        unsafe { RegisterHotKey(Some(self.host), id, modifiers, chord.vk) }
            .map_err(|err| register_error(&chord, &err))?;
        self.next_hotkey += 1;
        tracing::debug!(id, chord = %chord.label, "hotkey registered");
        self.hotkeys.insert(id, Hotkey { chord, callback });
        Ok(HotkeyId(id))
    }

    fn unregister(&mut self, id: HotkeyId) {
        if self.hotkeys.remove(&id.0).is_none() {
            return;
        }
        // SAFETY: `id` was registered on `host` by this thread.
        if let Err(err) = unsafe { UnregisterHotKey(Some(self.host), id.0) } {
            tracing::warn!(id = id.0, %err, "UnregisterHotKey failed");
        }
    }

    /// Destroys every overlay, releases every hotkey (dropping the callbacks here), closes the
    /// mailbox, and finally destroys the host window.
    fn destroy(mut self) {
        self.clear_all();
        crate::tray::remove();
        for &id in self.hotkeys.keys() {
            // SAFETY: `id` was registered on `host` by this thread.
            let _ = unsafe { UnregisterHotKey(Some(self.host), id) };
        }
        self.hotkeys.clear();
        let host = self.host;
        // Dropping the receiver makes later sends fail before they could post to `host`.
        drop(self);
        // SAFETY: `host` was created by this thread and is destroyed exactly once.
        let _ = unsafe { DestroyWindow(host) };
    }
}
