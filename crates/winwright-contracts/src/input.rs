//! Physical input contracts (spec §4 Level 4, §43, §44). Implemented by `winwright-input`.
//! Every point is physical virtual-desktop pixels.

use std::borrow::Cow;
use std::fmt;
use std::time::Duration;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use crate::WinwrightResult;
use crate::backend::{BackendFuture, OperationContext};
use crate::geometry::PhysicalPoint;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// A keyboard key. Serialized as its display name: `"Ctrl"`, `"F5"`, `"PageDown"`, `"a"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Key {
    Ctrl,
    Shift,
    Alt,
    Win,
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    CapsLock,
    PrintScreen,
    ContextMenu,
    /// F1..=F24.
    Function(u8),
    /// A printable character. Letters are stored lower-case; Shift is explicit.
    Char(char),
}

const NAMED: &[(&str, Key)] = &[
    ("ctrl", Key::Ctrl),
    ("control", Key::Ctrl),
    ("shift", Key::Shift),
    ("alt", Key::Alt),
    ("menu", Key::Alt),
    ("win", Key::Win),
    ("windows", Key::Win),
    ("meta", Key::Win),
    ("cmd", Key::Win),
    ("super", Key::Win),
    ("enter", Key::Enter),
    ("return", Key::Enter),
    ("tab", Key::Tab),
    ("escape", Key::Escape),
    ("esc", Key::Escape),
    ("backspace", Key::Backspace),
    ("delete", Key::Delete),
    ("del", Key::Delete),
    ("insert", Key::Insert),
    ("ins", Key::Insert),
    ("space", Key::Space),
    ("up", Key::Up),
    ("arrowup", Key::Up),
    ("down", Key::Down),
    ("arrowdown", Key::Down),
    ("left", Key::Left),
    ("arrowleft", Key::Left),
    ("right", Key::Right),
    ("arrowright", Key::Right),
    ("home", Key::Home),
    ("end", Key::End),
    ("pageup", Key::PageUp),
    ("pgup", Key::PageUp),
    ("pagedown", Key::PageDown),
    ("pgdn", Key::PageDown),
    ("capslock", Key::CapsLock),
    ("printscreen", Key::PrintScreen),
    ("prtsc", Key::PrintScreen),
    ("contextmenu", Key::ContextMenu),
    ("apps", Key::ContextMenu),
    ("plus", Key::Char('+')),
    ("minus", Key::Char('-')),
    ("comma", Key::Char(',')),
    ("period", Key::Char('.')),
];

impl Key {
    pub fn is_modifier(self) -> bool {
        matches!(self, Key::Ctrl | Key::Shift | Key::Alt | Key::Win)
    }

    /// Case-insensitive: `ctrl`, `Control`, `F5`, `PgDn`, `a`, `A` (-> `a`), `Plus`.
    pub fn parse(value: &str) -> Option<Key> {
        let trimmed = value.trim();
        let mut chars = trimmed.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return (!c.is_control() && !c.is_whitespace())
                .then(|| Key::Char(c.to_lowercase().next().unwrap_or(c)));
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(n) = lower.strip_prefix('f').and_then(|d| d.parse::<u8>().ok()) {
            return (1..=24).contains(&n).then_some(Key::Function(n));
        }
        NAMED
            .iter()
            .find(|(name, _)| *name == lower)
            .map(|(_, key)| *key)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Function(n) => write!(f, "F{n}"),
            Key::Char(c) => write!(f, "{c}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl TryFrom<String> for Key {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Key::parse(&value).ok_or_else(|| format!("unknown key {value:?}"))
    }
}

impl From<Key> for String {
    fn from(key: Key) -> Self {
        key.to_string()
    }
}

impl JsonSchema for Key {
    fn schema_name() -> Cow<'static, str> {
        "Key".into()
    }
    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(generator)
    }
}

