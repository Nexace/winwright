#!/usr/bin/env node
// JARVIS preflight — a friendly, advisory check you run with `npm run setup`.
//
// It changes nothing and installs nothing. It looks at your machine, tells you
// what is ready and what is missing, and prints the two commands that start
// JARVIS. Every check degrades to a single friendly line if something is not
// there, and the script always exits 0 — it is advice, not a gate.

import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

const tick = '  ok  ';
const warn = ' note ';
const info = '  ·   ';

function line(tag, msg) {
  console.log(`[${tag}] ${msg}`);
}

console.log('');
console.log('JARVIS preflight — checking your machine (nothing is changed)');
console.log('------------------------------------------------------------');

// --- Node version --------------------------------------------------------
try {
  const major = Number(process.versions.node.split('.')[0]);
  if (Number.isFinite(major) && major >= 20) {
    line(tick, `Node.js ${process.versions.node} (20+ required).`);
  } else {
    line(warn, `Node.js ${process.versions.node} is below 20. Please upgrade — the bridge needs Node 20 or newer.`);
  }
} catch {
  line(warn, 'Could not read the Node.js version. JARVIS needs Node 20 or newer.');
}

// --- Claude CLI on PATH ---------------------------------------------------
let claudeFound = false;
try {
  const res = spawnSync('claude', ['--version'], { encoding: 'utf8', timeout: 10000 });
  if (res.status === 0 && res.stdout) {
    claudeFound = true;
    line(tick, `Claude CLI found: ${res.stdout.trim()}`);
  }
} catch {
  // ignore — handled below
}
if (!claudeFound) {
  line(warn, 'Claude CLI not found on your PATH.');
  line(info, 'Install it: npm install -g @anthropic-ai/claude-code');
  line(info, '  (or the platform installer at https://docs.claude.com/en/docs/claude-code)');
  line(info, 'Then run `claude` once and complete login. The bridge uses that login — no API key needed.');
}

// --- ~/.claude.json and MCP servers --------------------------------------
const claudeJsonPath = join(homedir(), '.claude.json');
let mcpCount = 0;
try {
  const raw = readFileSync(claudeJsonPath, 'utf8');
  let parsed = {};
  try {
    parsed = JSON.parse(raw);
  } catch {
    parsed = {};
  }
  const servers = parsed && typeof parsed.mcpServers === 'object' && parsed.mcpServers ? parsed.mcpServers : {};
  mcpCount = Object.keys(servers).length;
  if (mcpCount > 0) {
    line(tick, `~/.claude.json found with ${mcpCount} MCP server${mcpCount === 1 ? '' : 's'} configured.`);
  } else {
    line(info, '~/.claude.json found, but no MCP servers are configured yet. JARVIS still answers and drives its own interface.');
  }
} catch {
  line(info, '~/.claude.json not found yet. It appears once you run `claude` and log in. JARVIS works without any MCP servers.');
}

// --- ElevenLabs key (env or the elevenlabs MCP entry) --------------------
function findElevenLabsKey() {
  if (process.env.ELEVENLABS_API_KEY && process.env.ELEVENLABS_API_KEY.trim()) {
    return 'environment (ELEVENLABS_API_KEY)';
  }
  try {
    const raw = readFileSync(claudeJsonPath, 'utf8');
    const parsed = JSON.parse(raw);
    const servers = parsed && parsed.mcpServers ? parsed.mcpServers : {};
    const el = servers.elevenlabs;
    const env = el && el.env ? el.env : {};
    if (env.ELEVENLABS_API_KEY && String(env.ELEVENLABS_API_KEY).trim()) {
      return 'the elevenlabs MCP server in ~/.claude.json';
    }
  } catch {
    // ignore — no key discoverable
  }
  return null;
}

const elSource = findElevenLabsKey();
if (elSource) {
  line(tick, `Premium voice available — ElevenLabs key found via ${elSource}.`);
} else {
  line(info, 'No ElevenLabs key found: type to the assistant (voice needs the key).');
  line(info, '  Optional: set ELEVENLABS_API_KEY for voice in and out. The free tier is enough.');
}

// --- How to run ----------------------------------------------------------
console.log('');
console.log('To run the assistant, from the Winwright checkout:');
console.log('  winwright assistant     # then open http://localhost:8787/');
console.log('');
console.log('Type, or press Ctrl+Space and talk.');
console.log('');

process.exit(0);
