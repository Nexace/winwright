//! Window pre-flight checks and geometry for window capture.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowDisplayAffinity, GetWindowRect, IsIconic, IsWindow, IsWindowVisible, WDA_NONE,
};
use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::{WinwrightError, WinwrightResult};

fn failed(reason: &str) -> WinwrightError {
    WinwrightError::CaptureFailed {
        reason: reason.to_owned(),
    }
}

/// Resolves `hwnd` and rejects windows Windows.Graphics.Capture cannot produce frames for,
/// so callers get a typed error instead of a first-frame timeout.
pub fn capturable(hwnd: u64) -> WinwrightResult<HWND> {
    let handle = HWND(hwnd as usize as *mut c_void);
    // SAFETY: IsWindow accepts any value and only reports whether it names a live window.
    if hwnd == 0 || !unsafe { IsWindow(Some(handle)) }.as_bool() {
        return Err(WinwrightError::WindowNotFound {
            query: format!("hwnd={hwnd:#x}"),
        });
    }
    // SAFETY: `handle` was just validated; these are read-only state queries.
    if unsafe { IsIconic(handle) }.as_bool() {
        return Err(failed("window is minimized"));
    }
    // SAFETY: as above.
    if !unsafe { IsWindowVisible(handle) }.as_bool() {
        return Err(failed("window is not visible"));
    }
    let mut cloaked: u32 = 0;
    // SAFETY: the out buffer is a u32 of exactly the size passed.
    let cloak_query = unsafe {
        DwmGetWindowAttribute(
            handle,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    if cloak_query.is_ok() && cloaked != 0 {
        return Err(failed(
            "window is cloaked (on another virtual desktop or hidden by the shell)",
        ));
    }
    let mut affinity: u32 = 0;
    // SAFETY: the out pointer is a valid u32; the query works for any process's window.
    if unsafe { GetWindowDisplayAffinity(handle, &mut affinity) }.is_ok() && affinity != WDA_NONE.0
    {
        return Err(failed(
            "window content is protected from capture (display affinity)",
        ));
    }
    Ok(handle)
}

/// The window's visible frame (`DWMWA_EXTENDED_FRAME_BOUNDS`, physical pixels, no invisible
/// resize borders); its top-left is where a window capture's first pixel sits.
pub fn frame_bounds(handle: HWND) -> PhysicalRect {
    let mut rect = RECT::default();
    // SAFETY: the out buffer is a RECT of exactly the size passed.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            handle,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    };
    if dwm.is_err() {
        // SAFETY: `rect` is a valid out pointer.
        let _ = unsafe { GetWindowRect(handle, &mut rect) };
    }
    PhysicalRect::new(rect.left, rect.top, rect.right, rect.bottom)
}
