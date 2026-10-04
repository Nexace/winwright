//! Risk follows the intended effect and the resolved target (spec §66): clicking a button
//! named "Send" or "Delete" is not an ordinary click.

use std::path::{Component, Path, Prefix};

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

/// Phrases that spend money or change security. Judged with the destructive ones, so even
/// relaxed mode asks before them.
const SPEND_OR_SECURITY: &[&str] = &[
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
    "upgrade",
    "redeem",
    "place bid",
    "book now",
    "install",
    "run as administrator",
    "grant",
    "allow",
    "authorize",
    "approve",
    "change password",
    "turn off protection",
    "deploy",
];

/// Phrases that send, publish, share, or agree.
const SENSITIVE: &[&str] = &[
    "send",
    "submit",
    "publish",
    "post",
    "reply all",
    "accept",
    "agree",
    "i agree",
    "share",
    "forward",
    "invite",
    "upload",
    "unsubscribe",
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
    if DESTRUCTIVE
        .iter()
        .chain(SPEND_OR_SECURITY)
        .any(|p| contains_phrase(&tokens, p))
    {
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

/// Folders whose contents are secrets wherever they are copied: browser profiles and app
/// tokens (AppData), SSH, cloud and cluster credentials.
const SECRET_FOLDERS: &[&str] = &[
    "appdata", ".ssh", ".aws", ".azure", ".gnupg", ".kube", ".docker",
];
/// File names that hold credentials.
const SECRET_FILES: &[&str] = &[
    ".env",
    ".git-credentials",
    ".netrc",
    "_netrc",
    ".npmrc",
    ".pypirc",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "login data",
    "cookies",
    "logins.json",
    "key4.db",
];
/// Key stores and password databases.
const SECRET_EXTENSIONS: &[&str] = &[
    "kdbx", "kdb", "pem", "key", "pfx", "p12", "ppk", "jks", "keystore", "ovpn", "gpg",
];

/// Risk of copying `from` to `to`. A copy to or from a share or another volume (a USB stick,
/// a mapped drive) can carry data off the machine; whole drives and profiles, and secrets
/// (keys, password databases, browser and app data), need a person's yes wherever they go.
pub fn transfer_risk(from: &Path, to: &Path) -> ActionRisk {
    let same_volume = volume(from).is_some_and(|v| volume(to) == Some(v));
    let parts: Vec<String> = from
        .components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    let file = parts.last().map(String::as_str).unwrap_or_default();
    let extension = file
        .rsplit_once('.')
        .map(|(_, ext)| ext)
        .unwrap_or_default();
    let secret = parts.iter().any(|p| SECRET_FOLDERS.contains(&p.as_str()))
        || SECRET_FILES.contains(&file)
        || file.starts_with(".env.")
        || SECRET_EXTENSIONS.contains(&extension);
    if !same_volume || secret || parts.len() <= 2 {
        ActionRisk::Sensitive
    } else {
        ActionRisk::Normal
    }
}

/// The drive letter of a local drive path; `None` for shares, device paths, and the rest.
fn volume(path: &Path) -> Option<char> {
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Some(char::from(letter).to_ascii_uppercase())
            }
            _ => None,
        },
        _ => None,
    }
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

/// The stricter of two capabilities, ranking PowerShell above other shells above the rest.
pub fn stricter(a: Capability, b: Capability) -> Capability {
    let rank = |c: Capability| match c {
        Capability::PowerShell => 2,
        Capability::Shell => 1,
        _ => 0,
    };
    if rank(b) > rank(a) { b } else { a }
}

/// Capability a typed command line needs when Enter runs it (the Run box, Start search, the
/// Explorer address bar). `cmd/c ...` counts as `cmd` too.
pub fn command_capability(command: &str) -> Capability {
    let command = command.trim_start();
    let program = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default(),
        None => command.split_whitespace().next().unwrap_or_default(),
    };
    let before_switch = program.split('/').next().unwrap_or_default();
    stricter(
        program_capability(program),
        program_capability(before_switch),
    )
}

