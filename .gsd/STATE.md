# Winwright State

**Updated:** 2026-10-01
**Current phase:** paused after Phase 11 (clean point for context compaction)
**Toolchain:** Rust 1.98.1 MSVC (pinned), windows-rs 0.62.2, tokio 1.53, schemars 1.2, regex 1.13, rmcp 3.5

## Done
- Phase 0: workspace, contracts, typed errors, config, logging, sessions/refs, lease, policy, redaction.
- Phase 1: Win32 windows + DPI, dedicated MTA UIA worker, compact snapshots with stable refs,
  inspect. Verified live on Notepad / Settings / File Explorer.
- Phase 2: locator engine (role/name/text/AutomationId/label/class/framework/ancestor/nth,
  exact/contains/regex, ranking, ambiguity with candidate refs, secret-safe text matching),
  `find`, stale-ref re-resolution by identity.
- Phase 3: pattern-first action engine with verification, activation-risk classifier,
  UIPI refusal, action lease, unknown-outcome timeouts; Win32 window control.
- Phase 4: `wait_for` (real-state polling, UIA events only wake the loop; listeners attached
  only while a wait runs), snapshot diff (`+`/`-`/`~`, focus moves).
- Phase 5: rmcp stdio MCP server, 23 tools (`winwright mcp`). Loopback HTTP not built.
- Phase 6: engine wired to WGC capture + native overlays (`screenshot`, `highlight`); verified
  live at 125% scaling (highlight lands exactly on the target).
- Phase 8: native Yes/No confirmation dialog (default No, auto-deny on timeout), engine refuses
  its own windows, audit log `%LOCALAPPDATA%\winwright\audit.jsonl` (2 MB cap + 1 backup, no typed
  text), Ctrl+Alt+Esc emergency stop + re-arm.
- Phase 11: tray icon while `winwright mcp` runs (Active/Stopped, Stop, Re-enable, Inspector,
  audit log); `winwright inspector` native window (tree, details, 3 s pick, highlight, copy locator).
- Verification hardening: combo select verified by the combo's value (Enter commit only while the
  list is open, focused, and in front); precise expectations re-checked up to 600 ms for providers
  that update asynchronously; tri-state toggle never double-toggles.
- Tests: all workspace tests pass; clippy `-D warnings` clean. Live: `live_fixture` 7/7,
  `mcp_stdio` 2/2, `live_confirm` 1/1.

## How to verify on resume
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p winwright-cli --test live_fixture --test mcp_stdio -- --ignored --test-threads=1`
  (opens the Win32 fixture; UIA patterns plus one guarded Enter for the combo; ~8 s)

## Next (in order; ask the user which first)
- Phase 7 acceptance: physical-input live test on the canvas fixture (moves the mouse, ~10 s).
  **Waiting for user permission. Do not run without a yes.**
- Phase 1b: `winwright serve` + current-user named pipe so refs survive CLI processes.
- Phase 12: recorder / codegen.
- Phase 13: assistant conversation mode (Claude API, key from env var only; local Windows speech;
  push-to-talk). Load the claude-api skill before coding.
- Phase 9 (browser bridge) and 10 (vision fallback): optional.

## Decisions
- One ref namespace (`eN`); refs per session; reused across snapshots when runtime id +
  static identity match; re-resolved by identity within the owning window when stale.
- Cancelling a session is terminal until an explicit user re-enable (MCP picks up a fresh session).
- Sensitive values are never read by the backend; `readText` on them -> `SENSITIVE_FIELD`.
- Window selectors and locators never guess between equally strong matches.
- UIA walks skip already-captured runtime ids (Win32 combo boxes expose cycles when expanded).
- No WebView/Tauri: all UI is native Win32 (user asked for minimal resource use).
- Capture is lazy; UIA event listeners exist only during waits.
- No worktree subagents (user asked to keep disk use minimal); debuginfo trimmed in profiles.

## Known gaps / follow-ups
- UWP windows report `ApplicationFrameHost.exe` as process.
- Capture: intermittent all-black region captures seen once by the capture agent (cause unknown).
- Overlays are clipped to one monitor and are visible in screen captures.
- Input: keyboard layout comes from the calling thread (VkKeyScanW), not the target app.
- Recycle-bin delete and app launch are only unit-tested (live tests opt-in, not yet run).
- Win32 list/tab selection via UIA may skip the app's change notification (e.g. LBN_SELCHANGE).
- Mixed-DPI multi-monitor untested (single monitor).
- MCP loopback HTTP transport not built (stdio only).
