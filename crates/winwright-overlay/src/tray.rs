//! Notification-area (tray) icon on the native UI thread (spec §11 plan: native, no WebView).
//! Present only while the owning process runs; removed on shutdown so no stale icon remains.
//!
//! The icon is the Winwright mark rendered at the exact small-icon size for the system DPI.
//! Left or right click opens one menu: a header with the live status, then the actions (each
//! with a Fluent glyph). The keyboard works too (NOTIFYICON_VERSION_4).
//!
//! Every menu row is owner-drawn in the shared theme (a menu with any owner-drawn row loses
//! the system theme anyway), so it follows light/dark mode; Windows 11 rounds its corners.
//! Rows keep their text for screen readers.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DRAW_TEXT_FORMAT, DT_BOTTOM, DT_END_ELLIPSIS, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, GetDC, HBRUSH, HGDIOBJ, ReleaseDC,
};
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_DISABLED, ODS_SELECTED, ODT_MENU,
};
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_LARGE_ICON,
    NIIF_RESPECT_QUIET_TIME, NIIF_USER, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NINF_KEY, NOTIFY_ICON_MESSAGE, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    NOTIFYICONDATAW_0, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
    GetCursorPos, HICON, HMENU, InsertMenuItemW, MENUINFO, MENUITEMINFOW, MFS_DISABLED,
    MFT_OWNERDRAW, MFT_SEPARATOR, MIIM_DATA, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING,
    MIM_BACKGROUND, PostMessageW, RegisterWindowMessageW, SM_CXICON, SM_CXSMICON,
    SetForegroundWindow, SetMenuInfo, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WINDOW_STYLE,
    WM_APP, WM_CONTEXTMENU, WM_DRAWITEM, WM_ENTERIDLE, WM_MEASUREITEM, WM_NULL, WS_EX_TOOLWINDOW,
    WS_POPUP,
};
use windows::core::{PWSTR, w};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::platform;
use crate::theme::{self, Fonts, Palette};

pub const WM_TRAY: u32 = WM_APP + 2;
const TRAY_CLASS: windows::core::PCWSTR = w!("WinwrightTrayOwner");
const ICON_ID: u32 = 1;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
/// `WM_ENTERIDLE` source for menus.
const MSGF_MENU: usize = 2;
/// Windows sends a second NIN_KEYSELECT for Enter; ignore selects this soon after a close.
const REOPEN_GUARD: Duration = Duration::from_millis(300);

pub type TrayCallback = Arc<dyn Fn(u32) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayMenuItem {
    pub id: u32,
    pub label: String,
    pub enabled: bool,
    /// A Segoe Fluent Icons code point shown before the label.
    pub glyph: Option<char>,
    /// Draw a separator line above this item.
    pub separator_before: bool,
}

/// What the icon shows. `active` picks the teal (running) or gray (stopped) mark;
/// `status` is the second line of the menu header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayState {
    pub tooltip: String,
    pub active: bool,
    pub status: String,
    pub items: Vec<TrayMenuItem>,
}

struct Tray {
    owner: HWND,
    state: TrayState,
    callback: TrayCallback,
    /// Small icons: active, stopped.
    icons: [HICON; 2],
    /// Large icon for notifications.
    large: HICON,
    taskbar_created: u32,
}

/// One owner-drawn menu row.
#[derive(Clone, Debug)]
enum Row {
    Header { active: bool, status: String },
    Separator,
    Item(TrayMenuItem),
}

/// What the open menu draws with; lives while the menu is shown.
struct MenuStyle {
    rows: Vec<Row>,
    palette: Palette,
    fonts: Fonts,
    styled: isize,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    static MENU_CLOSED: Cell<Option<Instant>> = const { Cell::new(None) };
    static MENU: RefCell<Option<MenuStyle>> = const { RefCell::new(None) };
}

