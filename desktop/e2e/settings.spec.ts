import { test, expect, type Page } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'
import { desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

// Settings panels: Context, Personalization, Permissions, Memories, Keyboard
// shortcuts, Archived threads, Config & profiles, plus light/dark screenshots
// of every panel (ODEX_SHOTS_DIR, default test-results/<out>/settings-shots).

const SHOTS = process.env.ODEX_SHOTS_DIR || path.join(desktopDir, 'test-results', process.env.ODEX_OUT || 'out', 'settings-shots')

let mock: Mock
let L: Launched

const rpc = (page: Page, method: string, params: unknown = {}) => page.evaluate(([m, p]) => (window as any).odex.request(m, p), [method, params] as const) as Promise<any>

async function openPanel(page: Page, label: string): Promise<void> {
  const nav = page.getByRole('navigation', { name: 'Settings sections' })
  // Ctrl+, opens settings (retry: the first key press can land before the window has focus)
  await expect(async () => {
    if (!(await nav.isVisible())) {
      await page.locator('body').focus().catch(() => {})
      await page.keyboard.press('Control+,')
    }
    await expect(nav).toBeVisible({ timeout: 2000 })
  }).toPass({ timeout: 20_000 })
  await nav.getByRole('button', { name: label, exact: true }).click()
  await expect(page.getByRole('heading', { name: label, exact: true, level: 2 })).toBeVisible()
}

const configText = () => fs.readFileSync(path.join(L.home, 'config.toml'), 'utf8')

test.beforeAll(async () => {
  mock = await startMock([
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Usage sample"}' } },
    { when: {}, reply: { kind: 'text', text: 'Sure, noted.' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  fs.mkdirSync(SHOTS, { recursive: true })
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('context: changing a threshold writes config.toml', async () => {
  const { page } = L
  await openPanel(page, 'Context')
  await expect(page.getByLabel('Context window budget')).toBeVisible()
  const prune = page.getByRole('spinbutton', { name: 'Prune at', exact: true })
  await expect(prune).toHaveValue('70')
  await prune.fill('65')
  await prune.press('Enter')
  await expect.poll(configText).toMatch(/\[context\][\s\S]*prune_at\s*=\s*0\.65/)
  await expect(prune).toHaveValue('65')
  // an integer field
  const images = page.getByRole('spinbutton', { name: 'Images kept', exact: true })
  await images.fill('4')
  await images.blur()
  await expect.poll(configText).toMatch(/max_images\s*=\s*4/)
  // reset one field
  await page.getByRole('button', { name: 'Reset Images kept' }).click()
  await expect.poll(configText).not.toMatch(/max_images/)
  await expect(images).toHaveValue('2')
  // the engine sees the new value
  const cfg = await rpc(page, 'config/read')
  expect(cfg.effective.context.prune_at).toBe(0.65)
})

test('personalization: custom instructions and the global AGENTS.md are saved', async () => {
  const { page } = L
  await openPanel(page, 'Personalization')
  await page.getByRole('textbox', { name: 'Custom instructions' }).fill('Explain trade-offs briefly.')
  await expect.poll(configText).toContain('custom_instructions = "Explain trade-offs briefly."')
  const agents = page.getByRole('textbox', { name: 'Global AGENTS.md' })
  await agents.fill('# Global rules\n\n- Run the tests before finishing.\n')
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect.poll(() => fs.existsSync(path.join(L.home, 'AGENTS.md')) && fs.readFileSync(path.join(L.home, 'AGENTS.md'), 'utf8')).toBe('# Global rules\n\n- Run the tests before finishing.\n')
})

test('permissions: mode, sandbox options and a rules file', async () => {
  const { page } = L
  await openPanel(page, 'Permissions & sandbox')
  await expect(page.getByLabel('Sandbox status', { exact: true })).toContainText('Sandbox backend')
  await page.getByRole('radio', { name: /Read only/ }).click()
  await expect.poll(configText).toMatch(/^permission_mode = "read-only"/m)
  await expect(page.getByRole('radio', { name: /Read only/ })).toHaveAttribute('aria-checked', 'true')
  await page.getByRole('switch', { name: 'Allow network access' }).click()
  await expect.poll(configText).toMatch(/\[sandbox\][\s\S]*network_access = true/)
  const root = path.join(L.project, 'cache')
  await page.getByRole('textbox', { name: 'New writable root' }).fill(root)
  await page.getByRole('button', { name: 'Add', exact: true }).click()
  await expect(page.getByRole('list', { name: 'Writable roots' }).getByText(root)).toBeVisible()
  expect((await rpc(page, 'config/read')).user.sandbox.writable_roots).toEqual([root])
  await page.getByRole('button', { name: `Remove ${root}` }).click()
  await expect.poll(async () => (await rpc(page, 'config/read')).user.sandbox?.writable_roots ?? []).toEqual([])
  await page.getByRole('radio', { name: /^Auto/ }).click()
  await expect.poll(configText).toMatch(/^permission_mode = "auto"/m)

  // a rules file
  await page.getByRole('button', { name: 'New file' }).click()
  const dlg = page.getByRole('dialog', { name: 'New rules file name' })
  await dlg.getByRole('textbox').fill('team')
  await dlg.getByRole('button', { name: 'OK' }).click()
  await expect(page.getByRole('combobox', { name: 'Rules file' })).toHaveValue('team.toml')
  const rules = '[[rule]]\nprefix = ["git", "push"]\ndecision = "prompt"\n'
  await page.getByRole('textbox', { name: 'Rules file contents' }).fill(rules)
  await page.getByRole('region', { name: 'Command rules' }).getByRole('button', { name: 'Save', exact: true }).click()
  await expect.poll(() => fs.readFileSync(path.join(L.home, 'rules', 'team.toml'), 'utf8')).toBe(rules)
})

test('memories: add, approve a suggestion, filter and delete', async () => {
  const { page } = L
  // a suggestion as the utility model would propose it
  await rpc(page, 'memory/upsert', { memory: { id: '', text: 'The project uses pytest for tests', scope: 'global', projectPath: null, status: 'proposed', category: 'stack', sourceThreadId: null, createdAt: 0, updatedAt: 0 } })
  await openPanel(page, 'Memories')

  await page.getByRole('switch', { name: 'Use memories' }).click()
  await expect.poll(configText).toMatch(/\[memories\][\s\S]*enabled\s*=\s*true/)

  await page.getByRole('textbox', { name: 'New memory' }).fill('Prefer tabs over spaces in Makefiles')
  await page.getByRole('combobox', { name: 'New memory category' }).selectOption('convention')
  await page.getByRole('button', { name: 'Add memory' }).click()
  const saved = page.getByRole('list', { name: 'Saved memories' })
  await expect(saved.getByText('Prefer tabs over spaces in Makefiles')).toBeVisible()
  expect((await rpc(page, 'memory/list')).memories.some((m: any) => m.text === 'Prefer tabs over spaces in Makefiles' && m.status === 'approved' && m.category === 'convention')).toBe(true)

  const suggestions = page.getByRole('list', { name: 'Suggested memories' })
  await expect(suggestions.getByText('The project uses pytest for tests')).toBeVisible()
  await suggestions.getByRole('button', { name: /^Approve memory/ }).click()
  await expect(saved.getByText('The project uses pytest for tests')).toBeVisible()
  await expect(suggestions).toHaveCount(0)

  // filter by category
  await page.getByRole('combobox', { name: 'Filter by category' }).selectOption('stack')
  await expect(saved.getByText('The project uses pytest for tests')).toBeVisible()
  await expect(saved.getByText('Prefer tabs over spaces in Makefiles')).toHaveCount(0)
  await page.getByRole('combobox', { name: 'Filter by category' }).selectOption('all')

  // edit
  await saved.getByRole('button', { name: 'Edit memory: The project uses pytest for tests' }).click()
  await saved.getByRole('textbox', { name: 'Memory text' }).fill('The project uses pytest and ruff')
  await saved.getByRole('button', { name: 'Save' }).click()
  await expect(saved.getByText('The project uses pytest and ruff')).toBeVisible()

  // delete
  await saved.getByRole('button', { name: 'Delete memory: The project uses pytest and ruff' }).click()
  await page.getByRole('dialog', { name: 'Delete memory' }).getByRole('button', { name: 'Delete' }).click()
  await expect(saved.getByText('The project uses pytest and ruff')).toHaveCount(0)
  expect((await rpc(page, 'memory/list')).memories.some((m: any) => m.text.includes('pytest'))).toBe(false)
})

test('keyboard shortcuts: a rebound combo opens the palette', async () => {
  const { page } = L
  await openPanel(page, 'Keyboard shortcuts')
  const bind = page.getByRole('button', { name: 'Change shortcut for Command palette', exact: true })
  await bind.click()
  await expect(bind).toHaveText('Press keys…')
  // Esc cancels without changing anything
  await page.keyboard.press('Escape')
  await expect(bind).toContainText('K')
  await bind.click()
  await page.keyboard.press('Control+Shift+Y')
  await expect(bind).toContainText('Y')
  await expect.poll(async () => (await page.evaluate(() => (window as any).odex.settings.get())).shortcuts.palette).toBe('Mod+Shift+Y')

  // conflict warning when two commands share a combo
  const fileSearch = page.getByRole('button', { name: 'Change shortcut for Search files', exact: true })
  await fileSearch.click()
  await page.keyboard.press('Control+Shift+Y')
  await expect(page.getByRole('listitem', { name: 'Search files' }).getByText('Also used by Command palette')).toBeVisible()
  await page.getByRole('button', { name: 'Reset shortcut for Search files' }).click()
  await expect(page.getByText('Also used by Command palette')).toHaveCount(0)

  // search by pressing keys
  await page.getByRole('button', { name: 'Search by keys', pressed: false }).click()
  await page.keyboard.press('Control+Shift+Y')
  await expect(page.getByRole('listitem', { name: 'Command palette', exact: true })).toBeVisible()
  await expect(page.getByRole('listitem', { name: 'New thread', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Search by keys', pressed: true }).click()

  // the new combo opens the palette, the old one no longer does
  await page.getByRole('button', { name: 'Back to app' }).click()
  const palette = page.getByRole('dialog', { name: 'Command palette' })
  await page.keyboard.press('Control+K')
  await page.waitForTimeout(300)
  await expect(palette).toHaveCount(0)
  await page.keyboard.press('Control+Shift+Y')
  await expect(palette).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(palette).toHaveCount(0)

  // reset all
  await openPanel(page, 'Keyboard shortcuts')
  await page.getByRole('button', { name: 'Reset all' }).click()
  await page.getByRole('dialog', { name: 'Reset all shortcuts' }).getByRole('button', { name: 'Reset all' }).click()
  await expect(bind).toContainText('K')
  await expect.poll(async () => Object.keys((await page.evaluate(() => (window as any).odex.settings.get())).shortcuts).length).toBe(0)
})

test('archived threads: archive then restore', async () => {
  const { page } = L
  const started = await rpc(page, 'thread/start', { cwd: L.project, name: 'Archive me please' })
  const id = started.thread.id as string
  await rpc(page, 'thread/archive', { threadId: id, removeWorktree: false })
  expect((await rpc(page, 'thread/list', { archived: true })).threads.some((t: any) => t.id === id)).toBe(true)

  await openPanel(page, 'Archived threads')
  const list = page.getByRole('list', { name: 'Archived threads' })
  await expect(list.getByRole('listitem', { name: 'Archive me please' })).toBeVisible()
  await page.getByRole('textbox', { name: 'Search archived threads' }).fill('nothing like this')
  await expect(list.getByRole('listitem', { name: 'Archive me please' })).toHaveCount(0)
  await page.getByRole('textbox', { name: 'Search archived threads' }).fill('archive me')
  await list.getByRole('button', { name: 'Unarchive Archive me please' }).click()
  await expect(list.getByRole('listitem', { name: 'Archive me please' })).toHaveCount(0)
  await expect.poll(async () => (await rpc(page, 'thread/list', {})).threads.some((t: any) => t.id === id && !t.archived)).toBe(true)

  // archive again and delete permanently
  await rpc(page, 'thread/archive', { threadId: id, removeWorktree: false })
  await page.getByRole('button', { name: 'Refresh archived threads' }).click()
  await list.getByRole('button', { name: 'Delete Archive me please permanently' }).click()
  await page.getByRole('dialog', { name: 'Delete thread permanently' }).getByRole('button', { name: 'Delete' }).click()
  await expect(list.getByRole('listitem', { name: 'Archive me please' })).toHaveCount(0)
  expect((await rpc(page, 'thread/list', { archived: true })).threads.some((t: any) => t.id === id)).toBe(false)
})

test('config: raw editor validates, profiles switch', async () => {
  const { page } = L
  await openPanel(page, 'Config & profiles')
  const editor = page.getByRole('textbox', { name: 'config.toml contents' })
  await expect(editor).toHaveValue(/model_providers\.mock/)
  const before = configText()

  // invalid TOML is rejected and the file is kept
  await editor.fill(`${before}\nthis is = = not toml\n`)
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('Not saved')
  expect(configText()).toBe(before)

  // valid edit with two profiles
  await editor.fill(`${before}\n[profiles.fast]\nreasoning_effort = "low"\n\n[profiles.careful]\npermission_mode = "read-only"\n`)
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('Saved')
  expect(fs.readFileSync(path.join(L.home, 'config.toml.bak'), 'utf8')).toBe(before)
  expect(configText()).toContain('[profiles.careful]')

  const select = page.getByRole('combobox', { name: 'Active profile' })
  await select.selectOption('fast')
  await expect.poll(async () => (await rpc(page, 'config/read')).activeProfile).toBe('fast')
  await select.selectOption('careful')
  await expect.poll(async () => (await rpc(page, 'config/read')).activeProfile).toBe('careful')
  expect((await rpc(page, 'config/read')).effective.permission_mode).toBe('read-only')
  await select.selectOption('')
  await expect.poll(async () => (await rpc(page, 'config/read')).activeProfile ?? null).toBe(null)
  expect(configText()).not.toMatch(/^profile\s*=/m)
})

test('screenshots of the settings panels (light and dark)', async () => {
  const { page } = L
  // some usage to chart, a memory and an archived thread to show
  const t = await rpc(page, 'thread/start', { cwd: L.project, name: 'Usage sample' })
  await rpc(page, 'turn/start', { threadId: t.thread.id, input: [{ type: 'text', text: 'Remember that I like small diffs' }] })
  await expect.poll(async () => (await rpc(page, 'usage/stats', {})).rows.length, { timeout: 30_000 }).toBeGreaterThan(0)
  await rpc(page, 'memory/upsert', { memory: { id: '', text: 'Keep diffs small and focused', scope: 'global', projectPath: null, status: 'approved', category: 'preference', sourceThreadId: null, createdAt: 0, updatedAt: 0 } })
  await rpc(page, 'memory/upsert', { memory: { id: '', text: 'Run the linter before committing', scope: 'global', projectPath: null, status: 'proposed', category: 'convention', sourceThreadId: null, createdAt: 0, updatedAt: 0 } })
  const a = await rpc(page, 'thread/start', { cwd: L.project, name: 'Old experiment' })
  await rpc(page, 'thread/archive', { threadId: a.thread.id, removeWorktree: false })

  await page.setViewportSize({ width: 1280, height: 1500 }).catch(() => {})
  const panels = ['Personalization', 'Permissions & sandbox', 'Context', 'Memories', 'Usage', 'Keyboard shortcuts', 'Archived threads', 'Config & profiles', 'About & data']
  for (const theme of ['light', 'dark'] as const) {
    await page.evaluate((th) => (window as any).odex.settings.set({ theme: th }), theme)
    await expect(page.locator(`html[data-theme="${theme}"]`)).toHaveCount(1)
    for (const p of panels) {
      await openPanel(page, p)
      await page.waitForTimeout(350)
      const slug = p.toLowerCase().replace(/[^a-z]+/g, '-').replace(/-+$/, '')
      await page.screenshot({ path: path.join(SHOTS, `${slug}-${theme}.png`) })
    }
  }
  // the usage chart has a hover tooltip
  await openPanel(page, 'Usage')
  const bars = page.locator('.sx-viz rect.hit')
  await bars.last().hover()
  await expect(page.getByRole('tooltip')).toContainText('Requests')
  await page.screenshot({ path: path.join(SHOTS, 'usage-hover-dark.png') })
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
  await page.setViewportSize({ width: 1280, height: 820 }).catch(() => {})
})
