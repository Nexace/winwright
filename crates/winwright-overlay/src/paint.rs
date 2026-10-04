//! Software rasterizer for overlay bitmaps: 32-bit **premultiplied** BGRA pixels in top-down
//! rows, which is what `UpdateLayeredWindow` expects with `AC_SRC_ALPHA`. A pixel read as a
//! little-endian `u32` is `0xAARRGGBB`.

use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};

use crate::layout::{Layout, Metrics, inflate, intersect, offset};

pub const WHITE: u32 = 0x00FF_FFFF;
const DARK_TEXT: u32 = 0x001A_1A1A;
const BLACK: u32 = 0;
/// The pointer style's caption bubble: near-black with a slightly lighter rim.
pub const BUBBLE_FILL: u32 = 0x001F_2023;
const BUBBLE_RIM: u32 = 0x0045_474D;
/// Alpha of each of the stacked layers that make a soft shadow.
const SHADOW_LAYER_ALPHA: u8 = 9;
const POINTER_SHADOW_ALPHA: u8 = 70;
/// Alpha of the faint fill inside a highlight.
pub const FILL_ALPHA: u8 = 24;
const MARKER_RING_ALPHA: u8 = 200;

/// `0xRRGGBB` at straight `alpha` -> premultiplied BGRA pixel.
pub fn premultiply(rgb: u32, alpha: u8) -> u32 {
    let a = u32::from(alpha);
    let ch = |shift: u32| (((rgb >> shift) & 0xFF) * a + 127) / 255;
    (a << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// `0xRRGGBB` -> GDI `COLORREF` bits (`0x00BBGGRR`).
pub fn colorref(rgb: u32) -> u32 {
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

/// Mixes `rgb` toward white by `amount / 255`.
pub fn lighten(rgb: u32, amount: u8) -> u32 {
    let t = u32::from(amount);
    let ch = |shift: u32| {
        let c = (rgb >> shift) & 0xFF;
        c + ((255 - c) * t + 127) / 255
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// White text, or near-black on light backgrounds (Rec. 601 luma) so labels stay legible.
pub fn text_color_on(background: u32) -> u32 {
    let ch = |shift: u32| (background >> shift) & 0xFF;
    let luma = (299 * ch(16) + 587 * ch(8) + 114 * ch(0)) / 1000;
    if luma > 170 { DARK_TEXT } else { WHITE }
}

/// Premultiplied source-over: `src + dst * (1 - src.a)`. Keeps every channel <= alpha.
pub(crate) fn over(src: u32, dst: u32) -> u32 {
    let inv = 255 - (src >> 24);
    let ch = |shift: u32| ((src >> shift) & 0xFF) + (((dst >> shift) & 0xFF) * inv + 127) / 255;
    (ch(24) << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// A mutable view of `width * height` premultiplied pixels. All drawing is clipped.
pub struct Canvas<'a> {
    width: i32,
    height: i32,
    px: &'a mut [u32],
}

impl<'a> Canvas<'a> {
    pub fn new(width: i32, height: i32, px: &'a mut [u32]) -> Self {
        assert_eq!(
            px.len(),
            width.max(0) as usize * height.max(0) as usize,
            "canvas buffer size"
        );
        Self { width, height, px }
    }

    fn bounds(&self) -> PhysicalRect {
        PhysicalRect::new(0, 0, self.width, self.height)
    }

    fn index(&self, x: i32, y: i32) -> usize {
        y as usize * self.width as usize + x as usize
    }

    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        self.px[self.index(x, y)]
    }

    pub fn clear(&mut self) {
        self.px.fill(0);
    }

    fn blend(&mut self, x: i32, y: i32, rgb: u32, alpha: u8) {
        if alpha == 0 {
            return;
        }
        let i = self.index(x, y);
        self.px[i] = over(premultiply(rgb, alpha), self.px[i]);
    }

    pub fn fill_rect(&mut self, r: PhysicalRect, rgb: u32, alpha: u8) {
        let r = intersect(r, self.bounds());
        if r.is_empty() {
            return;
        }
        let src = premultiply(rgb, alpha);
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                let i = self.index(x, y);
                self.px[i] = over(src, self.px[i]);
            }
        }
    }

    /// A `thickness`-wide frame drawn just outside `inner`.
    pub fn frame_outside(&mut self, inner: PhysicalRect, thickness: i32, rgb: u32, alpha: u8) {
        let o = inflate(inner, thickness);
        self.fill_rect(
            PhysicalRect::new(o.left, o.top, o.right, inner.top),
            rgb,
            alpha,
        );
        self.fill_rect(
            PhysicalRect::new(o.left, inner.bottom, o.right, o.bottom),
            rgb,
            alpha,
        );
        self.fill_rect(
            PhysicalRect::new(o.left, inner.top, inner.left, inner.bottom),
            rgb,
            alpha,
        );
        self.fill_rect(
            PhysicalRect::new(inner.right, inner.top, o.right, inner.bottom),
            rgb,
            alpha,
        );
    }

    /// Anti-aliased fill inside `bbox`; `coverage` (0..=1) is sampled at pixel centers.
    fn fill_coverage(
        &mut self,
        bbox: PhysicalRect,
        rgb: u32,
        alpha: u8,
        coverage: impl Fn(f32, f32) -> f32,
    ) {
        let r = intersect(bbox, self.bounds());
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                let c = coverage(x as f32 + 0.5, y as f32 + 0.5).clamp(0.0, 1.0);
                self.blend(x, y, rgb, (c * f32::from(alpha)).round() as u8);
            }
        }
    }

    pub fn fill_rounded_rect(&mut self, r: PhysicalRect, radius: f32, rgb: u32, alpha: u8) {
        let (cx, cy) = (
            (r.left + r.right) as f32 / 2.0,
            (r.top + r.bottom) as f32 / 2.0,
        );
        let (hx, hy) = (r.width() as f32 / 2.0, r.height() as f32 / 2.0);
        let radius = radius.clamp(0.0, hx.min(hy).max(0.0));
        self.fill_coverage(r, rgb, alpha, |x, y| {
            // Signed distance to a rounded box.
            let qx = (x - cx).abs() - (hx - radius);
            let qy = (y - cy).abs() - (hy - radius);
            let outside = qx.max(0.0).hypot(qy.max(0.0));
            let inside = qx.max(qy).min(0.0);
            0.5 - (outside + inside - radius)
        });
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, radius: f32, rgb: u32, alpha: u8) {
        let bbox = PhysicalRect::new(
            (cx - radius).floor() as i32 - 1,
            (cy - radius).floor() as i32 - 1,
            (cx + radius).ceil() as i32 + 1,
            (cy + radius).ceil() as i32 + 1,
        );
        self.fill_coverage(bbox, rgb, alpha, |x, y| {
            0.5 - ((x - cx).hypot(y - cy) - radius)
        });
    }

    /// Triangle through three pixel centers, anti-aliased with 4x4 supersampling.
    pub fn fill_triangle(&mut self, pts: [PhysicalPoint; 3], rgb: u32, alpha: u8) {
        let p = pts.map(|p| (p.x as f32 + 0.5, p.y as f32 + 0.5));
        let edge = |a: (f32, f32), b: (f32, f32), x: f32, y: f32| {
            (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0)
        };
        let area = edge(p[0], p[1], p[2].0, p[2].1);
        if area == 0.0 {
            return;
        }
        let sign = area.signum();
        let bbox = PhysicalRect::new(
            pts.iter().map(|p| p.x).min().unwrap_or(0),
            pts.iter().map(|p| p.y).min().unwrap_or(0),
            pts.iter().map(|p| p.x).max().unwrap_or(0) + 1,
            pts.iter().map(|p| p.y).max().unwrap_or(0) + 1,
        );
        self.fill_coverage(bbox, rgb, alpha, |cx, cy| {
            let mut hits = 0u8;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = cx - 0.5 + (sx as f32 + 0.5) / 4.0;
                    let y = cy - 0.5 + (sy as f32 + 0.5) / 4.0;
                    let inside = [(0, 1), (1, 2), (2, 0)]
                        .iter()
                        .all(|&(i, j)| edge(p[i], p[j], x, y) * sign >= 0.0);
                    hits += u8::from(inside);
                }
            }
            f32::from(hits) / 16.0
        });
    }

    /// Polygon through `pts` (pixel coordinates, even-odd fill), anti-aliased with 4x4
    /// supersampling.
    pub fn fill_polygon(&mut self, pts: &[(f32, f32)], rgb: u32, alpha: u8) {
        if pts.len() < 3 {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let bbox = PhysicalRect::new(
            x0.floor() as i32,
            y0.floor() as i32,
            x1.ceil() as i32 + 1,
            y1.ceil() as i32 + 1,
        );
        let inside = |x: f32, y: f32| {
            let mut odd = false;
            let mut j = pts.len() - 1;
            for i in 0..pts.len() {
                let ((xi, yi), (xj, yj)) = (pts[i], pts[j]);
                if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                    odd = !odd;
                }
                j = i;
            }
            odd
        };
        self.fill_coverage(bbox, rgb, alpha, |cx, cy| {
            let mut hits = 0u8;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = cx - 0.5 + (sx as f32 + 0.5) / 4.0;
                    let y = cy - 0.5 + (sy as f32 + 0.5) / 4.0;
                    hits += u8::from(inside(x, y));
                }
            }
            f32::from(hits) / 16.0
        });
    }

    /// Alpha bytes of `r`, row-major; pair with [`Canvas::restore_alpha`] around GDI text.
    pub fn alpha_snapshot(&self, r: PhysicalRect) -> Vec<u8> {
        let r = intersect(r, self.bounds());
        let mut out = Vec::new();
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                out.push((self.pixel(x, y) >> 24) as u8);
            }
        }
        out
    }

    /// Puts back the alpha saved by [`Canvas::alpha_snapshot`] after GDI drew text into `r`
    /// (GDI zeroes alpha on the pixels it writes) and clamps each color channel to it, so the
    /// pixels stay valid premultiplied values and text never shows outside its opaque box.
    pub fn restore_alpha(&mut self, r: PhysicalRect, saved: &[u8]) {
        let r = intersect(r, self.bounds());
        let mut saved = saved.iter();
        for y in r.top..r.bottom {
            for x in r.left..r.right {
                let Some(&alpha) = saved.next() else { return };
                let a = u32::from(alpha);
                let i = self.index(x, y);
                let p = self.px[i];
                let ch = |shift: u32| ((p >> shift) & 0xFF).min(a);
                self.px[i] = (a << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0);
            }
        }
    }
}

