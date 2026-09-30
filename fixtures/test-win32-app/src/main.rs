#![windows_subsystem = "windows"]
//! Winwright's controlled Win32 fixture (spec §50): standard USER32/comctl32 controls with
//! fixed control IDs, which UI Automation reports as AutomationIds. Every interaction writes
//! its result into the status label (ID 140), so tests can assert through UIA alone.
//!
//! Usage: `winwright-fixture-win32 [--title <text>]`.

mod controls;
mod dialog;
mod win;

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::Dialogs::{
    CommDlgExtendedError, GetSaveFileNameW, OFN_EXPLORER, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT,
    OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows::Win32::UI::Controls::{
    ICC_TAB_CLASSES, ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, NMHDR,
    NMTREEVIEWW, TCN_SELCHANGE, TVN_SELCHANGEDW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    ACCEL, AppendMenuW, BN_CLICKED, CBN_SELCHANGE, CW_USEDEFAULT, CreateAcceleratorTableW,
    CreateMenu, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    FCONTROL, FVIRTKEY, GetMessageW, HACCEL, HMENU, IsChild, IsDialogMessageW, IsWindow, KillTimer,
    LBN_SELCHANGE, MB_OK, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, MessageBoxW, PostQuitMessage,
    SW_SHOWDEFAULT, SWP_NOACTIVATE, SWP_NOZORDER, SetTimer, SetWindowPos, ShowWindow,
    TranslateAcceleratorW, TranslateMessage, WA_INACTIVE, WINDOW_EX_STYLE, WM_ACTIVATE, WM_COMMAND,
    WM_DESTROY, WM_DPICHANGED, WM_NOTIFY, WM_SETFOCUS, WM_TIMER, WS_CLIPCHILDREN,
    WS_OVERLAPPEDWINDOW,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

const MAIN_CLASS: PCWSTR = w!("WinwrightFixtureMain");
const CMD_SAVE_AS: i32 = 1001;
const CMD_EXIT: i32 = 1002;
const CMD_ABOUT: i32 = 1003;
const DELAYED_TIMER: usize = 1;
const DELAY_MS: u32 = 1500;

/// UI-thread state. Cells only, so no borrow is ever held across a Win32 call that can
/// re-enter a window procedure.
#[derive(Default)]
pub struct App {
    pub main: Cell<HWND>,
    pub dialog: Cell<HWND>,
    pub font: Cell<HFONT>,
    last_focus: Cell<HWND>,
    target_clicks: Cell<u32>,
    recreations: Cell<u32>,
}

thread_local! {
    static APP: &'static App = Box::leak(Box::default());
}

/// The UI thread's state (the fixture has exactly one UI thread).
pub fn app() -> &'static App {
    APP.with(|app| *app)
}

fn main() {
    let title = win::title_arg("Winwright Fixture");
    win::enable_per_monitor_dpi_awareness();
    init_common_controls();
    win::register_class(MAIN_CLASS, Some(main_proc));
    dialog::register();

    let menu = build_menu().expect("create menu bar");
    // SAFETY: the class is registered above, the strings outlive the call, and the menu is
    // attached to the window, which destroys it along with itself.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            MAIN_CLASS,
            &HSTRING::from(title.as_str()),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            Some(menu),
            Some(win::instance()),
            None,
        )
    }
    .expect("create main window");
    win::fit_client(hwnd, controls::CLIENT_SIZE, true);
    let dpi = win::dpi_of(hwnd);
    let font = win::message_font(dpi);
    app().main.set(hwnd);
    app().font.set(font);
    for spec in controls::MAIN {
        controls::create(hwnd, spec, dpi, font).expect("create control");
    }
    controls::populate(hwnd);
    let accelerators = accelerators();
    // SAFETY: shows our own window, honouring the launcher's STARTUPINFO show command.
    let _ = unsafe { ShowWindow(hwnd, SW_SHOWDEFAULT) };
    run(hwnd, accelerators);
}

