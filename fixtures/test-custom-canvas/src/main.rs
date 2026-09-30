#![windows_subsystem = "windows"]
//! Winwright's deliberately inaccessible fixture (spec §50): one custom-drawn window with no
//! child controls, so UI Automation sees nothing but an empty pane. Three coloured squares
//! are painted in the client area, and every input is reported in the window title as
//! `<title> - <event>`, readable with plain `GetWindowTextW`.
//!
//! Usage: `winwright-fixture-canvas [--title <text>]`.

use std::cell::{Cell, OnceCell, RefCell};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DC_BRUSH, EndPaint, FillRect, GetStockObject, HBRUSH, HDC, InvalidateRect,
    PAINTSTRUCT, SetDCBrushColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW,
    DispatchMessageW, GetClientRect, GetMessageW, IDC_ARROW, LoadCursorW, MSG, PostQuitMessage,
    RegisterClassExW, SW_SHOWDEFAULT, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_CAPTURECHANGED, WM_CHAR,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEWHEEL, WM_PAINT, WM_RBUTTONUP, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, PCWSTR, w};

const CLASS: PCWSTR = w!("WinwrightFixtureCanvas");
/// Client area in DIPs.
const CLIENT_SIZE: [i32; 2] = [520, 220];
const BACKGROUND: COLORREF = rgb(211, 211, 211);
/// Painted squares: name, `[left, top, right, bottom]` in DIPs, fill colour.
const SQUARES: [(&str, [i32; 4], COLORREF); 3] = [
    ("red", [40, 40, 160, 160], rgb(255, 0, 0)),
    ("green", [200, 40, 320, 160], rgb(0, 255, 0)),
    ("blue", [360, 40, 480, 160], rgb(0, 0, 255)),
];
/// A left press/release pair further apart than this (physical pixels) is a drag.
const DRAG_THRESHOLD: i32 = 10;
const TYPED_TAIL: usize = 40;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// UI-thread state. Borrows of `typed` never span a Win32 call.
#[derive(Default)]
struct Canvas {
    title: OnceCell<String>,
    press: Cell<Option<(i32, i32)>>,
    typed: RefCell<String>,
    high_surrogate: Cell<Option<u16>>,
    wheel: Cell<i32>,
}

thread_local! {
    static CANVAS: &'static Canvas = Box::leak(Box::default());
}

fn canvas() -> &'static Canvas {
    CANVAS.with(|canvas| *canvas)
}

