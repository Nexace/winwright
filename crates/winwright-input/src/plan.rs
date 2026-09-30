//! Pure input planning. Every operation is first turned into [`RawInput`] batches here, so
//! normalization, ordering, chunking, and cleanup are unit-tested without touching the desktop.

use std::collections::HashSet;
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{VK_RETURN, VK_TAB};
use windows::Win32::UI::WindowsAndMessaging::WHEEL_DELTA;
use winwright_contracts::geometry::{PhysicalPoint, PhysicalRect};
use winwright_contracts::input::{Key, MouseButton};
use winwright_contracts::{WinwrightError, WinwrightResult};

use crate::keys::{Layout, is_extended, is_modifier_vk, resolve_chord};

pub(crate) const MAX_CLICKS: u32 = 3;
pub(crate) const MAX_SCROLL_LINES: i32 = 100;
pub(crate) const MAX_TEXT_CHARS: usize = 10_000;
pub(crate) const TEXT_CHUNK_CHARS: usize = 32;
pub(crate) const MIN_DRAG: Duration = Duration::from_millis(100);
pub(crate) const MAX_DRAG: Duration = Duration::from_secs(10);
const MIN_DRAG_STEPS: u32 = 8;
const DRAG_STEP: Duration = Duration::from_millis(16);
const NORMALIZED_MAX: u64 = 65_535;

/// One synthesized event, before conversion to a Win32 `INPUT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RawInput {
    /// Absolute move in normalized virtual-desktop units (`0..=65535` on both axes).
    Move { x: i32, y: i32 },
    /// Press or release at the current cursor position.
    Button { button: MouseButton, down: bool },
    /// Wheel rotation in `WHEEL_DELTA` units. Vertical: positive rotates away from the user
    /// (content scrolls up). Horizontal: positive tilts right.
    Wheel { horizontal: bool, delta: i32 },
    Key {
        vk: u16,
        scan: u16,
        extended: bool,
        down: bool,
    },
    /// One UTF-16 code unit via `KEYEVENTF_UNICODE`.
    Unicode { unit: u16, down: bool },
}

impl RawInput {
    pub(crate) fn key(vk: u16, down: bool, layout: &impl Layout) -> Self {
        Self::Key {
            vk,
            scan: layout.scan_code(vk),
            extended: is_extended(vk),
            down,
        }
    }

    fn transition(self) -> Option<(Held, bool)> {
        match self {
            Self::Key { vk, down, .. } => Some((Held::Key(vk), down)),
            Self::Unicode { unit, down } => Some((Held::Unicode(unit), down)),
            Self::Button { button, down } => Some((Held::Button(button), down)),
            Self::Move { .. } | Self::Wheel { .. } => None,
        }
    }
}

/// Something this backend pressed and has not yet released.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Held {
    Key(u16),
    Unicode(u16),
    Button(MouseButton),
}

/// Applies the down/up transitions of events the system actually inserted.
pub(crate) fn track(held: &mut HashSet<Held>, inserted: &[RawInput]) {
    for (item, down) in inserted.iter().filter_map(|e| e.transition()) {
        if down {
            held.insert(item);
        } else {
            held.remove(&item);
        }
    }
}

/// Up events for everything in `held`: ordinary keys first, then modifiers (so a stuck
/// shortcut cannot re-fire), then mouse buttons. Deterministic regardless of set order.
pub(crate) fn plan_release(
    held: impl IntoIterator<Item = Held>,
    layout: &impl Layout,
) -> Vec<RawInput> {
    let mut held: Vec<Held> = held.into_iter().collect();
    held.sort_unstable_by_key(release_rank);
    held.dedup();
    held.into_iter()
        .map(|item| match item {
            Held::Key(vk) => RawInput::key(vk, false, layout),
            Held::Unicode(unit) => RawInput::Unicode { unit, down: false },
            Held::Button(button) => RawInput::Button {
                button,
                down: false,
            },
        })
        .collect()
}

