//! The Settings window, opened only from the tray menu: theme, how long a question waits,
//! "allow in an app for a while", the activity log, task-done notices and the update check.
//!
//! - It belongs to winwright.exe, which the engine refuses to automate, and on top of that every
//!   change and Save count only from a real keyboard, mouse, pen or touch screen: a program
//!   clicking it (SendInput, BM_CLICK, UI Automation's Invoke) changes nothing.
//! - Every control is a real button, radio button or check box (custom-drawn), and every piece
//!   of text an owner-drawn STATIC, so screen readers read it while it keeps the Winwright look.
//! - One window at a time: opening it again brings the open one to the front.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DT_CENTER, DT_LEFT, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject, EndPaint, GetDC, HDC, HGDIOBJ,
    InvalidateRect, PAINTSTRUCT, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::UI::Controls::{
    CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CDRF_DODEFAULT,
    CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::{GetCurrentInputMessageSource, IMO_HARDWARE, INPUT_MESSAGE_SOURCE};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_SETCHECK, BN_CLICKED, BS_CHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, BS_RADIOBUTTON,
    CreateWindowExW, DC_HASDEFID, DM_GETDEFID, DefWindowProcW, DestroyIcon, DestroyWindow,
    DispatchMessageW, GetClientRect, GetMessageW, HICON, HMENU, IDCANCEL, IDOK, InSendMessageEx,
    IsDialogMessageW, IsIconic, MSG, PostQuitMessage, SW_RESTORE, SW_SHOWNORMAL, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOZORDER, SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_KEYFIRST, WM_KEYLAST, WM_MOUSEFIRST,
    WM_MOUSELAST, WM_NOTIFY, WM_PAINT, WM_SETFONT, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_APPWINDOW, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};
use winwright_contracts::WinwrightResult;
use winwright_contracts::backend::WindowBackend;
use winwright_contracts::config::ThemeChoice;
use winwright_win32::Win32Windows;

use crate::platform;
use crate::theme::{self, ButtonKind, ButtonState, Fonts, Palette, glyph};

const CLASS: PCWSTR = w!("WinwrightSettings");
const TITLE: PCWSTR = w!("Winwright settings");
const WIDTH: i32 = 500;
const SS_OWNERDRAW: u32 = 0x0D;
const SS_NOPREFIX: u32 = 0x80;
const ODT_STATIC: u32 = 5;
const ID_SAVE: i32 = IDOK.0;
/// IDCANCEL, so Esc closes through IsDialogMessage.
const ID_CANCEL: i32 = IDCANCEL.0;
const ID_THEME: i32 = 100;
const ID_SHORTER: i32 = 110;
const ID_LONGER: i32 = 111;
const ID_SWITCH: i32 = 120;

/// Seconds a question can wait, as offered by the − and + buttons.
const WAITS: [u64; 9] = [15, 30, 45, 60, 90, 120, 180, 300, 600];
const THEMES: [(ThemeChoice, &str); 3] = [
    (ThemeChoice::System, "System"),
    (ThemeChoice::Light, "Light"),
    (ThemeChoice::Dark, "Dark"),
];
const NOTE: &str = "Changes apply when your AI apps next start Winwright. To apply them now, \
                    restart those apps.";

/// What the window edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub confirmation_timeout_seconds: u64,
    pub allow_for_a_while: bool,
    pub audit: bool,
    pub task_done: bool,
    pub update_check: bool,
}

/// Saves the settings (called on the window's thread); an error is shown in the window.
pub type SaveSettings = Box<dyn Fn(&Settings) -> Result<(), String> + Send>;
type SharedSave = Arc<dyn Fn(&Settings) -> Result<(), String> + Send>;

/// The on/off rows, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Switch {
    AllowForAWhile,
    Audit,
    TaskDone,
    UpdateCheck,
}

const SWITCHES: [Switch; 4] = [
    Switch::AllowForAWhile,
    Switch::Audit,
    Switch::TaskDone,
    Switch::UpdateCheck,
];

