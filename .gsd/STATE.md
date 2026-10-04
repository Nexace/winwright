# Winwright State

**Updated:** 2026-10-05
**Current phase:** PLAN COMPLETE (2026-10-05, resume here). Tree green, everything committed, installed (`~\.cargo\bin\winwright.exe`, 29 tools) and pushed to origin/main. Phases 14-18 and 20-22 are done; 19 was dropped by the user. The build cache was deleted at the wrap-up: the first build after this takes a few minutes. Winwright is used only inside the user's AI apps (Claude Code, Codex, opencode, Antigravity); apps/jarvis is deleted. MCP idle shutdown stays 10 min (user's call).
**Phase 22 (2026-10-05):** teaching mode. `overlay_highlight` takes pixel spots and defaults to the pointer style (a blue arrowhead with the caption in a dark bubble); `desktop_guide` points at each step and waits for the person's own click inside it (a low-level mouse hook only while waiting, injected input skipped, keys never seen) or for the spot's pixels to change (keyboard steps); a wrong click marks the step "Not there" and it keeps waiting, the third stops the guide; the result lists every click and carries a screenshot.
**Since the 2026-10-04 checkpoint:** typing waits for each keystroke to show (Notepad garbling fixed; text fields too), `desktop_mouse` (move/click/drag/scroll by screen or window pixels), text files in `filesystem_operation` (read/write/edit/grep; Recycle Bin before replace/edit; secrets ask), `process_terminate`, `process_session` (background programs with input/output; every start and input asks; emergency stop ends them), keyboard layout of the target window, Store apps report their own process, in-window dialogs give Yes/OK their context, Win32 list/tab items clicked for real.
**User settings:** `%APPDATA%\winwright\config.json` = relaxed + `allowShell: true` (written via an explorer-run .cmd; see memory appdata-sandbox-redirect). `allowPowershell` is still false: the permission system refused to let Claude turn a security switch on; the user was told how to add it in Notepad themselves.
**Session notes:** a Bash read of engine_tests.rs fake wiring was refused by the permission classifier on 2026-10-04; engine-level fakes were not extended since then (pure unit tests + live tests instead). Two `winwright-replace-test.txt` files sit in the user's Recycle Bin (opt-in test; Claude must not empty it). Old `winwright.old.*.exe` copies in `~\.cargo\bin` are deleted once no app holds them.
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
- Phase 5: rmcp stdio MCP server, 29 tools (`winwright mcp`). Loopback HTTP dropped.
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
  (`node --test bridge/taint.test.mjs`). The bridge was deleted 2026-10-04, so nothing writes the
  marker today; the opt-in hook (ROADMAP Phase 19) was dropped by the user 2026-10-05.
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
- Ask the user to be hands-off first: `cargo test -p winwright-input --test live_input -- --ignored`
  and `cargo test -p winwright-cli --test live_canvas -- --ignored --test-threads=1` (move the real
  mouse). Last full live run 2026-10-05: fixture 7/7, mcp_stdio 2/2, input 2/2, canvas 2/2,
  confirm 2/2.
- Opt-in, puts two tiny files in the Recycle Bin: `cargo test -p winwright-files --lib -- --ignored replaced`.

## Next
The plan is `.gsd/ROADMAP.md` "Plan from 2026-10-04": all done (14-18, 20-22; 19 dropped by the user). Nothing is planned. For the user: restart Codex, opencode and Antigravity to load the 29-tool build, then try a lesson ("teach me how to ... in <app>"). Disk after the wrap-up: the 23.1 GB `target` folder deleted; kept: .rustup 1.2 GB and the cargo registry 0.3 GB (needed to rebuild), installed exe ~7 MB.
Direction (user, 2026-10-04): "just use winwright in the app, no need of a separate web page for anything". apps/jarvis deleted (bridge, page, voice, push-to-talk); `winwright` alone now prints an overview. Lost with it: voice, and the outside-content (taint) rule, which only the bridge switched on (`--taint-file` stays for any client that wants it).

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
- Capture: intermittent all-black region captures seen once by the capture agent (cause unknown).
- Overlays are clipped to one monitor and are visible in screen captures.
- Mixed-DPI multi-monitor untested (single monitor).
- A minimized Store app still reports ApplicationFrameHost.exe (its frame holds no app then).
- In-window dialogs count only when UI Automation marks them as dialogs (IsDialog); web modals
  exposed as plain panes are judged by the button name.
