# Edits to the vendored JARVIS code

Upstream: https://github.com/adewaskar/jarvis (MIT). Everything else is unmodified; re-apply these when updating.

- `bridge/server.mjs`: `winwrightServer()` adds the `winwright` MCP server when `JARVIS_WINWRIGHT_EXE` is set; `decideTool` passes that server through; `SYSTEM_PROMPT` gains the DESKTOP and ROUTING paragraphs; idle shutdown (`JARVIS_IDLE_MINUTES`); `POST /ptt` relay (`JARVIS_PTT_TOKEN`) and the `timingSafeEqual` import.
- `bridge/taint.mjs` (new) and `bridge/taint.test.mjs` (new; `node --test bridge/taint.test.mjs`): the taint rule's tool classification and per-conversation marker. `bridge/server.mjs`: `winwrightServer(taintFile)` passes `--taint-file`; each connection makes a `taintMarker()`, adds the per-conversation `winwright` entry to `mcpServers`, marks it in a `PreToolUse` hook before any tool that brings outside content (refusing the tool if the marker cannot be written), and clears it on disconnect.
- Winwright's own page (`winwright assistant` sets `WINWRIGHT_FACE=1` and runs only `node bridge/server.mjs`, no Vite): `winwright-ui/` (new: `index.html`, `style.css`, `app.js`), `bridge/winwright-face.mjs` (new: the page's system prompt, static serving of exactly those files plus DOMPurify from node_modules, CSP, local-Host check) and `bridge/winwright-face.test.mjs` (new). `bridge/server.mjs`, all behind the flag: own origin accepted in `originAllowed`, `serveFace` before the routes, `WINWRIGHT_PROMPT` as the system prompt, `jarvis_ui` and `jarvis_eyes` left out of `mcpServers`, and a startup line with the page URL. Without the flag the bridge and the React face behave as upstream.
- `src/App.tsx`: no always-on listening (dormant = deaf, mic opened by `pttPress`/`ensureListening`, released by `stopListening`); clap listener removed; Space and the relayed hotkey call `pttPress`.
- `src/lib/audio.ts`: `releaseMic()`. `src/lib/bridge.ts`, `src/lib/brain.ts`: `watchPtt`.
- `src/scene/Scene.tsx`: low-power GPU, `dpr` capped at 1.5.
- `package.json`: Picovoice deps removed; `onnxruntime-node` overridden by `stubs/onnxruntime-node`; `onnxruntime-common` pinned.
- Removed: `.claude/` (editor launch config).
