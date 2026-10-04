// Winwright's chat page. Served by the JARVIS bridge (WINWRIGHT_FACE=1), which
// owns the agent, Winwright's desktop tools, the taint rule and the ElevenLabs
// speech proxy; this page only talks to it over its socket and HTTP routes.
'use strict'

const $ = (selector) => document.querySelector(selector)
const log = $('#log')
const welcome = $('#welcome')
const form = $('#composer')
const input = $('#input')
const micButton = $('#mic')
const sendButton = $('#send')
const statusBox = $('#status')

const STATUS_LABELS = {
  connecting: 'Connecting…',
  ready: 'Ready',
  listening: 'Listening…',
  transcribing: 'Transcribing…',
  thinking: 'Thinking…',
  working: 'Working…',
  speaking: 'Speaking…',
  offline: 'Offline',
}

const state = {
  socket: null,
  retries: 0,
  /** The turn being answered: { id, bubble, text, voice, activity, waitTimer }. */
  turn: null,
  stt: false,
  tts: false,
  listening: null,
  audio: null,
}

// -- status -------------------------------------------------------------------

function setStatus(kind) {
  statusBox.dataset.state = kind
  statusBox.querySelector('.label').textContent = STATUS_LABELS[kind] ?? kind
}

function idleStatus() {
  if (!isOpen()) setStatus('offline')
  else if (state.listening) setStatus('listening')
  else if (state.audio) setStatus('speaking')
  else if (state.turn) setStatus(state.turn.activity ? 'working' : 'thinking')
  else setStatus('ready')
}

// -- conversation -------------------------------------------------------------

function nearBottom() {
  return log.scrollHeight - log.scrollTop - log.clientHeight < 80
}

function append(node) {
  const stick = nearBottom()
  welcome?.remove()
  log.append(node)
  if (stick) log.scrollTop = log.scrollHeight
  return node
}

function element(tag, className, text) {
  const node = document.createElement(tag)
  if (className) node.className = className
  if (text !== undefined) node.textContent = text
  return node
}

function notice(text, kind = '') {
  append(element('div', `notice ${kind}`.trim(), text))
}

function addUser(text, voice) {
  append(element('div', voice ? 'msg user voice' : 'msg user', text))
}

/** Plain labels for the tools a turn uses. First match wins. */
const TOOL_LABELS = [
  [/^mcp__winwright__desktop_(snapshot|find|inspect|windows|read_text)$/, 'Looking at the screen'],
  [/^mcp__winwright__desktop_screenshot$/, 'Taking a screenshot'],
  [/^mcp__winwright__desktop_(fill|type)$/, 'Typing'],
  [/^mcp__winwright__desktop_wait_for$/, 'Waiting for the screen'],
  [/^mcp__winwright__desktop_/, 'Working on the desktop'],
  [/^mcp__winwright__app_launch$/, 'Opening an app'],
  [/^mcp__winwright__window_control$/, 'Arranging windows'],
  [/^mcp__winwright__filesystem_operation$/, 'Working with files'],
  [/^mcp__winwright__shell_execute$/, 'Running a command'],
  [/^mcp__winwright__/, 'Using the desktop'],
  [/^WebSearch$/, 'Searching the web'],
  [/^WebFetch$/, 'Reading a web page'],
  [/^mcp__jarvis_chrome__/, 'Using your browser'],
  [/^mcp__jarvis__probe_url$/, 'Checking a link'],
  [/^mcp__jarvis__/, 'Preparing a card'],
  [/^(Task|Agent)$/, 'Looking into it'],
  [/^(Read|Glob|Grep)$/, 'Reading files'],
]

/** Desktop changes that may stop at Winwright's Allow/Deny dialog. */
const MAY_ASK =
  /^mcp__winwright__(desktop_(click|fill|type|press|select|check|expand|focus|scroll)|app_launch|window_control|filesystem_operation|shell_execute)$/

function toolLabel(name) {
  for (const [pattern, label] of TOOL_LABELS) if (pattern.test(name)) return label
  const tool = name.startsWith('mcp__') ? name.split('__').slice(2).join(' ') : name
  return `Using ${tool.replace(/[_-]+/g, ' ')}`
}

