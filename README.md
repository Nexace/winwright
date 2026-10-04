# Winwright

Semantic Windows desktop automation for AI agents — *Playwright for the Windows desktop,
exposed through MCP*. Winwright reads the UI Automation tree, turns it into a compact
snapshot with short element refs, and acts through control patterns before ever falling back
to coordinates.

Status: early development. See [.gsd/SPEC.md](.gsd/SPEC.md) for the full build spec and
[.gsd/ROADMAP.md](.gsd/ROADMAP.md) for phase status.

## Build

Requires Windows 11, the pinned Rust MSVC toolchain (`rust-toolchain.toml`), and the
Windows SDK / C++ Build Tools.

```powershell
cargo build -p winwright-cli
cargo test --workspace
# Real-desktop tests are opt-in and read-only:
cargo test -p winwright-uia -- --ignored --test-threads=1
```

## Try it

```powershell
winwright windows                         # * marks the foreground window
winwright snapshot                        # active window, compact semantic tree
winwright snapshot --window "Notepad" --json
winwright snapshot --process explorer --bounds --patterns
winwright inspect --under-cursor          # or --focused, --at X,Y
```

Example (Save As dialog):

```text
DIALOG "Save As" [e1]
  EDIT "File name:" value="" [e3]
  BUTTON "Save" [e4]
  BUTTON "Cancel" [e5]
```

## Use it from your AI apps (MCP)

Winwright has no app of its own: it is a tool inside the AI apps you already use. Install it
once (`cargo install --path crates/winwright-cli --locked` puts `winwright` in `~\.cargo\bin`),
then add it to each app. The app starts `winwright mcp` itself and stops it when it closes:
no service, no startup entry. It also quits after 10 minutes without a tool call
(`WINWRIGHT_IDLE_MINUTES`, `0` = never); an app that does not restart it needs a restart. Typing `winwright` alone explains this and lists the commands.

Claude Code (all projects):

```powershell
claude mcp add winwright --scope user -- "C:\Users\you\.cargo\bin\winwright.exe" mcp
```

Codex (`~\.codex\config.toml`). Codex passes only the environment variables you list:

```toml
[mcp_servers.winwright]
command = 'C:\Users\you\.cargo\bin\winwright.exe'
args = ["mcp"]
tool_timeout_sec = 120   # the Allow/Deny dialog waits up to 60 s
env_vars = ["APPDATA", "LOCALAPPDATA", "USERPROFILE", "SystemRoot", "WINWRIGHT_NOTION_TOKEN", "WINWRIGHT_NOTION_PARENT"]
```

opencode v2 (`~\.config\opencode\opencode.json`, under `mcp.servers`):

```json
"winwright": { "type": "local", "command": ["C:\\Users\\you\\.cargo\\bin\\winwright.exe", "mcp"] }
```

Antigravity (`~\.gemini\config\mcp_config.json`) and Claude Desktop
(`claude_desktop_config.json`), under `mcpServers`:

```json
"winwright": { "command": "C:\\Users\\you\\.cargo\\bin\\winwright.exe", "args": ["mcp"] }
```

Tools: `desktop_snapshot` (use `diff: true` after actions), `desktop_find`, `desktop_click`,
`desktop_fill`, `desktop_type`, `desktop_press`, `desktop_select`, `desktop_check`,
`desktop_expand`, `desktop_scroll`, `desktop_focus`, `desktop_read_text`, `desktop_wait_for`,
`desktop_inspect`, `desktop_windows`, `window_control`, `desktop_screenshot`, `desktop_mouse`
(move, click, drag or scroll at a point, for apps with no UI tree such as games; the element
under the point is judged like a click on it),
`overlay_highlight`, `overlay_clear`, `app_launch`, `process_list`, `process_terminate`
(always asks; Windows' own processes and services are refused), `filesystem_operation`
(files and folders, and text files: read by lines, write, exact edits, grep; a replaced or
edited file goes to the Recycle Bin first, and secret files such as keys and `.env` ask first),
`shell_execute` (off by default), `memory_save`, `memory_recall`.

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

## Layout

| Crate | Role |
|---|---|
| `winwright-contracts` | Owned DTOs, typed errors with stable wire codes, backend traits |
| `winwright-core` | Engine, sessions, per-session refs, snapshot compression, lease, config |
| `winwright-security` | Default-deny policy, sensitive-field detection, redaction |
| `winwright-win32` | Top-level windows, processes, cursor, Per-Monitor-V2 DPI |
| `winwright-uia` | Raw UI Automation COM on a dedicated MTA worker thread |
| `winwright-cli` | `winwright.exe` |

App-by-app results live in [docs/app-compatibility.md](docs/app-compatibility.md).

Logs go to stderr; set `WINWRIGHT_LOG=debug` for detail.