- Typing into a field that never shows the first keystroke falls back to typing at once (old
  behaviour) and lets the final check judge; each such call costs about 1 s.
- Engine-level fakes (engine_tests.rs) do not cover desktop_mouse, the new file ops, sessions or
  guides; those are covered by unit tests in their modules and by live runs. The guide's "Not
  there, keep waiting" loop was checked by hand only.
- Guides: a window screenshot does not show overlays (region and monitor captures do); a change
  step fires on any pixel change in its spot (animations, a blinking caret); Windows drops a
  low-level hook whose thread stalls past its timeout, and a guide would then time out.
- UI-driven execution: Enter in a terminal window or an editor's terminal (xterm.js, a field
  named "Terminal ..."), Enter in the Run box / Start search / Explorer address bar / Task
  Manager's "Run new task" on a shell command line, OK in the Run box or "Create new task" on
  one, a Start result for a typed shell command, and opening a .bat/.cmd/WSH/.hta file in
  Explorer are judged as Shell/PowerShell; Win+R and Win+X as PowerShell. Still open: scripts in
  Explorer when extensions are hidden (the item name has none), terminals that expose no UIA
  text field (JetBrains). Opening an empty shell window from Start or the taskbar is allowed:
  it runs nothing until Enter.
- Relaxed mode (2026-10-04): Sensitive is allowed; Destructive (now including spending and
  security phrases), file delete, process terminate and the shell still ask. The user's real
  config is `%APPDATA%\winwright\config.json` = relaxed + `allowShell: true` (2026-10-05).
  The taint rule is off (nothing writes the marker; the hook, Phase 19, was dropped).
- `app_launch` URI allowlist is fixed in code (http, https, mailto, ms-settings, shell:<folder>);
  no config for extra schemes yet.
- Protecting the exe folder (M3) means file operations are refused in the folder winwright.exe
  runs from (e.g. Downloads if run from there): install it in its own folder.

## Apps (updated 2026-10-04)
- Registered: Claude Code (`claude mcp add winwright --scope user`, ~/.claude.json), Codex (~/.codex/config.toml, env_vars passes APPDATA/LOCALAPPDATA/USERPROFILE/SystemRoot and the WINWRIGHT_*/JARVIS_* memory and Notion names, tool_timeout_sec 120), opencode v2 (~/.config/opencode/opencode.json `mcp.servers`), Antigravity (~/.gemini/config/mcp_config.json).
- Installed build: `~\.cargo\bin\winwright.exe`; reinstall with `cargo install --path crates/winwright-cli --locked` (rename the running exe aside first if an app holds it).
- User config `%APPDATA%\winwright\config.json`: `confirmationMode: relaxed`.
- Notion: JARVIS_NOTION_TOKEN + JARVIS_NOTION_PARENT user env vars (Winwright reads WINWRIGHT_NOTION_* first, then these).

## CHECKPOINT 2026-10-03 (history)
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

## Footprint
- 2026-10-04: apps/jarvis deleted (311 MB with node_modules); Winwright is the only thing that runs.
- Winwright: idles ~23 MB / 11 threads (debug, 2026-10-01). Release profile: opt-level "s" + thin LTO + 1 codegen unit + stripped (not panic=abort: window procs use catch_unwind); `cargo install` builds with it.
- Cleanup at the end: `cargo clean` (~11 GB).
