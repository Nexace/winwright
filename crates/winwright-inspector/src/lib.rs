//! Native Inspector (spec §29): choose a window, browse its UI Automation tree, see every
//! property of an element, pick the element under the cursor, highlight it on screen, and
//! copy a locator for the CLI/MCP. Plain Win32 controls on the calling thread; no WebView.
//!
//! Read-only by design: the Inspector never clicks, types, or changes anything.

mod text;

use std::cell::RefCell;
use std::sync::Arc;

use windows::Win32::Foundation::{
    GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, FONT_CHARSET, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION,
    FONT_QUALITY, HFONT, HGDIOBJ,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Controls::{
    ICC_STANDARD_CLASSES, ICC_TREEVIEW_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, NMHDR,
    NMTREEVIEWW, TVE_EXPAND, TVI_LAST, TVI_ROOT, TVIF_PARAM, TVIF_TEXT, TVINSERTSTRUCTW,
    TVINSERTSTRUCTW_0, TVITEMW, TVM_DELETEITEM, TVM_EXPAND, TVM_INSERTITEMW, TVN_SELCHANGEDW,
    TVS_HASBUTTONS, TVS_HASLINES, TVS_LINESATROOT, TVS_SHOWSELALWAYS, WC_TREEVIEWW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, CBN_SELCHANGE, CBS_DROPDOWNLIST,
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW,
    GetWindowThreadProcessId, IDC_ARROW, IsDialogMessageW, KillTimer, LoadCursorW, MSG, MoveWindow,
    PostQuitMessage, RegisterClassExW, SW_SHOW, SendMessageW, SetTimer, SetWindowTextW, ShowWindow,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_NOTIFY,
    WM_SETFONT, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN,
    WS_EX_CLIENTEDGE, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL, WindowFromPoint,
};
use windows::core::{HSTRING, PCWSTR, w};
use winwright_contracts::action::ElementTarget;
use winwright_contracts::element::{ElementDetails, ElementInfo};
use winwright_contracts::ids::SessionId;
use winwright_contracts::overlay::{HighlightRequest, OverlayStyle};
use winwright_contracts::snapshot::{SnapshotNode, SnapshotRequest, SnapshotTarget};
use winwright_contracts::window::{WindowInfo, WindowSelector};
use winwright_contracts::{WinwrightError, WinwrightResult};
use winwright_core::session::Session;
use winwright_core::{Engine, InspectRequest};

const ID_COMBO: i32 = 100;
const ID_REFRESH: i32 = 101;
const ID_PICK: i32 = 102;
const ID_HIGHLIGHT: i32 = 103;
const ID_COPY: i32 = 104;
const ID_TREE: i32 = 105;
const TIMER_PICK: usize = 1;
const PICK_SECONDS: u32 = 3;
const HIGHLIGHT_MS: u64 = 1_500;

struct App {
    engine: Engine,
    rt: tokio::runtime::Runtime,
    session: Arc<Session>,
    main: HWND,
    combo: HWND,
    tree: HWND,
    details: HWND,
    status: HWND,
    buttons: [HWND; 4],
    font: HFONT,
    windows: Vec<WindowInfo>,
    items: Vec<ElementInfo>,
    selected_window: Option<String>,
    locator: Option<String>,
    current_ref: Option<String>,
    countdown: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
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
    // SAFETY: plain initialization/registration calls with fully initialized structs.
    let (hinstance, main) = unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TREEVIEW_CLASSES | ICC_STANDARD_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
        let hinstance: HINSTANCE = GetModuleHandleW(None)
            .map_err(|e| platform("GetModuleHandleW", &e))?
            .into();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(16 as _), // COLOR_BTNFACE + 1
            lpszClassName: w!("WinwrightInspector"),
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            let err = GetLastError();
            if err.0 != 1410 {
                // ERROR_CLASS_ALREADY_EXISTS is fine.
                return Err(WinwrightError::Platform {
                    operation: "RegisterClassExW(inspector)".into(),
                    hresult: windows::core::HRESULT::from_win32(err.0).0,
                });
            }
        }
        let main = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("WinwrightInspector"),
            w!("Winwright Inspector"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            80,
            80,
            1200,
            780,
            None,
            None,
            Some(hinstance),
            None,
        )
        .map_err(|e| platform("CreateWindowExW(inspector)", &e))?;
        (hinstance, main)
    };
    let app = create_children(engine, rt, session, hinstance, main)?;
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    with_app(|app| {
        app.layout();
        app.load_windows();
    });
    // SAFETY: `main` was created above on this thread.
    unsafe {
        let _ = ShowWindow(main, SW_SHOW);
    }
    let mut msg = MSG::default();
    // SAFETY: standard message loop on the thread that owns the windows.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(main, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().take() {
            // SAFETY: the font was created by this thread and is no longer selected anywhere.
            let _ = unsafe { DeleteObject(HGDIOBJ(app.font.0)) };
        }
    });
    Ok(())
}

