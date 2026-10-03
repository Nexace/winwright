/**
 * Copies each task report (reports.mjs) to Notion as a page under one parent
 * page, through Notion's REST API with an internal integration's key. The bridge
 * does it, not the model: no tokens spent, no write permission for the agent,
 * and the report file stays the permanent record if Notion is unreachable.
 *
 * Off unless both are set (Windows user environment variables):
 *   JARVIS_NOTION_TOKEN   the integration's secret (never logged)
 *   JARVIS_NOTION_PARENT  the parent page's link or id, shared with the integration
 */

const API = 'https://api.notion.com/v1/pages'
const VERSION = '2026-03-11'

/** The page id in a Notion link or id (dashes optional), as a UUID, or null. */
export function notionPageId(text) {
  const bare = String(text ?? '')
    .trim()
    .split(/[?#]/)[0]
    .replace(/-/g, '')
  const hex = bare.match(/([0-9a-f]{32})$/i)?.[1]?.toLowerCase()
  if (!hex) return null
  return [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)].join('-')
}

/** Mirror settings from the environment, or null when off. */
export function notionConfig(env = process.env) {
  const token = env.JARVIS_NOTION_TOKEN?.trim()
  const parent = notionPageId(env.JARVIS_NOTION_PARENT)
  return token && parent ? { token, parent } : null
}

/** A report file as a Notion page: its heading becomes the title, its front matter one line. */
export function notionPage(report) {
  const front = report.match(/^---\n([\s\S]*?)\n---\n/)?.[1] ?? ''
  const field = (name) => front.match(new RegExp(`^${name}: (.*)$`, 'm'))?.[1] ?? ''
  let body = report.replace(/^---\n[\s\S]*?\n---\n/, '')
  const title = body.match(/^# (.*)$/m)?.[1]?.trim() || 'Task'
  body = body.replace(/^# .*\n?/m, '').trim()
  const when = field('date') ? new Date(field('date')).toLocaleString() : ''
  const outside = field('outsideContent') === 'true' ? ' · read outside content' : ''
  const meta = [when, field('outcome')].filter(Boolean).join(' · ')
  return { title: title.slice(0, 200), markdown: `_${meta}${outside}_\n\n${body}` }
}

/** Creates the page; resolves to its URL. Errors say what the person should fix. */
export async function mirrorToNotion(report, { token, parent }, fetchImpl = fetch) {
  const page = notionPage(report)
  const res = await fetchImpl(API, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${token}`,
      'Notion-Version': VERSION,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      parent: { type: 'page_id', page_id: parent },
      properties: { title: { title: [{ type: 'text', text: { content: page.title } }] } },
      markdown: page.markdown,
    }),
    signal: AbortSignal.timeout(15_000),
  })
  if (res.ok) return (await res.json()).url ?? ''
  const hint = {
    401: 'the Notion key (JARVIS_NOTION_TOKEN) is wrong or was revoked',
    403: "the integration may not insert content: enable it in the integration's capabilities",
    404: 'the parent page was not found: share it with the integration (••• > Connections)',
  }[res.status]
  throw new Error(`Notion answered ${res.status}${hint ? `: ${hint}` : ''}`)
}
