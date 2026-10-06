//! Audited Win32 calls. Every `unsafe` block in this crate lives here; none of them hands the OS
//! a pointer that outlives the call.

use windows::Win32::Foundation::GetLastError;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP, OpenInputDesktop,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, GetLastInputInfo, HKL, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
    KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    LASTINPUTINFO, MAPVK_VK_TO_VSC, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyExW, SendInput, VIRTUAL_KEY, VkKeyScanExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};
use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::input::MouseButton;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::keys::Layout;
use crate::plan::RawInput;
use crate::{Platform, Shortfall};

/// The real desktop.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Win32;

/// The keyboard layout of the thread that owns the foreground window: keys go to that thread,
/// and it may use another layout than ours (each app keeps its own when the person switches).
fn target_layout() -> HKL {
    // SAFETY: handle and id queries; the optional process-id pointer is not passed.
    unsafe {
        let window = GetForegroundWindow();
        let thread = if window.is_invalid() {
            0
        } else {
            GetWindowThreadProcessId(window, None)
        };
        GetKeyboardLayout(thread)
    }
}

impl Layout for Win32 {
    fn scan_code(&self, vk: u16) -> u16 {
        // SAFETY: table lookup in a keyboard layout; no pointers involved.
        let scan =
            unsafe { MapVirtualKeyExW(u32::from(vk), MAPVK_VK_TO_VSC, Some(target_layout())) };
        u16::try_from(scan).unwrap_or(0)
    }

    fn vk_key_scan(&self, unit: u16) -> i16 {
        // SAFETY: table lookup in a keyboard layout; no pointers involved.
        unsafe { VkKeyScanExW(unit, target_layout()) }
    }
}

impl Platform for Win32 {
    fn check_input_desktop(&self) -> WinwrightResult<()> {
        // SAFETY: requests a handle to the current input desktop; it is closed just below.
        let desktop =
            unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP) }
                .map_err(|err| WinwrightError::BackendUnavailable {
                    backend: "input".into(),
                    reason: format!(
                        "the input desktop is not accessible (workstation locked, UAC secure \
                         desktop, or non-interactive session): {}",
                        err.message()
                    ),
                })?;
        // SAFETY: `desktop` was opened above, is owned here, and is closed exactly once.
        if let Err(err) = unsafe { CloseDesktop(desktop) } {
            tracing::debug!(%err, "CloseDesktop failed");
        }
        Ok(())
    }

    fn virtual_screen(&self) -> WinwrightResult<PhysicalRect> {
        // SAFETY: GetSystemMetrics only reads system-wide values; no pointers involved.
        let (x, y, cx, cy) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        if cx <= 0 || cy <= 0 {
            return Err(WinwrightError::InputFailed {
                reason: "virtual screen metrics are unavailable".into(),
            });
        }
        Ok(PhysicalRect::new(
            x,
            y,
            x.saturating_add(cx),
            y.saturating_add(cy),
        ))
    }

    fn send(&self, events: &[RawInput]) -> Result<(), Shortfall> {
        if events.is_empty() {
            return Ok(());
        }
        let inputs: Vec<INPUT> = events.iter().map(|&e| to_input(e)).collect();
        // SAFETY: `inputs` is a live slice of fully initialized INPUT values and `cbsize` is
        // their exact size, as SendInput requires. The slice is only read during the call.
        let inserted = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
        if inserted >= inputs.len() {
            return Ok(());
        }
        // SAFETY: reads the calling thread's last-error value, set by the SendInput call above.
        let last_error = unsafe { GetLastError() }.0;
        Err(Shortfall {
            inserted,
            last_error,
        })
    }

    fn input_ticks(&self) -> Option<(u32, u32)> {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: `info` is a valid out-parameter with `cbSize` set; GetTickCount reads a clock.
        unsafe {
            GetLastInputInfo(&mut info)
                .as_bool()
                .then(|| (GetTickCount(), info.dwTime))
        }
    }
}

