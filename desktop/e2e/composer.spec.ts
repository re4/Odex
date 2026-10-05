import { test, expect, type Page } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { addProject, desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

// Composer: attachments (drop / paste / Files-tree drag), the grouped @ menu (folders, skills,
// MCP resources and prompts), / mid-draft, /plan, worktree branch + environment chips, starter
// prompts from the utility model, and pop-out windows not leaking UI state.
// Set ODEX_SHOTS=<dir> to collect light/dark screenshots of the menus.

const FIXTURE = path.join(desktopDir, 'e2e', 'fixtures', 'mcp-server.mjs')
const PLAN = '## Plan\n\n1. Outline the refactor\n2. Report back\n'
const STARTERS = ['Explain the demo add function', 'Add tests for main.py', 'Document the project README', 'Review the utils helpers']

let mock: Mock
let L: Launched

const git = (cwd: string, ...args: string[]) => execFileSync('git', ['-c', 'user.email=e2e@odex.test', '-c', 'user.name=e2e', ...args], { cwd })

test.beforeAll(async () => {
  mock = await startMock([
    { when: { structured_name: 'starter_prompts' }, reply: { kind: 'text', text: JSON.stringify({ prompts: STARTERS }) } },
    { when: { last_role: 'user', system_contains: 'Planning mode' }, reply: { kind: 'text', text: PLAN } },
    { when: {}, reply: { kind: 'text', text: 'OK from the mock.' } },
  ])
  L = await launch({ mockUrl: mock.url })
  // a folder to mention and a branch to start a worktree from
  fs.mkdirSync(path.join(L.project, 'src', 'utils'), { recursive: true })
  fs.writeFileSync(path.join(L.project, 'src', 'utils', 'helpers.py'), 'def helper():\n    return 42\n')
  git(L.project, 'add', '.')
  git(L.project, 'commit', '-q', '-m', 'utils')
  git(L.project, 'checkout', '-q', '-b', 'feature')
  fs.writeFileSync(path.join(L.project, 'feature.txt'), 'only on the feature branch\n')
  git(L.project, 'add', '.')
  git(L.project, 'commit', '-q', '-m', 'feature')
  git(L.project, 'checkout', '-q', 'main')
  const { page } = L
  await engineReady(page)
  await addProject(page, L.project)
  await page.evaluate(
    async ({ node, fixture }) => {
      const w = window as any
      // starter prompts and follow-ups use the utility model (the harness turns them off)
      await w.odex.request('config/write', { edits: [{ keyPath: 'features.follow_up_suggestions', value: true }] })
      await w.odex.request('skills/write', { name: 'e2e-helper', description: 'Helps the composer e2e test', body: 'SKILL-BODY-MARKER: follow the e2e steps.', scope: 'user' })
      await w.odex.request('mcp/upsert', { name: 'fixture', server: { command: node, args: [fixture], env: {}, headers: {}, disabled_tools: [], auto_approve_tools: [] } })
    },
    { node: process.execPath, fixture: FIXTURE },
  )
  await expect
    .poll(async () => page.evaluate(async () => ((await (window as any).odex.request('mcp/list', {})).servers as any[]).find((s) => s.name === 'fixture')?.state), { timeout: 30_000 })
    .toBe('ready')
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

const box = (page: Page) => page.getByRole('textbox', { name: 'Message' })
const menu = (page: Page) => page.getByRole('listbox', { name: 'Suggestions' })

async function goHome(page: Page): Promise<void> {
  await page.evaluate(() => (window as any).__odexStore.getState().selectThread(null))
  await expect(page.getByRole('heading', { name: /What should we/ })).toBeVisible()
}

async function selectProject(page: Page): Promise<void> {
  const chip = page.locator('[data-chip="project"]')
  if ((await chip.textContent())?.trim() === 'project') return
  await chip.click()
  await page.getByRole('menuitem', { name: /^project/ }).click()
  await expect(chip).toHaveText('project')
}

async function setRunMode(page: Page, mode: 'Local' | 'Worktree'): Promise<void> {
  const chip = page.locator('[data-chip="runMode"]')
  if ((await chip.textContent())?.includes(mode)) return
  await chip.click()
  await page.getByRole('menuitem', { name: new RegExp(`^${mode}`) }).click()
  await expect(chip).toContainText(mode)
}

/** Screenshot in light and dark (only when ODEX_SHOTS is set). */
async function shoot(page: Page, name: string): Promise<void> {
  const dir = process.env.ODEX_SHOTS
  if (!dir) return
  fs.mkdirSync(dir, { recursive: true })
  for (const theme of ['light', 'dark'] as const) {
    await page.evaluate((t) => (window as any).odex.settings.set({ theme: t }), theme)
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
    await page.waitForTimeout(200)
    await page.screenshot({ path: path.join(dir, `${name}-${theme}.png`) })
  }
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
}

/** Screenshot an open suggestions menu, then re-open it (the theme switch can blur the composer). */
async function shootMenu(page: Page, name: string): Promise<void> {
  if (!process.env.ODEX_SHOTS) return
  await shoot(page, name)
  await box(page).focus()
  await box(page).press('End')
  await box(page).pressSequentially(' ')
  await box(page).press('Backspace')
  await expect(menu(page)).toBeVisible()
}

/** Every model request whose messages mention `text` (the turn itself, plus its title request). */
async function sentWith(text: string): Promise<string> {
  const reqs = await mock.requests()
  return reqs
    .map((r) => JSON.stringify(r.body?.messages ?? []))
    .filter((m) => m.includes(text))
    .join(' ')
}

test('starter prompts come from the utility model for the selected project', async () => {
  const { page } = L
  const starters = page.getByLabel('Starter prompts')
  // no project: the static list
  await expect(starters).toContainText('Explain the structure of this codebase')
  await selectProject(page)
  await expect(starters).toHaveAttribute('data-generated', 'true')
  for (const s of STARTERS) await expect(starters.getByRole('button', { name: s })).toBeVisible()
  // the utility request saw the README head and the git state
  const reqs = await mock.requests()
  const req = reqs.find((r) => JSON.stringify(r.body).includes('starter prompts'))
  expect(JSON.stringify(req?.body)).toContain('A tiny project for Odex tests')
  expect(JSON.stringify(req?.body)).toContain('Git branch: main')
  await shoot(page, 'home-starters')
  // clicking one fills the composer
  await starters.getByRole('button', { name: STARTERS[1] }).click()
  await expect(box(page)).toHaveValue(STARTERS[1])
  await box(page).fill('')
  // cached for the session: going home again doesn't ask the model again
  const before = (await mock.requests()).length
  await page.evaluate(() => (window as any).__odexStore.getState().setUi({ newThreadProjectId: null }))
  await expect(starters).not.toHaveAttribute('data-generated', 'true')
  await selectProject(page)
  await expect(starters).toHaveAttribute('data-generated', 'true')
  expect((await mock.requests()).length).toBe(before)
})

test('dropping and pasting real files attaches them by path; Files-tree drags attach a mention', async () => {
  const { page, project } = L
  await goHome(page)
  await selectProject(page)
  await setRunMode(page, 'Local')
  const notes = path.join(project, 'notes.txt')
  fs.writeFileSync(notes, 'DROP-MARKER-7731 notes for the agent\n')
  // a real on-disk File (webUtils.getPathForFile can't resolve synthetic `new File()` objects)
  await page.evaluate(() => {
    const input = document.createElement('input')
    input.type = 'file'
    input.id = 'e2e-file'
    input.style.display = 'none'
    document.body.appendChild(input)
  })
  await page.setInputFiles('#e2e-file', notes)
  await page.evaluate(() => {
    const file = (document.getElementById('e2e-file') as HTMLInputElement).files![0]
    const dt = new DataTransfer()
    dt.items.add(file)
    const target = document.querySelector('.composer')!
    target.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt }))
    target.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt }))
  })
  await expect(page.locator('.composer-attachments .attachment', { hasText: 'notes.txt' })).toHaveCount(1)
  // paste the same file
  await page.evaluate(() => {
    const file = (document.getElementById('e2e-file') as HTMLInputElement).files![0]
    const dt = new DataTransfer()
    dt.items.add(file)
    document.querySelector('.composer textarea')!.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: dt }))
  })
  await expect(page.locator('.composer-attachments .attachment', { hasText: 'notes.txt' })).toHaveCount(2)
  await page.getByRole('button', { name: 'Remove notes.txt' }).first().click()
  // a drag from the Files tree carries the absolute path as text/plain
  const dir = path.join(project, 'src', 'utils')
  await page.evaluate((p) => {
    const dt = new DataTransfer()
    dt.setData('text/plain', p)
    const target = document.querySelector('.composer')!
    target.dispatchEvent(new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: dt }))
    target.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: dt }))
  }, dir)
  await expect(page.locator('.composer-attachments .attachment', { hasText: 'utils' })).toBeVisible()
  await expect(box(page)).toHaveValue('')
  await box(page).fill('Read the attached notes')
  await box(page).press('Enter')
  await expect(page.getByText('OK from the mock.').first()).toBeVisible()
  const sent = await sentWith('Read the attached notes')
  expect(sent).toContain('DROP-MARKER-7731')
  expect(sent).toContain('helpers.py') // the folder mention lists its contents
})

