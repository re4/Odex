import { test, expect, type Page } from '@playwright/test'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { addProject, engineReady, launch, startMock, type Launched, type Mock } from './harness'

// Screenshots for visual checks (light + dark) land here.
const SHOTS = process.env.ODEX_SHOTS || path.join(os.tmpdir(), 'odex-shots', 'files')
const PNG_1X1 = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=='

let mock: Mock
let L: Launched

const tree = (page: Page) => page.getByRole('tree', { name: 'Files' })
const item = (page: Page, name: string) => tree(page).getByRole('treeitem', { name, exact: true })
const editor = (page: Page) => page.locator('.files-editor .cm-content')
const tab = (page: Page, name: string) => page.getByRole('tablist', { name: 'Open files' }).getByRole('tab', { name, exact: true })

test.beforeAll(async () => {
  mock = await startMock([
    { when: { last_role: 'user', last_user_contains: 'where' }, reply: { kind: 'text', text: 'The function lives in `main.py:2`.' } },
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Where is add"}' } },
    { when: {}, reply: { kind: 'text', text: 'ok' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  const p = (...s: string[]) => path.join(L.project, ...s)
  fs.mkdirSync(p('src', 'lib'), { recursive: true })
  fs.writeFileSync(p('src', 'lib', 'util.py'), 'def mul(a, b):\n    return a * b\n')
  fs.mkdirSync(p('docs', 'guide'), { recursive: true })
  fs.writeFileSync(p('docs', 'guide', 'intro.md'), '# Intro\n\nHello.\n')
  fs.mkdirSync(p('node_modules', 'left-pad'), { recursive: true })
  fs.writeFileSync(p('node_modules', 'left-pad', 'index.js'), 'module.exports = 1\n')
  fs.writeFileSync(p('README.md'), '# Demo project\n\nEdited outside git.\n')
  fs.writeFileSync(p('notes.txt'), 'todo\n')
  fs.writeFileSync(p('pixel.png'), Buffer.from(PNG_1X1, 'base64'))
  fs.mkdirSync(SHOTS, { recursive: true })
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('files tab shows the project tree with git status and hidden folders', async () => {
  const { page, project } = L
  await addProject(page, project)
  await page.getByRole('button', { name: 'No project', exact: true }).click()
  await page.getByRole('menuitem', { name: /^project/ }).click()
  await page.getByRole('button', { name: 'Toggle side panel' }).click()
  await page.getByRole('tab', { name: 'Files' }).click()

  await expect(item(page, 'main.py')).toBeVisible()
  await expect(item(page, 'README.md')).toHaveAttribute('data-git', 'modified')
  await expect(item(page, 'notes.txt')).toHaveAttribute('data-git', 'untracked')
  await expect(item(page, 'src')).toHaveAttribute('data-git', 'untracked')
  // dirs first
  const names = await tree(page).getByRole('treeitem').evaluateAll((els) => els.map((e) => e.getAttribute('aria-label')))
  expect(names.indexOf('src')).toBeLessThan(names.indexOf('main.py'))
  // ignored folders are hidden until toggled
  await expect(item(page, 'node_modules')).toHaveCount(0)
  await page.getByRole('button', { name: 'Show ignored files' }).click()
  await expect(item(page, 'node_modules')).toBeVisible()
  await expect(item(page, '.git')).toBeVisible()
  await page.getByRole('button', { name: 'Show ignored files' }).click()
  await expect(item(page, 'node_modules')).toHaveCount(0)
  // lazy expansion
  await item(page, 'src').click()
  await expect(item(page, 'src')).toHaveAttribute('aria-expanded', 'true')
  await item(page, 'lib').click()
  await expect(item(page, 'util.py')).toBeVisible()
  await expect(item(page, 'util.py')).toHaveAttribute('aria-level', '3')
})

test('open main.py, edit it and save with Ctrl+S', async () => {
  const { page, project } = L
  await item(page, 'main.py').click()
  await expect(tab(page, 'main.py')).toHaveAttribute('aria-selected', 'true')
  await expect(editor(page)).toContainText('def add(a, b):')
  await editor(page).click()
  await page.keyboard.press('Control+End')
  await page.keyboard.type('# edited in odex\n')
  await expect(tab(page, 'main.py')).toHaveAttribute('data-dirty', 'true')
  // Ctrl+F opens the editor's own search, not the thread find bar
  await page.keyboard.press('Control+F')
  await expect(page.locator('.files-editor .cm-search')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.locator('.files-editor .cm-search')).toHaveCount(0)
  await editor(page).click()
  await page.keyboard.press('Control+S')
  await expect.poll(() => fs.readFileSync(path.join(project, 'main.py'), 'utf8')).toContain('# edited in odex')
  await expect(tab(page, 'main.py')).not.toHaveAttribute('data-dirty', 'true')
  expect(fs.readFileSync(path.join(project, 'main.py'), 'utf8')).toContain('def add(a, b):\n    return a + b\n')
  // saving a tracked file marks it modified
  await expect(item(page, 'main.py')).toHaveAttribute('data-git', 'modified')
  await expect(tab(page, 'main.py')).toHaveAttribute('data-git', 'modified')
})

test('a clean file reloads when it changes on disk', async () => {
  const { page, project } = L
  const f = path.join(project, 'main.py')
  fs.writeFileSync(f, `${fs.readFileSync(f, 'utf8')}# changed outside\n`)
  await expect(editor(page)).toContainText('# changed outside')
  await expect(tab(page, 'main.py')).not.toHaveAttribute('data-dirty', 'true')
  // the tree follows files created and deleted in expanded folders
  const created = path.join(project, 'src', 'lib', 'new_mod.py')
  fs.writeFileSync(created, 'x = 1\n')
  await expect(item(page, 'new_mod.py')).toBeVisible()
  await expect(item(page, 'new_mod.py')).toHaveAttribute('data-git', 'untracked')
  fs.rmSync(created)
  await expect(item(page, 'new_mod.py')).toHaveCount(0)
})

test('filter loaded entries and search all files', async () => {
  const { page } = L
  const filter = page.getByRole('textbox', { name: 'Filter files' })
  await filter.fill('util')
  await expect(page.getByRole('option', { name: 'src/lib/util.py' })).toBeVisible()
  // docs/ was never expanded: only "search all files" finds it
  await filter.fill('intro')
  await expect(page.getByText('No loaded files match.')).toBeVisible()
  await page.getByRole('listbox', { name: 'Matching files' }).getByRole('button', { name: 'Search all files' }).click()
  await page.getByRole('option', { name: 'docs/guide/intro.md' }).click()
  await expect(tab(page, 'intro.md')).toHaveAttribute('aria-selected', 'true')
  await expect(editor(page)).toContainText('# Intro')
  await expect(page.getByRole('button', { name: 'All files', exact: true })).toHaveAttribute('aria-pressed', 'true')
  await page.getByRole('button', { name: 'All files', exact: true }).click()
  await filter.fill('')
  await expect(item(page, 'main.py')).toBeVisible()
})

test('images open as a preview', async () => {
  const { page } = L
  await item(page, 'pixel.png').click()
  await expect(page.locator('.files-image img[alt="pixel.png"]')).toBeVisible()
  await expect(page.locator('.files-image-meta')).toContainText('1 × 1')
})

test('Ctrl+P file search opens the file in the editor', async () => {
  const { page } = L
  await tab(page, 'intro.md').click()
  await expect(editor(page)).toContainText('# Intro')
  await item(page, 'README.md').click()
  await page.keyboard.press('Control+P')
  const palette = page.getByRole('dialog', { name: 'Command palette' })
  await expect(palette).toBeVisible()
  await palette.getByRole('textbox').fill('main')
  await expect(palette.getByText('main.py').first()).toBeVisible()
  await page.keyboard.press('Enter')
  await expect(palette).toHaveCount(0)
  await expect(tab(page, 'main.py')).toHaveAttribute('aria-selected', 'true')
  await expect(editor(page)).toContainText('def add(a, b):')
})

test('unsaved edits survive a change on disk and saving asks before overwriting', async () => {
  const { page, project } = L
  const f = path.join(project, 'notes.txt')
  await item(page, 'notes.txt').click()
  await expect(editor(page)).toContainText('todo')
  await editor(page).click()
  await page.keyboard.press('Control+End')
  await page.keyboard.type('mine\n')
  await expect(tab(page, 'notes.txt')).toHaveAttribute('data-dirty', 'true')
  fs.writeFileSync(f, 'theirs\n')
  await expect(page.getByText('This file changed on disk.')).toBeVisible()
  await expect(editor(page)).toContainText('mine')
  await editor(page).click()
  await page.keyboard.press('Control+S')
  const dlg = page.getByRole('dialog', { name: 'File changed on disk' })
  await expect(dlg).toBeVisible()
  await dlg.getByRole('button', { name: 'Overwrite' }).click()
  await expect.poll(() => fs.readFileSync(f, 'utf8')).toBe('todo\nmine\n')
  await expect(page.getByText('This file changed on disk.')).toHaveCount(0)
  await expect(tab(page, 'notes.txt')).not.toHaveAttribute('data-dirty', 'true')
})

test('markdown preview, keyboard navigation and the context menu', async () => {
  const { page, app } = L
  await item(page, 'README.md').click()
  await page.getByRole('button', { name: 'Show preview' }).click()
  await expect(page.locator('.files-md-preview h1')).toHaveText('Demo project')
  await page.getByRole('button', { name: 'Show source' }).click()
  await expect(editor(page)).toContainText('# Demo project')

  // keyboard: Up + Enter opens the previous row; Left/Right collapse and expand
  await item(page, 'README.md').click()
  await page.keyboard.press('ArrowUp')
  await page.keyboard.press('Enter')
  await expect(tab(page, 'pixel.png')).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('Home')
  await page.keyboard.press('ArrowDown')
  await expect(item(page, 'src')).toHaveAttribute('aria-selected', 'true')
  await page.keyboard.press('ArrowLeft')
  await expect(item(page, 'util.py')).toHaveCount(0)
  await page.keyboard.press('ArrowRight')
  await expect(item(page, 'lib')).toBeVisible()

  // context menu: copy relative path, mention in composer
  await item(page, 'lib').click()
  await item(page, 'util.py').click({ button: 'right' })
  await page.getByRole('menuitem', { name: 'Copy relative path' }).click()
  await expect(page.getByText('Relative path copied')).toBeVisible()
  // the clipboard only takes writes while the window has focus (not guaranteed under test)
  const clip = await app.evaluate(({ clipboard }) => clipboard.readText())
  if (clip) expect(clip).toBe('src/lib/util.py')
  await item(page, 'main.py').click({ button: 'right' })
  await page.getByRole('menuitem', { name: 'Mention in composer' }).click()
  await expect(page.getByRole('button', { name: 'Remove main.py' })).toBeVisible()
  await page.getByRole('button', { name: 'Remove main.py' }).click()
})

test('a file link in chat opens at its line', async () => {
  const { page } = L
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('where is add?')
  await box.press('Enter')
  const link = page.locator('a[data-file="main.py"]')
  await expect(link).toBeVisible()
  await item(page, 'README.md').click()
  await expect(editor(page)).toContainText('# Demo project')
  await link.click()
  await expect(tab(page, 'main.py')).toHaveAttribute('aria-selected', 'true')
  await expect(page.locator('.files-editor .cm-flash-line')).toContainText('return a + b')
  // the thread's working dir is the tree root now
  await expect(item(page, 'main.py')).toBeVisible()
})

test('screenshots (light and dark)', async () => {
  const { page } = L
  await item(page, 'README.md').click()
  await tab(page, 'main.py').click()
  await item(page, 'src').hover()
  await page.waitForTimeout(300)
  await page.screenshot({ path: path.join(SHOTS, 'files-light.png') })
  await page.locator('.side-panel').screenshot({ path: path.join(SHOTS, 'files-panel-light.png') })
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'dark' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  await page.waitForTimeout(400)
  await page.screenshot({ path: path.join(SHOTS, 'files-dark.png') })
  await page.locator('.side-panel').screenshot({ path: path.join(SHOTS, 'files-panel-dark.png') })
  await item(page, 'pixel.png').click()
  await page.locator('.side-panel').screenshot({ path: path.join(SHOTS, 'files-image-dark.png') })
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
})
