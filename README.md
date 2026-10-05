# Winwright

Give your AI apps hands on the Windows desktop, with you in charge. Winwright is an
[MCP](https://modelcontextprotocol.io) server: Claude, Codex, Cursor, Windsurf, opencode or
Antigravity can see and use any Windows app through it, and it asks you before anything risky.

- **Sees apps the way screen readers do.** It reads Windows UI Automation into a compact tree
  with short element refs (`[e12]`), and acts through the app's own controls before it ever
  falls back to the mouse.
- **You stay in charge.** Risky steps (deleting, paying, closing programs, running commands)
  show Winwright's own Allow/Deny dialog that only your real mouse or keyboard can answer.
  Ctrl+Alt+Esc stops everything; every action goes to a local audit log.
- **It can teach you.** Ask "teach me how to crop a photo in Lightroom": a pointer glides to
  each step with a caption and waits for *your* click, saying "Not there" if you miss.
- **Works where there is no UI tree** (games, canvases, custom-drawn apps) by screenshot pixels.

Windows 10 or 11, 64-bit. Tested on Windows 11.

## Install

1. Download `winwright-<version>-windows-x64.zip` from
   [Releases](https://github.com/Nexace/winwright/releases) and unzip it.
2. Run `winwright setup` in that folder (in Terminal: `.\winwright setup`). It copies itself to
   `%LOCALAPPDATA%\Programs\Winwright` and registers itself in every AI app it finds (Claude
   Desktop, Cursor, Windsurf, Antigravity, opencode, Codex), backing up each config first. For
   Claude Code it prints the one command to run. `--dry-run` shows what it would change.
3. Restart your AI apps, then run `winwright doctor` once to check this PC.

Windows may warn that the download is from an unknown publisher (the exe is not code-signed
yet): choose *More info*, then *Run anyway*. Each release lists the zip's SHA-256 so you can
check it (`Get-FileHash winwright-*.zip`).

To uninstall: `winwright setup --remove`, then delete `%LOCALAPPDATA%\Programs\Winwright`.

## Use it

Just ask your AI app: "open Notepad and write a shopping list", "rename the photos in
Downloads by date", "what is this error dialog saying?", "show me where the export button is",
"teach me to add a filter in Excel". The app starts `winwright mcp` itself and stops it when it
closes: no service, no startup entry. It also quits after 10 minutes without a tool call
(`WINWRIGHT_IDLE_MINUTES`, `0` = never). Typing `winwright` alone explains this and lists the
commands.

<details><summary>Registering by hand</summary>

Claude Code (all projects):

```powershell
claude mcp add winwright --scope user -- "$env:LOCALAPPDATA\Programs\Winwright\winwright.exe" mcp
```

Codex (`~\.codex\config.toml`). Codex passes only the environment variables you list:

```toml
[mcp_servers.winwright]
command = 'C:\Users\you\AppData\Local\Programs\Winwright\winwright.exe'
args = ["mcp"]
tool_timeout_sec = 120   # the Allow/Deny dialog waits up to 45 s
env_vars = ["APPDATA", "LOCALAPPDATA", "USERPROFILE", "SystemRoot", "WINWRIGHT_NOTION_TOKEN", "WINWRIGHT_NOTION_PARENT"]
```

opencode (`~\.config\opencode\opencode.json`, under `mcp`):

```json
"winwright": { "type": "local", "command": ["C:\\Users\\you\\AppData\\Local\\Programs\\Winwright\\winwright.exe", "mcp"], "enabled": true }
```

Claude Desktop (`claude_desktop_config.json`), Cursor (`~\.cursor\mcp.json`), Windsurf
(`~\.codeium\windsurf\mcp_config.json`) and Antigravity (`~\.gemini\config\mcp_config.json`),
under `mcpServers`:

```json
"winwright": { "command": "C:\\Users\\you\\AppData\\Local\\Programs\\Winwright\\winwright.exe", "args": ["mcp"] }
```

</details>

Tools: `desktop_snapshot` (use `diff: true` after actions), `desktop_find`, `desktop_click`,
`desktop_fill`, `desktop_type`, `desktop_press`, `desktop_select`, `desktop_check`,
`desktop_expand`, `desktop_scroll`, `desktop_focus`, `desktop_read_text`, `desktop_wait_for`,
`desktop_inspect`, `desktop_windows`, `window_control`, `desktop_screenshot`, `desktop_mouse`
(move, click, drag or scroll at a point, for apps with no UI tree such as games; the element
under the point is judged like a click on it),
`overlay_highlight` (shows you where something is: a pointer with a caption bubble at an
element or at a spot on a screenshot), `desktop_guide` (teaches you: points at each step in
turn and waits until you click it, or press the keys it names; it never clicks for you; while
it waits it sees only your mouse clicks, never your keys), `overlay_clear`, `app_launch`,
`process_list`, `process_session` (run a
program in the background, send it input, read its output; starting and every input ask, and
need `allowShell`; the emergency stop ends them all), `process_terminate`
(always asks; Windows' own processes and services are refused), `filesystem_operation`
(files and folders, and text files: read by lines, write, exact edits, grep; a replaced or
edited file goes to the Recycle Bin first, and secret files such as keys and `.env` ask first),
`shell_execute` (off by default), `memory_save`, `memory_recall`.

**Check your PC.** `winwright doctor` lists every monitor with its size and scaling, draws a
small box on each and checks it lands exactly in place and shows in a screenshot, then checks
UI Automation and the click watcher. Run it once after installing, and after adding a monitor.

**Safety.** Before anything risky (deleting, spending money, changing security, closing
programs, running commands) Winwright shows its own Allow/Deny dialog, which only your real
mouse or keyboard can answer. Password values are never read; elevated apps are refused
(`UIPI_BLOCKED`). `security.confirmationMode` in `%APPDATA%\winwright\config.json` is
`balanced` (default), `strict` (ask before every change) or `relaxed` (ask only before the
risky ones). Ctrl+Alt+Esc stops everything at any time; every action goes to an audit log.

**Memory.** After a desktop task the app's AI saves a short report with `memory_save` to
`%USERPROFILE%\.winwright\reports` (names and summaries only), and reads earlier ones with
`memory_recall`, so every app shares one memory. With `WINWRIGHT_NOTION_TOKEN` (a Notion
"API token" connection) and `WINWRIGHT_NOTION_PARENT` (the link of a page shared with it) set,
each report is also copied to Notion. `WINWRIGHT_MEMORY=0` turns memory off.

**Outside content.** A client that knows when its conversation read a web page can say so
with `winwright mcp --taint-file <path>` (it creates the file then); from that point every
desktop change needs your yes. Apps that do not do this get the normal rules.

## Build from source

Needs the Rust toolchain pinned in `rust-toolchain.toml` (MSVC) and the Windows SDK / C++
Build Tools.

```powershell
cargo install --path crates/winwright-cli --locked   # puts winwright.exe in ~\.cargo\bin
scripts\check.ps1                                    # format, lint, tests
scripts\check.ps1 -Live                              # also the real-desktop tests (hands off)
```

The command line drives the same engine: `winwright windows`, `winwright snapshot`,
`winwright inspect --under-cursor`, `winwright find --role Button --name Save`, and more
(`winwright --help`). Logs go to stderr; set `WINWRIGHT_LOG=debug` for detail. Design notes
live in [.gsd](.gsd) and app-by-app results in [docs/app-compatibility.md](docs/app-compatibility.md).

## Security

Winwright acts on your desktop with your rights, so treat the AI app driving it as you would a
person at your keyboard. Report vulnerabilities privately: see [SECURITY.md](SECURITY.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your
option. Contributions are accepted under the same terms.
