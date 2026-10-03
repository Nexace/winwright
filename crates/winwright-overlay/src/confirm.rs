//! Trusted local confirmation dialog (spec §66): a native Winwright window that only the
//! person at the keyboard can answer.
//!
//! - The dialog brings itself to the front when it opens, so one click on "Allow once" works.
//! - "Deny" is the default and focused button: Enter, Esc, Alt+F4 and closing all deny.
//! - "Allow once" stays disabled until a moment after the dialog becomes the active window
//!   (again after each time it loses activation), so a click or key meant for another window,
//!   or the click that brings the dialog to the front, cannot approve an action. From the keyboard it also counts only
//!   after a pause, so typing that runs into the dialog (Tab, then Space) cannot approve.
//! - Only a real keyboard, mouse, pen or touch screen can press "Allow once". A BM_CLICK or
//!   WM_COMMAND from another program, UI Automation's Invoke, and SendInput (Winwright's own
//!   input, but also on-screen keyboards and voice control) are all ignored. People who can
//!   only use those cannot approve, by design: anything they can do, a program can fake.
//! - A prompt that is not answered in time, or whose request is abandoned, is denied and the
//!   window closes itself.
//!
//! Every piece of text is an owner-drawn STATIC control, so screen readers read the prompt
//! while it keeps the Winwright look.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DT_END_ELLIPSIS, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject, EndPaint, GetDC,
    GetMonitorInfoW, HDC, HGDIOBJ, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, PAINTSTRUCT, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::Controls::{
    CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT,
    CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor,
    MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus};
use windows::Win32::UI::Input::{GetCurrentInputMessageSource, IMO_HARDWARE, INPUT_MESSAGE_SOURCE};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateWindowExW, DC_HASDEFID, DM_GETDEFID,
    DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, FLASHW_ALL, FLASHW_TIMERNOFG,
    FLASHWINFO, FlashWindowEx, GetCursorPos, GetForegroundWindow, GetMessageW, HICON, HMENU,
    ICON_BIG, ICON_SMALL, IDCANCEL, IsDialogMessageW, KillTimer, MSG, PostMessageW,
    PostQuitMessage, SM_CXICON, SM_CXSMICON, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOZORDER,
    SendMessageW, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage,
    WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_KEYDOWN, WM_KEYFIRST, WM_KEYLAST, WM_MOUSEFIRST,
    WM_MOUSELAST, WM_NOTIFY, WM_PAINT, WM_SETFONT, WM_SETICON, WM_SYSKEYDOWN, WM_TIMER, WS_CAPTION,
    WS_CHILD, WS_CLIPCHILDREN, WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_OVERLAPPED, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};
use winwright_contracts::WinwrightResult;
use winwright_contracts::backend::{BackendFuture, WindowBackend};
use winwright_contracts::security::{ConfirmationPrompt, Confirmer};

use winwright_win32::Win32Windows;

use crate::platform;
use crate::theme::{self, ButtonKind, ButtonState, Fonts, Palette, glyph};

const CLASS: PCWSTR = w!("WinwrightConfirm");
const TITLE: PCWSTR = w!("Winwright: confirm action");
/// IDCANCEL, so Esc denies through IsDialogMessage.
const ID_DENY: i32 = IDCANCEL.0;
const ID_ALLOW: i32 = 100;
const TIMER_TICK: usize = 1;
const TIMER_ARM: usize = 2;
const ARM_DELAY_MS: u32 = 800;
const ARM_DELAY: Duration = Duration::from_millis(ARM_DELAY_MS as u64);
const WIDTH: i32 = 460;
const SS_OWNERDRAW: u32 = 0x0D;
const SS_NOPREFIX: u32 = 0x80;
const ODT_STATIC: u32 = 5;

#[derive(Debug, Default)]
pub struct NativeConfirmer;

impl NativeConfirmer {
    pub fn new() -> Self {
        Self
    }
}

/// The dialog text as plain lines. Pure so it can be tested; never contains typed text or
/// field values (the engine's summaries only count characters).
pub fn dialog_text(prompt: &ConfirmationPrompt) -> String {
    let c = Content::of(prompt);
    let mut text = format!("{}\n{}\n\n{}\n", c.title, c.subtitle, c.summary);
    if !c.context.is_empty() {
        text.push_str(&format!("{}\n", c.context));
    }
    text.push_str(&format!(
        "\n{}\n{}\n{}",
        c.reason,
        c.hint,
        c.countdown(c.seconds, true)
    ));
    text
}

/// What the dialog says.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Content {
    title: String,
    subtitle: String,
    summary: String,
    context: String,
    reason: String,
    hint: String,
    seconds: u64,
}

