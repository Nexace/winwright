//! Risk follows the intended effect and the resolved target (spec §66): clicking a button
//! named "Send" or "Delete" is not an ordinary click.

use winwright_contracts::security::{ActionRisk, Capability};

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
    "move to recycle bin",
    "move to trash",
    "permanently",
    "factory reset",
    "reset",
    "end task",
    "terminate",
    "kill",
    "overwrite",
    "replace the file",
    "replace the files",
    "clear all",
    "clear history",
    "clear data",
    "clear browsing data",
    "purge",
    "destroy",
    "shred",
    "revoke",
    "deactivate",
    "cancel subscription",
    "unpublish",
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
    "sell",
    "withdraw",
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
    "share",
    "forward",
    "invite",
    "approve",
    "upload",
    "deploy",
    "unsubscribe",
    "upgrade",
    "redeem",
    "place bid",
    "book now",
];

/// Buttons that agree to whatever their dialog asks: judged by the dialog's text.
const AFFIRMATIVE: &[&str] = &[
    "yes",
    "ok",
    "okay",
    "continue",
    "confirm",
    "proceed",
    "go ahead",
    "i understand",
    "apply",
    "finish",
];

/// Text as a person reads it: invisible format characters (zero-width spaces, direction marks,
/// soft hyphens, variation selectors, tags, Hangul fillers) dropped and fullwidth ASCII folded,
/// so `De\u{200B}lete` and `Ｄｅｌｅｔｅ` still say "Delete".
pub(crate) fn normalize(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{E0000}'..='\u{E0FFF}' => None,
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0),
            '\u{3000}' => Some(' '),
            _ => Some(c),
        })
        .collect()
}