test('@ menu groups files, folders, skills, MCP resources and MCP prompts', async () => {
  const { page } = L
  await goHome(page)
  await selectProject(page)
  // empty query: grouped headers
  await box(page).fill('@')
  await expect(menu(page)).toBeVisible()
  const groups = menu(page).locator('.mention-group')
  await expect(groups.filter({ hasText: 'Files' })).toHaveCount(1)
  await expect(groups.filter({ hasText: 'Skills' })).toHaveCount(1)
  await expect(groups.filter({ hasText: 'MCP resources' })).toHaveCount(1)
  await expect(groups.filter({ hasText: 'MCP prompts' })).toHaveCount(1)
  await shootMenu(page, 'composer-at-menu')

  // folder
  await box(page).fill('Look at @util')
  await expect(groups.filter({ hasText: 'Folders' })).toHaveCount(1)
  await menu(page).getByRole('option', { name: 'src/utils/', exact: true }).click()
  await expect(box(page)).toHaveValue('Look at @src/utils/ ')
  await expect(page.locator('.composer-attachments .attachment', { hasText: 'src/utils/' })).toBeVisible()

  // skill via @
  await box(page).pressSequentially('and @e2e')
  await expect(menu(page).getByRole('option', { name: /\$e2e-helper/ })).toBeVisible()
  await box(page).press('Enter')
  await expect(box(page)).toHaveValue('Look at @src/utils/ and $e2e-helper ')
  await expect(page.locator('.composer-attachments .attachment', { hasText: 'e2e-helper' })).toBeVisible()

  // MCP resource
  await box(page).pressSequentially('with @readme')
  const resource = menu(page).getByRole('option', { name: /fixture · fixture:\/\/readme/ })
  await expect(resource).toBeVisible()
  await resource.click()
  await expect(box(page)).toHaveValue('Look at @src/utils/ and $e2e-helper with @fixture:readme ')
  await expect(page.locator('.composer-attachments .attachment', { hasText: 'fixture: readme' })).toBeVisible()

  // MCP prompt: asks for its argument, inserts the prompt text
  await box(page).pressSequentially('@greet')
  await menu(page).getByRole('option', { name: /greet/ }).click()
  const dlg = page.getByRole('dialog', { name: /greet: who/ })
  await expect(dlg).toBeVisible()
  await dlg.getByRole('textbox').fill('Ada')
  await dlg.getByRole('textbox').press('Enter')
  await expect(box(page)).toHaveValue('Look at @src/utils/ and $e2e-helper with @fixture:readme Say hello to Ada ')

  await box(page).press('Enter')
  await expect(page.getByText('OK from the mock.').first()).toBeVisible()
  const sent = await sentWith('Look at @src/utils/')
  expect(sent).toContain('helpers.py') // folder listing
  expect(sent).toContain('SKILL-BODY-MARKER') // skill body
  expect(sent).toContain('This resource comes from the Odex e2e MCP fixture') // resource contents
  expect(sent).toContain('Say hello to Ada')
})