fn create_children(
    engine: Engine,
    rt: tokio::runtime::Runtime,
    session: Arc<Session>,
    hinstance: HINSTANCE,
    main: HWND,
) -> WinwrightResult<App> {
    let child = |class: PCWSTR, text: PCWSTR, style: u32, ex: WINDOW_EX_STYLE, id: i32| {
        // SAFETY: creates a child of `main` on this thread; the menu handle carries the id.
        unsafe {
            CreateWindowExW(
                ex,
                class,
                text,
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
                0,
                0,
                10,
                10,
                Some(main),
                Some(windows::Win32::UI::WindowsAndMessaging::HMENU(
                    id as isize as _,
                )),
                Some(hinstance),
                None,
            )
        }
        .map_err(|e| platform("CreateWindowExW(child)", &e))
    };
    let combo = child(
        w!("COMBOBOX"),
        w!(""),
        CBS_DROPDOWNLIST as u32 | WS_VSCROLL.0 | WS_TABSTOP.0,
        WINDOW_EX_STYLE(0),
        ID_COMBO,
    )?;
    let button =
        |text: PCWSTR, id: i32| child(w!("BUTTON"), text, WS_TABSTOP.0, WINDOW_EX_STYLE(0), id);
    let buttons = [
        button(w!("Refresh"), ID_REFRESH)?,
        button(w!("Pick (3 s)"), ID_PICK)?,
        button(w!("Highlight"), ID_HIGHLIGHT)?,
        button(w!("Copy locator"), ID_COPY)?,
    ];
    let tree = child(
        WC_TREEVIEWW,
        w!(""),
        TVS_HASBUTTONS
            | TVS_HASLINES
            | TVS_LINESATROOT
            | TVS_SHOWSELALWAYS
            | WS_TABSTOP.0
            | WS_BORDER.0,
        WS_EX_CLIENTEDGE,
        ID_TREE,
    )?;
    let details = child(
        w!("EDIT"),
        w!(""),
        (windows::Win32::UI::WindowsAndMessaging::ES_MULTILINE
            | windows::Win32::UI::WindowsAndMessaging::ES_READONLY
            | windows::Win32::UI::WindowsAndMessaging::ES_AUTOVSCROLL) as u32
            | WS_VSCROLL.0
            | WS_TABSTOP.0,
        WS_EX_CLIENTEDGE,
        0,
    )?;
    let status = child(w!("STATIC"), w!("Ready"), 0, WINDOW_EX_STYLE(0), 0)?;
    // SAFETY: reads the window's DPI and creates a GDI font owned by the App.
    let font = unsafe {
        let dpi = GetDpiForWindow(main).max(96) as i32;
        CreateFontW(
            -(9 * dpi / 72),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            FONT_CHARSET(0),
            FONT_OUTPUT_PRECISION(0),
            FONT_CLIP_PRECISION(0),
            FONT_QUALITY(5),
            0,
            w!("Segoe UI"),
        )
    };
    for hwnd in [combo, tree, details, status].into_iter().chain(buttons) {
        // SAFETY: WM_SETFONT with a live font handle; redraw flag set.
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }
    }
    Ok(App {
        engine,
        rt,
        session,
        main,
        combo,
        tree,
        details,
        status,
        buttons,
        font,
        windows: Vec::new(),
        items: Vec::new(),
        selected_window: None,
        locator: None,
        current_ref: None,
        countdown: 0,
    })
}

/// Runs `f` with the app unless it is already borrowed (re-entrant notifications are skipped).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

impl App {
    fn scale(&self, v: i32) -> i32 {
        // SAFETY: reads the DPI of a live window.
        let dpi = unsafe { GetDpiForWindow(self.main) }.max(96) as i32;
        v * dpi / 96
    }

    fn set_status(&self, text: &str) {
        // SAFETY: `status` is a live child window; the string outlives the call.
        let _ = unsafe { SetWindowTextW(self.status, &wide(text)) };
    }

    fn set_details(&self, text: &str) {
        let text = text.replace('\n', "\r\n");
        // SAFETY: as above.
        let _ = unsafe { SetWindowTextW(self.details, &wide(&text)) };
    }

