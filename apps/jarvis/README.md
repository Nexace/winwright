# Winwright assistant

A chat and voice assistant for this PC. You type or talk; Claude Code (run as a
library, on your existing Claude Code login) does the thinking; Winwright does
the work on the Windows desktop, asking you in its own Allow/Deny dialog before
anything risky.

This folder started as [JARVIS](https://github.com/adewaskar/jarvis) (MIT). Its
Node bridge is kept; its 3D interface is replaced by a plain chat page that the
bridge serves itself. What changed is listed in `WINWRIGHT-EDITS.md`.

## Run it

Install once, from the Winwright checkout (puts `winwright` on your PATH via
`~\.cargo\bin`; run it again after changing the Rust code):

```
cargo install --path crates/winwright-cli --locked
```

Then type `winwright` in any terminal. It starts the assistant and opens
<http://localhost:8787/> in your browser. Type, or press **Ctrl+Space**
(anywhere, any app) and talk; it stops listening when you pause. **Esc** stops a
reply. Ctrl+C in the terminal stops it; it also stops by itself after 10 idle
minutes.

First time only: `npm install` in this folder.

## Needs

- Claude Code, installed and logged in (`claude` once). No API key.
- Node.js 20 or newer.
- Optional: an ElevenLabs API key in the `ELEVENLABS_API_KEY` user environment
  variable, for voice. Without it, type. See `.env.example` for every setting.

## What is here

- `bridge/server.mjs`: the agent session, the WebSocket, speech proxies
  (`/stt`, `/tts`), media proxies (`/img`, `/media`, `/file`, `/page`), and the
  push-to-talk relay.
- `bridge/winwright-face.mjs`: the assistant's system prompt and the page's
  file serving (strict CSP, local hosts only).
- `bridge/taint.mjs`: marks a conversation once it has read outside content, so
  Winwright asks before every desktop change after that.
- `bridge/reports.mjs`: after each task, a short report in `reports\` at the
  checkout root (question, tools, answer; never what was typed). A new
  conversation starts with the newest five as memory, marked as data; answers
  given after reading a web page are left out of memory.
- `bridge/panels.mjs`, `bridge/chrome.mjs`, `bridge/net.mjs`, `bridge/page.mjs`:
  cards, the user's Chrome (via the Claude extension), and safe fetching.
- `winwright-ui/`: the page (HTML, CSS, JS; no framework, no build).

`npm test` runs the bridge tests; `npm run lint` lints.
