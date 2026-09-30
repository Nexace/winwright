use std::collections::HashMap;
use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetWindow, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, IsZoomed, WS_EX_TOPMOST,
};
use windows::core::BOOL;
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::window::WindowInfo;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::process::process_name;

fn to_hwnd(value: u64) -> HWND {
    HWND(value as usize as *mut c_void)
}

fn from_hwnd(hwnd: HWND) -> u64 {
    hwnd.0 as usize as u64
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Vec<HWND>` passed by `top_level_hwnds`, alive for the
    // duration of the synchronous EnumWindows call.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
    out.push(hwnd);
    BOOL::from(true)
}

fn top_level_hwnds() -> WinwrightResult<Vec<HWND>> {
    let mut out: Vec<HWND> = Vec::with_capacity(256);
    // SAFETY: `collect` only writes to `out`, which outlives this synchronous call.
    unsafe { EnumWindows(Some(collect), LPARAM(&mut out as *mut Vec<HWND> as isize)) }.map_err(
        |e| WinwrightError::Platform {
            operation: "EnumWindows".into(),
            hresult: e.code().0,
        },
    )?;
    Ok(out)
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: the out buffer is a u32 of exactly the size passed.
    let r = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    r.is_ok() && cloaked != 0
}

fn window_bounds(hwnd: HWND) -> PhysicalRect {
    let mut rect = RECT::default();
    // SAFETY: the out buffer is a RECT of exactly the size passed.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    };
    if dwm.is_err() {
        // SAFETY: `rect` is a valid out pointer.
        let _ = unsafe { GetWindowRect(hwnd, &mut rect) };
    }
    PhysicalRect::new(rect.left, rect.top, rect.right, rect.bottom)
}

fn window_text(hwnd: HWND) -> String {
    // For windows of other processes this reads the cached caption and never sends
    // WM_GETTEXT, so a hung application cannot block enumeration.
    // SAFETY: no pointer arguments.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: `buf` is a writable slice; the API writes at most its length.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is a writable slice; the API writes at most its length.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn build_info(hwnd: HWND, foreground: HWND, names: &mut HashMap<u32, String>) -> WindowInfo {
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out pointer.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let process_name = names
        .entry(pid)
        .or_insert_with(|| process_name(pid).unwrap_or_default())
        .clone();
    // SAFETY: the remaining calls take only the HWND by value.
    let (minimized, maximized, exstyle, owner) = unsafe {
        (
            IsIconic(hwnd).as_bool(),
            IsZoomed(hwnd).as_bool(),
            GetWindowLongW(hwnd, GWL_EXSTYLE) as u32,
            GetWindow(hwnd, GW_OWNER).ok(),
        )
    };
    WindowInfo {
        hwnd: from_hwnd(hwnd),
        title: window_text(hwnd),
        class_name: class_name(hwnd),
        process_id: pid,
        process_name,
        bounds: window_bounds(hwnd),
        minimized,
        maximized,
        foreground: hwnd == foreground,
        topmost: exstyle & WS_EX_TOPMOST.0 != 0,
        owner_hwnd: owner.filter(|o| !o.is_invalid()).map(from_hwnd),
    }
}

fn foreground_hwnd() -> HWND {
    // SAFETY: no arguments.
    unsafe { GetForegroundWindow() }
}

/// Visible, uncloaked, titled top-level windows in z-order (topmost first).
pub fn list_windows() -> WinwrightResult<Vec<WindowInfo>> {
    let foreground = foreground_hwnd();
    let mut names = HashMap::new();
    let mut windows = Vec::new();
    for hwnd in top_level_hwnds()? {
        // SAFETY: takes the HWND by value; stale handles simply report false.
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() || is_cloaked(hwnd) {
            continue;
        }
        let info = build_info(hwnd, foreground, &mut names);
        if info.title.is_empty() || (info.bounds.is_empty() && !info.minimized) {
            continue;
        }
        windows.push(info);
    }
    Ok(windows)
}

pub fn window_info(hwnd: u64) -> Option<WindowInfo> {
    let hwnd = to_hwnd(hwnd);
    // SAFETY: takes the HWND by value; invalid handles report false.
    if hwnd.is_invalid() || !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return None;
    }
    Some(build_info(hwnd, foreground_hwnd(), &mut HashMap::new()))
}

pub fn foreground_window() -> Option<WindowInfo> {
    let hwnd = foreground_hwnd();
    (!hwnd.is_invalid()).then(|| build_info(hwnd, hwnd, &mut HashMap::new()))
}

pub fn cursor_position() -> WinwrightResult<PhysicalPoint> {
    let mut p = POINT::default();
    // SAFETY: `p` is a valid out pointer.
    unsafe { GetCursorPos(&mut p) }.map_err(|e| WinwrightError::Platform {
        operation: "GetCursorPos".into(),
        hresult: e.code().0,
    })?;
    Ok(PhysicalPoint { x: p.x, y: p.y })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hwnd_round_trips() {
        assert_eq!(from_hwnd(to_hwnd(0x0012_34ab)), 0x0012_34ab);
    }

    #[test]
    fn enumeration_returns_titled_windows() {
        // Runs against the real desktop but only reads; an empty list is valid on CI.
        for w in list_windows().unwrap() {
            assert!(!w.title.is_empty());
            assert_eq!(
                window_info(w.hwnd).map(|i| i.process_id),
                Some(w.process_id)
            );
        }
    }

    #[test]
    fn bogus_hwnd_is_none() {
        assert!(window_info(0).is_none());
    }
}