fn init_common_controls() {
    let init = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_TAB_CLASSES | ICC_TREEVIEW_CLASSES,
    };
    // SAFETY: `init` is correctly sized and outlives the call.
    let ok = unsafe { InitCommonControlsEx(&init) };
    assert!(ok.as_bool(), "InitCommonControlsEx failed");
}

fn build_menu() -> windows::core::Result<HMENU> {
    // SAFETY: freshly created menus receive 'static literals; the popups become owned by the
    // bar, and the bar by the main window once passed to CreateWindowExW.
    unsafe {
        let file = CreatePopupMenu()?;
        AppendMenuW(
            file,
            MF_STRING,
            CMD_SAVE_AS as usize,
            w!("Save &As...\tCtrl+S"),
        )?;
        AppendMenuW(file, MF_SEPARATOR, 0, PCWSTR::null())?;
        AppendMenuW(file, MF_STRING, CMD_EXIT as usize, w!("E&xit"))?;
        let help = CreatePopupMenu()?;
        AppendMenuW(help, MF_STRING, CMD_ABOUT as usize, w!("&About"))?;
        let bar = CreateMenu()?;
        AppendMenuW(bar, MF_POPUP, file.0 as usize, w!("&File"))?;
        AppendMenuW(bar, MF_POPUP, help.0 as usize, w!("&Help"))?;
        Ok(bar)
    }
}

fn accelerators() -> HACCEL {
    let table = [ACCEL {
        fVirt: FCONTROL | FVIRTKEY,
        key: u16::from(b'S'),
        cmd: CMD_SAVE_AS as u16,
    }];
    // SAFETY: the call copies `table` into a new accelerator table.
    unsafe { CreateAcceleratorTableW(&table) }.expect("create accelerator table")
}