function clearWait(turn) {
  clearTimeout(turn?.waitTimer)
  if (turn) turn.waitTimer = null
}

/** The agent's own bookkeeping, not steps worth showing. */
const HIDDEN_TOOLS = /^(ToolSearch|TodoWrite)$/

function showTool(name) {
  const turn = state.turn
  if (!turn || HIDDEN_TOOLS.test(name)) return
  clearWait(turn)
  const label = toolLabel(name)
  // The same step twice in a row (a snapshot, then another) is one line.
  if (turn.activity?.dataset.label !== label) {
    turn.activity = append(element('div', 'activity'))
    turn.activity.dataset.label = label
    turn.activity.append(element('span', '', label))
  }
  // A desktop change that goes quiet is most likely waiting on the dialog.
  if (MAY_ASK.test(name)) {
    const line = turn.activity
    turn.waitTimer = setTimeout(() => {
      if (!line.querySelector('.wait')) {
        line.append(element('span', 'wait', '· approve or deny in the Winwright dialog'))
      }
    }, 3500)
  }
  turn.bubble = null
  idleStatus()
}

function appendText(delta) {
  const turn = state.turn
  if (!turn) return
  clearWait(turn)
  if (!turn.bubble) {
    turn.bubble = append(element('div', 'msg assistant streaming'))
    turn.bubble.textContent = ''
  }
  const stick = nearBottom()
  turn.bubble.textContent += delta
  turn.text += delta
  if (stick) log.scrollTop = log.scrollHeight
}

function endTurn() {
  const turn = state.turn
  if (!turn) return
  clearWait(turn)
  log.querySelectorAll('.msg.streaming').forEach((bubble) => bubble.classList.remove('streaming'))
  state.turn = null
  idleStatus()
}

// -- cards (display / blade) --------------------------------------------------

/** Paths on this machine, as opposed to app-relative URLs that start with a slash. */
const DISK_PATH = /^(?:[A-Za-z]:[\\/]|\\\\|\/(?:Users|home|tmp|var|mnt|data)\/)/

