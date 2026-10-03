// node --test bridge/notion.test.mjs
import assert from 'node:assert/strict'
import { test } from 'node:test'

import { mirrorToNotion, notionConfig, notionPage, notionPageId } from './notion.mjs'

const ID = '1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d'
const UUID = '1a2b3c4d-5e6f-7a8b-9c0d-1e2f3a4b5c6d'

test('page ids come from links, bare ids and UUIDs', () => {
  for (const text of [
    `https://www.notion.so/myspace/Winwright-reports-${ID}`,
    `https://www.notion.so/Cafe-${ID}?pvs=4`,
    `https://notion.so/${ID}#heading`,
    ID,
    UUID,
    ` ${UUID.toUpperCase()} `,
  ]) {
    assert.equal(notionPageId(text), UUID, text)
  }
  for (const text of [undefined, '', 'https://www.notion.so/Winwright', 'abc123']) {
    assert.equal(notionPageId(text), null, String(text))
  }
})

test('the mirror is off unless both settings are present', () => {
  assert.equal(notionConfig({}), null)
  assert.equal(notionConfig({ JARVIS_NOTION_TOKEN: 'ntn_x' }), null)
  assert.equal(notionConfig({ JARVIS_NOTION_TOKEN: 'ntn_x', JARVIS_NOTION_PARENT: 'nope' }), null)
  assert.deepEqual(notionConfig({ JARVIS_NOTION_TOKEN: ' ntn_x ', JARVIS_NOTION_PARENT: ID }), {
    token: 'ntn_x',
    parent: UUID,
  })
})

const REPORT = [
  '---',
  'date: 2026-10-03T21:28:19.807Z',
  'outcome: done',
  'outsideContent: true',
  '---',
  '# Weather in Pune',
  '',
  '**Asked:** Weather in Pune',
  '',
  '**Tools:** WebSearch',
  '',
  '**Answer:** Sunny.',
  '',
].join('\n')

test('a report becomes a titled page with its front matter on one line', () => {
  const page = notionPage(REPORT)
  assert.equal(page.title, 'Weather in Pune')
  assert.doesNotMatch(page.markdown, /^---/m)
  assert.doesNotMatch(page.markdown, /^# /m)
  assert.match(page.markdown, /^_.*done · read outside content_\n/)
  assert.match(page.markdown, /\*\*Answer:\*\* Sunny\./)
})

test('the page is created under the parent with the key and API version', async () => {
  let seen
  const fake = async (url, init) => {
    seen = { url, init, body: JSON.parse(init.body) }
    return { ok: true, status: 200, json: async () => ({ url: 'https://www.notion.so/page' }) }
  }
  const url = await mirrorToNotion(REPORT, { token: 'ntn_secret', parent: UUID }, fake)
  assert.equal(url, 'https://www.notion.so/page')
  assert.equal(seen.url, 'https://api.notion.com/v1/pages')
  assert.equal(seen.init.method, 'POST')
  assert.equal(seen.init.headers.Authorization, 'Bearer ntn_secret')
  assert.equal(seen.init.headers['Notion-Version'], '2026-03-11')
  assert.deepEqual(seen.body.parent, { type: 'page_id', page_id: UUID })
  assert.equal(seen.body.properties.title.title[0].text.content, 'Weather in Pune')
  assert.match(seen.body.markdown, /\*\*Tools:\*\* WebSearch/)
})

test('failures say what to fix and never repeat the key', async () => {
  for (const [status, words] of [
    [401, /key .* wrong/],
    [404, /share it with the integration/],
    [500, /Notion answered 500$/],
  ]) {
    const fake = async () => ({ ok: false, status, json: async () => ({}) })
    await assert.rejects(
      mirrorToNotion(REPORT, { token: 'ntn_secret', parent: UUID }, fake),
      (err) => words.test(err.message) && !err.message.includes('ntn_secret'),
    )
  }
})
