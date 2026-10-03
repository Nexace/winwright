/**
 * The taint rule (Winwright's; see .gsd/INTEGRATION.md in the Winwright repo).
 * Once a conversation has read outside content — a web page, a Notion page, a
 * sub-agent's findings, another MCP server's results — instructions hidden in
 * it could steer the assistant, so every desktop change Winwright makes after
 * that needs the person's yes in its native dialog. The signal is one file per
 * conversation, created by the bridge before the first such tool runs, and
 * handed to `winwright mcp --taint-file`. It lives under
 * %LOCALAPPDATA%\winwright, a folder Winwright's own file tools refuse to touch.
 *
 * Trusted (no taint): Winwright itself (its desktop reads are the user's own
 * screen), the cards on Winwright's page, and local file reads.
 */

import { randomUUID } from 'node:crypto'
import { mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'

const TRUSTED_SERVERS = new Set(['winwright', 'jarvis'])
const UNTRUSTED_BUILTINS = new Set([
  'WebFetch', 'WebSearch', 'Task', 'Agent',
  'ReadMcpResource', 'ReadMcpResourceTool',
])

/** Whether running this tool puts outside content into the conversation. */
export function bringsOutsideContent(toolName) {
  if (UNTRUSTED_BUILTINS.has(toolName)) return true
  // MCP tools arrive as `mcp__<server>__<tool>`.
  const server = toolName.startsWith('mcp__') ? toolName.split('__')[1] : null
  return server !== null && !TRUSTED_SERVERS.has(server)
}

/** This conversation's marker, or null when Winwright is not in use. */
export function taintMarker(env = process.env) {
  const root = env.LOCALAPPDATA
  if (!env.JARVIS_WINWRIGHT_EXE || !root) return null
  const file = join(root, 'winwright', 'taint', randomUUID())
  let marked = false
  return {
    file,
    /** Throws when the marker cannot be written; the caller then refuses the tool. */
    mark() {
      if (marked) return
      mkdirSync(dirname(file), { recursive: true })
      writeFileSync(file, '')
      marked = true
    },
    clear() {
      rmSync(file, { force: true })
    },
  }
}