/** A source routed through the bridge, the only origin the page may load from. */
function viaBridge(raw, route) {
  const src = String(raw ?? '').trim()
  if (!src) return ''
  const path = src.replace(/^file:\/\/\/?/, '')
  if (DISK_PATH.test(path)) return `/file?path=${encodeURIComponent(path)}`
  if (/^https?:\/\//i.test(src)) return `/${route}?url=${encodeURIComponent(src)}`
  if (/^data:(image|video|audio)\//i.test(src) || /^\/(img|media|file|page)\?/.test(src)) return src
  return ''
}

const EMBED_PATHS = {
  'www.youtube-nocookie.com': /^\/embed\/[\w-]+/,
  'www.youtube.com': /^\/embed\/[\w-]+/,
  'player.vimeo.com': /^\/video\/\d+/,
}

/** A YouTube or Vimeo player URL for `raw`, or null for anything else. */
function embedUrl(raw) {
  let url
  try {
    url = new URL(String(raw ?? ''), location.href)
  } catch {
    return null
  }
  const host = url.hostname.toLowerCase().replace(/^(?:www|m|music)\./, '')
  let id = ''
  if (host === 'youtube.com' && url.pathname === '/watch') id = url.searchParams.get('v') ?? ''
  else if (host === 'youtu.be') id = url.pathname.slice(1)
  if (/^[\w-]{6,20}$/.test(id)) url = new URL(`https://www.youtube-nocookie.com/embed/${id}`)
  const allowed = EMBED_PATHS[url.hostname.toLowerCase()]
  if (!allowed || !allowed.test(url.pathname)) return null
  url.protocol = 'https:'
  return url.toString()
}

const HUD_CLASSES = new Set([
  'hud-rows', 'hud-row', 'hud-idx', 'hud-main', 'hud-label', 'hud-sub',
  'hud-tag', 'hud-metric', 'hud-unit', 'hud-note', 'hud-img', 'hud-caption',
  'hud-grid', 'hud-bar', 'hud-dim', 'hud-hot',
  'hud-gallery', 'hud-thumb', 'hud-video', 'hud-embed', 'hud-figure',
])

/** The model's card markup, made safe: DOMPurify, then only the HUD classes, sources via the bridge. */
function sanitise(html) {
  if (!window.DOMPurify) return null
  const fragment = window.DOMPurify.sanitize(String(html ?? ''), {
    RETURN_DOM_FRAGMENT: true,
    ALLOWED_TAGS: [
      'div', 'span', 'p', 'ul', 'ol', 'li', 'img', 'b', 'strong', 'em', 'i',
      'br', 'small', 'table', 'thead', 'tbody', 'tr', 'td', 'th', 'code', 'pre',
      'video', 'source', 'iframe',
    ],
    ALLOWED_ATTR: [
      'class', 'src', 'alt', 'style', 'controls', 'poster', 'loop', 'muted',
      'playsinline', 'preload', 'width', 'height', 'type', 'title',
    ],
    ALLOWED_URI_REGEXP: /^(?:data:(?:image|video|audio)\/|file:\/\/|https?:\/\/|\/|[A-Za-z]:[\\/])/i,
  })
  fragment.querySelectorAll('[class]').forEach((node) => {
    const kept = node.getAttribute('class').split(/\s+/).filter((c) => HUD_CLASSES.has(c))
    if (kept.length) node.setAttribute('class', kept.join(' '))
    else node.removeAttribute('class')
  })
  // The page's CSP refuses style attributes; the one style a card may carry is
  // a progress bar's --v, set through the DOM instead.
  fragment.querySelectorAll('[style]').forEach((node) => {
    const v = /--v:\s*([\d.]+)/.exec(node.getAttribute('style') ?? '')
    node.removeAttribute('style')
    if (v) node.style.setProperty('--v', v[1])
  })
  fragment.querySelectorAll('img').forEach((img) => {
    const src = viaBridge(img.getAttribute('src'), 'img')
    if (src) img.setAttribute('src', src)
    else img.remove()
  })
  fragment.querySelectorAll('video, source').forEach((media) => {
    const src = viaBridge(media.getAttribute('src'), 'media')
    if (src) media.setAttribute('src', src)
    else media.removeAttribute('src')
    const poster = media.getAttribute('poster')
    if (poster) media.setAttribute('poster', viaBridge(poster, 'img'))
  })
  fragment.querySelectorAll('video').forEach((video) => {
    video.setAttribute('controls', '')
    video.setAttribute('preload', 'metadata')
    video.setAttribute('playsinline', '')
  })
  fragment.querySelectorAll('iframe').forEach((frame) => {
    const src = embedUrl(frame.getAttribute('src'))
    if (!src) return frame.remove()
    for (const attr of [...frame.attributes]) frame.removeAttribute(attr.name)
    frame.setAttribute('src', src)
    frame.setAttribute('allowfullscreen', '')
    frame.setAttribute('referrerpolicy', 'no-referrer')
  })
  fragment.querySelectorAll('img, video').forEach((node) => node.setAttribute('referrerpolicy', 'no-referrer'))
  return fragment
}

function media(tag, src, alt) {
  const node = document.createElement(tag)
  node.src = src
  if (tag === 'img') {
    node.alt = alt
    node.loading = 'lazy'
  } else {
    node.controls = true
    node.preload = 'metadata'
    node.playsInline = true
  }
  node.referrerPolicy = 'no-referrer'
  return node
}

function frame(src, className) {
  const node = document.createElement('iframe')
  node.src = src
  node.className = className
  node.referrerPolicy = 'no-referrer'
  return node
}

function renderCard(blade) {
  const card = element('article', blade.size === 'wide' || blade.size === 'tall' ? 'card wide' : 'card')
  const header = element('header')
  header.append(element('span', '', blade.title || 'Card'))
  const url = /^https?:\/\//i.test(blade.url ?? '') ? blade.url : null
  if (url) {
    const link = element('a', '', 'Open')
    link.href = url
    link.target = '_blank'
    link.rel = 'noopener noreferrer'
    header.append(link)
  }
  const body = element('div', 'body')
  const title = blade.title || ''
  switch (blade.kind) {
    case 'markup': {
      const safe = sanitise(blade.html)
      if (safe) body.append(safe)
      else body.textContent = String(blade.html ?? '').replace(/<[^>]*>/g, ' ')
      break
    }
    case 'image':
      if (viaBridge(blade.url, 'img')) body.append(media('img', viaBridge(blade.url, 'img'), title))
      break
    case 'gallery': {
      const gallery = element('div', 'hud-gallery')
      for (const image of blade.images ?? []) {
        const src = viaBridge(image?.url ?? image, 'img')
        if (!src) continue
        const thumb = element('div', 'hud-thumb')
        thumb.append(media('img', src, image?.caption ?? title))
        gallery.append(thumb)
      }
      body.append(gallery)
      break
    }
    case 'video': {
      const player = embedUrl(blade.url)
      if (player) {
        const wrap = element('div', 'embed')
        wrap.append(frame(player, ''))
        body.append(wrap)
      } else if (viaBridge(blade.url, 'media')) {
        body.append(media('video', viaBridge(blade.url, 'media'), title))
      }
      break
    }
    case 'embed': {
      const player = embedUrl(blade.url)
      if (player) {
        const wrap = element('div', 'embed')
        wrap.append(frame(player, ''))
        body.append(wrap)
      } else {
        body.textContent = 'This page cannot be shown here. Use Open.'
      }
      break
    }
    case 'article': {
      if (!url) break
      const mode = blade.mode === 'live' ? 'live' : 'reader'
      const reader = frame(`/page?mode=${mode}&url=${encodeURIComponent(url)}`, 'reader')
      // A rendered page is something to read, not something that runs.
      reader.setAttribute('sandbox', '')
      body.classList.remove('body')
      body.append(reader)
      break
    }
    default:
      body.textContent = 'Nothing to show.'
  }
  card.append(header, body)
  if (state.turn) state.turn.bubble = null
  append(card)
}

// -- socket -------------------------------------------------------------------

function isOpen() {
  return state.socket?.readyState === WebSocket.OPEN
}

function sendMessage(message) {
  if (isOpen()) state.socket.send(JSON.stringify(message))
}

function connect() {
  setStatus('connecting')
  const socket = new WebSocket(`ws://${location.host}/ws`)
  state.socket = socket
  socket.addEventListener('open', () => {
    state.retries = 0
    idleStatus()
    updateComposer()
  })
  socket.addEventListener('message', (event) => {
    let message
    try {
      message = JSON.parse(event.data)
    } catch {
      return
    }
    handle(message)
  })
  socket.addEventListener('close', () => {
    if (state.socket !== socket) return
    state.socket = null
    if (state.turn) {
      notice('The connection dropped; that request was lost.', 'error')
      endTurn()
    }
    setStatus('offline')
    updateComposer()
    if (state.retries === 3) notice('Winwright is not running. Start it with: winwright assistant')
    const delay = Math.min(10_000, 500 * 2 ** state.retries++)
    setTimeout(connect, delay)
  })
}

/** Whether a turn message belongs to the request on screen (not one interrupted before it). */
function current(message) {
  return state.turn && (message.ask == null || message.ask === state.turn.id)
}

function handle(message) {
  switch (message.type) {
    case 'ready':
      idleStatus()
      return
    case 'text':
      if (current(message)) appendText(String(message.delta ?? ''))
      return
    case 'tool':
      if (current(message)) showTool(String(message.name ?? ''))
      return
    case 'done': {
      if (!current(message)) return
      const turn = state.turn
      if (!turn.text && message.text) appendText(String(message.text))
      const spoken = turn.voice ? turn.text || String(message.text ?? '') : ''
      endTurn()
      if (spoken) speak(spoken)
      return
    }
    case 'error':
      notice(String(message.message ?? 'Something went wrong.'), 'error')
      endTurn()
      return
    case 'blade':
      if (message.blade) renderCard(message.blade)
      return
    case 'panel':
      if (message.panel) renderCard({ ...message.panel, kind: 'markup' })
      return
    case 'ptt':
      toggleListening()
      return
    default:
      // A question from the bridge this page cannot answer (the camera, say):
      // answer at once rather than leave the agent waiting out a timeout.
      if (typeof message.id === 'string') {
        sendMessage({ type: 'reply', id: message.id, error: 'This interface cannot do that.' })
      }
  }
}

// -- asking -------------------------------------------------------------------

function newId() {
  return `w${Date.now().toString(36)}${Math.random().toString(36).slice(2, 7)}`
}

function ask(text, voice = false) {
  const trimmed = text.trim()
  if (!trimmed) return
  if (!isOpen()) {
    notice('Not connected to Winwright yet.', 'error')
    return
  }
  stopSpeaking()
  // A new request while one is running replaces it.
  if (state.turn) {
    sendMessage({ type: 'interrupt' })
    endTurn()
  }
  addUser(trimmed, voice)
  const id = newId()
  state.turn = { id, bubble: null, text: '', voice, activity: null, waitTimer: null }
  idleStatus()
  sendMessage({ type: 'ask', id, text: trimmed })
}

function interrupt() {
  stopSpeaking()
  if (state.listening) stopListening(false)
  if (state.turn) {
    sendMessage({ type: 'interrupt' })
    notice('Stopped.')
    endTurn()
  }
}

// -- composer -----------------------------------------------------------------

function updateComposer() {
  sendButton.disabled = !input.value.trim() || !isOpen()
  input.style.height = 'auto'
  input.style.height = `${Math.min(input.scrollHeight, 160)}px`
}

input.addEventListener('input', updateComposer)
// The box's height follows its width: measure again whenever the window changes.
window.addEventListener('resize', updateComposer)
input.addEventListener('keydown', (event) => {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault()
    form.requestSubmit()
  }
})
form.addEventListener('submit', (event) => {
  event.preventDefault()
  if (sendButton.disabled) return
  ask(input.value)
  input.value = ''
  updateComposer()
})
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') interrupt()
})
log.addEventListener('click', (event) => {
  const example = event.target.closest('.example')
  if (example) ask(example.textContent)
})
micButton.addEventListener('click', () => toggleListening())

