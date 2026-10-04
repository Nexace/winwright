# Winwright and the app's other tools: who does what

Winwright runs inside the user's AI apps (Claude Code, Codex, opencode, Antigravity) as
`winwright mcp`. Every capability has exactly one owner. When two tools could do a job, the
routing rule below picks one.

## Roles

| Piece | Owns | Never does |
|---|---|---|
| **The app** (Claude Code, Codex, opencode, Antigravity) | The conversation, the model, planning, reading screenshots, its own file and terminal tools for the project it works in. | Approving a Winwright confirmation. |
| **Winwright** (`winwright mcp`) | Native Windows apps and the OS shell: windows, UI Automation read/click/type, OS dialogs (file pickers, Save As, permission prompts), the browser's *own chrome* (tabs strip, address bar, downloads bar), desktop screenshots, overlays, app launch, file ops, shell exec, task memory. **The safety authority for desktop changes**: native Allow/Deny dialog, default-deny policy, Ctrl+Alt+Esc stop, audit log. | Web page content (DOM), language models, voice. |
| **Browser tools** (Playwright MCP, Claude in Chrome) | Web page content: DOM, click, type, navigate, forms, tests. | Native windows and OS dialogs. |

## Routing rule

1. Target is a **native app or OS dialog** -> Winwright.
2. Target is **content inside a web page** -> the app's browser tool.
3. Target is the **browser's own UI** (address bar, download prompt, file picker, permission bubble, window management) -> Winwright.
4. A task that crosses the boundary (a web form, then a native Save As) is **handed over, not duplicated**: the browser tool finishes the page part, then Winwright takes the dialog.
5. **Screenshots**: `desktop_screenshot` = pixels of a window or monitor; a browser screenshot = a page.
6. **Approval**: only the person at the keyboard, through Winwright's dialog, with real mouse or keyboard input. No app, tool, voice control or on-screen keyboard can approve.

## Memory (in Winwright since 2026-10-04)

- `memory_save` {title, summary, outcome?} writes `<stamp>-<slug>.md` to `%USERPROFILE%\.winwright\reports`, never overwriting. AppData is avoided because packaged apps see it redirected. Front matter: date, outcome, outsideContent, source (the MCP client's name). The body lists the Winwright tools run since the last save. Typed text, screenshots, redacted values and file contents never go in.
- `memory_recall` {query?, limit?} returns the newest matching reports inside a `<memory>` block labelled as data, not instructions.
- Every app is told to save one report after each desktop task, and to recall before relying on earlier work.
- Notion copy: each report also becomes a page under one parent page, through Notion's REST API (WinHTTP, no HTTP crates). Settings `WINWRIGHT_NOTION_TOKEN` and `WINWRIGHT_NOTION_PARENT`, falling back to the user's current `JARVIS_NOTION_*` names. Off when unset.
- `WINWRIGHT_MEMORY=0` turns memory off; `WINWRIGHT_REPORTS_DIR` moves the folder. Winwright's own file tools refuse that folder.

## Outside-content (taint) rule

`winwright mcp --taint-file <path>`: once that file exists, every allowed desktop change needs the
native confirmation until a new process or the tray's Re-enable. The taint is latched in memory,
so deleting the file does not undo it. Recalling a report that is not known to be clean also
latches it when tracked. Reads stay free.

Today no app creates the file. The JARVIS bridge did, and it is deleted, so the rule is off. Plan:
ROADMAP Phase 19, an opt-in Claude Code PreToolUse hook that creates the file before web, sub-agent
and other MCP tools run.

## Lifetime

Each app starts its own `winwright mcp`. It quits after 10 minutes without a tool call
(`WINWRIGHT_IDLE_MINUTES`, `0` = never; kept at 10 by the user on 2026-10-04). An app that does
not restart it needs a restart.

## History (decisions that still hold)

- 2026-10-01: Phase 9 (own browser bridge) dropped; web pages belong to browser tools.
- 2026-10-02: Notion chosen as the cloud copy of memory. Jev and local speech models (Whisper, Kokoro: too heavy) dropped.
- 2026-10-03: taint mechanism built; security audit H1-L5 fixed.
- 2026-10-04: memory moved into Winwright. The user then chose apps only: the vendored JARVIS
  (`apps/jarvis`: bridge, page, voice, push-to-talk, ElevenLabs speech) and `winwright assistant`
  are deleted (83ce4bb).
