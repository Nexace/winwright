# Winwright + JARVIS + Playwright: who does what

One rule: **every capability has exactly one owner.** If two tools can do a job, the routing rule below picks one and the other is told not to.

## Roles

| Piece | Owns | Never does |
|---|---|---|
| **Winwright** (`winwright mcp`) | Native Windows apps and the OS shell: windows, UI Automation read/click/type, OS dialogs (file pickers, Save As, permission prompts), the browser's *own chrome* (tabs strip, address bar, downloads bar), desktop screenshots, overlays, app launch, file ops, shell exec. **The safety authority**: native Allow/Deny dialog, default-deny policy, Ctrl+Alt+Esc stop, audit log. | Web page content (DOM), voice, language models, camera, phone. |
| **Playwright MCP** | Web page content in an automation browser it controls: DOM snapshot, click, type, navigate, wait, forms, tests, headless runs, a fresh or isolated profile. | Native windows, OS dialogs, the user's own logged-in browser session. |
| **JARVIS** | The conversation: wake word, speech in/out, the HUD, display panels, camera (`jarvis_eyes`), gestures, orchestration of the other servers through Claude Code. Also `jarvis_chrome`: reading and light actions in the **user's own live, logged-in browser** through the Claude extension. Phone via the `android` MCP, search/image tools as before. | Operating Windows apps itself, deciding desktop safety, owning a second automation engine. |

## Routing rule (the model is told this; see `DESKTOP` in `apps/jarvis/bridge/server.mjs`)

1. Target is a **native app or OS dialog** -> Winwright.
2. Target is **content inside a web page**:
   - needs the user's real logins, tabs or history -> `jarvis_chrome` (read-heavy; its acting tools stay behind `JARVIS_ALLOW_WRITES`);
   - automation, scraping, testing, repeated or background work -> Playwright.
3. Target is the **browser's own UI** (address bar, download prompt, file picker, permission bubble, window management) -> Winwright.
4. A task that crosses the boundary (e.g. web form -> native Save As) is **handed over, not duplicated**: Playwright/`jarvis_chrome` finishes the page part, then Winwright takes the dialog. No tool tries to drive the other's surface.
5. **Screenshots**: Winwright `desktop_screenshot` = pixels of a window/monitor; `chrome_screenshot`/Playwright = a page; `jarvis_eyes` = the camera. Pick by what is being looked at.
6. **Approval**: only ever by the person at the keyboard via Winwright's dialog. JARVIS never speaks, simulates or relays an approval; voice cannot approve.

## Overlaps removed from the old plan

- **Phase 9 (Winwright's own Playwright/CDP bridge): dropped.** Playwright MCP and `jarvis_chrome` already exist; a third browser engine inside Winwright would duplicate them. Replaced by the handoff in rule 4: Winwright detects browser windows and its tool descriptions point web-page tasks to Playwright.
- **Phase 13 (native Winwright assistant app): replaced by JARVIS.** Winwright keeps no model, voice or speech code; it stays a model-free engine.
- **Phase 10 (vision grounding)** stays in Winwright but is scoped to *screen pixels of non-UIA apps*; it has nothing to do with `jarvis_eyes` (camera).
- **UI**: Winwright's tray and Inspector are for safety and debugging; JARVIS's HUD is for conversation. Neither re-implements the other. Confirmation dialogs stay native Winwright.
- **Permission gates**: Winwright is trusted to gate itself (JARVIS passes it through); Playwright, `jarvis_chrome` and all other servers keep JARVIS's own default-deny gate. No double prompts, no gaps.

## Wiring (what exists / what is next)

Done: `winwright assistant` starts JARVIS with Winwright attached; idle shutdown (10 min) on both; JARVIS passes Winwright's tools through its gate.

Next, in order (small):
1. **Routing text**: the rule above in JARVIS's system prompt (done) and in `INSTRUCTIONS` of `crates/winwright-mcp/src/lib.rs` plus the `desktop_*` tool descriptions ("for web page content use Playwright").
2. **Playwright availability**: `winwright assistant` checks that a `playwright` server exists in the user's Claude Code MCP config and says how to add it if not (no auto-install).
3. **One stop**: the tray "Stop" and Ctrl+Alt+Esc also tell the JARVIS bridge to cancel the running turn (so it stops planning, not just acting); Resume re-enables both.
4. **Browser-window awareness**: `desktop_windows` marks browser windows so the model knows to route page work to Playwright/`jarvis_chrome`.
5. **One launcher**: `winwright assistant` is the only start command; it exports `JARVIS_WINWRIGHT_EXE` and shares the idle timer setting.
6. **Audit**: Winwright's audit log stays the record of desktop actions; JARVIS logs conversation turns separately. No merged log (different trust levels).
7. **Live check**: one end-to-end run: "open Notepad and type X" (Winwright), "search this on the web" (Playwright), "read my open tab" (`jarvis_chrome`), "save this page" (page by Playwright, Save As dialog by Winwright), then a Ctrl+Alt+Esc stop mid-task.

## Push-to-talk (2026-10-01)
Wake-word listening is removed; JARVIS's other features are untouched. Winwright (`winwright assistant`) registers the global hotkey (default Ctrl+Space) and POSTs `/ptt` to the bridge with a per-run token; the bridge relays `{type:'ptt'}` to the page, which opens the mic and starts a turn, and releases the mic when dormant. The ignition-screen clap listener is also removed (it held the mic open). Speech-to-text engine unchanged (ElevenLabs if keyed, else the browser's). Open: true hold-to-talk (end on key release) needs key-up detection; a later change. Brave has no browser speech recognition, so without an ElevenLabs key voice input will not work in Brave.
