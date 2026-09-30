# Winwright State

**Updated:** 2026-09-30
**Current phase:** 1b — persistent engine (`winwright serve`) so refs survive CLI processes
**Toolchain:** Rust 1.98.1 MSVC (pinned), windows-rs 0.62.2, tokio 1.53, schemars 1.2

## Done
- Phase 0: workspace (contracts, core, security, cli); typed errors with stable codes + hints;
  DTOs for elements/windows/snapshots/locators/security/config; backend traits
  (`WindowBackend`, `UiAutomationBackend`) with `ElementKey` epoch slots; per-session
  `RefTable` with ref reuse + epoch invalidation; terminal session cancel + emergency
  `cancel_all`; in-process `ActionLease`; default-deny `Policy`; redaction + `SecretString`;
  config loading; stderr logging; `winwright --version`.
- Phase 1: `winwright-win32` (EnumWindows + DWM cloak/bounds, process names, cursor,
  Per-Monitor-V2 DPI); `winwright-uia` (dedicated MTA worker, bounded mailbox + oneshot
  replies, cache-request tree walk with one cross-process call per descended node, epoch
  slots, inspect with ancestors, UIA transaction timeouts); core `Compressor` (keep/flatten/
  prune, list truncation, grid-cell folding, bidi stripping, redaction) and `Engine`
  (windows, snapshot, inspect); CLI `windows`, `snapshot`, `inspect`.
  - Verified live: Notepad, Settings, File Explorer trees readable with refs.
  - Release timings incl. process spawn + COM init: `windows` ~58 ms, active snapshot ~240 ms.
  - Tests: 63 unit + 3 opt-in live (`cargo test -p winwright-uia -- --ignored --test-threads=1`).

## Next
- Phase 1b: `winwright serve` owning the Engine; CLI talks to it over a current-user-ACL named
  pipe; `--session <id>`; `inspect <ref>`; cross-process action lease.
- Phase 2: locator engine + Rust Win32 fixture app (`fixtures/test-win32-app`).

## Decisions
- One ref namespace (`eN`) for windows and elements; no separate `wN` refs.
- Refs are per-session and monotonic; an element keeps its ref across snapshots when its
  runtime id + static identity match (keeps diffs readable).
- Cancelling a session is terminal until an explicit user reset.
- `WindowInfo.hwnd` is exposed as an opaque number; platform layer revalidates identity.
- Value of sensitive fields is never read by the backend (not just redacted later).
- Deadline expiry during a snapshot walk returns a partial tree marked truncated; cancellation
  returns `CANCELLED`.
- Window selectors never guess: multiple matches -> `ELEMENT_AMBIGUOUS` listing candidates.

## Known gaps
- UWP windows report `ApplicationFrameHost.exe` as process (see docs/app-compatibility.md).
- CLI `windows` starts the UIA worker unnecessarily (fine once `serve` exists).
