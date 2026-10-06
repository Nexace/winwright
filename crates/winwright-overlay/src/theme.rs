//! One look for every native Winwright window: the tray menu, the confirmation dialog, and
//! the Inspector. The palette follows the Windows light/dark app setting (and falls back to
//! system colors in high-contrast mode); shapes are anti-aliased by the software rasterizer
//! in `paint`, text is GDI ClearType, icons are Segoe Fluent glyphs. No WebView, no assets.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU8, Ordering};

use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, RECT,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, AlphaBlend, BI_RGB, BITMAPINFO,
    BITMAPINFOHEADER, BLENDFUNCTION, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_GRAYTEXT,
    COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_HOTLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT,
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DC_BRUSH, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FONT_QUALITY, FillRect, GdiFlush, GetDC,
    GetStockObject, GetSysColor, GetTextFaceW, HBITMAP, HBRUSH, HDC, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS, ReleaseDC, SelectObject, SetBkMode, SetDCBrushColor, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, HICON, ICONINFO, IDC_ARROW, LoadCursorW, RegisterClassExW,
    SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW, WNDCLASSEXW,
    WNDPROC,
};
use windows::core::{BOOL, HRESULT, HSTRING, PCWSTR, w};
use winwright_contracts::config::ThemeChoice;
use winwright_contracts::geometry::PhysicalRect;
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::paint::{self, Canvas, premultiply};
use crate::platform;

/// Winwright teal blue.
pub const BRAND: u32 = 0x0008_91B2;
const WHITE: u32 = 0x00FF_FFFF;
const STOP_RED: u32 = 0x00D1_3438;

/// Segoe Fluent Icons code points (same in Segoe MDL2 Assets on Windows 10).
pub mod glyph {
    pub const REFRESH: char = '\u{E72C}';
    pub const SEARCH: char = '\u{E721}';
    pub const COPY: char = '\u{E8C8}';
    pub const VIEW: char = '\u{E890}';
    pub const POINTER: char = '\u{E7C9}';
    pub const STOP: char = '\u{E71A}';
    pub const PLAY: char = '\u{E768}';
    pub const HISTORY: char = '\u{E81C}';
    pub const SHIELD: char = '\u{EA18}';
    pub const WARNING: char = '\u{E7BA}';
    pub const INFO: char = '\u{E946}';
    pub const CHEVRON_RIGHT: char = '\u{E76C}';
    pub const CHEVRON_DOWN: char = '\u{E70D}';
    pub const WINDOW: char = '\u{E737}';
    pub const LOCK: char = '\u{E72E}';
    pub const SETTINGS: char = '\u{E713}';
    pub const ADD: char = '\u{E710}';
    pub const REMOVE: char = '\u{E738}';
    pub const DIAGNOSTIC: char = '\u{E9D9}';
    pub const DOWNLOAD: char = '\u{E896}';
}

/// Colors as `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub dark: bool,
    pub high_contrast: bool,
    /// Window background (also the title bar).
    pub window: u32,
    /// Cards, inputs, secondary buttons.
    pub surface: u32,
    pub hover: u32,
    pub pressed: u32,
    pub border: u32,
    pub text: u32,
    pub muted: u32,
    pub faint: u32,
    pub accent: u32,
    pub accent_hover: u32,
    pub accent_pressed: u32,
    pub on_accent: u32,
    /// Selected rows.
    pub selection: u32,
    /// Control-type names in trees.
    pub role: u32,
    pub good: u32,
    pub warn: u32,
    pub bad: u32,
    /// Code blocks.
    pub code: u32,
}

impl Palette {
    pub const LIGHT: Self = Self {
        dark: false,
        high_contrast: false,
        window: 0x00F3_F2F0,
        surface: 0x00FF_FFFF,
        hover: 0x00F0_EEEB,
        pressed: 0x00E7_E4E0,
        border: 0x00E0_DCD7,
        text: 0x001C_1B1A,
        muted: 0x0062_5E59,
        faint: 0x009C_9791,
        // Darker than the mark so white text on buttons stays readable.
        accent: 0x000E_7490,
        accent_hover: 0x0015_5E75,
        accent_pressed: 0x0016_4E63,
        on_accent: WHITE,
        selection: 0x00D5_F3F8,
        role: 0x0022_59B8,
        good: 0x001E_8A4C,
        warn: 0x00A8_5F00,
        bad: 0x00C4_2B1C,
        code: 0x00F7_F6F4,
    };