/// Paints every shape of `layout` (text is drawn afterwards by GDI).
pub fn draw(canvas: &mut Canvas<'_>, layout: &Layout, m: &Metrics, color: u32) {
    if let Some(target) = layout.highlight {
        canvas.fill_rect(target, color, FILL_ALPHA);
        canvas.frame_outside(target, m.border, color, 255);
    }
    if let Some(arrow) = &layout.arrow {
        canvas.fill_rect(arrow.shaft, color, 255);
        canvas.fill_triangle(arrow.head, color, 255);
    }
    if let Some(center) = layout.marker {
        let (x, y) = (center.x as f32 + 0.5, center.y as f32 + 0.5);
        let ring = (m.marker_radius + m.marker_ring) as f32;
        canvas.fill_circle(x, y, ring, lighten(color, 140), MARKER_RING_ALPHA);
        canvas.fill_circle(x, y, m.marker_radius as f32, color, 255);
    }
    if let Some(pointer) = &layout.pointer {
        if let Some(bubble) = layout.label {
            // A soft shadow: stacked, growing, faint layers, a little below the bubble.
            let radius = m.bubble_radius as f32;
            for k in (1..=m.shadow).rev() {
                let layer = offset(inflate(bubble, k), 0, m.shadow_dy);
                canvas.fill_rounded_rect(layer, radius + k as f32, BLACK, SHADOW_LAYER_ALPHA);
            }
            canvas.fill_rounded_rect(bubble, radius, BUBBLE_RIM, 255);
            canvas.fill_rounded_rect(inflate(bubble, -1), radius - 1.0, BUBBLE_FILL, 255);
        }
        if let Some(badge) = layout.badge {
            let r = badge.width() as f32 / 2.0;
            let center = (badge.left as f32 + r, badge.top as f32 + r);
            canvas.fill_circle(center.0, center.1, r, color, 255);
        }
        let shape = pointer.shape.map(|p| (p.x as f32 + 0.5, p.y as f32 + 0.5));
        let shifted = |dx: f32, dy: f32| shape.map(|(x, y)| (x + dx, y + dy));
        let edge = m.pointer_edge as f32;
        canvas.fill_polygon(
            &shifted(edge / 2.0, m.shadow_dy as f32),
            BLACK,
            POINTER_SHADOW_ALPHA,
        );
        for k in 0..16 {
            let angle = k as f32 * std::f32::consts::TAU / 16.0;
            canvas.fill_polygon(&shifted(edge * angle.cos(), edge * angle.sin()), WHITE, 255);
        }
        canvas.fill_polygon(&shape, color, 255);
        return;
    }
    if let Some(label) = layout.label {
        canvas.fill_rounded_rect(label, m.label_radius as f32, color, 255);
    }
    if let Some(badge) = layout.badge {
        canvas.fill_rounded_rect(badge, badge.height() as f32 / 2.0, WHITE, 255);
        let inner = inflate(badge, -m.badge_ring);
        canvas.fill_rounded_rect(inner, inner.height() as f32 / 2.0, color, 255);
    }
}