/// Terminal windows: whatever shell they host, Enter in them runs a command.
const TERMINALS: &[&str] = &[
    "alacritty",
    "conemu",
    "conemu64",
    "conhost",
    "mintty",
    "openconsole",
    "tabby",
    "wezterm-gui",
    "windowsterminal",
    "wt",
];

/// Where typed text plus Enter starts a program: the Run box and the address bar (Explorer),
/// Start search, and Task Manager's "Run new task".
const LAUNCHERS: &[&str] = &[
    "explorer",
    "searchapp",
    "searchhost",
    "searchui",
    "startmenuexperiencehost",
    "taskmgr",
];

/// Start search: its results run what was typed ("Run command").
const START_SEARCH: &[&str] = &[
    "searchapp",
    "searchhost",
    "searchui",
    "startmenuexperiencehost",
];

/// Scripts that run when opened (Explorer's default action): batch files and Windows Script
/// Host and HTML Application scripts. `.ps1` opens in an editor by default.
const SCRIPT_EXTENSIONS: &[&str] = &["bat", "cmd", "hta", "js", "jse", "vbe", "vbs", "wsf", "wsh"];

fn process_stem(process: &str) -> String {
    let name = process.trim().to_ascii_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_owned()
}

/// Capability that pressing Enter in a window of `process` needs, when that runs a command
/// line: shells and terminals. A terminal may host any shell, so it counts as PowerShell.
pub fn console_capability(process: &str) -> Option<Capability> {
    match process_stem(process).as_str() {
        s if TERMINALS.contains(&s) || POWERSHELLS.contains(&s) => Some(Capability::PowerShell),
        "cmd" | "bash" | "wsl" => Some(Capability::Shell),
        _ => None,
    }
}

/// Whether text typed into `process`'s fields can be run as a command by Enter.
pub fn is_launcher(process: &str) -> bool {
    LAUNCHERS.contains(&process_stem(process).as_str())
}

/// Whether a text field is a terminal inside another app: xterm.js (VS Code, Cursor, terminals
/// in web pages) or a field named as one ("Terminal 1, pwsh").
pub fn is_terminal_field(name: &str, class_name: &str) -> bool {
    class_name.to_ascii_lowercase().contains("xterm")
        || words(name).first().is_some_and(|w| w == "terminal")
}

