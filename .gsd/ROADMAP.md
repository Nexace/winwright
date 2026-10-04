# Winwright Roadmap

Source of truth for requirements: [SPEC.md](SPEC.md) (§57 phase plan, §63 implementation rules).
Each phase keeps the workspace building, adds tests before behavior, and lands as atomic commits.

## Direction (2026-10-04)
Winwright is an MCP server that runs inside the user's AI apps: Claude Code, Codex, opencode and
Antigravity. The app's model does the thinking and the talking; Winwright is its hands on the
Windows desktop and the safety authority for every desktop change. There is no assistant, web page,
voice or browser engine of our own. Web pages belong to the app's browser tools (Playwright MCP,
Claude in Chrome). JARVIS (`apps/jarvis`, `winwright assistant`, push-to-talk) was deleted on
2026-10-04 (83ce4bb) and is not coming back.

## Phases 0-13 (original spec plan)
| Phase | Scope | Status |
|---|---|---|
| 0 | Workspace, contracts, typed errors, config, logging, sessions/refs, lease, redaction, default-deny policy | done |
| 1 | Win32 windows + DPI, MTA UIA worker, compact snapshot + refs, inspect | done |
| 1b | `winwright serve` over a named pipe | dropped (apps start `winwright mcp` themselves) |
| 2 | Semantic locators, ranking, ambiguity, Win32 fixture | done |
| 3 | Pattern-first actions | done |
| 4 | wait_for, UIA events, verification, snapshot diff | done |
| 5 | rmcp MCP server | done (stdio); loopback HTTP dropped: no app needs it |
| 6 | WGC capture + native overlays | done (125% verified; mixed DPI needs a second monitor) |
| 7 | SendInput physical fallback | done (`live_canvas`, `live_input`) |
| 8 | Permissions, confirmations, audit, emergency stop, elevation | done (+ audit H1-L5, relaxed mode) |
| 9 | Playwright/CDP bridge | dropped: the app's browser tools own web pages |
| 10 | Vision fallback | reworked as Phase 17 (the app's model is the vision; Winwright stays model-free) |
| 11 | Native tray, Inspector, confirm dialog | done |
| 12 | Recorder / codegen | dropped |
| 13 | Own assistant (voice, page) | dropped 2026-10-04; memory moved into Winwright (memory_save/recall + Notion copy) |

## Plan from 2026-10-04 (rebuilt from both earlier chats)
| Phase | Scope | Acceptance | Status |
|---|---|---|---|
| 14 | Command-gate gaps: OK in an open Run box, Start search "Run command" results, script files opened from Explorer, terminals inside other apps (VS Code xterm), Task Manager "Run new task" | engine tests: each path asks like Shell/PowerShell; opening a shell's own window or a plain file stays allowed | done (df7b5e0) |
| 15 | Docs say apps-only: INTEGRATION.md rewritten as "Winwright and the app's other tools" (no JARVIS, bridge, push-to-talk or ElevenLabs), STATE.md footprint and Next trimmed, README checked, stale memory notes removed | `grep -ri jarvis .gsd README.md` finds only history lines | done (2026-10-04; memory notes were already current) |
| 16 | Acceptance in the real apps, with the user at the PC. Claude Code first (connected 2026-10-04), then Codex, opencode, Antigravity after a restart | Notepad launched and a line typed, verified on the first try; a test file on the Desktop created then deleted with one click on Allow; memory_save called without being asked; desktop_screenshot of a window read by the model | done in Claude Code (2026-10-04): Allow with one click (a timeout first, while the user was typing); screenshot read; memory_save without being asked (file + Notion) and recall. Typing first failed in Windows 11 Notepad: its text services queue keys while busy and replay them against the keyboard state of that moment, so batched Unicode packets came out as the last character repeated, real keys lost Shift, chords lost Ctrl (one Ctrl+W closed an unmodified tab). Winwright flagged every failure (`verified: false`). Waiting for the window's thread (WM_NULL) and typing real keys were tried and did not help. Fix 424a4f3: documents get one keystroke at a time, each sent once the text shows the one before; live: 80 characters exact on the first try in 1.3 s, Ctrl+A and Ctrl+W right. Codex, opencode and Antigravity: the user checks after restarting them |
| 17 | Cursor and coordinates (reworked Phase 10; the user asked 2026-10-04 to move the cursor): a `desktop_mouse` tool to move the cursor, click/double/right-click, drag and scroll at a point, given relative to a named window or the screen, for apps with no UI tree (games, canvases) and for pointing things out; the element under the point decides the risk; results flagged `vision`; `desktop_drag` between elements (spec §11 drag_to; the input crate can already drag) | `live_canvas` driven through MCP by coordinates; drag works on the fixture; a game window screenshot answers "what should I do next" | done 9d8947f (2026-10-04): one `desktop_mouse` tool (move/click/drag/scroll; screen pixels or pixels of a window's screenshot); target = element under the point, risk = it or its nearest clickable ancestor, Explorer drags always ask, own windows refused. Live: `phase17_mouse_acts_by_position` + phase7 canvas pass. Drag between elements is a drag between their points (find gives bounds); no separate tool |
| 21 | Desktop Commander's features, rebuilt natively behind Winwright's gates (the user asked 2026-10-04): read files (size-capped; secret files ask) and several at once; write, append and search-and-replace edits (the previous version goes to the Recycle Bin, so relaxed mode can allow it; strict and balanced ask); content search; long-running and interactive processes (start, send input, read output, list, stop), where every command sent to a shell asks, as `shell_execute` does; kill a process (always asks). Left out: a tool changing Winwright's own config (the AI must not loosen its own safety), usage stats, prompts, feedback, PDF writing | engine tests per tool; a live run: edit a file, run a build in a session and read its output, each risky step asking once | done 2026-10-05, installed: 21a text files 1ed7a51 (filesystem_operation read/write/edit/grep; Recycle Bin before replace/edit; secrets ask; checked over MCP stdio); 21b process_terminate af65a24 (always asks; Windows/services/Winwright refused; reused ids never ended); 21c process_session 4e2ee62 (start/input/read/list/stop; start and every input ask; kill-on-close jobs; emergency stop ends all; tested with real cmd and ping). The user turned allowShell on (relaxed, every command asks). Not yet run through an app with the Allow window |
| 18 | Known-gap fixes: the per-keystroke wait for fields with a readable value too (WinUI text boxes may queue keys like Notepad; today only documents wait); type with the target window's keyboard layout; UWP windows report the real app process, not ApplicationFrameHost; in-window dialogs (WinUI ContentDialog) give dialog context to Yes/OK; Win32 list/tab selection sends the app's change notification | a unit or live test per fix | done 2026-10-05, installed: 4e89bf7 text fields one keystroke at a time (a field that never shows the first key falls back to typing at once), c3736e3 target window's keyboard layout, c59e709 Store apps report their own process, 20d0076 dialogs inside a window give Yes/OK their context, ca7a7e5 Win32 list/tab items clicked for real. Live 2026-10-05: live_fixture 7/7, mcp_stdio 2/2, live_input 2/2, live_canvas 2/2, live_confirm 2/2 |
| 19 | Outside-content rule inside the apps (opt-in): a Claude Code PreToolUse hook writes the `--taint-file` marker after web or other MCP reads, so later desktop changes ask | hook live-tested in Claude Code; off unless the user turns it on (it adds prompts to relaxed mode) | dropped (user, 2026-10-05: not needed); `--taint-file` stays for any client that wants it |
| 22 | Teaching mode (the user asked 2026-10-05: "can winwright teach me Lightroom", live, "similar to Clicky"): a `pointer` overlay style (a pointer with the caption in a bubble beside it); `overlay_highlight` also takes a spot by pixels (x/y of a window's screenshot) for apps with no UI tree; `desktop_guide` shows steps one at a time and waits for the person's own click inside each spot (a low-level mouse hook only while waiting; injected clicks ignored; keys never read), or for the spot's pixels to change (keyboard steps); visible steps advance without the model; a click elsewhere, a timeout or an emergency stop ends it; the result says where they clicked and carries a screenshot. Nothing is clicked for them | unit tests for the pointer layout, click judging and the hook's event filter; live: the hook installs and ignores injected clicks, a pointer is drawn | planned |
| 20 | Wrap-up: `scripts/check.ps1 -Live`, reinstall, push (only when asked), `cargo clean` (~23 GB), delete scratch and temp files and the old renamed winwright.old.*.exe | tree green; only source, config and reports remain on disk | planned |

Not planned: mixed-DPI testing (one monitor), a config list for extra URI schemes (add one when a
real need appears), local speech models, Jev, ElevenLabs.

For the user (outside the code): restart Codex, opencode and Antigravity for the new build;
optionally delete the "Winwright memory test" Notion page and the unused `ELEVENLABS_API_KEY`
user env var.

Standing rules: no live desktop test while the user is busy at the PC without asking; push only when
asked; secrets never pass through chat; keep disk and RAM use minimal; one PowerShell command per call.
