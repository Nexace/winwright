//! Risk follows the intended effect and the resolved target (spec §66): clicking a button
//! named "Send" or "Delete" is not an ordinary click.

use winwright_contracts::security::ActionRisk;

/// Whole-word phrases (lower-case, space separated) that make activating a control destructive.
const DESTRUCTIVE: &[&str] = &[
    "delete",
    "delete all",
    "remove",
    "erase",
    "wipe",
    "uninstall",
    "format disk",
    "format drive",
    "quick format",
    "discard",
    "empty recycle bin",
    "permanently",
    "factory reset",
    "reset",
    "end task",
    "terminate",
    "kill",
    "overwrite",
    "replace the file",
    "replace the files",
];

/// Phrases that send, publish, spend, or install.
const SENSITIVE: &[&str] = &[
    "send",
    "submit",
    "publish",
    "post",
    "reply all",
    "buy",
    "buy now",
    "purchase",
    "pay",
    "pay now",
    "checkout",
    "check out",
    "place order",
    "confirm order",
    "confirm payment",
    "order now",
    "transfer",
    "donate",
    "subscribe",
    "install",
    "run as administrator",
    "grant",
    "allow",
    "authorize",
    "accept",
    "agree",
    "i agree",
    "change password",
    "turn off protection",
];

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn contains_phrase(haystack: &[String], phrase: &str) -> bool {
    let needle: Vec<&str> = phrase.split(' ').collect();
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|w| w.iter().zip(&needle).all(|(a, b)| a == b))
}

/// Risk of activating (clicking/invoking/pressing Enter on) a control with this name.
/// AutomationIds are checked too because icon-only buttons often carry the verb there
/// (`btnDelete`), split on camel case.
pub fn classify_activation(name: &str, automation_id: &str) -> ActionRisk {
    let mut tokens = words(name);
    tokens.extend(words(&split_camel(automation_id)));
    if DESTRUCTIVE.iter().any(|p| contains_phrase(&tokens, p)) {
        ActionRisk::Destructive
    } else if SENSITIVE.iter().any(|p| contains_phrase(&tokens, p)) {
        ActionRisk::Sensitive
    } else {
        ActionRisk::Normal
    }
}

fn split_camel(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 8);
    let mut prev_lower = false;
    for c in id.chars() {
        if c.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_on_buttons() {
        assert_eq!(classify_activation("Delete", ""), ActionRisk::Destructive);
        assert_eq!(
            classify_activation("Empty Recycle Bin", ""),
            ActionRisk::Destructive
        );
        assert_eq!(classify_activation("Send", ""), ActionRisk::Sensitive);
        assert_eq!(
            classify_activation("Place order", ""),
            ActionRisk::Sensitive
        );
        assert_eq!(classify_activation("Submit", ""), ActionRisk::Sensitive);
        assert_eq!(
            classify_activation("", "btnDelete"),
            ActionRisk::Destructive
        );
        assert_eq!(classify_activation("", "SendButton"), ActionRisk::Sensitive);
    }

    #[test]
    fn ordinary_controls_stay_normal() {
        for name in [
            "Save",
            "Save As",
            "Open",
            "Cancel",
            "OK",
            "File name:",
            "Sender",
            "Posted by",
            "Postcode",
            "Format Painter",
            "Find and Replace",
            "Sign in",
            "Paste",
            "Settings",
            "Target",
        ] {
            assert_eq!(classify_activation(name, ""), ActionRisk::Normal, "{name}");
        }
        assert_eq!(
            classify_activation("Quick format", ""),
            ActionRisk::Destructive
        );
        assert_eq!(
            classify_activation("Replace the file in the destination", ""),
            ActionRisk::Destructive
        );
    }

    #[test]
    fn camel_case_split() {
        assert_eq!(split_camel("btnSaveAs"), "btn Save As");
        assert_eq!(split_camel("ID_DELETE"), "ID_DELETE");
        assert_eq!(
            classify_activation("", "ID_DELETE"),
            ActionRisk::Destructive
        );
    }
}
