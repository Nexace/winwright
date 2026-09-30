//! Virtual-key mapping for [`Key`] and the keyboard-layout lookups it depends on.
//! Everything here is pure; layout queries go through the [`Layout`] trait.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_APPS, VK_BACK, VK_CAPITAL, VK_CONTROL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT, VK_LWIN,
    VK_MENU, VK_NEXT, VK_NUMLOCK, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT,
    VK_RWIN, VK_SHIFT, VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP,
};
use winwright_contracts::input::{Key, validate_chord};
use winwright_contracts::{WinwrightError, WinwrightResult};

/// Keyboard-layout queries. The Win32 implementation wraps `MapVirtualKeyW` and `VkKeyScanW`
/// (the calling thread's active layout); tests substitute a fixed US layout.
pub(crate) trait Layout {
    /// Hardware scan code for `vk` (`MAPVK_VK_TO_VSC`), or 0 when the layout has none.
    fn scan_code(&self, vk: u16) -> u16;
    /// Raw `VkKeyScanW` result for one UTF-16 unit: low byte VK, high byte shift state, -1 when
    /// no key produces the character.
    fn vk_key_scan(&self, unit: u16) -> i16;
}

/// Virtual key for every key except [`Key::Char`], which depends on the layout.
pub(crate) fn named_vk(key: Key) -> Option<u16> {
    let vk: VIRTUAL_KEY = match key {
        Key::Ctrl => VK_CONTROL,
        Key::Shift => VK_SHIFT,
        Key::Alt => VK_MENU,
        Key::Win => VK_LWIN,
        Key::Enter => VK_RETURN,
        Key::Tab => VK_TAB,
        Key::Escape => VK_ESCAPE,
        Key::Backspace => VK_BACK,
        Key::Delete => VK_DELETE,
        Key::Insert => VK_INSERT,
        Key::Space => VK_SPACE,
        Key::Up => VK_UP,
        Key::Down => VK_DOWN,
        Key::Left => VK_LEFT,
        Key::Right => VK_RIGHT,
        Key::Home => VK_HOME,
        Key::End => VK_END,
        Key::PageUp => VK_PRIOR,
        Key::PageDown => VK_NEXT,
        Key::CapsLock => VK_CAPITAL,
        Key::PrintScreen => VK_SNAPSHOT,
        Key::ContextMenu => VK_APPS,
        Key::Function(n @ 1..=24) => return Some(VK_F1.0 + u16::from(n - 1)),
        Key::Function(_) | Key::Char(_) => return None,
    };
    Some(vk.0)
}

/// Keys whose hardware scan code carries the `E0` prefix, so `KEYEVENTF_EXTENDEDKEY` must be set:
/// the navigation cluster, right-hand Ctrl/Alt, both Windows keys, Apps, PrintScreen, numpad
/// divide, and NumLock. (Right Shift is *not* extended.)
const EXTENDED: [VIRTUAL_KEY; 18] = [
    VK_UP,
    VK_DOWN,
    VK_LEFT,
    VK_RIGHT,
    VK_HOME,
    VK_END,
    VK_PRIOR,
    VK_NEXT,
    VK_INSERT,
    VK_DELETE,
    VK_RCONTROL,
    VK_RMENU,
    VK_LWIN,
    VK_RWIN,
    VK_APPS,
    VK_SNAPSHOT,
    VK_DIVIDE,
    VK_NUMLOCK,
];

const MODIFIERS: [VIRTUAL_KEY; 11] = [
    VK_SHIFT,
    VK_CONTROL,
    VK_MENU,
    VK_LWIN,
    VK_RWIN,
    VK_LSHIFT,
    VK_RSHIFT,
    VK_LCONTROL,
    VK_RCONTROL,
    VK_LMENU,
    VK_RMENU,
];

pub(crate) fn is_extended(vk: u16) -> bool {
    EXTENDED.iter().any(|k| k.0 == vk)
}

pub(crate) fn is_modifier_vk(vk: u16) -> bool {
    MODIFIERS.iter().any(|k| k.0 == vk)
}

/// Modifiers a layout needs to produce a character (`VkKeyScanW` high byte, bits 0..=2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShiftState {
    pub(crate) shift: bool,
    pub(crate) ctrl: bool,
    pub(crate) alt: bool,
}

/// Decodes a `VkKeyScanW` result; `None` when no key on the layout produces the character.
pub(crate) fn decode_vk_key_scan(raw: i16) -> Option<(u16, ShiftState)> {
    let [vk, state] = raw.to_le_bytes();
    if vk == 0 || vk == 0xFF {
        return None;
    }
    Some((
        u16::from(vk),
        ShiftState {
            shift: state & 1 != 0,
            ctrl: state & 2 != 0,
            alt: state & 4 != 0,
        },
    ))
}

