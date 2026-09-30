//! Win32 adapters. All unsafe interop is confined to this crate's small private functions;
//! callers only see owned [`WindowInfo`] values.

mod dpi;
mod process;
mod windows_enum;

pub use dpi::enable_per_monitor_dpi_awareness;

use winwright_contracts::WinwrightResult;
use winwright_contracts::backend::WindowBackend;
use winwright_contracts::geometry::PhysicalPoint;
use winwright_contracts::window::WindowInfo;

/// Top-level window enumeration backed by `EnumWindows` and DWM.
#[derive(Clone, Copy, Debug, Default)]
pub struct Win32Windows;

impl WindowBackend for Win32Windows {
    fn list_windows(&self) -> WinwrightResult<Vec<WindowInfo>> {
        windows_enum::list_windows()
    }

    fn foreground_window(&self) -> WinwrightResult<Option<WindowInfo>> {
        Ok(windows_enum::foreground_window())
    }

    fn window(&self, hwnd: u64) -> WinwrightResult<Option<WindowInfo>> {
        Ok(windows_enum::window_info(hwnd))
    }

    fn cursor_position(&self) -> WinwrightResult<PhysicalPoint> {
        windows_enum::cursor_position()
    }

    fn process_name(&self, pid: u32) -> String {
        process::process_name(pid).unwrap_or_default()
    }
}