    fn layout(&self) {
        let mut rc = windows::Win32::Foundation::RECT::default();
        // SAFETY: reads the client rect of a live window and moves its children.
        unsafe {
            let _ = GetClientRect(self.main, &mut rc);
            let (w, h) = (rc.right, rc.bottom);
            let pad = self.scale(8);
            let bar = self.scale(28);
            let status_h = self.scale(22);
            let combo_w = self.scale(420);
            let _ = MoveWindow(self.combo, pad, pad, combo_w, self.scale(400), true);
            let mut x = pad * 2 + combo_w;
            for (b, width) in self.buttons.iter().zip([90, 100, 90, 110]) {
                let bw = self.scale(width);
                let _ = MoveWindow(*b, x, pad, bw, bar, true);
                x += bw + pad;
            }
            let top = pad * 2 + bar;
            let body_h = (h - top - status_h - pad * 2).max(10);
            let left_w = (w - pad * 3) * 45 / 100;
            let _ = MoveWindow(self.tree, pad, top, left_w, body_h, true);
            let _ = MoveWindow(
                self.details,
                pad * 2 + left_w,
                top,
                (w - pad * 3 - left_w).max(10),
                body_h,
                true,
            );
            let _ = MoveWindow(
                self.status,
                pad,
                h - status_h - pad / 2,
                w - pad * 2,
                status_h,
                true,
            );
        }
    }

    fn load_windows(&mut self) {
        let own = std::process::id();
        self.windows = match self.engine.list_windows() {
            Ok(ws) => ws.into_iter().filter(|w| w.process_id != own).collect(),
            Err(e) => {
                self.set_status(&format!("Cannot list windows: {e}"));
                return;
            }
        };
        let foreground = self.windows.iter().position(|w| w.foreground).unwrap_or(0);
        // SAFETY: combo box messages on a live child; strings outlive each call.
        unsafe {
            SendMessageW(self.combo, CB_RESETCONTENT, None, None);
            for w in &self.windows {
                let label = wide(&format!("{}  ({})", w.title, w.process_name));
                SendMessageW(
                    self.combo,
                    CB_ADDSTRING,
                    None,
                    Some(LPARAM(label.as_ptr() as isize)),
                );
            }
            SendMessageW(self.combo, CB_SETCURSEL, Some(WPARAM(foreground)), None);
        }
        self.load_tree();
    }

    fn current_window(&self) -> Option<&WindowInfo> {
        // SAFETY: combo box query on a live child.
        let index = unsafe { SendMessageW(self.combo, CB_GETCURSEL, None, None) }.0;
        usize::try_from(index)
            .ok()
            .and_then(|i| self.windows.get(i))
    }

    fn load_tree(&mut self) {
        let Some(window) = self.current_window().cloned() else {
            self.set_status("No window selected");
            return;
        };
        self.set_status(&format!("Reading {:?}...", window.title));
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
        // SAFETY: deleting every item of a live tree view.
        unsafe {
            SendMessageW(self.tree, TVM_DELETEITEM, None, Some(LPARAM(TVI_ROOT.0)));
        }
        self.items.clear();
        match snapshot {
            Ok(s) => {
                for node in s.nodes.as_deref().unwrap_or_default() {
                    self.insert(node, TVI_ROOT, 0);
                }
                self.selected_window = Some(window.title.clone());
                self.set_status(&format!(
                    "{} elements in {:?}{}. Select one to see its properties.",
                    s.node_count,
                    window.title,
                    if s.truncated { " (truncated)" } else { "" }
                ));
            }
            Err(e) => self.set_status(&format!("Cannot read {:?}: {e}", window.title)),
        }
    }

    fn insert(
        &mut self,
        node: &SnapshotNode,
        parent: windows::Win32::UI::Controls::HTREEITEM,
        depth: u32,
    ) {
        let index = self.items.len();
        self.items.push(node.element.clone());
        let mut label: Vec<u16> = text::tree_label(&node.element).encode_utf16().collect();
        label.push(0);
        let insert = TVINSERTSTRUCTW {
            hParent: parent,
            hInsertAfter: TVI_LAST,
            Anonymous: TVINSERTSTRUCTW_0 {
                item: TVITEMW {
                    mask: TVIF_TEXT | TVIF_PARAM,
                    pszText: windows::core::PWSTR(label.as_mut_ptr()),
                    lParam: LPARAM(index as isize),
                    ..Default::default()
                },
            },
        };
        // SAFETY: `insert` and `label` outlive the synchronous insert.
        let item = unsafe {
            SendMessageW(
                self.tree,
                TVM_INSERTITEMW,
                None,
                Some(LPARAM(&insert as *const _ as isize)),
            )
        };
        let handle = windows::Win32::UI::Controls::HTREEITEM(item.0);
        for child in &node.children {
            self.insert(child, handle, depth + 1);
        }
        if depth < 2 {
            // SAFETY: expands an item of a live tree view.
            unsafe {
                SendMessageW(
                    self.tree,
                    TVM_EXPAND,
                    Some(WPARAM(TVE_EXPAND.0 as usize)),
                    Some(LPARAM(item.0)),
                );
            }
        }
    }

