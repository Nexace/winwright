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
cargo run -p winwright-cli -- --version
```

## Layout

| Crate | Role |
|---|---|
| `winwright-contracts` | Owned DTOs, typed errors with stable wire codes, backend traits |
| `winwright-core` | Sessions, per-session element refs, action lease, config |
| `winwright-security` | Default-deny policy, sensitive-field detection, redaction |
| `winwright-cli` | `winwright.exe` |

Logs go to stderr; set `WINWRIGHT_LOG=debug` for detail.
