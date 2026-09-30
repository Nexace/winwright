//! Small safe wrappers over the Win32 calls this fixture repeats. Each wrapper fixes the
//! message or struct it passes, so callers never pair a message with a mismatched pointer.

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, CreateFontIndirectW, DeleteObject, GetSysColorBrush, HFONT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext, SystemParametersInfoForDpi,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetDlgItem, GetWindowLongW, GetWindowTextLengthW, GetWindowTextW,
    IDC_ARROW, LoadCursorW, NONCLIENTMETRICSW, RegisterClassExW, SPI_GETNONCLIENTMETRICS,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SendMessageW, SetWindowPos, SetWindowTextW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WNDCLASSEXW, WNDPROC,
};
use windows::core::{HSTRING, PCWSTR};

/// Reads `--title <text>` (or `--title=<text>`) from the command line.
pub fn title_arg(default: &str) -> String {
    let mut args = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned());
    while let Some(arg) = args.next() {
        if arg == "--title" {
            if let Some(value) = args.next() {
                return value;
            }
        } else if let Some(value) = arg.strip_prefix("--title=") {
            return value.to_owned();
        }
    }
    default.to_owned()
}

/// Opts into Per-Monitor-V2 so control geometry is in physical pixels. Must run before any
/// window exists; failure only means awareness was already fixed.
pub fn enable_per_monitor_dpi_awareness() {
    // SAFETY: process-wide flag; no pointers are passed.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub fn instance() -> HINSTANCE {
    // SAFETY: a null module name returns the running executable, which stays loaded.
    unsafe { GetModuleHandleW(PCWSTR::null()) }
        .map(Into::into)
        .unwrap_or_default()
}

/// Registers a window class with the dialog-face background. Panics at startup on failure.
pub fn register_class(name: PCWSTR, proc: WNDPROC) {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: proc,
        hInstance: instance(),
        // SAFETY: loads a shared system cursor; no ownership is transferred.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        // SAFETY: returns a cached system brush that must never be freed.
        hbrBackground: unsafe { GetSysColorBrush(COLOR_BTNFACE) },
        lpszClassName: name,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and its strings are 'static literals.
    let atom = unsafe { RegisterClassExW(&class) };
    assert_ne!(atom, 0, "RegisterClassExW failed");
}

/// Converts a length in DIPs to physical pixels at `dpi`, rounding to nearest.
pub fn scale(dips: i32, dpi: u32) -> i32 {
    (dips * dpi as i32 + 48) / 96
}

pub fn dpi_of(hwnd: HWND) -> u32 {
    // SAFETY: takes the HWND by value; invalid windows report 0.
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        dpi => dpi,
    }
}

/// Resizes `hwnd` so its client area is `size` DIPs at the window's current DPI.
pub fn fit_client(hwnd: HWND, size: [i32; 2], has_menu: bool) {
    let dpi = dpi_of(hwnd);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(size[0], dpi),
        bottom: scale(size[1], dpi),
    };
    // SAFETY: reads style bits of a window owned by this thread.
    let (style, ex_style) = unsafe {
        (
            WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32),
            WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32),
        )
    };
    // SAFETY: `rect` is a valid in/out pointer for the duration of the call.
    if unsafe { AdjustWindowRectExForDpi(&mut rect, style, has_menu, ex_style, dpi) }.is_ok() {
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
        // SAFETY: resizes a window owned by this thread; no pointers are passed.
        let _ = unsafe { SetWindowPos(hwnd, None, 0, 0, width, height, flags) };
    }
}

pub fn child(parent: HWND, id: i32) -> Option<HWND> {
    // SAFETY: plain handle/ID lookup; a missing control reports an error.
    unsafe { GetDlgItem(Some(parent), id) }.ok()
}

pub fn text(hwnd: HWND) -> String {
    // SAFETY: takes the HWND by value; stale handles report 0.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: `buf` is writable and the API writes at most `buf.len()` units.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

pub fn set_text(hwnd: HWND, value: &str) {
    // SAFETY: the HSTRING is null-terminated and outlives the call.
    let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(value)) };
}

/// The system message font scaled for `dpi`, so controls do not fall back to the bitmap
/// SYSTEM_FONT under Per-Monitor-V2.
pub fn message_font(dpi: u32) -> HFONT {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    let out = (&mut metrics as *mut NONCLIENTMETRICSW).cast();
    // SAFETY: `out` points to a NONCLIENTMETRICSW whose cbSize matches the size passed.
    let queried = unsafe {
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS.0, metrics.cbSize, Some(out), 0, dpi)
    };
    if queried.is_err() {
        return HFONT::default();
    }
    // SAFETY: the LOGFONTW was fully initialised by the query above.
    unsafe { CreateFontIndirectW(&metrics.lfMessageFont) }
}

/// Frees a font from [`message_font`] once no control uses it any more.
pub fn delete_font(font: HFONT) {
    if !font.is_invalid() {
        // SAFETY: callers re-font every control before deleting, so nothing references it.
        let _ = unsafe { DeleteObject(font.into()) };
    }
}

pub fn set_font(hwnd: HWND, font: HFONT) {
    if font.is_invalid() {
        return;
    }
    // SAFETY: WM_SETFONT carries the font handle by value; lparam 1 requests a redraw.
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0 as usize)),
            Some(LPARAM(1)),
        )
    };
}

/// Low and high words of a WPARAM, as used by WM_COMMAND and WM_DPICHANGED.
pub fn split_wparam(wparam: WPARAM) -> (u32, u32) {
    (
        (wparam.0 & 0xFFFF) as u32,
        ((wparam.0 >> 16) & 0xFFFF) as u32,
    )
}
