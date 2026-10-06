//! `winwright setup`: one command after downloading. Puts winwright.exe in a folder of its own
//! and registers it in every AI app found on this PC (each config backed up first; a config it
//! cannot read safely is left alone and the snippet printed instead). `--remove` undoes it.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use winwright_contracts::WinwrightError;

const NAME: &str = "winwright";
const CODEX_HEADER: &str = "[mcp_servers.winwright]";

/// How an app's JSON config lists MCP servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavor {
    /// `{"mcpServers": {"winwright": {"command": ..., "args": ["mcp"]}}}`
    McpServers,
    /// opencode: `mcp.<name>` (or `mcp.servers.<name>` where the config already nests them).
    Opencode,
}

fn entry(flavor: Flavor, exe: &str) -> Value {
    match flavor {
        Flavor::McpServers => json!({ "command": exe, "args": ["mcp"] }),
        Flavor::Opencode => json!({ "type": "local", "command": [exe, "mcp"], "enabled": true }),
    }
}

/// The object that holds the servers, created when missing.
fn servers(root: &mut Map<String, Value>, flavor: Flavor) -> Option<&mut Map<String, Value>> {
    let key = match flavor {
        Flavor::McpServers => "mcpServers",
        Flavor::Opencode => "mcp",
    };
    let mut table = root
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()?;
    if flavor == Flavor::Opencode && table.get("servers").is_some_and(Value::is_object) {
        table = table.get_mut("servers")?.as_object_mut()?;
    }
    Some(table)
}

/// Whether an existing entry starts `exe` (`command` as a string, or opencode's array).
fn runs(entry: &Value, exe: &str) -> bool {
    let command = &entry["command"];
    let first = command
        .as_array()
        .and_then(|a| a.first())
        .unwrap_or(command);
    first.as_str().is_some_and(|c| c.eq_ignore_ascii_case(exe))
}

#[derive(Debug, PartialEq, Eq)]
enum Edit {
    Unchanged,
    Write(String),
}

/// The config with Winwright registered (or removed). Errors leave the file alone.
fn edit_json(text: &str, flavor: Flavor, exe: Option<&str>) -> Result<Edit, String> {
    let mut root: Value = if text.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(text)
            .map_err(|e| format!("cannot read it safely ({e}); it may hold comments"))?
    };
    let root_map = root
        .as_object_mut()
        .ok_or("its top level is not a JSON object")?;
    let table = servers(root_map, flavor).ok_or("its server list is not a JSON object")?;
    match exe {
        Some(exe) => {
            if table.get(NAME).is_some_and(|e| runs(e, exe)) {
                // Already this exe: whatever else the person set there stays.
                return Ok(Edit::Unchanged);
            }
            table.insert(NAME.to_owned(), entry(flavor, exe));
        }
        None => {
            if table.remove(NAME).is_none() {
                return Ok(Edit::Unchanged);
            }
        }
    }
    let mut out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(Edit::Write(out))
}

fn codex_section(exe: &str) -> String {
    format!(
        "{CODEX_HEADER}\ncommand = '{exe}'\nargs = [\"mcp\"]\n# The Allow/Deny dialog waits up to 45 s.\n\
         tool_timeout_sec = 120\nenv_vars = [\"APPDATA\", \"LOCALAPPDATA\", \"USERPROFILE\", \
         \"SystemRoot\", \"WINWRIGHT_NOTION_TOKEN\", \"WINWRIGHT_NOTION_PARENT\"]\n"
    )
}

/// `[mcp_servers.winwright]`, with any spacing or a trailing comment.
fn is_codex_header(line: &str) -> bool {
    let line = line.split('#').next().unwrap_or_default().trim();
    let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) else {
        return false;
    };
    let parts: Vec<String> = inner
        .split('.')
        .map(|p| p.trim().trim_matches('"').trim_matches('\'').to_owned())
        .collect();
    parts == ["mcp_servers", "winwright"]
}