test('/ menu opens mid-draft, keeps the rest of the draft, and lists skills', async () => {
  const { page } = L
  await goHome(page)
  await selectProject(page)
  await setRunMode(page, 'Local')
  // inside a path: no menu
  await box(page).fill('look at src/ma')
  await expect(menu(page)).toBeHidden()
  // after whitespace mid-draft
  await box(page).fill('please refactor main.py /work')
  await expect(menu(page).getByRole('option', { name: /\/worktree/ })).toBeVisible()
  await shootMenu(page, 'composer-slash-menu')
  await box(page).press('Enter')
  await expect(box(page)).toHaveValue('please refactor main.py ')
  await expect(page.locator('[data-chip="runMode"]')).toContainText('Worktree')
  // at the start of a later line
  await box(page).press('Shift+Enter')
  await box(page).pressSequentially('/loc')
  await expect(menu(page).getByRole('option', { name: /\/local/ })).toBeVisible()
  await box(page).press('Enter')
  await expect(box(page)).toHaveValue('please refactor main.py \n')
  await expect(page.locator('[data-chip="runMode"]')).toContainText('Local')
  // skills are in the / menu; picking one inserts $name
  await box(page).fill('use /e2e')
  await expect(menu(page).locator('.mention-group', { hasText: 'Skills' })).toBeVisible()
  await menu(page).getByRole('option', { name: /\$e2e-helper/ }).click()
  await expect(box(page)).toHaveValue('use $e2e-helper ')
  await expect(page.locator('.composer-attachments .attachment', { hasText: 'e2e-helper' })).toBeVisible()
  await page.getByRole('button', { name: 'Remove e2e-helper' }).click()
  await box(page).fill('')
})