impl Switch {
    fn label(self) -> &'static str {
        match self {
            Self::AllowForAWhile => "Allow in an app for a while",
            Self::Audit => "Activity log",
            Self::TaskDone => "Task-done notices",
            Self::UpdateCheck => "Check for updates",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::AllowForAWhile => {
                "A question can offer to let one app go on for 10 minutes. Risky actions still ask."
            }
            Self::Audit => "Keep a record on this PC of what AI apps did, never what they typed.",
            Self::TaskDone => "Show a notification when an AI app finishes a task.",
            Self::UpdateCheck => "Once a day, ask GitHub whether a newer Winwright is out.",
        }
    }

    fn get(self, s: &Settings) -> bool {
        match self {
            Self::AllowForAWhile => s.allow_for_a_while,
            Self::Audit => s.audit,
            Self::TaskDone => s.task_done,
            Self::UpdateCheck => s.update_check,
        }
    }

    fn flip(self, s: &mut Settings) {
        let value = match self {
            Self::AllowForAWhile => &mut s.allow_for_a_while,
            Self::Audit => &mut s.audit,
            Self::TaskDone => &mut s.task_done,
            Self::UpdateCheck => &mut s.update_check,
        };
        *value = !*value;
    }
}

/// The next offered wait after `current` (`longer`) or before it; unchanged at either end.
fn step_wait(current: u64, longer: bool) -> u64 {
    let next = if longer {
        WAITS.iter().find(|&&w| w > current)
    } else {
        WAITS.iter().rev().find(|&&w| w < current)
    };
    next.copied().unwrap_or(current)
}

/// `45 seconds`, `1 minute`, `5 minutes`.
fn wait_text(seconds: u64) -> String {
    match seconds {
        60 => "1 minute".into(),
        s if s % 60 == 0 => format!("{} minutes", s / 60),
        s => format!("{s} seconds"),
    }
}

/// The open window, so a second "Settings" click brings it forward instead.
static OPEN: AtomicIsize = AtomicIsize::new(0);

/// Opens the Settings window on a thread of its own, or brings the open one to the front.
pub fn open_settings(initial: Settings, save: SaveSettings) {
    let open = OPEN.load(Ordering::SeqCst);
    if open != 0 {
        let hwnd = HWND(open as *mut _);
        // SAFETY: plain window calls; a handle that just closed makes them fail harmlessly.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
        }
        let _ = WindowBackend::focus_window(&Win32Windows, open as usize as u64);
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("winwright-settings".into())
        .spawn(move || {
            if let Err(err) = run(initial, Arc::from(save)) {
                tracing::warn!(%err, "settings window unavailable");
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "cannot start the settings window");
    }
}

// ---------------------------------------------------------------------------------------
// The window

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    Title,
    Subtitle,
    Label,
    Description,
    Value,
    Note,
    Error,
}

struct Block {
    look: Look,
    hwnd: HWND,
    text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Theme(ThemeChoice),
    Shorter,
    Longer,
    Switch(Switch),
    Save,
    Cancel,
}

struct Button {
    control: Control,
    hwnd: HWND,
}

struct Window {
    settings: Settings,
    save: SharedSave,
    blocks: Vec<Block>,
    buttons: Vec<Button>,
    palette: Palette,
    fonts: Fonts,
    icons: Vec<HICON>,
    badge: RECT,
    /// Lines above each row, and where the button band starts.
    rules: Vec<i32>,
    band_top: i32,
}

thread_local! {
    static WINDOW: RefCell<Option<Window>> = const { RefCell::new(None) };
    /// The input message being handled came from real hardware.
    static HARDWARE: Cell<bool> = const { Cell::new(false) };
}