    pub const DARK: Self = Self {
        dark: true,
        high_contrast: false,
        window: 0x001F_1E1D,
        surface: 0x002A_2928,
        hover: 0x0035_3331,
        pressed: 0x003E_3C3A,
        border: 0x003D_3B39,
        text: 0x00F3_F1EE,
        muted: 0x00AB_A69F,
        faint: 0x0078_736D,
        accent: BRAND,
        accent_hover: 0x0006_A3C6,
        accent_pressed: 0x000E_7490,
        on_accent: WHITE,
        selection: 0x0011_3C47,
        role: 0x008A_B4F8,
        good: 0x005C_CB8A,
        warn: 0x00F0_B849,
        bad: 0x00FF_7B72,
        code: 0x0023_2221,
    };

    /// High contrast first, then `WINWRIGHT_THEME=light|dark`, then the theme chosen in
    /// Settings (`overlay.theme`), then the Windows app mode.
    pub fn system() -> Self {
        if let Some(p) = Self::high_contrast() {
            return p;
        }
        match std::env::var("WINWRIGHT_THEME").as_deref() {
            Ok("dark") => Self::DARK,
            Ok("light") => Self::LIGHT,
            _ => match choice() {
                ThemeChoice::Dark => Self::DARK,
                ThemeChoice::Light => Self::LIGHT,
                ThemeChoice::System if apps_use_dark_theme() => Self::DARK,
                ThemeChoice::System => Self::LIGHT,
            },
        }
    }

