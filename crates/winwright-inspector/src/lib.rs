//! Native Inspector (spec §29): choose a window, browse its UI Automation tree, see every
//! property of an element, pick the element under the cursor, highlight it on screen, and
//! copy a locator for the CLI/MCP.
//!
//! Real Win32 controls (so the Inspector itself stays accessible) painted with Winwright's
//! own look: the shared theme from `winwright_overlay::theme`, light/dark/high-contrast
//! aware, per-monitor DPI aware. No WebView.
//!
//! Read-only by design: the Inspector never clicks, types, or changes anything.
//!
//! Keys: Ctrl+F filter, F5 refresh, Ctrl+C copy locator (tree focused), Esc cancel a pick.

mod details;
mod text;

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;

use windows::Win32::Foundation::{
    GlobalFree, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush,
    DRAW_TEXT_FORMAT, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC,
    DeleteObject, EndPaint, HBRUSH, HDC, HGDIOBJ, InvalidateRect, PAINTSTRUCT, SRCCOPY,
    ScreenToClient, SelectObject, SetBkColor, SetTextColor, UpdateWindow,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED,
    CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT, DRAWITEMSTRUCT, EM_SETCUEBANNER,
    HTREEITEM, ICC_STANDARD_CLASSES, ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX,
    InitCommonControlsEx, MEASUREITEMSTRUCT, NM_CUSTOMDRAW, NMCUSTOMDRAW, NMHDR, NMTREEVIEWW,
    NMTVCUSTOMDRAW, ODS_COMBOBOXEDIT, ODS_SELECTED, ODT_COMBOBOX, SetWindowTheme, TVE_EXPAND,
    TVGN_CARET, TVI_LAST, TVI_ROOT, TVIF_PARAM, TVIF_TEXT, TVINSERTSTRUCTW, TVINSERTSTRUCTW_0,
    TVIS_EXPANDED, TVITEMW, TVM_DELETEITEM, TVM_ENSUREVISIBLE, TVM_EXPAND, TVM_GETINDENT,
    TVM_GETITEMRECT, TVM_GETITEMSTATE, TVM_INSERTITEMW, TVM_SELECTITEM, TVM_SETBKCOLOR,
    TVM_SETEXTENDEDSTYLE, TVM_SETINDENT, TVM_SETITEMHEIGHT, TVM_SETTEXTCOLOR, TVN_SELCHANGEDW,
    TVS_EX_DOUBLEBUFFER, TVS_FULLROWSELECT, TVS_HASBUTTONS, TVS_LINESATROOT, TVS_NOHSCROLL,
    TVS_SHOWSELALWAYS, TVS_TRACKSELECT, WC_TREEVIEWW, WM_MOUSELEAVE,
};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CONTROL, VK_ESCAPE, VK_F5, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, CB_SETITEMHEIGHT, CBS_DROPDOWNLIST,
    CBS_HASSTRINGS, CBS_OWNERDRAWFIXED, CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyIcon,
    DestroyWindow, DispatchMessageW, DrawIconEx, EN_CHANGE, EN_KILLFOCUS, EN_SETFOCUS,
    ES_AUTOHSCROLL, GA_ROOT, GCLP_HICON, GCLP_HICONSM, GetAncestor, GetClassLongPtrW,
    GetClientRect, GetCursorPos, GetMessageW, GetWindowTextW, GetWindowThreadProcessId, HICON,
    HMENU, ICON_BIG, ICON_SMALL, ICON_SMALL2, IDC_SIZEWE, IDC_WAIT, IsDialogMessageW, KillTimer,
    LoadCursorW, MINMAXINFO, MSG, MoveWindow, PostMessageW, PostQuitMessage, SM_CXICON,
    SM_CXSMICON, SMTO_ABORTIFHUNG, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageTimeoutW,
    SendMessageW, SetCursor, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CAPTURECHANGED, WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_DESTROY, WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_GETICON,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MEASUREITEM, WM_MOUSEMOVE,
    WM_NOTIFY, WM_PAINT, WM_SETCURSOR, WM_SETFONT, WM_SETICON, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER,
    WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
    WindowFromPoint,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};
use winwright_contracts::action::ElementTarget;
use winwright_contracts::element::{ElementDetails, ElementInfo};
use winwright_contracts::ids::SessionId;
use winwright_contracts::overlay::{HighlightRequest, OverlayStyle};
use winwright_contracts::snapshot::{SnapshotNode, SnapshotRequest, SnapshotTarget};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_core::session::Session;
use winwright_core::{Engine, InspectRequest};
use winwright_overlay::theme::{self, ButtonKind, ButtonState, Fonts, Palette, glyph};

use crate::details::Model;
use crate::text::RowView;

const CLASS: PCWSTR = w!("WinwrightInspector");
const ID_COMBO: i32 = 100;
const ID_REFRESH: i32 = 101;
const ID_PICK: i32 = 102;
const ID_HIGHLIGHT: i32 = 103;
const ID_COPY: i32 = 104;
const ID_TREE: i32 = 105;
const ID_FILTER: i32 = 106;
const ID_DETAILS: i32 = 107;
const ID_COPY_ALL: i32 = 108;
const ID_FIND: i32 = 109;
const ID_CANCEL_PICK: i32 = 110;
const TIMER_PICK: usize = 1;
const TIMER_FILTER: usize = 2;
const TIMER_SELECT: usize = 3;
const PICK_SECONDS: u32 = 3;
const HIGHLIGHT_MS: u64 = 1_500;
const PICK_LABEL: &str = "Pick element";

// Layout, in device-independent pixels.
const TOOLBAR_H: i32 = 60;
const STATUS_H: i32 = 32;
const PAD: i32 = 14;
const GAP: i32 = 12;
const ROW_H: i32 = 28;
const INDENT: i32 = 18;
const BUTTON_H: i32 = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Info,
    Busy,
    Good,
    Bad,
}

#[derive(Clone, Copy)]
struct Controls {
    combo: HWND,
    refresh: HWND,
    pick: HWND,
    highlight: HWND,
    copy: HWND,
    tree: HWND,
    filter: HWND,
    details: HWND,
}

/// Engine state and data; used by commands.
struct App {
    engine: Engine,
    rt: tokio::runtime::Runtime,
    session: Arc<Session>,
    main: HWND,
    c: Controls,
    windows: Vec<WindowInfo>,
    nodes: Vec<SnapshotNode>,
    items: Vec<ElementInfo>,
    handles: Vec<HTREEITEM>,
    selected_window: Option<String>,
    /// The window whose tree is shown.
    loaded: Option<u64>,
    current: Option<ElementDetails>,
    locator: Option<String>,
    countdown: u32,
    pending_select: Option<usize>,
}

struct Brush(HBRUSH);

impl Brush {
    fn new(rgb: u32) -> Self {
        // SAFETY: plain GDI brush creation; deleted on drop.
        Self(unsafe { CreateSolidBrush(theme::cr(rgb)) })
    }
}

impl Drop for Brush {
    fn drop(&mut self) {
        // SAFETY: we created this brush and it is not selected into any DC.
        let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
    }
}

struct ComboRow {
    title: String,
    process: String,
    icon: Option<HICON>,
}

#[derive(Default, Clone, Copy)]
struct Rects {
    left: RECT,
    right: RECT,
    filter: RECT,
    splitter: RECT,
    status: RECT,
}

