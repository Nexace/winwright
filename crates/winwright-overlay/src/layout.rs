//! Pure overlay geometry. Everything is physical virtual-desktop pixels (spec §43): positions
//! are never scaled, only stroke, marker, and label sizes follow the target monitor's DPI.
//! Rects are half-open (`right`/`bottom` exclusive); points are pixels.

use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::overlay::OverlayStyle;

/// Converts device-independent pixels to physical pixels at `dpi` (96 = 100 %), at least 1.
pub fn dip(value: i32, dpi: u32) -> i32 {
    let px = (i64::from(value) * i64::from(dpi) + 48) / 96;
    px.clamp(1, i64::from(i32::MAX)) as i32
}

/// DPI-scaled sizes for one overlay, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub border: i32,
    /// Space between the target and an arrow tip or a label.
    pub gap: i32,
    pub arrow_length: i32,
    pub arrow_head_length: i32,
    pub arrow_head_width: i32,
    pub arrow_shaft: i32,
    pub marker_radius: i32,
    pub marker_ring: i32,
    pub label_pad_x: i32,
    pub label_pad_y: i32,
    pub label_radius: i32,
    /// Em height of the label font (Segoe UI 12 pt).
    pub label_font: i32,
    pub badge_diameter: i32,
    pub badge_ring: i32,
    pub badge_font: i32,
    /// Pointer style: the pointer's length along each side, and the white edge around it.
    pub pointer_size: i32,
    pub pointer_edge: i32,
}

impl Metrics {
    pub fn for_dpi(dpi: u32) -> Self {
        let s = |value| dip(value, dpi);
        Self {
            border: s(3),
            gap: s(6),
            arrow_length: s(56),
            arrow_head_length: s(22),
            arrow_head_width: s(30),
            arrow_shaft: s(10),
            marker_radius: s(14),
            marker_ring: s(4),
            label_pad_x: s(8),
            label_pad_y: s(4),
            label_radius: s(6),
            label_font: s(16),
            badge_diameter: s(24),
            badge_ring: s(2),
            badge_font: s(13),
            pointer_size: s(21),
            pointer_edge: s(2),
        }
    }
}

pub fn inflate(r: PhysicalRect, by: i32) -> PhysicalRect {
    PhysicalRect::new(r.left - by, r.top - by, r.right + by, r.bottom + by)
}

pub fn offset(r: PhysicalRect, dx: i32, dy: i32) -> PhysicalRect {
    PhysicalRect::new(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy)
}

pub fn union(a: PhysicalRect, b: PhysicalRect) -> PhysicalRect {
    PhysicalRect::new(
        a.left.min(b.left),
        a.top.min(b.top),
        a.right.max(b.right),
        a.bottom.max(b.bottom),
    )
}

/// Overlap of `a` and `b`; empty (possibly inverted) when they do not intersect.
pub fn intersect(a: PhysicalRect, b: PhysicalRect) -> PhysicalRect {
    PhysicalRect::new(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )
}

pub fn contains_rect(outer: PhysicalRect, inner: PhysicalRect) -> bool {
    inner.left >= outer.left
        && inner.top >= outer.top
        && inner.right <= outer.right
        && inner.bottom <= outer.bottom
}

/// Start of a `len`-long span that wants to begin at `start` but must stay within
/// `[lo, hi)`; aligned to `lo` when it cannot fit.
fn clamp_span(start: i32, len: i32, lo: i32, hi: i32) -> i32 {
    if len >= hi - lo {
        lo
    } else {
        start.clamp(lo, hi - len)
    }
}

/// Moves `r` (size unchanged) the least distance that puts it inside `bounds`.
pub fn clamp_into(r: PhysicalRect, bounds: PhysicalRect) -> PhysicalRect {
    let left = clamp_span(r.left, r.width(), bounds.left, bounds.right);
    let top = clamp_span(r.top, r.height(), bounds.top, bounds.bottom);
    offset(r, left - r.left, top - r.top)
}

/// Smallest rect covering both pixels.
fn span_rect(a: PhysicalPoint, b: PhysicalPoint) -> PhysicalRect {
    PhysicalRect::new(
        a.x.min(b.x),
        a.y.min(b.y),
        a.x.max(b.x) + 1,
        a.y.max(b.y) + 1,
    )
}

