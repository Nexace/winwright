//! Notification-area (tray) icon on the native UI thread (spec §11 plan: native, no WebView).
//! Present only while the owning process runs; removed on shutdown so no stale icon remains.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC, CreateDIBSection,
    CreateFontW, DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject,
    DrawTextW, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION, FONT_QUALITY, HGDIOBJ,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_MESSAGE,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, GetCursorPos, HICON, ICONINFO, MF_GRAYED, MF_STRING, PostMessageW,
    RegisterWindowMessageW, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WINDOW_STYLE, WM_APP, WM_CONTEXTMENU, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{HSTRING, PCWSTR, w};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::platform;

pub const WM_TRAY: u32 = WM_APP + 2;
const TRAY_CLASS: PCWSTR = w!("WinwrightTrayOwner");
const ICON_ID: u32 = 1;
const ICON_SIZE: i32 = 32;
const ACTIVE_COLOR: u32 = 0x00E0_4A2A;
const STOPPED_COLOR: u32 = 0x0080_8080;

pub type TrayCallback = Arc<dyn Fn(u32) + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayMenuItem {
    pub id: u32,
    pub label: String,
    pub enabled: bool,
}

/// What the icon shows. `active` picks the orange (running) or gray (stopped) icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayState {
    pub tooltip: String,
    pub active: bool,
    pub items: Vec<TrayMenuItem>,
}

struct Tray {
    owner: HWND,
    state: TrayState,
    callback: TrayCallback,
    icons: [HICON; 2],
    taskbar_created: u32,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

fn icon_color_bits(color: u32) -> Vec<u32> {
    // Rounded square, straight (non-premultiplied) ARGB, transparent outside.
    let (r, g, b) = ((color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF);
    let size = ICON_SIZE;
    let radius = 7;
    let mut px = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            if inside_rounded(x, y, size, radius) {
                px[(y * size + x) as usize] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
            }
        }
    }
    px
}

fn inside_rounded(x: i32, y: i32, size: i32, r: i32) -> bool {
    let cx = x.clamp(r, size - 1 - r);
    let cy = y.clamp(r, size - 1 - r);
    let (dx, dy) = (x - cx, y - cy);
    dx * dx + dy * dy <= r * r
}

/// Orange/gray rounded square with a white "W".
fn make_icon(color: u32) -> WinwrightResult<HICON> {
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: ICON_SIZE,
        biHeight: -ICON_SIZE,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let info = BITMAPINFO {
        bmiHeader: header,
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let base = icon_color_bits(color);
    // SAFETY: every GDI object created here is selected out and deleted before returning,
    // except the icon handle, which the caller owns. `bits` points at ICON_SIZE² u32 pixels.
    unsafe {
        let color_bmp = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| platform("CreateDIBSection(tray)", &e))?;
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, base.len());
        pixels.copy_from_slice(&base);

        let dc = CreateCompatibleDC(None);
        let old = SelectObject(dc, HGDIOBJ(color_bmp.0));
        let font = CreateFontW(
            -24,
            0,
            0,
            0,
            700,
            0,
            0,
            0,
            FONT_CHARSET(0),
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00FF_FFFF));
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: ICON_SIZE,
            bottom: ICON_SIZE,
        };
        let mut text: Vec<u16> = "W".encode_utf16().collect();
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        SelectObject(dc, old_font);
        let _ = DeleteObject(HGDIOBJ(font.0));
        SelectObject(dc, old);
        let _ = DeleteDC(dc);

        // GDI text clears alpha: restore opacity inside the square, keep the outside clear.
        for (i, p) in pixels.iter_mut().enumerate() {
            let (x, y) = ((i as i32) % ICON_SIZE, (i as i32) / ICON_SIZE);
            *p = if inside_rounded(x, y, ICON_SIZE, 7) {
                *p | 0xFF00_0000
            } else {
                0
            };
        }

        let mask = CreateBitmap(ICON_SIZE, ICON_SIZE, 1, 1, None);
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color_bmp,
        });
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(color_bmp.0));
        icon.map_err(|e| platform("CreateIconIndirect", &e))
    }
}

fn notify(
    owner: HWND,
    icons: [HICON; 2],
    state: &TrayState,
    message: NOTIFY_ICON_MESSAGE,
) -> WinwrightResult<()> {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: owner,
        uID: ICON_ID,
        uFlags: NIF_ICON | NIF_TIP | NIF_MESSAGE,
        uCallbackMessage: WM_TRAY,
        hIcon: icons[usize::from(!state.active)],
        ..Default::default()
    };
    let tip: Vec<u16> = state.tooltip.encode_utf16().take(127).collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    // SAFETY: `data` is fully initialized and outlives the call.
    if unsafe { Shell_NotifyIconW(message, &data) }.as_bool() {
        Ok(())
    } else {
        Err(WinwrightError::BackendUnavailable {
            backend: "tray".into(),
            reason: "Shell_NotifyIcon failed (is Explorer running?)".into(),
        })
    }
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
    let icons = [make_icon(ACTIVE_COLOR)?, make_icon(STOPPED_COLOR)?];
    // SAFETY: registers (or looks up) a system-wide message name.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    notify(owner, icons, &state, NIM_ADD)?;
    TRAY.with(|cell| {
        *cell.borrow_mut() = Some(Tray {
            owner,
            state,
            callback,
            icons,
            taskbar_created,
        });
    });
    Ok(())
}

/// Removes the icon and frees its resources. Safe to call when there is no icon.
pub fn remove() {
    let Some(tray) = TRAY.with(|cell| cell.try_borrow_mut().ok().and_then(|mut t| t.take())) else {
        return;
    };
    let _ = notify(tray.owner, tray.icons, &tray.state, NIM_DELETE);
    // SAFETY: the icons and window were created on this thread and are destroyed once.
    unsafe {
        for icon in tray.icons {
            let _ = DestroyIcon(icon);
        }
        let _ = DestroyWindow(tray.owner);
    }
}

fn show_menu(owner: HWND) {
    let Some((items, callback)) = TRAY.with(|cell| {
        cell.try_borrow().ok().and_then(|t| {
            t.as_ref()
                .map(|t| (t.state.items.clone(), Arc::clone(&t.callback)))
        })
    }) else {
        return;
    };
    // SAFETY: the menu is created, shown modally, and destroyed on this thread; strings
    // outlive the AppendMenuW calls.
    let chosen = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        for item in &items {
            let label = HSTRING::from(item.label.as_str());
            let flags = if item.enabled {
                MF_STRING
            } else {
                MF_STRING | MF_GRAYED
            };
            let _ = AppendMenuW(menu, flags, item.id as usize, PCWSTR(label.as_ptr()));
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(owner);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
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
    if chosen > 0 {
        callback(chosen as u32);
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

unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = catch_unwind(AssertUnwindSafe(|| {
        if msg == WM_TRAY {
            let event = (lparam.0 & 0xFFFF) as u32;
            if matches!(event, WM_RBUTTONUP | WM_LBUTTONUP | WM_CONTEXTMENU) {
                show_menu(hwnd);
            }
            return true;
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
    fn rounded_square_mask() {
        assert!(inside_rounded(16, 16, 32, 7));
        assert!(inside_rounded(0, 16, 32, 7), "edges are inside");
        assert!(!inside_rounded(0, 0, 32, 7), "corners are cut");
        let px = icon_color_bits(0x00E0_4A2A);
        assert_eq!(px[0], 0, "transparent corner");
        assert_eq!(px[16 * 32 + 16], 0xFFE0_4A2A);
    }
}
