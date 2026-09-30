//! Win32 half of an overlay (native UI thread only): monitor + DPI lookup, GDI text into a
//! 32-bit DIB section, and the layered, click-through, never-activating popup window.

use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER,
    BLENDFUNCTION, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW,
    FONT_WEIGHT, FW_BOLD, FW_SEMIBOLD, GdiFlush, GetMonitorInfoW, HDC, HGDIOBJ,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect, OUT_DEFAULT_PRECIS, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow,
    WINDOW_EX_STYLE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};
use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::overlay::OverlayRequest;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::layout::{self, LayoutInput, Metrics};
use crate::paint::{self, Canvas};
use crate::platform;

pub const OVERLAY_CLASS: PCWSTR = w!("WinwrightOverlay");

/// Layered + click-through + topmost, never activated, and kept off the taskbar and Alt+Tab.
const OVERLAY_EX_STYLE: WINDOW_EX_STYLE = WINDOW_EX_STYLE(
    WS_EX_LAYERED.0
        | WS_EX_TRANSPARENT.0
        | WS_EX_TOPMOST.0
        | WS_EX_TOOLWINDOW.0
        | WS_EX_NOACTIVATE.0,
);

const BASE_DPI: u32 = 96;

fn last_error(operation: &str) -> WinwrightError {
    platform(operation, &windows::core::Error::from_thread())
}

fn to_physical(r: RECT) -> PhysicalRect {
    PhysicalRect::new(r.left, r.top, r.right, r.bottom)
}

fn to_win32(r: PhysicalRect) -> RECT {
    RECT {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// Label text as one line of UTF-16: control characters become spaces, ends are trimmed.
pub fn label_text(label: &str) -> Vec<u16> {
    let line: String = label
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    line.trim().encode_utf16().collect()
}

struct MonitorGeometry {
    monitor: PhysicalRect,
    work: PhysicalRect,
    dpi: u32,
}

/// The monitor that shows most of `target` (or the nearest one), in physical pixels because
/// this thread is Per-Monitor-V2 aware.
fn monitor_geometry(target: PhysicalRect) -> WinwrightResult<MonitorGeometry> {
    let rect = to_win32(target);
    // SAFETY: `rect` is a valid RECT for the duration of the call.
    let monitor = unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a MONITORINFO out-parameter with `cbSize` set.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Err(last_error("GetMonitorInfoW"));
    }
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: both out-pointers are valid u32s.
    let dpi = match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
    {
        Ok(()) if dpi_x > 0 => dpi_x,
        _ => BASE_DPI,
    };
    Ok(MonitorGeometry {
        monitor: to_physical(info.rcMonitor),
        work: to_physical(info.rcWork),
        dpi,
    })
}

/// A memory DC compatible with the screen, deleted on drop.
struct MemoryDc(HDC);

impl MemoryDc {
    fn new() -> WinwrightResult<Self> {
        // SAFETY: no reference DC means "compatible with the screen".
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return Err(last_error("CreateCompatibleDC"));
        }
        Ok(Self(dc))
    }
}

impl Drop for MemoryDc {
    fn drop(&mut self) {
        // SAFETY: we created this DC; every `Selection` into it was restored before this drop.
        let _ = unsafe { DeleteDC(self.0) };
    }
}

/// A GDI object (font or bitmap) deleted on drop.
struct GdiObject(HGDIOBJ);

impl Drop for GdiObject {
    fn drop(&mut self) {
        // SAFETY: we created this object and it is no longer selected into any DC.
        let _ = unsafe { DeleteObject(self.0) };
    }
}

/// Selects an object into a DC and restores the previous one on drop.
struct Selection {
    dc: HDC,
    previous: HGDIOBJ,
}

impl Selection {
    fn new(dc: HDC, object: &GdiObject) -> Self {
        // SAFETY: `dc` and `object` are live GDI handles created by this module.
        let previous = unsafe { SelectObject(dc, object.0) };
        Self { dc, previous }
    }
}

