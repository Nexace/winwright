//! Top-level window control (spec §17) and integrity-level checks (spec §26).

use std::ffi::c_void;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
    TOKEN_QUERY, TokenIntegrityLevel,
};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsZoomed, PostMessageW, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SWP_NOACTIVATE,
    SWP_NOOWNERZORDER, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow, WM_CLOSE,
};
use winwright_contracts::action::WindowVisualState;
use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::{WinwrightError, WinwrightResult};

fn to_hwnd(value: u64) -> HWND {
    HWND(value as usize as *mut c_void)
}

fn platform(operation: &str, err: &windows::core::Error) -> WinwrightError {
    WinwrightError::Platform {
        operation: operation.to_owned(),
        hresult: err.code().0,
    }
}

fn live(hwnd: u64) -> WinwrightResult<HWND> {
    let h = to_hwnd(hwnd);
    // SAFETY: takes the handle by value; stale handles report false.
    if h.is_invalid() || !unsafe { IsWindow(Some(h)) }.as_bool() {
        return Err(WinwrightError::WindowNotFound {
            query: format!("hwnd={hwnd:#x}"),
        });
    }
    Ok(h)
}

fn foreground() -> HWND {
    // SAFETY: no arguments.
    unsafe { GetForegroundWindow() }
}

/// Brings `hwnd` to the foreground. Windows only lets the foreground (or last-input) process
/// change focus, so we temporarily attach to the foreground thread's input queue.
pub fn focus_window(hwnd: u64) -> WinwrightResult<()> {
    let h = live(hwnd)?;
    // SAFETY: handle-by-value Win32 calls; attach/detach are always paired below.
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        for attempt in 0..3 {
            if foreground() == h {
                return Ok(());
            }
            let fg_thread = GetWindowThreadProcessId(foreground(), None);
            let me = GetCurrentThreadId();
            let attached = fg_thread != 0
                && fg_thread != me
                && AttachThreadInput(me, fg_thread, true).as_bool();
            let _ = BringWindowToTop(h);
            let _ = SetForegroundWindow(h);
            if attached {
                let _ = AttachThreadInput(me, fg_thread, false);
            }
            if foreground() == h {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(30 * (attempt + 1)));
        }
    }
    if foreground() == h {
        Ok(())
    } else {
        Err(WinwrightError::WindowNotFocused {
            window: format!("hwnd={hwnd:#x}"),
        })
    }
}

pub fn set_window_state(hwnd: u64, state: WindowVisualState) -> WinwrightResult<()> {
    let h = live(hwnd)?;
    let cmd = match state {
        WindowVisualState::Minimized => SW_MINIMIZE,
        WindowVisualState::Maximized => SW_MAXIMIZE,
        WindowVisualState::Normal => SW_RESTORE,
    };
    // SAFETY: handle-by-value call; the return value is the previous visibility, not success.
    let _ = unsafe { ShowWindow(h, cmd) };
    Ok(())
}

fn frame_and_window(h: HWND) -> WinwrightResult<(RECT, RECT)> {
    let mut window = RECT::default();
    // SAFETY: `window` is a valid out pointer.
    unsafe { GetWindowRect(h, &mut window) }.map_err(|e| platform("GetWindowRect", &e))?;
    let mut frame = RECT::default();
    // SAFETY: the out buffer is a RECT of exactly the size passed.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut frame as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    };
    Ok((if dwm.is_ok() { frame } else { window }, window))
}

/// Window rect that makes the visible frame equal `target`, given the current invisible
/// resize borders (`window` minus `frame`).
pub fn window_rect_for_frame(target: PhysicalRect, frame: RECT, window: RECT) -> PhysicalRect {
    PhysicalRect::new(
        target.left - (frame.left - window.left),
        target.top - (frame.top - window.top),
        target.right + (window.right - frame.right),
        target.bottom + (window.bottom - frame.bottom),
    )
}