fn to_input(event: RawInput) -> INPUT {
    match event {
        RawInput::Move { x, y } => mouse(
            x,
            y,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        ),
        RawInput::Button { button, down } => mouse(0, 0, 0, button_flag(button, down)),
        RawInput::Wheel { horizontal, delta } => mouse(
            0,
            0,
            delta.cast_unsigned(),
            if horizontal {
                MOUSEEVENTF_HWHEEL
            } else {
                MOUSEEVENTF_WHEEL
            },
        ),
        RawInput::Key {
            vk,
            scan,
            extended,
            down,
        } => {
            let mut flags = KEYBD_EVENT_FLAGS(0);
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            keyboard(VIRTUAL_KEY(vk), scan, flags)
        }
        RawInput::Unicode { unit, down } => {
            let flags = if down {
                KEYEVENTF_UNICODE
            } else {
                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
            };
            keyboard(VIRTUAL_KEY(0), unit, flags)
        }
    }
}

fn button_flag(button: MouseButton, down: bool) -> MOUSE_EVENT_FLAGS {
    match (button, down) {
        (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
        (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
        (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
        (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
        (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
        (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
    }
}

fn mouse(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn keyboard(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mi(input: &INPUT) -> MOUSEINPUT {
        assert_eq!(input.r#type, INPUT_MOUSE);
        // SAFETY: `mi` is the active union arm for INPUT_MOUSE, checked above.
        unsafe { input.Anonymous.mi }
    }

    fn ki(input: &INPUT) -> KEYBDINPUT {
        assert_eq!(input.r#type, INPUT_KEYBOARD);
        // SAFETY: `ki` is the active union arm for INPUT_KEYBOARD, checked above.
        unsafe { input.Anonymous.ki }
    }

    #[test]
    fn absolute_move_targets_the_virtual_desktop() {
        let m = mi(&to_input(RawInput::Move { x: 65_535, y: 0 }));
        assert_eq!((m.dx, m.dy, m.mouseData), (65_535, 0, 0));
        assert_eq!(
            m.dwFlags,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
        );
    }

    #[test]
    fn buttons_do_not_move() {
        let m = mi(&to_input(RawInput::Button {
            button: MouseButton::Right,
            down: false,
        }));
        assert_eq!(m.dwFlags, MOUSEEVENTF_RIGHTUP);
        assert_eq!((m.dx, m.dy), (0, 0));
        assert_eq!(
            button_flag(MouseButton::Middle, true),
            MOUSEEVENTF_MIDDLEDOWN
        );
        assert_eq!(button_flag(MouseButton::Left, false), MOUSEEVENTF_LEFTUP);
    }

    #[test]
    fn wheel_delta_is_twos_complement() {
        let m = mi(&to_input(RawInput::Wheel {
            horizontal: false,
            delta: -120,
        }));
        assert_eq!(m.dwFlags, MOUSEEVENTF_WHEEL);
        assert_eq!(m.mouseData, 0xFFFF_FF88);
        let m = mi(&to_input(RawInput::Wheel {
            horizontal: true,
            delta: 240,
        }));
        assert_eq!(m.dwFlags, MOUSEEVENTF_HWHEEL);
        assert_eq!(m.mouseData, 240);
    }

    #[test]
    fn key_flags() {
        let k = ki(&to_input(RawInput::Key {
            vk: 0x25,
            scan: 0x4B,
            extended: true,
            down: false,
        }));
        assert_eq!(k.wVk, VIRTUAL_KEY(0x25));
        assert_eq!(k.wScan, 0x4B);
        assert_eq!(k.dwFlags, KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
        let k = ki(&to_input(RawInput::Key {
            vk: 0x41,
            scan: 0x1E,
            extended: false,
            down: true,
        }));
        assert_eq!(k.dwFlags, KEYBD_EVENT_FLAGS(0));
    }

    #[test]
    fn unicode_units_have_no_virtual_key() {
        let k = ki(&to_input(RawInput::Unicode {
            unit: 0xD83D,
            down: true,
        }));
        assert_eq!(k.wVk, VIRTUAL_KEY(0));
        assert_eq!(k.wScan, 0xD83D);
        assert_eq!(k.dwFlags, KEYEVENTF_UNICODE);
        let k = ki(&to_input(RawInput::Unicode {
            unit: 0xD83D,
            down: false,
        }));
        assert_eq!(k.dwFlags, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
    }
}
