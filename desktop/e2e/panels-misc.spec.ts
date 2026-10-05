import { test, expect, type Page } from '@playwright/test'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { addProject, desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

// Side panel (+ menu, reorder, layouts), Files (HTML preview, back/forward, recent, agent-edit
// annotations, image save), Sources save-as, the summary card, "Always allow" rules, MCP server
// instructions, the built-in skill-creator, automation templates, memory suggestions, the tray
// usage tooltip, General settings additions + deep settings search, and the Quick Chat window.
// Set ODEX_SHOTS=<dir> to save light/dark screenshots of the new UI.

const SHOTS = process.env.ODEX_SHOTS
const WIN = process.platform === 'win32'
const echo = (s: string) => (WIN ? `Write-Output ${s}` : `echo ${s}`)
const PREFIX = WIN ? 'write-output' : 'echo'
const FIXTURE = path.join(desktopDir, 'e2e', 'fixtures', 'mcp-server.mjs')

const SUMMARY = {
  goal_and_requirements: ['Keep main.py tidy and documented'],
  decisions: [{ decision: 'Keep add() pure', reason: 'simpler tests' }],
  plan: [],
  files_changed: [{ path: 'main.py', purpose: 'documents add()', state: 'edited' }],
  codebase_facts: ['main.py defines add(a, b)'],
  commands_and_tests: [],
  open_errors: [],
  next_steps: ['Run the test suite'],
  important_refs: [],
}

let mock: Mock
let L: Launched
let out: string

async function rpc(page: Page, method: string, params: unknown = {}): Promise<any> {
  return page.evaluate(([m, p]) => (window as any).odex.request(m, p), [method, params] as const)
}

async function shoot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return
  fs.mkdirSync(SHOTS, { recursive: true })
  for (const theme of ['light', 'dark'] as const) {
    await page.evaluate((t) => (window as any).odex.settings.set({ theme: t }), theme)
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
    await page.waitForTimeout(250)
    await page.screenshot({ path: path.join(SHOTS, `${name}-${theme}.png`) })
  }
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
}

const side = (page: Page) => page.getByRole('complementary', { name: 'Side panel' })
const sideTabs = (page: Page) => side(page).getByRole('tablist', { name: 'Side panel tabs' })
const sideTab = (page: Page, name: string) => sideTabs(page).getByRole('tab', { name, exact: true })

async function openSide(page: Page, tab?: string): Promise<void> {
  if (!(await side(page).isVisible().catch(() => false))) await page.getByRole('button', { name: 'Toggle side panel' }).click()
  await expect(side(page)).toBeVisible()
  if (tab) {
    await sideTab(page, tab).click()
    await expect(sideTab(page, tab)).toHaveAttribute('aria-selected', 'true')
  }
}

/** Make the next native save / open dialog return `result` (main process stub). */
async function stubSaveDialog(path_: string): Promise<void> {
  await L.app.evaluate(({ dialog }, p) => {
    ;(dialog as any).showSaveDialog = async () => ({ canceled: false, filePath: p })
  }, path_)
}

async function send(page: Page, text: string): Promise<void> {
  const box = page.getByRole('textbox', { name: 'Message' }).last()
  await box.fill(text)
  await box.press('Enter')
}

