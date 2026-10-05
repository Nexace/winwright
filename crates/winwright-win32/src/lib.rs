//! Win32 adapters. All unsafe interop is confined to this crate's small private functions;
//! callers only see owned [`WindowInfo`] values.

mod control;
mod dpi;
mod process;
pub mod shared;
mod windows_enum;

pub use control::current_integrity;
pub use dpi::enable_per_monitor_dpi_awareness;

use winwright_contracts::WinwrightResult;
use winwright_contracts::action::WindowVisualState;
use winwright_contracts::backend::WindowBackend;
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
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

    fn last_input_ms(&self) -> Option<u64> {
        let mut info = windows::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO {
            cbSize: size_of::<windows::Win32::UI::Input::KeyboardAndMouse::LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a valid out-parameter with `cbSize` set.
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetLastInputInfo(&mut info) }
            .as_bool()
            .then_some(u64::from(info.dwTime))
    }

    fn focus_window(&self, hwnd: u64) -> WinwrightResult<()> {
        control::focus_window(hwnd)
    }

    fn set_window_state(&self, hwnd: u64, state: WindowVisualState) -> WinwrightResult<()> {
        control::set_window_state(hwnd, state)
    }

    fn set_window_bounds(&self, hwnd: u64, bounds: PhysicalRect) -> WinwrightResult<()> {
        control::set_window_bounds(hwnd, bounds)
    }

    fn close_window(&self, hwnd: u64) -> WinwrightResult<()> {
        control::close_window(hwnd)
    }

    fn is_more_privileged(&self, pid: u32) -> bool {
        control::is_more_privileged(pid)
    }
}