test('/plan with no task turns on plan mode for the next message', async () => {
  const { page } = L
  await goHome(page)
  await selectProject(page)
  await setRunMode(page, 'Local')
  const plan = page.getByRole('button', { name: 'Plan', exact: true })
  await expect(plan).toHaveAttribute('aria-pressed', 'false')
  await box(page).fill('/plan')
  await box(page).press('Escape') // close the suggestions, then send `/plan` as typed
  await box(page).press('Enter')
  await expect(plan).toHaveAttribute('aria-pressed', 'true')
  await expect(box(page)).toHaveValue('')
  // the odex:plan-mode event toggles it too (used by the palette's /plan)
  await page.evaluate(() => window.dispatchEvent(new CustomEvent('odex:plan-mode', { detail: { on: false } })))
  await expect(plan).toHaveAttribute('aria-pressed', 'false')
  await page.evaluate(() => window.dispatchEvent(new CustomEvent('odex:plan-mode')))
  await expect(plan).toHaveAttribute('aria-pressed', 'true')
  await box(page).fill('Outline a refactor of main.py')
  await box(page).press('Enter')
  await expect(page.getByText('Proposed plan')).toBeVisible()
  const reqs = await mock.requests()
  const planned = reqs.filter((r) => JSON.stringify(r.body?.messages ?? []).includes('Outline a refactor of main.py'))
  expect(planned.some((r) => JSON.stringify(r.body.messages[0]).includes('Planning mode'))).toBe(true)
})

