# Winwright State

**Updated:** 2026-10-04
**Current phase:** CHECKPOINT (2026-10-04, resume here). Tree green; the last commits are not pushed. Done: security audit, taint rule, Phase 7 (live), our own assistant page (old JARVIS face deleted), `winwright` as a one-word command (installed to ~\.cargo\bin), assistant default Sonnet 5.5 at medium effort, Winwright registered in opencode/Codex/Antigravity, and `confirmationMode: "relaxed"` (the user's own config uses it). Next: the user tries `winwright` in Brave (voice + taint dialog) and Winwright from opencode with Go models.
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
- Security audit H1-L5 (2026-10-03, see checkpoint): hardware-only approval, arming only while
  in front, send shortcuts/Win chords/typed line breaks, launch allowlists, classifier hardening,
  dialog context, own folders protected, risky copies/moves confirmed, prompts show the resolved
  program and all arguments, reads and screenshots audited, 128 KB output cap.
- Taint rule (2026-10-03, 8569de4 Winwright + 24ef6a3 bridge; mechanism in INTEGRATION.md):
  `winwright mcp --taint-file`; once the bridge's PreToolUse hook writes the marker (before any
  web/Notion/sub-agent/third-party MCP tool), every allowed desktop change needs the native
  confirmation until a new conversation or Re-enable. Unit-tested on both sides
  (`node --test bridge/taint.test.mjs`); not yet exercised live with JARVIS running.
- Tests: all workspace tests pass; clippy `-D warnings` clean. Live after the audit fixes
  (2026-10-03): `live_fixture` 7/7, `mcp_stdio` 2/2, `live_confirm` 2/2, `live_desktop` 3/3.
- Phase 7 live (2026-10-03, user OK'd): `live_canvas` 1/1 twice (click, right-click,
  double-click, type, Ctrl+K, wheel on the inaccessible canvas, all through SendInput) and
  `live_input` 2/2. The first canvas run found a wheel step sending 3 notches (fixed, 218bf04).
  Opt-in, moves the real mouse: `cargo test -p winwright-cli --test live_canvas -- --ignored`.

## How to verify on resume
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p winwright-cli --test live_fixture --test mcp_stdio -- --ignored --test-threads=1`
  (opens the Win32 fixture; UIA patterns plus one guarded Enter for the combo; ~8 s)
- `cargo test -p winwright-overlay --test live_confirm -- --ignored --test-threads=1 --skip manual_`
  (shows the confirm dialog twice, ~5 s; do not click it). `... manual_` asks a person to click Allow.

## Next (in order; plan in .gsd/INTEGRATION.md "Decisions and changes from the plan review")
1. Assistant end to end in Brave with the user (`winwright`): voice round trip, then the taint
   check ("weather in Pune", then "Open Notepad" must show Winwright's dialog). Then the user's
   feedback on the page's look.
2. Reports + Notion memory (cloud, user's choice), last.
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
- Dialog context (M1) covers owned windows and `#32770` dialogs; in-window dialogs (WinUI
  ContentDialog, web modals) are judged by the button name only.
- UI-driven execution: Enter in a terminal window, and Enter in the Run box / Start search /
  Explorer address bar on a shell command line, is judged as Shell/PowerShell; Win+R and Win+X
  as PowerShell. Still open: clicking OK in an already-open Run box, Start's "Run command"
  result, terminals inside other apps (VS Code), Task Manager's "Run new task".
- Relaxed mode (2026-10-04): Sensitive is allowed; Destructive (now including spending and
  security phrases), file delete, process terminate and the shell still ask. The user's real
  config is `%APPDATA%\winwright\config.json` = `{"security":{"confirmationMode":"relaxed"}}`.
  Outside the assistant the taint rule is off (nothing writes the marker).
- `app_launch` URI allowlist is fixed in code (http, https, mailto, ms-settings, shell:<folder>);
  no config for extra schemes yet.
- Protecting the exe folder (M3) means file operations are refused in the folder winwright.exe
  runs from (e.g. Downloads if run from there): install it in its own folder.