#[cfg(test)]
mod tests {
    use winwright_contracts::overlay::OverlayStyle;

    use super::*;
    use crate::layout::{LayoutInput, compute_layout};

    const RED: u32 = 0x00E0_4A2A;

    fn alpha(p: u32) -> u32 {
        p >> 24
    }

    fn assert_premultiplied(px: &[u32]) {
        for &p in px {
            let a = alpha(p);
            for shift in [0, 8, 16] {
                assert!((p >> shift) & 0xFF <= a, "{p:#010x} is not premultiplied");
            }
        }
    }

    #[test]
    fn premultiplied_bgra_conversion() {
        assert_eq!(premultiply(0x00FF_8000, 255), 0xFFFF_8000);
        assert_eq!(premultiply(0x00FF_8000, 128), 0x8080_4000);
        assert_eq!(premultiply(0x0012_3456, 0), 0);
        assert_eq!(premultiply(WHITE, 24), 0x1818_1818);
        // Blue stays in the low byte (B,G,R,A in memory).
        assert_eq!(premultiply(0x0000_00FF, 255), 0xFF00_00FF);
    }

    #[test]
    fn color_helpers() {
        assert_eq!(colorref(0x0011_2233), 0x0033_2211);
        assert_eq!(lighten(0x0000_0000, 255), WHITE);
        assert_eq!(lighten(RED, 0), RED);
        assert_eq!(lighten(0x0000_0000, 128), 0x0080_8080);
        assert_eq!(text_color_on(RED), WHITE);
        assert_eq!(text_color_on(0x0000_78D4), WHITE);
        assert_eq!(text_color_on(0x00FF_FF00), DARK_TEXT);
    }

