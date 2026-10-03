use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Caller session selector. The engine owns identity and permissions; this is only a key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct SessionId(String);

/// Deserializing applies the same rules as [`SessionId::parse`].
impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "invalid session id {value:?}: use 1-{} of A-Z a-z 0-9 _ -",
                Self::MAX_LEN
            ))
        })
    }
}

impl SessionId {
    pub const MAX_LEN: usize = 64;

    /// Accepts `[A-Za-z0-9_-]{1,64}` so ids stay safe in pipe names, logs, and paths.
    pub fn parse(value: &str) -> Option<Self> {
        let ok = !value.is_empty()
            && value.len() <= Self::MAX_LEN
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        ok.then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Snapshot generation as shown on the wire: `s_104`.
pub fn format_generation(generation: u64) -> String {
    format!("s_{generation}")
}

/// Element reference as shown on the wire: `e42`.
pub fn format_element_ref(number: u64) -> String {
    format!("e{number}")
}

/// Parses `e42` into `42`. Rejects `e0`, signs, and leading zeros so refs have one spelling.
pub fn parse_element_ref(value: &str) -> Option<u64> {
    let digits = value.strip_prefix('e')?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_restricted() {
        assert!(SessionId::parse("demo").is_some());
        assert!(SessionId::parse("sess_01-a").is_some());
        assert!(SessionId::parse("").is_none());
        assert!(SessionId::parse("a b").is_none());
        assert!(SessionId::parse(r"..\pipe").is_none());
        assert!(SessionId::parse(&"x".repeat(65)).is_none());
    }

    #[test]
    fn session_ids_deserialize_with_the_same_rules() {
        let id: SessionId = serde_json::from_str(r#""sess_01-a""#).unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""sess_01-a""#);
        for bad in [r#""""#, r#""a b""#, r#""..\\pipe""#] {
            assert!(serde_json::from_str::<SessionId>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn element_refs_round_trip() {
        assert_eq!(parse_element_ref(&format_element_ref(42)), Some(42));
        for bad in ["e", "e0", "e042", "42", "e-1", "e+1", "w1", "e1x"] {
            assert_eq!(parse_element_ref(bad), None, "{bad}");
        }
    }
}