    fn show(&mut self, reference: &str) {
        let details = self.rt.block_on(
            self.engine
                .inspect(&self.session, InspectRequest::Ref(reference.into())),
        );
        self.apply_details(details);
    }

    fn apply_details(&mut self, details: WinwrightResult<ElementDetails>) {
        match details {
            Ok(d) => {
                self.current_ref = Some(d.element.reference.clone());
                self.locator = Some(text::locator_json(&d, self.selected_window.as_deref()));
                self.set_details(&text::details(
                    &d,
                    self.locator.as_deref().unwrap_or_default(),
                ));
                self.highlight(&d.element.reference);
            }
            Err(e) => {
                self.locator = None;
                self.current_ref = None;
                self.set_details(&format!("Cannot inspect this element: {e}"));
            }
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
            self.set_status(&format!("Cannot highlight: {e}"));
        }
    }

    fn start_pick(&mut self) {
        self.countdown = PICK_SECONDS;
        self.set_status(&format!(
            "Pick: hover over any element in another app... {PICK_SECONDS}"
        ));
        // SAFETY: timer on this thread's live main window.
        unsafe {
            SetTimer(Some(self.main), TIMER_PICK, 1_000, None);
        }
    }

    fn tick_pick(&mut self) {
        self.countdown = self.countdown.saturating_sub(1);
        if self.countdown > 0 {
            self.set_status(&format!(
                "Pick: hover over any element in another app... {}",
                self.countdown
            ));
            return;
        }
        // SAFETY: stops this window's timer and reads the cursor/window under it.
        let over_self = unsafe {
            let _ = KillTimer(Some(self.main), TIMER_PICK);
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let hwnd = WindowFromPoint(pt);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            pid == std::process::id()
        };
        if over_self {
            self.set_status("The cursor was over the Inspector. Press Pick and hover another app.");
            return;
        }
        let details = self.rt.block_on(
            self.engine
                .inspect(&self.session, InspectRequest::UnderCursor),
        );
        self.set_status("Picked the element under the cursor.");
        self.apply_details(details);
    }

    fn copy_locator(&self) {
        let Some(locator) = &self.locator else {
            self.set_status("Select or pick an element first.");
            return;
        };
        match set_clipboard(self.main, locator) {
            Ok(()) => self.set_status(
                "Locator copied. Paste it into desktop_click / desktop_find arguments.",
            ),
            Err(e) => self.set_status(&format!("Cannot copy: {e}")),
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
                return Err(WinwrightError::invalid("cannot lock clipboard memory"));
            }
            std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, target as *mut u8, bytes);
            let _ = GlobalUnlock(memory);
            SetClipboardData(13, Some(HANDLE(memory.0)))
                .map_err(|e| platform("SetClipboardData", &e))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => {
            with_app(|app| app.layout());
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            with_app(|app| match id {
                ID_REFRESH => app.load_windows(),
                ID_COMBO if code == CBN_SELCHANGE => app.load_tree(),
                ID_PICK => app.start_pick(),
                ID_HIGHLIGHT => match app.current_ref.clone() {
                    Some(reference) => app.highlight(&reference),
                    None => app.set_status("Select or pick an element first."),
                },
                ID_COPY => app.copy_locator(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: WM_NOTIFY's lParam points at an NMHDR (NMTREEVIEWW for tree events).
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            if header.idFrom == ID_TREE as usize && header.code == TVN_SELCHANGEDW {
                // SAFETY: TVN_SELCHANGEDW carries an NMTREEVIEWW.
                let tv = unsafe { &*(lparam.0 as *const NMTREEVIEWW) };
                let index = tv.itemNew.lParam.0 as usize;
                with_app(|app| {
                    if let Some(reference) = app.items.get(index).map(|i| i.reference.clone()) {
                        app.show(&reference);
                    }
                });
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_PICK => {
            with_app(|app| app.tick_pick());
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: destroys this thread's own window.
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: forwards the unmodified message.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