test.beforeAll(async () => {
  mock = await startMock([
    { when: { structured_name: 'context_summary' }, reply: { kind: 'json', value: SUMMARY } },
    { when: { structured_name: 'memories' }, reply: { kind: 'json', value: { memories: [{ text: 'Runs end-to-end tests with Playwright', category: 'convention', scope: 'global' }] } } },
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Panels test"}' } },
    {
      when: { last_role: 'user', last_user_contains: 'edit main' },
      reply: { kind: 'tool_calls', calls: [{ name: 'edit_file', arguments: { path: 'main.py', old_string: '    return a + b\n', new_string: '    # add two numbers\n    return a + b  # simple\n' } }] },
    },
    { when: { last_role: 'tool', last_user_contains: 'edit main' }, reply: { kind: 'text', text: 'Edited main.py.' } },
    { when: { last_role: 'user', last_user_contains: 'run always' }, reply: { kind: 'tool_calls', calls: [{ name: 'shell', arguments: { command: echo('always-one'), escalated: true, justification: 'test' } }] } },
    { when: { last_role: 'tool', last_user_contains: 'run always' }, reply: { kind: 'text', text: 'Ran it.' } },
    { when: { last_role: 'user', last_user_contains: 'run again' }, reply: { kind: 'tool_calls', calls: [{ name: 'shell', arguments: { command: echo('always-two'), escalated: true, justification: 'test' } }] } },
    { when: { last_role: 'tool', last_user_contains: 'run again' }, reply: { kind: 'text', text: 'Ran it again.' } },
    { when: { last_role: 'user', last_user_contains: 'quick hello' }, reply: { kind: 'text', text: 'Hi from quick chat.' } },
    { when: { last_role: 'user', last_user_contains: 'check the mcp instructions' }, reply: { kind: 'text', text: 'MCP check done.' } },
    { when: {}, reply: { kind: 'text', text: 'Noted.' } },
  ])
  L = await launch({ mockUrl: mock.url })
  out = fs.mkdtempSync(path.join(os.tmpdir(), 'odex-s-out-'))
  await engineReady(L.page)
  await addProject(L.page, L.project)
  fs.writeFileSync(
    path.join(L.project, 'page.html'),
    '<!doctype html><html><head><link rel="stylesheet" href="style.css"></head><body><h1 id="t">static</h1>' +
      '<script>document.getElementById("t").textContent = "scripted " + (1 + 1);' +
      'try { void parent.document.title; document.body.dataset.iso = "no" } catch (e) { document.body.dataset.iso = "yes" }</script></body></html>\n',
  )
  fs.writeFileSync(path.join(L.project, 'style.css'), 'h1 { color: rgb(200, 10, 20); }\n')
  // 1x1 PNG
  fs.writeFileSync(path.join(L.project, 'logo.png'), Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64'))
  // pick the project for new threads
  await L.page.getByRole('button', { name: 'No project', exact: true }).click()
  await L.page.getByRole('menuitem', { name: /^project/ }).click()
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
  try {
    fs.rmSync(out, { recursive: true, force: true })
  } catch {}
})

test('side panel + menu opens a terminal, a browser tab and a file', async () => {
  const { page, app, project } = L
  await openSide(page, 'Review')
  await side(page).getByRole('button', { name: 'New side panel tab' }).click()
  const menu = page.getByRole('menu')
  await expect(menu.getByRole('menuitem', { name: 'Terminal' })).toBeVisible()
  await expect(menu.getByRole('menuitem', { name: /Browser tab/ })).toBeVisible()
  await expect(menu.getByRole('menuitem', { name: 'File…' })).toBeVisible()
  await shoot(page, 'side-plus-menu')
  await menu.getByRole('menuitem', { name: 'Terminal' }).click()
  await expect(sideTab(page, 'Terminal')).toHaveAttribute('aria-selected', 'true')
  await expect(side(page).locator('.xterm-rows').first()).toBeVisible()
  expect((await page.evaluate(() => (window as any).odex.terminals.list(null))).length).toBeGreaterThanOrEqual(1)

  await side(page).getByRole('button', { name: 'New side panel tab' }).click()
  await page.getByRole('menu').getByRole('menuitem', { name: /Browser tab/ }).click()
  await expect(sideTab(page, 'Browser')).toHaveAttribute('aria-selected', 'true')
  await expect.poll(async () => (await page.evaluate(() => (window as any).odex.browser.state())).tabs.length).toBeGreaterThanOrEqual(1)

  // File… goes through the native open dialog (stubbed) into the Files tab
  const target = path.join(project, 'README.md')
  await app.evaluate(({ dialog }, p) => {
    ;(dialog as any).showOpenDialog = async () => ({ canceled: false, filePaths: [p] })
  }, target)
  await side(page).getByRole('button', { name: 'New side panel tab' }).click()
  await page.getByRole('menu').getByRole('menuitem', { name: 'File…' }).click()
  await expect(sideTab(page, 'Files')).toHaveAttribute('aria-selected', 'true')
  await expect(side(page).getByRole('tab', { name: 'README.md' })).toHaveAttribute('aria-selected', 'true')
})

test('side panel tabs reorder by drag and the order persists', async () => {
  const { page } = L
  await openSide(page, 'Review')
  const names = async () => (await sideTabs(page).getByRole('tab').allTextContents()).map((t) => t.trim())
  expect((await names()).slice(0, 4)).toEqual(['Review', 'Plan', 'Sources', 'Files'])
  await sideTab(page, 'Files').dragTo(sideTab(page, 'Review'))
  await expect.poll(async () => (await names()).slice(0, 4)).toEqual(['Files', 'Review', 'Plan', 'Sources'])
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem('odex.ui') || '{}').sidePanelOrder)
  expect(saved.slice(0, 2)).toEqual(['files', 'review'])
  // survives a reload
  await page.reload()
  await engineReady(page)
  await openSide(page)
  await expect.poll(async () => (await names()).slice(0, 2)).toEqual(['Files', 'Review'])
  // drag it back
  await sideTab(page, 'Files').dragTo(sideTab(page, 'Sources'))
  await expect.poll(async () => (await names()).slice(0, 4)).toEqual(['Review', 'Plan', 'Sources', 'Files'])
})