fn char_vk(c: char, layout: &impl Layout) -> WinwrightResult<(u16, ShiftState)> {
    let mut buf = [0u16; 2];
    let decoded = match c.encode_utf16(&mut buf) {
        [unit] => decode_vk_key_scan(layout.vk_key_scan(*unit)),
        _ => None,
    };
    decoded.ok_or_else(|| {
        WinwrightError::invalid(format!(
            "key {c:?} is not on the current keyboard layout; use type_text for arbitrary characters"
        ))
    })
}

/// Resolves a chord to virtual keys in pressing order. A character that needs Shift (or
/// Ctrl+Alt for AltGr) gets those modifiers pressed before it unless the chord already has them;
/// repeated keys are pressed once.
pub(crate) fn resolve_chord(keys: &[Key], layout: &impl Layout) -> WinwrightResult<Vec<u16>> {
    validate_chord(keys).map_err(WinwrightError::invalid)?;
    let mut vks: Vec<u16> = Vec::with_capacity(keys.len() + 3);
    let mut push = |vk: u16| {
        if !vks.contains(&vk) {
            vks.push(vk);
        }
    };
    for &key in keys {
        if let Key::Char(c) = key {
            let (vk, state) = char_vk(c, layout)?;
            for (needed, modifier) in [
                (state.ctrl, VK_CONTROL),
                (state.alt, VK_MENU),
                (state.shift, VK_SHIFT),
            ] {
                if needed {
                    push(modifier.0);
                }
            }
            push(vk);
        } else {
            let vk = named_vk(key)
                .ok_or_else(|| WinwrightError::invalid(format!("unsupported key {key}")))?;
            push(vk);
        }
    }
    Ok(vks)
}

/// Deterministic US-QWERTY stand-in for unit tests. Scan codes are `0x100 + vk` so tests can
/// see that they were filled from the layout.
#[cfg(test)]
pub(crate) struct UsLayout;

#[cfg(test)]
impl Layout for UsLayout {
    fn scan_code(&self, vk: u16) -> u16 {
        0x100 + vk
    }

    fn vk_key_scan(&self, unit: u16) -> i16 {
        let Some(c) = char::from_u32(u32::from(unit)) else {
            return -1;
        };
        let code = u32::from(c) as i16;
        match c {
            'a'..='z' => code - 0x20,
            'A'..='Z' => 0x100 | code,
            '0'..='9' => code,
            '=' => 0xBB,
            '+' => 0x1BB,
            '-' => 0xBD,
            ',' => 0xBC,
            '.' => 0xBE,
            '/' => 0xBF,
            // AltGr+E, as on German layouts: Ctrl (2) + Alt (4).
            '€' => 0x645,
            _ => -1,
        }
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F24, VK_OEM_PLUS};

    use super::*;

    #[test]
    fn named_keys_map_to_expected_virtual_keys() {
        let table: &[(Key, VIRTUAL_KEY)] = &[
            (Key::Ctrl, VK_CONTROL),
            (Key::Shift, VK_SHIFT),
            (Key::Alt, VK_MENU),
            (Key::Win, VK_LWIN),
            (Key::Enter, VK_RETURN),
            (Key::Tab, VK_TAB),
            (Key::Escape, VK_ESCAPE),
            (Key::Backspace, VK_BACK),
            (Key::Delete, VK_DELETE),
            (Key::Insert, VK_INSERT),
            (Key::Space, VK_SPACE),
            (Key::Up, VK_UP),
            (Key::Down, VK_DOWN),
            (Key::Left, VK_LEFT),
            (Key::Right, VK_RIGHT),
            (Key::Home, VK_HOME),
            (Key::End, VK_END),
            (Key::PageUp, VK_PRIOR),
            (Key::PageDown, VK_NEXT),
            (Key::CapsLock, VK_CAPITAL),
            (Key::PrintScreen, VK_SNAPSHOT),
            (Key::ContextMenu, VK_APPS),
            (Key::Function(1), VK_F1),
            (Key::Function(24), VK_F24),
        ];
        for &(key, vk) in table {
            assert_eq!(named_vk(key), Some(vk.0), "{key}");
        }
        assert_eq!(named_vk(Key::Function(5)), Some(0x74));
        assert_eq!(named_vk(Key::Function(0)), None);
        assert_eq!(named_vk(Key::Function(25)), None);
        assert_eq!(named_vk(Key::Char('a')), None);
    }