// -- voice in -----------------------------------------------------------------

const SPEECH_LEVEL = 0.018
const END_OF_SPEECH_MS = 1200
const NO_SPEECH_MS = 7000
const MAX_RECORDING_MS = 30_000

const PAGE_TITLE = document.title

function toggleListening() {
  if (state.listening) stopListening(true)
  else startListening()
}

/**
 * Browsers keep a page's audio off until the person has clicked or typed in it
 * once. A page the launcher opened, then driven by the global hotkey, may never
 * have been clicked: say so instead of listening to silence.
 */
function audioAllowed() {
  return navigator.userActivation?.hasBeenActive !== false
}

async function startListening() {
  if (!state.stt) {
    notice('Voice needs an ElevenLabs key (ELEVENLABS_API_KEY). You can still type.')
    return
  }
  if (!audioAllowed()) {
    notice(
      'Click anywhere on this page once: the browser keeps the microphone off until you do. ' +
        'Then press Ctrl+Space again.',
      'error',
    )
    document.title = `Click me · ${PAGE_TITLE}`
    return
  }
  stopSpeaking()
  let stream
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true },
    })
  } catch {
    notice('The microphone is blocked. Allow it in the address bar and try again.', 'error')
    return
  }
  const type = ['audio/webm;codecs=opus', 'audio/webm', 'audio/ogg;codecs=opus'].find((t) =>
    MediaRecorder.isTypeSupported(t),
  )
  const recorder = new MediaRecorder(stream, type ? { mimeType: type } : undefined)
  const chunks = []
  recorder.addEventListener('dataavailable', (event) => {
    if (event.data.size) chunks.push(event.data)
  })

  // A pause after speech ends the recording; so does silence from the start.
  // Without a running audio context nothing can hear the pause, so the second
  // press (or the time limit) ends it instead.
  const context = new AudioContext()
  if (context.state !== 'running') await context.resume().catch(() => {})
  const hearsPauses = context.state === 'running'
  if (!hearsPauses) notice('Listening. Press Ctrl+Space again when you are done.')
  const analyser = context.createAnalyser()
  analyser.fftSize = 1024
  context.createMediaStreamSource(stream).connect(analyser)
  const samples = new Float32Array(analyser.fftSize)
  const started = performance.now()
  let heard = false
  let lastLoud = started
  const timer = setInterval(() => {
    analyser.getFloatTimeDomainData(samples)
    let sum = 0
    for (const s of samples) sum += s * s
    const level = Math.sqrt(sum / samples.length)
    micButton.style.setProperty('--level', Math.min(1, level * 12).toFixed(2))
    const now = performance.now()
    if (level > SPEECH_LEVEL) {
      heard = true
      lastLoud = now
    }
    if (hearsPauses && heard && now - lastLoud > END_OF_SPEECH_MS) stopListening(true)
    else if (hearsPauses && !heard && now - started > NO_SPEECH_MS) stopListening(false)
    else if (now - started > MAX_RECORDING_MS) stopListening(true)
  }, 50)

  state.listening = { recorder, stream, context, timer, chunks, transcribe: true }
  recorder.addEventListener('stop', () => finishListening(recorder.mimeType || type || 'audio/webm'))
  recorder.start(250)
  micButton.setAttribute('aria-pressed', 'true')
  // Visible in the tab strip too: the hotkey works while the page is behind.
  document.title = `● Listening · ${PAGE_TITLE}`
  idleStatus()
}

