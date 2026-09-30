//! Control table and per-control helpers. The numeric IDs are a contract: UI Automation
//! reports a Win32 child's control ID as its AutomationId, and Winwright's tests target them.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::{
    HTREEITEM, TCIF_TEXT, TCITEMW, TCM_GETCURSEL, TCM_INSERTITEMW, TVI_LAST, TVI_ROOT, TVIF_HANDLE,
    TVIF_TEXT, TVINSERTSTRUCTW, TVINSERTSTRUCTW_0, TVITEMW, TVM_GETITEMW, TVM_INSERTITEMW,
    TVS_HASBUTTONS, TVS_HASLINES, TVS_LINESATROOT, TVS_SHOWSELALWAYS, WC_TABCONTROLW, WC_TREEVIEWW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_DEFPUSHBUTTON, BS_PUSHBUTTON,
    CB_ADDSTRING, CB_GETCURSEL, CB_GETLBTEXT, CB_GETLBTEXTLEN, CBS_DROPDOWNLIST, CreateWindowExW,
    ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_PASSWORD, ES_WANTRETURN, HMENU, LB_ADDSTRING,
    LB_GETCURSEL, LB_GETTEXT, LB_GETTEXTLEN, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY, MoveWindow,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SendMessageW, SetWindowPos, WINDOW_EX_STYLE,
    WINDOW_STYLE, WS_CHILD, WS_DISABLED, WS_EX_CLIENTEDGE, WS_GROUP, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use crate::win;

pub const NAME: i32 = 101;
pub const NOTES: i32 = 105;
pub const SUBMIT: i32 = 110;
pub const FEATURE: i32 = 112;
pub const SMALL: i32 = 113;
pub const LARGE: i32 = 114;
pub const COLOR: i32 = 116;
pub const ITEMS: i32 = 118;
pub const OPEN_DIALOG: i32 = 120;
pub const ADD_DELAYED: i32 = 121;
pub const DELAYED: i32 = 122;
pub const RECREATE: i32 = 123;
pub const TARGET: i32 = 124;
pub const TABS: i32 = 130;
pub const TREE: i32 = 131;
pub const STATUS: i32 = 140;

/// Client area of the main window, in DIPs.
pub const CLIENT_SIZE: [i32; 2] = [880, 640];
pub const TAB_NAMES: [&str; 2] = ["General", "Advanced"];
pub const COLORS: [&str; 3] = ["Red", "Green", "Blue"];
pub const LIST_ITEMS: usize = 200;

#[derive(Clone, Copy)]
pub enum Kind {
    Static,
    Edit,
    Button,
    Combo,
    List,
    Tab,
    Tree,
}

impl Kind {
    fn class(self) -> PCWSTR {
        match self {
            Kind::Static => w!("STATIC"),
            Kind::Edit => w!("EDIT"),
            Kind::Button => w!("BUTTON"),
            Kind::Combo => w!("COMBOBOX"),
            Kind::List => w!("LISTBOX"),
            Kind::Tab => WC_TABCONTROLW,
            Kind::Tree => WC_TREEVIEWW,
        }
    }

    fn ex_style(self) -> WINDOW_EX_STYLE {
        match self {
            Kind::Edit | Kind::List | Kind::Tree => WS_EX_CLIENTEDGE,
            _ => WINDOW_EX_STYLE::default(),
        }
    }
}

/// One child control: its ID, class, text, extra style bits, and `[x, y, w, h]` in DIPs.
pub struct Spec {
    pub id: i32,
    pub kind: Kind,
    pub text: &'static str,
    pub style: u32,
    pub rect: [i32; 4],
}

const fn spec(id: i32, kind: Kind, text: &'static str, style: u32, rect: [i32; 4]) -> Spec {
    Spec {
        id,
        kind,
        text,
        style,
        rect,
    }
}