    fn high_contrast() -> Option<Self> {
        let mut hc = HIGHCONTRASTW {
            cbSize: size_of::<HIGHCONTRASTW>() as u32,
            ..Default::default()
        };
        // SAFETY: `hc` is a HIGHCONTRASTW out-parameter with `cbSize` set.
        unsafe {
            SystemParametersInfoW(
                SPI_GETHIGHCONTRAST,
                hc.cbSize,
                Some(&mut hc as *mut HIGHCONTRASTW as *mut c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .ok()?;
        if hc.dwFlags.0 & HCF_HIGHCONTRASTON.0 == 0 {
            return None;
        }
        // SAFETY: plain reads of system colors.
        let sys = |i| rgb_of(unsafe { GetSysColor(i) });
        let (window, text) = (sys(COLOR_WINDOW), sys(COLOR_WINDOWTEXT));
        let (highlight, on_highlight) = (sys(COLOR_HIGHLIGHT), sys(COLOR_HIGHLIGHTTEXT));
        Some(Self {
            dark: luma(window) < 128,
            high_contrast: true,
            window,
            surface: window,
            hover: window,
            pressed: window,
            border: text,
            text,
            muted: text,
            faint: sys(COLOR_GRAYTEXT),
            accent: highlight,
            accent_hover: highlight,
            accent_pressed: highlight,
            on_accent: on_highlight,
            selection: highlight,
            role: sys(COLOR_HOTLIGHT),
            good: text,
            warn: text,
            bad: text,
            code: window,
        })
    }

    /// Text color on a selected row.
    pub fn on_selection(&self) -> u32 {
        if self.high_contrast {
            self.on_accent
        } else {
            self.text
        }
    }
}

/// The configured theme, as `ThemeChoice as u8`.
static CHOICE: AtomicU8 = AtomicU8::new(ThemeChoice::System as u8);

/// Sets the theme (from config) for every window opened from now on.
pub fn set_choice(theme: ThemeChoice) {
    CHOICE.store(theme as u8, Ordering::Relaxed);
}

fn choice() -> ThemeChoice {
    match CHOICE.load(Ordering::Relaxed) {
        c if c == ThemeChoice::Light as u8 => ThemeChoice::Light,
        c if c == ThemeChoice::Dark as u8 => ThemeChoice::Dark,
        _ => ThemeChoice::System,
    }
}

fn apps_use_dark_theme() -> bool {
    let mut value = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: `value`/`size` describe a DWORD buffer that outlives the call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    status.is_ok() && value == 0
}

/// `0x00BBGGRR` -> `0xRRGGBB`.
fn rgb_of(colorref: u32) -> u32 {
    paint::colorref(colorref)
}

fn luma(rgb: u32) -> u32 {
    let ch = |shift: u32| (rgb >> shift) & 0xFF;
    (299 * ch(16) + 587 * ch(8) + 114 * ch(0)) / 1000
}

pub fn cr(rgb: u32) -> COLORREF {
    COLORREF(paint::colorref(rgb))
}

/// `a` mixed toward `b` by `t` (0.0..=1.0).
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((x + (y - x) * t).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// Device pixels for `v` device-independent pixels at `dpi`.
pub fn scale(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48).div_euclid(96)
}

// ---------------------------------------------------------------------------------------
// Fonts

/// A GDI font deleted on drop.
pub struct Font(HFONT);

impl Font {
    pub fn handle(&self) -> HFONT {
        self.0
    }

    fn create(face: &str, px: i32, weight: u32, quality: FONT_QUALITY) -> Self {
        let face = HSTRING::from(face);
        // SAFETY: plain values and a NUL-terminated face name that outlives the call.
        Self(unsafe {
            CreateFontW(
                -px,
                0,
                0,
                0,
                weight as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                quality,
                0,
                &face,
            )
        })
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we created this font; callers never keep it selected past a paint.
            let _ = unsafe { DeleteObject(HGDIOBJ(self.0.0)) };
        }
    }
}

/// The first installed face of `candidates` (GDI silently substitutes missing faces).
fn resolve_face(candidates: &[&str]) -> String {
    for face in candidates {
        let font = Font::create(face, 12, 400, CLEARTYPE_QUALITY);
        let mut buf = [0u16; 64];
        // SAFETY: screen DC borrowed and released here; the font is selected out before drop.
        let len = unsafe {
            let dc = GetDC(None);
            let old = SelectObject(dc, HGDIOBJ(font.0.0));
            let len = GetTextFaceW(dc, Some(&mut buf));
            SelectObject(dc, old);
            ReleaseDC(None, dc);
            len
        };
        let got = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
        if got.trim_end_matches('\0').eq_ignore_ascii_case(face) {
            return (*face).to_owned();
        }
    }
    candidates.last().copied().unwrap_or("Segoe UI").to_owned()
}

/// Every font the native windows use, sized for one DPI.
pub struct Fonts {
    pub dpi: u32,
    pub body: Font,
    pub strong: Font,
    pub small: Font,
    pub small_strong: Font,
    pub title: Font,
    pub mono: Font,
    pub icons: Font,
    pub icons_small: Font,
    pub icons_large: Font,
}

impl Fonts {
    pub fn new(dpi: u32) -> Self {
        let text = resolve_face(&["Segoe UI Variable Text", "Segoe UI"]);
        let display = resolve_face(&["Segoe UI Variable Display", "Segoe UI"]);
        let mono = resolve_face(&["Cascadia Mono", "Consolas"]);
        let icons = icon_face();
        let px = |v| scale(v, dpi);
        Self {
            dpi,
            body: Font::create(&text, px(14), 400, CLEARTYPE_QUALITY),
            strong: Font::create(&text, px(14), 600, CLEARTYPE_QUALITY),
            small: Font::create(&text, px(12), 400, CLEARTYPE_QUALITY),
            small_strong: Font::create(&text, px(12), 600, CLEARTYPE_QUALITY),
            title: Font::create(&display, px(20), 600, CLEARTYPE_QUALITY),
            mono: Font::create(&mono, px(13), 400, CLEARTYPE_QUALITY),
            icons: Font::create(&icons, px(16), 400, CLEARTYPE_QUALITY),
            icons_small: Font::create(&icons, px(10), 400, CLEARTYPE_QUALITY),
            icons_large: Font::create(&icons, px(28), 400, CLEARTYPE_QUALITY),
        }
    }

    pub fn px(&self, v: i32) -> i32 {
        scale(v, self.dpi)
    }
}

fn icon_face() -> String {
    resolve_face(&["Segoe Fluent Icons", "Segoe MDL2 Assets"])
}

// ---------------------------------------------------------------------------------------
// Pixels

fn round_box_distance(x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    let (hx, hy) = (w / 2.0, h / 2.0);
    let r = radius.clamp(0.0, hx.min(hy));
    let qx = (x - hx).abs() - (hx - r);
    let qy = (y - hy).abs() - (hy - r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    };
    (p.0 - (a.0 + t * dx)).hypot(p.1 - (a.1 + t * dy))
}

/// Coverage of a pixel whose center is `distance` outside a shape edge (negative = inside).
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

fn alpha(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn scale_pixel(p: u32, keep: f32) -> u32 {
    let k = keep.clamp(0.0, 1.0);
    let ch = |shift: u32| ((((p >> shift) & 0xFF) as f32 * k).round() as u32) << shift;
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// The Winwright mark as `size * size` premultiplied BGRA pixels: a rounded square with a
/// vertical gradient and a white "W". Stopped: warm gray, with a red stop badge.
pub fn brand_pixels(size: i32, stopped: bool) -> Vec<u32> {
    let s = size.max(1) as f32;
    let (top, bottom) = if stopped {
        (0x009E_9993, 0x0073_6E68)
    } else {
        (mix(BRAND, WHITE, 0.16), mix(BRAND, 0x0000_0000, 0.14))
    };
    let radius = (s * 0.25).max(2.0);
    let w: [(f32, f32); 5] = [
        (0.19, 0.29),
        (0.345, 0.72),
        (0.5, 0.43),
        (0.655, 0.72),
        (0.81, 0.29),
    ]
    .map(|(x, y)| (x * s, y * s));
    let half_stroke = (s * 0.07).max(0.95);
    let mut px = vec![0u32; (size.max(1) * size.max(1)) as usize];
    for y in 0..size {
        for x in 0..size {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let body = coverage(round_box_distance(p.0, p.1, s, s, radius));
            if body == 0.0 {
                continue;
            }
            let fill = mix(top, bottom, p.1 / s);
            let stroke = w
                .windows(2)
                .map(|seg| segment_distance(p, seg[0], seg[1]))
                .fold(f32::MAX, f32::min);
            let letter = coverage(stroke - half_stroke) * body;
            px[(y * size + x) as usize] = paint::over(
                premultiply(WHITE, alpha(letter)),
                premultiply(fill, alpha(body)),
            );
        }
    }
    if stopped {
        let (cx, cy, r) = (s * 0.76, s * 0.76, (s * 0.25).max(3.0));
        let gap = (s * 0.07).max(1.0);
        let side = r * 0.9;
        for y in 0..size {
            for x in 0..size {
                let p = (x as f32 + 0.5, y as f32 + 0.5);
                let d = (p.0 - cx).hypot(p.1 - cy);
                let i = (y * size + x) as usize;
                // A transparent ring keeps the badge readable on any taskbar color.
                px[i] = scale_pixel(px[i], 1.0 - coverage(d - (r + gap)));
                px[i] = paint::over(premultiply(STOP_RED, alpha(coverage(d - r))), px[i]);
                let square = round_box_distance(
                    p.0 - (cx - side / 2.0),
                    p.1 - (cy - side / 2.0),
                    side,
                    side,
                    side * 0.2,
                );
                px[i] = paint::over(premultiply(WHITE, alpha(coverage(square))), px[i]);
            }
        }
    }
    px
}

fn unpremultiply(p: u32) -> u32 {
    let a = p >> 24;
    if a == 0 {
        return 0;
    }
    let ch = |shift: u32| ((((p >> shift) & 0xFF) * 255 + a / 2) / a).min(255) << shift;
    (a << 24) | ch(16) | ch(8) | ch(0)
}

/// A top-down 32-bpp DIB section holding `px` (premultiplied). The caller owns it.
pub fn bitmap_from_pixels(width: i32, height: i32, px: &[u32]) -> WinwrightResult<HBITMAP> {
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
    // SAFETY: `info` describes a width*height 32-bpp bitmap; `bits` receives its pixels,
    // which are exactly `px.len()` u32s (checked below before copying).
    unsafe {
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| platform("CreateDIBSection", &e))?;
        let count = (width.max(0) * height.max(0)) as usize;
        if bits.is_null() || px.len() != count {
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            return Err(WinwrightError::invalid("bitmap size mismatch"));
        }
        std::ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, count);
        Ok(bitmap)
    }
}

/// An icon from premultiplied pixels (icons take straight alpha). The caller owns it.
pub fn icon_from_pixels(size: i32, px: &[u32]) -> WinwrightResult<HICON> {
    let straight: Vec<u32> = px.iter().map(|&p| unpremultiply(p)).collect();
    let color = bitmap_from_pixels(size, size, &straight)?;
    let mask_bits = vec![0u8; (((size + 15) / 16) * 2 * size) as usize];
    // SAFETY: `mask_bits` covers a 1-bpp size*size bitmap with WORD-aligned rows; both
    // bitmaps are deleted after the icon copies them.
    unsafe {
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const c_void));
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        });
        let _ = DeleteObject(HGDIOBJ(mask.0));
        let _ = DeleteObject(HGDIOBJ(color.0));
        icon.map_err(|e| platform("CreateIconIndirect", &e))
    }
}

