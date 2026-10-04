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
| 15 | Docs say apps-only: INTEGRATION.md rewritten as "Winwright and the app's other tools" (no JARVIS, bridge, push-to-talk or ElevenLabs), STATE.md footprint and Next trimmed, README checked, stale memory notes removed | `grep -ri jarvis .gsd README.md` finds only history lines | planned |
| 16 | Acceptance in the real apps, with the user at the PC. Claude Code first (connected 2026-10-04), then Codex, opencode, Antigravity after a restart | Notepad launched and a line typed, verified on the first try; a test file on the Desktop created then deleted with one click on Allow; memory_save called without being asked; desktop_screenshot of a window read by the model | planned (needs the user) |
| 17 | Vision and coordinates (reworked Phase 10): click/double/right-click and drag at a point inside a named window for apps with no UI tree (games, canvases); results flagged `vision`; the risk judged by the element under the point; `desktop_drag` between elements (spec §11 drag_to; the input crate can already drag) | `live_canvas` driven through MCP by coordinates; drag works on the fixture; a game window screenshot answers "what should I do next" | planned |
| 18 | Known-gap fixes: type with the target window's keyboard layout; UWP windows report the real app process, not ApplicationFrameHost; in-window dialogs (WinUI ContentDialog) give dialog context to Yes/OK; Win32 list/tab selection sends the app's change notification | a unit or live test per fix | planned |
| 19 | Outside-content rule inside the apps (opt-in): a Claude Code PreToolUse hook writes the `--taint-file` marker after web or other MCP reads, so later desktop changes ask | hook live-tested in Claude Code; off unless the user turns it on (it adds prompts to relaxed mode) | ask the user |
| 20 | Wrap-up: `scripts/check.ps1 -Live`, reinstall, push (only when asked), `cargo clean` (~11 GB), delete scratch and temp files | tree green; only source, config and reports remain on disk | planned |

Not planned: mixed-DPI testing (one monitor), a config list for extra URI schemes (add one when a
real need appears), local speech models, Jev, ElevenLabs.

For the user (outside the code): restart Codex, opencode and Antigravity for the new build;
optionally delete the "Winwright memory test" Notion page and the unused `ELEVENLABS_API_KEY`
user env var.

Standing rules: no live desktop test while the user is busy at the PC without asking; push only when
asked; secrets never pass through chat; keep disk and RAM use minimal; one PowerShell command per call.