fn main() {
    let title = title_arg("Winwright Canvas");
    let _ = canvas().title.set(title.clone());
    // SAFETY: process-wide flag set before any window exists; no pointers are passed.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // SAFETY: a null module name returns the running executable, which stays loaded.
    let instance: HINSTANCE = unsafe { GetModuleHandleW(PCWSTR::null()) }
        .expect("module handle")
        .into();
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(canvas_proc),
        hInstance: instance,
        // SAFETY: loads a shared system cursor; no ownership is transferred.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: CLASS,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and its strings are 'static literals.
    let atom = unsafe { RegisterClassExW(&class) };
    assert_ne!(atom, 0, "RegisterClassExW failed");
    // SAFETY: the class is registered above and the strings outlive the call.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            &HSTRING::from(title.as_str()),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .expect("create canvas window");
    fit_client(hwnd);
    // SAFETY: shows our own window, honouring the launcher's STARTUPINFO show command.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWDEFAULT) };

    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid out pointer; 0 means WM_QUIT and -1 an error.
    while matches!(unsafe { GetMessageW(&mut msg, None, 0, 0) }.0, 1..) {
        // SAFETY: standard translation (needed for WM_CHAR) and dispatch of a retrieved message.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn title_arg(default: &str) -> String {
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

fn dpi_of(hwnd: HWND) -> u32 {
    // SAFETY: takes the HWND by value; invalid windows report 0.
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        dpi => dpi,
    }
}

fn scale(dips: i32, dpi: u32) -> i32 {
    (dips * dpi as i32 + 48) / 96
}

fn scaled(rect: [i32; 4], dpi: u32) -> RECT {
    let [left, top, right, bottom] = rect.map(|v| scale(v, dpi));
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

/// Sizes the window so its client area is [`CLIENT_SIZE`] DIPs at its current DPI.
fn fit_client(hwnd: HWND) {
    let dpi = dpi_of(hwnd);
    let mut rect = scaled([0, 0, CLIENT_SIZE[0], CLIENT_SIZE[1]], dpi);
    let style = WS_OVERLAPPEDWINDOW;
    let ex_style = WINDOW_EX_STYLE::default();
    // SAFETY: `rect` is a valid in/out pointer for the duration of the call.
    if unsafe { AdjustWindowRectExForDpi(&mut rect, style, false, ex_style, dpi) }.is_ok() {
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let flags = SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE;
        // SAFETY: resizes our own window; no pointers.
        let _ = unsafe { SetWindowPos(hwnd, None, 0, 0, width, height, flags) };
    }
}

/// Name of the square under a client-area point (physical pixels), or `none`.
fn hit(hwnd: HWND, (x, y): (i32, i32)) -> &'static str {
    let dpi = dpi_of(hwnd);
    SQUARES
        .iter()
        .find(|(_, rect, _)| {
            let r = scaled(*rect, dpi);
            (r.left..r.right).contains(&x) && (r.top..r.bottom).contains(&y)
        })
        .map_or("none", |(name, _, _)| name)
}

/// Replaces the title with `<title> - <event>`.
fn report(hwnd: HWND, event: &str) {
    let base = canvas().title.get().map_or("", String::as_str);
    let title = HSTRING::from(format!("{base} - {event}"));
    // SAFETY: the HSTRING is null-terminated and outlives the call.
    let _ = unsafe { SetWindowTextW(hwnd, &title) };
}

/// Client coordinates packed into a mouse message's LPARAM (signed 16-bit each).
fn point(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16;
    (i32::from(x), i32::from(y))
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: `ps` is a valid out pointer; the paint cycle is closed by EndPaint below.
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut client = RECT::default();
    // SAFETY: `client` is a valid out pointer.
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    fill(hdc, &client, BACKGROUND);
    let dpi = dpi_of(hwnd);
    for (_, rect, color) in SQUARES {
        fill(hdc, &scaled(rect, dpi), color);
    }
    // SAFETY: ends the paint cycle begun above with the same struct.
    let _ = unsafe { EndPaint(hwnd, &ps) };
}

fn fill(hdc: HDC, rect: &RECT, color: COLORREF) {
    // SAFETY: `hdc` is the live paint DC; DC_BRUSH is a stock object (never freed) that
    // paints with the colour most recently set on this DC.
    unsafe {
        SetDCBrushColor(hdc, color);
        FillRect(hdc, rect, HBRUSH(GetStockObject(DC_BRUSH).0));
    }
}

fn on_left_down(hwnd: HWND, at: (i32, i32)) {
    canvas().press.set(Some(at));
    // SAFETY: captures the mouse for our own window so the release is seen even outside it.
    unsafe { SetCapture(hwnd) };
}

fn on_left_up(hwnd: HWND, at: (i32, i32)) {
    // No recorded press: this is the release that follows a double click.
    let Some(start) = canvas().press.take() else {
        return;
    };
    // SAFETY: releases the capture taken in `on_left_down`.
    let _ = unsafe { ReleaseCapture() };
    let moved = (at.0 - start.0).pow(2) + (at.1 - start.1).pow(2);
    if moved > DRAG_THRESHOLD.pow(2) {
        let (from, to) = (hit(hwnd, start), hit(hwnd, at));
        report(hwnd, &format!("dragged {from} to {to}"));
    } else {
        report(hwnd, &format!("clicked {}", hit(hwnd, start)));
    }
}

fn on_char(hwnd: HWND, unit: u16) {
    let canvas = canvas();
    let shown = {
        let mut typed = canvas.typed.borrow_mut();
        match unit {
            0x08 => {
                typed.pop();
            }
            0x0D => typed.push('⏎'),
            0xD800..=0xDBFF => {
                canvas.high_surrogate.set(Some(unit));
                return;
            }
            0xDC00..=0xDFFF => {
                let pair = canvas
                    .high_surrogate
                    .take()
                    .and_then(|high| char::decode_utf16([high, unit]).next()?.ok());
                match pair {
                    Some(c) => typed.push(c),
                    None => return,
                }
            }
            // Other control characters, e.g. the 0x0B that Ctrl+K produces.
            u if u < 0x20 || u == 0x7F => return,
            u => typed.extend(char::from_u32(u32::from(u))),
        }
        let count = typed.chars().count();
        typed
            .chars()
            .skip(count.saturating_sub(TYPED_TAIL))
            .collect::<String>()
    };
    report(hwnd, &format!("typed: {shown}"));
}

fn ctrl_held() -> bool {
    // SAFETY: reads this thread's key state; no pointers.
    let state = unsafe { GetKeyState(i32::from(VK_CONTROL.0)) };
    state < 0
}

unsafe extern "system" fn canvas_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => paint(hwnd),
        // The whole client area is painted in WM_PAINT.
        WM_ERASEBKGND => return LRESULT(1),
        WM_LBUTTONDOWN => on_left_down(hwnd, point(lparam)),
        WM_LBUTTONUP => on_left_up(hwnd, point(lparam)),
        WM_LBUTTONDBLCLK => {
            canvas().press.set(None);
            report(
                hwnd,
                &format!("double-clicked {}", hit(hwnd, point(lparam))),
            );
        }
        WM_RBUTTONUP => report(hwnd, &format!("right-clicked {}", hit(hwnd, point(lparam)))),
        WM_CAPTURECHANGED => canvas().press.set(None),
        WM_CHAR => on_char(hwnd, wparam.0 as u16),
        WM_KEYDOWN if wparam.0 == usize::from(b'K') && ctrl_held() => {
            report(hwnd, "chord ctrl+k");
        }
        WM_MOUSEWHEEL => {
            let delta = i32::from(((wparam.0 >> 16) & 0xFFFF) as u16 as i16);
            let total = canvas().wheel.get() + delta;
            canvas().wheel.set(total);
            report(hwnd, &format!("wheel {total}"));
        }
        WM_DPICHANGED => {
            // SAFETY: for WM_DPICHANGED, lparam points to the suggested window RECT.
            let r = unsafe { *(lparam.0 as *const RECT) };
            let flags = SWP_NOZORDER | SWP_NOACTIVATE;
            // SAFETY: moves our own window to the suggested rectangle and repaints it.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    flags,
                );
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        // SAFETY: ends this thread's message loop.
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        // SAFETY: forwards the unmodified message to the default procedure.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}
