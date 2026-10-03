# Winwright State

**Updated:** 2026-10-03
**Current phase:** CHECKPOINT. Tree green; the debugging batch is committed (c48e464..39a2a16). Next: remaining security fixes, see "CHECKPOINT 2026-10-03".
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

## Next (in order; plan in .gsd/INTEGRATION.md "Decisions and changes from the plan review")
1. Remaining security fixes (list in the checkpoint).
2. Taint rule (web/Notion content read -> desktop changes need confirmation).
3. Phase 7 physical-input live test (ask the user first; moves the mouse ~10 s).
4. JARVIS end to end, then ElevenLabs (user sets ELEVENLABS_API_KEY as a user env var).
5. Reports + Notion memory (cloud, user's choice), last.
Dropped: Phase 1b, Phase 9, Phase 12, local speech models (Whisper/Kokoro), Jev.

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

## CHECKPOINT 2026-10-03 (resume here)
State of the tree: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace` all pass; live `live_fixture` 7/7, `mcp_stdio` 2/2, `live_confirm` 1/1 (run 2026-10-03). `scripts/check.ps1` runs the same (`-Live` adds live tests).

COMMITTED 2026-10-03 as c48e464 (contracts/mcp/cli, incl. idle timer + push-to-talk), 1b6a954 files, c4afe04 uia, 9bb432d shell, 8c3ad61 overlay, e276d97 inspector, 39a2a16 core/security:
- contracts/mcp/cli: 20 fixes (camelCase file-op fields, typo-rejecting actions, stdout purity in mcp mode, negative coords, config switches honoured, overflow panics, CANCELLED hint, real elapsed ms) + my MCP idle timer (WINWRIGHT_IDLE_MINUTES) + `winwright assistant` push-to-talk hotkey (Ctrl+Space, /ptt relay).
- files/uia/shell: 8.3 short-name protected-path bypass fixed; UIA timeout labels, slot leaks, event-handler leaks, ELEMENT_STALE on inspect; script-host file types blocked.
- overlay/inspector: confirm dialog (abandon-before-poll, no WM_CLOSE to recycled handle, typing cannot approve, BN_CLICKED from Allow only, per-thread DPI), tray set() leaks + single menu, Inspector races/splitter/Esc/clipboard fixes.
- core/security: every Engine entry refuses after emergency stop (ensure_running); guard_self refuses any winwright.exe process; interpreters (cmd, powershell, python, wscript, mshta, rundll32, ..., and winwright itself) count as shell execution so app_launch of them is default-denied; PowerShell keeps its own switch in exec; shell_execute gets its requested timeout; Delete key, risky Select options and Enter on send-style buttons need confirmation; redaction/classifier extensions; snapshot/diff/wait fixes. 93 core tests.

Remaining security audit items (not yet done):
- H1: Allow must accept only real hardware input (reject injected/BM_CLICK/UIA Invoke via GetCurrentInputMessageSource); document: no voice-control/OSK approval.
- H2 rest: classify the focused element when keys have no target; Ctrl/Alt+Enter, Win chords, newline in type_text.
- H3: launch allowlist for URI schemes and file types; no UNC executables; .lnk/.url targets.
- M1 classifier: strip zero-width/format chars, more verbs, dialog context. M2: is_sensitive uses labeled_by/help_text.
- M3: protect %APPDATA%\winwright, %LOCALAPPDATA%\winwright, the exe folder; confirm move/rename. M4: confirm UNC/cross-volume/sensitive-source copies.
- M5: guard_self in highlight; no overlays while a confirm is pending. M6: show full args + resolved path in prompts.
- L2 audit screenshots/reads. L4 output caps. L5 arm delay from first activation.
- Other reported: capture/input timeouts still say 0 ms (use ctx.started); config-disabled backend should be ACTION_BLOCKED; load_config value sanity; UIA depth-limit truncation never reported.

Nothing is running (JARVIS and Winwright stopped). Final cleanup still owed: `cargo clean` (~11 GB), apps/jarvis/node_modules if unused.

## Footprint (2026-10-01)
- JARVIS: dropped unused Picovoice wake-word deps; the 208 MB `onnxruntime-node` is no longer installed (override stub in apps/jarvis/stubs, `onnxruntime-common` pinned for the web build); node_modules 1.1 GB -> 814 MB (253 MB of it is the Claude Agent SDK binary, required). 3D scene: low-power GPU, pixel ratio capped at 1.5. Bridge idles ~99 MB.
- Winwright: idles ~23 MB / 11 threads (debug). Release profile now opt-level "s" + thin LTO + 1 codegen unit + stripped (not panic=abort: window procs use catch_unwind). Release build not made yet (storage); do it only when needed.
- Cleanup at the end: `cargo clean` (~11 GB), `apps/jarvis/node_modules` if unused.
