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

## JARVIS integration (2026-10-01)
- `apps/jarvis` = vendored github.com/adewaskar/jarvis (MIT). Bridge patched to add a `winwright` MCP server when `JARVIS_WINWRIGHT_EXE` is set and to pass its tools through `decideTool` (Winwright enforces its own gates). `winwright assistant` launches it; it refuses until `npm install` is run in `apps/jarvis` (not auto-run: large download).
- Not yet run live (needs `npm install`, Chrome, and the user's Claude Code login). Next: run it once with the user.

## CHECKPOINT 2026-10-01 (resume here)
Committed: Phases 0-6, 8, 11; Winwright UI theme/tray/dialog/Inspector redesign; JARVIS vendored (apps/jarvis, `winwright assistant`).

UNCOMMITTED in the working tree (~32 modified files) = fixes from 5 debugging agents, not yet verified together:
- DONE (reports reviewed): MCP/CLI/contracts (20 fixes); platform adapters (files 8.3 short-name protected-path bypass, uia timeout labels / slot leaks / event-handler leaks, shell script-host blocklist); native UI (confirm dialog: drop before first poll, WM_CLOSE to a recycled handle, typing-approves; tray set() leaks; Inspector races).
- INCOMPLETE: core+security agent hit the usage limit twice. Partial edits in winwright-core and winwright-security; winwright-security tests did not compile (classify.rs `program_capability`, test near line 189). Intended: every Engine entry point (incl. list_windows, process_list) refuses after emergency stop; guard_self refuses any winwright.exe process; classify interpreters on launch.

Next steps, in order:
1. `cargo check --workspace --all-targets`; fix winwright-security compile; then clippy `-D warnings` and `cargo test --workspace`. If the partial core/security edits are too broken, finish or revert only those two crates (git diff first).
2. Security audit findings (full report was in chat):
   - C1 app_launch runs cmd/powershell/winwright.exe with args and no confirm -> Confirm when args non-empty or image is an interpreter; always deny winwright.exe; show full args + resolved path in the prompt (M6).
   - H1 Allow button must accept only real mouse/keyboard input (reject injected / BM_CLICK); documented limitation: no voice-control or on-screen-keyboard approval.
   - H2 keyboard path: classify the focused element; Ctrl/Alt+Enter, Delete, Win chords, newline in type_text count as risky.
   - H3 launch allowlist for URI schemes and file types; no UNC executables.
   - M1 classifier: NFKC + strip format chars + more verbs + dialog context. M2 is_sensitive: include labeled_by/help_text, more terms.
   - M3 protect %APPDATA%\winwright, %LOCALAPPDATA%\winwright and the exe folder; confirm move/rename. M4 confirm UNC / cross-volume / sensitive-source copies.
   - M5 guard_self in highlight; no overlays while a confirm is pending. L1 powershell capability. L3 stop covers reads. L5 arm delay from first activation.
3. Other agent-reported bugs: shell_execute capped at 10 s (services.rs ~327; use max(request timeout, default)); timeouts report "0 ms" in capture/worker.rs:35 and input/lib.rs:250 (use ctx.started); config-disabled backend should be ACTION_BLOCKED not BACKEND_UNAVAILABLE; load_config value sanity (defaultTimeoutMs 0); UIA depth-limit truncation never reported.
4. Commit in logical chunks (fix(files), fix(uia), fix(mcp)/(cli)/(contracts), fix(overlay)/(inspector), fix(core)/(security)).
5. ONE live-test agent, serialized: live_fixture, mcp_stdio, live_confirm, live_desktop, live_capture (repeat ~20x; add a 150 ms delay to test the black-capture race). Phase 7 physical-input test only with the user's OK (moves the mouse ~10 s). Never live-test file ops against the real user profile.
6. Run JARVIS once with the user (`npm install` in apps/jarvis first).
7. Final cleanup the user asked for: `cargo clean` (~11 GB), scratchpad temp files, apps/jarvis/node_modules if unused; keep only source and docs.

- 2026-10-01 idle shutdown added: `winwright mcp` (WINWRIGHT_IDLE_MINUTES, default 10; edits in mcp/src/lib.rs + cli/src/main.rs, uncommitted with the agents' fixes) and JARVIS bridge (JARVIS_IDLE_MINUTES, default 10; committed). Verified live with short limits.

## Footprint (2026-10-01)
- JARVIS: dropped unused Picovoice wake-word deps; the 208 MB `onnxruntime-node` is no longer installed (override stub in apps/jarvis/stubs, `onnxruntime-common` pinned for the web build); node_modules 1.1 GB -> 814 MB (253 MB of it is the Claude Agent SDK binary, required). 3D scene: low-power GPU, pixel ratio capped at 1.5. Bridge idles ~99 MB.
- Winwright: idles ~23 MB / 11 threads (debug). Release profile now opt-level "s" + thin LTO + 1 codegen unit + stripped (not panic=abort: window procs use catch_unwind). Release build not made yet (storage); do it only when needed.
- Cleanup at the end: `cargo clean` (~11 GB), `apps/jarvis/node_modules` if unused.
