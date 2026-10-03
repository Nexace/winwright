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