/// Text from apps and the model (window titles, arguments) shown as it really is: control
/// characters (a line break could fake a new line of the prompt) and invisible format
/// characters (a right-to-left override reverses what follows) become visible escapes.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            let invisible = matches!(c,
                '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}');
            if c.is_control() || invisible {
                format!("\\u{{{:04X}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

impl Content {
    fn of(prompt: &ConfirmationPrompt) -> Self {
        let mut context = Vec::new();
        if let Some(t) = &prompt.target {
            if let Some(window) = &t.window {
                context.push(format!("Window: {}", printable(window)));
            }
            if let Some(process) = &t.process {
                context.push(format!("App: {}", printable(process)));
            }
        }
        Self {
            title: "Allow this action?".into(),
            subtitle: "An AI assistant using Winwright is asking for your approval.".into(),
            summary: printable(&prompt.summary),
            context: context.join("  \u{00B7}  "),
            reason: format!("Why you are asked: {}", printable(&prompt.reason)),
            hint: "Deny is the default. Press Ctrl+Alt+Esc at any time to stop Winwright.".into(),
            seconds: prompt.timeout_ms.max(1_000) / 1000,
        }
    }

    /// While the dialog is not the active window "Allow once" is off, so say how to turn it on.
    fn countdown(&self, left: u64, active: bool) -> String {
        if active {
            format!("Denied automatically in {left} s")
        } else {
            format!("Click here first · {left} s left")
        }
    }
}

/// Shared by a request and its dialog thread.
#[derive(Default)]
struct Link {
    /// The dialog window while it exists (0 before and after: window handles are recycled,
    /// so a stale one could name another app's window).
    hwnd: AtomicIsize,
    /// The request stopped waiting (timeout, cancellation): the dialog must deny and close.
    abandoned: AtomicBool,
}

/// Answers an abandoned or timed-out dialog with "Deny" so it never lingers, even when the
/// request gives up before the window exists (the dialog checks `abandoned` once created).
struct DenyOnDrop(Arc<Link>);

impl Drop for DenyOnDrop {
    fn drop(&mut self) {
        self.0.abandoned.store(true, Ordering::SeqCst);
        let hwnd = self.0.hwnd.load(Ordering::SeqCst);
        if hwnd != 0 {
            // SAFETY: posts to the dialog window; a stale handle just fails the post.
            let _ = unsafe {
                PostMessageW(
                    Some(HWND(hwnd as *mut std::ffi::c_void)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
    }
}

/// Clears [`Link::hwnd`] when the dialog thread is done with its window, on every path.
struct ForgetWindow<'a>(&'a Link);

impl Drop for ForgetWindow<'_> {
    fn drop(&mut self) {
        self.0.hwnd.store(0, Ordering::SeqCst);
    }
}

/// The dialog's answer: `false` on timeout or when the dialog thread ends without one. The
/// guard is created by the caller and owned by the returned future (an `async fn` holds its
/// arguments from the call), so dropping the future at any point, even before its first
/// poll, denies and closes the dialog.
async fn wait_for_answer(
    guard: DenyOnDrop,
    rx: tokio::sync::oneshot::Receiver<bool>,
    timeout: Duration,
) -> bool {
    let _guard = guard;
    tokio::time::timeout(timeout, rx)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false)
}

impl Confirmer for NativeConfirmer {
    fn confirm<'a>(&'a self, prompt: ConfirmationPrompt) -> BackendFuture<'a, bool> {
        let timeout = Duration::from_millis(prompt.timeout_ms.max(1_000));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let link = Arc::new(Link::default());
        let thread_link = Arc::clone(&link);
        let spawned = std::thread::Builder::new()
            .name("winwright-confirm".into())
            .spawn(move || {
                let answer = match run_dialog(&thread_link, &prompt, timeout) {
                    Ok(answer) => answer,
                    Err(err) => {
                        tracing::warn!(%err, "confirmation dialog unavailable; denying");
                        false
                    }
                };
                let _ = tx.send(answer);
            });
        // Created now, not on the first poll: the dialog already exists.
        let answer = wait_for_answer(DenyOnDrop(link), rx, timeout);
        if spawned.is_err() {
            return Box::pin(async { Ok(false) });
        }
        Box::pin(async move { Ok(answer.await) })
    }
}

// ---------------------------------------------------------------------------------------
// The window

#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Title,
    Subtitle,
    Summary,
    Context,
    Reason,
    Hint,
    Countdown,
}

struct Block {
    look: Look,
    hwnd: HWND,
    rect: RECT,
}

struct Dialog {
    deny: HWND,
    allow: HWND,
    blocks: Vec<Block>,
    palette: Palette,
    fonts: Fonts,
    content: Content,
    deadline: Instant,
    armed: bool,
    /// When the dialog last really became the active window (`None` while it is not).
    active_since: Option<Instant>,
    answer: Option<bool>,
    link: Arc<Link>,
    focus: HWND,
    icons: Vec<HICON>,
    /// Painted by the dialog: badge, card, and the button band.
    badge: RECT,
    card: RECT,
    band_top: i32,
}

/// Keyboard activity seen by the dialog, so typing meant for another window that runs into
/// it (Tab to "Allow once", then Space or Enter) cannot approve.
#[derive(Clone, Copy, Debug, Default)]
struct Keys {
    last_press: Option<Instant>,
    /// The latest key press came at least `ARM_DELAY` after the one before it.
    paused: bool,
    /// A keyboard message is being handled right now.
    handling: bool,
}

impl Keys {
    /// Starts handling a keyboard message (`press`: key down, including auto-repeat).
    fn begin(self, press: bool, now: Instant) -> Self {
        let mut next = Self {
            handling: true,
            ..self
        };
        if press {
            next.paused = self
                .last_press
                .is_none_or(|t| now.saturating_duration_since(t) >= ARM_DELAY);
            next.last_press = Some(now);
        }
        next
    }