    #[test]
    fn extended_flag_covers_navigation_right_modifiers_and_win() {
        for key in [
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Insert,
            Key::Delete,
            Key::Win,
            Key::ContextMenu,
        ] {
            assert!(
                is_extended(named_vk(key).unwrap()),
                "{key} should be extended"
            );
        }
        for vk in [VK_RCONTROL, VK_RMENU, VK_RWIN] {
            assert!(is_extended(vk.0));
        }
        for key in [
            Key::Ctrl,
            Key::Shift,
            Key::Alt,
            Key::Enter,
            Key::Tab,
            Key::Escape,
            Key::Backspace,
            Key::Space,
            Key::CapsLock,
            Key::Function(5),
        ] {
            assert!(
                !is_extended(named_vk(key).unwrap()),
                "{key} is not extended"
            );
        }
        assert!(!is_extended(VK_RSHIFT.0), "right shift has no E0 prefix");
        assert!(!is_extended(u16::from(b'A')));
    }

    #[test]
    fn modifier_detection() {
        assert!(is_modifier_vk(VK_CONTROL.0));
        assert!(is_modifier_vk(VK_LWIN.0));
        assert!(is_modifier_vk(VK_RMENU.0));
        assert!(!is_modifier_vk(VK_RETURN.0));
        assert!(!is_modifier_vk(u16::from(b'S')));
    }

    #[test]
    fn vk_key_scan_decoding() {
        assert_eq!(
            decode_vk_key_scan(0x41),
            Some((0x41, ShiftState::default()))
        );
        assert_eq!(
            decode_vk_key_scan(0x1BB),
            Some((
                VK_OEM_PLUS.0,
                ShiftState {
                    shift: true,
                    ..ShiftState::default()
                }
            ))
        );
        assert_eq!(
            decode_vk_key_scan(0x645),
            Some((
                0x45,
                ShiftState {
                    shift: false,
                    ctrl: true,
                    alt: true
                }
            ))
        );
        assert_eq!(decode_vk_key_scan(-1), None);
        assert_eq!(decode_vk_key_scan(0x01FF), None);
    }

    #[test]
    fn chords_keep_order_and_add_needed_modifiers() {
        let s = u16::from(b'S');
        assert_eq!(
            resolve_chord(&[Key::Ctrl, Key::Shift, Key::Char('s')], &UsLayout).unwrap(),
            [VK_CONTROL.0, VK_SHIFT.0, s]
        );
        // `+` needs Shift on US layouts: added before the key.
        assert_eq!(
            resolve_chord(&[Key::Ctrl, Key::Char('+')], &UsLayout).unwrap(),
            [VK_CONTROL.0, VK_SHIFT.0, VK_OEM_PLUS.0]
        );
        // ...but not twice when the caller already holds it.
        assert_eq!(
            resolve_chord(&[Key::Shift, Key::Ctrl, Key::Char('+')], &UsLayout).unwrap(),
            [VK_SHIFT.0, VK_CONTROL.0, VK_OEM_PLUS.0]
        );
        // AltGr characters get Ctrl+Alt.
        assert_eq!(
            resolve_chord(&[Key::Char('€')], &UsLayout).unwrap(),
            [VK_CONTROL.0, VK_MENU.0, 0x45]
        );
        // Duplicated modifiers are pressed once.
        assert_eq!(
            resolve_chord(&[Key::Ctrl, Key::Ctrl, Key::Char('c')], &UsLayout).unwrap(),
            [VK_CONTROL.0, u16::from(b'C')]
        );
        assert_eq!(resolve_chord(&[Key::Alt], &UsLayout).unwrap(), [VK_MENU.0]);
    }

    #[test]
    fn invalid_chords_and_unmapped_characters_are_invalid_requests() {
        for keys in [
            vec![],
            vec![Key::Char('a'), Key::Ctrl],
            vec![Key::Char('a'), Key::Char('b')],
            vec![Key::Function(30)],
        ] {
            let err = resolve_chord(&keys, &UsLayout).unwrap_err();
            assert_eq!(err.code().as_str(), "INVALID_REQUEST", "{keys:?}");
        }
        let err = resolve_chord(&[Key::Ctrl, Key::Char('ß')], &UsLayout).unwrap_err();
        assert!(err.to_string().contains("keyboard layout"), "{err}");
        let err = resolve_chord(&[Key::Char('😀')], &UsLayout).unwrap_err();
        assert!(err.to_string().contains("keyboard layout"), "{err}");
    }
}