pub const TAB: u32 = WS_TABSTOP.0;
pub const PUSH: u32 = TAB | BS_PUSHBUTTON as u32;
pub const DEFAULT_PUSH: u32 = TAB | BS_DEFPUSHBUTTON as u32;
pub const LINE_EDIT: u32 = TAB | ES_AUTOHSCROLL as u32;
/// SS_NOPREFIX (lives in `System_SystemServices`, not worth a feature): show '&' literally.
const SS_NOPREFIX: u32 = 0x80;

/// Initial controls in creation (= z- and tab-) order. Every label directly precedes the
/// control it names, which is how the MSAA proxy derives an edit's Name.
pub const MAIN: &[Spec] = &[
    spec(100, Kind::Static, "Name:", WS_GROUP.0, [16, 20, 84, 20]),
    spec(NAME, Kind::Edit, "", LINE_EDIT, [108, 16, 272, 24]),
    spec(102, Kind::Static, "Password:", 0, [16, 52, 84, 20]),
    spec(
        103,
        Kind::Edit,
        "",
        LINE_EDIT | ES_PASSWORD as u32,
        [108, 48, 272, 24],
    ),
    spec(104, Kind::Static, "Notes:", 0, [16, 84, 84, 20]),
    spec(
        NOTES,
        Kind::Edit,
        "",
        TAB | WS_VSCROLL.0 | (ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN) as u32,
        [108, 80, 272, 96],
    ),
    spec(SUBMIT, Kind::Button, "Submit", PUSH, [108, 188, 120, 28]),
    spec(
        111,
        Kind::Button,
        "Disabled Action",
        PUSH | WS_DISABLED.0,
        [240, 188, 140, 28],
    ),
    spec(
        FEATURE,
        Kind::Button,
        "Enable feature",
        TAB | BS_AUTOCHECKBOX as u32,
        [108, 228, 272, 24],
    ),
    spec(
        SMALL,
        Kind::Button,
        "Small",
        TAB | WS_GROUP.0 | BS_AUTORADIOBUTTON as u32,
        [108, 258, 120, 24],
    ),
    spec(
        LARGE,
        Kind::Button,
        "Large",
        BS_AUTORADIOBUTTON as u32,
        [240, 258, 120, 24],
    ),
    spec(115, Kind::Static, "Color:", WS_GROUP.0, [16, 296, 84, 20]),
    spec(
        COLOR,
        Kind::Combo,
        "",
        TAB | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
        [108, 292, 180, 200],
    ),
    spec(117, Kind::Static, "Items:", 0, [16, 332, 84, 20]),
    spec(
        ITEMS,
        Kind::List,
        "",
        TAB | WS_VSCROLL.0 | (LBS_NOTIFY | LBS_NOINTEGRALHEIGHT) as u32,
        [108, 328, 272, 220],
    ),
    spec(
        OPEN_DIALOG,
        Kind::Button,
        "Open Dialog",
        PUSH,
        [420, 16, 150, 28],
    ),
    spec(
        ADD_DELAYED,
        Kind::Button,
        "Add Delayed",
        PUSH,
        [420, 52, 150, 28],
    ),
    spec(RECREATE, Kind::Button, "Recreate", PUSH, [420, 88, 150, 28]),
    TARGET_SPEC,
    spec(TABS, Kind::Tab, "", TAB, [420, 132, 316, 30]),
    spec(
        TREE,
        Kind::Tree,
        "",
        TAB | TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_SHOWSELALWAYS,
        [420, 172, 316, 180],
    ),
    spec(
        STATUS,
        Kind::Static,
        "Ready",
        SS_NOPREFIX,
        [16, 600, 848, 24],
    ),
];

/// Created 1500 ms after `Add Delayed`, next to it.
pub const DELAYED_SPEC: Spec = spec(
    DELAYED,
    Kind::Button,
    "Delayed Button",
    PUSH,
    [586, 52, 150, 28],
);

