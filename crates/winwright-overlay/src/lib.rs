//! Native overlays and global hotkeys (spec §21, §22).
//!
//! # Threading
//! [`NativeUi::start`] spawns one dedicated thread, `winwright-native-ui`, separate from the
//! UI Automation MTA worker. It declares Per-Monitor-V2 DPI awareness before creating any
//! window and runs a Win32 message loop that owns every overlay window, a hidden
//! message-only host window, the auto-hide timers, and every `RegisterHotKey` registration.
//!
//! Handles talk to it through a mailbox plus one coalesced `PostMessageW` wake-up.
//! [`OverlayService::show`] and [`OverlayService::clear`] never wait for the thread, so the
//! cancellation path (spec §44) can call `clear(None)` from anywhere, including a hotkey
//! callback. Hotkey registration waits for the thread's reply because conflicts must be
//! reported, never ignored (spec §22).
//!
//! [`NativeOverlay`] and [`HotkeyHost`] are handles to the same thread. It stops when the last
//! handle drops or when either handle's `shutdown` is called, which destroys every overlay and
//! unregisters every hotkey.
//!
//! # Overlays
//! Each overlay is a `WS_POPUP` layered window (`WS_EX_LAYERED | WS_EX_TRANSPARENT |
//! WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`) rendered once with
//! `UpdateLayeredWindow` from a premultiplied 32-bit DIB. It answers `WM_NCHITTEST` with
//! `HTTRANSPARENT` and `WM_MOUSEACTIVATE` with `MA_NOACTIVATE`: it never takes focus or clicks.
//! Request rects are physical virtual-desktop pixels and are used as-is; only stroke, marker,
//! and label sizes scale with the target monitor's DPI. The window is clipped to the monitor
//! that shows most of the rect; a rect on no monitor renders nothing.

mod confirm;
mod keys;
mod layout;
mod paint;
mod render;
pub mod theme;
mod thread;
mod tray;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use winwright_contracts::input::{Key, parse_chord};
use winwright_contracts::overlay::{OverlayId, OverlayRequest, OverlayService};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::thread::{Callback, Command, Signals, WM_APP_WAKE};

pub use crate::confirm::{NativeConfirmer, dialog_text};
pub use crate::tray::{TrayMenuItem, TrayState};

/// Default emergency-stop chord (spec §22). The engine binds it to `cancel_all`.
pub const EMERGENCY_STOP_DEFAULT: &str = "Ctrl+Alt+Escape";
/// Longest accepted overlay label, in characters.
pub const MAX_LABEL_CHARS: usize = 120;

/// Largest accepted overlay rect side, in pixels.
const MAX_EXTENT: i32 = 32_767;
/// Accepted coordinate range; the virtual desktop is far smaller.
const COORD_LIMIT: u32 = 65_535;
/// `show` fails with `BACKEND_UNAVAILABLE` when this many are queued and unrendered.
const MAX_PENDING_SHOWS: usize = 256;
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
const BACKEND: &str = "NativeUi";

/// A registered global hotkey.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HotkeyId(pub i32);

pub(crate) fn platform(operation: &str, err: &windows::core::Error) -> WinwrightError {
    WinwrightError::Platform {
        operation: operation.to_owned(),
        hresult: err.code().0,
    }
}