/// Which side of the target the arrow sits on; it points toward the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowSide {
    Left,
    Right,
    Top,
    Bottom,
}

impl ArrowSide {
    /// Tried in this order; the first side where the whole arrow fits the work area wins.
    pub const PREFERENCE: [ArrowSide; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrow {
    pub side: ArrowSide,
    /// Tip pixel, `gap` away from the target edge.
    pub tip: PhysicalPoint,
    /// Triangle: the tip, then the two base corners.
    pub head: [PhysicalPoint; 3],
    pub shaft: PhysicalRect,
    pub bounds: PhysicalRect,
}

impl Arrow {
    fn offset(self, dx: i32, dy: i32) -> Self {
        let shift = |p: PhysicalPoint| PhysicalPoint {
            x: p.x + dx,
            y: p.y + dy,
        };
        Self {
            side: self.side,
            tip: shift(self.tip),
            head: self.head.map(shift),
            shaft: offset(self.shaft, dx, dy),
            bounds: offset(self.bounds, dx, dy),
        }
    }
}

/// The arrow on `side` of `target`, pointing at it. Its cross-axis position is the target's
/// center, kept inside `work` so the whole head stays visible.
pub fn arrow_on_side(
    target: PhysicalRect,
    side: ArrowSide,
    work: PhysicalRect,
    m: &Metrics,
) -> Arrow {
    let half = m.arrow_head_width / 2;
    let center = target.center();
    let cx = clamp_span(center.x - half, 2 * half + 1, work.left, work.right) + half;
    let cy = clamp_span(center.y - half, 2 * half + 1, work.top, work.bottom) + half;
    // `dir` is the pointing direction.
    let (tip, dir) = match side {
        ArrowSide::Left => (
            PhysicalPoint {
                x: target.left - m.gap,
                y: cy,
            },
            (1, 0),
        ),
        ArrowSide::Right => (
            PhysicalPoint {
                x: target.right - 1 + m.gap,
                y: cy,
            },
            (-1, 0),
        ),
        ArrowSide::Top => (
            PhysicalPoint {
                x: cx,
                y: target.top - m.gap,
            },
            (0, 1),
        ),
        ArrowSide::Bottom => (
            PhysicalPoint {
                x: cx,
                y: target.bottom - 1 + m.gap,
            },
            (0, -1),
        ),
    };
    // `along` runs from the tip back toward the tail; `across` is perpendicular to it.
    let at = |along: i32, across: i32| PhysicalPoint {
        x: tip.x - dir.0 * along + i32::abs(dir.1) * across,
        y: tip.y - dir.1 * along + i32::abs(dir.0) * across,
    };
    let head_length = m.arrow_head_length.min(m.arrow_length);
    let shaft_half = m.arrow_shaft / 2;
    Arrow {
        side,
        tip,
        head: [tip, at(head_length, -half), at(head_length, half)],
        shaft: span_rect(
            at(head_length - 1, -shaft_half),
            at(m.arrow_length - 1, m.arrow_shaft - shaft_half - 1),
        ),
        bounds: span_rect(at(0, -half), at(m.arrow_length - 1, half)),
    }
}

fn room(target: PhysicalRect, work: PhysicalRect, side: ArrowSide) -> i32 {
    match side {
        ArrowSide::Left => target.left - work.left,
        ArrowSide::Right => work.right - target.right,
        ArrowSide::Top => target.top - work.top,
        ArrowSide::Bottom => work.bottom - target.bottom,
    }
}

/// Left if the arrow fits there, else right, top, bottom; when none fits, the roomiest side.
pub fn choose_arrow_side(target: PhysicalRect, work: PhysicalRect, m: &Metrics) -> ArrowSide {
    ArrowSide::PREFERENCE
        .into_iter()
        .find(|&side| contains_rect(work, arrow_on_side(target, side, work, m).bounds))
        .unwrap_or_else(|| {
            // `max_by_key` keeps the last maximum, so iterate in reverse to favor preference.
            ArrowSide::PREFERENCE
                .into_iter()
                .rev()
                .max_by_key(|&side| room(target, work, side))
                .unwrap_or(ArrowSide::Left)
        })
}

/// The arrow on the chosen side, shifted into `work` if it still sticks out.
pub fn place_arrow(target: PhysicalRect, work: PhysicalRect, m: &Metrics) -> Arrow {
    let arrow = arrow_on_side(target, choose_arrow_side(target, work, m), work, m);
    let moved = clamp_into(arrow.bounds, work);
    arrow.offset(moved.left - arrow.bounds.left, moved.top - arrow.bounds.top)
}

/// The pointer style's dart, its tip on the target, pointing up and left like a mouse pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pointer {
    pub tip: PhysicalPoint,
    /// The colored dart, drawn over [`Pointer::edge`], a slightly larger white one.
    pub fill: [PhysicalPoint; 3],
    pub edge: [PhysicalPoint; 3],
    pub bounds: PhysicalRect,
}

impl Pointer {
    fn offset(self, dx: i32, dy: i32) -> Self {
        let shift = |p: PhysicalPoint| PhysicalPoint {
            x: p.x + dx,
            y: p.y + dy,
        };
        Self {
            tip: shift(self.tip),
            fill: self.fill.map(shift),
            edge: self.edge.map(shift),
            bounds: offset(self.bounds, dx, dy),
        }
    }
}

pub fn place_pointer(tip: PhysicalPoint, m: &Metrics) -> Pointer {
    let (s, e) = (m.pointer_size, m.pointer_edge);
    let narrow = s * 2 / 7;
    let at = |x: i32, y: i32| PhysicalPoint {
        x: tip.x + x,
        y: tip.y + y,
    };
    let edge = [
        at(-e, -e),
        at(narrow - e / 2, s + 2 * e),
        at(s + 2 * e, narrow - e / 2),
    ];
    Pointer {
        tip,
        fill: [at(0, 0), at(narrow, s), at(s, narrow)],
        edge,
        bounds: span_rect(edge[0], at(s + 2 * e, s + 2 * e)),
    }
}

/// The caption bubble just past the pointer's tail, below and right of the tip; left of the
/// tip or above it where the work area ends, and always inside it.
pub fn place_bubble(
    tip: PhysicalPoint,
    size: (i32, i32),
    work: PhysicalRect,
    m: &Metrics,
) -> PhysicalRect {
    let w = size.0.clamp(1, work.width().max(1));
    let h = size.1.clamp(1, work.height().max(1));
    let reach = m.pointer_size * 6 / 7;
    let mut left = tip.x + reach;
    if left + w > work.right {
        left = tip.x - m.gap - w;
    }
    let mut top = tip.y + reach;
    if top + h > work.bottom {
        top = tip.y - m.gap - h;
    }
    clamp_into(PhysicalRect::new(left, top, left + w, top + h), work)
}

/// A `size` box above `anchor` (below it when there is no room above, overlapping it when
/// neither fits), starting at `anchor.left` or `min_left`, whichever is further right, and
/// kept inside `work`. Boxes wider or taller than `work` are shrunk to it.
pub fn place_label(
    anchor: PhysicalRect,
    size: (i32, i32),
    min_left: i32,
    work: PhysicalRect,
    gap: i32,
) -> PhysicalRect {
    let w = size.0.clamp(1, work.width().max(1));
    let h = size.1.clamp(1, work.height().max(1));
    let left = clamp_span(anchor.left.max(min_left), w, work.left, work.right);
    let above = anchor.top - gap - h;
    let below = anchor.bottom + gap;
    let fits = |top: i32| top >= work.top && top + h <= work.bottom;
    let top = [above, below]
        .into_iter()
        .find(|&top| fits(top))
        .unwrap_or_else(|| clamp_span(above, h, work.top, work.bottom));
    PhysicalRect::new(left, top, left + w, top + h)
}

/// A round step badge centered on `corner` (a pill when the number is wide), inside `work`.
pub fn place_badge(
    corner: PhysicalPoint,
    text: (i32, i32),
    m: &Metrics,
    work: PhysicalRect,
) -> PhysicalRect {
    let h = m.badge_diameter.max(text.1 + 2 * m.badge_ring);
    let w = h.max(text.0 + 2 * m.badge_ring + m.label_pad_x);
    let left = corner.x - w / 2;
    let top = corner.y - h / 2;
    clamp_into(PhysicalRect::new(left, top, left + w, top + h), work)
}

pub fn marker_bounds(center: PhysicalPoint, m: &Metrics) -> PhysicalRect {
    let r = m.marker_radius + m.marker_ring + 1;
    PhysicalRect::new(
        center.x - r,
        center.y - r,
        center.x + r + 1,
        center.y + r + 1,
    )
}

/// Everything the layout needs; text sizes are measured by the renderer beforehand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutInput {
    pub target: PhysicalRect,
    pub style: OverlayStyle,
    /// Full bounds of the monitor the target is (mostly) on.
    pub monitor: PhysicalRect,
    /// That monitor's work area (excludes the taskbar).
    pub work: PhysicalRect,
    pub metrics: Metrics,
    /// Single-line label text size in pixels, without padding.
    pub label_text: Option<(i32, i32)>,
    /// Step number text size in pixels.
    pub badge_text: Option<(i32, i32)>,
}

