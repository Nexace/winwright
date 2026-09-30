# Winwright Roadmap

Source of truth for requirements: [SPEC.md](SPEC.md) (§57 phase plan, §63 implementation rules).
Each phase keeps the workspace building, adds tests before behavior, and lands as atomic commits.

| Phase | Scope | Acceptance | Status |
|---|---|---|---|
| 0 | Cargo workspace, owned contracts + backend traits, typed errors, config, logging, session/ref authority skeleton, action lease, cancellation context, redaction, default-deny policy, CLI version | workspace builds on MSVC; `cargo test` runs; `winwright --version` | done |
| 1 | Win32 window list/active window/DPI; dedicated MTA UIA worker; raw tree capture; compact snapshot + refs; inspect | Notepad / Settings / Explorer trees readable; interactive controls get refs | done |
| 1b | `winwright serve` per-user engine over current-user named pipe; `--session` refs across CLI processes | `snapshot --session demo` then `inspect --session demo e14` | planned |
| 2 | Semantic locators: role, name, text, AutomationId, label, class, framework, ancestor, nth; ranking + ambiguity; Win32 fixture app | `winwright find --role Button --name Save` resolves fixture controls | done |
| 3 | Pattern-first actions: click, fill, focus, select, toggle, expand/collapse, scroll, press, hotkey | fixture workflows with no coordinate clicks | done |
| 4 | wait_for, UIA event subscriptions, action verification, snapshot diff | no fixed sleeps in fixture workflows | planned |
| 5 | rmcp MCP server (stdio + loopback HTTP) | MCP model operates Notepad semantically | planned |
| 6 | WGC capture + native no-activate overlays | element highlight accurate at mixed DPI | planned |
| 7 | SendInput physical fallback | custom canvas fixture controlled physically | planned |
| 8 | Permission engine, confirmations, audit, emergency stop, elevated-app detection | passwords never returned; stop halts queued work | planned |
| 9 | Optional Playwright/CDP browser bridge | DOM + native dialog in one workflow | planned |
| 10 | VisionGrounder fallback | visual-only target found + clicked, flagged as vision | planned |
| 11 | Native desktop UX, no WebView (user decision: keep usage minimal): tray icon only while running (Active/Stopped, Stop, Re-enable, Open Inspector); native Inspector window (UIA tree + properties, pick under cursor, live highlight, copy locator/ref); native Allow/Deny confirmation dialogs for risky actions. Tauri dropped unless requested later. | tray + inspector work against fixtures; confirmation approvals only from the dialog | planned |
| 12 | Recorder / codegen | recorded Notepad save replays semantically | planned |
| 13 | Assistant conversation mode (user request; spec §2 experience, §59 post-MVP): separate `winwright-assistant` app on top of the engine (engine stays model-free). Hold hotkey (default Ctrl+Space) -> local Windows speech-to-text -> Claude via the Anthropic API (user-provided key in an env var, never handled by us) with Winwright tools -> acts on screen with highlights -> spoken reply via local Windows text-to-speech; multi-turn memory; same confirmations + emergency stop. User must be told UI text/screenshots go to the API; voice stays local. | spoken request completes a multi-step task on the fixture; confirmations still gate risky steps | planned |