fn icon_sizes() -> (i32, i32) {
    // SAFETY: plain metric queries.
    unsafe {
        let dpi = GetDpiForSystem();
        (
            GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16),
            GetSystemMetricsForDpi(SM_CXICON, dpi).max(32),
        )
    }
}

fn base_data(owner: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: owner,
        uID: ICON_ID,
        ..Default::default()
    }
}

fn copy_wide<const N: usize>(dst: &mut [u16; N], s: &str) {
    let src: Vec<u16> = s.encode_utf16().take(N - 1).collect();
    dst[..src.len()].copy_from_slice(&src);
    dst[src.len()] = 0;
}

fn shell(message: NOTIFY_ICON_MESSAGE, data: &NOTIFYICONDATAW) -> WinwrightResult<()> {
    // SAFETY: `data` is fully initialized and outlives the call.
    if unsafe { Shell_NotifyIconW(message, data) }.as_bool() {
        Ok(())
    } else {
        Err(WinwrightError::BackendUnavailable {
            backend: "tray".into(),
            reason: "Shell_NotifyIcon failed (is Explorer running?)".into(),
        })
    }
}

fn notify(
    owner: HWND,
    icons: [HICON; 2],
    state: &TrayState,
    message: NOTIFY_ICON_MESSAGE,
) -> WinwrightResult<()> {
    let mut data = base_data(owner);
    data.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE | NIF_SHOWTIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = icons[usize::from(!state.active)];
    copy_wide(&mut data.szTip, &state.tooltip);
    shell(message, &data)?;
    if message == NIM_ADD {
        data.Anonymous = NOTIFYICONDATAW_0 {
            uVersion: NOTIFYICON_VERSION_4,
        };
        shell(NIM_SETVERSION, &data)?;
    }
    Ok(())
}

/// Adds the icon, or updates it when it already exists (`callback` then optional).
pub fn set(
    hinstance: HINSTANCE,
    state: TrayState,
    callback: Option<TrayCallback>,
) -> WinwrightResult<()> {
    // Update in place when the icon exists. Shell/window calls below may re-enter tray_proc,
    // so TRAY is never borrowed across them.
    let exists = TRAY.with(|cell| {
        cell.try_borrow_mut().ok().and_then(|mut slot| {
            slot.as_mut().map(|tray| {
                tray.state = state.clone();
                if let Some(cb) = callback.clone() {
                    tray.callback = cb;
                }
                (tray.owner, tray.icons, tray.state.clone())
            })
        })
    });
    if let Some((owner, icons, current)) = exists {
        return notify(owner, icons, &current, NIM_MODIFY);
    }
    let callback =
        callback.ok_or_else(|| WinwrightError::invalid("the first tray call needs a callback"))?;
    crate::thread::register_class(hinstance, TRAY_CLASS, Some(tray_proc))?;
    // SAFETY: the class is registered; the window is never shown (it only owns the icon and
    // the popup menu, which needs a window that can take the foreground).
    let owner = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            TRAY_CLASS,
            w!("Winwright"),
            WINDOW_STYLE(WS_POPUP.0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| platform("CreateWindowExW(tray)", &e))?;
    let (small, large) = icon_sizes();
    let mut made = Vec::new();
    for (size, stopped) in [(small, false), (small, true), (large, false)] {
        match theme::brand_icon(size, stopped) {
            Ok(icon) => made.push(icon),
            Err(err) => {
                // SAFETY: destroys what was created above on this thread, each exactly once.
                unsafe {
                    for icon in made {
                        let _ = DestroyIcon(icon);
                    }
                    let _ = DestroyWindow(owner);
                }
                return Err(err);
            }
        }
    }
    let (icons, large) = ([made[0], made[1]], made[2]);
    // SAFETY: registers (or looks up) a system-wide message name.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    // Kept even if adding fails (Explorer not started yet, or busy): TaskbarCreated adds the
    // icon later, and `remove` deletes an icon that was added after all.
    TRAY.with(|cell| {
        *cell.borrow_mut() = Some(Tray {
            owner,
            state: state.clone(),
            callback,
            icons,
            large,
            taskbar_created,
        });
    });
    notify(owner, icons, &state, NIM_ADD)
}

