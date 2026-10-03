// node --test bridge/taint.test.mjs
import assert from 'node:assert/strict'
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'

import { bringsOutsideContent, taintMarker } from './taint.mjs'

test('web, Notion, sub-agents and unknown servers bring outside content', () => {
  for (const name of [
    'WebFetch',
    'WebSearch',
    'Task',
    'Agent',
    'ReadMcpResourceTool',
    'mcp__jarvis_chrome__chrome_navigate',
    'mcp__notion__notion-search',
    'mcp__playwright__browser_navigate',
    'mcp__some_new_server__anything',
  ]) {
    assert.equal(bringsOutsideContent(name), true, name)
  }
  for (const name of [
    'mcp__winwright__desktop_snapshot',
    'mcp__jarvis__display',
    'mcp__jarvis_ui__ui_theme',
    'mcp__jarvis_eyes__look',
    'Read',
    'Grep',
    'TodoWrite',
  ]) {
    assert.equal(bringsOutsideContent(name), false, name)
  }
})

test('the marker is per conversation, written once, and removed at the end', () => {
  const root = mkdtempSync(join(tmpdir(), 'jarvis-taint-'))
  try {
    assert.equal(taintMarker({ LOCALAPPDATA: root }), null, 'no Winwright, no marker')
    const env = { LOCALAPPDATA: root, JARVIS_WINWRIGHT_EXE: 'winwright.exe' }
    const a = taintMarker(env)
    const b = taintMarker(env)
    assert.notEqual(a.file, b.file)
    assert.ok(a.file.startsWith(join(root, 'winwright', 'taint')))
    assert.equal(existsSync(a.file), false, 'nothing until outside content arrives')
    a.mark()
    a.mark()
    assert.equal(existsSync(a.file), true)
    assert.equal(existsSync(b.file), false, 'other conversations stay clean')
    a.clear()
    assert.equal(existsSync(a.file), false)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('a marker that cannot be written throws, so the tool is refused', () => {
  const root = mkdtempSync(join(tmpdir(), 'jarvis-taint-'))
  try {
    // `winwright` is a file here, so the marker's folder cannot be created.
    writeFileSync(join(root, 'winwright'), '')
    const marker = taintMarker({ LOCALAPPDATA: root, JARVIS_WINWRIGHT_EXE: 'w.exe' })
    assert.throws(() => marker.mark())
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})