    fn end(self) -> Self {
        Self {
            handling: false,
            ..self
        }
    }

    /// A mouse click, or a key pressed after a pause.
    fn may_allow(self) -> bool {
        !self.handling || self.paused
    }
}

/// Where the input the dialog is handling right now came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Origin {
    /// No input message from the dialog's own loop: a sent or posted BM_CLICK or WM_COMMAND,
    /// which is also how UI Automation's Invoke reaches a Win32 button.
    #[default]
    None,
    /// Input a program made: SendInput, so also on-screen keyboards and voice control.
    Injected,
    /// A real keyboard, mouse, pen or touch screen.
    Hardware,
}

impl Origin {
    /// The origin of the input message this thread just retrieved.
    fn of_current_message() -> Self {
        let mut source = INPUT_MESSAGE_SOURCE::default();
        // SAFETY: fills the struct we pass.
        match unsafe { GetCurrentInputMessageSource(&mut source) } {
            Ok(()) if source.originId == IMO_HARDWARE => Self::Hardware,
            _ => Self::Injected,
        }
    }
}

/// What an arm-timer message may do. Any program can send WM_ACTIVATE or post WM_TIMER, so
/// arming checks the facts: the dialog really is in front and has been since `ARM_DELAY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arming {
    Arm,
    /// Too early (a forged timer): check again in this many milliseconds.
    Wait(u32),
    /// Not the active window: the next activation starts the delay again.
    Not,
}

fn arming(active_since: Option<Instant>, foreground: bool, now: Instant) -> Arming {
    match active_since {
        Some(since) if foreground => {
            let waited = now.saturating_duration_since(since);
            match ARM_DELAY.checked_sub(waited) {
                None | Some(Duration::ZERO) => Arming::Arm,
                Some(left) => Arming::Wait(left.as_millis() as u32 + 1),
            }
        }
        _ => Arming::Not,
    }
}

/// Whether a WM_COMMAND for `ID_ALLOW` approves: only the armed Allow button's own click, made
/// by a person with real hardware.
fn allow_counts(armed: bool, code: u32, from_allow: bool, keys: Keys, origin: Origin) -> bool {
    armed && code == BN_CLICKED && from_allow && origin == Origin::Hardware && keys.may_allow()
}

thread_local! {
    static DIALOG: RefCell<Option<Dialog>> = const { RefCell::new(None) };
    static KEYS: Cell<Keys> = Cell::new(Keys::default());
    static ORIGIN: Cell<Origin> = const { Cell::new(Origin::None) };
}