function stopListening(transcribe) {
  const listening = state.listening
  if (!listening || listening.stopping) return
  listening.stopping = true
  listening.transcribe = transcribe
  clearInterval(listening.timer)
  if (listening.recorder.state !== 'inactive') listening.recorder.stop()
}

async function finishListening(type) {
  const listening = state.listening
  state.listening = null
  listening.stream.getTracks().forEach((track) => track.stop())
  listening.context.close().catch(() => {})
  document.title = PAGE_TITLE
  micButton.setAttribute('aria-pressed', 'false')
  micButton.style.removeProperty('--level')
  if (!listening.transcribe) {
    idleStatus()
    return
  }
  setStatus('transcribing')
  try {
    const response = await fetch('/stt', {
      method: 'POST',
      headers: { 'content-type': type },
      body: new Blob(listening.chunks, { type }),
    })
    if (!response.ok) {
      const reason = response.status === 401 ? 'the ElevenLabs key was refused' : `error ${response.status}`
      notice(`Could not transcribe (${reason}).`, 'error')
      return
    }
    const { text } = await response.json()
    if (text && text.trim()) ask(text, true)
    else notice("I didn't catch that.")
  } catch {
    notice('Could not reach the transcriber.', 'error')
  } finally {
    idleStatus()
  }
}

