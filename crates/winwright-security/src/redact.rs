use std::fmt;

use winwright_contracts::backend::UiProps;
use winwright_contracts::element::{ControlRole, UiPattern};

pub const REDACTED: &str = "[REDACTED]";

/// Name/AutomationId fragments that mark a field as secret even when the provider does not set
/// `IsPassword`. Over-redaction is the safe failure mode.
const SENSITIVE_HINTS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "passcode",
    "pin code",
    "secret",
    "cvv",
    "cvc",
    "otp",
    "one-time code",
    "security code",
    "api key",
    "apikey",
    "token",
];

pub fn is_sensitive(props: &UiProps) -> bool {
    if props.is_password {
        return true;
    }
    let lower_name = props.name.to_lowercase();
    let lower_id = props.automation_id.to_lowercase();
    if SENSITIVE_HINTS
        .iter()
        .any(|hint| lower_name.contains(hint) || lower_id.contains(hint))
    {
        return true;
    }
    // A bare "PIN" (Windows Hello, banking) is a secret on an input; on a button it is a verb.
    let input = props.role == ControlRole::Edit || props.has_pattern(UiPattern::Value);
    input
        && lower_name
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| w == "pin")
}

/// The value the model may see for an element.
pub fn redacted_value(props: &UiProps) -> Option<String> {
    if is_sensitive(props) {
        props
            .has_pattern(winwright_contracts::element::UiPattern::Value)
            .then(|| REDACTED.to_owned())
    } else {
        props.value.clone()
    }
}

/// Log-safe summary of typed text: `chars=16 sensitive=true`, never the text itself.
pub fn summarize_text(text: &str, sensitive: bool) -> String {
    format!("chars={} sensitive={sensitive}", text.chars().count())
}

/// A secret the model must never see. Not `Serialize`, not `Display`, and `Debug` is redacted.
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretString({REDACTED})")
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        // Best-effort wipe of the heap buffer before it is freed.
        // SAFETY: zero bytes are valid UTF-8, and `self.0` is not used after this.
        unsafe { self.0.as_bytes_mut() }.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winwright_contracts::element::{ControlRole, UiPattern};

    fn edit(name: &str, is_password: bool, value: Option<&str>) -> UiProps {
        UiProps {
            role: ControlRole::Edit,
            name: name.into(),
            is_password,
            patterns: vec![UiPattern::Value],
            value: value.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn password_values_are_redacted() {
        let p = edit("Password", true, Some("hunter2"));
        assert!(is_sensitive(&p));
        assert_eq!(redacted_value(&p).as_deref(), Some(REDACTED));
    }

    #[test]
    fn name_heuristics_catch_unflagged_secrets() {
        for name in ["Enter PIN code", "API key", "One-time code", "CVV"] {
            assert!(is_sensitive(&edit(name, false, Some("x"))), "{name}");
        }
        let plain = edit("File name", false, Some("report.docx"));
        assert!(!is_sensitive(&plain));
        assert_eq!(redacted_value(&plain).as_deref(), Some("report.docx"));
    }

    #[test]
    fn pins_and_abbreviated_passwords_are_sensitive() {
        assert!(is_sensitive(&edit("PIN", false, Some("1234"))));
        assert!(is_sensitive(&edit("Passphrase", false, None)));
        let mut by_id = edit("Code", false, Some("x"));
        by_id.automation_id = "txtPwd".into();
        assert!(is_sensitive(&by_id));
        let pin_to_start = UiProps {
            role: ControlRole::Button,
            name: "Pin to Start".into(),
            ..Default::default()
        };
        assert!(!is_sensitive(&pin_to_start), "a verb, not a field");
        assert!(!is_sensitive(&edit("Shipping address", false, None)));
    }

    #[test]
    fn secrets_never_print() {
        let s = SecretString::new("hunter2".into());
        assert_eq!(format!("{s:?}"), "SecretString([REDACTED])");
        assert_eq!(s.expose(), "hunter2");
        assert_eq!(summarize_text(s.expose(), true), "chars=7 sensitive=true");
    }
}
