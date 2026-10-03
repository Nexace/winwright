// node --test bridge/winwright-face.test.mjs
import assert from 'node:assert/strict'
import { test } from 'node:test'

import { WINWRIGHT_PROMPT, contentSecurityPolicy, serveFace } from './winwright-face.mjs'

function fakeResponse() {
  return {
    status: 0,
    headers: {},
    body: undefined,
    writeHead(status, headers = {}) {
      this.status = status
      this.headers = headers
    },
    end(body) {
      this.body = body
    },
  }
}

const get = (url, host = 'localhost:8787', method = 'GET') => ({ method, url, headers: { host } })

test('the page and its scripts are served, with a strict policy on the page', async () => {
  const page = fakeResponse()
  assert.equal(await serveFace(get('/'), page), true)
  assert.equal(page.status, 200)
  assert.match(page.headers['content-type'], /text\/html/)
  assert.match(String(page.body), /<title>Winwright<\/title>/)
  assert.equal(page.headers['content-security-policy'], contentSecurityPolicy('localhost:8787'))
  for (const [path, type] of [
    ['/ui/app.js', /javascript/],
    ['/ui/style.css', /text\/css/],
    ['/ui/purify.min.js', /javascript/],
  ]) {
    const res = fakeResponse()
    assert.equal(await serveFace(get(path), res), true, path)
    assert.equal(res.status, 200, path)
    assert.match(res.headers['content-type'], type, path)
    assert.equal(res.headers['x-content-type-options'], 'nosniff')
  }
})

test('nothing else on disk is reachable, and only local hosts are served', async () => {
  for (const path of ['/ui/../bridge/server.mjs', '/package.json', '/ui/', '/health']) {
    assert.equal(await serveFace(get(path), fakeResponse()), false, path)
  }
  assert.equal(await serveFace(get('/', 'localhost:8787', 'POST'), fakeResponse()), false)
  const rebound = fakeResponse()
  assert.equal(await serveFace(get('/', 'evil.example:8787'), rebound), true)
  assert.equal(rebound.status, 403)
})

test('the policy loads only from the bridge and frames only the video hosts', () => {
  const csp = contentSecurityPolicy('127.0.0.1:8787')
  assert.match(csp, /default-src 'none'/)
  assert.match(csp, /script-src 'self'(;|$)/)
  assert.match(csp, /connect-src 'self' ws:\/\/127\.0\.0\.1:8787/)
  assert.doesNotMatch(csp, /unsafe-inline|unsafe-eval/)
})

test('the assistant is Winwright and keeps the safety rules', () => {
  assert.match(WINWRIGHT_PROMPT, /^You are Winwright/)
  assert.match(WINWRIGHT_PROMPT, /Never try to get around a block/)
  assert.match(WINWRIGHT_PROMPT, /You can never approve an Allow\/Deny box yourself/)
  assert.doesNotMatch(WINWRIGHT_PROMPT, /JARVIS|ui_theme|blades — the ONLY surface/)
})