impl Drop for Selection {
    fn drop(&mut self) {
        // SAFETY: puts back the object that was selected before `Selection::new`.
        unsafe { SelectObject(self.dc, self.previous) };
    }
}

fn font(height: i32, weight: FONT_WEIGHT) -> WinwrightResult<GdiObject> {
    // SAFETY: plain values plus a static, NUL-terminated face name. A negative height is the
    // em height in pixels.
    let font = unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            0,
            w!("Segoe UI"),
        )
    };
    if font.is_invalid() {
        return Err(last_error("CreateFontW"));
    }
    Ok(GdiObject(font.into()))
}

/// Single-line text size in pixels.
fn measure(dc: HDC, font: &GdiObject, text: &[u16]) -> (i32, i32) {
    let _font = Selection::new(dc, font);
    let mut buf = text.to_vec();
    let mut rect = RECT::default();
    // SAFETY: `buf` and `rect` outlive the call; DT_CALCRECT only writes `rect`.
    unsafe {
        DrawTextW(
            dc,
            &mut buf,
            &mut rect,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        )
    };
    (rect.right - rect.left, rect.bottom - rect.top)
}

fn draw_text(
    dc: HDC,
    font: &GdiObject,
    text: &[u16],
    rect: PhysicalRect,
    align: DRAW_TEXT_FORMAT,
    rgb: u32,
) {
    let _font = Selection::new(dc, font);
    let mut buf = text.to_vec();
    let mut rect = to_win32(rect);
    // SAFETY: `dc` has the overlay DIB selected; `buf` and `rect` outlive the calls.
    unsafe {
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(paint::colorref(rgb)));
        DrawTextW(
            dc,
            &mut buf,
            &mut rect,
            align | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
    }
}

/// A 32-bpp top-down DIB section and a pointer to its `width * height` pixels, which stay
/// valid until the returned object is deleted.
fn dib_section(dc: HDC, width: i32, height: i32) -> WinwrightResult<(GdiObject, *mut u32)> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: `info` describes a 32-bpp top-down bitmap; `bits` receives the pixel pointer.
    let bitmap = unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) }
        .map_err(|e| platform("CreateDIBSection", &e))?;
    let bitmap = GdiObject(bitmap.into());
    if bits.is_null() {
        return Err(last_error("CreateDIBSection"));
    }
    Ok((bitmap, bits.cast()))
}

struct Text<'a> {
    rect: PhysicalRect,
    text: &'a [u16],
    font: &'a GdiObject,
    align: DRAW_TEXT_FORMAT,
}