test('layout cycle: split → full width → hidden, and the chat ↔ tabs swap', async () => {
  const { page } = L
  await openSide(page, 'Review')
  const chat = page.locator('.center-main')
  await page.keyboard.press('Control+Shift+B')
  await expect(side(page)).toHaveClass(/side-panel-full/)
  await expect(chat).toBeHidden()
  await shoot(page, 'side-full-width')
  await page.keyboard.press('Control+Shift+B')
  await expect(side(page)).toBeHidden()
  await expect(chat).toBeVisible()
  await page.keyboard.press('Control+Shift+B')
  await expect(side(page)).toBeVisible()
  await expect(side(page)).not.toHaveClass(/side-panel-full/)
  // the full-width button and "Show chat" do the same
  await side(page).getByRole('button', { name: 'Full width' }).click()
  await expect(chat).toBeHidden()
  await side(page).getByRole('button', { name: 'Show chat' }).click()
  await expect(chat).toBeVisible()
  // swap: the panel moves left of the chat
  await side(page).getByRole('button', { name: 'Swap chat and panel' }).click()
  await expect(page.locator('.center-split')).toHaveClass(/swapped/)
  const sideBox = (await side(page).boundingBox())!
  const chatBox = (await chat.boundingBox())!
  expect(sideBox.x).toBeLessThan(chatBox.x)
  await shoot(page, 'side-swapped')
  await side(page).getByRole('button', { name: 'Swap chat and panel' }).click()
  await expect(page.locator('.center-split')).not.toHaveClass(/swapped/)
  expect((await side(page).boundingBox())!.x).toBeGreaterThan((await chat.boundingBox())!.x)
})

test('Ctrl+Shift+E toggles the Files tab', async () => {
  const { page } = L
  await openSide(page, 'Review')
  await page.keyboard.press('Control+Shift+E')
  await expect(sideTab(page, 'Files')).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Control+Shift+E')
  await expect(side(page)).toBeHidden()
  await page.keyboard.press('Control+Shift+E')
  await expect(side(page)).toBeVisible()
  await expect(sideTab(page, 'Files')).toHaveAttribute('aria-selected', 'true')
})

test('HTML files get a sandboxed live preview with a source toggle that refreshes on save', async () => {
  const { page, project } = L
  await openSide(page, 'Files')
  await side(page).getByRole('treeitem', { name: 'page.html' }).click()
  const frame = page.frameLocator('iframe[title="HTML preview"]')
  // inline scripts run, relative assets load, and the page cannot reach the app
  await expect(frame.locator('#t')).toHaveText('scripted 2')
  await expect(frame.locator('body')).toHaveAttribute('data-iso', 'yes')
  await expect(frame.locator('#t')).toHaveCSS('color', 'rgb(200, 10, 20)')
  await shoot(page, 'files-html-preview')
  // source view, edit, save → the preview shows the saved file
  await side(page).getByRole('button', { name: 'Show source' }).click()
  const editor = side(page).locator('.cm-content')
  await expect(editor).toContainText('scripted')
  await editor.click()
  await page.keyboard.press('Control+A')
  await page.keyboard.insertText('<!doctype html><h1 id="t">edited</h1><script>document.getElementById("t").textContent += " + js"</script>\n')
  await page.keyboard.press('Control+S')
  await expect.poll(() => fs.readFileSync(path.join(project, 'page.html'), 'utf8')).toContain('edited')
  await side(page).getByRole('button', { name: 'Show preview' }).click()
  await expect(frame.locator('#t')).toHaveText('edited + js')
  // a change on disk (e.g. by the agent) refreshes it too
  fs.writeFileSync(path.join(project, 'page.html'), '<!doctype html><h1 id="t">from disk</h1>\n')
  await expect(frame.locator('#t')).toHaveText('from disk', { timeout: 20_000 })
})