pub fn set_window_bounds(hwnd: u64, bounds: PhysicalRect) -> WinwrightResult<()> {
    if bounds.width() < 1 || bounds.height() < 1 {
        return Err(WinwrightError::invalid(
            "window bounds must have a positive size",
        ));
    }
    let h = live(hwnd)?;
    // SAFETY: handle-by-value calls.
    unsafe {
        if IsIconic(h).as_bool() || IsZoomed(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
    }
    // Two passes: moving across monitors with different DPI changes the border widths.
    for _ in 0..2 {
        let (frame, window) = frame_and_window(h)?;
        let r = window_rect_for_frame(bounds, frame, window);
        // SAFETY: handle-by-value call.
        unsafe {
            SetWindowPos(
                h,
                None,
                r.left,
                r.top,
                r.width(),
                r.height(),
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            )
        }
        .map_err(|e| platform("SetWindowPos", &e))?;
    }
    Ok(())
}

pub fn close_window(hwnd: u64) -> WinwrightResult<()> {
    let h = live(hwnd)?;
    // SAFETY: posting WM_CLOSE with null parameters is always valid.
    unsafe { PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)) }
        .map_err(|e| platform("PostMessage(WM_CLOSE)", &e))
}

/// Mandatory integrity RID of a process token (0x2000 medium, 0x3000 high, ...).
fn token_integrity(token: HANDLE) -> Option<u32> {
    let mut len = 0u32;
    // SAFETY: size query with a null buffer; failure with ERROR_INSUFFICIENT_BUFFER is expected.
    let _ = unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut len) };
    if len == 0 || len > 4096 {
        return None;
    }
    // u64 storage keeps the TOKEN_MANDATORY_LABEL pointer field aligned.
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` holds at least `len` bytes and outlives the call.
    unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
    }
    .ok()?;
    // SAFETY: on success the buffer starts with a TOKEN_MANDATORY_LABEL whose SID pointer
    // points inside `buf`, which is still alive; the sub-authority index is count - 1.
    unsafe {
        let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let sid = label.Label.Sid;
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return None;
        }
        Some(*GetSidSubAuthority(sid, u32::from(count) - 1))
    }
}

fn process_integrity(pid: u32) -> Option<u32> {
    // SAFETY: handles are closed on every path below.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        let _ = CloseHandle(process);
        opened.ok()?;
        let level = token_integrity(token);
        let _ = CloseHandle(token);
        level
    }
}

pub fn current_integrity() -> Option<u32> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle needs no closing; the token handle is closed below.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let level = token_integrity(token);
        let _ = CloseHandle(token);
        level
    }
}

/// UIPI blocks input into processes above our integrity level. Unqueryable processes are
/// treated as more privileged: failing closed yields a clear `UIPI_BLOCKED` error.
pub fn is_more_privileged(pid: u32) -> bool {
    let ours = current_integrity().unwrap_or(0x2000);
    process_integrity(pid).is_none_or(|theirs| theirs > ours)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_to_window_rect_accounts_for_invisible_borders() {
        // Typical Win11: 7px invisible borders left/right/bottom, none on top.
        let window = RECT {
            left: 93,
            top: 100,
            right: 907,
            bottom: 707,
        };
        let frame = RECT {
            left: 100,
            top: 100,
            right: 900,
            bottom: 700,
        };
        let r = window_rect_for_frame(PhysicalRect::new(0, 0, 640, 480), frame, window);
        assert_eq!(r, PhysicalRect::new(-7, 0, 647, 487));
    }

    #[test]
    fn own_process_is_not_more_privileged_than_itself() {
        assert!(current_integrity().is_some());
        assert!(!is_more_privileged(std::process::id()));
    }

    #[test]
    fn missing_window_is_typed() {
        assert_eq!(
            focus_window(0).unwrap_err().code().as_str(),
            "WINDOW_NOT_FOUND"
        );
        assert_eq!(
            close_window(0).unwrap_err().code().as_str(),
            "WINDOW_NOT_FOUND"
        );
    }
}