/// Line ranges of the Winwright tables (`[mcp_servers.winwright]` and its sub-tables).
fn codex_block(text: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.iter().position(|l| is_codex_header(l))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| {
            let t = l.trim_start();
            t.starts_with('[') && !t.starts_with("[mcp_servers.winwright.")
        })
        .map_or(lines.len(), |i| start + 1 + i);
    let offset = |n: usize| lines[..n].iter().map(|l| l.len()).sum::<usize>();
    Some((offset(start), offset(end)))
}

/// Codex's TOML: Winwright's tables are added, replaced or removed as text, so the rest of the
/// file stays exactly as it was.
fn edit_codex(text: &str, exe: Option<&str>) -> Result<Edit, String> {
    let block = codex_block(text);
    match (exe, block) {
        (Some(exe), _) if exe.contains('\'') => {
            Err("the path holds a ' and cannot be written as a TOML literal".into())
        }
        (Some(exe), Some((a, b))) => {
            let section = codex_section(exe);
            let current = &text[a..b];
            let same_exe = current.lines().any(|l| {
                let l = l.trim().to_ascii_lowercase();
                let exe = exe.to_ascii_lowercase();
                l == format!("command = '{exe}'")
                    || l == format!("command = \"{}\"", exe.replace('\\', "\\\\"))
            });
            if same_exe {
                // Already this exe: whatever else the person set there stays.
                return Ok(Edit::Unchanged);
            }
            let tail = if b < text.len() { "\n" } else { "" };
            Ok(Edit::Write(format!(
                "{}{section}{tail}{}",
                &text[..a],
                &text[b..]
            )))
        }
        // Written in a form not parsed here (dotted keys, an inline table): a second table would
        // make the whole file invalid, so leave it to the person.
        (Some(_), None) if text.contains("winwright") && text.contains("mcp_servers") => {
            Err("it already mentions winwright in a form setup cannot edit safely".into())
        }
        (Some(exe), None) => {
            let mut out = text.to_owned();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&codex_section(exe));
            Ok(Edit::Write(out))
        }
        (None, Some((a, b))) => Ok(Edit::Write(format!(
            "{}{}",
            text[..a].trim_end_matches('\n').to_owned() + if a > 0 { "\n" } else { "" },
            &text[b..]
        ))),
        (None, None) => Ok(Edit::Unchanged),
    }
}

enum Format {
    Json(Flavor),
    Codex,
}