test("agent edits show as gutter annotations; back/forward and recent files", async () => {
  const { page } = L
  // a thread in the project where the agent edits main.py
  await page.keyboard.press('Control+N')
  await send(page, 'please edit main (edit main)')
  await expect(page.getByText('Edited main.py.')).toBeVisible()
  await openSide(page, 'Files')
  await side(page).getByRole('treeitem', { name: 'main.py' }).click()
  await expect(side(page).getByRole('tab', { name: 'main.py' })).toHaveAttribute('aria-selected', 'true')
  const marks = side(page).locator('.cm-agent-edit')
  await expect(marks).toHaveCount(2)
  await expect(marks.first()).toHaveAttribute('title', 'Changed by the agent (turn 1)')
  // the markers sit on lines 2 and 3 (the edited lines)
  const lines = await side(page).locator('.cm-lineNumbers .cm-gutterElement').evaluateAll((els) => els.map((e) => ({ n: e.textContent, top: e.getBoundingClientRect().top })))
  const markTops = await marks.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().top))
  const markedLines = markTops.map((t) => lines.find((l) => Math.abs(l.top - t) < 3)?.n)
  expect(markedLines).toEqual(['2', '3'])
  await shoot(page, 'files-annotations')

  // back / forward between files
  await side(page).getByRole('treeitem', { name: 'README.md' }).click()
  await expect(side(page).getByRole('tab', { name: 'README.md' })).toHaveAttribute('aria-selected', 'true')
  await side(page).getByRole('button', { name: 'Go back' }).click()
  await expect(side(page).getByRole('tab', { name: 'main.py' })).toHaveAttribute('aria-selected', 'true')
  await side(page).locator('.cm-content').click()
  await page.keyboard.press('Alt+ArrowRight')
  await expect(side(page).getByRole('tab', { name: 'README.md' })).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Alt+ArrowLeft')
  await expect(side(page).getByRole('tab', { name: 'main.py' })).toHaveAttribute('aria-selected', 'true')
  // back re-opens a closed file
  await side(page).getByRole('button', { name: 'Close README.md' }).click()
  await expect(side(page).getByRole('tab', { name: 'README.md' })).toHaveCount(0)
  await side(page).getByRole('button', { name: 'Go forward' }).click()
  await expect(side(page).getByRole('tab', { name: 'README.md' })).toHaveAttribute('aria-selected', 'true')

  // recent files
  await side(page).getByRole('button', { name: 'Close page.html' }).click()
  await side(page).getByRole('button', { name: 'Recent files' }).click()
  const recent = page.getByRole('menu')
  await expect(recent.getByRole('menuitem')).toContainText(['README.md', 'main.py', 'page.html'])
  await shoot(page, 'files-recent')
  await recent.getByRole('menuitem', { name: 'page.html' }).click()
  await expect(side(page).getByRole('tab', { name: 'page.html' })).toHaveAttribute('aria-selected', 'true')
})