/// Destroyed and re-created (new HWND, new runtime ID) by `Recreate`.
pub const TARGET_SPEC: Spec = spec(TARGET, Kind::Button, "Target", PUSH, [586, 88, 150, 28]);

/// Creates `spec` under `parent` at its DIP rectangle scaled to `dpi`.
pub fn create(parent: HWND, spec: &Spec, dpi: u32, font: HFONT) -> windows::core::Result<HWND> {
    let [x, y, width, height] = spec.rect.map(|v| win::scale(v, dpi));
    let style = WS_CHILD | WS_VISIBLE | WINDOW_STYLE(spec.style);
    // A child window's "menu" parameter carries its control ID.
    let id = HMENU(spec.id as usize as *mut c_void);
    // SAFETY: class and text are null-terminated and outlive the call; `parent` is a live
    // window owned by this thread.
    let hwnd = unsafe {
        CreateWindowExW(
            spec.kind.ex_style(),
            spec.kind.class(),
            &HSTRING::from(spec.text),
            style,
            x,
            y,
            width,
            height,
            Some(parent),
            Some(id),
            Some(win::instance()),
            None,
        )
    }?;
    win::set_font(hwnd, font);
    Ok(hwnd)
}

/// Moves an existing control to its DIP rectangle at `dpi` (after WM_DPICHANGED).
pub fn place(parent: HWND, spec: &Spec, dpi: u32, font: HFONT) {
    let Some(hwnd) = win::child(parent, spec.id) else {
        return;
    };
    let [x, y, width, height] = spec.rect.map(|v| win::scale(v, dpi));
    // SAFETY: repositions a child window owned by this thread; no pointers are passed.
    let _ = unsafe { MoveWindow(hwnd, x, y, width, height, true) };
    win::set_font(hwnd, font);
}

/// Puts `hwnd` right after `after` in z-order, which is also the Tab order.
pub fn order_after(hwnd: HWND, after: Option<HWND>) {
    let Some(after) = after else {
        return;
    };
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    // SAFETY: both windows are children of the same parent on this thread; no pointers.
    let _ = unsafe { SetWindowPos(hwnd, Some(after), 0, 0, 0, 0, flags) };
}

/// Fills the combo, list, tab, and tree controls with their fixed content.
pub fn populate(parent: HWND) {
    if let Some(combo) = win::child(parent, COLOR) {
        for color in COLORS {
            add_string(combo, CB_ADDSTRING, color);
        }
    }
    if let Some(list) = win::child(parent, ITEMS) {
        for i in 1..=LIST_ITEMS {
            add_string(list, LB_ADDSTRING, &format!("Item {i}"));
        }
    }
    if let Some(tabs) = win::child(parent, TABS) {
        for (index, name) in TAB_NAMES.iter().enumerate() {
            insert_tab(tabs, index, name);
        }
    }
    if let Some(tree) = win::child(parent, TREE) {
        let root = insert_tree_item(tree, TVI_ROOT, "Root");
        insert_tree_item(tree, root, "Child A");
        insert_tree_item(tree, root, "Child B");
    }
}

/// `message` must be CB_ADDSTRING or LB_ADDSTRING, both of which copy the string.
fn add_string(ctrl: HWND, message: u32, value: &str) {
    debug_assert!(message == CB_ADDSTRING || message == LB_ADDSTRING);
    let value = HSTRING::from(value);
    // SAFETY: *_ADDSTRING reads the null-terminated string during the call only.
    unsafe { SendMessageW(ctrl, message, None, Some(LPARAM(value.as_ptr() as isize))) };
}

pub fn combo_selection(combo: HWND) -> Option<String> {
    selected_text(combo, [CB_GETCURSEL, CB_GETLBTEXTLEN, CB_GETLBTEXT])
}

pub fn list_selection(list: HWND) -> Option<String> {
    selected_text(list, [LB_GETCURSEL, LB_GETTEXTLEN, LB_GETTEXT])
}