// -- voice out ----------------------------------------------------------------

async function speak(text) {
  if (!state.tts) return
  try {
    const response = await fetch('/tts', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ text: text.slice(0, 4000) }),
    })
    if (!response.ok) return
    const url = URL.createObjectURL(await response.blob())
    const audio = new Audio(url)
    const done = () => {
      URL.revokeObjectURL(url)
      if (state.audio === audio) state.audio = null
      idleStatus()
    }
    audio.addEventListener('ended', done)
    audio.addEventListener('error', done)
    stopSpeaking()
    state.audio = audio
    idleStatus()
    await audio.play()
  } catch (err) {
    // Not being heard is not worth an error line, the reply is on screen; but a
    // browser that blocks sound until the page is clicked is worth one hint.
    if (err?.name === 'NotAllowedError' && !state.toldAboutSound) {
      state.toldAboutSound = true
      notice('Click anywhere on this page once so replies can be spoken.')
    }
  }
}

function stopSpeaking() {
  if (!state.audio) return
  const audio = state.audio
  state.audio = null
  audio.pause()
  audio.dispatchEvent(new Event('ended'))
}

// -- start --------------------------------------------------------------------

async function probe() {
  try {
    const health = await (await fetch('/health')).json()
    state.stt = Boolean(health.stt)
    state.tts = Boolean(health.tts)
  } catch {
    state.stt = state.tts = false
  }
  micButton.disabled = !state.stt
  micButton.title = state.stt ? 'Talk (Ctrl+Space)' : 'Voice needs an ElevenLabs key'
}

probe()
connect()
updateComposer()
input.focus()
