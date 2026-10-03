//! The details panel: a custom-painted, scrollable view of one element, or an empty/error
//! state. Its window text carries the same content as plain text for screen readers.

use std::cell::RefCell;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DRAW_TEXT_FORMAT, DT_CENTER,
    DT_EDITCONTROL, DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteDC,
    DeleteObject, EndPaint, GetDC, HDC, HGDIOBJ, InvalidateRect, PAINTSTRUCT, ReleaseDC, SRCCOPY,
    SelectObject,
};
use windows::Win32::UI::Controls::{SetScrollInfo, SetWindowTheme};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, GetClientRect,
    GetCursorPos, GetScrollInfo, HMENU, MF_GRAYED, MF_STRING, PostMessageW, SB_BOTTOM, SB_LINEDOWN,
    SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION, SB_THUMBTRACK, SB_TOP, SB_VERT,
    SCROLLINFO, SIF_ALL, SIF_TRACKPOS, SetWindowTextW, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COMMAND, WM_CONTEXTMENU, WM_ERASEBKGND,
    WM_MOUSEWHEEL, WM_PAINT, WM_SIZE, WM_VSCROLL, WS_CHILD, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, w};
use winwright_contracts::WinwrightResult;
use winwright_overlay::theme::{self, Fonts, Palette, glyph};

use crate::text::DetailView;

pub const CLASS: PCWSTR = w!("WinwrightInspectorDetails");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Model {
    Empty,
    Error(String),
    Element(Box<DetailView>),
}

struct Panel {
    hwnd: HWND,
    parent: HWND,
    copy_locator: i32,
    copy_all: i32,
    model: Model,
    scroll: i32,
    content: i32,
    palette: Palette,
    fonts: Rc<Fonts>,
}

thread_local! {
    static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) };
}

fn with_panel<R>(f: impl FnOnce(&mut Panel) -> R) -> Option<R> {
    PANEL.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Creates the panel as a child of `parent`. Its context menu posts `copy_locator` or
/// `copy_all` as WM_COMMAND to the parent.
pub fn create(
    parent: HWND,
    id: i32,
    copy_locator: i32,
    copy_all: i32,
    palette: Palette,
    fonts: Rc<Fonts>,
) -> WinwrightResult<HWND> {
    let hinstance = theme::register_class(CLASS, Some(proc))?;
    // SAFETY: the class is registered; the id travels in the menu-handle slot.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Element details"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_VSCROLL.0),
            0,
            0,
            10,
            10,
            Some(parent),
            Some(HMENU(id as isize as _)),
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| winwright_contracts::WinwrightError::Platform {
        operation: "CreateWindowExW(details)".into(),
        hresult: e.code().0,
    })?;
    PANEL.with(|cell| {
        *cell.borrow_mut() = Some(Panel {
            hwnd,
            parent,
            copy_locator,
            copy_all,
            model: Model::Empty,
            scroll: 0,
            content: 0,
            palette,
            fonts,
        });
    });
    set_style(palette, None);
    Ok(hwnd)
}

/// Shows `model`; `accessible` is the same content as plain text (the window's name).
pub fn set_model(model: Model, accessible: &str) {
    let hwnd = with_panel(|p| {
        p.model = model;
        p.scroll = 0;
        p.hwnd
    });
    if let Some(hwnd) = hwnd {
        // SAFETY: our own window; the string outlives the call.
        let _ = unsafe { SetWindowTextW(hwnd, &HSTRING::from(accessible)) };
        refresh(hwnd);
    }
}

/// New colors (theme change) and/or fonts (DPI change).
pub fn set_style(palette: Palette, fonts: Option<Rc<Fonts>>) {
    let hwnd = with_panel(|p| {
        p.palette = palette;
        if let Some(f) = fonts {
            p.fonts = f;
        }
        p.hwnd
    });
    if let Some(hwnd) = hwnd {
        let theme = if palette.dark {
            w!("DarkMode_Explorer")
        } else {
            w!("Explorer")
        };
        // SAFETY: themes our own window's scrollbar.
        let _ = unsafe { SetWindowTheme(hwnd, theme, PCWSTR::null()) };
        refresh(hwnd);
    }
}

/// Re-measures the content, updates the scrollbar, and repaints.
fn refresh(hwnd: HWND) {
    let mut client = RECT::default();
    // SAFETY: reads our own client rect; a window DC is borrowed for measuring.
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
    }
    let info = with_panel(|p| {
        // SAFETY: borrowed and released here.
        let dc = unsafe { GetDC(Some(hwnd)) };
        p.content = render(dc, p, client.right, client.bottom, false);
        // SAFETY: releases the DC borrowed above.
        unsafe { ReleaseDC(Some(hwnd), dc) };
        let max = (p.content - client.bottom).max(0);
        p.scroll = p.scroll.clamp(0, max);
        SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as u32,
            fMask: SIF_ALL,
            nMin: 0,
            nMax: (p.content - 1).max(0),
            nPage: client.bottom.max(0) as u32,
            nPos: p.scroll,
            nTrackPos: 0,
        }
    });
    if let Some(info) = info {
        // SAFETY: our own window's scrollbar; then a repaint.
        unsafe {
            SetScrollInfo(hwnd, SB_VERT, &info, true);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
    }
}