    #[test]
    fn source_over_blending() {
        let dst = premultiply(0x0000_00FF, 255);
        assert_eq!(over(premultiply(RED, 255), dst), premultiply(RED, 255));
        assert_eq!(over(0, dst), dst);
        let half = over(premultiply(0x00FF_0000, 128), dst);
        assert_eq!(alpha(half), 255);
        assert_eq!((half >> 16) & 0xFF, 128);
        assert_eq!(half & 0xFF, 127);
    }

    #[test]
    fn rects_and_frames_are_clipped() {
        let mut px = vec![0u32; 10 * 10];
        let mut c = Canvas::new(10, 10, &mut px);
        c.fill_rect(PhysicalRect::new(-5, -5, 3, 3), RED, 255);
        c.fill_rect(PhysicalRect::new(20, 20, 30, 30), RED, 255);
        assert_eq!(c.pixel(0, 0), premultiply(RED, 255));
        assert_eq!(c.pixel(2, 2), premultiply(RED, 255));
        assert_eq!(c.pixel(3, 3), 0);

        c.clear();
        c.frame_outside(PhysicalRect::new(3, 3, 7, 7), 2, RED, 255);
        assert_eq!(c.pixel(1, 1), premultiply(RED, 255));
        assert_eq!(c.pixel(8, 5), premultiply(RED, 255));
        assert_eq!(c.pixel(5, 5), 0);
        assert_eq!(c.pixel(0, 0), 0);
    }