fn with_dialog<R>(f: impl FnOnce(&mut Dialog) -> R) -> Option<R> {
    DIALOG.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

fn rect(left: i32, top: i32, width: i32, height: i32) -> RECT {
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

impl Dialog {
    fn font(&self, look: Look) -> &theme::Font {
        match look {
            Look::Title => &self.fonts.title,
            Look::Summary => &self.fonts.strong,
            Look::Subtitle | Look::Context | Look::Reason | Look::Hint | Look::Countdown => {
                &self.fonts.small
            }
        }
    }

    fn colors(&self, look: Look) -> (u32, u32) {
        let p = &self.palette;
        match look {
            Look::Title | Look::Summary => (
                p.text,
                if look == Look::Summary {
                    p.window
                } else {
                    p.surface
                },
            ),
            Look::Context => (p.muted, p.window),
            Look::Subtitle | Look::Reason => (p.muted, p.surface),
            Look::Hint => (p.faint, p.surface),
            Look::Countdown => (p.muted, p.window),
        }
    }

    fn text_of(&self, look: Look) -> String {
        let c = &self.content;
        match look {
            Look::Title => c.title.clone(),
            Look::Subtitle => c.subtitle.clone(),
            Look::Summary => c.summary.clone(),
            Look::Context => c.context.clone(),
            Look::Reason => c.reason.clone(),
            Look::Hint => c.hint.clone(),
            Look::Countdown => c.countdown(self.seconds_left(), self.active_since.is_some()),
        }
    }

    fn seconds_left(&self) -> u64 {
        self.deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f64()
            .ceil() as u64
    }

    /// Positions every block and button for the current DPI; returns the client size.
    fn layout(&mut self) -> (i32, i32) {
        let dpi = self.fonts.dpi;
        let px = |v| theme::scale(v, dpi);
        let pad = px(24);
        let width = px(WIDTH);
        let inner = width - pad * 2;
        // SAFETY: a screen DC borrowed for measuring only.
        let dc = unsafe { GetDC(None) };
        let measure = |look: Look, w: i32| -> i32 {
            let text = self.text_of(look);
            if text.is_empty() {
                return 0;
            }
            theme::measure(dc, self.font(look), &text, Some(w), DT_WORDBREAK).1
        };
        let badge = px(40);
        let text_x = pad + badge + px(14);
        let text_w = width - pad - text_x;
        let mut rects = Vec::new();
        let title_h = measure(Look::Title, text_w);
        let sub_h = measure(Look::Subtitle, text_w);
        let header_h = (title_h + px(2) + sub_h).max(badge);
        let header_top = pad;
        let text_top = header_top + (header_h - (title_h + px(2) + sub_h)) / 2;
        rects.push((Look::Title, rect(text_x, text_top, text_w, title_h)));
        rects.push((
            Look::Subtitle,
            rect(text_x, text_top + title_h + px(2), text_w, sub_h),
        ));
        let badge_rect = rect(pad, header_top + (header_h - badge) / 2, badge, badge);
        let mut y = header_top + header_h + px(18);
        let card_pad = px(14);
        let card_inner = inner - card_pad * 2;
        let sum_h = measure(Look::Summary, card_inner);
        let ctx_h = measure(Look::Context, card_inner);
        let card_h = card_pad * 2 + sum_h + if ctx_h > 0 { px(4) + ctx_h } else { 0 };
        let card_rect = rect(pad, y, inner, card_h);
        rects.push((
            Look::Summary,
            rect(pad + card_pad, y + card_pad, card_inner, sum_h),
        ));
        if ctx_h > 0 {
            rects.push((
                Look::Context,
                rect(
                    pad + card_pad,
                    y + card_pad + sum_h + px(4),
                    card_inner,
                    ctx_h,
                ),
            ));
        }
        y += card_h + px(14);
        let reason_h = measure(Look::Reason, inner);
        rects.push((Look::Reason, rect(pad, y, inner, reason_h)));
        y += reason_h + px(6);
        let hint_h = measure(Look::Hint, inner);
        rects.push((Look::Hint, rect(pad, y, inner, hint_h)));
        y += hint_h + px(22);
        let band_top = y;
        let band_h = px(68);
        let button_h = px(36);
        let button_w = px(124);
        let button_y = y + (band_h - button_h) / 2;
        let allow_x = width - pad + px(2) - button_w;
        let deny_x = allow_x - px(6) - button_w;
        let countdown_w = deny_x - pad - px(8);
        rects.push((
            Look::Countdown,
            rect(pad, y + 1, countdown_w.max(0), band_h - 1),
        ));
        // SAFETY: releases the DC borrowed above.
        unsafe { ReleaseDC(None, dc) };
        self.badge = badge_rect;
        self.card = card_rect;
        self.band_top = band_top;
        for (look, r) in rects {
            if let Some(b) = self.blocks.iter_mut().find(|b| b.look == look) {
                b.rect = r;
            }
        }
        // SAFETY: moves this dialog's own child windows.
        unsafe {
            for b in &self.blocks {
                let r = b.rect;
                let _ = SetWindowPos(
                    b.hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            let _ = SetWindowPos(
                self.deny,
                None,
                deny_x,
                button_y,
                button_w,
                button_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = SetWindowPos(
                self.allow,
                None,
                allow_x,
                button_y,
                button_w,
                button_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        (width, y + band_h)
    }

    fn paint(&self, hdc: HDC, client: RECT) {
        let p = &self.palette;
        let band = RECT {
            top: self.band_top,
            ..client
        };
        theme::fill(
            hdc,
            RECT {
                bottom: self.band_top,
                ..client
            },
            p.surface,
        );
        theme::fill(hdc, band, p.window);
        theme::fill(
            hdc,
            RECT {
                bottom: self.band_top + 1,
                ..band
            },
            p.border,
        );
        theme::dot(hdc, self.badge, p.selection);
        theme::icon(hdc, &self.fonts.icons, glyph::SHIELD, self.badge, p.accent);
        theme::rounded(
            hdc,
            self.card,
            self.fonts.px(6) as f32,
            p.window,
            Some(p.border),
        );
    }

    fn draw_block(&self, d: &DRAWITEMSTRUCT) {
        let Some(block) = self.blocks.iter().find(|b| b.hwnd == d.hwndItem) else {
            return;
        };
        let (fg, bg) = self.colors(block.look);
        theme::fill(d.hDC, d.rcItem, bg);
        let flags = if block.look == Look::Countdown {
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS
        } else {
            DT_WORDBREAK | DT_LEFT
        };
        theme::text(
            d.hDC,
            self.font(block.look),
            &self.text_of(block.look),
            d.rcItem,
            fg,
            flags,
        );
    }

    fn draw_button(&self, cd: &NMCUSTOMDRAW) {
        let is_deny = cd.hdr.hwndFrom == self.deny;
        let state = ButtonState {
            hot: cd.uItemState.0 & CDIS_HOT.0 != 0,
            pressed: cd.uItemState.0 & CDIS_SELECTED.0 != 0,
            focused: cd.uItemState.0 & CDIS_FOCUS.0 != 0,
            disabled: cd.uItemState.0 & CDIS_DISABLED.0 != 0,
        };
        theme::draw_button(
            cd.hdc,
            cd.rc,
            self.palette.window,
            if is_deny {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            },
            state,
            if is_deny { "Deny" } else { "Allow once" },
            None,
            &self.palette,
            &self.fonts,
        );
    }
}

fn child(parent: HWND, class: PCWSTR, text: &str, style: u32, id: i32) -> WinwrightResult<HWND> {
    let text = HSTRING::from(text);
    // SAFETY: creates a child of `parent` on this thread; the menu handle carries the id.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &text,
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as isize as _)),
            None,
            None,
        )
    }
    .map_err(|e| platform("CreateWindowExW(confirm child)", &e))
}

fn cursor_monitor_dpi_and_work_area() -> (u32, RECT) {
    // SAFETY: plain cursor/monitor queries with local out-parameters.
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (dx.max(96), info.rcWork)
    }
}

fn window_size(client: (i32, i32), dpi: u32) -> (i32, i32) {
    let mut r = rect(0, 0, client.0, client.1);
    // SAFETY: `r` is a local RECT adjusted in place.
    let _ =
        unsafe { AdjustWindowRectExForDpi(&mut r, dialog_style(), false, dialog_ex_style(), dpi) };
    (r.right - r.left, r.bottom - r.top)
}

fn dialog_style() -> WINDOW_STYLE {
    WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0)
}

fn dialog_ex_style() -> WINDOW_EX_STYLE {
    WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_APPWINDOW.0)
}

