//! "Fixture Dialog": a modal top-level window owned by the main window. It is a plain
//! window class driven by `IsDialogMessageW`, so Enter/Esc/Tab behave like a real dialog.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BN_CLICKED, CreateWindowExW, DC_HASDEFID, DM_GETDEFID, DefWindowProcW, DestroyWindow,
    GetWindowRect, IDCANCEL, IDOK, IsDialogMessageW, MSG, SW_SHOW, SetForegroundWindow, ShowWindow,
    WM_CLOSE, WM_COMMAND, WM_SETFOCUS, WS_CAPTION, WS_EX_DLGMODALFRAME, WS_POPUP, WS_SYSMENU,
};
use windows::core::{PCWSTR, w};

use crate::controls::{self, DEFAULT_PUSH, Kind, LINE_EDIT, PUSH, Spec};
use crate::{app, win};

const CLASS: PCWSTR = w!("WinwrightFixtureDialog");
const VALUE: i32 = 201;
const OK: i32 = 202;
const CANCEL: i32 = 203;
const CLIENT_SIZE: [i32; 2] = [300, 104];

const CONTROLS: [Spec; 4] = [
    Spec {
        id: 200,
        kind: Kind::Static,
        text: "Value:",
        style: 0,
        rect: [12, 18, 56, 20],
    },
    Spec {
        id: VALUE,
        kind: Kind::Edit,
        text: "",
        style: LINE_EDIT,
        rect: [72, 14, 216, 24],
    },
    Spec {
        id: OK,
        kind: Kind::Button,
        text: "OK",
        style: DEFAULT_PUSH,
        rect: [112, 64, 84, 28],
    },
    Spec {
        id: CANCEL,
        kind: Kind::Button,
        text: "Cancel",
        style: PUSH,
        rect: [204, 64, 84, 28],
    },
];

pub fn register() {
    win::register_class(CLASS, Some(dialog_proc));
}

/// Shows the dialog centred over `owner` and disables `owner` until it closes.
pub fn open(owner: HWND) -> Result<(), String> {
    if let Some(existing) = current() {
        // SAFETY: activates our own window; no pointers.
        let _ = unsafe { SetForegroundWindow(existing) };
        return Ok(());
    }
    let dpi = win::dpi_of(owner);
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let ex_style = WS_EX_DLGMODALFRAME;
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: win::scale(CLIENT_SIZE[0], dpi),
        bottom: win::scale(CLIENT_SIZE[1], dpi),
    };
    // SAFETY: `frame` is a valid in/out pointer for the duration of the call.
    unsafe { AdjustWindowRectExForDpi(&mut frame, style, false, ex_style, dpi) }
        .map_err(|e| e.to_string())?;
    let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
    let mut owner_rect = RECT::default();
    // SAFETY: `owner_rect` is a valid out pointer.
    unsafe { GetWindowRect(owner, &mut owner_rect) }.map_err(|e| e.to_string())?;
    let x = owner_rect.left + (owner_rect.right - owner_rect.left - width) / 2;
    let y = owner_rect.top + (owner_rect.bottom - owner_rect.top - height) / 2;

    // SAFETY: the class is registered at startup, strings are 'static, and `owner` is our
    // live main window (for a popup, the parent argument sets the owner).
    let dialog = unsafe {
        CreateWindowExW(
            ex_style,
            CLASS,
            w!("Fixture Dialog"),
            style,
            x,
            y,
            width,
            height,
            Some(owner),
            None,
            Some(win::instance()),
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    let font = app().font.get();
    for spec in &CONTROLS {
        if let Err(err) = controls::create(dialog, spec, dpi, font) {
            // SAFETY: destroys the half-built window created above.
            let _ = unsafe { DestroyWindow(dialog) };
            return Err(err.to_string());
        }
    }
    app().dialog.set(dialog);
    // SAFETY: standard modal sequence on windows owned by this thread; no pointers.
    unsafe {
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(dialog, SW_SHOW);
    }
    focus_value(dialog);
    Ok(())
}

/// Routes keyboard navigation (Tab, Enter, Esc) for the open dialog.
pub fn translate(msg: &MSG) -> bool {
    let Some(dialog) = current() else {
        return false;
    };
    // SAFETY: `msg` came from GetMessageW and `dialog` is a live window on this thread.
    unsafe { IsDialogMessageW(dialog, msg) }.as_bool()
}

/// Re-fonts the open dialog after the main window's font was rebuilt for a new DPI.
pub fn apply_font(font: HFONT) {
    if let Some(dialog) = current() {
        for spec in &CONTROLS {
            if let Some(ctrl) = win::child(dialog, spec.id) {
                win::set_font(ctrl, font);
            }
        }
    }
}

fn current() -> Option<HWND> {
    let dialog = app().dialog.get();
    (!dialog.is_invalid()).then_some(dialog)
}

fn focus_value(dialog: HWND) {
    if let Some(edit) = win::child(dialog, VALUE) {
        // SAFETY: focuses a child of our own foreground-capable window; no pointers.
        let _ = unsafe { SetFocus(Some(edit)) };
    }
}

fn finish(dialog: HWND, status: &str) {
    let main = app().main.get();
    crate::set_status(main, status);
    app().dialog.set(HWND::default());
    // SAFETY: windows owned by this thread. The owner is re-enabled before the dialog is
    // destroyed so activation returns to it rather than to another application.
    unsafe {
        let _ = EnableWindow(main, true);
        let _ = DestroyWindow(dialog);
        let _ = SetForegroundWindow(main);
    }
}

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            // Enter/Esc arrive as IDOK/IDCANCEL when IsDialogMessageW finds no default button.
            let (id, code) = win::split_wparam(wparam);
            let id = id as i32;
            if code == BN_CLICKED && (id == OK || id == IDOK.0) {
                finish(hwnd, &format!("Dialog: {}", value_text(hwnd)));
            } else if code == BN_CLICKED && (id == CANCEL || id == IDCANCEL.0) {
                finish(hwnd, "Dialog: cancelled");
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            finish(hwnd, "Dialog: cancelled");
            LRESULT(0)
        }
        // IsDialogMessageW asks which button Enter should press.
        DM_GETDEFID => LRESULT(((DC_HASDEFID << 16) | OK as u32) as isize),
        WM_SETFOCUS => {
            focus_value(hwnd);
            LRESULT(0)
        }
        // SAFETY: forwards the unmodified message to the default procedure.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn value_text(dialog: HWND) -> String {
    win::child(dialog, VALUE).map(win::text).unwrap_or_default()
}
