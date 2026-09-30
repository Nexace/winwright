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