    #[test]
    fn circles_and_rounded_rects_are_antialiased() {
        let mut px = vec![0u32; 40 * 40];
        let mut c = Canvas::new(40, 40, &mut px);
        c.fill_circle(20.0, 20.0, 10.0, RED, 255);
        assert_eq!(alpha(c.pixel(20, 20)), 255);
        assert_eq!(c.pixel(2, 2), 0);
        let edge = alpha(c.pixel(29, 20));
        assert!(edge > 0 && edge <= 255);
        assert_eq!(alpha(c.pixel(31, 20)), 0);

        c.clear();
        c.fill_rounded_rect(PhysicalRect::new(0, 0, 40, 20), 8.0, RED, 255);
        assert_eq!(alpha(c.pixel(20, 10)), 255);
        assert_eq!(alpha(c.pixel(0, 0)), 0);
        assert_eq!(alpha(c.pixel(20, 0)), 255);
        assert_eq!(c.pixel(20, 25), 0);
        assert_premultiplied(&px);
    }

    #[test]
    fn triangles_cover_their_interior_only() {
        let mut px = vec![0u32; 20 * 20];
        let mut c = Canvas::new(20, 20, &mut px);
        let tri = [
            PhysicalPoint { x: 19, y: 10 },
            PhysicalPoint { x: 1, y: 1 },
            PhysicalPoint { x: 1, y: 19 },
        ];
        c.fill_triangle(tri, RED, 255);
        assert_eq!(alpha(c.pixel(6, 10)), 255);
        assert_eq!(c.pixel(18, 2), 0);
        assert_eq!(c.pixel(0, 10), 0);
        // Degenerate triangles draw nothing.
        c.clear();
        c.fill_triangle([PhysicalPoint { x: 1, y: 1 }; 3], RED, 255);
        assert!(px.iter().all(|&p| p == 0));
    }

    #[test]
    fn restore_alpha_undoes_gdi_alpha_and_clamps_channels() {
        let mut px = vec![0u32; 4];
        let mut c = Canvas::new(2, 2, &mut px);
        c.fill_rect(PhysicalRect::new(0, 0, 2, 2), RED, 200);
        let saved = c.alpha_snapshot(PhysicalRect::new(0, 0, 2, 2));
        assert_eq!(saved, vec![200; 4]);
        // Simulate GDI writing opaque-white text with alpha 0.
        c.px[0] = 0x00FF_FFFF;
        c.restore_alpha(PhysicalRect::new(0, 0, 2, 2), &saved);
        assert_eq!(c.pixel(0, 0), 0xC8C8_C8C8);
        assert_eq!(c.pixel(1, 1), premultiply(RED, 200));
    }