fn set_icons(hwnd: HWND, dpi: u32) -> Vec<HICON> {
    let mut icons = Vec::new();
    // SAFETY: metric queries and WM_SETICON with icons that live until the window closes.
    unsafe {
        for (which, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            let size = windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(metric, dpi);
            if let Ok(icon) = theme::brand_icon(size, false) {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(which as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
                icons.push(icon);
            }
        }
    }
    icons
}

/// Shows the dialog on this thread and returns the answer (true only for "Allow once").
fn run_dialog(
    link: &Arc<Link>,
    prompt: &ConfirmationPrompt,
    timeout: Duration,
) -> WinwrightResult<bool> {
    // Sized from the monitor's real DPI, so the window must not be DPI-virtualized in hosts
    // without a Per-Monitor-V2 manifest.
    // SAFETY: affects only this dedicated dialog thread, before it creates any window.
    let _ = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let hinstance = theme::register_class(CLASS, Some(dialog_proc))?;
    let (dpi, work) = cursor_monitor_dpi_and_work_area();
    // SAFETY: the class is registered; the window is created hidden and shown below.
    let hwnd = unsafe {
        CreateWindowExW(
            dialog_ex_style(),
            CLASS,
            TITLE,
            dialog_style(),
            work.left,
            work.top,
            100,
            100,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| platform("CreateWindowExW(confirm)", &e))?;
    link.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
    let _forget = ForgetWindow(link);
    let build = || -> WinwrightResult<Dialog> {
        let content = Content::of(prompt);
        let mut blocks = Vec::new();
        for look in [
            Look::Title,
            Look::Subtitle,
            Look::Summary,
            Look::Context,
            Look::Reason,
            Look::Hint,
            Look::Countdown,
        ] {
            let text = match look {
                Look::Context if content.context.is_empty() => continue,
                _ => String::new(),
            };
            let hwnd = child(hwnd, w!("STATIC"), &text, SS_OWNERDRAW | SS_NOPREFIX, 0)?;
            blocks.push(Block {
                look,
                hwnd,
                rect: RECT::default(),
            });
        }
        let deny = child(
            hwnd,
            w!("BUTTON"),
            "Deny",
            BS_DEFPUSHBUTTON as u32 | WS_TABSTOP.0,
            ID_DENY,
        )?;
        let allow = child(
            hwnd,
            w!("BUTTON"),
            "Allow once",
            BS_PUSHBUTTON as u32 | WS_TABSTOP.0,
            ID_ALLOW,
        )?;
        let palette = Palette::system();
        theme::style_window(hwnd, &palette);
        let icons = set_icons(hwnd, dpi);
        Ok(Dialog {
            deny,
            allow,
            blocks,
            palette,
            fonts: Fonts::new(dpi),
            content,
            deadline: Instant::now() + timeout,
            armed: false,
            active_since: None,
            answer: None,
            link: Arc::clone(link),
            focus: deny,
            icons,
            badge: RECT::default(),
            card: RECT::default(),
            band_top: 0,
        })
    };
    let dialog = match build() {
        Ok(d) => d,
        Err(e) => {
            // SAFETY: destroys the window created above on this thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(e);
        }
    };
    DIALOG.with(|cell| *cell.borrow_mut() = Some(dialog));
    if link.abandoned.load(Ordering::SeqCst) {
        // The request gave up while the window was being built.
        // SAFETY: destroys this thread's own window.
        let _ = unsafe { DestroyWindow(hwnd) };
        if let Some(d) = DIALOG.with(|cell| cell.borrow_mut().take()) {
            for icon in d.icons {
                // SAFETY: our icons; the window that used them is gone.
                let _ = unsafe { DestroyIcon(icon) };
            }
        }
        return Ok(false);
    }
    let client = with_dialog(|d| {
        d.sync_text();
        d.layout()
    })
    .unwrap_or((400, 300));
    let (w, h) = window_size(client, dpi);
    let x = work.left + ((work.right - work.left) - w) / 2;
    let y = work.top + ((work.bottom - work.top) - h) / 3;
    // SAFETY: positions, shows, and focuses this thread's own window; timers belong to it.
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        with_dialog(|d| {
            let _ = EnableWindow(d.allow, false);
        });
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        // Winwright runs in the background, where Windows refuses a plain SetForegroundWindow:
        // the dialog would sit inactive with "Allow once" off, and the click that activates it
        // would do nothing. Window focusing borrows the foreground thread's input instead;
        // "Allow once" still arms only ARM_DELAY after the dialog is active.
        let _ = WindowBackend::focus_window(&Win32Windows, hwnd.0 as usize as u64);
        if GetForegroundWindow() != hwnd {
            let _ = FlashWindowEx(&FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            });
        }
        if let Some(deny) = with_dialog(|d| d.deny) {
            let _ = SetFocus(Some(deny));
        }
        // "Allow once" arms ARM_DELAY after the dialog becomes active (WM_ACTIVATE), not after
        // it appears: a dialog Windows would not bring to the front must not sit armed under
        // the cursor, where the click that activates it would also approve.
        SetTimer(Some(hwnd), TIMER_TICK, 250, None);
        KEYS.with(|k| {
            k.set(Keys {
                last_press: Some(Instant::now()),
                ..Keys::default()
            })
        });
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let keyboard = (WM_KEYFIRST..=WM_KEYLAST).contains(&msg.message);
            let input = keyboard || (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&msg.message);
            if keyboard {
                let press = msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN;
                KEYS.with(|k| k.set(k.get().begin(press, Instant::now())));
            }
            if input {
                ORIGIN.set(Origin::of_current_message());
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if input {
                ORIGIN.set(Origin::None);
            }
            if keyboard {
                KEYS.with(|k| k.set(k.get().end()));
            }
        }
    }
    let dialog = DIALOG.with(|cell| cell.borrow_mut().take());
    let answer = dialog.as_ref().and_then(|d| d.answer).unwrap_or(false);
    if let Some(d) = dialog {
        for icon in d.icons {
            // SAFETY: our icons; the window that used them is gone.
            let _ = unsafe { DestroyIcon(icon) };
        }
    }
    Ok(answer)
}