struct App {
    name: &'static str,
    /// The config file; the app counts as installed when its folder exists.
    config: PathBuf,
    format: Format,
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn apps() -> Vec<App> {
    let home = home();
    let mut apps = Vec::new();
    let mut claude_desktop = Vec::new();
    if let Some(appdata) = env_dir("APPDATA") {
        claude_desktop.push(appdata.join("Claude"));
    }
    // The Microsoft Store build keeps its config inside its package.
    if let Some(local) = env_dir("LOCALAPPDATA")
        && let Ok(packages) = std::fs::read_dir(local.join("Packages"))
    {
        for p in packages.flatten() {
            if p.file_name().to_string_lossy().starts_with("Claude_") {
                claude_desktop.push(p.path().join("LocalCache").join("Roaming").join("Claude"));
            }
        }
    }
    for dir in claude_desktop {
        apps.push(App {
            name: "Claude Desktop",
            config: dir.join("claude_desktop_config.json"),
            format: Format::Json(Flavor::McpServers),
        });
    }
    apps.push(App {
        name: "Cursor",
        config: home.join(".cursor").join("mcp.json"),
        format: Format::Json(Flavor::McpServers),
    });
    apps.push(App {
        name: "Windsurf",
        config: home
            .join(".codeium")
            .join("windsurf")
            .join("mcp_config.json"),
        format: Format::Json(Flavor::McpServers),
    });
    apps.push(App {
        name: "Antigravity",
        config: home.join(".gemini").join("config").join("mcp_config.json"),
        format: Format::Json(Flavor::McpServers),
    });
    apps.push(App {
        name: "opencode",
        config: home.join(".config").join("opencode").join("opencode.json"),
        format: Format::Json(Flavor::Opencode),
    });
    apps.push(App {
        name: "Codex",
        config: home.join(".codex").join("config.toml"),
        format: Format::Codex,
    });
    apps
}

fn snippet(format: &Format, exe: &str) -> String {
    match format {
        Format::Json(flavor) => format!("\"{NAME}\": {}", entry(*flavor, exe)),
        Format::Codex => codex_section(exe),
    }
}

/// Writes through a temporary file after keeping the old one as `<name>.winwright-backup`.
fn write_config(path: &Path, text: &str) -> std::io::Result<()> {
    if path.exists() {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".winwright-backup");
        let backup = PathBuf::from(backup);
        // The first backup is the person's own file: later runs must not replace it.
        if !backup.exists() {
            std::fs::copy(path, backup)?;
        }
    } else if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".winwright-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Where winwright.exe lives once installed: next to cargo's own binaries for a source
/// install, else a folder of its own (file operations are refused in the exe's folder, so it
/// must not stay in Downloads).
fn install(dry_run: bool) -> Result<PathBuf, WinwrightError> {
    let current = std::env::current_exe()
        .map_err(|e| WinwrightError::invalid(format!("cannot find winwright.exe: {e}")))?;
    let cargo_bin = home().join(".cargo").join("bin");
    if current.parent() == Some(cargo_bin.as_path()) {
        return Ok(current);
    }
    let dir = env_dir("LOCALAPPDATA")
        .ok_or_else(|| WinwrightError::invalid("LOCALAPPDATA is not set"))?
        .join("Programs")
        .join("Winwright");
    let target = dir.join("winwright.exe");
    if current == target {
        return Ok(target);
    }
    println!(
        "{} {}",
        if dry_run {
            "Would install to"
        } else {
            "Installing to"
        },
        target.display()
    );
    if !dry_run {
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::copy(&current, &target).map(drop))
            .map_err(|e| {
                WinwrightError::invalid(format!(
                    "cannot copy to {} ({e}); close the AI apps that use Winwright and run setup again",
                    target.display()
                ))
            })?;
    }
    Ok(target)
}

pub fn run(dry_run: bool, remove: bool) -> Result<(), WinwrightError> {
    let exe = if remove {
        None
    } else {
        Some(install(dry_run)?.display().to_string())
    };
    let mut found = 0;
    for app in apps() {
        let Some(dir) = app.config.parent() else {
            continue;
        };
        if !dir.exists() {
            continue;
        }
        found += 1;
        let text = match std::fs::read_to_string(&app.config) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                println!("  {}: cannot read {} ({e})", app.name, app.config.display());
                continue;
            }
        };
        let edit = match &app.format {
            Format::Json(flavor) => edit_json(&text, *flavor, exe.as_deref()),
            Format::Codex => edit_codex(&text, exe.as_deref()),
        };
        let path = app.config.display();
        match edit {
            Ok(Edit::Unchanged) => println!(
                "  {}: {}",
                app.name,
                if remove {
                    "not registered"
                } else {
                    "already set up"
                }
            ),
            Ok(Edit::Write(new)) if dry_run => {
                println!("  {}: would update {path}", app.name);
                drop(new);
            }
            Ok(Edit::Write(new)) => match write_config(&app.config, &new) {
                Ok(()) => println!(
                    "  {}: {} {path} (backup: .winwright-backup); restart the app",
                    app.name,
                    if remove {
                        "removed from"
                    } else {
                        "registered in"
                    }
                ),
                Err(e) => println!("  {}: cannot write {path} ({e})", app.name),
            },
            Err(why) => {
                println!("  {}: left {path} alone: {why}", app.name);
                if let Some(exe) = &exe {
                    println!("    add this by hand:\n{}", snippet(&app.format, exe));
                }
            }
        }
    }
    if found == 0 {
        println!(
            "  No supported AI app found (Claude Desktop, Cursor, Windsurf, Antigravity, opencode, Codex)."
        );
    }
    claude_code(exe.as_deref(), dry_run);
    if exe.is_some() {
        println!("Then run `winwright doctor` once to check this PC.");
    }
    Ok(())
}