    #[test]
    fn highlight_draws_an_opaque_border_and_faint_fill() {
        let m = Metrics::for_dpi(96);
        let layout = compute_layout(&LayoutInput {
            target: PhysicalRect::new(100, 100, 140, 120),
            style: OverlayStyle::Highlight,
            monitor: PhysicalRect::new(0, 0, 800, 600),
            work: PhysicalRect::new(0, 0, 800, 560),
            metrics: m,
            label_text: None,
            badge_text: None,
        })
        .unwrap();
        let (w, h) = (layout.window.width(), layout.window.height());
        let mut px = vec![0u32; (w * h) as usize];
        let mut c = Canvas::new(w, h, &mut px);
        draw(&mut c, &layout, &m, RED);
        assert_eq!(c.pixel(0, 0), premultiply(RED, 255));
        assert_eq!(c.pixel(w - 1, h - 1), premultiply(RED, 255));
        assert_eq!(c.pixel(w / 2, h / 2), premultiply(RED, FILL_ALPHA));
        assert_premultiplied(&px);
    }

    #[test]
    fn polygons_cover_their_interior_only() {
        let mut px = vec![0u32; 20 * 20];
        let mut c = Canvas::new(20, 20, &mut px);
        // A notched arrowhead: the notch at (10, 10) stays empty.
        let shape = [(1.0, 1.0), (6.0, 18.0), (10.0, 10.0), (18.0, 6.0)];
        c.fill_polygon(&shape, RED, 255);
        assert_eq!(c.pixel(4, 4), premultiply(RED, 255));
        assert_eq!(c.pixel(13, 13), 0);
        assert_eq!(c.pixel(19, 0), 0);
        assert_premultiplied(&px);
    }

    #[test]
    fn pointer_is_an_arrowhead_with_a_white_edge_over_a_dark_bubble() {
        let m = Metrics::for_dpi(96);
        let layout = compute_layout(&LayoutInput {
            target: PhysicalRect::new(100, 100, 140, 120),
            style: OverlayStyle::Pointer,
            monitor: PhysicalRect::new(0, 0, 800, 600),
            work: PhysicalRect::new(0, 0, 800, 560),
            metrics: m,
            label_text: Some((60, 18)),
            badge_text: Some((8, 16)),
        })
        .unwrap();
        let (w, h) = (layout.window.width(), layout.window.height());
        let mut px = vec![0u32; (w * h) as usize];
        let mut c = Canvas::new(w, h, &mut px);
        draw(&mut c, &layout, &m, RED);
        let tip = layout.pointer.unwrap().tip;
        assert_eq!(c.pixel(tip.x + 5, tip.y + 5), premultiply(RED, 255));
        // Just outside a wing: the white edge.
        assert_eq!(c.pixel(tip.x + 2, tip.y + 12), premultiply(WHITE, 255));
        let text = layout.text.unwrap();
        assert_eq!(c.pixel(text.left, text.top), premultiply(BUBBLE_FILL, 255));
        let badge = layout.badge.unwrap().center();
        assert_eq!(c.pixel(badge.x, badge.y), premultiply(RED, 255));
        assert_premultiplied(&px);
    }

    #[test]
    fn every_style_paints_valid_premultiplied_pixels() {
        let m = Metrics::for_dpi(144);
        for style in [
            OverlayStyle::Highlight,
            OverlayStyle::Arrow,
            OverlayStyle::ClickMarker,
            OverlayStyle::Pointer,
        ] {
            let layout = compute_layout(&LayoutInput {
                target: PhysicalRect::new(300, 300, 400, 340),
                style,
                monitor: PhysicalRect::new(0, 0, 800, 600),
                work: PhysicalRect::new(0, 0, 800, 560),
                metrics: m,
                label_text: Some((60, 18)),
                badge_text: Some((9, 16)),
            })
            .unwrap();
            let (w, h) = (layout.window.width(), layout.window.height());
            let mut px = vec![0u32; (w * h) as usize];
            let mut c = Canvas::new(w, h, &mut px);
            draw(&mut c, &layout, &m, 0x00FF_D700);
            let label = layout.label.unwrap();
            let mid = label.center();
            assert_eq!(alpha(c.pixel(mid.x, mid.y)), 255, "{style:?} label");
            assert_premultiplied(&px);
            assert!(
                px.iter().any(|&p| alpha(p) == 255),
                "{style:?} drew nothing"
            );
        }
    }
}
