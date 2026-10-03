# Edits to the vendored JARVIS code

Upstream: https://github.com/adewaskar/jarvis (MIT). Only the bridge is kept; the interface is Winwright's own. Re-apply these when taking upstream bridge changes.

## Removed (2026-10-04)

The JARVIS face and everything only it used: `src/` (React + Three.js HUD), `index.html`, `vite.config.ts`, the `tsconfig*.json` files, `public/` (audio, favicon; the vendored MediaPipe runtime), `scripts/start.mjs` (ran Vite beside the bridge), `stubs/` (onnxruntime override for the in-browser voice model), `bridge/ui.mjs` (3D scene controls, `jarvis_ui`) and `bridge/vision.mjs` (camera, `jarvis_eyes`), plus `.claude/` (editor launch config). `package.json` keeps only `@anthropic-ai/claude-agent-sdk`, `dompurify` (served to the page), `ws`, `zod` and `oxlint`. The README is rewritten.

## Added

- `winwright-ui/` (`index.html`, `style.css`, `app.js`): the chat page.
- `bridge/winwright-face.mjs`: the system prompt (`WINWRIGHT_PROMPT`, replacing JARVIS's) and static serving of exactly the page's files plus DOMPurify, with a strict CSP and a local-Host check. Tests: `bridge/winwright-face.test.mjs`.
- `bridge/taint.mjs`: the taint rule's tool classification and per-conversation marker. Tests: `bridge/taint.test.mjs`. Run all with `npm test`.

## Changed in `bridge/server.mjs`

- `winwrightServer(taintFile)` adds the `winwright` MCP server when `JARVIS_WINWRIGHT_EXE` is set, passing `--taint-file`; each connection makes a `taintMarker()`, adds its own `winwright` entry to `mcpServers`, marks it in a `PreToolUse` hook before any tool that brings outside content (refusing the tool if the marker cannot be written), and clears it on disconnect.
- `decideTool` passes the configured `winwright` server through; the `jarvis_ui` and `jarvis_eyes` cases are gone.
- `originAllowed` accepts only the bridge's own port (plus `JARVIS_ALLOWED_ORIGINS`); the Vite dev-port ranges are gone.
- `serveFace` answers before the other routes; `WINWRIGHT_PROMPT` is the system prompt; the camera's ask/reply plumbing is gone; a startup line prints the page URL.
- Idle shutdown (`JARVIS_IDLE_MINUTES`); `POST /ptt` relay (`JARVIS_PTT_TOKEN`) and the `timingSafeEqual` import.

## Other

- `bridge/chrome.mjs`: one comment no longer refers to the removed `ui.mjs`.
- `.env.example`: bridge settings only.
