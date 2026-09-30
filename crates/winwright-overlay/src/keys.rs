//! Global-hotkey chords (pure): [`Key`]s -> `RegisterHotKey` modifiers + one virtual-key code.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1,
    VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};
use winwright_contracts::input::{Key, validate_chord};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chord {
    /// `MOD_*` flags (without `MOD_NOREPEAT`).
    pub modifiers: u32,
    pub vk: u32,
    /// Canonical display form, e.g. `Ctrl+Alt+Escape`.
    pub label: String,
}

const MODIFIER_NAMES: [(u32, &str); 4] = [
    (MOD_CONTROL.0, "Ctrl"),
    (MOD_ALT.0, "Alt"),
    (MOD_SHIFT.0, "Shift"),
    (MOD_WIN.0, "Win"),
];

fn key_name(key: Key) -> String {
    match key {
        Key::Char(c) => c.to_uppercase().collect(),
        other => other.to_string(),
    }
}

fn describe(keys: &[Key]) -> String {
    keys.iter()
        .map(|&k| key_name(k))
        .collect::<Vec<_>>()
        .join("+")
}

/// Maps a validated chord to `RegisterHotKey` arguments. Requires at least one modifier (a bare
/// global key would hijack it in every application) and exactly one supported non-modifier.
/// `scan` maps a letter or digit to its virtual key in the active layout (`VkKeyScanW` low
/// byte); when it cannot, the ASCII upper-case code (`VK_A`..`VK_Z`, `VK_0`..`VK_9`) is used.
pub fn chord(keys: &[Key], scan: impl Fn(char) -> Option<u8>) -> Result<Chord, String> {
    validate_chord(keys).map_err(|e| format!("invalid hotkey {}: {e}", describe(keys)))?;
    let mut modifiers = 0;
    let mut main = None;
    for &key in keys {
        match key {
            Key::Ctrl => modifiers |= MOD_CONTROL.0,
            Key::Alt => modifiers |= MOD_ALT.0,
            Key::Shift => modifiers |= MOD_SHIFT.0,
            Key::Win => modifiers |= MOD_WIN.0,
            other => main = Some(other),
        }
    }
    let Some(main) = main else {
        return Err(format!(
            "hotkey {} needs exactly one non-modifier key",
            describe(keys)
        ));
    };
    if modifiers == 0 {
        return Err(format!(
            "hotkey {} needs at least one modifier (Ctrl, Alt, Shift or Win)",
            describe(keys)
        ));
    }
    let vk = virtual_key(main, &scan).ok_or_else(|| {
        format!(
            "{} cannot be used in a global hotkey; use a letter, digit, F1-F24, Escape, Space, \
             Enter, Tab, an arrow key, Home, End, PageUp, PageDown, Insert or Delete",
            key_name(main)
        )
    })?;
    let mut parts: Vec<String> = MODIFIER_NAMES
        .iter()
        .filter(|(flag, _)| modifiers & flag != 0)
        .map(|(_, name)| (*name).to_owned())
        .collect();
    parts.push(key_name(main));
    Ok(Chord {
        modifiers,
        vk,
        label: parts.join("+"),
    })
}

fn virtual_key(key: Key, scan: &impl Fn(char) -> Option<u8>) -> Option<u32> {
    let vk = match key {
        Key::Function(n @ 1..=24) => VK_F1.0 + u16::from(n) - 1,
        Key::Escape => VK_ESCAPE.0,
        Key::Space => VK_SPACE.0,
        Key::Enter => VK_RETURN.0,
        Key::Tab => VK_TAB.0,
        Key::Up => VK_UP.0,
        Key::Down => VK_DOWN.0,
        Key::Left => VK_LEFT.0,
        Key::Right => VK_RIGHT.0,
        Key::Home => VK_HOME.0,
        Key::End => VK_END.0,
        Key::PageUp => VK_PRIOR.0,
        Key::PageDown => VK_NEXT.0,
        Key::Insert => VK_INSERT.0,
        Key::Delete => VK_DELETE.0,
        Key::Char(c) if c.is_ascii_alphanumeric() => {
            u16::from(scan(c).unwrap_or(c.to_ascii_uppercase() as u8))
        }
        _ => return None,
    };
    Some(u32::from(vk))
}