fn with_window<R>(f: impl FnOnce(&mut Window) -> R) -> Option<R> {
    WINDOW.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

fn rect(left: i32, top: i32, width: i32, height: i32) -> RECT {
    RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

fn place(hwnd: HWND, r: RECT) {
    // SAFETY: moves one of this window's own children.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

impl Window {
    fn font(&self, look: Look) -> &theme::Font {
        match look {
            Look::Title => &self.fonts.title,
            Look::Label => &self.fonts.strong,
            Look::Value => &self.fonts.body,
            Look::Subtitle | Look::Description | Look::Note | Look::Error => &self.fonts.small,
        }
    }

    /// Foreground and background of a text block.
    fn colors(&self, look: Look) -> (u32, u32) {
        let p = &self.palette;
        match look {
            Look::Title | Look::Label | Look::Value => (p.text, p.surface),
            Look::Subtitle | Look::Description => (p.muted, p.surface),
            Look::Note => (p.muted, p.window),
            Look::Error => (p.bad, p.window),
        }
    }

    fn button(&self, control: Control) -> Option<HWND> {
        self.buttons
            .iter()
            .find(|b| b.control == control)
            .map(|b| b.hwnd)
    }

    fn block_mut(&mut self, look: Look) -> Option<&mut Block> {
        self.blocks.iter_mut().find(|b| b.look == look)
    }

    /// Puts each block's text into its control (its accessible name), the check marks into
    /// the radio buttons and check boxes, and the body font into every button.
    fn sync(&self) {
        for b in &self.blocks {
            let text = HSTRING::from(b.text.as_str());
            // SAFETY: our own child windows; the string outlives the call.
            unsafe {
                let _ = SetWindowTextW(b.hwnd, &text);
                let _ = InvalidateRect(Some(b.hwnd), None, false);
            }
        }
        for b in &self.buttons {
            let checked = match b.control {
                Control::Theme(t) => Some(t == self.settings.theme),
                Control::Switch(s) => Some(s.get(&self.settings)),
                _ => None,
            };
            // SAFETY: messages to our own child windows with plain values; the font lives as
            // long as the window.
            unsafe {
                if let Some(checked) = checked {
                    SendMessageW(
                        b.hwnd,
                        BM_SETCHECK,
                        Some(WPARAM(usize::from(checked))),
                        None,
                    );
                }
                SendMessageW(
                    b.hwnd,
                    WM_SETFONT,
                    Some(WPARAM(self.fonts.body.handle().0 as usize)),
                    Some(LPARAM(0)),
                );
                let _ = InvalidateRect(Some(b.hwnd), None, false);
            }
        }
    }

    /// Positions everything for the current DPI; returns the client size.
    fn layout(&mut self) -> (i32, i32) {
        let px = |v| theme::scale(v, self.fonts.dpi);
        let (pad, width) = (px(24), px(WIDTH));
        let inner = width - pad * 2;
        // SAFETY: a screen DC borrowed for measuring only.
        let dc = unsafe { GetDC(None) };
        let height = |font: &theme::Font, text: &str, w: i32| {
            theme::measure(dc, font, text, Some(w), DT_WORDBREAK).1
        };
        let mut places: Vec<(HWND, RECT)> = Vec::new();
        // Blocks are taken in the order `build` made them.
        let mut blocks = self.blocks.iter();
        let mut next_block = || {
            blocks
                .next()
                .map_or((HWND::default(), ""), |b| (b.hwnd, b.text.as_str()))
        };
        // Header: badge, title and subtitle.
        let badge = px(40);
        let text_x = pad + badge + px(14);
        let text_w = width - pad - text_x;
        let (title, title_text) = next_block();
        let (subtitle, subtitle_text) = next_block();
        let title_h = height(&self.fonts.title, title_text, text_w);
        let sub_h = height(&self.fonts.small, subtitle_text, text_w);
        let header_h = (title_h + px(2) + sub_h).max(badge);
        let text_top = pad + (header_h - title_h - px(2) - sub_h) / 2;
        places.push((title, rect(text_x, text_top, text_w, title_h)));
        places.push((
            subtitle,
            rect(text_x, text_top + title_h + px(2), text_w, sub_h),
        ));
        self.badge = rect(pad, pad + (header_h - badge) / 2, badge, badge);
        let mut y = pad + header_h + px(16);
        // Rows: label and description on the left, the control on the right.
        let control_h = px(32);
        let seg_w = px(76);
        let switch_w = px(92);
        let step_w = px(32);
        let value_w = px(96);
        self.rules.clear();
        let mut buttons = self.buttons.iter();
        for row in 0..2 + SWITCHES.len() {
            self.rules.push(y);
            y += px(14);
            let control_w = match row {
                0 => seg_w * 3 + px(4) * 2,
                1 => step_w * 2 + value_w,
                _ => switch_w,
            };
            let text_w = inner - control_w - px(16);
            let (label, label_text) = next_block();
            let (description, description_text) = next_block();
            let label_h = height(&self.fonts.strong, label_text, text_w);
            let description_h = height(&self.fonts.small, description_text, text_w);
            let text_h = label_h + px(2) + description_h;
            let row_h = text_h.max(control_h);
            let top = y + (row_h - text_h) / 2;
            places.push((label, rect(pad, top, text_w, label_h)));
            places.push((
                description,
                rect(pad, top + label_h + px(2), text_w, description_h),
            ));
            let (mut x, cy) = (pad + inner - control_w, y + (row_h - control_h) / 2);
            match row {
                0 => {
                    for _ in THEMES {
                        if let Some(b) = buttons.next() {
                            places.push((b.hwnd, rect(x, cy, seg_w, control_h)));
                        }
                        x += seg_w + px(4);
                    }
                }
                1 => {
                    if let Some(b) = buttons.next() {
                        places.push((b.hwnd, rect(x, cy, step_w, control_h)));
                    }
                    places.push((next_block().0, rect(x + step_w, cy, value_w, control_h)));
                    if let Some(b) = buttons.next() {
                        places.push((b.hwnd, rect(x + step_w + value_w, cy, step_w, control_h)));
                    }
                }
                _ => {
                    if let Some(b) = buttons.next() {
                        places.push((b.hwnd, rect(x, cy, switch_w, control_h)));
                    }
                }
            }
            y += row_h + px(14);
        }
        // The band: the note (or the error) beside Cancel and Save.
        y += px(8);
        self.band_top = y;
        let (button_w, button_h) = (px(104), px(36));
        let note_w = inner - button_w * 2 - px(6) - px(16);
        let (note, note_text) = next_block();
        let note_h = height(&self.fonts.small, note_text, note_w);
        let band_h = (note_h + px(32)).max(px(68));
        places.push((note, rect(pad, y + (band_h - note_h) / 2, note_w, note_h)));
        let button_y = y + (band_h - button_h) / 2;
        let save_x = width - pad + px(2) - button_w;
        for (control, x) in [
            (Control::Cancel, save_x - px(6) - button_w),
            (Control::Save, save_x),
        ] {
            if let Some(hwnd) = self.button(control) {
                places.push((hwnd, rect(x, button_y, button_w, button_h)));
            }
        }
        // SAFETY: releases the DC borrowed above.
        unsafe { ReleaseDC(None, dc) };
        for (hwnd, r) in places {
            place(hwnd, r);
        }
        (width, y + band_h)
    }

    fn paint(&self, hdc: HDC, client: RECT) {
        let p = &self.palette;
        let px = |v| self.fonts.px(v);
        theme::fill(
            hdc,
            RECT {
                bottom: self.band_top,
                ..client
            },
            p.surface,
        );
        theme::fill(
            hdc,
            RECT {
                top: self.band_top,
                ..client
            },
            p.window,
        );
        // A line between rows, and one across the top of the band.
        let rows = self.rules.iter().skip(1).map(|&y| (y, px(24)));
        for (y, inset) in rows.chain([(self.band_top, 0)]) {
            theme::fill(
                hdc,
                RECT {
                    left: client.left + inset,
                    right: client.right - inset,
                    top: y,
                    bottom: y + 1,
                },
                p.border,
            );
        }
        theme::dot(hdc, self.badge, p.selection);
        theme::icon(
            hdc,
            &self.fonts.icons,
            glyph::SETTINGS,
            self.badge,
            p.accent,
        );
    }

    fn draw_block(&self, d: &DRAWITEMSTRUCT) {
        let Some(block) = self.blocks.iter().find(|b| b.hwnd == d.hwndItem) else {
            return;
        };
        let (fg, bg) = self.colors(block.look);
        theme::fill(d.hDC, d.rcItem, bg);
        let flags = if block.look == Look::Value {
            DT_SINGLELINE | DT_VCENTER | DT_CENTER
        } else {
            DT_WORDBREAK | DT_LEFT
        };
        theme::text(
            d.hDC,
            self.font(block.look),
            &block.text,
            d.rcItem,
            fg,
            flags,
        );
    }

    fn draw_button(&self, cd: &NMCUSTOMDRAW) -> bool {
        let Some(button) = self.buttons.iter().find(|b| b.hwnd == cd.hdr.hwndFrom) else {
            return false;
        };
        let state = ButtonState {
            hot: cd.uItemState.0 & CDIS_HOT.0 != 0,
            pressed: cd.uItemState.0 & CDIS_SELECTED.0 != 0,
            focused: cd.uItemState.0 & CDIS_FOCUS.0 != 0,
            disabled: cd.uItemState.0 & CDIS_DISABLED.0 != 0,
        };
        let (p, f) = (&self.palette, &self.fonts);
        let (backdrop, kind, label, glyph_char) = match button.control {
            Control::Theme(t) => {
                let name = THEMES.iter().find(|(c, _)| *c == t).map_or("", |(_, n)| n);
                let kind = if t == self.settings.theme {
                    ButtonKind::Primary
                } else {
                    ButtonKind::Secondary
                };
                (p.surface, kind, name, None)
            }
            Control::Shorter => (p.surface, ButtonKind::Subtle, "", Some(glyph::REMOVE)),
            Control::Longer => (p.surface, ButtonKind::Subtle, "", Some(glyph::ADD)),
            Control::Switch(s) => {
                draw_switch(cd.hdc, cd.rc, s.get(&self.settings), state, p, f);
                return true;
            }
            Control::Save => (p.window, ButtonKind::Primary, "Save", None),
            Control::Cancel => (p.window, ButtonKind::Secondary, "Cancel", None),
        };
        theme::draw_button(
            cd.hdc, cd.rc, backdrop, kind, state, label, glyph_char, p, f,
        );
        true
    }
}

/// "On" or "Off" beside a switch, right-aligned in `r`.
fn draw_switch(hdc: HDC, r: RECT, on: bool, state: ButtonState, p: &Palette, f: &Fonts) {
    theme::fill(hdc, r, p.surface);
    let (w, h) = (f.px(40), f.px(20));
    let ring = f.px(2);
    let track = RECT {
        left: r.right - ring - w,
        top: r.top + (r.bottom - r.top - h) / 2,
        right: r.right - ring,
        bottom: r.top + (r.bottom - r.top - h) / 2 + h,
    };
    let radius = h as f32 / 2.0;
    let (fill, border, knob) = match (on, state.hot || state.pressed) {
        (true, false) => (p.accent, None, p.on_accent),
        (true, true) => (p.accent_hover, None, p.on_accent),
        (false, hot) => (
            if hot { p.hover } else { p.surface },
            Some(p.muted),
            p.muted,
        ),
    };
    theme::rounded(hdc, track, radius, fill, border);
    let k = f.px(12);
    let kx = if on {
        track.right - f.px(4) - k
    } else {
        track.left + f.px(4)
    };
    let ky = track.top + (h - k) / 2;
    theme::dot(hdc, rect(kx, ky, k, k), knob);
    if state.focused {
        let outer = RECT {
            left: track.left - ring,
            top: track.top - ring,
            right: track.right + ring,
            bottom: track.bottom + ring,
        };
        theme::ring(hdc, outer, radius + ring as f32, ring as f32, p.accent);
    }
    theme::text(
        hdc,
        &f.body,
        if on { "On" } else { "Off" },
        RECT {
            right: track.left - f.px(10),
            ..r
        },
        if state.disabled { p.faint } else { p.text },
        DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
    );
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
    .map_err(|e| platform("CreateWindowExW(settings child)", &e))
}

fn window_style() -> WINDOW_STYLE {
    WINDOW_STYLE(
        WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0,
    )
}

fn window_size(client: (i32, i32), dpi: u32) -> (i32, i32) {
    let mut r = rect(0, 0, client.0, client.1);
    // SAFETY: `r` is a local RECT adjusted in place.
    let _ =
        unsafe { AdjustWindowRectExForDpi(&mut r, window_style(), false, WS_EX_APPWINDOW, dpi) };
    (r.right - r.left, r.bottom - r.top)
}

/// Builds every child control of `hwnd`, in layout order.
fn build(hwnd: HWND, settings: &Settings) -> WinwrightResult<(Vec<Block>, Vec<Button>)> {
    let static_style = SS_OWNERDRAW | SS_NOPREFIX;
    let mut texts = vec![
        (Look::Title, "Settings".to_owned()),
        (
            Look::Subtitle,
            "Only you can change these. AI apps cannot.".to_owned(),
        ),
        (Look::Label, "Theme".to_owned()),
        (
            Look::Description,
            "How Winwright's own windows look.".to_owned(),
        ),
        (Look::Label, "Wait for an answer".to_owned()),
        (
            Look::Description,
            "How long a question waits for you before it counts as No.".to_owned(),
        ),
        (
            Look::Value,
            wait_text(settings.confirmation_timeout_seconds),
        ),
    ];
    for s in SWITCHES {
        texts.push((Look::Label, s.label().to_owned()));
        texts.push((Look::Description, s.description().to_owned()));
    }
    texts.push((Look::Note, NOTE.to_owned()));
    // Layout takes the value block right after the second row's description: keep it there.
    let mut blocks = Vec::new();
    for (look, text) in texts {
        blocks.push(Block {
            look,
            hwnd: child(hwnd, w!("STATIC"), &text, static_style, 0)?,
            text,
        });
    }
    let mut buttons = Vec::new();
    let mut add = |control, text: &str, style: u32, id| -> WinwrightResult<()> {
        buttons.push(Button {
            control,
            hwnd: child(hwnd, w!("BUTTON"), text, style | WS_TABSTOP.0, id)?,
        });
        Ok(())
    };
    for (i, (theme, name)) in THEMES.iter().enumerate() {
        add(
            Control::Theme(*theme),
            name,
            BS_RADIOBUTTON as u32,
            ID_THEME + i as i32,
        )?;
    }
    add(
        Control::Shorter,
        "Shorter wait",
        BS_PUSHBUTTON as u32,
        ID_SHORTER,
    )?;
    add(
        Control::Longer,
        "Longer wait",
        BS_PUSHBUTTON as u32,
        ID_LONGER,
    )?;
    for (i, s) in SWITCHES.iter().enumerate() {
        add(
            Control::Switch(*s),
            s.label(),
            BS_CHECKBOX as u32,
            ID_SWITCH + i as i32,
        )?;
    }
    add(Control::Cancel, "Cancel", BS_PUSHBUTTON as u32, ID_CANCEL)?;
    add(Control::Save, "Save", BS_DEFPUSHBUTTON as u32, ID_SAVE)?;
    Ok((blocks, buttons))
}

/// Shows the window on this thread until it closes.
fn run(settings: Settings, save: SharedSave) -> WinwrightResult<()> {
    // SAFETY: affects only this dedicated thread, before it creates any window.
    let _ = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let hinstance = theme::register_class(CLASS, Some(window_proc))?;
    let (dpi, work) = crate::confirm::cursor_monitor_dpi_and_work_area();
    // SAFETY: the class is registered; the window is created hidden and shown below.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_APPWINDOW,
            CLASS,
            TITLE,
            window_style(),
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
    .map_err(|e| platform("CreateWindowExW(settings)", &e))?;
    let (blocks, buttons) = match build(hwnd, &settings) {
        Ok(parts) => parts,
        Err(e) => {
            // SAFETY: destroys the window created above on this thread.
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(e);
        }
    };
    let palette = Palette::system();
    theme::style_window(hwnd, &palette);
    let icons = crate::confirm::set_icons(hwnd, dpi);
    WINDOW.with(|cell| {
        *cell.borrow_mut() = Some(Window {
            settings,
            save,
            blocks,
            buttons,
            palette,
            fonts: Fonts::new(dpi),
            icons,
            badge: RECT::default(),
            rules: Vec::new(),
            band_top: 0,
        })
    });
    OPEN.store(hwnd.0 as isize, Ordering::SeqCst);
    let client = with_window(|w| {
        w.sync();
        w.layout()
    })
    .unwrap_or((500, 600));
    let (w, h) = window_size(client, dpi);
    let x = work.left + ((work.right - work.left) - w) / 2;
    let y = work.top + ((work.bottom - work.top) - h).max(0) / 3;
    // SAFETY: positions, shows and focuses this thread's own window.
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    }
    // Opened from the tray menu: the person's click lets it come to the front.
    let _ = WindowBackend::focus_window(&Win32Windows, hwnd.0 as usize as u64);
    if let Some(save) = with_window(|w| w.button(Control::Save)).flatten() {
        // SAFETY: our own child window.
        let _ = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(save)) };
    }
    let mut msg = MSG::default();
    // SAFETY: a standard message loop for this thread's window.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let input = (WM_KEYFIRST..=WM_KEYLAST).contains(&msg.message)
                || (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&msg.message);
            if input {
                HARDWARE.set(from_hardware());
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if input {
                HARDWARE.set(false);
            }
        }
    }
    OPEN.store(0, Ordering::SeqCst);
    if let Some(w) = WINDOW.with(|cell| cell.borrow_mut().take()) {
        for icon in w.icons {
            // SAFETY: our icons; the window that used them is gone.
            let _ = unsafe { DestroyIcon(icon) };
        }
    }
    Ok(())
}

