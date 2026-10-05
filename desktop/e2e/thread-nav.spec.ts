import { test, expect, type Locator, type Page } from '@playwright/test'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { addProject, engineReady, launch, startMock, type Launched, type Mock, type MockRule } from './harness'

// Goal pause/resume/edit, model warnings, Activity "Running", fork to a worktree,
// roll back, redo, recent-thread shortcuts, /status /doctor /memories, the
// rebindable interrupt, mermaid diagrams, the image lightbox and "Ask Odex".

const SHOTS = process.env.ODEX_SHOTS || path.join(os.tmpdir(), 'odex-thread-nav-shots')

const RULES: MockRule[] = [
  // titles fall back to the first words of the message
  { when: { system_contains: 'Write a short title' }, reply: { kind: 'text', text: '' } },
  // memory proposals (matched by prompt: Doctor may switch structured output off for the model)
  { when: { system_contains: 'long-lived memories' }, reply: { kind: 'text', text: JSON.stringify({ memories: [{ text: 'The demo project prints add(2, 3) from main.py', category: 'stack', scope: 'project' }] }) } },
  { when: { structured: true }, reply: { kind: 'text', text: '{}' } },
  { when: { last_user_contains: '[goal]' }, reply: { kind: 'text', text: 'Parser shipped and verified.\n\nGOAL: DONE' } },
  { when: { last_user_contains: 'Goal: Ship the parser' }, reply: { kind: 'stall', ms: 120_000 } },
  { when: { last_user_contains: 'please stall' }, reply: { kind: 'stall', ms: 120_000 } },
  {
    when: { last_user_contains: 'diagram' },
    reply: { kind: 'text', text: 'Here is the flow:\n\n```mermaid\ngraph TD\n  A[Request] --> B{Cached?}\n  B -->|yes| C[Serve]\n  B -->|no| D[Fetch]\n```\n\nThat is all.' },
  },
  { when: { last_user_contains: 'apples' }, reply: { kind: 'text', text: 'Apples are crisp and sweet. They grow on trees in orchards.' } },
  { when: { last_user_contains: 'pears' }, reply: { kind: 'text', text: 'Pears ripen off the tree.' } },
  { when: {}, reply: { kind: 'text', text: 'Noted.' } },
]

let mock: Mock
let L: Launched
let t1 = ''
let forkId = ''
let goalId = ''