fn words(text: &str) -> Vec<String> {
    normalize(text)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// A button that says yes to its dialog ("Yes", "OK", "Continue"), so the dialog's own text
/// says what clicking it does.
pub fn is_affirmative(name: &str) -> bool {
    let tokens = words(name);
    tokens.len() <= 4 && AFFIRMATIVE.iter().any(|p| contains_phrase(&tokens, p))
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

/// Words naming a field people write messages in, where Enter usually sends.
const COMPOSER: &[&str] = &["message", "messages", "reply", "comment", "chat", "compose"];

/// Risk of pressing Enter in a text field with this name: Enter submits the field's form, and
/// in a message box it sends the message.
pub fn classify_submit(name: &str, automation_id: &str) -> ActionRisk {
    let mut tokens = words(name);
    tokens.extend(words(&split_camel(automation_id)));
    let risk = classify_activation(name, automation_id);
    if COMPOSER.iter().any(|p| contains_phrase(&tokens, p)) {
        risk.max(ActionRisk::Sensitive)
    } else {
        risk
    }
}

/// `btnSaveAs` -> `btn Save As`; an acronym ends before its last capital (`IDDelete` ->
/// `ID Delete`).
fn split_camel(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    let mut out = String::with_capacity(id.len() + 8);
    let mut prev_lower = false;
    for (i, &c) in chars.iter().enumerate() {
        let prev_upper = i > 0 && chars[i - 1].is_uppercase();
        let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
        if c.is_uppercase() && (prev_lower || (prev_upper && next_lower)) {
            out.push(' ');
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        out.push(c);
    }
    out
}

/// Programs that run whatever command or script their arguments name.
const SHELLS: &[&str] = &[
    "bash",
    "bitsadmin",
    "certutil",
    "cmd",
    "cmstp",
    "conhost",
    "cscript",
    "forfiles",
    "hh",
    "installutil",
    "msbuild",
    "mshta",
    "msiexec",
    "node",
    "py",
    "python",
    "python3",
    "pythonw",
    "reg",
    "regasm",
    "regsvcs",
    "regsvr32",
    "rundll32",
    "sc",
    "schtasks",
    // Winwright's own CLI can clear the audit log and drive the desktop without this
    // session's confirmations, so starting it is shell execution too.
    "winwright",
    "wmic",
    "wscript",
    "wsl",
    "wt",
];
const POWERSHELLS: &[&str] = &["powershell", "powershell_ise", "pwsh"];

/// Capability needed to start `program` (a name, path, or URI): interpreters and script hosts
/// are shell execution whichever tool starts them; anything else is an ordinary launch.
pub fn program_capability(program: &str) -> Capability {
    let name = program
        .trim()
        .trim_matches('"')
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(['.', ' '])
        .to_ascii_lowercase();
    let stem = name
        .strip_suffix(".exe")
        .or_else(|| name.strip_suffix(".com"))
        .unwrap_or(&name);
    if POWERSHELLS.contains(&stem) {
        Capability::PowerShell
    } else if SHELLS.contains(&stem) {
        Capability::Shell
    } else {
        Capability::ProcessLaunch
    }
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
    fn recycle_bin_and_money_verbs_are_caught() {
        for (name, id, risk) in [
            ("Move to Recycle Bin", "", ActionRisk::Destructive),
            ("Move to Trash", "", ActionRisk::Destructive),
            ("Sell", "", ActionRisk::Sensitive),
            ("Withdraw funds", "", ActionRisk::Sensitive),
            ("", "IDDelete", ActionRisk::Destructive),
            ("", "btnOKSend", ActionRisk::Sensitive),
            ("Undelete", "", ActionRisk::Normal),
            ("Bestseller", "", ActionRisk::Normal),
            ("Trash can", "", ActionRisk::Normal),
        ] {
            assert_eq!(classify_activation(name, id), risk, "{name:?} {id:?}");
        }
    }

    #[test]
    fn interpreters_are_shell_execution() {
        for program in [
            "cmd",
            "CMD.EXE",
            r"C:\Windows\System32\cmd.exe",
            "mshta.exe",
            "wscript",
            "rundll32.exe",
            "wt",
            r"C:\tools\winwright.exe",
        ] {
            assert_eq!(program_capability(program), Capability::Shell, "{program}");
        }
        for program in [
            "powershell",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            "pwsh.exe",
        ] {
            assert_eq!(
                program_capability(program),
                Capability::PowerShell,
                "{program}"
            );
        }
        for program in [
            "notepad.exe",
            "ms-settings:display",
            r"C:\Users\a\Documents",
            "cmdlet.exe",
        ] {
            assert_eq!(
                program_capability(program),
                Capability::ProcessLaunch,
                "{program}"
            );
        }
    }

    #[test]
    fn invisible_and_fullwidth_characters_do_not_hide_a_verb() {
        for name in [
            "De\u{200B}lete",
            "Del\u{00AD}ete",
            "\u{202E}Delete",
            "Dele\u{2060}te all",
            "De\u{3164}lete",
            "Ｄｅｌｅｔｅ",
            "S\u{FE0F}end",
        ] {
            assert_ne!(
                classify_activation(name, ""),
                ActionRisk::Normal,
                "{name:?}"
            );
        }
    }

    #[test]
    fn more_verbs_are_caught() {
        for (name, risk) in [
            ("Clear browsing data", ActionRisk::Destructive),
            ("Revoke access", ActionRisk::Destructive),
            ("Deactivate account", ActionRisk::Destructive),
            ("Cancel subscription", ActionRisk::Destructive),
            ("Share", ActionRisk::Sensitive),
            ("Forward", ActionRisk::Sensitive),
            ("Invite people", ActionRisk::Sensitive),
            ("Approve", ActionRisk::Sensitive),
            ("Upload files", ActionRisk::Sensitive),
            ("Upgrade to Pro", ActionRisk::Sensitive),
            ("Clear", ActionRisk::Normal),
            ("Shared with me", ActionRisk::Normal),
            ("Merge cells", ActionRisk::Normal),
        ] {
            assert_eq!(classify_activation(name, ""), risk, "{name}");
        }
    }

    #[test]
    fn affirmative_buttons_are_recognised() {
        for name in [
            "Yes",
            "OK",
            "Continue",
            "Yes to all",
            "I understand",
            "Finish",
        ] {
            assert!(is_affirmative(name), "{name}");
        }
        for name in ["No", "Cancel", "Save", "Okay so here is a long sentence"] {
            assert!(!is_affirmative(name), "{name}");
        }
    }

    #[test]
    fn enter_in_a_message_box_sends() {
        for (name, id) in [
            ("Type a message", ""),
            ("Reply", ""),
            ("Add a comment", ""),
            ("", "chatInput"),
            ("", "composeBox"),
        ] {
            assert_eq!(
                classify_submit(name, id),
                ActionRisk::Sensitive,
                "{name:?} {id:?}"
            );
        }
        for name in ["Search", "Address and search bar", "Name:", "Text editor"] {
            assert_eq!(classify_submit(name, ""), ActionRisk::Normal, "{name}");
        }
        assert_eq!(
            classify_submit("Type DELETE to confirm", ""),
            ActionRisk::Destructive
        );
    }

    #[test]
    fn camel_case_split() {
        assert_eq!(split_camel("btnSaveAs"), "btn Save As");
        assert_eq!(split_camel("ID_DELETE"), "ID_DELETE");
        assert_eq!(split_camel("IDDelete"), "ID Delete");
        assert_eq!(
            classify_activation("", "ID_DELETE"),
            ActionRisk::Destructive
        );
    }
}