/// The input message this thread just retrieved came from a real keyboard, mouse, pen or touch
/// screen (not SendInput, so not Winwright's own input either).
fn from_hardware() -> bool {
    let mut source = INPUT_MESSAGE_SOURCE::default();
    // SAFETY: fills the struct we pass.
    matches!(unsafe { GetCurrentInputMessageSource(&mut source) }, Ok(()) if source.originId == IMO_HARDWARE)
}

/// Lays the window out again (its text changed height) and keeps its top-left corner.
fn relayout(hwnd: HWND) {
    let Some((client, dpi)) = with_window(|w| {
        w.sync();
        (w.layout(), w.fonts.dpi)
    }) else {
        return;
    };
    let (w, h) = window_size(client, dpi);
    // SAFETY: resizes and repaints this thread's own window.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE,
        );
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// A click on one of the controls, made by a person at real hardware.
fn clicked(hwnd: HWND, id: i32) {
    let control = with_window(|w| {
        w.buttons
            .iter()
            .find(|b| {
                // SAFETY: reads the id of our own child window.
                (unsafe { windows::Win32::UI::WindowsAndMessaging::GetDlgCtrlID(b.hwnd) }) == id
            })
            .map(|b| b.control)
    })
    .flatten();
    match control {
        Some(Control::Theme(t)) => {
            with_window(|w| w.settings.theme = t);
        }
        Some(Control::Shorter | Control::Longer) => {
            let longer = control == Some(Control::Longer);
            with_window(|w| {
                let s = &mut w.settings;
                s.confirmation_timeout_seconds = step_wait(s.confirmation_timeout_seconds, longer);
                let text = wait_text(s.confirmation_timeout_seconds);
                if let Some(b) = w.block_mut(Look::Value) {
                    b.text = text;
                }
            });
        }
        Some(Control::Switch(s)) => {
            with_window(|w| s.flip(&mut w.settings));
        }
        Some(Control::Save) => {
            let Some((settings, save)) = with_window(|w| (w.settings.clone(), Arc::clone(&w.save)))
            else {
                return;
            };
            match save(&settings) {
                Ok(()) => {
                    // SAFETY: destroys this thread's own window.
                    let _ = unsafe { DestroyWindow(hwnd) };
                    return;
                }
                Err(err) => {
                    with_window(|w| {
                        if let Some(b) = w.blocks.iter_mut().find(|b| b.look == Look::Note) {
                            b.look = Look::Error;
                        }
                        if let Some(b) = w.block_mut(Look::Error) {
                            b.text = format!("Not saved: {err}");
                        }
                    });
                    relayout(hwnd);
                    return;
                }
            }
        }
        Some(Control::Cancel) | None => return,
    }
    with_window(|w| w.sync());
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: standard double-buffered WM_PAINT on this thread's window; every GDI object is
    // released before EndPaint.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let (w, h) = (client.right, client.bottom);
        let mem = CreateCompatibleDC(Some(hdc));
        let bitmap = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        with_window(|win| win.paint(mem, client));
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn window_proc(
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
                    with_window(|w| w.draw_block(d));
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
                        && with_window(|w| w.draw_button(cd)) == Some(true)
                    {
                        return Some(LRESULT(CDRF_SKIPDEFAULT as isize));
                    }
                    return Some(LRESULT(CDRF_DODEFAULT as isize));
                }
                None
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as i32;
                let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
                if id == ID_CANCEL {
                    // Closing changes nothing, whoever asks.
                    // SAFETY: destroys this thread's own window.
                    let _ = unsafe { DestroyWindow(hwnd) };
                } else if code == BN_CLICKED {
                    // A click another thread sent inherits nothing: a real click is never
                    // inside another thread's send.
                    // SAFETY: no arguments; reads this thread's message state.
                    let sent = unsafe { InSendMessageEx(None) } != 0;
                    if HARDWARE.get() && !sent {
                        clicked(hwnd, id);
                    }
                }
                Some(LRESULT(0))
            }
            DM_GETDEFID => Some(LRESULT(((DC_HASDEFID as isize) << 16) | ID_SAVE as isize)),
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xFFFF) as u32;
                // SAFETY: WM_DPICHANGED's lParam points at the suggested window RECT.
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                let client = with_window(|w| {
                    w.fonts = Fonts::new(dpi);
                    w.sync();
                    w.layout()
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
                // SAFETY: destroys this thread's own window.
                let _ = unsafe { DestroyWindow(hwnd) };
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                OPEN.store(0, Ordering::SeqCst);
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
            tracing::error!("settings window procedure panicked; closing it");
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_step_through_the_offered_values() {
        assert_eq!(step_wait(45, true), 60);
        assert_eq!(step_wait(45, false), 30);
        assert_eq!(step_wait(600, true), 600, "the longest stays");
        assert_eq!(step_wait(15, false), 15, "the shortest stays");
        // A value set by hand moves to its neighbors.
        assert_eq!(step_wait(50, true), 60);
        assert_eq!(step_wait(50, false), 45);
        assert_eq!(step_wait(5, true), 15);
    }

    #[test]
    fn waits_read_as_words() {
        assert_eq!(wait_text(45), "45 seconds");
        assert_eq!(wait_text(60), "1 minute");
        assert_eq!(wait_text(300), "5 minutes");
        assert_eq!(wait_text(90), "90 seconds");
    }

    fn start() -> Settings {
        Settings {
            theme: ThemeChoice::System,
            confirmation_timeout_seconds: 45,
            allow_for_a_while: true,
            audit: true,
            task_done: true,
            update_check: false,
        }
    }

    #[test]
    #[ignore = "live: shows the Settings window on the desktop for a moment"]
    fn live_programs_cannot_change_or_save_settings() {
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        use windows::Win32::UI::WindowsAndMessaging::{BM_CLICK, GetDlgItem, PostMessageW};
        let saved = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&saved);
        open_settings(
            start(),
            Box::new(move |_| {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while OPEN.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let hwnd = HWND(OPEN.load(Ordering::SeqCst) as *mut _);
        assert!(!hwnd.is_invalid(), "the window opened");
        // SAFETY: messages to the test's own window from another thread, as a program would.
        unsafe {
            for id in [ID_SWITCH, ID_SAVE] {
                let button = GetDlgItem(Some(hwnd), id).unwrap();
                SendMessageW(button, BM_CLICK, None, None);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !saved.load(Ordering::SeqCst),
            "a sent click saved the settings"
        );
        // SAFETY: as above.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        while OPEN.load(Ordering::SeqCst) != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(OPEN.load(Ordering::SeqCst), 0, "closing works for anyone");
    }

    #[test]
    fn switches_flip_only_their_own_setting() {
        let start = start();
        for s in SWITCHES {
            let mut changed = start.clone();
            s.flip(&mut changed);
            assert_ne!(s.get(&changed), s.get(&start));
            for other in SWITCHES.iter().filter(|o| **o != s) {
                assert_eq!(
                    other.get(&changed),
                    other.get(&start),
                    "{s:?} moved {other:?}"
                );
            }
        }
    }
}