#[cfg(test)]
mod tests {
    use winwright_contracts::input::parse_chord;

    use super::*;

    fn no_layout(_: char) -> Option<u8> {
        None
    }

    fn map(chord_text: &str) -> Result<Chord, String> {
        chord(&parse_chord(chord_text).unwrap(), no_layout)
    }

    #[test]
    fn emergency_default_maps_to_ctrl_alt_escape() {
        let c = map(crate::EMERGENCY_STOP_DEFAULT).unwrap();
        assert_eq!(c.modifiers, MOD_CONTROL.0 | MOD_ALT.0);
        assert_eq!(c.vk, 0x1B);
        assert_eq!(c.label, "Ctrl+Alt+Escape");
    }

    #[test]
    fn modifiers_are_canonicalized_in_the_label() {
        let c = map("shift+win+alt+ctrl+f12").unwrap();
        assert_eq!(c.modifiers, 0b1111);
        assert_eq!(c.vk, 0x7B);
        assert_eq!(c.label, "Ctrl+Alt+Shift+Win+F12");
        let dup = chord(&[Key::Ctrl, Key::Ctrl, Key::Function(1)], no_layout).unwrap();
        assert_eq!(
            (dup.modifiers, dup.label.as_str()),
            (MOD_CONTROL.0, "Ctrl+F1")
        );
    }

    #[test]
    fn named_keys_map_to_virtual_keys() {
        let cases = [
            ("Ctrl+F1", 0x70),
            ("Ctrl+F24", 0x87),
            ("Ctrl+Space", 0x20),
            ("Ctrl+Enter", 0x0D),
            ("Ctrl+Tab", 0x09),
            ("Ctrl+Up", 0x26),
            ("Ctrl+Down", 0x28),
            ("Ctrl+Left", 0x25),
            ("Ctrl+Right", 0x27),
            ("Ctrl+Home", 0x24),
            ("Ctrl+End", 0x23),
            ("Ctrl+PageUp", 0x21),
            ("Ctrl+PageDown", 0x22),
            ("Ctrl+Insert", 0x2D),
            ("Ctrl+Delete", 0x2E),
        ];
        for (text, vk) in cases {
            assert_eq!(map(text).unwrap().vk, vk, "{text}");
        }
    }

    #[test]
    fn letters_and_digits_use_the_layout_then_ascii() {
        assert_eq!(map("Win+a").unwrap().vk, 0x41);
        assert_eq!(map("Win+a").unwrap().label, "Win+A");
        assert_eq!(map("Alt+7").unwrap().vk, 0x37);
        let azerty = chord(&[Key::Ctrl, Key::Char('a')], |_| Some(0x51)).unwrap();
        assert_eq!(azerty.vk, 0x51);
    }

    #[test]
    fn invalid_chords_are_rejected_with_the_chord_named() {
        let bare = map("Escape").unwrap_err();
        assert!(
            bare.contains("Escape") && bare.contains("modifier"),
            "{bare}"
        );
        let only_mods = map("Ctrl+Alt").unwrap_err();
        assert!(only_mods.contains("Ctrl+Alt"), "{only_mods}");
        let slash = map("Ctrl+/").unwrap_err();
        assert!(slash.starts_with("/ cannot be used"), "{slash}");
        assert!(map("Ctrl+Backspace").is_err());
        assert!(map("Ctrl+PrintScreen").is_err());
        assert!(chord(&[Key::Ctrl, Key::Function(25)], no_layout).is_err());
        assert!(chord(&[], no_layout).is_err());
        let two = chord(&[Key::Ctrl, Key::Char('a'), Key::Char('b')], no_layout).unwrap_err();
        assert!(two.contains("Ctrl+A+B"), "{two}");
    }
}