/// What painting needs; never borrowed across engine calls.
struct Ui {
    main: HWND,
    c: Controls,
    palette: Palette,
    fonts: Rc<Fonts>,
    window_brush: Brush,
    surface_brush: Brush,
    rows: Vec<RowView>,
    combo_rows: Vec<ComboRow>,
    status: String,
    tone: Tone,
    count: String,
    split: f32,
    splitter_hot: bool,
    dragging: bool,
    filter_focused: bool,
    tracking_leave: bool,
    own_icons: Vec<HICON>,
    rects: Rects,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Runs `f` with the app unless it is already borrowed (re-entrant notifications are skipped).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

fn platform(operation: &str, err: &windows::core::Error) -> WinwrightError {
    WinwrightError::Platform {
        operation: operation.to_owned(),
        hresult: err.code().0,
    }
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` outlives the call.
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// Runs the Inspector window on the calling thread until it is closed.
/// Must not be called from inside a Tokio runtime (it drives its own).
pub fn run(engine: Engine) -> WinwrightResult<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| WinwrightError::BackendUnavailable {
            backend: "inspector".into(),
            reason: e.to_string(),
        })?;
    let session = engine.session(&SessionId::parse("inspector").expect("valid"), "inspector")?;
    // SAFETY: plain initialization with a fully initialized struct.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TREEVIEW_CLASSES | ICC_STANDARD_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
    }
    let hinstance = theme::register_class(CLASS, Some(wndproc))?;
    // SAFETY: the class is registered; the window is shown after its children exist.
    let main = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Winwright Inspector"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            80,
            80,
            1240,
            800,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| platform("CreateWindowExW(inspector)", &e))?;
    // SAFETY: plain DPI query of the new window.
    let dpi = unsafe { GetDpiForWindow(main) }.max(96);
    let palette = Palette::system();
    let fonts = Rc::new(Fonts::new(dpi));
    let c = create_children(hinstance, main, palette, Rc::clone(&fonts))?;
    let own_icons = set_window_icons(main, dpi);
    UI.with(|cell| {
        *cell.borrow_mut() = Some(Ui {
            main,
            c,
            palette,
            fonts,
            window_brush: Brush::new(palette.window),
            surface_brush: Brush::new(palette.surface),
            rows: Vec::new(),
            combo_rows: Vec::new(),
            status: "Ready".into(),
            tone: Tone::Info,
            count: String::new(),
            split: 0.44,
            splitter_hot: false,
            dragging: false,
            filter_focused: false,
            tracking_leave: false,
            own_icons,
            rects: Rects::default(),
        });
    });
    APP.with(|cell| {
        *cell.borrow_mut() = Some(App {
            engine,
            rt,
            session,
            main,
            c,
            windows: Vec::new(),
            nodes: Vec::new(),
            items: Vec::new(),
            handles: Vec::new(),
            selected_window: None,
            loaded: None,
            current: None,
            locator: None,
            countdown: 0,
            pending_select: None,
        });
    });
    apply_theme();
    apply_fonts();
    // Size for the DPI, then lay out and show.
    // SAFETY: resizes and shows this thread's own window.
    unsafe {
        let _ = SetWindowPos(
            main,
            None,
            0,
            0,
            theme::scale(1240, dpi),
            theme::scale(800, dpi),
            SWP_NOZORDER | SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
        );
    }
    layout();
    details::set_model(Model::Empty, "Nothing selected");
    // SAFETY: shows our own window.
    unsafe {
        let _ = ShowWindow(main, SW_SHOW);
        let _ = UpdateWindow(main);
    }
    with_app(|app| {
        app.sync_actions();
        app.load_windows();
    });
    message_loop(main);
    UI.with(|cell| {
        if let Some(ui) = cell.borrow_mut().take() {
            for icon in &ui.own_icons {
                // SAFETY: our own icons; the window that used them is gone.
                let _ = unsafe { DestroyIcon(*icon) };
            }
        }
    });
    APP.with(|cell| cell.borrow_mut().take());
    Ok(())
}

fn message_loop(main: HWND) {
    let mut msg = MSG::default();
    // SAFETY: standard message loop on the thread that owns the windows.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_KEYDOWN {
                // AltGr is Ctrl+Alt: it types characters, it is not a shortcut.
                let ctrl =
                    GetKeyState(VK_CONTROL.0 as i32) < 0 && GetKeyState(VK_MENU.0 as i32) >= 0;
                let tree_focused = with_ui(|ui| ui.c.tree) == Some(GetFocus());
                let picking = with_app(|app| app.countdown > 0) == Some(true);
                let command = shortcut(ctrl, msg.wParam.0 as u16, tree_focused, picking);
                if let Some(id) = command {
                    let _ = PostMessageW(Some(main), WM_COMMAND, WPARAM(id as usize), LPARAM(0));
                    continue;
                }
            }
            if !IsDialogMessageW(main, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// The Inspector's own command for a key press, if any. Esc is taken only while picking,
/// so it still closes the window list and reaches the other controls.
fn shortcut(ctrl: bool, key: u16, tree_focused: bool, picking: bool) -> Option<i32> {
    match (ctrl, key) {
        (true, k) if k == u16::from(b'F') => Some(ID_FIND),
        (false, k) if k == VK_F5.0 => Some(ID_REFRESH),
        (true, k) if k == u16::from(b'C') && tree_focused => Some(ID_COPY),
        (false, k) if k == VK_ESCAPE.0 && picking => Some(ID_CANCEL_PICK),
        _ => None,
    }
}

/// Width shared by the two panes: the client width minus the outer padding and the gap.
fn panes_width(client_w: i32, pad: i32, gap: i32, min: i32) -> i32 {
    (client_w - pad * 2 - gap).max(min)
}

/// The split that puts the splitter's center under `x` (the inverse of `layout`).
fn split_at(x: i32, panes: i32, pad: i32, gap: i32) -> f32 {
    ((x - pad - gap / 2) as f32 / panes.max(1) as f32).clamp(0.25, 0.72)
}

fn create_children(
    hinstance: HINSTANCE,
    main: HWND,
    palette: Palette,
    fonts: Rc<Fonts>,
) -> WinwrightResult<Controls> {
    let child = |class: PCWSTR, text: PCWSTR, style: u32, id: i32| {
        // SAFETY: creates a child of `main` on this thread; the menu handle carries the id.
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                text,
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
                0,
                0,
                10,
                10,
                Some(main),
                Some(HMENU(id as isize as _)),
                Some(hinstance),
                None,
            )
        }
        .map_err(|e| platform("CreateWindowExW(child)", &e))
    };
    let combo = child(
        w!("COMBOBOX"),
        w!("Window"),
        (CBS_DROPDOWNLIST | CBS_OWNERDRAWFIXED | CBS_HASSTRINGS) as u32
            | WS_VSCROLL.0
            | WS_TABSTOP.0,
        ID_COMBO,
    )?;
    let button = |text: PCWSTR, id: i32| child(w!("BUTTON"), text, WS_TABSTOP.0, id);
    let refresh = button(w!("Refresh"), ID_REFRESH)?;
    let filter = child(
        w!("EDIT"),
        w!(""),
        ES_AUTOHSCROLL as u32 | WS_TABSTOP.0,
        ID_FILTER,
    )?;
    let tree = child(
        WC_TREEVIEWW,
        w!("Elements"),
        TVS_HASBUTTONS
            | TVS_LINESATROOT
            | TVS_SHOWSELALWAYS
            | TVS_FULLROWSELECT
            | TVS_TRACKSELECT
            | TVS_NOHSCROLL
            | WS_TABSTOP.0,
        ID_TREE,
    )?;
    let pick = button(w!("Pick element"), ID_PICK)?;
    let highlight = button(w!("Highlight"), ID_HIGHLIGHT)?;
    let copy = button(w!("Copy locator"), ID_COPY)?;
    let details = details::create(main, ID_DETAILS, ID_COPY, ID_COPY_ALL, palette, fonts)?;
    // SAFETY: plain messages to our own new children; the cue string is static.
    unsafe {
        SendMessageW(
            tree,
            TVM_SETEXTENDEDSTYLE,
            Some(WPARAM(TVS_EX_DOUBLEBUFFER as usize)),
            Some(LPARAM(TVS_EX_DOUBLEBUFFER as isize)),
        );
        SendMessageW(
            filter,
            EM_SETCUEBANNER,
            Some(WPARAM(1)),
            Some(LPARAM(
                w!("Filter by name, role, or id   (Ctrl+F)").as_ptr() as isize,
            )),
        );
    }
    Ok(Controls {
        combo,
        refresh,
        pick,
        highlight,
        copy,
        tree,
        filter,
        details,
    })
}

fn set_window_icons(hwnd: HWND, dpi: u32) -> Vec<HICON> {
    let mut icons = Vec::new();
    // SAFETY: metric queries and WM_SETICON with icons we own until exit.
    unsafe {
        for (which, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            let size = GetSystemMetricsForDpi(metric, dpi);
            if let Ok(icon) = theme::brand_icon(size, false) {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(which as usize)),
                    Some(LPARAM(icon.0 as isize)),
                );
                icons.push(icon);
            }
        }
    }
    icons
}

/// Colors: window chrome, control themes, tree colors, brushes, details panel.
fn apply_theme() {
    let Some((main, c, p)) = with_ui(|ui| {
        ui.window_brush = Brush::new(ui.palette.window);
        ui.surface_brush = Brush::new(ui.palette.surface);
        (ui.main, ui.c, ui.palette)
    }) else {
        return;
    };
    theme::style_window(main, &p);
    let (tree_theme, field_theme) = if p.dark {
        (w!("DarkMode_Explorer"), w!("DarkMode_CFD"))
    } else {
        (w!("Explorer"), w!("CFD"))
    };
    // SAFETY: theming and coloring our own child controls.
    unsafe {
        let _ = SetWindowTheme(c.tree, tree_theme, PCWSTR::null());
        let _ = SetWindowTheme(c.combo, field_theme, PCWSTR::null());
        let _ = SetWindowTheme(c.filter, field_theme, PCWSTR::null());
        SendMessageW(
            c.tree,
            TVM_SETBKCOLOR,
            None,
            Some(LPARAM(theme::cr(p.surface).0 as isize)),
        );
        SendMessageW(
            c.tree,
            TVM_SETTEXTCOLOR,
            None,
            Some(LPARAM(theme::cr(p.text).0 as isize)),
        );
    }
    details::set_style(p, None);
    // SAFETY: repaints our own window tree.
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
            Some(main),
            None,
            None,
            windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN
                | windows::Win32::Graphics::Gdi::RDW_FRAME,
        );
    }
}

/// Fonts and font-dependent metrics for every control.
fn apply_fonts() {
    let Some((c, fonts)) = with_ui(|ui| (ui.c, Rc::clone(&ui.fonts))) else {
        return;
    };
    // SAFETY: WM_SETFONT and metric messages to our own children; fonts outlive them.
    unsafe {
        for hwnd in [
            c.combo,
            c.refresh,
            c.pick,
            c.highlight,
            c.copy,
            c.tree,
            c.filter,
        ] {
            SendMessageW(
                hwnd,
                WM_SETFONT,
                Some(WPARAM(fonts.body.handle().0 as usize)),
                Some(LPARAM(1)),
            );
        }
        SendMessageW(
            c.tree,
            TVM_SETITEMHEIGHT,
            Some(WPARAM(fonts.px(ROW_H) as usize)),
            None,
        );
        SendMessageW(
            c.tree,
            TVM_SETINDENT,
            Some(WPARAM(fonts.px(INDENT) as usize)),
            None,
        );
        // Selection field, then list items.
        SendMessageW(
            c.combo,
            CB_SETITEMHEIGHT,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(fonts.px(28) as isize)),
        );
        SendMessageW(
            c.combo,
            CB_SETITEMHEIGHT,
            Some(WPARAM(0)),
            Some(LPARAM(fonts.px(32) as isize)),
        );
    }
    details::set_style(
        with_ui(|ui| ui.palette).unwrap_or(Palette::LIGHT),
        Some(fonts),
    );
}

fn layout() {
    let Some((main, c, fonts, split)) =
        with_ui(|ui| (ui.main, ui.c, Rc::clone(&ui.fonts), ui.split))
    else {
        return;
    };
    let px = |v| fonts.px(v);
    let mut client = RECT::default();
    // SAFETY: reads our own client rect and a screen DC for measuring.
    let (bw_pick, bw_highlight, bw_copy) = unsafe {
        let _ = GetClientRect(main, &mut client);
        let dc = windows::Win32::Graphics::Gdi::GetDC(Some(main));
        let widths = (
            theme::button_width(dc, &fonts, "Picking\u{2026} 3", true, false),
            theme::button_width(dc, &fonts, "Highlight", true, false),
            theme::button_width(dc, &fonts, "Copy locator", true, true),
        );
        windows::Win32::Graphics::Gdi::ReleaseDC(Some(main), dc);
        widths
    };
    let (w, h) = (client.right, client.bottom);
    let pad = px(PAD);
    let toolbar = px(TOOLBAR_H);
    let bh = px(BUTTON_H);
    let by = (toolbar - bh) / 2;
    let combo_face = px(28) + 6;
    let right_group = bw_pick + bw_highlight + bw_copy + px(6) * 2;
    let combo_w = ((w - pad * 2 - right_group - bh - px(24)) as f32)
        .clamp(px(220) as f32, px(560) as f32) as i32;
    let status_h = px(STATUS_H);
    let gap = px(GAP);
    let top = toolbar;
    let bottom = (h - status_h).max(top + px(40));
    let avail = panes_width(w, pad, gap, px(200));
    let left_w = (avail as f32 * split) as i32;
    let left = rect(pad, top, left_w, bottom - top);
    let right = rect(pad + left_w + gap, top, avail - left_w, bottom - top);
    let filter = rect(
        left.left + px(10),
        left.top + px(10),
        (left.right - left.left) - px(20),
        px(34),
    );
    let edit_h = px(20);
    let tree_top = filter.bottom + px(8);
    // SAFETY: moves our own child windows.
    unsafe {
        let _ = MoveWindow(
            c.combo,
            pad,
            (toolbar - combo_face) / 2,
            combo_w,
            px(420),
            true,
        );
        let _ = MoveWindow(c.refresh, pad + combo_w + px(6), by, bh, bh, true);
        let mut x = w - pad + px(2);
        for (hwnd, bw) in [
            (c.copy, bw_copy),
            (c.highlight, bw_highlight),
            (c.pick, bw_pick),
        ] {
            x -= bw;
            let _ = MoveWindow(hwnd, x, by, bw, bh, true);
            x -= px(6);
        }
        let _ = MoveWindow(
            c.filter,
            filter.left + px(36),
            filter.top + (px(34) - edit_h) / 2,
            (filter.right - filter.left) - px(36) - px(10),
            edit_h,
            true,
        );
        let _ = MoveWindow(
            c.tree,
            left.left + px(4),
            tree_top,
            (left.right - left.left) - px(8),
            (left.bottom - px(6) - tree_top).max(10),
            true,
        );
        let _ = MoveWindow(
            c.details,
            right.left + 1,
            right.top + px(6),
            (right.right - right.left) - 2,
            (right.bottom - right.top) - px(12),
            true,
        );
    }
    with_ui(|ui| {
        ui.rects = Rects {
            left,
            right,
            filter,
            splitter: RECT {
                left: left.right,
                right: right.left,
                ..left
            },
            status: rect(0, h - status_h, w, status_h),
        };
    });
    // SAFETY: repaints our own window.
    let _ = unsafe { InvalidateRect(Some(main), None, false) };
}

fn paint_main(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: double-buffered WM_PAINT; every GDI object is released before EndPaint.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let (w, h) = (client.right.max(1), client.bottom.max(1));
        let mem = CreateCompatibleDC(Some(hdc));
        let bitmap = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        with_ui(|ui| ui.paint(mem, client));
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

impl Ui {
    fn px(&self, v: i32) -> i32 {
        self.fonts.px(v)
    }

    fn paint(&self, hdc: HDC, client: RECT) {
        let p = &self.palette;
        let f = &*self.fonts;
        let r = &self.rects;
        theme::fill(hdc, client, p.window);
        let radius = self.px(8) as f32;
        theme::rounded(hdc, r.left, radius, p.surface, Some(p.border));
        theme::rounded(hdc, r.right, radius, p.surface, Some(p.border));
        // Filter field: Windows 11 text box with an accent underline while focused.
        let field_border = if self.filter_focused {
            p.accent
        } else {
            p.border
        };
        theme::rounded(
            hdc,
            r.filter,
            self.px(4) as f32,
            p.window,
            Some(field_border),
        );
        if self.filter_focused {
            theme::fill(
                hdc,
                RECT {
                    left: r.filter.left + self.px(3),
                    right: r.filter.right - self.px(3),
                    top: r.filter.bottom - self.px(2),
                    bottom: r.filter.bottom,
                },
                p.accent,
            );
        }
        theme::icon(
            hdc,
            &f.icons,
            glyph::SEARCH,
            RECT {
                left: r.filter.left + self.px(8),
                right: r.filter.left + self.px(30),
                ..r.filter
            },
            p.muted,
        );
        // Splitter grip.
        if self.splitter_hot || self.dragging {
            let mid_x = (r.splitter.left + r.splitter.right) / 2;
            let mid_y = (r.splitter.top + r.splitter.bottom) / 2;
            let grip = rect(
                mid_x - self.px(2),
                mid_y - self.px(18),
                self.px(4),
                self.px(36),
            );
            theme::rounded(
                hdc,
                grip,
                self.px(2) as f32,
                if self.dragging { p.accent } else { p.faint },
                None,
            );
        }
        // Status bar.
        let s = r.status;
        let dot = self.px(8);
        let dot_color = match self.tone {
            Tone::Info => p.faint,
            Tone::Busy => p.accent,
            Tone::Good => p.good,
            Tone::Bad => p.bad,
        };
        let x = s.left + self.px(PAD) + self.px(4);
        theme::dot(
            hdc,
            rect(x, s.top + (s.bottom - s.top - dot) / 2, dot, dot),
            dot_color,
        );
        let count_w = theme::measure(hdc, &f.small, &self.count, None, DRAW_TEXT_FORMAT(0)).0;
        theme::text(
            hdc,
            &f.small,
            &self.status,
            RECT {
                left: x + dot + self.px(8),
                right: s.right - self.px(PAD) - count_w - self.px(16),
                ..s
            },
            if self.tone == Tone::Bad {
                p.bad
            } else {
                p.muted
            },
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
        theme::text(
            hdc,
            &f.small,
            &self.count,
            RECT {
                right: s.right - self.px(PAD),
                ..s
            },
            p.faint,
            DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
        );
    }

    fn draw_button(&self, cd: &NMCUSTOMDRAW) {
        let id = cd.hdr.idFrom as i32;
        let state = ButtonState {
            hot: cd.uItemState.0 & CDIS_HOT.0 != 0,
            pressed: cd.uItemState.0 & CDIS_SELECTED.0 != 0,
            focused: cd.uItemState.0 & CDIS_FOCUS.0 != 0
                && cd.uItemState.0 & windows::Win32::UI::Controls::CDIS_SHOWKEYBOARDCUES.0 != 0,
            disabled: cd.uItemState.0 & CDIS_DISABLED.0 != 0,
        };
        let (kind, glyph_char, label) = match id {
            ID_REFRESH => (ButtonKind::Subtle, glyph::REFRESH, String::new()),
            ID_PICK => (
                ButtonKind::Secondary,
                glyph::POINTER,
                window_text(cd.hdr.hwndFrom),
            ),
            ID_HIGHLIGHT => (
                ButtonKind::Secondary,
                glyph::VIEW,
                window_text(cd.hdr.hwndFrom),
            ),
            _ => (
                ButtonKind::Primary,
                glyph::COPY,
                window_text(cd.hdr.hwndFrom),
            ),
        };
        theme::draw_button(
            cd.hdc,
            cd.rc,
            self.palette.window,
            kind,
            state,
            &label,
            Some(glyph_char),
            &self.palette,
            &self.fonts,
        );
    }

    fn draw_tree_row(&self, cd: &NMTVCUSTOMDRAW) {
        let p = &self.palette;
        let f = &*self.fonts;
        let hdc = cd.nmcd.hdc;
        let rc = cd.nmcd.rc;
        let item = HTREEITEM(cd.nmcd.dwItemSpec as isize);
        let Some(row) = self.rows.get(cd.nmcd.lItemlParam.0 as usize) else {
            return;
        };
        let state = cd.nmcd.uItemState.0;
        let selected = state & CDIS_SELECTED.0 != 0;
        let hot = state & CDIS_HOT.0 != 0;
        theme::fill(hdc, rc, p.surface);
        let band = RECT {
            left: rc.left + self.px(4),
            right: rc.right - self.px(4),
            top: rc.top + self.px(1),
            bottom: rc.bottom - self.px(1),
        };
        if selected {
            theme::rounded(hdc, band, self.px(4) as f32, p.selection, None);
            let pill_h = self.px(14);
            theme::rounded(
                hdc,
                rect(
                    band.left,
                    band.top + (band.bottom - band.top - pill_h) / 2,
                    self.px(3),
                    pill_h,
                ),
                1.5,
                p.accent,
                None,
            );
        } else if hot {
            theme::rounded(hdc, band, self.px(4) as f32, p.hover, None);
        }
        // Where the tree puts the text (after indentation and the expand button).
        let mut text_rect = RECT::default();
        // SAFETY: TVM_GETITEMRECT reads the item handle from the RECT's first field and
        // writes the rect back; both live on this stack frame.
        let (indent, expanded) = unsafe {
            *(&mut text_rect as *mut RECT as *mut isize) = item.0;
            SendMessageW(
                self.c.tree,
                TVM_GETITEMRECT,
                Some(WPARAM(1)),
                Some(LPARAM(&mut text_rect as *mut RECT as isize)),
            );
            let indent = SendMessageW(self.c.tree, TVM_GETINDENT, None, None).0 as i32;
            let st = SendMessageW(
                self.c.tree,
                TVM_GETITEMSTATE,
                Some(WPARAM(item.0 as usize)),
                Some(LPARAM(TVIS_EXPANDED.0 as isize)),
            )
            .0 as u32;
            (indent, st & TVIS_EXPANDED.0 != 0)
        };
        if row.has_children {
            theme::icon(
                hdc,
                &f.icons_small,
                if expanded {
                    glyph::CHEVRON_DOWN
                } else {
                    glyph::CHEVRON_RIGHT
                },
                RECT {
                    left: text_rect.left - indent,
                    right: text_rect.left,
                    ..rc
                },
                p.muted,
            );
        }
        let dim = !row.enabled || !row.visible;
        let on_sel = selected && p.high_contrast;
        let main_color = if on_sel {
            p.on_selection()
        } else if dim {
            p.muted
        } else {
            p.text
        };
        let role_color = if on_sel {
            p.on_selection()
        } else if dim {
            p.faint
        } else {
            p.role
        };
        // Right side: tags and the ref.
        let tags = row.tags();
        let right_text = if tags.is_empty() {
            row.reference.clone()
        } else {
            format!("{tags}   {}", row.reference)
        };
        let right_w = theme::measure(hdc, &f.small, &right_text, None, DRAW_TEXT_FORMAT(0)).0;
        let right_edge = band.right - self.px(10);
        theme::text(
            hdc,
            &f.small,
            &right_text,
            RECT {
                left: right_edge - right_w,
                right: right_edge,
                ..rc
            },
            if on_sel { p.on_selection() } else { p.faint },
            DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
        );
        let mut x = text_rect.left + self.px(2);
        let limit = right_edge - right_w - self.px(12);
        let role_w = theme::measure(hdc, &f.small_strong, &row.role, None, DRAW_TEXT_FORMAT(0)).0;
        theme::text(
            hdc,
            &f.small_strong,
            &row.role,
            RECT {
                left: x,
                right: limit.max(x),
                ..rc
            },
            role_color,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
        x += role_w + self.px(8);
        let (label, color) = if !row.name.is_empty() {
            (row.name.clone(), main_color)
        } else if !row.automation_id.is_empty() {
            (format!("#{}", row.automation_id), p.muted)
        } else {
            (String::new(), main_color)
        };
        if x < limit && !label.is_empty() {
            theme::text(
                hdc,
                &f.body,
                &label,
                RECT {
                    left: x,
                    right: limit,
                    ..rc
                },
                color,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_LEFT,
            );
        }
    }

    fn measure_combo(&self, m: &mut MEASUREITEMSTRUCT) {
        m.itemHeight = if m.itemID == u32::MAX {
            self.px(28) as u32
        } else {
            self.px(32) as u32
        };
    }

    fn draw_combo(&self, d: &DRAWITEMSTRUCT) {
        let p = &self.palette;
        let f = &*self.fonts;
        let r = d.rcItem;
        let in_field = d.itemState.0 & ODS_COMBOBOXEDIT.0 != 0;
        let highlighted = d.itemState.0 & ODS_SELECTED.0 != 0 && !in_field;
        let bg = if highlighted { p.selection } else { p.surface };
        theme::fill(d.hDC, r, bg);
        // itemID is -1 for an empty list or no selection: the field is just background.
        let Some(row) = self.combo_rows.get(d.itemID as usize) else {
            return;
        };
        let icon = self.px(16);
        let x = r.left + self.px(8);
        let iy = r.top + (r.bottom - r.top - icon) / 2;
        match row.icon {
            // SAFETY: draws another process's shared icon handle; we never destroy it.
            Some(h) => unsafe {
                let _ = DrawIconEx(d.hDC, x, iy, h, icon, icon, 0, None, DI_NORMAL);
            },
            None => theme::icon(
                d.hDC,
                &f.icons,
                glyph::WINDOW,
                rect(x, iy, icon, icon),
                p.muted,
            ),
        }
        let text_x = x + icon + self.px(10);
        let fg = if highlighted {
            p.on_selection()
        } else {
            p.text
        };
        let process_w = theme::measure(d.hDC, &f.small, &row.process, None, DRAW_TEXT_FORMAT(0)).0;
        let process_left = r.right - self.px(10) - process_w;
        theme::text(
            d.hDC,
            &f.body,
            &row.title,
            RECT {
                left: text_x,
                right: (process_left - self.px(12)).max(text_x),
                ..r
            },
            fg,
            DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
        );
        theme::text(
            d.hDC,
            &f.small,
            &row.process,
            RECT {
                left: process_left,
                right: r.right - self.px(10),
                ..r
            },
            if highlighted {
                p.on_selection()
            } else {
                p.muted
            },
            DT_SINGLELINE | DT_VCENTER | DT_RIGHT,
        );
    }
}

fn set_status(tone: Tone, text: &str) {
    let main = with_ui(|ui| {
        ui.tone = tone;
        ui.status = text.to_owned();
        ui.main
    });
    if let Some(main) = main {
        // SAFETY: repaints our own window now, so busy messages show before blocking work.
        unsafe {
            let _ = InvalidateRect(Some(main), None, false);
            let _ = UpdateWindow(main);
        }
    }
}

fn set_count(text: String) {
    if let Some(main) = with_ui(|ui| {
        ui.count = text;
        ui.main
    }) {
        // SAFETY: repaints our own window.
        let _ = unsafe { InvalidateRect(Some(main), None, false) };
    }
}

fn busy_cursor() {
    // SAFETY: shows the wait cursor until the next WM_SETCURSOR.
    unsafe {
        let _ = SetCursor(LoadCursorW(None, IDC_WAIT).ok());
    }
}

/// Small icon of another app's window, if it has one (never blocks on a hung app).
fn window_icon(hwnd: u64) -> Option<HICON> {
    let h = HWND(hwnd as usize as *mut c_void);
    // SAFETY: read-only queries with a short timeout; the icon stays owned by that app.
    unsafe {
        for kind in [ICON_SMALL2, ICON_SMALL, ICON_BIG] {
            let mut result = 0usize;
            SendMessageTimeoutW(
                h,
                WM_GETICON,
                WPARAM(kind as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                60,
                Some(&mut result),
            );
            if result != 0 {
                return Some(HICON(result as *mut c_void));
            }
        }
        for index in [GCLP_HICONSM, GCLP_HICON] {
            let v = GetClassLongPtrW(h, index);
            if v != 0 {
                return Some(HICON(v as *mut c_void));
            }
        }
    }
    None
}

impl App {
    fn load_windows(&mut self) {
        let foreground = self.refresh_window_list();
        // SAFETY: combo message on our own child.
        unsafe {
            SendMessageW(self.c.combo, CB_SETCURSEL, Some(WPARAM(foreground)), None);
        }
        self.load_tree();
    }

    /// Reloads the window list into the picker; returns the foreground window's index.
    fn refresh_window_list(&mut self) -> usize {
        let own = std::process::id();
        self.windows = match self.engine.list_windows() {
            Ok(ws) => ws.into_iter().filter(|w| w.process_id != own).collect(),
            Err(e) => {
                set_status(Tone::Bad, &format!("Cannot list windows: {e}"));
                return 0;
            }
        };
        let rows: Vec<ComboRow> = self
            .windows
            .iter()
            .map(|w| ComboRow {
                title: if w.title.is_empty() {
                    "(untitled)".into()
                } else {
                    w.title.clone()
                },
                process: w.process_name.clone(),
                icon: window_icon(w.hwnd),
            })
            .collect();
        with_ui(|ui| ui.combo_rows = rows);
        // SAFETY: combo box messages on our own child; strings outlive each call.
        unsafe {
            SendMessageW(self.c.combo, CB_RESETCONTENT, None, None);
            for w in &self.windows {
                let label = wide(&format!("{}  ({})", w.title, w.process_name));
                SendMessageW(
                    self.c.combo,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(label.as_ptr() as isize)),
                );
            }
        }
        self.windows.iter().position(|w| w.foreground).unwrap_or(0)
    }

    fn current_window(&self) -> Option<&WindowInfo> {
        // SAFETY: combo box query on our own child.
        let index = unsafe { SendMessageW(self.c.combo, CB_GETCURSEL, None, None) }.0;
        usize::try_from(index)
            .ok()
            .and_then(|i| self.windows.get(i))
    }

    fn load_tree(&mut self) {
        let Some(window) = self.current_window().cloned() else {
            set_status(
                Tone::Info,
                "No window to inspect. Open an app and press Refresh.",
            );
            return;
        };
        busy_cursor();
        set_status(Tone::Busy, &format!("Reading {}\u{2026}", window.title));
        let request = SnapshotRequest {
            target: SnapshotTarget::Window(WindowSelector {
                hwnd: Some(window.hwnd),
                ..Default::default()
            }),
            interactive_only: false,
            include_bounds: true,
            include_patterns: true,
            max_depth: 30,
            max_nodes: 2_000,
            max_list_items: 200,
            structured: true,
            ..Default::default()
        };
        let snapshot = self
            .rt
            .block_on(self.engine.snapshot(&self.session, request));
        self.current = None;
        self.locator = None;
        self.sync_actions();
        details::set_model(Model::Empty, "Nothing selected");
        match snapshot {
            Ok(s) => {
                self.nodes = s.nodes.unwrap_or_default();
                self.loaded = Some(window.hwnd);
                self.selected_window = Some(window.title.clone());
                let needle = self.filter_text();
                self.fill_tree(&needle);
                set_status(
                    Tone::Good,
                    &format!(
                        "{}{}. Select an element to see its properties.",
                        window.title,
                        if s.truncated {
                            " (large window: showing the first 2,000 elements)"
                        } else {
                            ""
                        }
                    ),
                );
            }
            Err(e) => {
                self.nodes.clear();
                self.loaded = Some(window.hwnd);
                self.selected_window = Some(window.title.clone());
                self.fill_tree("");
                set_status(Tone::Bad, &format!("Cannot read {}: {e}", window.title));
            }
        }
    }

    fn filter_text(&self) -> String {
        window_text(self.c.filter).trim().to_lowercase()
    }

    /// Rebuilds the tree from the last snapshot, filtered by `needle`.
    fn fill_tree(&mut self, needle: &str) {
        let total = text::count_nodes(&self.nodes);
        let shown = if needle.is_empty() {
            self.nodes.clone()
        } else {
            text::filter_nodes(&self.nodes, needle)
        };
        // SAFETY: redraw off while rebuilding our own tree view.
        unsafe {
            SendMessageW(
                self.c.tree,
                windows::Win32::UI::WindowsAndMessaging::WM_SETREDRAW,
                Some(WPARAM(0)),
                None,
            );
            SendMessageW(self.c.tree, TVM_DELETEITEM, None, Some(LPARAM(TVI_ROOT.0)));
        }
        self.items.clear();
        self.handles.clear();
        // Indices now name other elements: a debounced selection from the old tree must not
        // show (and highlight) whatever takes its place.
        self.pending_select = None;
        // SAFETY: stops this window's debounce timer.
        let _ = unsafe { KillTimer(Some(self.main), TIMER_SELECT) };
        let mut rows = Vec::new();
        for node in &shown {
            self.insert(node, TVI_ROOT, 0, !needle.is_empty(), &mut rows);
        }
        with_ui(|ui| ui.rows = rows);
        // SAFETY: redraw back on for our own tree view.
        unsafe {
            SendMessageW(
                self.c.tree,
                windows::Win32::UI::WindowsAndMessaging::WM_SETREDRAW,
                Some(WPARAM(1)),
                None,
            );
            let _ = InvalidateRect(Some(self.c.tree), None, true);
        }
        let count = if needle.is_empty() {
            format!("{total} elements")
        } else {
            format!("{} of {total} elements match", self.items.len())
        };
        set_count(count);
    }

    fn insert(
        &mut self,
        node: &SnapshotNode,
        parent: HTREEITEM,
        depth: u32,
        expand_all: bool,
        rows: &mut Vec<RowView>,
    ) {
        let index = self.items.len();
        self.items.push(node.element.clone());
        rows.push(RowView::of(node));
        let mut label: Vec<u16> = text::tree_label(&node.element)
            .encode_utf16()
            .chain([0])
            .collect();
        let insert = TVINSERTSTRUCTW {
            hParent: parent,
            hInsertAfter: TVI_LAST,
            Anonymous: TVINSERTSTRUCTW_0 {
                item: TVITEMW {
                    mask: TVIF_TEXT | TVIF_PARAM,
                    pszText: PWSTR(label.as_mut_ptr()),
                    lParam: LPARAM(index as isize),
                    ..Default::default()
                },
            },
        };
        // SAFETY: `insert` and `label` outlive the synchronous insert.
        let item = unsafe {
            SendMessageW(
                self.c.tree,
                TVM_INSERTITEMW,
                None,
                Some(LPARAM(&insert as *const _ as isize)),
            )
        };
        let handle = HTREEITEM(item.0);
        self.handles.push(handle);
        for child in &node.children {
            self.insert(child, handle, depth + 1, expand_all, rows);
        }
        if expand_all || depth < 2 {
            // SAFETY: expands an item of our own tree view.
            unsafe {
                SendMessageW(
                    self.c.tree,
                    TVM_EXPAND,
                    Some(WPARAM(TVE_EXPAND.0 as usize)),
                    Some(LPARAM(item.0)),
                );
            }
        }
    }

    fn show_index(&mut self, index: usize) {
        let Some(reference) = self.items.get(index).map(|i| i.reference.clone()) else {
            return;
        };
        let details = self.rt.block_on(
            self.engine
                .inspect(&self.session, InspectRequest::Ref(reference)),
        );
        self.apply_details(details);
    }

    fn apply_details(&mut self, details: WinwrightResult<ElementDetails>) {
        match details {
            Ok(d) => {
                let locator = text::locator_json(&d, self.selected_window.as_deref());
                let display = text::locator_display(&d, self.selected_window.as_deref());
                let plain = text::details(&d, &locator);
                details::set_model(
                    Model::Element(Box::new(text::detail_view(&d, display))),
                    &plain,
                );
                let reference = d.element.reference.clone();
                self.locator = Some(locator);
                self.current = Some(d);
                self.sync_actions();
                self.highlight(&reference);
            }
            Err(e) => {
                self.locator = None;
                self.current = None;
                self.sync_actions();
                let message = e.to_string();
                details::set_model(Model::Error(message.clone()), &message);
            }
        }
    }

    /// Highlight and Copy only make sense with an element selected.
    fn sync_actions(&self) {
        let on = self.current.is_some();
        // SAFETY: enables or disables our own buttons.
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(self.c.highlight, on);
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(self.c.copy, on);
        }
    }

    fn highlight(&self, reference: &str) {
        let request = HighlightRequest {
            target: ElementTarget::by_ref(reference),
            style: OverlayStyle::Highlight,
            label: None,
            step: None,
            color: None,
            duration_ms: Some(HIGHLIGHT_MS),
        };
        if let Err(e) = self
            .rt
            .block_on(self.engine.highlight(&self.session, request))
        {
            set_status(Tone::Bad, &format!("Cannot highlight: {e}"));
        }
    }

    fn set_pick_label(&self, label: &str) {
        // SAFETY: our own button; the string outlives the call.
        unsafe {
            let _ = SetWindowTextW(self.c.pick, &wide(label));
            let _ = InvalidateRect(Some(self.c.pick), None, false);
        }
    }

    fn start_pick(&mut self) {
        self.countdown = PICK_SECONDS;
        self.set_pick_label(&format!("Picking\u{2026} {PICK_SECONDS}"));
        set_status(
            Tone::Busy,
            "Hover over any element in another app. Esc cancels.",
        );
        // SAFETY: timer on this thread's own main window.
        unsafe {
            SetTimer(Some(self.main), TIMER_PICK, 1_000, None);
        }
    }

    fn cancel_pick(&mut self) {
        if self.countdown == 0 {
            return;
        }
        self.countdown = 0;
        // SAFETY: stops this window's timer.
        let _ = unsafe { KillTimer(Some(self.main), TIMER_PICK) };
        self.set_pick_label(PICK_LABEL);
        set_status(Tone::Info, "Pick cancelled.");
    }

    fn tick_pick(&mut self) {
        self.countdown = self.countdown.saturating_sub(1);
        if self.countdown > 0 {
            self.set_pick_label(&format!("Picking\u{2026} {}", self.countdown));
            return;
        }
        self.set_pick_label(PICK_LABEL);
        // SAFETY: stops this window's timer and reads the cursor/window under it.
        let (over_self, root) = unsafe {
            let _ = KillTimer(Some(self.main), TIMER_PICK);
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let hwnd = WindowFromPoint(pt);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            (
                pid == std::process::id(),
                GetAncestor(hwnd, GA_ROOT).0 as usize as u64,
            )
        };
        if over_self {
            set_status(
                Tone::Bad,
                "The cursor was over the Inspector. Press Pick element, then hover another app.",
            );
            return;
        }
        busy_cursor();
        let details = self.rt.block_on(
            self.engine
                .inspect(&self.session, InspectRequest::UnderCursor),
        );
        let Ok(d) = details else {
            self.apply_details(details);
            set_status(Tone::Bad, "Nothing inspectable under the cursor.");
            return;
        };
        // Reveal the picked element in its window's tree.
        self.refresh_window_list();
        let listed = self.windows.iter().position(|w| w.hwnd == root);
        // The reloaded list has no selection: show the picked window, or else keep showing
        // the one whose tree is loaded.
        let shown = listed.or_else(|| {
            self.windows
                .iter()
                .position(|w| Some(w.hwnd) == self.loaded)
        });
        if let Some(index) = shown {
            // SAFETY: combo message on our own child.
            unsafe {
                SendMessageW(self.c.combo, CB_SETCURSEL, Some(WPARAM(index)), None);
            }
        }
        if listed.is_some() {
            self.load_tree();
        }
        let reference = d.element.reference.clone();
        if let Some(i) = self.items.iter().position(|e| e.reference == reference)
            && let Some(handle) = self.handles.get(i).copied()
        {
            // SAFETY: selects and reveals an item of our own tree view (its SELCHANGED
            // notification is skipped while the app is busy; the details are applied below).
            unsafe {
                SendMessageW(
                    self.c.tree,
                    TVM_SELECTITEM,
                    Some(WPARAM(TVGN_CARET as usize)),
                    Some(LPARAM(handle.0)),
                );
                SendMessageW(self.c.tree, TVM_ENSUREVISIBLE, None, Some(LPARAM(handle.0)));
            }
        }
        if listed.is_some() {
            self.apply_details(Ok(d));
        } else {
            // Not a listed window (a popup or tool window): the locator names the window the
            // element is in, not the one whose tree is still shown.
            let title = window_text(HWND(root as usize as *mut c_void));
            let shown = std::mem::replace(
                &mut self.selected_window,
                (!title.is_empty()).then_some(title),
            );
            self.apply_details(Ok(d));
            self.selected_window = shown;
        }
        set_status(Tone::Good, "Picked the element under the cursor.");
    }

    fn copy_text(&self, text: Option<String>, what: &str) {
        let Some(text) = text else {
            set_status(Tone::Info, "Select or pick an element first.");
            return;
        };
        match set_clipboard(self.main, &text) {
            Ok(()) => set_status(Tone::Good, &format!("{what} copied to the clipboard.")),
            Err(e) => set_status(Tone::Bad, &format!("Cannot copy: {e}")),
        }
    }

    fn command(&mut self, id: i32, code: u32) {
        match id {
            ID_REFRESH => self.load_windows(),
            // UI Automation selections skip CBN_SELCHANGE; any combo notification that shows
            // a different window than the loaded one reloads.
            ID_COMBO if self.current_window().map(|w| w.hwnd) != self.loaded => self.load_tree(),
            ID_PICK if self.countdown == 0 => self.start_pick(),
            ID_PICK | ID_CANCEL_PICK => self.cancel_pick(),
            ID_HIGHLIGHT => match self.current.as_ref().map(|d| d.element.reference.clone()) {
                Some(reference) => self.highlight(&reference),
                None => set_status(Tone::Info, "Select or pick an element first."),
            },
            ID_COPY => self.copy_text(self.locator.clone(), "Locator"),
            ID_COPY_ALL => self.copy_text(
                self.current
                    .as_ref()
                    .map(|d| text::details(d, self.locator.as_deref().unwrap_or_default())),
                "Properties",
            ),
            ID_FILTER if code == EN_CHANGE => {
                // SAFETY: (re)starts a debounce timer on our own window.
                unsafe {
                    SetTimer(Some(self.main), TIMER_FILTER, 200, None);
                }
            }
            _ => {}
        }
    }

    fn timer(&mut self, id: usize) {
        match id {
            TIMER_PICK => self.tick_pick(),
            TIMER_FILTER => {
                // SAFETY: stops this window's one-shot debounce timer.
                let _ = unsafe { KillTimer(Some(self.main), TIMER_FILTER) };
                let needle = self.filter_text();
                self.fill_tree(&needle);
            }
            TIMER_SELECT => {
                // SAFETY: stops this window's one-shot debounce timer.
                let _ = unsafe { KillTimer(Some(self.main), TIMER_SELECT) };
                if let Some(index) = self.pending_select.take() {
                    self.show_index(index);
                }
            }
            _ => {}
        }
    }
}

fn set_clipboard(owner: HWND, text: &str) -> WinwrightResult<()> {
    let mut data: Vec<u16> = text.encode_utf16().collect();
    data.push(0);
    let bytes = data.len() * 2;
    // SAFETY: standard clipboard sequence; the global memory is handed to the clipboard on
    // success (it then owns it) and copied from `data`, which outlives the copy.
    unsafe {
        OpenClipboard(Some(owner)).map_err(|e| platform("OpenClipboard", &e))?;
        let result = (|| {
            EmptyClipboard().map_err(|e| platform("EmptyClipboard", &e))?;
            let memory =
                GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|e| platform("GlobalAlloc", &e))?;
            let target = GlobalLock(memory);
            if target.is_null() {
                let _ = GlobalFree(Some(memory));
                return Err(WinwrightError::invalid("cannot lock clipboard memory"));
            }
            std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, target as *mut u8, bytes);
            let _ = GlobalUnlock(memory);
            if let Err(e) = SetClipboardData(13, Some(HANDLE(memory.0))) {
                // The clipboard did not take the memory, so it is still ours to free.
                let _ = GlobalFree(Some(memory));
                return Err(platform("SetClipboardData", &e));
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Commands that need only the UI. They run without the app: focusing the filter sends
/// EN_SETFOCUS at once, which an app-borrowing handler would drop.
fn ui_command(main: HWND, id: i32, code: u32) -> bool {
    match id {
        ID_FIND => {
            let Some(filter) = with_ui(|ui| ui.c.filter) else {
                return true;
            };
            // SAFETY: focuses our own edit control and selects its text.
            unsafe {
                let _ = SetFocus(Some(filter));
                SendMessageW(
                    filter,
                    windows::Win32::UI::Controls::EM_SETSEL,
                    Some(WPARAM(0)),
                    Some(LPARAM(-1)),
                );
            }
        }
        ID_FILTER if code == EN_SETFOCUS || code == EN_KILLFOCUS => {
            let focused = code == EN_SETFOCUS;
            with_ui(|ui| ui.filter_focused = focused);
            // SAFETY: repaints our own window.
            let _ = unsafe { InvalidateRect(Some(main), None, false) };
        }
        _ => return false,
    }
    true
}

fn mouse_point(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam.0 & 0xFFFF) as u16 as i16 as i32,
        ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
}

fn on_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let changed = with_ui(|ui| {
        if ui.dragging {
            let (pad, gap) = (ui.px(PAD), ui.px(GAP));
            let panes = panes_width(ui.rects.status.right, pad, gap, ui.px(200));
            ui.split = split_at(x, panes, pad, gap);
            return (true, true, false);
        }
        let hot = contains(&ui.rects.splitter, x, y);
        let changed = hot != ui.splitter_hot;
        ui.splitter_hot = hot;
        let track = !ui.tracking_leave;
        ui.tracking_leave = true;
        (changed, false, track)
    });
    if let Some((changed, relayout, track)) = changed {
        if track {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: asks for WM_MOUSELEAVE on our own window.
            let _ = unsafe { TrackMouseEvent(&mut tme) };
        }
        if relayout {
            layout();
        } else if changed {
            // SAFETY: repaints our own window.
            let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
        }
    }
}

fn on_notify(lparam: LPARAM) -> Option<LRESULT> {
    // SAFETY: WM_NOTIFY's lParam points at an NMHDR (and the larger struct for its code).
    let header = unsafe { &*(lparam.0 as *const NMHDR) };
    let id = header.idFrom as i32;
    if header.code == NM_CUSTOMDRAW {
        if id == ID_TREE {
            // SAFETY: tree-view custom draw carries an NMTVCUSTOMDRAW.
            let cd = unsafe { &*(lparam.0 as *const NMTVCUSTOMDRAW) };
            if cd.nmcd.dwDrawStage == CDDS_PREPAINT {
                return Some(LRESULT(CDRF_NOTIFYITEMDRAW as isize));
            }
            if cd.nmcd.dwDrawStage == CDDS_ITEMPREPAINT
                && with_ui(|ui| ui.draw_tree_row(cd)).is_some()
            {
                return Some(LRESULT(CDRF_SKIPDEFAULT as isize));
            }
            return Some(LRESULT(CDRF_DODEFAULT as isize));
        }
        if (ID_REFRESH..=ID_COPY).contains(&id) {
            // SAFETY: button custom draw carries an NMCUSTOMDRAW.
            let cd = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
            if cd.dwDrawStage == CDDS_PREPAINT && with_ui(|ui| ui.draw_button(cd)).is_some() {
                return Some(LRESULT(CDRF_SKIPDEFAULT as isize));
            }
            return Some(LRESULT(CDRF_DODEFAULT as isize));
        }
    }
    if id == ID_TREE && header.code == TVN_SELCHANGEDW {
        // SAFETY: TVN_SELCHANGEDW carries an NMTREEVIEWW.
        let tv = unsafe { &*(lparam.0 as *const NMTREEVIEWW) };
        let index = tv.itemNew.lParam.0 as usize;
        if tv.itemNew.hItem.0 != 0 {
            with_app(|app| {
                app.pending_select = Some(index);
                // SAFETY: (re)starts a debounce timer on our own window.
                unsafe {
                    SetTimer(Some(app.main), TIMER_SELECT, 120, None);
                }
            });
        }
    }
    Some(LRESULT(0))
}

fn on_dpi_changed(hwnd: HWND, dpi: u32, suggested: RECT) {
    let old_icons = with_ui(|ui| {
        ui.fonts = Rc::new(Fonts::new(dpi));
        std::mem::take(&mut ui.own_icons)
    })
    .unwrap_or_default();
    // New icons first: the window must never hold a destroyed one.
    let icons = set_window_icons(hwnd, dpi);
    for icon in old_icons {
        // SAFETY: our own icons, no longer set on the window.
        let _ = unsafe { DestroyIcon(icon) };
    }
    with_ui(|ui| ui.own_icons = icons);
    apply_fonts();
    // SAFETY: moves our own window to the rect Windows suggests for the new DPI.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            suggested.left,
            suggested.top,
            suggested.right - suggested.left,
            suggested.bottom - suggested.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    layout();
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                paint_main(hwnd);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_SIZE => {
                layout();
                Some(LRESULT(0))
            }
            WM_GETMINMAXINFO => {
                // SAFETY: WM_GETMINMAXINFO's lParam points at a MINMAXINFO.
                let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
                // SAFETY: plain DPI query of our own window.
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                info.ptMinTrackSize = POINT {
                    x: theme::scale(760, dpi),
                    y: theme::scale(480, dpi),
                };
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as i32;
                let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
                if !ui_command(hwnd, id, code) {
                    with_app(|app| app.command(id, code));
                }
                Some(LRESULT(0))
            }
            WM_NOTIFY => on_notify(lparam),
            WM_TIMER => {
                with_app(|app| app.timer(wparam.0));
                Some(LRESULT(0))
            }
            WM_MEASUREITEM => {
                // SAFETY: WM_MEASUREITEM's lParam points at a MEASUREITEMSTRUCT.
                let m = unsafe { &mut *(lparam.0 as *mut MEASUREITEMSTRUCT) };
                if m.CtlType == ODT_COMBOBOX {
                    with_ui(|ui| ui.measure_combo(m));
                    return Some(LRESULT(1));
                }
                None
            }
            WM_DRAWITEM => {
                // SAFETY: WM_DRAWITEM's lParam points at a DRAWITEMSTRUCT.
                let d = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
                if d.CtlType == ODT_COMBOBOX {
                    with_ui(|ui| ui.draw_combo(d));
                    return Some(LRESULT(1));
                }
                None
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
                let hdc = HDC(wparam.0 as *mut c_void);
                with_ui(|ui| {
                    let (bg, brush) = if msg == WM_CTLCOLOREDIT {
                        (ui.palette.window, ui.window_brush.0)
                    } else {
                        (ui.palette.surface, ui.surface_brush.0)
                    };
                    // SAFETY: colors the DC the control is about to paint with.
                    unsafe {
                        SetTextColor(hdc, theme::cr(ui.palette.text));
                        SetBkColor(hdc, theme::cr(bg));
                    }
                    LRESULT(brush.0 as isize)
                })
            }
            WM_SETCURSOR => {
                let over_splitter = with_ui(|ui| {
                    let mut pt = POINT::default();
                    // SAFETY: cursor position mapped into our client area.
                    unsafe {
                        let _ = GetCursorPos(&mut pt);
                        let _ = ScreenToClient(hwnd, &mut pt);
                    }
                    HWND(wparam.0 as *mut c_void) == hwnd
                        && (ui.dragging || contains(&ui.rects.splitter, pt.x, pt.y))
                });
                if over_splitter == Some(true) {
                    // SAFETY: shows the resize cursor over the splitter.
                    unsafe {
                        let _ = SetCursor(LoadCursorW(None, IDC_SIZEWE).ok());
                    }
                    return Some(LRESULT(1));
                }
                None
            }
            WM_MOUSEMOVE => {
                let (x, y) = mouse_point(lparam);
                on_mouse_move(hwnd, x, y);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                let changed = with_ui(|ui| {
                    ui.tracking_leave = false;
                    std::mem::replace(&mut ui.splitter_hot, false)
                });
                if changed == Some(true) {
                    // SAFETY: repaints our own window.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = mouse_point(lparam);
                let start = with_ui(|ui| {
                    let hit = contains(&ui.rects.splitter, x, y);
                    ui.dragging = hit;
                    hit
                });
                if start == Some(true) {
                    // SAFETY: captures the mouse for the splitter drag.
                    unsafe {
                        SetCapture(hwnd);
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if with_ui(|ui| std::mem::replace(&mut ui.dragging, false)) == Some(true) {
                    // SAFETY: ends our own capture.
                    let _ = unsafe { ReleaseCapture() };
                    layout();
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                // Capture lost mid-drag (Alt+Tab, a popup): stop and repaint the grip.
                if with_ui(|ui| std::mem::replace(&mut ui.dragging, false)) == Some(true) {
                    // SAFETY: repaints our own window.
                    let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
                }
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                // SAFETY: WM_DPICHANGED's lParam points at the suggested window RECT.
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                on_dpi_changed(hwnd, (wparam.0 & 0xFFFF) as u32, suggested);
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                let palette = Palette::system();
                if with_ui(|ui| std::mem::replace(&mut ui.palette, palette) != palette)
                    == Some(true)
                {
                    apply_theme();
                }
                None
            }
            WM_CLOSE => {
                // SAFETY: destroys this thread's own window.
                let _ = unsafe { DestroyWindow(hwnd) };
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            _ => None,
        }
    }));
    match result {
        Ok(Some(r)) => r,
        Ok(None) => {
            // SAFETY: forwards the unmodified message.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        Err(_) => {
            tracing::error!("inspector window procedure panicked");
            LRESULT(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_is_the_inspectors_only_while_picking() {
        let esc = VK_ESCAPE.0;
        assert_eq!(shortcut(false, esc, false, true), Some(ID_CANCEL_PICK));
        assert_eq!(
            shortcut(false, esc, false, false),
            None,
            "Esc must still close the window list"
        );
        assert_eq!(shortcut(true, u16::from(b'F'), false, false), Some(ID_FIND));
        assert_eq!(shortcut(false, VK_F5.0, false, false), Some(ID_REFRESH));
        assert_eq!(shortcut(true, u16::from(b'C'), true, false), Some(ID_COPY));
        assert_eq!(
            shortcut(true, u16::from(b'C'), false, false),
            None,
            "Ctrl+C copies text elsewhere"
        );
        assert_eq!(shortcut(false, u16::from(b'F'), false, false), None);
    }

    #[test]
    fn dragging_keeps_the_splitter_under_the_cursor() {
        for dpi in [96, 120, 144, 192] {
            let px = |v| theme::scale(v, dpi);
            let (pad, gap) = (px(PAD), px(GAP));
            for client_w in [px(760), px(1240), px(2400)] {
                let panes = panes_width(client_w, pad, gap, px(200));
                for x in [
                    client_w * 3 / 10,
                    client_w / 2,
                    client_w * 6 / 10,
                    client_w * 7 / 10,
                ] {
                    // As `layout` places it.
                    let left_w = (panes as f32 * split_at(x, panes, pad, gap)) as i32;
                    let center = pad + left_w + gap / 2;
                    assert!(
                        (center - x).abs() <= 1,
                        "dpi {dpi}, width {client_w}: cursor {x}, splitter {center}"
                    );
                }
            }
        }
    }
}
