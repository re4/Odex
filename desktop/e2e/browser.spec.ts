import { test, expect, type Page } from '@playwright/test'
import fs from 'node:fs'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import path from 'node:path'
import { desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

/**
 * In-app browser: tabs, navigation, the native view following the panel and
 * hiding under overlays, comment mode, agent browser use over CDP, and the
 * Browser / Computer use settings pages.
 */

let server: http.Server
let base: string
let mock: Mock
let L: Launched
const shotsDir = process.env.ODEX_SHOTS_DIR || path.join(desktopDir, 'test-results', process.env.ODEX_OUT || 'out', 'browser-shots')

function page(title: string, body: string): string {
  return `<!doctype html><html><head><meta charset="utf-8"><title>${title}</title>
<style>body{font:15px system-ui,sans-serif;margin:24px;color:#222;background:#fff} h1{font-size:22px;margin:0 0 12px} .box{padding:12px;border:1px solid #ccc;border-radius:8px;max-width:420px}</style>
</head><body>${body}</body></html>`
}

test.beforeAll(async () => {
  fs.mkdirSync(shotsDir, { recursive: true })
  server = http.createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' })
    if (req.url?.startsWith('/agent')) {
      res.end(page('Agent Fixture Page', '<h1>Agent fixture</h1><p class="box">The secret phrase is ODEX-E2E-MARKER-42.</p><button id="go">Continue</button>'))
    } else {
      res.end(page('Odex Fixture Page', '<h1 id="hello">Hello from the fixture</h1><div class="box"><p>A tiny page served by the test.</p><a href="/agent">Agent page</a></div>'))
    }
  })
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r))
  base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`
  mock = await startMock([
    {
      when: { last_role: 'user', last_user_contains: 'read the fixture' },
      reply: { kind: 'tool_calls', text: 'Opening the page.', calls: [{ name: 'browser_navigate', arguments: { url: `${base}/agent` } }] },
    },
    {
      when: { last_role: 'tool', last_user_contains: 'read the fixture', tool_results_since_user: 1 },
      reply: { kind: 'tool_calls', calls: [{ name: 'browser_snapshot', arguments: {} }] },
    },
    { when: { last_role: 'tool', last_user_contains: 'read the fixture' }, reply: { kind: 'text', text: 'The page says the secret phrase.' } },
    { when: { last_role: 'user', last_user_contains: 'take a screenshot' }, reply: { kind: 'tool_calls', calls: [{ name: 'browser_screenshot', arguments: {} }] } },
    { when: { last_role: 'tool', last_user_contains: 'take a screenshot' }, reply: { kind: 'text', text: 'Captured the page.' } },
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Read the fixture page"}' } },
    { when: {}, reply: { kind: 'text', text: 'OK' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
  server?.close()
})

const state = (p: Page) => p.evaluate(() => (window as any).odex.browser.state())

/** Number of fixture pages attached to the main window as native views. */
function attachedPages(): Promise<number> {
  return L.app.evaluate(({ BrowserWindow }, host) => {
    const w = BrowserWindow.getAllWindows()[0]
    return w.contentView.children.filter((v: any) => v.webContents && !v.webContents.isDestroyed() && v.webContents.getURL().startsWith(host)).length
  }, base)
}

async function shot(name: string): Promise<void> {
  await L.page.waitForTimeout(250) // let theme/hover transitions finish
  await L.page.screenshot({ path: path.join(shotsDir, `${name}.png`) })
}

async function scrollSettings(to: 'top' | 'bottom'): Promise<void> {
  await L.page.locator('main').last().evaluate((el, end) => el.scrollTo(0, end === 'top' ? 0 : el.scrollHeight), to)
}

/** The native page view is not part of page.screenshot(): capture it separately. */
async function pageShot(name: string): Promise<void> {
  const b64 = await L.app.evaluate(async ({ webContents }, host) => {
    const wc = webContents.getAllWebContents().find((w) => w.getURL().startsWith(host))
    return wc ? (await wc.capturePage()).toPNG().toString('base64') : ''
  }, base)
  if (b64) fs.writeFileSync(path.join(shotsDir, `${name}.png`), Buffer.from(b64, 'base64'))
}

test('opens a tab, navigates and shows the title in the tab strip', async () => {
  const { page } = L
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeVisible()
  await page.keyboard.press('Control+T')
  const strip = page.getByRole('tablist', { name: 'Browser tabs' })
  await expect(strip).toBeVisible()
  await expect(strip.getByRole('tab', { name: /New tab/ })).toBeVisible()
  await expect(page.getByText('Local servers')).toBeVisible()
  await shot('ntp-light')

  const address = page.getByRole('textbox', { name: 'Address' })
  await address.fill(`${base}/`)
  await address.press('Enter')
  await expect(strip.getByRole('tab', { name: /Odex Fixture Page/ })).toBeVisible()
  await expect(address).toHaveValue(`${base}/`)

  // the native view is attached exactly over the placeholder
  await expect.poll(attachedPages).toBe(1)
  await expect.poll(async () => (await state(page)).visible).toBe(true)
  const box = (await page.locator('[data-browser-viewport]').boundingBox())!
  const st = await state(page)
  const zoom = await L.app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows()[0].webContents.getZoomFactor())
  expect(Math.abs(st.bounds.x - box.x * zoom)).toBeLessThanOrEqual(2)
  expect(Math.abs(st.bounds.y - box.y * zoom)).toBeLessThanOrEqual(2)
  expect(Math.abs(st.bounds.width - box.width * zoom)).toBeLessThanOrEqual(2)
  expect(Math.abs(st.bounds.height - box.height * zoom)).toBeLessThanOrEqual(2)
  await shot('page-light')
  await pageShot('page-content')
})

test('the native view hides while the command palette or a menu is open', async () => {
  const { page } = L
  await page.keyboard.press('Control+K')
  const palette = page.getByRole('dialog', { name: 'Command palette' })
  await expect(palette).toBeVisible()
  await expect.poll(attachedPages).toBe(0)
  expect((await state(page)).visible).toBe(false)
  await page.keyboard.press('Escape')
  await expect(palette).toBeHidden()
  await expect.poll(attachedPages).toBe(1)

  // app shortcuts still work while the page has keyboard focus
  await L.app.evaluate(({ webContents }, host) => {
    const wc = webContents.getAllWebContents().find((w) => w.getURL().startsWith(host))!
    wc.focus()
    wc.sendInputEvent({ type: 'keyDown', keyCode: 'K', modifiers: ['control'] })
    wc.sendInputEvent({ type: 'keyUp', keyCode: 'K', modifiers: ['control'] })
  }, base)
  await expect(palette).toBeVisible()
  await expect.poll(attachedPages).toBe(0)
  await page.keyboard.press('Escape')
  await expect(palette).toBeHidden()
  await expect.poll(attachedPages).toBe(1)

  // menus anchored in the toolbar overlap the page too
  await page.getByRole('button', { name: 'More browser actions' }).click()
  await expect(page.getByRole('menuitem', { name: /Open in system browser/ })).toBeVisible()
  await expect.poll(attachedPages).toBe(0)
  await page.keyboard.press('Escape')
  await expect.poll(attachedPages).toBe(1)

  // history popover with search
  await page.getByRole('button', { name: 'History', exact: true }).click()
  const hist = page.getByRole('dialog', { name: 'Browsing history' })
  await expect(hist.getByText('Odex Fixture Page')).toBeVisible()
  await expect.poll(attachedPages).toBe(0)
  await shot('history-light')
  await hist.getByRole('textbox', { name: 'Search history' }).fill('no-such-page')
  await expect(hist.getByText('No matches.')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(hist).toBeHidden()
  await expect.poll(attachedPages).toBe(1)
})

test('switching the side panel tab hides the page', async () => {
  const { page } = L
  await page.getByRole('tab', { name: 'Files', exact: true }).click()
  await expect.poll(attachedPages).toBe(0)
  await page.getByRole('tab', { name: 'Browser', exact: true }).click()
  await expect.poll(attachedPages).toBe(1)
})

test('comment mode attaches a page comment to the composer', async () => {
  const { page, app } = L
  await page.getByRole('button', { name: 'Comment on page' }).click()
  await expect(page.getByText(/Click an element or drag/)).toBeVisible()
  // click the heading inside the page (real input events into the tab)
  await app.evaluate(async ({ webContents }, host) => {
    const wc = webContents.getAllWebContents().find((w) => w.getURL().startsWith(host))!
    const r = JSON.parse(await wc.executeJavaScript('JSON.stringify(document.querySelector("#hello").getBoundingClientRect())'))
    const x = Math.round(r.x + 20)
    const y = Math.round(r.y + r.height / 2)
    wc.sendInputEvent({ type: 'mouseMove', x, y })
    wc.sendInputEvent({ type: 'mouseDown', x, y, button: 'left', clickCount: 1 })
    wc.sendInputEvent({ type: 'mouseUp', x, y, button: 'left', clickCount: 1 })
  }, base)
  const dialog = page.getByRole('dialog', { name: /Comment on #hello/ })
  await expect(dialog).toBeVisible()
  await expect.poll(attachedPages).toBe(0)
  await dialog.getByRole('textbox').fill('Make this heading bigger')
  await dialog.getByRole('textbox').press('Enter')
  await expect(page.getByText('Comment: Make this heading bigger')).toBeVisible()
  await expect.poll(attachedPages).toBe(1)
  await shot('comment-light')
  // drop the attachment so the agent test sends plain text
  await page.getByRole('button', { name: 'Remove Comment: Make this heading bigger' }).click()
  await expect(page.getByText('Comment: Make this heading bigger')).toBeHidden()
})

test('the agent reads the page through the browser tool', async () => {
  const { page } = L
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('Please read the fixture page and tell me the phrase')
  await box.press('Enter')
  await expect(page.getByText('The page says the secret phrase.')).toBeVisible()
  // the snapshot text reached the model in the next request
  const reqs = await mock.requests()
  const toolTexts = reqs.flatMap((r) => (r.body?.messages ?? []).filter((m: any) => m.role === 'tool').map((m: any) => JSON.stringify(m.content)))
  expect(toolTexts.some((t: string) => t.includes('Navigated to') && t.includes('/agent'))).toBe(true)
  expect(toolTexts.some((t: string) => t.includes('ODEX-E2E-MARKER-42'))).toBe(true)
  // the agent's tab shows up in the strip with the agent indicator
  const strip = page.getByRole('tablist', { name: 'Browser tabs' })
  await expect(strip.getByRole('tab', { name: /Agent Fixture Page/ })).toBeVisible()
  await expect(page.getByRole('status').filter({ hasText: /Agent tab|The agent is using this tab/ })).toBeVisible()
  const st = await state(page)
  const agentTab = st.tabs.find((t: any) => t.title === 'Agent Fixture Page')
  expect(agentTab.threadId).toBeTruthy()
  await shot('agent-light')
})

test('the agent can screenshot its tab while the browser panel is closed', async () => {
  const { page } = L
  await page.getByRole('button', { name: 'Close side panel' }).click()
  await expect(page.getByRole('tablist', { name: 'Browser tabs' })).toBeHidden()
  await expect.poll(attachedPages).toBe(0)
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('Now take a screenshot of it')
  await box.press('Enter')
  await expect(page.getByText('Captured the page.')).toBeVisible()
  const reqs = await mock.requests()
  const toolTexts = reqs.flatMap((r) => (r.body?.messages ?? []).filter((m: any) => m.role === 'tool').map((m: any) => JSON.stringify(m.content)))
  expect(toolTexts.some((t: string) => /Screenshot \d+x\d+/.test(t))).toBe(true)
  // reopen the panel on the browser tab for the next tests
  await page.keyboard.press('Control+T')
  await expect(page.getByRole('tablist', { name: 'Browser tabs' })).toBeVisible()
})

test('browser settings: site lists, developer mode and history', async () => {
  const { page } = L
  await page.keyboard.press('Control+,')
  await page.getByRole('button', { name: 'Browser', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Browser', exact: true })).toBeVisible()
  // the browser panel is gone with the thread view
  await expect.poll(attachedPages).toBe(0)

  await page.getByRole('switch', { name: 'Developer mode' }).click()
  await expect(page.getByRole('switch', { name: 'Developer mode' })).toHaveAttribute('aria-checked', 'true')
  await page.getByRole('textbox', { name: 'Add to Allowed sites' }).fill('https://docs.rs/serde')
  await page.getByRole('textbox', { name: 'Add to Allowed sites' }).press('Enter')
  await expect(page.getByLabel('Allowed sites').getByText('https://docs.rs')).toBeVisible()
  const cfg = await page.evaluate(() => (window as any).odex.request('config/read', {}))
  expect(cfg.effective.browser.developer_mode).toBe(true)
  expect(cfg.effective.browser.allowed_sites).toContain('https://docs.rs')

  await expect(page.getByText('Odex Fixture Page').first()).toBeVisible()
  await shot('settings-browser-light')
  await page.getByRole('button', { name: 'Last hour' }).click()
  await expect(page.getByText('No history yet.')).toBeVisible()
  const h = await page.evaluate(() => (window as any).odex.browser.history())
  expect(h.length).toBe(0)
})

test('computer use settings: status, kill switch and toggles', async () => {
  const { page } = L
  await page.getByRole('button', { name: 'Computer use', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Computer use', exact: true })).toBeVisible()
  const status = page.getByRole('status', { name: 'Computer use status' })
  await expect(status).toContainText(/Computer use is off|Not available/)

  await page.getByRole('switch', { name: 'Enable computer use' }).click()
  await expect(page.getByRole('switch', { name: 'Enable computer use' })).toHaveAttribute('aria-checked', 'true')
  const cfg = await page.evaluate(() => (window as any).odex.request('config/read', {}))
  expect(cfg.effective.computer_use.enabled).toBe(true)
  await page.getByRole('textbox', { name: 'Add to Allowed apps' }).fill('notepad.exe')
  await page.getByRole('textbox', { name: 'Add to Allowed apps' }).press('Enter')
  await expect(page.getByLabel('Allowed apps').getByText('notepad.exe')).toBeVisible()

  await page.getByRole('button', { name: 'Engage kill switch' }).click()
  await expect(status).toContainText('Kill switch engaged')
  expect(await page.evaluate(() => (window as any).odex.app.killSwitchState())).toBe(true)
  const cu = await page.evaluate(() => (window as any).odex.request('computerUse/status', {}))
  expect(cu.killed).toBe(true)
  await shot('settings-computer-killed-light')
  await page.getByRole('button', { name: 'Release kill switch' }).first().click()
  await expect(status).not.toContainText('Kill switch engaged')
  await shot('settings-computer-light')
  await expect(page.getByRole('table', { name: 'Open windows' }).or(page.getByText('No windows found.'))).toBeVisible()
  await scrollSettings('bottom')
  await shot('settings-computer-bottom-light')
})

test('dark theme screenshots', async () => {
  const { page } = L
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'dark' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  await shot('settings-computer-bottom-dark')
  await scrollSettings('top')
  await shot('settings-computer-dark')
  await page.getByRole('button', { name: 'Browser', exact: true }).click()
  await shot('settings-browser-dark')
  await page.getByRole('button', { name: 'Back to app' }).click()
  const strip = page.getByRole('tablist', { name: 'Browser tabs' })
  await expect(strip).toBeVisible()
  // the blank tab opened last shows the start page, not a native view
  await expect(page.getByText('Local servers')).toBeVisible()
  await expect.poll(attachedPages).toBe(0)
  await shot('ntp-dark')
  await strip.getByRole('tab', { name: /Agent Fixture Page/ }).click()
  await expect.poll(attachedPages).toBe(1)
  await shot('agent-dark')
  await page.getByRole('button', { name: 'History', exact: true }).click()
  await shot('history-dark')
  await page.keyboard.press('Escape')
})