/// The arguments of the `claude mcp` command that registers (`exe` given) or removes Winwright.
fn claude_args(exe: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["mcp", if exe.is_some() { "add" } else { "remove" }, NAME]
        .map(String::from)
        .into();
    args.extend(["--scope".into(), "user".into()]);
    if let Some(exe) = exe {
        args.extend(["--".into(), exe.into(), "mcp".into()]);
    }
    args
}

/// Runs `claude` (installed as claude.exe, or as claude.cmd by npm); `None` when it is not on
/// PATH.
fn claude(args: &[String]) -> Option<std::process::Output> {
    ["claude.exe", "claude.cmd"].iter().find_map(|name| {
        std::process::Command::new(name)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
    })
}

/// Registers Winwright in Claude Code through its own command when `claude` is on PATH, and
/// prints the command otherwise.
fn claude_code(exe: Option<&str>, dry_run: bool) {
    let args = claude_args(exe);
    let command = format!(
        "claude {}",
        args.iter()
            .map(|a| if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            })
            .collect::<Vec<_>>()
            .join(" ")
    );
    let known = claude(&["mcp".into(), "get".into(), NAME.into()]).map(|o| o.status.success());
    match (known, exe.is_some()) {
        (None, _) => println!("  Claude Code: run  {command}"),
        (Some(true), true) => println!("  Claude Code: already set up"),
        (Some(false), false) => println!("  Claude Code: not registered"),
        (Some(_), _) if dry_run => println!("  Claude Code: would run  {command}"),
        (Some(_), _) => match claude(&args) {
            Some(out) if out.status.success() => {
                println!("  Claude Code: done ({command}); open a new session");
            }
            _ => println!("  Claude Code: it did not work; run  {command}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\Ada\AppData\Local\Programs\Winwright\winwright.exe";

    fn written(edit: Result<Edit, String>) -> String {
        match edit.unwrap() {
            Edit::Write(text) => text,
            Edit::Unchanged => panic!("expected a change"),
        }
    }

    #[test]
    fn claude_code_gets_the_user_scope_commands() {
        assert_eq!(
            claude_args(Some(EXE)),
            [
                "mcp",
                "add",
                "winwright",
                "--scope",
                "user",
                "--",
                EXE,
                "mcp"
            ]
        );
        assert_eq!(
            claude_args(None),
            ["mcp", "remove", "winwright", "--scope", "user"]
        );
    }

    #[test]
    fn json_configs_gain_one_entry_and_keep_everything_else_in_order() {
        let before = r#"{"theme": "dark", "mcpServers": {"other": {"command": "x", "env": {"KEY": "secret"}}}}"#;
        let after = written(edit_json(before, Flavor::McpServers, Some(EXE)));
        let v: Value = serde_json::from_str(&after).unwrap();
        assert_eq!(v["mcpServers"]["winwright"]["command"], EXE);
        assert_eq!(v["mcpServers"]["winwright"]["args"], json!(["mcp"]));
        assert_eq!(v["mcpServers"]["other"]["env"]["KEY"], "secret");
        assert!(after.find("theme").unwrap() < after.find("mcpServers").unwrap());
        // Running it again changes nothing; removing takes only Winwright out.
        assert_eq!(
            edit_json(&after, Flavor::McpServers, Some(EXE)),
            Ok(Edit::Unchanged)
        );
        // A registration of this exe with the person's own extras is kept as it is.
        let custom = format!(
            r#"{{"mcpServers": {{"winwright": {{"command": "{}", "args": ["mcp"], "env": {{"A": "1"}}}}}}}}"#,
            EXE.replace('\\', "\\\\")
        );
        assert_eq!(
            edit_json(&custom, Flavor::McpServers, Some(EXE)),
            Ok(Edit::Unchanged)
        );
        let removed = written(edit_json(&after, Flavor::McpServers, None));
        let v: Value = serde_json::from_str(&removed).unwrap();
        assert!(v["mcpServers"].get("winwright").is_none());
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        // A missing or empty file becomes a fresh config.
        let fresh = written(edit_json("", Flavor::McpServers, Some(EXE)));
        assert!(fresh.contains("winwright.exe"));
    }

    #[test]
    fn opencode_uses_its_own_shape_and_nesting() {
        let flat = written(edit_json(r#"{"mcp": {}}"#, Flavor::Opencode, Some(EXE)));
        let v: Value = serde_json::from_str(&flat).unwrap();
        assert_eq!(v["mcp"]["winwright"]["command"], json!([EXE, "mcp"]));
        assert_eq!(v["mcp"]["winwright"]["type"], "local");
        let nested = written(edit_json(
            r#"{"mcp": {"servers": {}}}"#,
            Flavor::Opencode,
            Some(EXE),
        ));
        let v: Value = serde_json::from_str(&nested).unwrap();
        assert_eq!(v["mcp"]["servers"]["winwright"]["enabled"], true);
    }

    #[test]
    fn unreadable_json_is_left_alone() {
        let with_comments = "{\n  // my servers\n  \"mcpServers\": {}\n}";
        assert!(edit_json(with_comments, Flavor::McpServers, Some(EXE)).is_err());
        assert!(edit_json("[1, 2]", Flavor::McpServers, Some(EXE)).is_err());
        assert!(edit_json(r#"{"mcpServers": 3}"#, Flavor::McpServers, Some(EXE)).is_err());
    }

    #[test]
    fn codex_tables_are_added_replaced_and_removed_as_text() {
        let before = "model = \"o4\"\n\n[mcp_servers.other]\ncommand = 'x'\n";
        let added = written(edit_codex(before, Some(EXE)));
        assert!(added.starts_with(before));
        assert!(added.contains(&format!("{CODEX_HEADER}\ncommand = '{EXE}'")));
        assert_eq!(edit_codex(&added, Some(EXE)), Ok(Edit::Unchanged));

        // An old registration (with a sub-table) in the middle is replaced in place.
        let old = format!(
            "a = 1\n{CODEX_HEADER}\ncommand = 'old.exe'\n[mcp_servers.winwright.env]\nX = '1'\n[mcp_servers.other]\ncommand = 'x'\n"
        );
        let replaced = written(edit_codex(&old, Some(EXE)));
        assert!(replaced.starts_with("a = 1\n[mcp_servers.winwright]"));
        assert!(!replaced.contains("old.exe") && !replaced.contains("X = '1'"));
        assert!(replaced.ends_with("[mcp_servers.other]\ncommand = 'x'\n"));

        let custom = format!("{CODEX_HEADER}\ncommand = '{EXE}'\nenv_vars = [\"MINE\"]\n");
        assert_eq!(edit_codex(&custom, Some(EXE)), Ok(Edit::Unchanged));

        let removed = written(edit_codex(&added, None));
        assert_eq!(removed, before);
        assert_eq!(edit_codex(before, None), Ok(Edit::Unchanged));
        assert!(edit_codex("", Some(r"C:\it's\winwright.exe")).is_err());

        // A header with spacing or a comment is still found and replaced, never duplicated.
        let commented = "[ mcp_servers.\"winwright\" ] # mine\ncommand = 'old.exe'\n";
        let fixed = written(edit_codex(commented, Some(EXE)));
        assert_eq!(fixed.matches("winwright]").count(), 1, "{fixed}");
        // Dotted keys are not parsed: refused rather than doubled.
        let dotted = "mcp_servers.winwright.command = 'old.exe'\n";
        assert!(edit_codex(dotted, Some(EXE)).is_err());
    }
}