/// Message loop: modal dialog first, then Ctrl+S, then Tab/Enter navigation for the main
/// window, then ordinary dispatch.
fn run(main: HWND, accelerators: HACCEL) {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid out pointer for the duration of the call.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if got == 0 || got == -1 {
            return;
        }
        if dialog::translate(&msg) {
            continue;
        }
        // SAFETY: `msg` came from GetMessageW and the table is live for the whole loop.
        if unsafe { TranslateAcceleratorW(main, accelerators, &msg) } != 0 {
            continue;
        }
        // SAFETY: `msg` came from GetMessageW; stale windows simply report false.
        if unsafe { IsDialogMessageW(main, &msg) }.as_bool() {
            continue;
        }
        // SAFETY: standard dispatch of a message retrieved by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Writes `text` into the status label (ID 140).
pub fn set_status(main: HWND, text: &str) {
    if let Some(status) = win::child(main, controls::STATUS) {
        win::set_text(status, text);
    }
}

fn control_text(main: HWND, id: i32) -> String {
    win::child(main, id).map(win::text).unwrap_or_default()
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let (id, code) = win::split_wparam(wparam);
            on_command(hwnd, id as i32, code);
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: for WM_NOTIFY, lparam points to a notification struct that starts with
            // an NMHDR and stays valid for the duration of this call.
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            match (header.idFrom as i32, header.code) {
                (controls::TABS, TCN_SELCHANGE) => {
                    if let Some(name) = controls::selected_tab(header.hwndFrom) {
                        set_status(hwnd, &format!("Tab: {name}"));
                    }
                }
                (controls::TREE, TVN_SELCHANGEDW) => {
                    // SAFETY: TVN_SELCHANGEDW from our Unicode tree view carries an NMTREEVIEWW.
                    let item = unsafe { &*(lparam.0 as *const NMTREEVIEWW) }.itemNew.hItem;
                    let text = controls::tree_item_text(header.hwndFrom, item);
                    set_status(hwnd, &format!("Tree: {text}"));
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == DELAYED_TIMER => {
            add_delayed(hwnd);
            LRESULT(0)
        }
        WM_DPICHANGED => {
            // SAFETY: for WM_DPICHANGED, lparam points to the suggested window RECT.
            let suggested = unsafe { *(lparam.0 as *const RECT) };
            on_dpi_changed(hwnd, win::split_wparam(wparam).1, suggested);
            LRESULT(0)
        }
        WM_ACTIVATE => {
            if win::split_wparam(wparam).0 == WA_INACTIVE {
                remember_focus(hwnd);
            }
            // SAFETY: forwards the unmodified message to the default procedure.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_SETFOCUS => {
            restore_focus(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: cancels our own timer (if armed) and ends this thread's message loop.
            unsafe {
                let _ = KillTimer(Some(hwnd), DELAYED_TIMER);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        // SAFETY: forwards the unmodified message to the default procedure.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn on_command(hwnd: HWND, id: i32, code: u32) {
    let status = match (id, code) {
        (controls::SUBMIT, BN_CLICKED) => {
            format!("Submitted: {}", control_text(hwnd, controls::NAME))
        }
        (controls::FEATURE, BN_CLICKED) => {
            let on = win::child(hwnd, controls::FEATURE).is_some_and(controls::is_checked);
            format!("Feature: {}", if on { "on" } else { "off" })
        }
        (controls::SMALL, BN_CLICKED) => "Size: Small".to_owned(),
        (controls::LARGE, BN_CLICKED) => "Size: Large".to_owned(),
        (controls::COLOR, CBN_SELCHANGE) => {
            match win::child(hwnd, controls::COLOR).and_then(controls::combo_selection) {
                Some(color) => format!("Color: {color}"),
                None => return,
            }
        }
        (controls::ITEMS, LBN_SELCHANGE) => {
            match win::child(hwnd, controls::ITEMS).and_then(controls::list_selection) {
                Some(item) => format!("Selected: {item}"),
                None => return,
            }
        }
        (controls::OPEN_DIALOG, BN_CLICKED) => match dialog::open(hwnd) {
            Ok(()) => return,
            Err(err) => format!("Dialog failed: {err}"),
        },
        (controls::ADD_DELAYED, BN_CLICKED) => {
            // SAFETY: arms a one-shot timer on our own window; with no callback the system
            // posts WM_TIMER, handled in `main_proc`.
            unsafe { SetTimer(Some(hwnd), DELAYED_TIMER, DELAY_MS, None) };
            return;
        }
        (controls::DELAYED, BN_CLICKED) => "Delayed clicked".to_owned(),
        (controls::RECREATE, BN_CLICKED) => recreate_target(hwnd),
        (controls::TARGET, BN_CLICKED) => {
            let clicks = app().target_clicks.get() + 1;
            app().target_clicks.set(clicks);
            format!("Target clicked {clicks}")
        }
        // Menu items (code 0) and the Ctrl+S accelerator (code 1).
        (CMD_SAVE_AS, _) => save_as(hwnd),
        (CMD_EXIT, _) => {
            // SAFETY: destroys our own main window; WM_DESTROY ends the loop.
            let _ = unsafe { DestroyWindow(hwnd) };
            return;
        }
        (CMD_ABOUT, _) => {
            // SAFETY: modal message box owned by our window; literals are 'static.
            unsafe { MessageBoxW(Some(hwnd), w!("Winwright Fixture"), w!("About"), MB_OK) };
            return;
        }
        _ => return,
    };
    set_status(hwnd, &status);
}

fn add_delayed(hwnd: HWND) {
    // SAFETY: cancels our own timer so it fires once.
    let _ = unsafe { KillTimer(Some(hwnd), DELAYED_TIMER) };
    if win::child(hwnd, controls::DELAYED).is_some() {
        return;
    }
    let dpi = win::dpi_of(hwnd);
    match controls::create(hwnd, &controls::DELAYED_SPEC, dpi, app().font.get()) {
        Ok(button) => controls::order_after(button, win::child(hwnd, controls::ADD_DELAYED)),
        Err(err) => set_status(hwnd, &format!("Delayed failed: {err}")),
    }
}

/// Replaces the Target button with a new window (new HWND, new UIA runtime ID) at the same
/// place; the click count lives in `App`, so it survives.
fn recreate_target(hwnd: HWND) -> String {
    if let Some(old) = win::child(hwnd, controls::TARGET) {
        // SAFETY: destroys a child window owned by this thread.
        let _ = unsafe { DestroyWindow(old) };
    }
    let dpi = win::dpi_of(hwnd);
    match controls::create(hwnd, &controls::TARGET_SPEC, dpi, app().font.get()) {
        Ok(button) => {
            controls::order_after(button, win::child(hwnd, controls::RECREATE));
            let count = app().recreations.get() + 1;
            app().recreations.set(count);
            format!("Recreated {count}")
        }
        Err(err) => format!("Recreate failed: {err}"),
    }
}

/// File > Save As / Ctrl+S: writes the Notes text as UTF-8 to the chosen path.
fn save_as(hwnd: HWND) -> String {
    let filter: Vec<u16> = "Text files (*.txt)\0*.txt\0\0".encode_utf16().collect();
    let mut file = vec![0u16; 1024];
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrDefExt: w!("txt"),
        Flags: OFN_EXPLORER | OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: every pointer in `dialog` refers to `filter`, `file`, or a 'static literal,
    // all of which outlive this modal call.
    let chosen = unsafe { GetSaveFileNameW(&mut dialog) }.as_bool();
    if !chosen {
        // SAFETY: reads this thread's last common-dialog error; no arguments.
        let error = unsafe { CommDlgExtendedError() }.0;
        return match error {
            0 => "Save cancelled".to_owned(),
            code => format!("Save failed: dialog error {code:#x}"),
        };
    }
    let len = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    let path = String::from_utf16_lossy(&file[..len]);
    match std::fs::write(&path, control_text(hwnd, controls::NOTES)) {
        Ok(()) => format!("Saved: {path}"),
        Err(err) => format!("Save failed: {err}"),
    }
}

fn on_dpi_changed(hwnd: HWND, dpi: u32, suggested: RECT) {
    let (width, height) = (
        suggested.right - suggested.left,
        suggested.bottom - suggested.top,
    );
    let flags = SWP_NOZORDER | SWP_NOACTIVATE;
    // SAFETY: moves our own window to the rectangle Windows suggested; no pointers.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            suggested.left,
            suggested.top,
            width,
            height,
            flags,
        )
    };
    let font = win::message_font(dpi);
    for spec in controls::MAIN.iter().chain([&controls::DELAYED_SPEC]) {
        controls::place(hwnd, spec, dpi, font);
    }
    dialog::apply_font(font);
    win::delete_font(app().font.replace(font));
}

/// Dialog-style focus memory: keyboard focus returns to the last focused control when the
/// window is re-activated, defaulting to the Name edit.
fn remember_focus(hwnd: HWND) {
    // SAFETY: no arguments; returns this thread's focus window or null.
    let focus = unsafe { GetFocus() };
    // SAFETY: takes both handles by value; unrelated or null handles report false.
    if unsafe { IsChild(hwnd, focus) }.as_bool() {
        app().last_focus.set(focus);
    }
}

fn restore_focus(hwnd: HWND) {
    let last = app().last_focus.get();
    // SAFETY: takes both handles by value; destroyed controls report false.
    let alive = unsafe { IsWindow(Some(last)).as_bool() && IsChild(hwnd, last).as_bool() };
    let target = if alive {
        Some(last)
    } else {
        win::child(hwnd, controls::NAME)
    };
    if let Some(target) = target {
        // SAFETY: focuses a child of our own active window; no pointers.
        let _ = unsafe { SetFocus(Some(target)) };
    }
}