fn unavailable() -> WinwrightError {
    WinwrightError::BackendUnavailable {
        backend: BACKEND.into(),
        reason: "the native UI thread stopped".into(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Rejects requests that cannot render sensibly; never touches the desktop.
fn validate_request(request: &OverlayRequest) -> WinwrightResult<()> {
    let r = request.rect;
    let coords = [r.left, r.top, r.right, r.bottom];
    if coords.iter().any(|c| c.unsigned_abs() > COORD_LIMIT) {
        return Err(WinwrightError::invalid(format!(
            "overlay rect {coords:?} is outside the virtual desktop coordinate range"
        )));
    }
    if r.is_empty() {
        return Err(WinwrightError::invalid(format!(
            "overlay rect {coords:?} must have a positive width and height"
        )));
    }
    if r.width() > MAX_EXTENT || r.height() > MAX_EXTENT {
        return Err(WinwrightError::invalid(format!(
            "overlay rect is {}x{} px; the maximum is {MAX_EXTENT} px per side",
            r.width(),
            r.height()
        )));
    }
    if request.color > 0x00FF_FFFF {
        return Err(WinwrightError::invalid(format!(
            "overlay color {:#x} must be 0xRRGGBB",
            request.color
        )));
    }
    if let Some(label) = &request.label {
        let chars = label.chars().count();
        if chars > MAX_LABEL_CHARS {
            return Err(WinwrightError::invalid(format!(
                "overlay label has {chars} characters; the maximum is {MAX_LABEL_CHARS}"
            )));
        }
    }
    if request.duration_ms == Some(0) {
        return Err(WinwrightError::invalid(
            "overlay durationMs must be positive; omit it to keep the overlay until cleared",
        ));
    }
    Ok(())
}

/// The shared native UI thread. Dropping the last handle shuts it down and joins it.
struct UiThread {
    /// `None` once shut down: later sends fail instead of posting to a destroyed window.
    sender: Mutex<Option<Sender<Command>>>,
    /// Host window (`HWND` as integer; raw handles are not `Send`).
    host: usize,
    signals: Arc<Signals>,
    thread: Mutex<Option<JoinHandle<()>>>,
    thread_id: ThreadId,
    next_overlay: AtomicU64,
}

impl UiThread {
    fn start() -> WinwrightResult<Arc<Self>> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let signals = Arc::new(Signals::default());
        let thread_signals = Arc::clone(&signals);
        let handle = std::thread::Builder::new()
            .name("winwright-native-ui".into())
            .spawn(move || thread::run(rx, thread_signals, ready_tx))
            .map_err(|e| WinwrightError::BackendUnavailable {
                backend: BACKEND.into(),
                reason: format!("cannot spawn the native UI thread: {e}"),
            })?;
        let host = match ready_rx.recv() {
            Ok(Ok(host)) => host,
            Ok(Err(err)) => {
                let _ = handle.join();
                return Err(err);
            }
            Err(_) => {
                let _ = handle.join();
                return Err(unavailable());
            }
        };
        Ok(Arc::new(Self {
            sender: Mutex::new(Some(tx)),
            host,
            signals,
            thread_id: handle.thread().id(),
            thread: Mutex::new(Some(handle)),
            next_overlay: AtomicU64::new(1),
        }))
    }

    /// Posts one wake-up unless one is already queued. Call with the sender lock held so the
    /// host window is known to be alive.
    fn wake(&self) {
        if self.signals.wake_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let host = HWND(self.host as *mut _);
        // SAFETY: PostMessageW is thread-safe; `host` stays alive while the sender exists.
        if let Err(err) = unsafe { PostMessageW(Some(host), WM_APP_WAKE, WPARAM(0), LPARAM(0)) } {
            self.signals.wake_pending.store(false, Ordering::Release);
            tracing::warn!(%err, "cannot wake the native UI thread");
        }
    }

    fn submit(&self, command: Command) -> WinwrightResult<()> {
        let sender = lock(&self.sender);
        let tx = sender.as_ref().ok_or_else(unavailable)?;
        tx.send(command).map_err(|_| unavailable())?;
        self.wake();
        Ok(())
    }

    /// Sends a command carrying a reply channel and waits for the answer.
    fn request<T>(
        &self,
        operation: &'static str,
        make: impl FnOnce(SyncSender<T>) -> Command,
    ) -> WinwrightResult<T> {
        if std::thread::current().id() == self.thread_id {
            return Err(WinwrightError::invalid(format!(
                "{operation} cannot run on the native UI thread (for example inside a hotkey \
                 callback)"
            )));
        }
        let (reply, rx) = mpsc::sync_channel(1);
        self.submit(make(reply))?;
        let started = Instant::now();
        rx.recv_timeout(REPLY_TIMEOUT).map_err(|err| match err {
            RecvTimeoutError::Timeout => WinwrightError::Timeout {
                operation: operation.into(),
                elapsed_ms: started.elapsed().as_millis() as u64,
            },
            RecvTimeoutError::Disconnected => unavailable(),
        })
    }

    fn shutdown(&self) {
        {
            let mut sender = lock(&self.sender);
            let Some(tx) = sender.take() else {
                return;
            };
            let _ = tx.send(Command::Shutdown);
            self.wake();
        }
        let Some(handle) = lock(&self.thread).take() else {
            return;
        };
        // Called from a hotkey callback: the thread exits once the callback returns.
        if handle.thread().id() != std::thread::current().id() && handle.join().is_err() {
            tracing::error!("native UI thread panicked");
        }
    }
}

impl Drop for UiThread {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Overlays, hotkeys and the tray icon on one shared native UI thread.
pub struct NativeUi {
    pub overlay: NativeOverlay,
    pub hotkeys: HotkeyHost,
    pub tray: TrayHost,
}

impl NativeUi {
    /// Spawns the native UI thread and waits until its message loop is ready.
    pub fn start() -> WinwrightResult<Self> {
        let ui = UiThread::start()?;
        Ok(Self {
            overlay: NativeOverlay {
                ui: Arc::clone(&ui),
            },
            tray: TrayHost {
                ui: Arc::clone(&ui),
            },
            hotkeys: HotkeyHost { ui },
        })
    }
}

/// The notification-area icon. Menu callbacks run on the native UI thread and must be quick;
/// they may call [`TrayHost::update`]. The icon is removed on shutdown.
#[derive(Clone)]
pub struct TrayHost {
    ui: Arc<UiThread>,
}

impl TrayHost {
    /// Adds the icon; `on_command` receives the chosen menu item's id.
    pub fn show(
        &self,
        state: TrayState,
        on_command: Box<dyn Fn(u32) + Send + Sync>,
    ) -> WinwrightResult<()> {
        let callback: crate::tray::TrayCallback = Arc::from(on_command);
        self.ui.request("tray show", |reply| Command::TraySet {
            state,
            callback: Some(callback),
            reply: Some(reply),
        })?
    }

    /// Changes tooltip, icon color, or menu. Never blocks; usable from callbacks.
    pub fn update(&self, state: TrayState) {
        let _ = self.ui.submit(Command::TraySet {
            state,
            callback: None,
            reply: None,
        });
    }

    /// Shows a notification from the icon (a toast on Windows 11). Never blocks.
    pub fn notify(&self, title: &str, body: &str) {
        let _ = self.ui.submit(Command::TrayBalloon {
            title: title.to_owned(),
            body: body.to_owned(),
        });
    }

    pub fn remove(&self) {
        let _ = self.ui.submit(Command::TrayRemove);
    }
}

/// Click-through, never-activating overlays ([`OverlayService`]).
pub struct NativeOverlay {
    ui: Arc<UiThread>,
}

impl NativeOverlay {
    /// A native UI thread used only for overlays; use [`NativeUi::start`] to share it.
    pub fn start() -> WinwrightResult<Self> {
        Ok(NativeUi::start()?.overlay)
    }

    /// Diagnostics: the overlay's window handle once rendered, or `None` if it was cleared,
    /// expired, or had nothing on screen. Waits for every earlier command to be handled.
    pub fn window_handle(&self, id: OverlayId) -> WinwrightResult<Option<u64>> {
        self.ui
            .request("overlay window_handle", |reply| Command::WindowHandle {
                id,
                reply,
            })
    }

    /// Destroys every overlay and hotkey and stops the shared thread (also done on drop).
    pub fn shutdown(&self) {
        self.ui.shutdown();
    }
}

impl OverlayService for NativeOverlay {
    /// Validates, queues, and returns at once; rendering happens on the UI thread.
    fn show(&self, request: OverlayRequest) -> WinwrightResult<OverlayId> {
        validate_request(&request)?;
        let pending = &self.ui.signals.pending_shows;
        if pending.fetch_add(1, Ordering::AcqRel) >= MAX_PENDING_SHOWS {
            pending.fetch_sub(1, Ordering::AcqRel);
            return Err(WinwrightError::BackendUnavailable {
                backend: BACKEND.into(),
                reason: format!("{MAX_PENDING_SHOWS} overlays are queued and not yet drawn"),
            });
        }
        let id = OverlayId(self.ui.next_overlay.fetch_add(1, Ordering::Relaxed));
        if let Err(err) = self.ui.submit(Command::Show { id, request }) {
            pending.fetch_sub(1, Ordering::AcqRel);
            return Err(err);
        }
        Ok(id)
    }

    /// Never blocks. Unknown ids are fine; after shutdown there is nothing left to clear.
    fn clear(&self, id: Option<OverlayId>) -> WinwrightResult<()> {
        match self.ui.submit(Command::Clear { id }) {
            Err(WinwrightError::BackendUnavailable { .. }) => Ok(()),
            other => other,
        }
    }
}

/// Global hotkeys via `RegisterHotKey` (with `MOD_NOREPEAT`) on the native UI thread.
///
/// Callbacks run **on the native UI thread**: they must be quick and must not block (the
/// engine uses one to trigger `cancel_all`). They may call [`OverlayService::show`] and
/// [`OverlayService::clear`]; [`HotkeyHost::register`], [`HotkeyHost::unregister`], and
/// [`NativeOverlay::window_handle`] return `INVALID_REQUEST` there instead of deadlocking.
/// A callback that owns a [`NativeUi`] handle keeps the thread alive; call `shutdown`
/// explicitly in that case.
pub struct HotkeyHost {
    ui: Arc<UiThread>,
}

impl HotkeyHost {
    /// Registers `binding` (modifiers plus exactly one key). Conflicts with other
    /// applications, or with this instance, are `INVALID_REQUEST` naming the chord.
    pub fn register(
        &self,
        binding: &[Key],
        callback: Box<dyn Fn() + Send + Sync>,
    ) -> WinwrightResult<HotkeyId> {
        let keys = binding.to_vec();
        let callback: Callback = Arc::from(callback);
        self.ui
            .request("hotkey register", |reply| Command::Register {
                keys,
                callback,
                reply,
            })?
    }

    /// Registers the emergency stop: `binding` (e.g. `"Ctrl+Alt+Escape"`) or
    /// [`EMERGENCY_STOP_DEFAULT`].
    pub fn register_emergency_stop(
        &self,
        binding: Option<&str>,
        callback: Box<dyn Fn() + Send + Sync>,
    ) -> WinwrightResult<HotkeyId> {
        let text = binding.unwrap_or(EMERGENCY_STOP_DEFAULT);
        let keys = parse_chord(text).map_err(|e| {
            WinwrightError::invalid(format!("invalid emergency stop hotkey {text:?}: {e}"))
        })?;
        self.register(&keys, callback)
    }

    /// Unregisters and drops the callback. Unknown ids are fine.
    pub fn unregister(&self, id: HotkeyId) -> WinwrightResult<()> {
        self.ui
            .request("hotkey unregister", |reply| Command::Unregister {
                id,
                reply,
            })
    }

    /// Destroys every overlay and hotkey and stops the shared thread (also done on drop).
    pub fn shutdown(&self) {
        self.ui.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use winwright_contracts::ErrorCode;
    use winwright_contracts::geometry::PhysicalRect;
    use winwright_contracts::overlay::OverlayStyle;

    use super::*;

    fn request(rect: [i32; 4]) -> OverlayRequest {
        OverlayRequest {
            rect: PhysicalRect::from(rect),
            style: OverlayStyle::Highlight,
            label: None,
            step: None,
            color: 0x00E0_4A2A,
            duration_ms: None,
        }
    }

    fn message(result: WinwrightResult<()>) -> String {
        let err = result.unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
        err.to_string()
    }

    #[test]
    fn valid_requests_pass() {
        validate_request(&request([-1920, -200, -1000, 400])).unwrap();
        let mut full = request([0, 0, 3840, 2160]);
        full.label = Some("x".repeat(MAX_LABEL_CHARS));
        full.duration_ms = Some(1);
        full.color = 0x00FF_FFFF;
        validate_request(&full).unwrap();
    }

    #[test]
    fn empty_huge_and_far_rects_are_rejected() {
        assert!(message(validate_request(&request([10, 10, 10, 50]))).contains("positive"));
        assert!(message(validate_request(&request([10, 50, 20, 40]))).contains("positive"));
        assert!(message(validate_request(&request([0, 0, 40_000, 10]))).contains("maximum"));
        assert!(message(validate_request(&request([i32::MIN, 0, 10, 10]))).contains("coordinate"));
    }

    #[test]
    fn labels_colors_and_durations_are_checked() {
        let mut long = request([0, 0, 10, 10]);
        long.label = Some("é".repeat(MAX_LABEL_CHARS + 1));
        assert!(message(validate_request(&long)).contains("121 characters"));

        let mut argb = request([0, 0, 10, 10]);
        argb.color = 0xFF00_FF00;
        assert!(message(validate_request(&argb)).contains("0xRRGGBB"));

        let mut zero = request([0, 0, 10, 10]);
        zero.duration_ms = Some(0);
        assert!(message(validate_request(&zero)).contains("durationMs"));
    }
}