test('the Plan tab shows the latest context summary after /compact', async () => {
  const { page } = L
  await send(page, 'remember that the tests live in tests/')
  await expect(page.getByText('Noted.')).toBeVisible()
  await openSide(page, 'Plan')
  await expect(side(page).getByRole('region', { name: 'Thread summary' })).toHaveCount(0)
  await send(page, '/compact keep the summary short')
  await expect(page.getByText(/context compacted .* \(summary #1, manual\)/)).toBeVisible()
  const card = side(page).getByRole('region', { name: 'Thread summary' })
  await expect(card).toBeVisible()
  await expect(card).toContainText('#1 · model summary')
  await expect(card).toContainText('Keep main.py tidy and documented')
  await expect(card).toContainText('Keep add() pure')
  await expect(card).toContainText('Run the test suite')
  await shoot(page, 'plan-summary-card')
  // files in the card open in Files
  await card.getByRole('button', { name: 'main.py' }).click()
  await expect(sideTab(page, 'Files')).toHaveAttribute('aria-selected', 'true')
  await expect(side(page).getByRole('tab', { name: 'main.py' })).toHaveAttribute('aria-selected', 'true')
  // thread/read returns it too
  const tid = await page.evaluate(() => (window as any).__odexStore.getState().selectedThreadId)
  const read = await rpc(page, 'thread/read', { threadId: tid })
  expect(read.summary.nextSteps).toEqual(['Run the test suite'])
})

test('Sources: save as and reveal; images in Files save as too', async () => {
  const { page, app, project } = L
  await openSide(page, 'Sources')
  const row = side(page).getByRole('listitem', { name: 'main.py' })
  await expect(row).toBeVisible()
  await row.hover()
  await shoot(page, 'sources-actions')
  const dest = path.join(out, 'copy-of-main.py')
  await stubSaveDialog(dest)
  await row.getByRole('button', { name: 'Save a copy of main.py' }).click()
  await expect.poll(() => fs.existsSync(dest)).toBe(true)
  expect(fs.readFileSync(dest, 'utf8')).toBe(fs.readFileSync(path.join(project, 'main.py'), 'utf8'))
  await expect(page.getByText(/Saved a copy of main\.py/)).toBeVisible()
  // reveal goes to the OS file manager (stubbed)
  await app.evaluate(({ shell }) => {
    ;(globalThis as any).__revealed = []
    ;(shell as any).showItemInFolder = (p: string) => (globalThis as any).__revealed.push(p)
  })
  await row.hover()
  await row.getByRole('button', { name: 'Reveal main.py' }).click()
  await expect.poll(() => app.evaluate(() => (globalThis as any).__revealed as string[])).toEqual([expect.stringMatching(/main\.py$/)])

  // image preview → Save as…
  await openSide(page, 'Files')
  await side(page).getByRole('treeitem', { name: 'logo.png' }).click()
  await expect(side(page).getByRole('img', { name: 'logo.png' })).toBeVisible()
  const png = path.join(out, 'logo-copy.png')
  await stubSaveDialog(png)
  await side(page).getByRole('button', { name: 'Save logo.png as' }).click()
  await expect.poll(() => fs.existsSync(png)).toBe(true)
  expect(fs.readFileSync(png).equals(fs.readFileSync(path.join(project, 'logo.png')))).toBe(true)
  await shoot(page, 'files-image-save')
})

test('"Always allow" persists an exec rule that the next command honors', async () => {
  const { page, home } = L
  await page.keyboard.press('Control+N')
  await send(page, 'please run always')
  const card = page.getByRole('alertdialog')
  await expect(card).toBeVisible()
  await card.getByRole('button', { name: `Always allow ${PREFIX}` }).click()
  const confirm = card.getByRole('dialog', { name: 'Confirm always allow' })
  await expect(confirm).toContainText('~/.odex/rules/default.toml')
  await shoot(page, 'approval-always-allow')
  await confirm.getByRole('button', { name: 'Always allow', exact: true }).click()
  await expect(page.getByText('Ran it.')).toBeVisible()
  const rules = fs.readFileSync(path.join(home, 'rules', 'default.toml'), 'utf8')
  expect(rules).toContain(`prefix = ["${PREFIX}"]`)
  expect(rules).toContain('decision = "allow"')

  // a new thread: same prefix, no approval
  await page.keyboard.press('Control+N')
  // count approval requests that reach the window from now on
  await page.evaluate(() => {
    ;(window as any).__approvals = 0
    ;(window as any).odex.onServerRequest((r: any) => {
      if (r.method === 'approval/request') (window as any).__approvals++
    })
  })
  await send(page, 'please run again')
  await expect(page.getByText('Ran it again.')).toBeVisible()
  expect(await page.evaluate(() => (window as any).__approvals)).toBe(0)
  await expect(page.getByRole('alertdialog')).toHaveCount(0)
  const tid = await page.evaluate(() => (window as any).__odexStore.getState().selectedThreadId)
  const read = await rpc(page, 'thread/read', { threadId: tid })
  const cmd = read.turns.flatMap((t: any) => t.items).find((i: any) => i.type === 'commandExecution')
  expect(cmd.output).toContain('always-two')
})

test('MCP server instructions reach the model request', async () => {
  const { page } = L
  await rpc(page, 'mcp/upsert', { name: 'fixture', server: { command: process.execPath, args: [FIXTURE], env: {}, headers: {}, disabled_tools: [], auto_approve_tools: [] } })
  await expect.poll(async () => (await rpc(page, 'mcp/list')).servers.find((s: any) => s.name === 'fixture')?.state, { timeout: 30_000 }).toBe('ready')
  await page.keyboard.press('Control+N')
  await send(page, 'check the mcp instructions')
  await expect(page.getByText('MCP check done.')).toBeVisible()
  const reqs = await mock.requests()
  const turn = reqs.filter((r) => !r.body?.response_format && JSON.stringify(r.body?.messages ?? []).includes('check the mcp instructions')).pop()
  expect(turn).toBeTruthy()
  const system = JSON.stringify(turn.body.messages.filter((m: any) => m.role === 'system'))
  expect(system).toContain('# MCP server instructions')
  expect(system).toContain('## fixture')
  expect(system).toContain('FIXTURE-INSTRUCTIONS-42')
})

test('the built-in skill-creator skill is listed', async () => {
  const { page } = L
  const r = await rpc(page, 'skills/list', { cwd: null })
  const sk = r.skills.find((s: any) => s.name === 'skill-creator')
  expect(sk).toMatchObject({ scope: 'builtin', enabled: true })
  await page.getByRole('button', { name: 'Settings', exact: true }).click()
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('button', { name: 'Skills', exact: true }).click()
  await expect(page.getByText('Built-in', { exact: true })).toBeVisible()
  await expect(page.getByText('skill-creator').first()).toBeVisible()
  await page.getByRole('button', { name: 'Back to app' }).click()
})

test('automation templates fill the editor; archive all runs of an automation', async () => {
  const { page } = L
  await page.locator('.sidebar').getByRole('button', { name: /^Automations/ }).click()
  await page.getByRole('button', { name: 'New automation' }).first().click()
  const dlg = page.getByRole('dialog', { name: 'New automation' })
  await dlg.getByRole('button', { name: /Use template/ }).click()
  const menu = page.getByRole('menu')
  for (const name of ['Daily summary of commits', 'Dependency update check', 'Flaky test triage', 'TODO sweep', 'Changelog draft', 'Nightly test run'])
    await expect(menu.getByRole('menuitem', { name })).toBeVisible()
  await shoot(page, 'automation-templates')
  await menu.getByRole('menuitem', { name: 'TODO sweep' }).click()
  await expect(dlg.getByLabel('Name')).toHaveValue('TODO sweep')
  await expect(dlg.getByLabel('Prompt')).toHaveValue(/TODO, FIXME and HACK/)
  await expect(dlg.getByRole('radio', { name: 'Weekly' })).toHaveAttribute('aria-checked', 'true')
  await expect(dlg.getByLabel('Schedule preview')).toContainText(/Friday/)
  await expect(dlg.getByLabel('Permissions')).toHaveValue('read-only')
  await dlg.getByLabel('Project').selectOption({ label: 'project' })
  await dlg.getByRole('button', { name: 'Create' }).click()
  await expect(dlg).toBeHidden()
  const card = page.getByRole('article', { name: 'TODO sweep' })
  await expect(card).toBeVisible()

  // two runs, then archive them all
  const runs = async () =>
    page.evaluate(async () => (await (window as any).odex.request('automation/runs', { unreadOnly: false, includeArchived: true, limit: 50 })).runs.filter((r: any) => r.status !== 'running'))
  for (const n of [1, 2]) {
    await card.getByRole('button', { name: 'Run now' }).click()
    await expect.poll(async () => (await runs()).length, { timeout: 60_000 }).toBe(n)
  }
  const header = card.getByRole('button', { name: 'TODO sweep' })
  if ((await header.getAttribute('aria-expanded')) !== 'true') await header.click()
  await card.getByRole('button', { name: 'Archive all runs' }).click()
  await page.getByRole('dialog', { name: 'Archive all runs' }).getByRole('button', { name: 'Archive' }).click()
  await expect.poll(async () => (await runs()).filter((r: any) => !r.archived).length).toBe(0)
  await expect(card.locator('.run-row .badge', { hasText: 'archived' })).toHaveCount(2)
  await expect(card.getByRole('button', { name: 'Archive all runs' })).toHaveCount(0)
})

test('memories: suggest from a thread', async () => {
  const { page } = L
  await page.getByRole('button', { name: 'Settings', exact: true }).click()
  await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('button', { name: 'Memories', exact: true }).click()
  await page.getByRole('button', { name: /Suggest memories from a thread/ }).click()
  const dlg = page.getByRole('dialog', { name: 'Suggest memories from a thread' })
  await expect(dlg.getByLabel('Thread to learn from')).toBeVisible()
  await shoot(page, 'memories-suggest')
  await dlg.getByRole('button', { name: 'Suggest' }).click()
  await expect(dlg).toBeHidden()
  const list = page.getByRole('list', { name: 'Suggested memories' })
  await expect(list.getByRole('listitem', { name: 'Runs end-to-end tests with Playwright' })).toBeVisible()
  const reqs = await mock.requests()
  expect(reqs.some((r) => r.body?.response_format?.json_schema?.name === 'memories')).toBe(true)
})

test("the tray tooltip shows today's token usage", async () => {
  const { app } = L
  // refreshed shortly after each turn (non-zero: this app ran several turns today)
  await expect
    .poll(async () => app.evaluate(() => (globalThis as any).__odexTray?.tooltip as string | undefined), { timeout: 30_000 })
    .toMatch(/^Odex — \d+ running.* · (?!0 )[\d.]+[kM]? tokens today$/)
  const st = await app.evaluate(() => (globalThis as any).__odexTray)
  expect(st.usageLabel).toMatch(/tokens today$/)
})

test('settings: agent shell, follow-ups, colors and deep search', async () => {
  const { page, home } = L
  const config = () => fs.readFileSync(path.join(home, 'config.toml'), 'utf8')
  await page.getByRole('button', { name: 'Settings', exact: true }).click()
  const nav = page.getByRole('navigation', { name: 'Settings sections' })
  await nav.getByRole('button', { name: 'General', exact: true }).click()
  await page.getByLabel('Agent shell').selectOption(WIN ? 'pwsh' : 'zsh')
  await expect.poll(config).toMatch(new RegExp(`default_shell = "${WIN ? 'pwsh' : 'zsh'}"`))
  await page.getByLabel('Agent shell').selectOption('')
  await expect.poll(config).not.toMatch(/default_shell/)
  const follow = page.getByRole('switch', { name: 'Suggest follow-ups' })
  await expect(follow).toHaveAttribute('aria-checked', 'false')
  await follow.click()
  await expect.poll(config).toMatch(/follow_up_suggestions = true/)
  await follow.click()
  await expect.poll(config).toMatch(/follow_up_suggestions = false/)

  // background / text color overrides apply as CSS variables
  await page.getByLabel('Background color', { exact: true }).fill('#203040')
  await expect.poll(() => page.evaluate(() => document.documentElement.style.getPropertyValue('--bg'))).toBe('#203040')
  await page.getByLabel('Text color', { exact: true }).fill('#f0e0d0')
  await expect.poll(() => page.evaluate(() => getComputedStyle(document.body).color)).toBe('rgb(240, 224, 208)')
  await page.getByRole('button', { name: 'Reset background color' }).click()
  await page.getByRole('button', { name: 'Reset text color' }).click()
  await expect.poll(() => page.evaluate(() => document.documentElement.style.getPropertyValue('--bg'))).toBe('')
  expect(JSON.parse(fs.readFileSync(path.join(home, 'desktop.json'), 'utf8'))).toMatchObject({ bgColor: '', fgColor: '' })

  // deep search: a setting row in another panel
  await nav.getByRole('button', { name: 'Memories', exact: true }).click()
  const search = page.getByRole('textbox', { name: 'Search settings' })
  await search.fill('agent shell')
  const hits = page.getByRole('list', { name: 'Matching settings' })
  await expect(hits.getByRole('listitem').first()).toContainText('Agent shell')
  await expect(hits.getByRole('listitem').first()).toContainText('General')
  await shoot(page, 'settings-deep-search')
  await hits.getByRole('listitem').first().click()
  await expect(page.getByRole('heading', { level: 2, name: 'General' })).toBeVisible()
  await expect(page.locator('[data-setting-label="Agent shell"]')).toHaveClass(/setting-match/)
  await expect(page.locator('[data-setting-label="Agent shell"]')).toBeInViewport()
  // a hint word finds its panel too; Enter opens the best hit
  await search.fill('windows sandbox backend')
  await expect(hits.getByRole('listitem').first()).toContainText('Windows sandbox backend')
  await search.press('Enter')
  await expect(page.getByRole('heading', { level: 2, name: 'Permissions & sandbox' })).toBeVisible()
  await search.fill('')
  await expect(hits).toHaveCount(0)
  await page.getByRole('button', { name: 'Back to app' }).click()
})

test('Quick Chat opens a small window with a projectless quick chat', async () => {
  const { page, app } = L
  const winPromise = app.waitForEvent('window')
  await page.keyboard.press('Control+Alt+N')
  const qc = await winPromise
  await qc.waitForLoadState('domcontentloaded')
  expect(qc.url()).toContain('quickchat=1')
  await expect(qc.getByText('Quick chat', { exact: true })).toBeVisible()
  const sizes = await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().map((w) => ({ url: w.webContents.getURL(), size: w.getSize() })))
  expect(sizes.find((s) => s.url.includes('quickchat=1'))?.size).toEqual([520, 640])
  const box = qc.getByRole('textbox', { name: 'Message' })
  await expect(box).toBeVisible()
  await box.fill('quick hello')
  await box.press('Enter')
  await expect(qc.getByText('Hi from quick chat.')).toBeVisible()
  const tid = await qc.evaluate(() => (window as any).__odexStore.getState().selectedThreadId)
  const t = (await rpc(qc, 'thread/read', { threadId: tid })).thread
  expect(t.kind).toBe('quickChat')
  expect(t.projectId ?? null).toBeNull()
  // no sidebar or side panel in the quick window
  await expect(qc.locator('.sidebar')).toHaveCount(0)
  await expect(qc.getByRole('complementary', { name: 'Side panel' })).toHaveCount(0)
  // pin on top
  await qc.getByRole('button', { name: 'Keep on top' }).click()
  await expect
    .poll(() => app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().find((w) => w.webContents.getURL().includes('quickchat=1'))?.isAlwaysOnTop()))
    .toBe(true)
  if (SHOTS) {
    for (const theme of ['light', 'dark'] as const) {
      await qc.evaluate((th) => (window as any).odex.settings.set({ theme: th }), theme)
      await expect(qc.locator('html')).toHaveAttribute('data-theme', theme)
      await qc.waitForTimeout(250)
      await qc.screenshot({ path: path.join(SHOTS, `quick-chat-${theme}.png`) })
    }
    await qc.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
  }
  // the same shortcut focuses the existing window instead of opening another
  await page.keyboard.press('Control+Alt+N')
  await page.waitForTimeout(500)
  const count = await app.evaluate(({ BrowserWindow }) => BrowserWindow.getAllWindows().filter((w) => w.webContents.getURL().includes('quickchat=1')).length)
  expect(count).toBe(1)
  // the main window never picked up quick-chat UI state
  const ui = await page.evaluate(() => JSON.parse(localStorage.getItem('odex.ui') || '{}'))
  expect(ui.sidebarOpen).not.toBe(false)
  await qc.close()
})