test.beforeAll(async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  mock = await startMock(RULES)
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  await addProject(L.page, L.project)
  await L.page.getByRole('heading', { name: /What should we/ }).waitFor()
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

// ------------------------------------------------------------------ helpers

const box = (page: Page) => page.getByRole('textbox', { name: 'Message' })

async function selectedId(page: Page): Promise<string> {
  return page.evaluate(() => (window as any).__odexStore.getState().selectedThreadId as string)
}

async function threadOf(page: Page, id: string): Promise<any> {
  return page.evaluate((tid) => (window as any).__odexStore.getState().threads[tid]?.thread, id)
}

async function selectThread(page: Page, id: string): Promise<void> {
  await page.evaluate((tid) => (window as any).__odexStore.getState().selectThread(tid), id)
  await expect.poll(() => selectedId(page)).toBe(id)
}

async function rpc(page: Page, method: string, params: unknown): Promise<any> {
  return page.evaluate(({ m, p }) => (window as any).odex.request(m, p), { m: method, p: params })
}

async function pickProject(page: Page): Promise<void> {
  const chip = page.getByRole('button', { name: 'No project', exact: true })
  if (await chip.isVisible()) {
    await chip.click()
    await page.getByRole('menuitem', { name: /^project/ }).click()
  }
}

async function send(page: Page, text: string, reply?: string | RegExp): Promise<void> {
  await box(page).fill(text)
  await box(page).press('Enter')
  if (reply) await expect(page.getByText(reply).last()).toBeVisible()
}

/** Light and dark screenshots of `target` (or the window) for review. */
async function shots(page: Page, name: string, target?: Locator): Promise<void> {
  for (const theme of ['light', 'dark'] as const) {
    await page.evaluate((t) => (window as any).odex.settings.set({ theme: t }), theme)
    await page.waitForFunction((t) => document.documentElement.dataset.theme === t, theme)
    await page.waitForTimeout(250)
    const file = path.join(SHOTS, `${name}-${theme}.png`)
    if (target) await target.screenshot({ path: file })
    else await page.screenshot({ path: file })
  }
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
}

/** A neutral spot for keyboard shortcuts (not a text field). */
async function focusThread(page: Page): Promise<void> {
  await page.locator('.thread-header .name').click()
}

// ------------------------------------------------------------------ tests

test('/status shows thread id, model, endpoint, server version and token usage', async () => {
  const { page } = L
  await pickProject(page)
  await send(page, 'tell me about apples', 'Apples are crisp and sweet.')
  t1 = await selectedId(page)
  await rpc(page, 'thread/update', { threadId: t1, name: 'Alpha thread' })
  await send(page, '/status')
  const dlg = page.getByRole('dialog', { name: 'Status & context' })
  await expect(dlg).toBeVisible()
  const st = dlg.getByLabel('Thread status')
  await expect(st).toContainText(t1)
  await expect(st).toContainText('mock-coder')
  await expect(st).toContainText(`${mock.url}/v1`)
  await expect(st).toContainText('0.30.0-mock')
  const usage = dlg.getByLabel('Token usage')
  await expect(usage).toContainText('input')
  await expect(usage).toContainText('cached')
  await expect(usage).toContainText('output')
  await shots(page, 'status', dlg)
  await dlg.getByRole('button', { name: 'Copy thread id' }).click()
  await expect(page.getByText('Thread id copied')).toBeVisible()
  // the clipboard only takes writes while the window has focus (not guaranteed under test)
  const clip = await L.app.evaluate(({ clipboard }) => clipboard.readText())
  if (clip) expect(clip).toBe(t1)
  await page.keyboard.press('Escape')
  await expect(dlg).toBeHidden()
})

test('/doctor runs quick checks for the thread model in a modal', async () => {
  const { page } = L
  await send(page, '/doctor')
  const dlg = page.getByRole('dialog', { name: 'Doctor' })
  await expect(dlg).toBeVisible()
  await expect(dlg).toContainText('mock:mock-coder')
  await expect(dlg.locator('.card').first()).toContainText('mock-coder', { timeout: 60_000 })
  await expect(dlg.locator('.card').first()).toContainText('vLLM 0.30.0-mock')
  await shots(page, 'doctor', dlg)
  await dlg.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(dlg).toBeHidden()
})

test('/memories turns memories on and off and generates proposals', async () => {
  const { page } = L
  await send(page, '/memories off')
  await expect.poll(async () => (await threadOf(page, t1)).memoriesEnabled).toBe(false)
  await send(page, '/memories on')
  await expect.poll(async () => (await threadOf(page, t1)).memoriesEnabled).toBe(true)
  await send(page, '/memories generate')
  await expect(page.getByText('Proposed 1 memory · review in Settings → Memories')).toBeVisible()
  const list = await rpc(page, 'memory/list', {})
  expect(JSON.stringify(list)).toContain('prints add(2, 3)')
})

test('a mermaid block renders as a diagram (light and dark)', async () => {
  const { page } = L
  await send(page, 'draw a diagram of the cache', 'That is all.')
  const block = page.locator('.mermaid-block')
  await expect(block.locator('.mermaid-svg svg')).toBeVisible()
  await expect(block.locator('.mermaid-fallback')).toBeHidden()
  await expect(block.locator('.mermaid-svg svg')).toContainText('Cached?')
  await shots(page, 'mermaid', block)
  // re-rendered for the dark theme too (still a diagram, not the fallback)
  await expect(block.locator('.mermaid-svg svg')).toBeVisible()
})

test('chat images open in a lightbox with zoom and save', async () => {
  const { page, app, home } = L
  const dataUrl = await page.evaluate(() => {
    const c = document.createElement('canvas')
    c.width = 640
    c.height = 400
    const g = c.getContext('2d')!
    const grad = g.createLinearGradient(0, 0, 640, 400)
    grad.addColorStop(0, '#4f6bed')
    grad.addColorStop(1, '#1f9d55')
    g.fillStyle = grad
    g.fillRect(0, 0, 640, 400)
    g.fillStyle = '#fff'
    g.font = 'bold 48px sans-serif'
    g.fillText('screenshot', 180, 215)
    return c.toDataURL('image/png')
  })
  await rpc(page, 'turn/start', { threadId: t1, input: [{ type: 'text', text: 'what is in this picture' }, { type: 'image', url: dataUrl }] })
  const att = page.locator('.user-bubble .attachment.image-attachment').last()
  await expect(att).toBeVisible()
  await expect(page.getByText('Noted.').last()).toBeVisible()
  await att.click()
  const lb = page.getByRole('dialog', { name: /^Image:/ })
  await expect(lb).toBeVisible()
  await expect(lb).toContainText('640×400')
  const pct = lb.getByLabel('Zoom level')
  await lb.getByRole('button', { name: 'Zoom in' }).click()
  await expect(pct).toHaveText('125%')
  await lb.getByRole('button', { name: 'Actual size' }).click()
  await expect(pct).toHaveText('100%')
  await page.mouse.move(640, 450)
  await page.mouse.wheel(0, -300)
  await expect(pct).not.toHaveText('100%')
  await lb.getByRole('button', { name: /Fit/ }).click()
  await shots(page, 'lightbox')
  const target = path.join(home, 'saved-image.png')
  await app.evaluate(({ dialog }, p) => {
    ;(dialog as any).showSaveDialog = async () => ({ canceled: false, filePath: p })
  }, target)
  await lb.getByRole('button', { name: 'Save' }).click()
  await expect.poll(() => fs.existsSync(target)).toBe(true)
  expect(fs.readFileSync(target).subarray(1, 4).toString()).toBe('PNG')
  await page.keyboard.press('Escape')
  await expect(lb).toBeHidden()
})

test('Ask Odex quotes selected thread text into the composer', async () => {
  const { page } = L
  const para = page.locator('.markdown p', { hasText: 'Apples are crisp and sweet' }).first()
  await para.scrollIntoViewIfNeeded()
  await para.click({ clickCount: 3 })
  const ask = page.getByRole('button', { name: 'Ask Odex' })
  await expect(ask).toBeVisible()
  await shots(page, 'ask-odex')
  await ask.click()
  await expect(box(page)).toHaveValue(/^> Apples are crisp and sweet\. They grow on trees in orchards\.\n\n$/)
  await expect(box(page)).toBeFocused()
  await expect(ask).toBeHidden()
  await box(page).fill('')

  // a selection in the code editor quotes the file and line range
  await page.evaluate((p) => {
    const s = (window as any).__odexStore
    s.setState({ fileToOpen: { path: p, at: Date.now() } })
    s.getState().setUi({ sidePanelOpen: true, sidePanelTab: 'files' })
  }, path.join(L.project, 'main.py'))
  const editor = page.locator('.cm-content', { hasText: 'def add' })
  await expect(editor).toBeVisible()
  await editor.click()
  await page.keyboard.press('Control+A')
  await expect(ask).toBeVisible()
  await ask.click()
  await expect(box(page)).toHaveValue('From `main.py` lines 1-4:\n```python\ndef add(a, b):\n    return a + b\n\nprint(add(2, 3))\n```\n\n')
  await box(page).fill('')
  await page.evaluate(() => (window as any).__odexStore.getState().setUi({ sidePanelOpen: false }))
})

test('roll back to here removes the message and later turns without resending', async () => {
  const { page } = L
  const before = (await mock.requests()).length
  const bubble = page.locator('.user-bubble', { hasText: 'draw a diagram' })
  await bubble.hover()
  await page.locator('.item-user', { has: bubble }).getByRole('button', { name: 'Roll back to here' }).click()
  const dlg = page.getByRole('dialog', { name: 'Roll back to here?' })
  await expect(dlg).toContainText('the 1 turn after it')
  await expect(dlg).toContainText('draw a diagram of the cache')
  await dlg.getByLabel('Also restore files').check()
  await expect(dlg.getByRole('button', { name: 'Roll back and restore files' })).toBeVisible()
  await dlg.getByLabel('Also restore files').uncheck()
  await shots(page, 'rollback', dlg)
  await dlg.getByRole('button', { name: 'Roll back', exact: true }).click()
  await expect(dlg).toBeHidden()
  await expect(page.locator('.user-bubble', { hasText: 'draw a diagram' })).toHaveCount(0)
  await expect(page.locator('.user-bubble', { hasText: 'what is in this picture' })).toHaveCount(0)
  await expect(page.locator('.mermaid-block')).toHaveCount(0)
  await expect(page.getByText('Apples are crisp and sweet.').first()).toBeVisible()
  // the message is back in the composer; nothing was sent
  await expect(box(page)).toHaveValue('draw a diagram of the cache')
  expect((await mock.requests()).length).toBe(before)
  const r = await rpc(page, 'thread/read', { threadId: t1 })
  expect(r.turns.length).toBe(1)
  await box(page).fill('')
})

test('fork from a message into a new worktree', async () => {
  const { page } = L
  const bubble = page.locator('.user-bubble', { hasText: 'tell me about apples' })
  await bubble.hover()
  await page.locator('.item-user', { has: bubble }).getByRole('button', { name: 'Fork from here' }).click()
  await expect(page.getByRole('menuitem', { name: /To a new thread/ })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: /To a new worktree/ })).toBeVisible()
  await shots(page, 'fork-menu')
  await page.getByRole('menuitem', { name: /To a new worktree/ }).click()
  await expect.poll(() => selectedId(page)).not.toBe(t1)
  forkId = await selectedId(page)
  await expect.poll(async () => !!(await threadOf(page, forkId))?.worktree, { timeout: 30_000 }).toBe(true)
  await expect(page.locator('.thread-header .badge.accent')).toBeVisible()
  await expect(page.locator('.user-bubble', { hasText: 'tell me about apples' })).toBeVisible()
  const fork = await threadOf(page, forkId)
  expect(fork.parentThreadId).toBe(t1)
  expect(fork.runMode).toBe('worktree')
  await rpc(page, 'thread/update', { threadId: forkId, name: 'Beta thread' })
})