/// Renders `request` into a new, shown overlay window. `None` when no part of it lies on a
/// monitor (for example the rect of a minimized window).
pub fn create_overlay(
    hinstance: HINSTANCE,
    request: &OverlayRequest,
) -> WinwrightResult<Option<HWND>> {
    let geometry = monitor_geometry(request.rect)?;
    let metrics = Metrics::for_dpi(geometry.dpi);
    let dc = MemoryDc::new()?;
    let label_font = font(metrics.label_font, FW_SEMIBOLD)?;
    let badge_font = font(metrics.badge_font, FW_BOLD)?;
    let label = request
        .label
        .as_deref()
        .map(label_text)
        .filter(|text| !text.is_empty());
    let badge = request
        .step
        .map(|n| n.to_string().encode_utf16().collect::<Vec<u16>>());
    let input = LayoutInput {
        target: request.rect,
        style: request.style,
        monitor: geometry.monitor,
        work: geometry.work,
        metrics,
        label_text: label.as_deref().map(|t| measure(dc.0, &label_font, t)),
        badge_text: badge.as_deref().map(|t| measure(dc.0, &badge_font, t)),
    };
    let Some(layout) = layout::compute_layout(&input) else {
        return Ok(None);
    };

    let (width, height) = (layout.window.width(), layout.window.height());
    let pixels = width as usize * height as usize;
    let (bitmap, bits) = dib_section(dc.0, width, height)?;
    let _bitmap = Selection::new(dc.0, &bitmap);

    let mut texts = Vec::new();
    if let (Some(rect), Some(text)) = (layout.label, label.as_deref()) {
        texts.push(Text {
            rect: PhysicalRect::new(
                rect.left + metrics.label_pad_x,
                rect.top + metrics.label_pad_y,
                rect.right - metrics.label_pad_x,
                rect.bottom - metrics.label_pad_y,
            ),
            text,
            font: &label_font,
            align: DT_LEFT,
        });
    }
    if let (Some(rect), Some(text)) = (layout.badge, badge.as_deref()) {
        texts.push(Text {
            rect,
            text,
            font: &badge_font,
            align: DT_CENTER,
        });
    }

    let saved: Vec<Vec<u8>> = {
        // SAFETY: `bits` points at the DIB's `width * height` 32-bit pixels, alive until
        // `bitmap` drops; GDI does not write them while this slice exists.
        let px = unsafe { std::slice::from_raw_parts_mut(bits, pixels) };
        let mut canvas = Canvas::new(width, height, px);
        canvas.clear();
        paint::draw(&mut canvas, &layout, &metrics, request.color);
        texts
            .iter()
            .map(|t| canvas.alpha_snapshot(t.rect))
            .collect()
    };
    let text_rgb = paint::text_color_on(request.color);
    for t in &texts {
        draw_text(dc.0, t.font, t.text, t.rect, t.align, text_rgb);
    }
    // SAFETY: no arguments; completes batched GDI drawing before the pixels are read.
    let _ = unsafe { GdiFlush() };
    {
        // SAFETY: as above; GDI drawing into the DIB has been flushed.
        let px = unsafe { std::slice::from_raw_parts_mut(bits, pixels) };
        let mut canvas = Canvas::new(width, height, px);
        for (t, alpha) in texts.iter().zip(&saved) {
            canvas.restore_alpha(t.rect, alpha);
        }
    }

    // SAFETY: the class is registered by the UI thread setup; no creation parameters.
    let hwnd = unsafe {
        CreateWindowExW(
            OVERLAY_EX_STYLE,
            OVERLAY_CLASS,
            w!("Winwright overlay"),
            WS_POPUP,
            layout.window.left,
            layout.window.top,
            width,
            height,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| platform("CreateWindowExW(overlay)", &e))?;
    if let Err(err) = present(hwnd, dc.0, layout.window) {
        // SAFETY: we just created `hwnd` on this thread and nothing else references it.
        let _ = unsafe { DestroyWindow(hwnd) };
        return Err(err);
    }
    Ok(Some(hwnd))
}

/// Pushes the premultiplied DIB to the layered window, then shows it topmost without
/// activating it (so focus never moves).
fn present(hwnd: HWND, dc: HDC, bounds: PhysicalRect) -> WinwrightResult<()> {
    let position = POINT {
        x: bounds.left,
        y: bounds.top,
    };
    let size = SIZE {
        cx: bounds.width(),
        cy: bounds.height(),
    };
    let origin = POINT::default();
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    // SAFETY: every pointer refers to a local that outlives the call; `dc` holds the DIB.
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            Some(&raw const position),
            Some(&raw const size),
            Some(dc),
            Some(&raw const origin),
            COLORREF(0),
            Some(&raw const blend),
            ULW_ALPHA,
        )
    }
    .map_err(|e| platform("UpdateLayeredWindow", &e))?;
    // SAFETY: `hwnd` is our live window; SW_SHOWNOACTIVATE and SWP_NOACTIVATE never activate.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
    .map_err(|e| platform("SetWindowPos", &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_text_is_one_trimmed_line() {
        let text = String::from_utf16(&label_text("  Click\nthis\tbutton \u{7} ")).unwrap();
        assert_eq!(text, "Click this button");
        assert!(label_text(" \n ").is_empty());
        assert_eq!(label_text("Größe ✓").len(), 7);
    }
}