/// Overlay window bounds on screen plus every part in window-local pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub window: PhysicalRect,
    /// The target rect: filled faintly, with the border drawn just outside it.
    pub highlight: Option<PhysicalRect>,
    pub arrow: Option<Arrow>,
    /// Click-marker center.
    pub marker: Option<PhysicalPoint>,
    pub pointer: Option<Pointer>,
    /// Label box including padding (the bubble, with the pointer style).
    pub label: Option<PhysicalRect>,
    pub badge: Option<PhysicalRect>,
}

/// Lays out one overlay. The window is the union of all parts clipped to the target's
/// monitor; `None` when nothing of it is on that monitor.
pub fn compute_layout(input: &LayoutInput) -> Option<Layout> {
    let m = &input.metrics;
    let target = input.target;
    let work = if input.work.is_empty() {
        input.monitor
    } else {
        input.work
    };
    let highlight = (input.style == OverlayStyle::Highlight).then_some(target);
    let arrow = (input.style == OverlayStyle::Arrow).then(|| place_arrow(target, work, m));
    let marker = (input.style == OverlayStyle::ClickMarker).then(|| target.center());
    let pointer = (input.style == OverlayStyle::Pointer).then(|| place_pointer(target.center(), m));
    let anchor = if highlight.is_some() {
        inflate(target, m.border)
    } else {
        target
    };
    let corner = PhysicalPoint {
        x: target.left,
        y: target.top,
    };
    let badge = input
        .badge_text
        .filter(|_| pointer.is_none())
        .map(|text| place_badge(corner, text, m, work));
    let label = input.label_text.map(|(w, h)| {
        let size = (w + 2 * m.label_pad_x, h + 2 * m.label_pad_y);
        match &pointer {
            Some(p) => place_bubble(p.tip, size, work, m),
            None => {
                let min_left = badge.map_or(i32::MIN, |b| b.right + m.gap / 2);
                place_label(anchor, size, min_left, work, m.gap)
            }
        }
    });

    let parts = [
        highlight.map(|t| inflate(t, m.border)),
        arrow.map(|a| a.bounds),
        marker.map(|c| marker_bounds(c, m)),
        pointer.map(|p| p.bounds),
        label,
        badge,
    ];
    let window = intersect(parts.into_iter().flatten().reduce(union)?, input.monitor);
    if window.is_empty() {
        return None;
    }
    let (dx, dy) = (-window.left, -window.top);
    Some(Layout {
        window,
        highlight: highlight.map(|r| offset(r, dx, dy)),
        arrow: arrow.map(|a| a.offset(dx, dy)),
        marker: marker.map(|p| PhysicalPoint {
            x: p.x + dx,
            y: p.y + dy,
        }),
        pointer: pointer.map(|p| p.offset(dx, dy)),
        label: label.map(|r| offset(r, dx, dy)),
        badge: badge.map(|r| offset(r, dx, dy)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: PhysicalRect = PhysicalRect::new(0, 0, 1920, 1080);
    const WORK: PhysicalRect = PhysicalRect::new(0, 0, 1920, 1040);

    fn m100() -> Metrics {
        Metrics::for_dpi(96)
    }

    fn input(target: PhysicalRect, style: OverlayStyle) -> LayoutInput {
        LayoutInput {
            target,
            style,
            monitor: MONITOR,
            work: WORK,
            metrics: m100(),
            label_text: None,
            badge_text: None,
        }
    }

    fn on_screen(layout: &Layout, r: PhysicalRect) -> PhysicalRect {
        offset(r, layout.window.left, layout.window.top)
    }

    #[test]
    fn dip_scales_and_rounds_with_a_floor_of_one() {
        assert_eq!(dip(3, 96), 3);
        assert_eq!(dip(3, 120), 4); // 3.75
        assert_eq!(dip(3, 144), 5); // 4.5 rounds up
        assert_eq!(dip(3, 192), 6);
        assert_eq!(dip(0, 96), 1);
        let big = Metrics::for_dpi(192);
        assert_eq!((big.border, big.marker_radius, big.label_font), (6, 28, 32));
    }

    #[test]
    fn highlight_window_is_target_plus_border() {
        let target = PhysicalRect::new(100, 200, 300, 260);
        let layout = compute_layout(&input(target, OverlayStyle::Highlight)).unwrap();
        assert_eq!(layout.window, PhysicalRect::new(97, 197, 303, 263));
        assert_eq!(layout.highlight, Some(PhysicalRect::new(3, 3, 203, 63)));
        assert_eq!(
            (layout.arrow, layout.marker, layout.label),
            (None, None, None)
        );
    }

    #[test]
    fn positions_are_never_scaled_only_sizes() {
        let target = PhysicalRect::new(1000, 500, 1100, 540);
        let mut i = input(target, OverlayStyle::Highlight);
        i.metrics = Metrics::for_dpi(192);
        let layout = compute_layout(&i).unwrap();
        assert_eq!(layout.window, PhysicalRect::new(994, 494, 1106, 546));
        assert_eq!(on_screen(&layout, layout.highlight.unwrap()), target);
    }

    #[test]
    fn label_goes_above_the_highlight() {
        let target = PhysicalRect::new(400, 300, 600, 340);
        let mut i = input(target, OverlayStyle::Highlight);
        i.label_text = Some((80, 20));
        let layout = compute_layout(&i).unwrap();
        let label = on_screen(&layout, layout.label.unwrap());
        // 80 + 2*8 wide, 20 + 2*4 tall, bottom `gap` above the border.
        assert_eq!(label, PhysicalRect::new(397, 263, 493, 291));
        assert!(contains_rect(layout.window, label));
    }

    #[test]
    fn label_flips_below_when_no_room_above() {
        let target = PhysicalRect::new(400, 5, 600, 45);
        let label = place_label(inflate(target, 3), (96, 28), i32::MIN, WORK, 6);
        assert_eq!(label.top, 48 + 6);
    }

    #[test]
    fn label_overlaps_when_neither_side_fits_and_stays_in_work() {
        let target = PhysicalRect::new(0, 0, 1920, 1040);
        let label = place_label(target, (96, 28), i32::MIN, WORK, 6);
        assert_eq!(label, PhysicalRect::new(0, 0, 96, 28));
    }

    #[test]
    fn label_is_clamped_horizontally_and_shrunk_to_the_work_area() {
        let near_right = PhysicalRect::new(1880, 500, 1915, 520);
        let label = place_label(near_right, (200, 28), i32::MIN, WORK, 6);
        assert_eq!((label.left, label.right), (1720, 1920));
        let huge = place_label(near_right, (5000, 28), i32::MIN, WORK, 6);
        assert_eq!((huge.left, huge.right), (0, 1920));
    }

    #[test]
    fn label_respects_a_negative_origin_work_area() {
        let work = PhysicalRect::new(-1920, -300, 0, 780);
        let target = PhysicalRect::new(-1915, -290, -1800, -250);
        let label = place_label(target, (96, 28), i32::MIN, work, 6);
        assert!(contains_rect(work, label), "{label:?}");
        assert_eq!(label.top, -250 + 6);
        assert_eq!(label.left, -1915);
    }

    #[test]
    fn badge_sits_on_the_top_left_corner_and_pushes_the_label_right() {
        let target = PhysicalRect::new(400, 300, 600, 340);
        let mut i = input(target, OverlayStyle::Highlight);
        i.badge_text = Some((8, 16));
        i.label_text = Some((80, 20));
        let layout = compute_layout(&i).unwrap();
        let badge = on_screen(&layout, layout.badge.unwrap());
        assert_eq!(badge, PhysicalRect::new(388, 288, 412, 312));
        let label = on_screen(&layout, layout.label.unwrap());
        assert_eq!(label.left, badge.right + 3);
    }

    #[test]
    fn badge_widens_into_a_pill_and_stays_in_work() {
        let m = m100();
        let wide = place_badge(PhysicalPoint { x: 0, y: 0 }, (40, 16), &m, WORK);
        assert_eq!((wide.width(), wide.height()), (52, 24));
        assert_eq!((wide.left, wide.top), (0, 0));
    }

    #[test]
    fn arrow_prefers_the_left_side_and_points_right() {
        let target = PhysicalRect::new(500, 400, 600, 440);
        let m = m100();
        let arrow = place_arrow(target, WORK, &m);
        assert_eq!(arrow.side, ArrowSide::Left);
        assert_eq!(arrow.tip, PhysicalPoint { x: 494, y: 420 });
        assert!(arrow.head[1].x < arrow.tip.x && arrow.head[2].x < arrow.tip.x);
        assert_eq!(arrow.bounds, PhysicalRect::new(439, 405, 495, 436));
        assert!(arrow.bounds.right <= target.left);
        assert!(contains_rect(arrow.bounds, arrow.shaft));
        assert_eq!(arrow.shaft.height(), m.arrow_shaft);
    }

    #[test]
    fn arrow_flips_right_top_then_bottom_near_edges() {
        let m = m100();
        let near_left = PhysicalRect::new(10, 400, 100, 440);
        let right = place_arrow(near_left, WORK, &m);
        assert_eq!(right.side, ArrowSide::Right);
        assert_eq!(right.tip.x, 99 + 6);
        assert!(right.bounds.left > near_left.right - 1);

        let wide = PhysicalRect::new(20, 400, 1900, 440);
        let top = place_arrow(wide, WORK, &m);
        assert_eq!(top.side, ArrowSide::Top);
        assert_eq!(top.tip, PhysicalPoint { x: 960, y: 394 });
        assert!(top.head[1].y < top.tip.y);

        let wide_at_top = PhysicalRect::new(20, 10, 1900, 60);
        let bottom = place_arrow(wide_at_top, WORK, &m);
        assert_eq!(bottom.side, ArrowSide::Bottom);
        assert_eq!(bottom.tip.y, 59 + 6);
        assert!(bottom.head[1].y > bottom.tip.y);
    }

    #[test]
    fn arrow_with_no_room_anywhere_is_clamped_into_work() {
        let m = m100();
        let full = PhysicalRect::new(0, 0, 1920, 1040);
        assert_eq!(choose_arrow_side(full, WORK, &m), ArrowSide::Left);
        let arrow = place_arrow(full, WORK, &m);
        assert!(contains_rect(WORK, arrow.bounds), "{:?}", arrow.bounds);

        // Most room below: bottom wins when nothing fits.
        let tall = PhysicalRect::new(0, 0, 1920, 1000);
        assert_eq!(choose_arrow_side(tall, WORK, &m), ArrowSide::Bottom);
    }

    #[test]
    fn arrow_cross_axis_is_kept_inside_work() {
        let m = m100();
        let hugging_top = PhysicalRect::new(500, -40, 600, 4);
        let arrow = place_arrow(hugging_top, WORK, &m);
        assert_eq!(arrow.side, ArrowSide::Left);
        assert_eq!(arrow.bounds.top, 0);
        assert!(contains_rect(WORK, arrow.bounds));
    }

    #[test]
    fn arrow_layout_window_excludes_the_target() {
        let target = PhysicalRect::new(500, 400, 600, 440);
        let layout = compute_layout(&input(target, OverlayStyle::Arrow)).unwrap();
        assert_eq!(layout.window, PhysicalRect::new(439, 405, 495, 436));
        assert_eq!(layout.highlight, None);
        let arrow = layout.arrow.unwrap();
        assert_eq!(arrow.bounds, PhysicalRect::new(0, 0, 56, 31));
        assert_eq!(arrow.tip, PhysicalPoint { x: 55, y: 15 });
    }

    #[test]
    fn click_marker_is_centered() {
        let target = PhysicalRect::new(100, 100, 200, 140);
        let layout = compute_layout(&input(target, OverlayStyle::ClickMarker)).unwrap();
        let center = layout.marker.unwrap();
        assert_eq!(
            (center.x + layout.window.left, center.y + layout.window.top),
            (150, 120)
        );
        assert_eq!(layout.window, PhysicalRect::new(131, 101, 170, 140));
    }

    #[test]
    fn window_is_clipped_to_the_monitor_and_offscreen_targets_render_nothing() {
        let spilling = PhysicalRect::new(1800, 1000, 2100, 1200);
        let layout = compute_layout(&input(spilling, OverlayStyle::Highlight)).unwrap();
        assert_eq!(layout.window, PhysicalRect::new(1797, 997, 1920, 1080));

        let minimized = PhysicalRect::new(-32000, -32000, -31840, -31972);
        assert_eq!(
            compute_layout(&input(minimized, OverlayStyle::Highlight)),
            None
        );
    }

    #[test]
    fn pointer_tips_the_center_and_its_bubble_follows_the_room() {
        let m = m100();
        let target = PhysicalRect::new(500, 400, 600, 440);
        let mut i = input(target, OverlayStyle::Pointer);
        i.label_text = Some((80, 20));
        i.badge_text = Some((8, 16));
        let layout = compute_layout(&i).unwrap();
        let p = layout.pointer.unwrap();
        let tip = (p.tip.x + layout.window.left, p.tip.y + layout.window.top);
        assert_eq!(tip, (550, 420));
        assert_eq!(p.fill[0], p.tip);
        assert!(p.fill[1..].iter().all(|c| c.x > p.tip.x && c.y > p.tip.y));
        assert_eq!((layout.badge, layout.highlight), (None, None));
        // 18 px past the tip, 80 + 2*8 wide and 20 + 2*4 tall.
        let bubble = on_screen(&layout, layout.label.unwrap());
        assert_eq!(bubble, PhysicalRect::new(568, 438, 664, 466));
        let local = PhysicalRect::new(0, 0, layout.window.width(), layout.window.height());
        assert!(contains_rect(local, p.bounds));

        // In the bottom-right corner the bubble goes left of the tip and above it.
        let corner = place_bubble(PhysicalPoint { x: 1900, y: 1030 }, (96, 28), WORK, &m);
        assert_eq!(corner, PhysicalRect::new(1798, 996, 1894, 1024));
        // No caption: just the pointer.
        let bare = compute_layout(&input(target, OverlayStyle::Pointer)).unwrap();
        assert_eq!(bare.label, None);
        assert_eq!(bare.window, PhysicalRect::new(548, 418, 576, 446));
    }

    #[test]
    fn rect_helpers() {
        let a = PhysicalRect::new(0, 0, 10, 10);
        let b = PhysicalRect::new(5, 5, 20, 20);
        assert_eq!(union(a, b), PhysicalRect::new(0, 0, 20, 20));
        assert_eq!(intersect(a, b), PhysicalRect::new(5, 5, 10, 10));
        assert!(intersect(a, PhysicalRect::new(30, 30, 40, 40)).is_empty());
        assert_eq!(
            clamp_into(
                PhysicalRect::new(-5, 15, 5, 25),
                PhysicalRect::new(0, 0, 20, 20)
            ),
            PhysicalRect::new(0, 10, 10, 20)
        );
        assert!(contains_rect(b, PhysicalRect::new(5, 5, 20, 20)));
        assert!(!contains_rect(b, a));
    }
}