/// Parses `"Ctrl+Shift+S"` (or `"Ctrl + s"`) into keys, modifiers first in the given order.
/// Use `Plus` for a literal `+`. Exactly one non-modifier key is allowed, and it must be last.
pub fn parse_chord(value: &str) -> Result<Vec<Key>, String> {
    let keys = value
        .split('+')
        .map(|part| {
            Key::parse(part).ok_or_else(|| {
                if part.trim().is_empty() {
                    format!("empty key in {value:?} (write a literal + as Plus, e.g. Ctrl+Plus)")
                } else {
                    format!("unknown key {:?} in {value:?}", part.trim())
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_chord(&keys)?;
    Ok(keys)
}

pub fn validate_chord(keys: &[Key]) -> Result<(), String> {
    if keys.is_empty() {
        return Err("empty key chord".into());
    }
    if keys.len() > 6 {
        return Err("a chord has at most 6 keys".into());
    }
    let non_modifiers = keys.iter().filter(|k| !k.is_modifier()).count();
    if non_modifiers > 1 {
        return Err("a chord has at most one non-modifier key".into());
    }
    if non_modifiers == 1 && keys.last().is_some_and(|k| k.is_modifier()) {
        return Err("the non-modifier key must come last".into());
    }
    Ok(())
}

/// Synthesized input. Implementations track every key/button they press so
/// [`InputBackend::release_all`] can undo them from the emergency-stop path.
pub trait InputBackend: Send + Sync {
    fn move_to<'a>(
        &'a self,
        point: PhysicalPoint,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()>;

    fn click<'a>(
        &'a self,
        point: PhysicalPoint,
        button: MouseButton,
        count: u32,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()>;

    fn drag<'a>(
        &'a self,
        from: PhysicalPoint,
        to: PhysicalPoint,
        button: MouseButton,
        duration: Duration,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()>;

    /// Wheel at `point`; positive `notches_y` scrolls down, positive `notches_x` scrolls right.
    fn scroll<'a>(
        &'a self,
        point: PhysicalPoint,
        notches_x: i32,
        notches_y: i32,
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()>;

    /// Types Unicode text into the focused control (`KEYEVENTF_UNICODE`; `\n` -> Enter).
    fn type_text<'a>(&'a self, text: &'a str, ctx: &'a OperationContext) -> BackendFuture<'a, ()>;

    /// Presses `keys` in order and releases them in reverse (a chord such as Ctrl+Shift+S).
    fn press_keys<'a>(
        &'a self,
        keys: &'a [Key],
        ctx: &'a OperationContext,
    ) -> BackendFuture<'a, ()>;

    /// Emergency cleanup: releases every key and button this backend currently holds down.
    /// Synchronous and independent of any queue so the stop path cannot be blocked.
    fn release_all(&self) -> WinwrightResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_case_insensitively_with_aliases() {
        assert_eq!(Key::parse("ctrl"), Some(Key::Ctrl));
        assert_eq!(Key::parse("CONTROL"), Some(Key::Ctrl));
        assert_eq!(Key::parse("Esc"), Some(Key::Escape));
        assert_eq!(Key::parse("F5"), Some(Key::Function(5)));
        assert_eq!(Key::parse("f24"), Some(Key::Function(24)));
        assert_eq!(Key::parse("F25"), None);
        assert_eq!(Key::parse("F0"), None);
        assert_eq!(Key::parse("A"), Some(Key::Char('a')));
        assert_eq!(Key::parse("/"), Some(Key::Char('/')));
        assert_eq!(Key::parse("Plus"), Some(Key::Char('+')));
        assert_eq!(Key::parse("nope"), None);
        assert_eq!(Key::parse(""), None);
    }

    #[test]
    fn keys_round_trip_through_json() {
        for key in [
            Key::Ctrl,
            Key::PageDown,
            Key::Function(11),
            Key::Char('s'),
            Key::Char('+'),
        ] {
            let json = serde_json::to_string(&key).unwrap();
            let back: Key = serde_json::from_str(&json).unwrap();
            assert_eq!(back, key, "{json}");
        }
        assert_eq!(serde_json::to_string(&Key::Function(5)).unwrap(), "\"F5\"");
        assert!(serde_json::from_str::<Key>("\"Hyper\"").is_err());
    }

    #[test]
    fn chords() {
        assert_eq!(
            parse_chord("Ctrl+Shift+S").unwrap(),
            vec![Key::Ctrl, Key::Shift, Key::Char('s')]
        );
        assert_eq!(
            parse_chord("ctrl + plus").unwrap(),
            vec![Key::Ctrl, Key::Char('+')]
        );
        assert_eq!(parse_chord("Alt").unwrap(), vec![Key::Alt]);
        assert!(parse_chord("Ctrl+A+B").is_err());
        assert!(parse_chord("A+Ctrl").is_err());
        assert!(parse_chord("Ctrl+Bogus").is_err());
        assert!(parse_chord("").is_err());
        assert!(parse_chord("Ctrl++").unwrap_err().contains("Plus"));
    }
}