/// Shows a notification from the icon (a toast on Windows 11). Quiet hours are respected.
pub fn balloon(title: &str, body: &str) {
    let Some((owner, large)) = TRAY.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|t| t.as_ref().map(|t| (t.owner, t.large)))
    }) else {
        return;
    };
    let mut data = base_data(owner);
    data.uFlags = NIF_INFO;
    copy_wide(&mut data.szInfoTitle, title);
    copy_wide(&mut data.szInfo, body);
    data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON | NIIF_RESPECT_QUIET_TIME;
    data.hBalloonIcon = large;
    if let Err(err) = shell(NIM_MODIFY, &data) {
        tracing::debug!(%err, "tray notification not shown");
    }
}

/// Removes the icon and frees its resources. Safe to call when there is no icon.
pub fn remove() {
    let Some(tray) = TRAY.with(|cell| cell.try_borrow_mut().ok().and_then(|mut t| t.take())) else {
        return;
    };
    let _ = notify(tray.owner, tray.icons, &tray.state, NIM_DELETE);
    // SAFETY: the icons and window were created on this thread and are destroyed once.
    unsafe {
        for icon in tray.icons.into_iter().chain([tray.large]) {
            let _ = DestroyIcon(icon);
        }
        let _ = DestroyWindow(tray.owner);
    }
}

/// The menu background brush, deleted when the menu closes.
struct Brush(HBRUSH);

impl Drop for Brush {
    fn drop(&mut self) {
        // SAFETY: created for this menu only; the menu is destroyed first.
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

fn insert(menu: HMENU, position: u32, info: &MENUITEMINFOW) {
    // SAFETY: `info` and any string it points at outlive the synchronous call.
    let _ = unsafe { InsertMenuItemW(menu, position, true, info) };
}

fn rows_for(state: &TrayState) -> Vec<Row> {
    let mut rows = vec![Row::Header {
        active: state.active,
        status: state.status.clone(),
    }];
    for (i, item) in state.items.iter().enumerate() {
        if i == 0 || item.separator_before {
            rows.push(Row::Separator);
        }
        rows.push(Row::Item(item.clone()));
    }
    rows
}

fn build_menu(menu: HMENU, rows: &[Row]) {
    for (i, row) in rows.iter().enumerate() {
        let mut info = MENUITEMINFOW {
            cbSize: size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE | MIIM_DATA,
            // Header and separators are owner-drawn separators: never selectable.
            fType: MFT_OWNERDRAW | MFT_SEPARATOR,
            dwItemData: i,
            ..Default::default()
        };
        // Keep the text on real items so screen readers can read the menu.
        let mut label: Vec<u16> = match row {
            Row::Item(item) => item.label.encode_utf16().chain([0]).collect(),
            _ => Vec::new(),
        };
        if let Row::Item(item) = row {
            info.fMask |= MIIM_ID | MIIM_STATE | MIIM_STRING;
            info.fType = MFT_OWNERDRAW;
            info.wID = item.id;
            info.fState = if item.enabled {
                Default::default()
            } else {
                MFS_DISABLED
            };
            info.dwTypeData = PWSTR(label.as_mut_ptr());
            info.cch = (label.len() - 1) as u32;
        }
        insert(menu, i as u32, &info);
    }
}

fn show_menu(owner: HWND, anchor: Option<POINT>) {
    // One menu at a time: a tray event dispatched by the open menu's modal loop must not
    // replace (and on return clear) the rows the open menu draws from.
    if MENU.with(|m| m.try_borrow().map_or(true, |m| m.is_some())) {
        return;
    }
    let Some((state, callback)) = TRAY.with(|cell| {
        cell.try_borrow().ok().and_then(|t| {
            t.as_ref()
                .map(|t| (t.state.clone(), Arc::clone(&t.callback)))
        })
    }) else {
        return;
    };
    // SAFETY: plain DPI query of our own window.
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(owner) }.max(96);
    let palette = Palette::system();
    let rows = rows_for(&state);
    // SAFETY: an empty popup menu, destroyed below on this thread.
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    MENU.with(|m| {
        *m.borrow_mut() = Some(MenuStyle {
            rows: rows.clone(),
            palette,
            fonts: Fonts::new(dpi),
            styled: 0,
        });
    });
    // SAFETY: plain GDI brush for the menu background; deleted after the menu.
    let brush = Brush(unsafe { CreateSolidBrush(theme::cr(palette.surface)) });
    // SAFETY: the menu is created, shown modally, and destroyed on this thread; strings and
    // the brush outlive the menu.
    let chosen = unsafe {
        build_menu(menu, &rows);
        let _ = SetMenuInfo(
            menu,
            &MENUINFO {
                cbSize: size_of::<MENUINFO>() as u32,
                fMask: MIM_BACKGROUND,
                hbrBack: brush.0,
                ..Default::default()
            },
        );
        let pt = anchor.unwrap_or_else(|| {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            pt
        });
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(owner);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            owner,
            None,
        );
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        chosen.0
    };
    drop(brush);
    MENU.with(|m| m.borrow_mut().take());
    MENU_CLOSED.with(|c| c.set(Some(Instant::now())));
    if chosen > 0 {
        callback(chosen as u32);
    }
}