pub fn brand_icon(size: i32, stopped: bool) -> WinwrightResult<HICON> {
    icon_from_pixels(size, &brand_pixels(size, stopped))
}

/// A glyph rendered to `size * size` premultiplied pixels in `rgb` (menu item bitmaps).
pub fn glyph_pixels(ch: char, size: i32, rgb: u32) -> WinwrightResult<Vec<u32>> {
    let font = Font::create(&icon_face(), size, 400, ANTIALIASED_QUALITY);
    let blank = vec![0u32; (size * size) as usize];
    let bitmap = bitmap_from_pixels(size, size, &blank)?;
    let mut text: Vec<u16> = ch.to_string().encode_utf16().collect();
    let mut out = vec![0u32; blank.len()];
    // SAFETY: a private memory DC draws into our DIB; every selection is restored and every
    // object deleted before returning. The pixels are read back after GdiFlush.
    unsafe {
        let dc = CreateCompatibleDC(None);
        let old_bitmap = SelectObject(dc, HGDIOBJ(bitmap.0));
        let old_font = SelectObject(dc, HGDIOBJ(font.0.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, cr(WHITE));
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        DrawTextW(
            dc,
            &mut text,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let _ = GdiFlush();
        let mut dib = windows::Win32::Graphics::Gdi::DIBSECTION::default();
        let got = windows::Win32::Graphics::Gdi::GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<windows::Win32::Graphics::Gdi::DIBSECTION>() as i32,
            Some(&mut dib as *mut _ as *mut c_void),
        );
        if got > 0 && !dib.dsBm.bmBits.is_null() {
            let src = std::slice::from_raw_parts(dib.dsBm.bmBits as *const u32, out.len());
            for (o, &p) in out.iter_mut().zip(src) {
                // White on black: any channel is the glyph's coverage.
                *o = premultiply(rgb, ((p >> 8) & 0xFF) as u8);
            }
        }
        SelectObject(dc, old_font);
        SelectObject(dc, old_bitmap);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Drawing on a DC

/// Draws premultiplied pixels with per-pixel alpha.
pub fn blit(hdc: HDC, x: i32, y: i32, width: i32, height: i32, px: &[u32]) {
    if width <= 0 || height <= 0 {
        return;
    }
    let Ok(bitmap) = bitmap_from_pixels(width, height, px) else {
        return;
    };
    // SAFETY: a private memory DC holds our bitmap for one AlphaBlend; both are released.
    unsafe {
        let mem = CreateCompatibleDC(Some(hdc));
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        let _ = AlphaBlend(
            hdc,
            x,
            y,
            width,
            height,
            mem,
            0,
            0,
            width,
            height,
            BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            },
        );
        SelectObject(mem, old);
        let _ = DeleteDC(mem);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
    }
}

pub fn fill(hdc: HDC, r: RECT, rgb: u32) {
    // SAFETY: the stock DC brush takes the color just set; nothing is created.
    unsafe {
        SetDCBrushColor(hdc, cr(rgb));
        FillRect(hdc, &r, HBRUSH(GetStockObject(DC_BRUSH).0));
    }
}

fn size_of_rect(r: &RECT) -> (i32, i32) {
    (r.right - r.left, r.bottom - r.top)
}

/// An anti-aliased rounded rectangle, optionally with a 1 px border.
pub fn rounded(hdc: HDC, r: RECT, radius: f32, fill_rgb: u32, border: Option<u32>) {
    let (w, h) = size_of_rect(&r);
    if w <= 0 || h <= 0 {
        return;
    }
    let mut px = vec![0u32; (w * h) as usize];
    {
        let mut canvas = Canvas::new(w, h, &mut px);
        let outer = PhysicalRect::new(0, 0, w, h);
        match border {
            Some(b) => {
                canvas.fill_rounded_rect(outer, radius, b, 255);
                canvas.fill_rounded_rect(
                    PhysicalRect::new(1, 1, w - 1, h - 1),
                    (radius - 1.0).max(0.0),
                    fill_rgb,
                    255,
                );
            }
            None => canvas.fill_rounded_rect(outer, radius, fill_rgb, 255),
        }
    }
    blit(hdc, r.left, r.top, w, h, &px);
}

/// An anti-aliased rounded outline `thickness` px wide, inside `r`.
pub fn ring(hdc: HDC, r: RECT, radius: f32, thickness: f32, rgb: u32) {
    let (w, h) = size_of_rect(&r);
    if w <= 0 || h <= 0 {
        return;
    }
    let (fw, fh) = (w as f32, h as f32);
    let mut px = vec![0u32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = round_box_distance(cx, cy, fw, fh, radius);
            let c = coverage(d) * (1.0 - coverage(d + thickness));
            px[(y * w + x) as usize] = premultiply(rgb, alpha(c));
        }
    }
    blit(hdc, r.left, r.top, w, h, &px);
}

/// An anti-aliased filled circle centered in `r`.
pub fn dot(hdc: HDC, r: RECT, rgb: u32) {
    let (w, h) = size_of_rect(&r);
    if w <= 0 || h <= 0 {
        return;
    }
    let mut px = vec![0u32; (w * h) as usize];
    Canvas::new(w, h, &mut px).fill_circle(
        w as f32 / 2.0,
        h as f32 / 2.0,
        w.min(h) as f32 / 2.0,
        rgb,
        255,
    );
    blit(hdc, r.left, r.top, w, h, &px);
}

/// Draws `s` and returns the height used. `flags` adds to `DT_NOPREFIX`.
pub fn text(hdc: HDC, font: &Font, s: &str, r: RECT, rgb: u32, flags: DRAW_TEXT_FORMAT) -> i32 {
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    if buf.is_empty() {
        return 0;
    }
    let mut rect = r;
    // SAFETY: `buf` and `rect` outlive the call; the previous font is restored.
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(font.0.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, cr(rgb));
        let h = DrawTextW(hdc, &mut buf, &mut rect, flags | DT_NOPREFIX);
        SelectObject(hdc, old);
        h
    }
}