fn release_rank(held: &Held) -> (u8, u16) {
    match *held {
        Held::Unicode(unit) => (0, unit),
        Held::Key(vk) if !is_modifier_vk(vk) => (1, vk),
        Held::Key(vk) => (2, vk),
        Held::Button(MouseButton::Left) => (3, 0),
        Held::Button(MouseButton::Right) => (3, 1),
        Held::Button(MouseButton::Middle) => (3, 2),
    }
}

/// Maps a physical virtual-desktop point to `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`
/// units. See [`normalize_axis`]. Points outside `virt` are clamped; callers check first.
pub(crate) fn normalize(p: PhysicalPoint, virt: PhysicalRect) -> (i32, i32) {
    (
        normalize_axis(p.x, virt.left, virt.right),
        normalize_axis(p.y, virt.top, virt.bottom),
    )
}

/// Normalizes one axis of `[start, end)`: the first pixel maps to 0 and the last to 65535.
///
/// The value is the nearest point of the linear map `offset * 65535 / (extent - 1)` (rounded
/// half up), nudged into `[ceil(offset * 65536 / extent), ceil((offset + 1) * 65536 / extent) - 1]`
/// — the range Windows maps back to exactly this pixel with `floor(n * extent / 65536)`. The plain
/// linear formula alone lands one pixel short near the start of wide screens.
fn normalize_axis(value: i32, start: i32, end: i32) -> i32 {
    let extent = (i64::from(end) - i64::from(start)).max(1);
    let offset = (i64::from(value) - i64::from(start)).clamp(0, extent - 1) as u64;
    let extent = extent as u64;
    if extent == 1 {
        return 0;
    }
    let linear = (2 * offset * NORMALIZED_MAX + (extent - 1)) / (2 * (extent - 1));
    let lo = (offset * 65_536).div_ceil(extent);
    let hi = ((offset + 1) * 65_536).div_ceil(extent) - 1;
    let n = if lo <= hi {
        linear.clamp(lo, hi)
    } else {
        linear
    };
    n.min(NORMALIZED_MAX) as i32
}

/// Absolute move to `p`, rejected when `p` is not on the virtual screen.
pub(crate) fn plan_move(p: PhysicalPoint, virt: PhysicalRect) -> WinwrightResult<RawInput> {
    if virt.is_empty() || !virt.contains(p) {
        return Err(WinwrightError::InputFailed {
            reason: format!(
                "point ({}, {}) is outside the virtual screen [{}, {}, {}, {}]",
                p.x, p.y, virt.left, virt.top, virt.right, virt.bottom
            ),
        });
    }
    let (x, y) = normalize(p, virt);
    Ok(RawInput::Move { x, y })
}

/// Down/up pairs for a single, double, or triple click, sent back-to-back in one batch so the
/// clicks are always well inside `GetDoubleClickTime()`.
pub(crate) fn plan_click(button: MouseButton, count: u32) -> WinwrightResult<Vec<RawInput>> {
    if !(1..=MAX_CLICKS).contains(&count) {
        return Err(WinwrightError::invalid(format!(
            "click count must be between 1 and {MAX_CLICKS}, got {count}"
        )));
    }
    Ok((0..count)
        .flat_map(|_| {
            [
                RawInput::Button { button, down: true },
                RawInput::Button {
                    button,
                    down: false,
                },
            ]
        })
        .collect())
}

/// Wheel events: positive `lines_y` scrolls down (negative wheel delta), positive `lines_x`
/// scrolls right. One "line" is one wheel notch (`WHEEL_DELTA`); each axis is clamped to
/// ±[`MAX_SCROLL_LINES`]. Vertical first, then horizontal; zero axes are omitted.
pub(crate) fn plan_scroll(lines_x: i32, lines_y: i32) -> Vec<RawInput> {
    let delta = |lines: i32| lines.clamp(-MAX_SCROLL_LINES, MAX_SCROLL_LINES) * WHEEL_DELTA as i32;
    let mut events = Vec::with_capacity(2);
    if lines_y != 0 {
        events.push(RawInput::Wheel {
            horizontal: false,
            delta: -delta(lines_y),
        });
    }
    if lines_x != 0 {
        events.push(RawInput::Wheel {
            horizontal: true,
            delta: delta(lines_x),
        });
    }
    events
}