/// `[get_cursel, get_text_len, get_text]` for either a combo box or a list box.
fn selected_text(ctrl: HWND, [cursel, text_len, get_text]: [u32; 3]) -> Option<String> {
    // SAFETY: *_GETCURSEL takes no pointers.
    let index = unsafe { SendMessageW(ctrl, cursel, None, None) }.0;
    let index = usize::try_from(index).ok()?;
    // SAFETY: *_GETTEXTLEN takes the index by value.
    let len = unsafe { SendMessageW(ctrl, text_len, Some(WPARAM(index)), None) }.0;
    let len = usize::try_from(len).ok()?;
    let mut buf = vec![0u16; len + 1];
    let out = LPARAM(buf.as_mut_ptr() as isize);
    // SAFETY: `buf` holds len + 1 units, the documented requirement for CB_GETLBTEXT and
    // LB_GETTEXT, and outlives the call.
    let copied = unsafe { SendMessageW(ctrl, get_text, Some(WPARAM(index)), Some(out)) }.0;
    let copied = usize::try_from(copied).ok()?.min(len);
    Some(String::from_utf16_lossy(&buf[..copied]))
}

pub fn is_checked(button: HWND) -> bool {
    // SAFETY: BM_GETCHECK takes no pointers.
    unsafe { SendMessageW(button, BM_GETCHECK, None, None) }.0 == 1
}

fn insert_tab(tabs: HWND, index: usize, name: &str) {
    let mut text: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let item = TCITEMW {
        mask: TCIF_TEXT,
        pszText: PWSTR(text.as_mut_ptr()),
        ..Default::default()
    };
    // SAFETY: `item` and the text it points to outlive the call; the control copies both.
    unsafe {
        SendMessageW(
            tabs,
            TCM_INSERTITEMW,
            Some(WPARAM(index)),
            Some(LPARAM(&item as *const TCITEMW as isize)),
        )
    };
}

pub fn selected_tab(tabs: HWND) -> Option<&'static str> {
    // SAFETY: TCM_GETCURSEL takes no pointers.
    let index = unsafe { SendMessageW(tabs, TCM_GETCURSEL, None, None) }.0;
    usize::try_from(index)
        .ok()
        .and_then(|i| TAB_NAMES.get(i).copied())
}

fn insert_tree_item(tree: HWND, parent: HTREEITEM, text: &str) -> HTREEITEM {
    let mut text: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let insert = TVINSERTSTRUCTW {
        hParent: parent,
        hInsertAfter: TVI_LAST,
        Anonymous: TVINSERTSTRUCTW_0 {
            item: TVITEMW {
                mask: TVIF_TEXT,
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            },
        },
    };
    // SAFETY: `insert` and the text it points to outlive the call; the control copies both.
    let item = unsafe {
        SendMessageW(
            tree,
            TVM_INSERTITEMW,
            None,
            Some(LPARAM(&insert as *const TVINSERTSTRUCTW as isize)),
        )
    };
    HTREEITEM(item.0)
}

pub fn tree_item_text(tree: HWND, item: HTREEITEM) -> String {
    let mut buf = [0u16; 128];
    let mut query = TVITEMW {
        mask: TVIF_TEXT | TVIF_HANDLE,
        hItem: item,
        pszText: PWSTR(buf.as_mut_ptr()),
        cchTextMax: buf.len() as i32,
        ..Default::default()
    };
    // SAFETY: `query` points at `buf` with its capacity in cchTextMax; both outlive the call.
    let found = unsafe {
        SendMessageW(
            tree,
            TVM_GETITEMW,
            None,
            Some(LPARAM(&mut query as *mut TVITEMW as isize)),
        )
    };
    if found.0 == 0 || query.pszText.is_null() {
        return String::new();
    }
    // SAFETY: on success pszText points at a null-terminated string, either `buf` or the
    // control's own buffer (which it may substitute), valid until the next tree message.
    unsafe { query.pszText.to_string() }.unwrap_or_default()
}