## Assistant (apps/jarvis; updated 2026-10-04)
- Started as vendored github.com/adewaskar/jarvis (MIT); only its Node bridge is kept. Our own chat page (`winwright-ui/`, plain HTML/CSS/JS) is served by the bridge at http://localhost:8787/; the JARVIS React/Three.js face is deleted. `winwright assistant` runs `node bridge/server.mjs` (one process); it refuses until `npm install` was run in `apps/jarvis`.
- Live so far: typed turns work (desktop_windows + a card; a WebSocket turn from a script); push-to-talk reached `/stt` in Brave. Not yet confirmed by the user in Brave: voice round trip and the taint dialog ("weather in Pune", then "Open Notepad").
- ElevenLabs key is set as the user env var ELEVENLABS_API_KEY (sk_, 51 chars; the first try had angle brackets and the wrong value).
- Start: type `winwright` in any PowerShell (no subcommand = assistant; it opens http://localhost:8787/ in the default browser, Brave). It is a release build in `~\.cargo\bin\winwright.exe` (on the user PATH) that finds `apps\jarvis` via the checkout it was built from. After changing the Rust code, reinstall with `cargo install --path crates/winwright-cli --locked` (~1 min). The debug `target\debug\winwright.exe` still works too.
- Background tasks in a Claude session are killed after 10 min, so run the assistant in the user's own window, not as a session background task. The Terminal panel's PowerShell did not reach a prompt in 60 s (slow profile); `Start-Process powershell -NoExit -NoProfile ...` worked.

## CHECKPOINT 2026-10-03 (resume here)
State of the tree: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace` all pass (2026-10-03, after the audit fixes); live `live_fixture` 7/7, `mcp_stdio` 2/2, `live_confirm` 2/2 and `live_desktop` 3/3 after the fixes. `scripts/check.ps1` runs the same (`-Live` adds live tests).

COMMITTED 2026-10-03 as c48e464 (contracts/mcp/cli, incl. idle timer + push-to-talk), 1b6a954 files, c4afe04 uia, 9bb432d shell, 8c3ad61 overlay, e276d97 inspector, 39a2a16 core/security:
- contracts/mcp/cli: 20 fixes (camelCase file-op fields, typo-rejecting actions, stdout purity in mcp mode, negative coords, config switches honoured, overflow panics, CANCELLED hint, real elapsed ms) + my MCP idle timer (WINWRIGHT_IDLE_MINUTES) + `winwright assistant` push-to-talk hotkey (Ctrl+Space, /ptt relay).
- files/uia/shell: 8.3 short-name protected-path bypass fixed; UIA timeout labels, slot leaks, event-handler leaks, ELEMENT_STALE on inspect; script-host file types blocked.
- overlay/inspector: confirm dialog (abandon-before-poll, no WM_CLOSE to recycled handle, typing cannot approve, BN_CLICKED from Allow only, per-thread DPI), tray set() leaks + single menu, Inspector races/splitter/Esc/clipboard fixes.
- core/security: every Engine entry refuses after emergency stop (ensure_running); guard_self refuses any winwright.exe process; interpreters (cmd, powershell, python, wscript, mshta, rundll32, ..., and winwright itself) count as shell execution so app_launch of them is default-denied; PowerShell keeps its own switch in exec; shell_execute gets its requested timeout; Delete key, risky Select options and Enter on send-style buttons need confirmation; redaction/classifier extensions; snapshot/diff/wait fixes. 93 core tests.

Security audit items, all DONE 2026-10-03 (origin = github.com/Nexace/winwright, private):
- H1 327fa5f: Allow counts only for input whose GetCurrentInputMessageSource origin is IMO_HARDWARE, handled by the dialog's own loop (BM_CLICK, forged WM_COMMAND, UIA Invoke, SendInput, OSK, voice control refused; documented). Live-verified both ways: forged clicks refused; a real mouse click approved (the user clicked a test dialog).
- H2 d5a84fe: Win chords and Ctrl/Alt+Enter confirm; Enter in a message/reply/comment/chat/compose field confirms (classify_submit); typed \n/\r judged as Enter on the field (focused one when untargeted), Tab before the last line break confirms.
- H3 3736b4d: URI scheme allowlist; default-handler opens only for document/image/media types (.lnk/.url/scripts/unknown refused); bare names via System32/PATH/App Paths registry, never ShellExecute search; UNC/device/mapped-drive executables refused before any filesystem access.
- M1/M2 524bbaf: names normalized (format chars dropped, fullwidth folded), more verbs, dialog context for affirmative buttons; is_sensitive reads labeled_by/help_text.
- M3/M4 bd41edc: own config/log/exe folders protected (both spellings: MSIX redirects AppData); move/rename confirm; copies judged by transfer_risk (share, other volume, secrets, whole drives/profiles).
- M5/M6 97d726c: highlight guard_self + refused (and overlays cleared) while a confirm is open; prompts show resolved program + all args (600-char cap); policy judges the stricter of requested/resolved program; exec runs the path it resolved; dialog escapes control/bidi chars.
- L5 2b07b80: arming starts at each real activation, disarms on deactivation, and checks foreground + elapsed (forged WM_ACTIVATE/WM_TIMER cannot arm early).
- L2 755ad51 audit reads/screenshots/file reads; L4 d74cf6e 128 KB text cap in MCP.
- Bugs: c38315e real elapsed in capture/input timeouts; 6414e1f overlay disabled -> ACTION_BLOCKED; 4d40fd0 config value ranges + loopback-only httpHost; e40dadf depth-limit truncation reported.

Final cleanup still owed: `cargo clean` (~11 GB).

## Footprint (2026-10-01)
- Assistant (2026-10-04): the JARVIS face is deleted; node_modules 814 MB -> ~310 MB (the Claude Agent SDK binary is most of it, required). One Node process (the bridge serves the page); no Vite, no 3D.
- Winwright: idles ~23 MB / 11 threads (debug). Release profile now opt-level "s" + thin LTO + 1 codegen unit + stripped (not panic=abort: window procs use catch_unwind). Release build not made yet (storage); do it only when needed.
- Cleanup at the end: `cargo clean` (~11 GB).