/// Validates a drag duration: at most [`MAX_DRAG`], raised to at least [`MIN_DRAG`].
pub(crate) fn drag_duration(requested: Duration) -> WinwrightResult<Duration> {
    if requested > MAX_DRAG {
        return Err(WinwrightError::invalid(format!(
            "drag duration must be at most {} ms, got {} ms",
            MAX_DRAG.as_millis(),
            requested.as_millis()
        )));
    }
    Ok(requested.max(MIN_DRAG))
}

/// Evenly spaced points strictly between `from` and `to` (one per ~16 ms, at least eight),
/// followed by `to` itself, and the pause before each so the path spans `duration`.
pub(crate) fn drag_path(
    from: PhysicalPoint,
    to: PhysicalPoint,
    duration: Duration,
) -> (Vec<PhysicalPoint>, Duration) {
    let between = u32::try_from(duration.as_millis() / DRAG_STEP.as_millis())
        .unwrap_or(u32::MAX)
        .max(MIN_DRAG_STEPS);
    let segments = between.saturating_add(1);
    let lerp = |a: i32, b: i32, i: u32| {
        let t = f64::from(i) / f64::from(segments);
        (f64::from(a) + (f64::from(b) - f64::from(a)) * t).round() as i32
    };
    let points = (1..=segments)
        .map(|i| PhysicalPoint {
            x: lerp(from.x, to.x, i),
            y: lerp(from.y, to.y, i),
        })
        .collect();
    (points, duration / segments)
}

/// A chord: every key down in order, then every key up in reverse.
pub(crate) struct ChordPlan {
    pub(crate) press: Vec<RawInput>,
    pub(crate) release: Vec<RawInput>,
}

pub(crate) fn plan_chord(keys: &[Key], layout: &impl Layout) -> WinwrightResult<ChordPlan> {
    let vks = resolve_chord(keys, layout)?;
    Ok(ChordPlan {
        press: vks
            .iter()
            .map(|&vk| RawInput::key(vk, true, layout))
            .collect(),
        release: vks
            .iter()
            .rev()
            .map(|&vk| RawInput::key(vk, false, layout))
            .collect(),
    })
}

enum Stroke {
    Vk(u16),
    Char(char),
}