fn with_menu<R>(f: impl FnOnce(&mut MenuStyle) -> R) -> Option<R> {
    MENU.with(|m| m.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Label and accelerator (`"Stop\tCtrl+Alt+Esc"`).
fn split_label(label: &str) -> (&str, &str) {
    label.split_once('\t').unwrap_or((label, ""))
}

impl MenuStyle {
    fn measure(&self, m: &mut MEASUREITEMSTRUCT) {
        let f = &self.fonts;
        let Some(row) = self.rows.get(m.itemData) else {
            return;
        };
        // SAFETY: a screen DC borrowed for measuring only.
        let dc = unsafe { GetDC(None) };
        let width =
            |font: &theme::Font, s: &str| theme::measure(dc, font, s, None, DRAW_TEXT_FORMAT(0)).0;
        let (w, h) = match row {
            Row::Header { status, .. } => (
                f.px(12 + 32 + 12)
                    + width(&f.strong, "Winwright").max(width(&f.small, status) + f.px(14))
                    + f.px(20),
                f.px(60),
            ),
            Row::Separator => (f.px(40), f.px(9)),
            Row::Item(item) => {
                let (label, accel) = split_label(&item.label);
                let accel_w = if accel.is_empty() {
                    0
                } else {
                    f.px(28) + width(&f.small, accel)
                };
                (
                    f.px(44) + width(&f.body, label) + accel_w + f.px(16),
                    f.px(36),
                )
            }
        };
        // SAFETY: releases the DC borrowed above.
        unsafe { ReleaseDC(None, dc) };
        m.itemWidth = w.max(f.px(240)) as u32;
        m.itemHeight = h as u32;
    }

    fn draw(&self, d: &DRAWITEMSTRUCT) {
        let p = &self.palette;
        let f = &self.fonts;
        let Some(row) = self.rows.get(d.itemData) else {
            return;
        };
        let (hdc, r) = (d.hDC, d.rcItem);
        theme::fill(hdc, r, p.surface);
        match row {
            Row::Header { active, status } => {
                let mark = f.px(32);
                let x = r.left + f.px(12);
                let y = r.top + (r.bottom - r.top - mark) / 2;
                theme::blit(hdc, x, y, mark, mark, &theme::brand_pixels(mark, !active));
                let text_x = x + mark + f.px(12);
                let mid = (r.top + r.bottom) / 2;
                theme::text(
                    hdc,
                    &f.strong,
                    "Winwright",
                    RECT {
                        left: text_x,
                        top: r.top,
                        right: r.right - f.px(8),
                        bottom: mid + f.px(1),
                    },
                    p.text,
                    DT_SINGLELINE | DT_BOTTOM,
                );
                let dot = f.px(8);
                let line = RECT {
                    left: text_x,
                    top: mid + f.px(3),
                    right: r.right - f.px(8),
                    bottom: mid + f.px(21),
                };
                let dot_top = line.top + (line.bottom - line.top - dot) / 2;
                theme::dot(
                    hdc,
                    RECT {
                        left: text_x,
                        top: dot_top,
                        right: text_x + dot,
                        bottom: dot_top + dot,
                    },
                    if *active { p.good } else { p.bad },
                );
                theme::text(
                    hdc,
                    &f.small,
                    status,
                    RECT {
                        left: text_x + dot + f.px(6),
                        ..line
                    },
                    p.muted,
                    DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                );
            }
            Row::Separator => {
                let y = (r.top + r.bottom) / 2;
                theme::fill(
                    hdc,
                    RECT {
                        left: r.left + f.px(12),
                        right: r.right - f.px(12),
                        top: y,
                        bottom: y + 1,
                    },
                    p.border,
                );
            }
            Row::Item(item) => {
                let disabled = d.itemState.0 & ODS_DISABLED.0 != 0;
                let hot = d.itemState.0 & ODS_SELECTED.0 != 0 && !disabled;
                if hot {
                    theme::rounded(
                        hdc,
                        RECT {
                            left: r.left + f.px(4),
                            right: r.right - f.px(4),
                            top: r.top + f.px(2),
                            bottom: r.bottom - f.px(2),
                        },
                        f.px(4) as f32,
                        if p.high_contrast { p.accent } else { p.hover },
                        None,
                    );
                }
                let fg = if disabled {
                    p.faint
                } else if hot && p.high_contrast {
                    p.on_accent
                } else {
                    p.text
                };
                if let Some(g) = item.glyph {
                    theme::icon(
                        hdc,
                        &f.icons,
                        g,
                        RECT {
                            left: r.left + f.px(14),
                            right: r.left + f.px(34),
                            ..r
                        },
                        fg,
                    );
                }
                let (label, accel) = split_label(&item.label);
                theme::text(
                    hdc,
                    &f.body,
                    label,
                    RECT {
                        left: r.left + f.px(44),
                        right: r.right - f.px(12),
                        ..r
                    },
                    fg,
                    DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                );
                if !accel.is_empty() {
                    theme::text(
                        hdc,
                        &f.small,
                        accel,
                        RECT {
                            right: r.right - f.px(14),
                            ..r
                        },
                        if disabled { p.faint } else { p.muted },
                        DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
                    );
                }
            }
        }
    }
}

/// Rounds the open menu's corners and colors its border (Windows 11; ignored elsewhere).
fn style_menu_window(menu_hwnd: HWND) {
    let Some(border) = with_menu(|m| {
        let first = m.styled != menu_hwnd.0 as isize;
        m.styled = menu_hwnd.0 as isize;
        first.then_some(m.palette.border)
    })
    .flatten() else {
        return;
    };
    let corner = DWMWCP_ROUND.0 as u32;
    let color = theme::cr(border).0;
    // SAFETY: 4-byte attribute values that outlive the calls.
    unsafe {
        let _ = DwmSetWindowAttribute(
            menu_hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const u32 as *const std::ffi::c_void,
            4,
        );
        let _ = DwmSetWindowAttribute(
            menu_hwnd,
            DWMWA_BORDER_COLOR,
            &color as *const u32 as *const std::ffi::c_void,
            4,
        );
    }
}

fn readd_after_explorer_restart() {
    let current = TRAY.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|t| t.as_ref().map(|t| (t.owner, t.icons, t.state.clone())))
    });
    if let Some((owner, icons, state)) = current {
        let _ = notify(owner, icons, &state, NIM_ADD);
    }
}