test('/goal sets a goal with time and token budgets', async () => {
  const { page } = L
  await page.getByRole('button', { name: 'New thread' }).first().click()
  await pickProject(page)
  await send(page, '/goal --time 30m --tokens 200k Ship the parser')
  const row = page.getByRole('status', { name: 'Goal' })
  await expect(row).toContainText('Ship the parser')
  await expect(row).toContainText('active')
  await expect(row).toContainText('/ 30m')
  await expect(row).toContainText('/ 200k tok')
  goalId = await selectedId(page)
  const t = await threadOf(page, goalId)
  expect(t.goal.timeBudgetSecs).toBe(1800)
  expect(t.goal.tokenBudget).toBe(200_000)
  await expect.poll(async () => (await threadOf(page, goalId)).status).toBe('running')
  await rpc(page, 'thread/update', { threadId: goalId, name: 'Gamma thread' })
})

test('Activity lists running threads; Mark all read clears unread threads', async () => {
  const { page } = L
  await rpc(page, 'thread/update', { threadId: t1, unread: true })
  await page.evaluate(() => window.dispatchEvent(new CustomEvent('odex:shortcut', { detail: 'activity' })))
  const running = page.getByRole('region', { name: 'Running' })
  await expect(running).toContainText('Gamma thread')
  await expect(running).toContainText('Goal: Ship the parser')
  const attention = page.getByRole('region', { name: 'Needs attention' })
  await expect(attention).toContainText('Alpha thread')
  await shots(page, 'activity')
  await page.getByRole('button', { name: 'Mark all read' }).click()
  await expect.poll(async () => (await threadOf(page, t1)).unread).toBe(false)
  await expect(page.getByRole('region', { name: 'Needs attention' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Mark all read' })).toBeDisabled()
  await selectThread(page, goalId)
})

test('goal: pause stops the turn, edit keeps it paused, resume continues it', async () => {
  const { page } = L
  const row = page.getByRole('status', { name: 'Goal' })
  await row.getByRole('button', { name: 'Pause goal' }).click()
  await expect(row).toContainText('paused')
  await expect.poll(async () => (await threadOf(page, goalId)).status).toBe('idle')
  await shots(page, 'goal-paused', row)

  await row.getByRole('button', { name: 'Edit goal' }).click()
  const dlg = page.getByRole('dialog', { name: 'Edit goal' })
  await expect(dlg.getByLabel('Time budget')).toHaveValue('30m')
  await expect(dlg.getByLabel('Token budget')).toHaveValue('200k')
  await dlg.getByLabel('Goal objective').fill('Ship the parser v2')
  await dlg.getByLabel('Time budget').fill('1h')
  await dlg.getByLabel('Token budget').fill('lots')
  await expect(dlg.getByText('Use a count like 200k or 1.5m')).toBeVisible()
  await expect(dlg.getByRole('button', { name: 'Save' })).toBeDisabled()
  await dlg.getByLabel('Token budget').fill('500k')
  await shots(page, 'goal-edit', dlg)
  await dlg.getByRole('button', { name: 'Save' }).click()
  await expect(dlg).toBeHidden()
  await expect(row).toContainText('Ship the parser v2')
  await expect(row).toContainText('/ 1h')
  await expect(row).toContainText('/ 500k tok')
  await expect(row).toContainText('paused')
  const t = await threadOf(page, goalId)
  expect(t.status).toBe('idle')
  expect(t.goal.timeBudgetSecs).toBe(3600)
  expect(t.goal.tokenBudget).toBe(500_000)

  await row.getByRole('button', { name: 'Resume goal' }).click()
  await expect(page.getByText('Parser shipped and verified.')).toBeVisible()
  await expect(row).toContainText('done')
  const req = (await mock.requests()).map((r) => JSON.stringify(r.body?.messages ?? [])).find((m) => m.includes('[goal] Resume'))
  expect(req).toContain('Ship the parser v2')

  // options alone edit the budget (and reopen the finished goal)
  await send(page, '/goal --tokens 300k')
  await expect.poll(async () => (await threadOf(page, goalId)).goal.tokenBudget).toBe(300_000)
  await expect.poll(async () => (await threadOf(page, goalId)).goal.status).toBe('done')
  await send(page, '/goal clear')
  await expect(row).toBeHidden()
})

test('model warnings: a model the endpoint stopped serving, and a shrunk context window', async () => {
  const { page } = L
  await selectThread(page, t1)
  await rpc(page, 'thread/update', { threadId: t1, model: 'mock:mock-coder-xl' })
  const banner = page.getByRole('alert', { name: 'Model warning' })
  await expect(banner).toContainText('mock-coder-xl is not served by Mock vLLM anymore')
  await expect(banner).toContainText('it now serves mock-coder (same family)')
  await shots(page, 'model-not-served', banner)
  await banner.getByRole('button', { name: /^Use mock-coder/ }).click()
  await expect(banner).toHaveCount(0)
  expect((await threadOf(page, t1)).model).toBe('mock:mock-coder')

  // a turn records the window the thread runs with
  await send(page, 'tell me about pears', 'Pears ripen off the tree.')
  await expect.poll(async () => (await threadOf(page, t1)).lastModel?.contextWindow).toBe(32768)
  const small = await startMock(RULES, { maxModelLen: 16384 })
  try {
    await rpc(page, 'provider/upsert', { id: 'mock', provider: { name: 'Mock vLLM', base_url: `${small.url}/v1`, headers: {}, query_params: {} } })
    await expect(banner).toContainText('shrank from 32K to 16K tokens')
    await shots(page, 'model-window', banner)
    // the next turn notes it in the transcript and the banner clears
    await send(page, 'more about pears please', /Pears ripen off the tree/)
    await expect(page.locator('.notice.warning', { hasText: 'shrank from 32K to 16K' })).toBeVisible()
    await expect(banner).toHaveCount(0)
  } finally {
    await rpc(page, 'provider/upsert', { id: 'mock', provider: { name: 'Mock vLLM', base_url: `${mock.url}/v1`, headers: {}, query_params: {} } })
    small.stop()
  }
})

test('the interrupt shortcut is rebindable; Esc defers to the binding', async () => {
  const { page } = L
  const status = async () => (await threadOf(page, t1)).status
  await page.evaluate(() => (window as any).odex.settings.set({ shortcuts: { interrupt: 'Mod+Shift+K' } }))
  await send(page, 'please stall for a while')
  await expect.poll(status).toBe('running')
  await box(page).focus()
  await box(page).press('Escape')
  await focusThread(page)
  await page.keyboard.press('Escape')
  await page.waitForTimeout(700)
  expect(await status()).toBe('running')
  await page.keyboard.press('Control+Shift+K')
  await expect.poll(status).toBe('idle')

  // default binding: Esc works from the composer and from the thread
  await page.evaluate(() => (window as any).odex.settings.set({ shortcuts: {} }))
  await send(page, 'please stall once more')
  await expect.poll(status).toBe('running')
  await box(page).press('Escape')
  await expect.poll(status).toBe('idle')
  await send(page, 'please stall one last time')
  await expect.poll(status).toBe('running')
  await focusThread(page)
  await page.keyboard.press('Escape')
  await expect.poll(status).toBe('idle')
})

test('redo (Ctrl+Shift+Z / Ctrl+Y) re-applies an undone action', async () => {
  const { page } = L
  const pinned = async () => (await threadOf(page, t1)).pinned
  await focusThread(page)
  await page.keyboard.press('Control+Alt+P')
  await expect.poll(pinned).toBe(true)
  await page.keyboard.press('Control+Z')
  await expect.poll(pinned).toBe(false)
  await page.keyboard.press('Control+Shift+Z')
  await expect.poll(pinned).toBe(true)
  await page.keyboard.press('Control+Z')
  await expect.poll(pinned).toBe(false)
  await page.keyboard.press('Control+Y')
  await expect.poll(pinned).toBe(true)
  await page.keyboard.press('Control+Y')
  await expect(page.getByText('Nothing to redo')).toBeVisible()
  await page.keyboard.press('Control+Z')
  await expect.poll(pinned).toBe(false)
})

test('recent-thread shortcuts follow the navigation history', async () => {
  const { page } = L
  await selectThread(page, forkId)
  await selectThread(page, goalId)
  await selectThread(page, t1)
  const name = page.locator('.thread-header .name')
  await expect(name).toHaveText('Alpha thread')
  await focusThread(page)
  await page.keyboard.press('Control+Alt+1')
  await expect(name).toHaveText('Gamma thread')
  // now: Alpha was the last one before Gamma, Beta before that
  await page.keyboard.press('Control+Alt+1')
  await expect(name).toHaveText('Alpha thread')
  await page.keyboard.press('Control+Alt+2')
  await expect(name).toHaveText('Beta thread')
})