/// Plans `type_text` as batches of at most [`TEXT_CHUNK_CHARS`] characters. Each character is a
/// `KEYEVENTF_UNICODE` down/up per UTF-16 unit (a surrogate pair is one character, two units);
/// `\n`, `\r\n`, and a lone `\r` become one Enter, `\t` becomes Tab. Other control characters
/// are rejected: they are keys, not text.
pub(crate) fn plan_text(text: &str, layout: &impl Layout) -> WinwrightResult<Vec<Vec<RawInput>>> {
    let count = text.chars().count();
    if count > MAX_TEXT_CHARS {
        return Err(WinwrightError::invalid(format!(
            "type_text accepts at most {MAX_TEXT_CHARS} characters, got {count}"
        )));
    }
    let mut strokes = Vec::with_capacity(count);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        strokes.push(match c {
            '\r' => {
                chars.next_if_eq(&'\n');
                Stroke::Vk(VK_RETURN.0)
            }
            '\n' => Stroke::Vk(VK_RETURN.0),
            '\t' => Stroke::Vk(VK_TAB.0),
            c if c.is_control() => {
                return Err(WinwrightError::invalid(format!(
                    "type_text cannot type control character U+{:04X}; use press_keys",
                    u32::from(c)
                )));
            }
            c => Stroke::Char(c),
        });
    }
    Ok(strokes
        .chunks(TEXT_CHUNK_CHARS)
        .map(|chunk| {
            let mut batch = Vec::with_capacity(chunk.len() * 2);
            for stroke in chunk {
                match *stroke {
                    Stroke::Vk(vk) => {
                        batch.push(RawInput::key(vk, true, layout));
                        batch.push(RawInput::key(vk, false, layout));
                    }
                    Stroke::Char(c) => {
                        let mut buf = [0u16; 2];
                        for &unit in c.encode_utf16(&mut buf).iter() {
                            batch.push(RawInput::Unicode { unit, down: true });
                            batch.push(RawInput::Unicode { unit, down: false });
                        }
                    }
                }
            }
            batch
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_LEFT, VK_OEM_PLUS, VK_SHIFT};

    use super::*;
    use crate::keys::UsLayout;

    fn pt(x: i32, y: i32) -> PhysicalPoint {
        PhysicalPoint { x, y }
    }

    fn key(vk: u16, down: bool) -> RawInput {
        RawInput::key(vk, down, &UsLayout)
    }

    fn uni(unit: u16, down: bool) -> RawInput {
        RawInput::Unicode { unit, down }
    }

    // ---- normalization ------------------------------------------------------------------

    /// Windows' inverse mapping for absolute input (`n * extent >> 16`).
    fn floor_back(n: i32, extent: i64) -> i64 {
        i64::from(n) * extent / 65_536
    }

    /// The inverse of the documented linear mapping, rounded.
    fn linear_back(n: i32, extent: i64) -> i64 {
        (i64::from(n) * (extent - 1) * 2 + 65_535) / (2 * 65_535)
    }

    #[test]
    fn edges_map_to_zero_and_max() {
        let virt = PhysicalRect::new(0, 0, 1920, 1080);
        assert_eq!(normalize(pt(0, 0), virt), (0, 0));
        assert_eq!(normalize(pt(1919, 1079), virt), (65_535, 65_535));
    }

    #[test]
    fn negative_origin_edges() {
        // 1920x1080 primary plus a 2560x1440 monitor to its left, 360 px higher.
        let virt = PhysicalRect::new(-2560, -360, 1920, 1080);
        assert_eq!((virt.width(), virt.height()), (4480, 1440));
        assert_eq!(normalize(pt(-2560, -360), virt), (0, 0));
        assert_eq!(normalize(pt(1919, 1079), virt), (65_535, 65_535));
        // The primary's origin sits 2560/4480 and 360/1440 of the way in.
        let (x, y) = normalize(pt(0, 0), virt);
        assert_eq!(floor_back(x, 4480), 2560);
        assert_eq!(floor_back(y, 1440), 360);
    }

    #[test]
    fn two_monitor_layouts_round_trip_every_pixel() {
        for virt in [
            // Side by side, secondary right.
            PhysicalRect::new(0, 0, 3840, 1080),
            // Secondary left and above.
            PhysicalRect::new(-2560, -360, 1920, 1080),
            // Portrait secondary above.
            PhysicalRect::new(0, -1920, 1920, 1080),
        ] {
            for x in virt.left..virt.right {
                let (nx, _) = normalize(pt(x, virt.top), virt);
                assert_eq!(
                    floor_back(nx, i64::from(virt.width())) + i64::from(virt.left),
                    i64::from(x)
                );
            }
            for y in virt.top..virt.bottom {
                let (_, ny) = normalize(pt(virt.left, y), virt);
                assert_eq!(
                    floor_back(ny, i64::from(virt.height())) + i64::from(virt.top),
                    i64::from(y)
                );
            }
        }
    }

    #[test]
    fn every_offset_round_trips_under_both_inverse_models() {
        for extent in [
            1, 2, 3, 7, 640, 1024, 1080, 1366, 1440, 1920, 2160, 3840, 5760, 7680, 32_767,
        ] {
            for offset in 0..extent {
                let n = normalize_axis(offset, 0, extent);
                assert!((0..=65_535).contains(&n), "extent {extent} offset {offset}");
                let e = i64::from(extent);
                assert_eq!(
                    floor_back(n, e),
                    i64::from(offset),
                    "floor {extent}/{offset}"
                );
                assert_eq!(
                    linear_back(n, e),
                    i64::from(offset),
                    "linear {extent}/{offset}"
                );
            }
            assert_eq!(normalize_axis(0, 0, extent), 0);
            if extent > 1 {
                assert_eq!(normalize_axis(extent - 1, 0, extent), 65_535);
            }
        }
    }

    #[test]
    fn plain_linear_formula_would_miss_near_the_origin() {
        // Documents why the value is nudged: round(1 * 65535 / 1919) = 34 maps back to pixel 0.
        assert_eq!(floor_back(34, 1920), 0);
        assert_eq!(floor_back(normalize_axis(1, 0, 1920), 1920), 1);
    }

    #[test]
    fn one_pixel_extremes() {
        let virt = PhysicalRect::new(-5, 7, -4, 8);
        assert_eq!(normalize(pt(-5, 7), virt), (0, 0));
        assert_eq!(
            plan_move(pt(-5, 7), virt).unwrap(),
            RawInput::Move { x: 0, y: 0 }
        );
        assert!(plan_move(pt(-4, 7), virt).is_err());
        let wide = PhysicalRect::new(0, 0, 2, 2);
        assert_eq!(normalize(pt(1, 1), wide), (65_535, 65_535));
        assert_eq!(normalize(pt(0, 1), wide), (0, 65_535));
    }

    #[test]
    fn points_outside_the_virtual_screen_are_input_failed() {
        let virt = PhysicalRect::new(-1920, 0, 1920, 1080);
        for p in [pt(1920, 0), pt(-1921, 0), pt(0, -1), pt(0, 1080)] {
            let err = plan_move(p, virt).unwrap_err();
            assert_eq!(err.code().as_str(), "INPUT_FAILED", "{p:?}");
        }
        let err = plan_move(pt(0, 0), PhysicalRect::new(0, 0, 0, 0)).unwrap_err();
        assert_eq!(err.code().as_str(), "INPUT_FAILED");
    }

    // ---- clicks, scroll, drag -----------------------------------------------------------

    #[test]
    fn click_counts() {
        for count in [0, 4, u32::MAX] {
            let err = plan_click(MouseButton::Left, count).unwrap_err();
            assert_eq!(err.code().as_str(), "INVALID_REQUEST");
        }
        let down = |button| RawInput::Button { button, down: true };
        let up = |button| RawInput::Button {
            button,
            down: false,
        };
        assert_eq!(
            plan_click(MouseButton::Right, 1).unwrap(),
            [down(MouseButton::Right), up(MouseButton::Right)]
        );
        let triple = plan_click(MouseButton::Left, 3).unwrap();
        assert_eq!(triple.len(), 6);
        assert!(
            triple
                .chunks(2)
                .all(|pair| pair == [down(MouseButton::Left), up(MouseButton::Left)])
        );
        assert_eq!(plan_click(MouseButton::Middle, 2).unwrap().len(), 4);
    }

    #[test]
    fn scroll_sign_conventions_and_clamping() {
        let v = |delta| RawInput::Wheel {
            horizontal: false,
            delta,
        };
        let h = |delta| RawInput::Wheel {
            horizontal: true,
            delta,
        };
        assert_eq!(plan_scroll(0, 3), [v(-360)], "positive y scrolls down");
        assert_eq!(plan_scroll(0, -2), [v(240)], "negative y scrolls up");
        assert_eq!(plan_scroll(1, 0), [h(120)], "positive x scrolls right");
        assert_eq!(plan_scroll(-4, 0), [h(-480)]);
        assert_eq!(plan_scroll(2, 1), [v(-120), h(240)]);
        assert_eq!(plan_scroll(0, 1_000), [v(-12_000)]);
        assert_eq!(plan_scroll(i32::MIN, i32::MAX), [v(-12_000), h(-12_000)]);
        assert!(plan_scroll(0, 0).is_empty());
    }

    #[test]
    fn drag_duration_bounds() {
        assert_eq!(drag_duration(Duration::ZERO).unwrap(), MIN_DRAG);
        assert_eq!(
            drag_duration(Duration::from_millis(750)).unwrap(),
            Duration::from_millis(750)
        );
        assert_eq!(drag_duration(MAX_DRAG).unwrap(), MAX_DRAG);
        let err = drag_duration(MAX_DRAG + Duration::from_millis(1)).unwrap_err();
        assert_eq!(err.code().as_str(), "INVALID_REQUEST");
    }

    #[test]
    fn drag_path_has_at_least_eight_intermediate_points_and_ends_on_target() {
        let (from, to) = (pt(-100, 50), pt(300, -30));
        for duration in [MIN_DRAG, Duration::from_millis(500), MAX_DRAG] {
            let (path, step) = drag_path(from, to, duration);
            let intermediate = &path[..path.len() - 1];
            assert!(intermediate.len() >= 8, "{duration:?}");
            assert_eq!(*path.last().unwrap(), to);
            assert!(intermediate.iter().all(|p| *p != to && *p != from));
            // Monotonic along both axes and inside the bounding box.
            assert!(
                path.windows(2)
                    .all(|w| w[1].x >= w[0].x && w[1].y <= w[0].y)
            );
            let total = step * u32::try_from(path.len()).unwrap();
            assert!(total <= duration && duration - total < Duration::from_millis(1));
        }
        let (path, _) = drag_path(from, to, MIN_DRAG);
        assert_eq!(path.len(), 9);
        let (path, _) = drag_path(from, to, Duration::from_secs(1));
        assert_eq!(path.len(), 63);
    }

    #[test]
    fn zero_length_drag_still_moves() {
        let (path, _) = drag_path(pt(5, 5), pt(5, 5), MIN_DRAG);
        assert_eq!(path.len(), 9);
        assert!(path.iter().all(|p| *p == pt(5, 5)));
    }

    // ---- keys ---------------------------------------------------------------------------

    #[test]
    fn chord_presses_in_order_and_releases_in_reverse() {
        let plan = plan_chord(&[Key::Ctrl, Key::Shift, Key::Char('s')], &UsLayout).unwrap();
        let s = u16::from(b'S');
        assert_eq!(
            plan.press,
            [key(VK_CONTROL.0, true), key(VK_SHIFT.0, true), key(s, true)]
        );
        assert_eq!(
            plan.release,
            [
                key(s, false),
                key(VK_SHIFT.0, false),
                key(VK_CONTROL.0, false)
            ]
        );
        // Scan codes come from the layout; extended flag from the table.
        assert_eq!(
            plan.press[2],
            RawInput::Key {
                vk: s,
                scan: 0x100 + s,
                extended: false,
                down: true
            }
        );
    }

    #[test]
    fn chord_auto_shift_and_extended_keys() {
        let plan = plan_chord(&[Key::Ctrl, Key::Char('+')], &UsLayout).unwrap();
        assert_eq!(
            plan.press,
            [
                key(VK_CONTROL.0, true),
                key(VK_SHIFT.0, true),
                key(VK_OEM_PLUS.0, true)
            ]
        );
        let plan = plan_chord(&[Key::Shift, Key::Left], &UsLayout).unwrap();
        assert!(matches!(
            plan.press[1],
            RawInput::Key {
                vk,
                extended: true,
                ..
            } if vk == VK_LEFT.0
        ));
        assert!(plan_chord(&[Key::Char('a'), Key::Ctrl], &UsLayout).is_err());
    }

    // ---- text ---------------------------------------------------------------------------

    #[test]
    fn text_is_unicode_down_up_per_unit() {
        let batches = plan_text("hi", &UsLayout).unwrap();
        assert_eq!(
            batches,
            [vec![
                uni(u16::from(b'h'), true),
                uni(u16::from(b'h'), false),
                uni(u16::from(b'i'), true),
                uni(u16::from(b'i'), false),
            ]]
        );
    }

    #[test]
    fn surrogate_pairs_are_two_units_in_order() {
        let batches = plan_text("😀", &UsLayout).unwrap();
        assert_eq!(
            batches,
            [vec![
                uni(0xD83D, true),
                uni(0xD83D, false),
                uni(0xDE00, true),
                uni(0xDE00, false),
            ]]
        );
    }

    #[test]
    fn newlines_and_tabs_are_keys() {
        let enter = [key(VK_RETURN.0, true), key(VK_RETURN.0, false)];
        let tab = [key(VK_TAB.0, true), key(VK_TAB.0, false)];
        let a = [uni(u16::from(b'a'), true), uni(u16::from(b'a'), false)];
        let expected = [[a, enter, a].concat()];
        for text in ["a\r\na", "a\na", "a\ra"] {
            assert_eq!(plan_text(text, &UsLayout).unwrap(), expected, "{text:?}");
        }
        assert_eq!(
            plan_text("\n\r\n\t", &UsLayout).unwrap(),
            [[enter, enter, tab].concat()]
        );
    }

    #[test]
    fn text_is_chunked_by_characters() {
        let text: String = "ab😀".repeat(30); // 90 characters, 120 UTF-16 units
        let batches = plan_text(&text, &UsLayout).unwrap();
        assert_eq!(batches.len(), 3);
        let chars: Vec<usize> = batches
            .iter()
            .map(|b| {
                b.iter()
                    .filter(|e| matches!(e, RawInput::Unicode { unit, down: true } if !(0xDC00..0xE000).contains(unit)))
                    .count()
            })
            .collect();
        assert_eq!(chars, [32, 32, 26]);
        // A surrogate pair never straddles a batch boundary.
        for batch in &batches {
            assert!(!matches!(
                batch.last(),
                Some(RawInput::Unicode {
                    unit: 0xD800..=0xDBFF,
                    ..
                })
            ));
        }
        // `\r\n` counts as one character toward the chunk size.
        let text = "\r\n".repeat(40);
        let batches = plan_text(&text, &UsLayout).unwrap();
        assert_eq!(batches.iter().map(Vec::len).collect::<Vec<_>>(), [64, 16]);
    }

    #[test]
    fn text_limits() {
        assert!(plan_text("", &UsLayout).unwrap().is_empty());
        assert_eq!(
            plan_text(&"x".repeat(MAX_TEXT_CHARS), &UsLayout)
                .unwrap()
                .len(),
            MAX_TEXT_CHARS.div_ceil(TEXT_CHUNK_CHARS)
        );
        let err = plan_text(&"x".repeat(MAX_TEXT_CHARS + 1), &UsLayout).unwrap_err();
        assert_eq!(err.code().as_str(), "INVALID_REQUEST");
        let err = plan_text("a\u{8}b", &UsLayout).unwrap_err();
        assert_eq!(err.code().as_str(), "INVALID_REQUEST");
        assert!(err.to_string().contains("U+0008"));
    }

    // ---- held state ---------------------------------------------------------------------

    #[test]
    fn tracking_follows_inserted_transitions() {
        let mut held = HashSet::new();
        track(
            &mut held,
            &[
                key(VK_CONTROL.0, true),
                RawInput::Move { x: 1, y: 1 },
                RawInput::Button {
                    button: MouseButton::Left,
                    down: true,
                },
                uni(0x41, true),
                uni(0x41, false),
                RawInput::Wheel {
                    horizontal: false,
                    delta: 120,
                },
            ],
        );
        assert_eq!(
            held,
            HashSet::from([Held::Key(VK_CONTROL.0), Held::Button(MouseButton::Left)])
        );
        track(&mut held, &[key(VK_CONTROL.0, false)]);
        assert_eq!(held, HashSet::from([Held::Button(MouseButton::Left)]));
        // Releasing something not held is a no-op.
        track(&mut held, &[key(VK_SHIFT.0, false)]);
        assert_eq!(held.len(), 1);
    }

    #[test]
    fn release_plan_orders_keys_then_modifiers_then_buttons() {
        let held = [
            Held::Button(MouseButton::Middle),
            Held::Key(VK_SHIFT.0),
            Held::Button(MouseButton::Left),
            Held::Key(u16::from(b'S')),
            Held::Unicode(0xE9),
            Held::Key(VK_CONTROL.0),
        ];
        let plan = plan_release(held, &UsLayout);
        assert_eq!(
            plan,
            [
                uni(0xE9, false),
                key(u16::from(b'S'), false),
                key(VK_SHIFT.0, false),
                key(VK_CONTROL.0, false),
                RawInput::Button {
                    button: MouseButton::Left,
                    down: false
                },
                RawInput::Button {
                    button: MouseButton::Middle,
                    down: false
                },
            ]
        );
        assert!(plan_release([], &UsLayout).is_empty());
        // Every planned event is an "up": applying it clears the held set.
        let mut set: HashSet<Held> = held.into_iter().collect();
        track(&mut set, &plan);
        assert!(set.is_empty());
    }
}
