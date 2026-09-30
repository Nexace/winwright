//! Monitor enumeration and DPI lookups. Plain Win32 (no COM), so safe on any thread.

use std::marker::PhantomData;

use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW,
    MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor,
    MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::BOOL;
use winwright_contracts::capture::MonitorInfo;
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::{WinwrightError, WinwrightResult};

const DEFAULT_DPI: u32 = 96;

/// Puts the calling thread in Per-Monitor-V2 mode while alive, so monitor and window rectangles
/// are physical pixels even when the host process is DPI-unaware; restores the previous mode on
/// drop.
pub struct PhysicalDpiScope {
    previous: DPI_AWARENESS_CONTEXT,
    _thread_bound: PhantomData<*const ()>,
}

impl PhysicalDpiScope {
    pub fn enter() -> Self {
        // SAFETY: changes only the calling thread's DPI awareness; no pointers are passed.
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Self {
            previous,
            _thread_bound: PhantomData,
        }
    }
}

impl Drop for PhysicalDpiScope {
    fn drop(&mut self) {
        if !self.previous.0.is_null() {
            // SAFETY: restores the context returned by the matching call on this thread.
            unsafe { SetThreadDpiAwarenessContext(self.previous) };
        }
    }
}

/// A monitor plus its live handle. The handle stays on the enumerating thread.
pub struct Monitor {
    pub info: MonitorInfo,
    pub handle: HMONITOR,
}

unsafe extern "system" fn collect(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: `lparam` is the `&mut Vec<HMONITOR>` passed by `enumerate`, alive for the
    // duration of the synchronous EnumDisplayMonitors call.
    let out = unsafe { &mut *(lparam.0 as *mut Vec<HMONITOR>) };
    out.push(monitor);
    BOOL::from(true)
}

fn rect(r: RECT) -> PhysicalRect {
    PhysicalRect::new(r.left, r.top, r.right, r.bottom)
}

pub fn dpi_for(monitor: HMONITOR) -> u32 {
    let (mut x, mut y) = (0, 0);
    // SAFETY: both out pointers are valid u32s; an invalid handle only yields an error.
    match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) } {
        Ok(()) if x > 0 => x,
        _ => DEFAULT_DPI,
    }
}

/// Effective DPI of the monitor holding (or nearest to) a physical desktop point.
pub fn dpi_at(point: PhysicalPoint) -> u32 {
    let _physical = PhysicalDpiScope::enter();
    // SAFETY: plain lookup; DEFAULTTONEAREST always yields a monitor handle.
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: point.x,
                y: point.y,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    dpi_for(monitor)
}

fn describe(handle: HMONITOR) -> Option<Monitor> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a MONITORINFOEXW whose cbSize announces the extended layout, so the
    // API may write the device name after the base MONITORINFO.
    if !unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo) }.as_bool() {
        return None;
    }
    let name_len = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    let dpi = dpi_for(handle);
    Some(Monitor {
        info: MonitorInfo {
            index: 0,
            name: String::from_utf16_lossy(&info.szDevice[..name_len]),
            bounds: rect(info.monitorInfo.rcMonitor),
            work_area: rect(info.monitorInfo.rcWork),
            dpi,
            scale_percent: scale_percent(dpi),
            primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        },
        handle,
    })
}

pub const fn scale_percent(dpi: u32) -> u32 {
    dpi * 100 / DEFAULT_DPI
}

/// Deterministic order: primary first, then by left edge, then top edge. Indices follow it.
fn order(monitors: &mut [Monitor]) {
    monitors.sort_by_key(|m| (!m.info.primary, m.info.bounds.left, m.info.bounds.top));
    for (index, m) in monitors.iter_mut().enumerate() {
        m.info.index = index as u32;
    }
}

/// Every attached monitor in physical pixels, in [`order`].
pub fn enumerate() -> WinwrightResult<Vec<Monitor>> {
    let _physical = PhysicalDpiScope::enter();
    let mut handles: Vec<HMONITOR> = Vec::with_capacity(8);
    // SAFETY: `collect` only pushes into `handles`, which outlives this synchronous call.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        )
    };
    if !ok.as_bool() {
        return Err(WinwrightError::Platform {
            operation: "EnumDisplayMonitors".into(),
            hresult: windows::core::Error::from_thread().code().0,
        });
    }
    let mut monitors: Vec<Monitor> = handles.into_iter().filter_map(describe).collect();
    order(&mut monitors);
    Ok(monitors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(primary: bool, left: i32, top: i32) -> Monitor {
        Monitor {
            info: MonitorInfo {
                index: 99,
                name: format!("{left},{top}"),
                bounds: PhysicalRect::new(left, top, left + 100, top + 100),
                work_area: PhysicalRect::new(left, top, left + 100, top + 90),
                dpi: 96,
                scale_percent: 100,
                primary,
            },
            handle: HMONITOR::default(),
        }
    }

    #[test]
    fn primary_first_then_left_then_top() {
        let mut monitors = vec![
            monitor(false, 1920, 0),
            monitor(false, -2560, 200),
            monitor(true, 0, 0),
            monitor(false, -2560, -1240),
        ];
        order(&mut monitors);
        let names: Vec<_> = monitors.iter().map(|m| m.info.name.as_str()).collect();
        assert_eq!(names, ["0,0", "-2560,-1240", "-2560,200", "1920,0"]);
        let indices: Vec<_> = monitors.iter().map(|m| m.info.index).collect();
        assert_eq!(indices, [0, 1, 2, 3]);
    }

    #[test]
    fn scale_from_dpi() {
        assert_eq!(scale_percent(96), 100);
        assert_eq!(scale_percent(120), 125);
        assert_eq!(scale_percent(144), 150);
        assert_eq!(scale_percent(192), 200);
    }
}
