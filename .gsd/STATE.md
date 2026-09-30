# Winwright State

**Updated:** 2026-09-30
**Current phase:** 1 — Window + UIA observation
**Toolchain:** Rust 1.98.1 MSVC (pinned), windows-rs 0.62, tokio 1.53, schemars 1.2

## Done
- Phase 0: workspace (contracts, core, security, cli); typed errors with stable codes + hints;
  DTOs for elements/windows/snapshots/locators/security/config; backend traits
  (`WindowBackend`, `UiAutomationBackend`) with `ElementKey` epoch slots; per-session
  `RefTable` with ref reuse + epoch invalidation; terminal session cancel + emergency
  `cancel_all`; in-process `ActionLease`; default-deny `Policy`; redaction + `SecretString`;
  config loading; stderr logging; `winwright --version`.

## Next
- Phase 1: `winwright-win32` (EnumWindows, foreground, DPI awareness), `winwright-uia`
  (dedicated MTA worker, cached tree walk), core snapshot compression/rendering, engine,
  CLI `windows` / `snapshot` / `inspect`.

## Decisions
- Refs are per-session and monotonic; an element keeps its ref across snapshots when its
  runtime id + static identity match (keeps diffs readable).
- Cancelling a session is terminal until an explicit user reset.
- `WindowInfo.hwnd` is exposed as an opaque number; platform layer revalidates identity.
- Value of password/sensitive fields is never read by the backend (not just redacted later).
