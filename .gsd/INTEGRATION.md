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

## Memory and reports (Notion) — planned 2026-10-02
Jev (classifier/router) was considered and dropped. Not building a classifier; JARVIS keeps its fixed effort, and Winwright stays the only safety authority.

- **Decision 2026-10-02: Notion stays the cloud memory (user's choice); no local-only/opt-in downgrade.** Reports are still written as markdown files first. After each task JARVIS writes a short report to `reports/YYYY-MM-DD-<topic>.md` in the repo (what was asked, what was done, which tools ran, outcome, anything denied). The file is the permanent record and works with no Notion.
- **Notion mirrors them** through a Notion MCP server from the user's Claude Code config (no new code to talk to Notion; JARVIS passes it through its normal gate, writes need JARVIS_ALLOW_WRITES). Each report becomes a page under one fixed parent page; the page body is the same markdown.
- **Memory read-back:** before a turn, JARVIS may load the few most recent or most relevant reports (from disk, or from Notion if present) as context. It is labeled as data, never instructions, and capped in size.
- **What never goes in a report:** screenshots, typed text, password-field values, anything Winwright redacts, full file contents. Reports hold summaries and names only.
- **Owner:** JARVIS owns reports and memory (conversation layer). Winwright is not involved except that its audit log stays the separate, authoritative record of desktop actions.
- **Steps:** (1) report writer in the bridge (turn end -> markdown file); (2) memory loader with size cap and data-label; (3) Notion mirror via the MCP server, off unless configured; (4) live check: do a task, see the file, see the Notion page, ask a follow-up that needs the memory.
- **Open:** user to confirm a Notion MCP server is connected in Claude Code and which parent page to use.

## Decisions and changes from the plan review (2026-10-02)

**Speech: ElevenLabs.** Chosen over local Whisper/Kokoro (too heavy) and Brave's missing browser speech. With a key, JARVIS uses ElevenLabs Scribe for input and an ElevenLabs voice for output; nothing runs locally. The user supplies the key later and sets it themselves as a user environment variable, `ELEVENLABS_API_KEY` (e.g. PowerShell: `[Environment]::SetEnvironmentVariable('ELEVENLABS_API_KEY','<key>','User')`, then restart the terminal). `winwright assistant` passes it to the bridge; it is never written to the repo, config or chat. Audio goes to ElevenLabs while the hotkey is held. Brave then works.

**Order (feature freeze until green).**
1. Get the workspace compiling and passing (`scripts/check.ps1`), commit the agents' fixes in chunks. No new features before this.
2. Security fixes (the audit list in STATE.md), starting with `app_launch`.
3. Taint rule (below).
4. Live tests, one at a time, then JARVIS end to end, then ElevenLabs.
5. Reports/Notion, last.

**Taint rule (web -> desktop injection).** Once the session has read untrusted content (a web page via Playwright or `jarvis_chrome`, a Notion page, memory), any desktop action that changes something (type, press, launch, file ops, shell, window close) needs the native confirmation, until the user clears it (new conversation, or Winwright re-enable). Winwright enforces it: `winwright mcp` accepts a `taint` signal from the bridge (the bridge marks it when a web or Notion tool result arrives). Reads (snapshot, find, screenshot) stay free.

*Mechanism (decided 2026-10-03).* The SDK owns the MCP pipe, so the signal is a per-conversation file. The bridge starts each conversation's server as `winwright mcp --taint-file %LOCALAPPDATA%\winwright\taint\<random id>` (a folder Winwright's own file tools refuse, M3) and creates that file when `canUseTool` first allows an untrusted tool, before it runs. Untrusted = WebFetch, WebSearch, Task/Agent (sub-agents can fetch), MCP resource reads, `jarvis_chrome`, and every MCP server other than `winwright`, `jarvis`, `jarvis_ui` and `jarvis_eyes` (so Playwright, Notion, search and any server added later). Local file reads, the camera and Winwright's own reads do not taint. Winwright checks the file before each state-changing action and latches the taint in memory (deleting the file does not undo it); any Allow then becomes Confirm with the reason in the dialog, Deny stays Deny, ReadOnly stays free. Cleared by a new conversation (new process, new file) or the tray's Re-enable (clears the latch and removes the file). The bridge removes its file when the connection closes.

**Pruned.** Dropped: Phase 1b (named-pipe service; the MCP process already persists), Phase 12 (recorder), Phase 9 (own browser bridge), and any local speech models. Kept: Phase 7 acceptance, Phase 10 (vision, screen pixels only).

**Smaller.** `scripts/check.ps1` is the one command for build, lint and tests (`-Live` adds the live tests). JARVIS shows a reconnect message after an idle shutdown. Every edit to the vendored JARVIS code is listed in `apps/jarvis/WINWRIGHT-EDITS.md`.

## Own assistant UI (decided 2026-10-04)

The user does not like the JARVIS face (React + Three.js HUD). Replace it with our own page; keep the bridge.

**Decisions (user):** clean chat panel; voice and typing; plain HTML/CSS/JS with no framework, served by the bridge itself; the assistant is called Winwright.

**Spec.**
- `winwright assistant` runs only the bridge (`node bridge/server.mjs`, no Vite, no 3D) with `WINWRIGHT_FACE=1`, and prints `http://localhost:8787/`.
- With `WINWRIGHT_FACE=1` the bridge: serves the page (`/` and `/ui/*`, plus DOMPurify from node_modules) with a strict CSP; accepts its own origin on the socket and HTTP routes; uses a Winwright system prompt (JARVIS's desktop, routing, brevity and reporting rules; no blades/theme/camera); leaves out `jarvis_ui` (3D scene controls) and `jarvis_eyes` (camera). `display`/`blade` stay and render as cards in the chat. Without the flag the bridge behaves exactly as before.
- Page: header with name and status (connecting, ready, listening, thinking, working, speaking, offline); conversation with user bubbles, streamed replies, tool activity lines with plain labels, error lines, and cards for blades (markup sanitised with DOMPurify and the `hud-*` class allowlist; images/video via the bridge's `/img`, `/media`, `/file`; articles via `/page`; YouTube/Vimeo embeds only); a text box (Enter sends, Shift+Enter new line) and a mic button.
- Voice: Ctrl+Space (relayed `ptt`) or the mic button starts listening; recording stops after a pause in speech, a second press, or 30 s; audio goes to `/stt`; replies to a spoken request are read out with `/tts` (typed ones stay silent). A new request while a turn runs interrupts it. Without an ElevenLabs key the mic is disabled with a hint.
- Light and dark themes follow the system; usable at phone width; keyboard reachable; reduced motion respected.

**Plan.** (1) Bridge: `bridge/winwright-face.mjs` (prompt, static serving, CSP) and the flagged wiring in `server.mjs`. (2) Page: `winwright-ui/index.html`, `style.css`, `app.js`. (3) Launcher change and checks: node tests for the face module, a typed end-to-end turn in a browser, then voice with the user.

**Update 2026-10-04: old face deleted.** At the user's request the JARVIS face is gone (src/, Vite, Three.js, MediaPipe, audio, scripts/start.mjs, stubs/, bridge/ui.mjs, bridge/vision.mjs), so the page is no longer behind a flag: the bridge always serves it, always uses `WINWRIGHT_PROMPT`, and accepts only its own origin (the Vite dev-port allowance is removed). `winwright assistant` finds `apps/jarvis` by `bridge/server.mjs`. Dependencies: the Agent SDK, dompurify, ws, zod (+ oxlint); node_modules went from ~814 MB to ~310 MB (the SDK binary is most of it). Details in `apps/jarvis/WINWRIGHT-EDITS.md`.
