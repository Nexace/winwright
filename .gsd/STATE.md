# Winwright State

**Updated:** 2026-09-30
**Current phase:** 4 — waits, UIA events, verification, snapshot diff
**Toolchain:** Rust 1.98.1 MSVC (pinned), windows-rs 0.62.2, tokio 1.53, schemars 1.2, regex 1.13

## Done
- Phase 0: workspace, contracts, typed errors, config, logging, sessions/refs, lease, policy, redaction.
- Phase 1: Win32 windows + DPI, dedicated MTA UIA worker, compact snapshots with stable refs,
  inspect. Verified live on Notepad / Settings / File Explorer.
- Phase 2: locator engine (role/name/text/AutomationId/label/class/framework/ancestor/nth,
  exact/contains/regex, ranking, ambiguity with candidate refs, secret-safe text matching),
  `find`, stale-ref re-resolution by identity. CLI `find`.
- Phase 3: pattern-first action engine (click/fill/type/focus/select/check/uncheck/toggle/
  expand/collapse/scroll/scrollIntoView/press/readText) with verification, activation-risk
  classifier (Send/Delete/Buy... -> CONFIRMATION_REQUIRED), UIPI refusal, action lease,
  unknown-outcome timeouts; Win32 window focus/move/resize/state/close. CLI commands.
- Parallel adapter crates (subagents, merged): `winwright-input` (SendInput, 48 tests),
  `winwright-capture` (WGC + WIC, 19 tests + 6 live), `winwright-overlay` (native overlays +
  hotkeys, 36 tests + 5 live), `winwright-shell` / `winwright-files` (typed process/file ops,
  protected paths, recycle-only delete), fixtures (`winwright-fixture-win32`,
  `winwright-fixture-canvas`) and `winwright-test-support`.
- Live fixture acceptance (`cargo test -p winwright-cli --test live_fixture -- --ignored
  --test-threads=1`): 6/6, stable across 3 runs, UIA patterns only (no injected input).
- Totals: 269 unit/integration tests passing; 30 opt-in live tests.

## Next
- Phase 4: `wait_for` (event-accelerated polling), UIA event handlers on the worker, snapshot diff.
- Phase 5: rmcp MCP server (stdio) over the engine.
- Phase 1b: `winwright serve` + named pipe so refs survive CLI processes.
- Wire capture/overlay/shell/files into the engine (screenshot, highlight, app_launch,
  process_list, filesystem_operation) behind policy.
- Phase 7 acceptance needs a physical-input run on the canvas fixture: ask the user first.

## Decisions
- One ref namespace (`eN`); refs per session; reused across snapshots when runtime id +
  static identity match; re-resolved by identity within the owning window when stale.
- Cancelling a session is terminal until an explicit user reset.
- Sensitive values are never read by the backend; `readText` on them -> `SENSITIVE_FIELD`.
- Window selectors and locators never guess between equally strong matches.
- UIA walks skip already-captured runtime ids (Win32 combo boxes expose cycles when expanded).

## Known gaps / follow-ups
- UWP windows report `ApplicationFrameHost.exe` as process.
- Capture: intermittent all-black region captures seen once by the capture agent (cause unknown).
- Overlays are clipped to one monitor and are visible in screen captures.
- Input: keyboard layout comes from the calling thread (VkKeyScanW), not the target app.
- Recycle-bin delete and app launch are implemented but only unit-tested (live tests opt-in).
- Win32 list/tab/combo selection via UIA sets the selection without the app's change
  notification (e.g. LBN_SELCHANGE) in some controls; verification reads the control state.