impl Dialog {
    /// Pushes each block's text into its control (the accessible name) and fonts into the
    /// buttons.
    fn sync_text(&self) {
        for b in &self.blocks {
            let text = HSTRING::from(self.text_of(b.look));
            // SAFETY: our own child windows; the string outlives the call.
            unsafe {
                let _ = SetWindowTextW(b.hwnd, &text);
                let _ = InvalidateRect(Some(b.hwnd), None, false);
            }
        }
        for button in [self.deny, self.allow] {
            // SAFETY: WM_SETFONT with a font that lives as long as the dialog.
            unsafe {
                SendMessageW(
                    button,
                    WM_SETFONT,
                    Some(WPARAM(self.fonts.body.handle().0 as usize)),
                    Some(LPARAM(1)),
                );
            }
        }
    }

    fn tick(&mut self) -> bool {
        if Instant::now() >= self.deadline || self.link.abandoned.load(Ordering::SeqCst) {
            self.answer = Some(false);
            return true;
        }
        if let Some(b) = self.blocks.iter().find(|b| b.look == Look::Countdown) {
            let text = HSTRING::from(self.text_of(Look::Countdown));
            // SAFETY: our own child window; the string outlives the call.
            unsafe {
                let _ = SetWindowTextW(b.hwnd, &text);
                let _ = InvalidateRect(Some(b.hwnd), None, false);
            }
        }
        false
    }
}