fn anchor_from(wparam: WPARAM) -> POINT {
    POINT {
        x: (wparam.0 & 0xFFFF) as u16 as i16 as i32,
        y: ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
    }
}

fn on_tray_event(hwnd: HWND, event: u32, wparam: WPARAM) {
    match event {
        NIN_SELECT | NIN_KEYSELECT | WM_CONTEXTMENU => {
            let recently_closed = MENU_CLOSED
                .with(Cell::get)
                .is_some_and(|t| t.elapsed() < REOPEN_GUARD);
            if !recently_closed {
                show_menu(hwnd, Some(anchor_from(wparam)));
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = catch_unwind(AssertUnwindSafe(|| {
        match msg {
            WM_TRAY => {
                on_tray_event(hwnd, (lparam.0 & 0xFFFF) as u32, wparam);
                return true;
            }
            WM_MEASUREITEM => {
                // SAFETY: WM_MEASUREITEM's lParam points at a MEASUREITEMSTRUCT.
                let m = unsafe { &mut *(lparam.0 as *mut MEASUREITEMSTRUCT) };
                return m.CtlType == ODT_MENU && with_menu(|menu| menu.measure(m)).is_some();
            }
            WM_DRAWITEM => {
                // SAFETY: WM_DRAWITEM's lParam points at a DRAWITEMSTRUCT.
                let d = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
                return d.CtlType == ODT_MENU && with_menu(|menu| menu.draw(d)).is_some();
            }
            WM_ENTERIDLE if wparam.0 == MSGF_MENU => {
                style_menu_window(HWND(lparam.0 as *mut std::ffi::c_void));
                return true;
            }
            _ => {}
        }
        // `try_borrow`: this proc can run while `set` holds the state (window creation).
        let taskbar_created = TRAY.with(|cell| {
            cell.try_borrow()
                .ok()
                .and_then(|t| t.as_ref().map(|t| t.taskbar_created))
        });
        if taskbar_created == Some(msg) {
            readd_after_explorer_restart();
            return true;
        }
        false
    }));
    match handled {
        Ok(true) if msg == WM_MEASUREITEM || msg == WM_DRAWITEM => LRESULT(1),
        Ok(true) => LRESULT(0),
        Ok(false) => {
            // SAFETY: forwards the unmodified message to the default procedure.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        Err(_) => {
            tracing::error!("tray window procedure panicked");
            LRESULT(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_coordinates_are_signed() {
        let p = anchor_from(WPARAM(((200u32 << 16) | 0xFFF6) as usize));
        assert_eq!((p.x, p.y), (-10, 200));
    }

    #[test]
    fn menu_rows_group_items_with_separators() {
        let item = |id, separator_before| TrayMenuItem {
            id,
            label: format!("Item {id}\tCtrl+{id}"),
            enabled: true,
            glyph: None,
            separator_before,
        };
        let rows = rows_for(&TrayState {
            tooltip: String::new(),
            active: true,
            status: "Active".into(),
            items: vec![item(1, false), item(2, true), item(3, false)],
        });
        let kinds: Vec<&str> = rows
            .iter()
            .map(|r| match r {
                Row::Header { .. } => "header",
                Row::Separator => "sep",
                Row::Item(_) => "item",
            })
            .collect();
        assert_eq!(kinds, ["header", "sep", "item", "sep", "item", "item"]);
        assert_eq!(split_label("Stop\tCtrl+Alt+Esc"), ("Stop", "Ctrl+Alt+Esc"));
        assert_eq!(split_label("Open"), ("Open", ""));
    }

    #[test]
    fn wide_copies_truncate_and_terminate() {
        let mut buf = [1u16; 8];
        copy_wide(&mut buf, "Winwright tray");
        assert_eq!(
            &buf[..7],
            "Winwrig".encode_utf16().collect::<Vec<_>>().as_slice()
        );
        assert_eq!(buf[7], 0, "always NUL-terminated");
    }
}