/// Size of `s` in `font`; wraps within `width` when given (`flags` should then include
/// `DT_WORDBREAK`).
pub fn measure(
    hdc: HDC,
    font: &Font,
    s: &str,
    width: Option<i32>,
    flags: DRAW_TEXT_FORMAT,
) -> (i32, i32) {
    let mut buf: Vec<u16> = s.encode_utf16().collect();
    if buf.is_empty() {
        return (0, 0);
    }
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width.unwrap_or(0),
        bottom: 0,
    };
    let flags = if width.is_none() {
        flags | DT_SINGLELINE
    } else {
        flags
    };
    // SAFETY: as in `text`; DT_CALCRECT only writes `rect`.
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(font.0.0));
        DrawTextW(hdc, &mut buf, &mut rect, flags | DT_CALCRECT | DT_NOPREFIX);
        SelectObject(hdc, old);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

/// A glyph centered in `r`.
pub fn icon(hdc: HDC, font: &Font, ch: char, r: RECT, rgb: u32) {
    text(
        hdc,
        font,
        &ch.to_string(),
        r,
        rgb,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// Filled with the accent color.
    Primary,
    /// Surface with a border.
    Secondary,
    /// No chrome until hovered.
    Subtle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonState {
    pub hot: bool,
    pub pressed: bool,
    pub focused: bool,
    pub disabled: bool,
}

/// Paints a whole button into `r` (custom-drawn push buttons). The body is inset by the
/// 2 px focus ring, which is drawn only when `state.focused`.
#[allow(clippy::too_many_arguments)]
pub fn draw_button(
    hdc: HDC,
    r: RECT,
    backdrop: u32,
    kind: ButtonKind,
    state: ButtonState,
    label: &str,
    glyph_char: Option<char>,
    p: &Palette,
    f: &Fonts,
) {
    fill(hdc, r, backdrop);
    let ring_w = f.px(2);
    let body = RECT {
        left: r.left + ring_w,
        top: r.top + ring_w,
        right: r.right - ring_w,
        bottom: r.bottom - ring_w,
    };
    let radius = f.px(4) as f32;
    let (bg, border, fg) = match kind {
        ButtonKind::Primary => {
            let bg = if state.disabled {
                mix(p.accent, backdrop, 0.55)
            } else if state.pressed {
                p.accent_pressed
            } else if state.hot {
                p.accent_hover
            } else {
                p.accent
            };
            (bg, None, p.on_accent)
        }
        ButtonKind::Secondary => {
            let bg = if state.disabled {
                p.surface
            } else if state.pressed {
                p.pressed
            } else if state.hot {
                p.hover
            } else {
                p.surface
            };
            let fg = if state.disabled { p.faint } else { p.text };
            (bg, Some(p.border), fg)
        }
        ButtonKind::Subtle => {
            let bg = if state.disabled {
                backdrop
            } else if state.pressed {
                p.pressed
            } else if state.hot {
                p.hover
            } else {
                backdrop
            };
            let fg = if state.disabled { p.faint } else { p.text };
            (bg, None, fg)
        }
    };
    if bg != backdrop || border.is_some() {
        rounded(hdc, body, radius, bg, border);
    }
    if state.focused {
        ring(hdc, r, radius + ring_w as f32, ring_w as f32, p.accent);
    }
    let font = if kind == ButtonKind::Primary {
        &f.strong
    } else {
        &f.body
    };
    let label_w = measure(hdc, font, label, None, DRAW_TEXT_FORMAT(0)).0;
    let glyph_w = if glyph_char.is_some() { f.px(16) } else { 0 };
    let gap = if glyph_char.is_some() && label_w > 0 {
        f.px(8)
    } else {
        0
    };
    let total = glyph_w + gap + label_w;
    let mut x = body.left + ((body.right - body.left) - total) / 2;
    if let Some(ch) = glyph_char {
        icon(
            hdc,
            &f.icons,
            ch,
            RECT {
                left: x,
                top: body.top,
                right: x + glyph_w,
                bottom: body.bottom,
            },
            fg,
        );
        x += glyph_w + gap;
    }
    if label_w > 0 {
        text(
            hdc,
            font,
            label,
            RECT {
                left: x,
                top: body.top,
                right: body.right,
                bottom: body.bottom,
            },
            fg,
            DT_SINGLELINE | DT_VCENTER,
        );
    }
}

/// Width a button needs for `label` (+ glyph), including padding and the focus ring.
pub fn button_width(hdc: HDC, f: &Fonts, label: &str, has_glyph: bool, primary: bool) -> i32 {
    let font = if primary { &f.strong } else { &f.body };
    let label_w = measure(hdc, font, label, None, DRAW_TEXT_FORMAT(0)).0;
    let glyph = if has_glyph { f.px(16) } else { 0 };
    let gap = if has_glyph && label_w > 0 { f.px(8) } else { 0 };
    let pad = if label_w > 0 { f.px(14) } else { f.px(8) };
    glyph + gap + label_w + pad * 2 + f.px(2) * 2
}

// ---------------------------------------------------------------------------------------
// Windows

/// Dark/light title bar colored like the window, border, and rounded corners (Windows 11;
/// earlier versions ignore the attributes they do not know).
pub fn style_window(hwnd: HWND, p: &Palette) {
    let set = |attr, value: u32| {
        // SAFETY: `value` is a 4-byte attribute value that outlives the call.
        let _ =
            unsafe { DwmSetWindowAttribute(hwnd, attr, &value as *const u32 as *const c_void, 4) };
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, BOOL::from(p.dark).0 as u32);
    set(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND.0 as u32);
    if !p.high_contrast {
        set(DWMWA_CAPTION_COLOR, cr(p.window).0);
        set(DWMWA_TEXT_COLOR, cr(p.text).0);
        set(DWMWA_BORDER_COLOR, cr(p.border).0);
    }
}

/// Registers a window class with the arrow cursor and no background brush (the window
/// paints everything). Registering twice is fine.
pub fn register_class(name: PCWSTR, proc: WNDPROC) -> WinwrightResult<HINSTANCE> {
    // SAFETY: plain module/cursor lookups and a fully initialized class description.
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None)
            .map_err(|e| platform("GetModuleHandleW", &e))?
            .into();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc,
            hInstance: hinstance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: name,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            let error = GetLastError();
            if error != ERROR_CLASS_ALREADY_EXISTS {
                return Err(WinwrightError::Platform {
                    operation: "RegisterClassExW".into(),
                    hresult: HRESULT::from_win32(error.0).0,
                });
            }
        }
        Ok(hinstance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(p: u32) -> u32 {
        p >> 24
    }

    #[test]
    fn brand_mark_is_valid_premultiplied_and_shaped() {
        for size in [16, 20, 24, 32, 48] {
            for stopped in [false, true] {
                let px = brand_pixels(size, stopped);
                assert_eq!(px.len(), (size * size) as usize);
                for &p in &px {
                    for shift in [0, 8, 16] {
                        assert!((p >> shift) & 0xFF <= a(p), "not premultiplied: {p:08x}");
                    }
                }
                assert_eq!(a(px[0]), 0, "rounded corner is transparent");
                let mid = (size / 2 * size + 2) as usize;
                assert_eq!(a(px[mid]), 255, "body is opaque");
            }
        }
        // The "W" is white: some opaque pixels are near-white.
        let px = brand_pixels(32, false);
        assert!(
            px.iter()
                .any(|&p| a(p) == 255 && p & 0xFF > 0xF0 && (p >> 16) & 0xFF > 0xF0)
        );
        // The stopped badge is red in the corner.
        let px = brand_pixels(32, true);
        let corner = px[(29 * 32 + 22) as usize];
        assert!(
            (corner >> 16) & 0xFF > 0xB0 && corner & 0xFF < 0x80,
            "{corner:08x}"
        );
    }

    #[test]
    fn unpremultiply_round_trips() {
        for rgb in [0x00E0_4A2A, 0x0012_3456, WHITE] {
            for alpha in [0u8, 1, 64, 128, 255] {
                let p = premultiply(rgb, alpha);
                let s = unpremultiply(p);
                assert_eq!(s >> 24, u32::from(alpha));
                if alpha == 255 {
                    assert_eq!(s & 0x00FF_FFFF, rgb);
                }
            }
        }
    }

    #[test]
    fn palettes_have_readable_text() {
        for p in [Palette::LIGHT, Palette::DARK] {
            let contrast = |a: u32, b: u32| luma(a).abs_diff(luma(b));
            assert!(contrast(p.text, p.window) > 150);
            assert!(contrast(p.text, p.surface) > 150);
            assert!(contrast(p.muted, p.surface) > 70);
        }
        assert_eq!(mix(0x0000_0000, 0x00FF_FFFF, 0.5), 0x0080_8080);
        assert_eq!(scale(16, 96), 16);
        assert_eq!(scale(16, 120), 20);
        assert_eq!(scale(16, 144), 24);
    }
}