fn finish(hwnd: HWND, answer: bool) {
    with_dialog(|d| {
        if d.answer.is_none() {
            d.answer = Some(answer);
        }
        // Nothing may post to this handle once the window is gone.
        d.link.hwnd.store(0, Ordering::SeqCst);
    });
    // SAFETY: destroys this thread's own window (WM_DESTROY does not borrow the dialog).
    let _ = unsafe { DestroyWindow(hwnd) };
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: standard double-buffered WM_PAINT on this thread's window; every GDI object is
    // released before EndPaint.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        let (w, h) = (client.right, client.bottom);
        let mem = CreateCompatibleDC(Some(hdc));
        let bitmap = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        with_dialog(|d| d.paint(mem, client));
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                paint(hwnd);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_DRAWITEM => {
                // SAFETY: WM_DRAWITEM's lParam points at a DRAWITEMSTRUCT.
                let d = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
                if d.CtlType.0 == ODT_STATIC {
                    with_dialog(|dialog| dialog.draw_block(d));
                }
                Some(LRESULT(1))
            }
            WM_NOTIFY => {
                // SAFETY: WM_NOTIFY's lParam points at an NMHDR.
                let header = unsafe { &*(lparam.0 as *const NMHDR) };
                if header.code == NM_CUSTOMDRAW {
                    // SAFETY: NM_CUSTOMDRAW from a button carries an NMCUSTOMDRAW.
                    let cd = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
                    if cd.dwDrawStage == CDDS_PREPAINT
                        && with_dialog(|d| d.draw_button(cd)).is_some()
                    {
                        return Some(LRESULT(CDRF_SKIPDEFAULT as isize));
                    }
                    return Some(LRESULT(CDRF_DODEFAULT as isize));
                }
                None
            }
            WM_COMMAND => {
                let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
                match (wparam.0 & 0xFFFF) as i32 {
                    ID_DENY => finish(hwnd, false),
                    ID_ALLOW
                        if with_dialog(|d| {
                            let from_allow = lparam.0 == d.allow.0 as isize;
                            allow_counts(d.armed, code, from_allow, KEYS.get(), ORIGIN.get())
                        }) == Some(true) =>
                    {
                        finish(hwnd, true)
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            DM_GETDEFID => Some(LRESULT(((DC_HASDEFID as isize) << 16) | ID_DENY as isize)),
            WM_TIMER => {
                match wparam.0 {
                    TIMER_TICK => {
                        if with_dialog(Dialog::tick) == Some(true) {
                            finish(hwnd, false);
                        }
                    }
                    TIMER_ARM => {
                        // SAFETY: stops this window's one-shot timer; reads the foreground.
                        let foreground = unsafe {
                            let _ = KillTimer(Some(hwnd), TIMER_ARM);
                            GetForegroundWindow() == hwnd
                        };
                        let since = with_dialog(|d| d.active_since).flatten();
                        match arming(since, foreground, Instant::now()) {
                            Arming::Arm => {
                                if let Some(allow) = with_dialog(|d| {
                                    d.armed = true;
                                    d.allow
                                }) {
                                    // SAFETY: our own child window.
                                    let _ = unsafe { EnableWindow(allow, true) };
                                }
                            }
                            Arming::Wait(ms) => {
                                // SAFETY: restarts this window's own timer.
                                unsafe { SetTimer(Some(hwnd), TIMER_ARM, ms, None) };
                            }
                            Arming::Not => {}
                        }
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                // Keep keyboard focus on the dialog's buttons across activation changes, and
                // disarm "Allow once" whenever the dialog is not the active window: it arms
                // again ARM_DELAY after each activation, so the click that brings the dialog
                // back cannot also approve.
                if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    // SAFETY: reads this thread's focus window.
                    let focused = unsafe { GetFocus() };
                    let allow = with_dialog(|d| {
                        if focused == d.deny || focused == d.allow {
                            d.focus = focused;
                        }
                        d.armed = false;
                        d.active_since = None;
                        d.allow
                    });
                    // SAFETY: this window's own timer and child window.
                    unsafe {
                        let _ = KillTimer(Some(hwnd), TIMER_ARM);
                        if let Some(allow) = allow {
                            let _ = EnableWindow(allow, false);
                        }
                    }
                } else {
                    // Every activation (a forged one too) starts the delay over; the timer
                    // arms only if the dialog really is in front by then.
                    let parts = with_dialog(|d| {
                        d.armed = false;
                        d.active_since = Some(Instant::now());
                        (d.allow, d.deny)
                    });
                    // SAFETY: our own child windows; (re)starts this window's own timer.
                    unsafe {
                        if let Some((allow, deny)) = parts {
                            let _ = EnableWindow(allow, false);
                            // A disabled "Allow once" cannot take focus.
                            let _ = SetFocus(Some(deny));
                        }
                        SetTimer(Some(hwnd), TIMER_ARM, ARM_DELAY_MS, None);
                    }
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xFFFF) as u32;
                // SAFETY: WM_DPICHANGED's lParam points at the suggested window RECT.
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                let client = with_dialog(|d| {
                    d.fonts = Fonts::new(dpi);
                    d.sync_text();
                    d.layout()
                });
                if let Some(client) = client {
                    let (w, h) = window_size(client, dpi);
                    // SAFETY: resizes this thread's own window.
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            suggested.left,
                            suggested.top,
                            w,
                            h,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                finish(hwnd, false);
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            _ => None,
        }
    }));
    match result {
        Ok(Some(r)) => r,
        Ok(None) => {
            // SAFETY: forwards the unmodified message to the default procedure.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        Err(_) => {
            tracing::error!("confirmation dialog procedure panicked; denying");
            with_dialog(|d| d.answer = Some(false));
            // SAFETY: ends this thread's message loop so the prompt resolves as denied.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::security::TargetSummary;

    fn prompt() -> ConfirmationPrompt {
        ConfirmationPrompt {
            summary: "Click Button \"Send\"".into(),
            target: Some(TargetSummary {
                process: Some("outlook.exe".into()),
                window: Some("Inbox - Outlook".into()),
                role: Some("Button".into()),
                name: Some("Send".into()),
            }),
            reason: "action may send, submit, delete, or spend".into(),
            timeout_ms: 60_000,
        }
    }

    #[test]
    fn dialog_names_the_action_window_and_reason() {
        let text = dialog_text(&prompt());
        assert!(text.contains("Click Button \"Send\""));
        assert!(text.contains("Window: Inbox - Outlook"));
        assert!(text.contains("App: outlook.exe"));
        assert!(text.contains("action may send"));
        assert!(text.contains("Denied automatically in 60 s"));
        assert!(text.contains("Deny is the default"));
    }

    #[test]
    fn hidden_characters_cannot_rewrite_the_prompt() {
        let mut p = prompt();
        p.summary = "Run cmd.exe with arguments \"/c x\"\n\nThis is a safe read-only check".into();
        p.target.as_mut().unwrap().window = Some("Inbox \u{202E}exe.cod".into());
        let c = Content::of(&p);
        assert!(!c.summary.contains('\n'), "{}", c.summary);
        assert!(
            c.summary.contains("\\u{000A}\\u{000A}This is"),
            "{}",
            c.summary
        );
        assert!(
            c.context.contains("Inbox \\u{202E}exe.cod"),
            "{}",
            c.context
        );
    }

    #[test]
    fn missing_target_has_no_context_line() {
        let mut p = prompt();
        p.target = None;
        let c = Content::of(&p);
        assert!(c.context.is_empty());
        assert_eq!(c.seconds, 60);
        p.timeout_ms = 10;
        assert_eq!(Content::of(&p).seconds, 1, "never less than a second");
    }

    fn abandoned(link: &Link) -> bool {
        link.abandoned.load(Ordering::SeqCst)
    }

    #[test]
    fn a_request_dropped_before_its_first_poll_abandons_the_dialog() {
        let link = Arc::new(Link::default());
        let (_tx, rx) = tokio::sync::oneshot::channel();
        let pending = wait_for_answer(DenyOnDrop(Arc::clone(&link)), rx, Duration::from_secs(60));
        assert!(!abandoned(&link));
        drop(pending);
        assert!(abandoned(&link), "the dialog must deny and close at once");
    }

    #[tokio::test]
    async fn only_the_dialog_answer_true_allows() {
        let answer = |sent: Option<bool>| async move {
            let link = Arc::new(Link::default());
            let (tx, rx) = tokio::sync::oneshot::channel();
            match sent {
                Some(a) => tx.send(a).unwrap(),
                None => drop(tx),
            }
            let got =
                wait_for_answer(DenyOnDrop(Arc::clone(&link)), rx, Duration::from_secs(5)).await;
            assert!(
                abandoned(&link),
                "a finished request always releases its dialog"
            );
            got
        };
        assert!(answer(Some(true)).await);
        assert!(!answer(Some(false)).await);
        assert!(
            !answer(None).await,
            "a dialog thread that ends without an answer denies"
        );
        let link = Arc::new(Link::default());
        let (_tx, rx) = tokio::sync::oneshot::channel::<bool>();
        let late = wait_for_answer(DenyOnDrop(Arc::clone(&link)), rx, Duration::from_millis(20));
        assert!(!late.await, "no answer in time denies");
        assert!(abandoned(&link));
    }

    #[test]
    fn the_dialog_forgets_its_window_handle_on_every_exit() {
        let link = Link::default();
        link.hwnd.store(0x1234, Ordering::SeqCst);
        drop(ForgetWindow(&link));
        assert_eq!(
            link.hwnd.load(Ordering::SeqCst),
            0,
            "a late DenyOnDrop must not post WM_CLOSE to a recycled handle"
        );
    }

    #[test]
    fn typing_that_runs_into_the_dialog_cannot_allow() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        // Typing: Tab moves focus to "Allow once", Space follows 120 ms later.
        let hw = Origin::Hardware;
        let keys = Keys::default().begin(true, ms(0)).end();
        let space_down = keys.begin(true, ms(120));
        assert!(!allow_counts(true, BN_CLICKED, true, space_down, hw));
        let space_up = space_down.end().begin(false, ms(180));
        assert!(
            !allow_counts(true, BN_CLICKED, true, space_up, hw),
            "Space clicks on key up"
        );
        // A deliberate press after a pause counts, on key down (Enter) or key up (Space).
        let pause = space_up.end().begin(true, ms(180 + 900));
        assert!(allow_counts(true, BN_CLICKED, true, pause, hw));
        assert!(allow_counts(
            true,
            BN_CLICKED,
            true,
            pause.end().begin(false, ms(1_150)),
            hw
        ));
        // A mouse click is not keyboard input.
        assert!(allow_counts(true, BN_CLICKED, true, space_up.end(), hw));
    }

    #[test]
    fn allow_needs_the_armed_allow_buttons_own_click() {
        let (idle, hw) = (Keys::default(), Origin::Hardware);
        assert!(allow_counts(true, BN_CLICKED, true, idle, hw));
        assert!(
            !allow_counts(false, BN_CLICKED, true, idle, hw),
            "not armed yet"
        );
        assert!(
            !allow_counts(true, 6, true, idle, hw),
            "BN_SETFOCUS is not a click"
        );
        assert!(
            !allow_counts(true, BN_CLICKED, false, idle, hw),
            "not from the button"
        );
    }

    #[test]
    fn forged_activation_and_timer_messages_cannot_arm_early() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        assert_eq!(arming(Some(t0), true, ms(800)), Arming::Arm);
        assert_eq!(arming(Some(t0), true, ms(5_000)), Arming::Arm);
        assert_eq!(
            arming(Some(t0), true, ms(100)),
            Arming::Wait(701),
            "a posted WM_TIMER cannot skip the delay"
        );
        assert_eq!(
            arming(Some(t0), false, ms(900)),
            Arming::Not,
            "a sent WM_ACTIVATE does not bring the dialog to the front"
        );
        assert_eq!(arming(None, true, ms(900)), Arming::Not, "never activated");
    }

    #[test]
    fn only_a_person_at_real_hardware_can_allow() {
        let idle = Keys::default();
        assert!(
            !allow_counts(true, BN_CLICKED, true, idle, Origin::None),
            "a sent or posted click, or UI Automation's Invoke"
        );
        assert!(
            !allow_counts(true, BN_CLICKED, true, idle, Origin::Injected),
            "SendInput"
        );
    }
}