test('branch and environment chips start a worktree thread on the chosen base', async () => {
  const { page, project } = L
  const pid = await page.evaluate(() => (window as any).__odexStore.getState().ui.newThreadProjectId as string)
  await page.evaluate(
    async (id) =>
      (window as any).odex.request('project/update', {
        id,
        environments: [
          { id: 'alpha', name: 'Alpha', setupScript: 'echo alpha > env-alpha.txt', env: {} },
          { id: 'beta', name: 'Beta', setupScript: 'echo beta > env-beta.txt', env: {} },
        ],
        defaultEnvironment: 'alpha',
      }),
    pid,
  )
  await goHome(page)
  await selectProject(page)
  await setRunMode(page, 'Worktree')
  const branch = page.locator('[data-chip="branch"]')
  await expect(branch).toContainText('main')
  const env = page.locator('[data-chip="environment"]')
  await expect(env).toContainText('Alpha')
  await branch.click()
  await expect(page.getByRole('menuitem', { name: /main with uncommitted changes/ })).toBeVisible()
  await shoot(page, 'composer-branch-menu')
  await page.getByRole('menuitem', { name: 'feature' }).click()
  await expect(branch).toContainText('feature')
  await env.click()
  await expect(page.getByRole('menuitem', { name: /No environment/ })).toBeVisible()
  await page.getByRole('menuitem', { name: /^Beta/ }).click()
  await expect(env).toContainText('Beta')
  await box(page).fill('Work on the feature branch')
  await box(page).press('Enter')
  await expect(page.getByText('OK from the mock.').first()).toBeVisible({ timeout: 60_000 })
  const started = await page.evaluate(() => {
    const s = (window as any).__odexStore.getState()
    return s.threads[s.selectedThreadId]?.thread
  })
  const wt = started?.worktree
  expect(started?.environmentId).toBe('beta')
  expect(wt?.baseBranch).toBe('feature')
  expect(fs.existsSync(path.join(wt.path, 'feature.txt'))).toBe(true)
  // the chosen environment's setup script ran in the new worktree
  await expect.poll(() => fs.existsSync(path.join(wt.path, 'env-beta.txt')), { timeout: 30_000 }).toBe(true)
  expect(fs.existsSync(path.join(wt.path, 'env-alpha.txt'))).toBe(false)
  expect(fs.existsSync(path.join(project, 'feature.txt'))).toBe(false)
  await goHome(page)
  await setRunMode(page, 'Local')
})

test('pop-out windows never write the shared UI state', async () => {
  const { app, page } = L
  await goHome(page)
  const read = () => page.evaluate(() => JSON.parse(localStorage.getItem('odex.ui') || '{}'))
  await page.evaluate(() => (window as any).__odexStore.getState().setUi({ sidebarOpen: true, sidebarWidth: 280 }))
  expect(await read()).toMatchObject({ sidebarOpen: true, sidebarWidth: 280 })
  const [pop] = await Promise.all([app.waitForEvent('window'), page.evaluate(() => (window as any).odex.win.newWindow())])
  await pop.waitForFunction(() => !!(window as any).__odexStore)
  await expect.poll(() => pop.evaluate(() => (window as any).__odexStore.getState().ui.popout)).toBe(true)
  // the pop-out changes its own layout; nothing reaches localStorage
  await pop.evaluate(() => (window as any).__odexStore.getState().setUi({ sidebarWidth: 333, bottomOpen: true }))
  const saved = await read()
  expect(saved.popout).toBeUndefined()
  expect(saved.sidebarOpen).toBe(true)
  expect(saved.sidebarWidth).toBe(280)
  expect(saved.bottomOpen).not.toBe(true)
  await pop.close()
  // state leaked by older builds is repaired on load
  await page.evaluate(() => localStorage.setItem('odex.ui', JSON.stringify({ popout: true, sidebarOpen: false, sidebarWidth: 280 })))
  await page.reload()
  await page.waitForFunction(() => !!(window as any).__odexStore)
  const ui = await page.evaluate(() => (window as any).__odexStore.getState().ui)
  expect(ui.popout).toBe(false)
  expect(ui.sidebarOpen).toBe(true)
  await expect(page.getByRole('button', { name: 'Toggle sidebar' })).toBeVisible()
})