fn scroll_to(hwnd: HWND, target: i32) {
    let changed = with_panel(|p| {
        let mut client = RECT::default();
        // SAFETY: reads our own client rect.
        let _ = unsafe { GetClientRect(hwnd, &mut client) };
        let next = target.clamp(0, (p.content - client.bottom).max(0));
        let changed = next != p.scroll;
        p.scroll = next;
        changed
    });
    if changed == Some(true) {
        refresh(hwnd);
    }
}

fn current_scroll() -> i32 {
    with_panel(|p| p.scroll).unwrap_or(0)
}

struct Pen<'a> {
    hdc: HDC,
    p: &'a Palette,
    f: &'a Fonts,
    draw: bool,
    dy: i32,
}

impl Pen<'_> {
    fn px(&self, v: i32) -> i32 {
        self.f.px(v)
    }

    /// Wrapped text at (x, y) within `w`; returns its height.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        font: &theme::Font,
        s: &str,
        x: i32,
        y: i32,
        w: i32,
        rgb: u32,
        flags: DRAW_TEXT_FORMAT,
    ) -> i32 {
        let h = theme::measure(self.hdc, font, s, Some(w), flags | DT_WORDBREAK).1;
        if self.draw {
            theme::text(
                self.hdc,
                font,
                s,
                RECT {
                    left: x,
                    top: y + self.dy,
                    right: x + w,
                    bottom: y + self.dy + h,
                },
                rgb,
                flags | DT_WORDBREAK,
            );
        }
        h
    }

    fn rounded(&self, r: RECT, radius: i32, fill: u32, border: Option<u32>) {
        if self.draw {
            theme::rounded(self.hdc, self.shift(r), radius as f32, fill, border);
        }
    }

    fn shift(&self, r: RECT) -> RECT {
        RECT {
            top: r.top + self.dy,
            bottom: r.bottom + self.dy,
            ..r
        }
    }

    /// A pill with an optional status dot; returns its width.
    fn pill(&self, x: i32, y: i32, label: &str, dot: Option<u32>, fill: u32, fg: u32) -> i32 {
        let h = self.px(24);
        let label_w = theme::measure(self.hdc, &self.f.small, label, None, DRAW_TEXT_FORMAT(0)).0;
        let dot_w = if dot.is_some() { self.px(14) } else { 0 };
        let w = self.px(10) + dot_w + label_w + self.px(10);
        if self.draw {
            let r = self.shift(rect(x, y, w, h));
            theme::rounded(self.hdc, r, (h / 2) as f32, fill, None);
            if let Some(color) = dot {
                let d = self.px(8);
                theme::dot(
                    self.hdc,
                    rect(r.left + self.px(10), r.top + (h - d) / 2, d, d),
                    color,
                );
            }
            theme::text(
                self.hdc,
                &self.f.small,
                label,
                RECT {
                    left: r.left + self.px(10) + dot_w,
                    ..r
                },
                fg,
                DT_SINGLELINE | DT_VCENTER,
            );
        }
        w
    }

    fn heading(&self, s: &str, x: i32, y: i32, w: i32) -> i32 {
        self.text(
            &self.f.small_strong,
            s,
            x,
            y,
            w,
            self.p.muted,
            DT_SINGLELINE | DT_LEFT,
        ) + self.px(8)
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

/// Measures (`draw == false`) or paints the model; returns the content height.
fn render(hdc: HDC, panel: &Panel, width: i32, height: i32, draw: bool) -> i32 {
    let pen = Pen {
        hdc,
        p: &panel.palette,
        f: &panel.fonts,
        draw,
        dy: -panel.scroll,
    };
    match &panel.model {
        Model::Empty => render_state(
            &pen,
            width,
            height,
            glyph::POINTER,
            pen.p.accent,
            "Nothing selected",
            "Choose an element in the tree, or press Pick element and hover over any app for three seconds.",
        ),
        Model::Error(message) => render_state(
            &pen,
            width,
            height,
            glyph::WARNING,
            pen.p.warn,
            "Can't inspect this element",
            message,
        ),
        Model::Element(view) => render_element(&pen, width, view),
    }
}

fn render_state(
    pen: &Pen<'_>,
    width: i32,
    height: i32,
    icon: char,
    color: u32,
    title: &str,
    body: &str,
) -> i32 {
    let circle = pen.px(64);
    let body_w = (width - pen.px(48)).min(pen.px(340)).max(pen.px(120));
    let title_h = theme::measure(pen.hdc, &pen.f.strong, title, Some(body_w), DT_WORDBREAK).1;
    let body_h = theme::measure(pen.hdc, &pen.f.small, body, Some(body_w), DT_WORDBREAK).1;
    let total = circle + pen.px(16) + title_h + pen.px(6) + body_h;
    let top = ((height - total) / 2).max(pen.px(24));
    if pen.draw {
        let c = rect((width - circle) / 2, top, circle, circle);
        theme::dot(pen.hdc, c, theme::mix(color, pen.p.surface, 0.86));
        theme::icon(pen.hdc, &pen.f.icons_large, icon, c, color);
    }
    let x = (width - body_w) / 2;
    let y = top + circle + pen.px(16);
    pen.text(&pen.f.strong, title, x, y, body_w, pen.p.text, DT_CENTER);
    pen.text(
        &pen.f.small,
        body,
        x,
        y + title_h + pen.px(6),
        body_w,
        pen.p.muted,
        DT_CENTER,
    );
    // The state view never scrolls.
    height.min(top + total)
}

fn render_element(pen: &Pen<'_>, width: i32, v: &DetailView) -> i32 {
    let p = pen.p;
    let f = pen.f;
    let pad = pen.px(20);
    let w = (width - pad * 2).max(pen.px(80));
    let mut y = pad;

    // Role chip, title, subtitle.
    pen.pill(
        pad,
        y,
        &v.role,
        None,
        theme::mix(p.role, p.surface, 0.86),
        p.role,
    );
    y += pen.px(24) + pen.px(10);
    y += pen.text(
        &f.title,
        &v.title,
        pad,
        y,
        w,
        p.text,
        DT_LEFT | DT_EDITCONTROL,
    ) + pen.px(2);
    y += pen.text(
        &f.small,
        &v.subtitle,
        pad,
        y,
        w,
        p.muted,
        DT_LEFT | DT_EDITCONTROL,
    ) + pen.px(14);

    // State pills (wrapping).
    let mut x = pad;
    let pill_h = pen.px(24);
    let mut pills: Vec<(String, Option<u32>, u32, u32)> = v
        .states
        .iter()
        .map(|(label, on)| {
            if *on {
                (
                    label.clone(),
                    Some(p.good),
                    theme::mix(p.good, p.surface, 0.88),
                    p.text,
                )
            } else {
                (label.clone(), Some(p.faint), p.window, p.muted)
            }
        })
        .collect();
    if v.sensitive {
        pills.push((
            "Sensitive: value never read".into(),
            Some(p.warn),
            theme::mix(p.warn, p.surface, 0.86),
            p.text,
        ));
    }
    for (label, dot, fill, fg) in &pills {
        let needed = Pen {
            draw: false,
            ..*pen
        }
        .pill(0, 0, label, *dot, *fill, *fg);
        if x > pad && x + needed > pad + w {
            x = pad;
            y += pill_h + pen.px(6);
        }
        x += pen.pill(x, y, label, *dot, *fill, *fg) + pen.px(6);
    }
    y += pill_h + pen.px(22);

    // Property sections: key/value rows in a rounded container.
    let key_w = pen.px(112);
    let inner = pen.px(12);
    let val_w = (w - inner * 2 - key_w).max(pen.px(60));
    for (title, rows) in &v.sections {
        y += pen.heading(title, pad, y, w);
        let heights: Vec<i32> = rows
            .iter()
            .map(|(k, val)| {
                let kh = theme::measure(pen.hdc, &f.small, k, Some(key_w), DT_WORDBREAK).1;
                let vh = theme::measure(
                    pen.hdc,
                    &f.body,
                    val,
                    Some(val_w),
                    DT_WORDBREAK | DT_EDITCONTROL,
                )
                .1;
                kh.max(vh) + pen.px(16)
            })
            .collect();
        let box_h: i32 = heights.iter().sum();
        pen.rounded(rect(pad, y, w, box_h), pen.px(6), p.window, Some(p.border));
        let mut ry = y;
        for (i, ((k, val), h)) in rows.iter().zip(&heights).enumerate() {
            if i > 0 && pen.draw {
                theme::fill(
                    pen.hdc,
                    pen.shift(rect(pad + inner, ry, w - inner * 2, 1)),
                    p.border,
                );
            }
            pen.text(
                &f.small,
                k,
                pad + inner,
                ry + pen.px(9),
                key_w,
                p.muted,
                DT_LEFT,
            );
            pen.text(
                &f.body,
                val,
                pad + inner + key_w,
                ry + pen.px(8),
                val_w,
                p.text,
                DT_LEFT | DT_EDITCONTROL,
            );
            ry += h;
        }
        y += box_h + pen.px(20);
    }

    // Patterns as chips.
    if !v.patterns.is_empty() {
        y += pen.heading("Patterns", pad, y, w);
        let mut x = pad;
        for pattern in &v.patterns {
            let needed = Pen {
                draw: false,
                ..*pen
            }
            .pill(0, 0, pattern, None, p.window, p.text);
            if x > pad && x + needed > pad + w {
                x = pad;
                y += pill_h + pen.px(6);
            }
            x += pen.pill(x, y, pattern, None, p.selection, p.on_selection()) + pen.px(6);
        }
        y += pill_h + pen.px(20);
    }

    // Locator as a code block.
    if !v.locator.is_empty() {
        y += pen.heading("Locator", pad, y, w);
        let code_pad = pen.px(12);
        let th = theme::measure(
            pen.hdc,
            &f.mono,
            &v.locator,
            Some(w - code_pad * 2),
            DT_WORDBREAK | DT_EDITCONTROL,
        )
        .1;
        pen.rounded(
            rect(pad, y, w, th + code_pad * 2),
            pen.px(6),
            p.code,
            Some(p.border),
        );
        pen.text(
            &f.mono,
            &v.locator,
            pad + code_pad,
            y + code_pad,
            w - code_pad * 2,
            p.text,
            DT_LEFT | DT_EDITCONTROL,
        );
        y += th + code_pad * 2 + pen.px(8);
        y += pen.text(
            &f.small,
            "Paste into desktop_find or desktop_click, or copy it with Copy locator.",
            pad,
            y,
            w,
            p.faint,
            DT_LEFT | DT_END_ELLIPSIS,
        );
    }
    y + pad
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: double-buffered WM_PAINT on our own window; every GDI object is released.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let (w, h) = (client.right.max(1), client.bottom.max(1));
        let mem = CreateCompatibleDC(Some(hdc));
        let bitmap = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        with_panel(|p| {
            theme::fill(mem, client, p.palette.surface);
            render(mem, p, w, h, true);
        });
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn context_menu(hwnd: HWND) {
    let Some((parent, copy_locator, copy_all, has_element)) = with_panel(|p| {
        (
            p.parent,
            p.copy_locator,
            p.copy_all,
            matches!(p.model, Model::Element(_)),
        )
    }) else {
        return;
    };
    // SAFETY: a popup menu created, tracked, and destroyed on this thread.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let flags = if has_element {
            MF_STRING
        } else {
            MF_STRING | MF_GRAYED
        };
        let _ = AppendMenuW(menu, flags, copy_locator as usize, w!("Copy locator"));
        let _ = AppendMenuW(menu, flags, copy_all as usize, w!("Copy all properties"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        if chosen.0 > 0 {
            let _ = PostMessageW(
                Some(parent),
                WM_COMMAND,
                WPARAM(chosen.0 as usize),
                LPARAM(0),
            );
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // A panic must not unwind into user32 (that aborts the process).
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle(hwnd, msg, wparam, lparam)
    }))
    .unwrap_or_else(|_| {
        tracing::error!("details panel procedure panicked");
        LRESULT(0)
    })
}

fn handle(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let line = with_panel(|p| p.fonts.px(40)).unwrap_or(40);
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            refresh(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
            scroll_to(hwnd, current_scroll() - delta * line / 120);
            LRESULT(0)
        }
        WM_VSCROLL => {
            let mut client = RECT::default();
            // SAFETY: reads our own client rect.
            let _ = unsafe { GetClientRect(hwnd, &mut client) };
            let page = client.bottom;
            let now = current_scroll();
            let code = (wparam.0 & 0xFFFF) as i32;
            let target = match code {
                c if c == SB_LINEUP.0 => now - line,
                c if c == SB_LINEDOWN.0 => now + line,
                c if c == SB_PAGEUP.0 => now - page,
                c if c == SB_PAGEDOWN.0 => now + page,
                c if c == SB_TOP.0 => 0,
                c if c == SB_BOTTOM.0 => i32::MAX / 2,
                c if c == SB_THUMBTRACK.0 || c == SB_THUMBPOSITION.0 => {
                    let mut info = SCROLLINFO {
                        cbSize: size_of::<SCROLLINFO>() as u32,
                        fMask: SIF_TRACKPOS,
                        ..Default::default()
                    };
                    // SAFETY: `info` is a SCROLLINFO out-parameter for our own scrollbar.
                    let _ = unsafe { GetScrollInfo(hwnd, SB_VERT, &mut info) };
                    info.nTrackPos
                }
                _ => now,
            };
            scroll_to(hwnd, target);
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            context_menu(hwnd);
            LRESULT(0)
        }
        // SAFETY: forwards the unmodified message.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