/// Capability that activating an item named `name` in `process` needs when that runs a
/// command or script: a Start search result for a typed shell command ("cmd /c ..., Run
/// command"), or a script file in Explorer. Opening a shell's own window runs nothing yet.
pub fn opened_capability(process: &str, name: &str) -> Option<Capability> {
    let stem = process_stem(process);
    if START_SEARCH.contains(&stem.as_str()) {
        let command = name.split(',').next().unwrap_or_default();
        let capability = command_capability(command);
        if matches!(capability, Capability::Shell | Capability::PowerShell) {
            return Some(capability);
        }
    }
    let script = name
        .trim()
        .trim_end_matches(['.', ' '])
        .rsplit_once('.')
        .is_some_and(|(_, ext)| SCRIPT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
    (stem == "explorer" && script).then_some(Capability::Shell)
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
        // Spending money and changing security are judged like deleting: relaxed mode asks.
        for name in [
            "Place order",
            "Pay now",
            "Install",
            "Turn off protection",
            "Allow",
        ] {
            assert_eq!(
                classify_activation(name, ""),
                ActionRisk::Destructive,
                "{name}"
            );
        }
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
            ("Sell", "", ActionRisk::Destructive),
            ("Withdraw funds", "", ActionRisk::Destructive),
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
    fn typed_command_lines_name_their_program() {
        for (line, want) in [
            ("powershell -c Remove-Item x", Capability::PowerShell),
            ("  pwsh.exe", Capability::PowerShell),
            (
                "\"C:\\Windows\\System32\\cmd.exe\" /c del x",
                Capability::Shell,
            ),
            ("cmd/c del x", Capability::Shell),
            ("notepad", Capability::ProcessLaunch),
            ("C:\\Users\\Ann\\Documents", Capability::ProcessLaunch),
            ("", Capability::ProcessLaunch),
        ] {
            assert_eq!(command_capability(line), want, "{line:?}");
        }
    }

    #[test]
    fn terminals_and_launchers_are_recognized() {
        assert_eq!(
            console_capability("WindowsTerminal.exe"),
            Some(Capability::PowerShell)
        );
        assert_eq!(console_capability("cmd.exe"), Some(Capability::Shell));
        assert_eq!(
            console_capability("powershell.exe"),
            Some(Capability::PowerShell)
        );
        assert_eq!(console_capability("notepad.exe"), None);
        assert!(is_launcher("explorer.exe") && is_launcher("SearchHost.exe"));
        assert!(is_launcher("Taskmgr.exe"));
        assert!(!is_launcher("notepad.exe"));
    }

    #[test]
    fn terminals_inside_other_apps_are_recognized() {
        assert!(is_terminal_field(
            "Terminal 1, pwsh",
            "xterm-helper-textarea"
        ));
        assert!(is_terminal_field("", "xterm-helper-textarea"));
        assert!(is_terminal_field("Terminal", ""));
        assert!(!is_terminal_field("Search terminals", "TextBox"));
        assert!(!is_terminal_field("Text Editor", "Edit"));
    }

    #[test]
    fn opening_a_command_or_script_is_shell_execution() {
        for (process, name, want) in [
            (
                "SearchHost.exe",
                "powershell -c Remove-Item x, Run command",
                Some(Capability::PowerShell),
            ),
            ("SearchHost.exe", "cmd /c del x", Some(Capability::Shell)),
            ("explorer.exe", "cleanup.BAT", Some(Capability::Shell)),
            ("explorer.exe", "setup.vbs. ", Some(Capability::Shell)),
            // Opening a shell's window, or an ordinary file, runs nothing yet.
            ("SearchHost.exe", "Windows PowerShell, App", None),
            ("SearchHost.exe", "Notepad, App", None),
            ("explorer.exe", "Terminal", None),
            ("explorer.exe", "notes.txt", None),
            ("explorer.exe", "run.ps1", None),
            // A script name elsewhere is just text.
            ("notepad.exe", "cleanup.bat", None),
        ] {
            assert_eq!(opened_capability(process, name), want, "{process} {name:?}");
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
            ("Approve", ActionRisk::Destructive),
            ("Upload files", ActionRisk::Sensitive),
            ("Upgrade to Pro", ActionRisk::Destructive),
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
    fn copies_that_can_carry_data_away_need_a_yes() {
        let risk = |from: &str, to: &str| transfer_risk(Path::new(from), Path::new(to));
        for (from, to) in [
            (
                r"C:\Users\me\Documents\report.docx",
                r"\\server\share\r.docx",
            ),
            (
                r"\\server\share\tool.zip",
                r"C:\Users\me\Downloads\tool.zip",
            ),
            (r"C:\Users\me\Documents\report.docx", r"E:\report.docx"),
            (r"C:\Users\me\.ssh\id_ed25519", r"C:\Users\me\Desktop\k"),
            (
                r"C:\Users\me\AppData\Local\Google\Chrome\User Data\Default\Login Data",
                r"C:\Users\me\Desktop\x",
            ),
            (
                r"C:\Users\me\Documents\vault.KDBX",
                r"C:\Users\me\Desktop\v.kdbx",
            ),
            (
                r"C:\Users\me\code\app\.env.local",
                r"C:\Users\me\Desktop\env",
            ),
            (r"C:\Users\me", r"C:\backup\me"),
        ] {
            assert_eq!(risk(from, to), ActionRisk::Sensitive, "{from} -> {to}");
        }
        assert_eq!(
            risk(
                r"C:\Users\me\Documents\report.docx",
                r"c:\Users\me\Desktop\report.docx"
            ),
            ActionRisk::Normal
        );
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
