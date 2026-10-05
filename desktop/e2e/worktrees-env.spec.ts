import { test, expect, type Page } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { addProject, desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

/**
 * Worktrees and environments: environment selection and variables (setup script, agent shell,
 * `!cmd`), per-OS setup scripts surviving an in-app save, Settings → Local environments, moving a
 * local thread into a worktree, retention, per-OS action commands, editor command placeholders and
 * per-project override, dev-server URL auto-open (action terminal and agent process), the
 * terminal's Background list with Kill, and Ctrl+L.
 */

const WIN = process.platform === 'win32'
const shots = process.env.ODEX_SHOTS_DIR || path.join(desktopDir, 'test-results', process.env.ODEX_OUT || 'out', 'g2b-shots')

let mock: Mock
let L: Launched
let projectId: string
let scratch: string
let serverJs: string
const ids: Record<string, string> = {}

const setupScripts = {
  windows: "Set-Content -Path setup.txt -Value ($env:GREETING + '|' + $env:ODEX_ENVIRONMENT)",
  linux: `printf '%s|%s' "$GREETING" "$ODEX_ENVIRONMENT" > setup.txt`,
  macos: `printf '%s|%s' "$GREETING" "$ODEX_ENVIRONMENT" > setup.txt`,
}
const perOs = `{ windows = '''${setupScripts.windows}''', linux = '''${setupScripts.linux}''', macos = '''${setupScripts.macos}''' }`
const ENVS = `[[environment]]
id = "alpha"
name = "Alpha"
setup_script = "exit 9"
setup_scripts = ${perOs}
env = { GREETING = "hello-alpha" }

[[environment]]
id = "beta"
name = "Beta"
setup_scripts = ${perOs}
env = { GREETING = "hello-beta" }
`

function git(args: string[], cwd: string): string {
  return execFileSync('git', ['-c', 'user.email=e2e@odex.test', '-c', 'user.name=e2e', ...args], { cwd, encoding: 'utf8' })
}

async function rpc<T = any>(page: Page, method: string, params: unknown): Promise<T> {
  return page.evaluate(([m, p]) => (window as any).odex.request(m, p), [method, params] as const) as Promise<T>
}

async function thread(page: Page, id: string): Promise<any> {
  return (await rpc(page, 'thread/read', { threadId: id })).thread
}

async function setupDone(page: Page, id: string): Promise<string> {
  let status = ''
  await expect
    .poll(async () => {
      status = (await thread(page, id)).worktree?.setupStatus ?? ''
      return status && status !== 'running'
    }, { timeout: 60_000 })
    .toBeTruthy()
  return status
}

async function shoot(page: Page, name: string, target?: { screenshot(opts: { path: string }): Promise<unknown> }): Promise<void> {
  const t = target ?? page
  await t.screenshot({ path: path.join(shots, `${name}-light.png`) })
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'dark' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  await page.waitForTimeout(250)
  await t.screenshot({ path: path.join(shots, `${name}-dark.png`) })
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
}

async function browserTitles(page: Page): Promise<string[]> {
  const st = await page.evaluate(() => (window as any).odex.browser.state())
  return (st.tabs as Array<{ title: string; url: string }>).map((t) => `${t.title} ${t.url}`)
}

async function openProjectEditor(page: Page) {
  // the project menu button shows on hover
  const header = page.locator('.sidebar-section-header[aria-expanded]').first()
  await header.hover()
  await header.locator('button[aria-label="Project actions"]').click()
  await page.getByRole('menuitem', { name: 'Edit project…' }).click()
  return page.getByRole('dialog', { name: /^Edit / })
}

function openThreadRow(page: Page, name: string) {
  return page.locator('.thread-row', { hasText: name }).first()
}

test.beforeAll(async () => {
  fs.mkdirSync(shots, { recursive: true })
  const agentCmd = WIN ? 'Write-Output "agent-sees-$env:GREETING"' : 'echo agent-sees-$GREETING'
  // tools/server.js is committed, so it exists in every worktree (the agent runs it from its cwd)
  mock = await startMock([
    { when: { last_role: 'user', last_user_contains: 'show greeting' }, reply: { kind: 'tool_calls', calls: [{ name: 'shell', arguments: { command: agentCmd } }] } },
    { when: { last_role: 'tool', last_user_contains: 'show greeting' }, reply: { kind: 'text', text: 'Greeting shown.' } },
    {
      when: { last_role: 'user', last_user_contains: 'start server' },
      reply: { kind: 'tool_calls', calls: [{ name: 'exec_command', arguments: { command: 'node tools/server.js agent', yield_ms: 1500 } }] },
    },
    { when: { last_role: 'tool', last_user_contains: 'start server' }, reply: { kind: 'text', text: 'Server started.' } },
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Env test"}' } },
    { when: {}, reply: { kind: 'text', text: 'OK' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  const { project } = L
  // test files live in an excluded folder so they never count as changes
  scratch = path.join(project, 'scratch')
  fs.mkdirSync(path.join(scratch, 'my tools'), { recursive: true })
  fs.appendFileSync(path.join(project, '.git', 'info', 'exclude'), '\nscratch/\nsetup.txt\n')
  serverJs = path.join(project, 'tools', 'server.js')
  fs.mkdirSync(path.dirname(serverJs), { recursive: true })
  fs.writeFileSync(
    serverJs,
    `const http = require('http')
const who = process.argv[2] || 'x'
const s = http.createServer((q, r) => { r.writeHead(200, { 'content-type': 'text/html' }); r.end('<title>dev-server ' + who + '</title><h1>dev-server-ok ' + who + '</h1>') })
s.listen(0, () => {
  const port = s.address().port
  if (who === 'agent') console.log('Listening on http://0.0.0.0:' + port)
  else console.log('  \\x1b[32m➜\\x1b[39m  Local:   \\x1b[36mhttp://localhost:\\x1b[1m' + port + '\\x1b[22m/\\x1b[39m')
})
setTimeout(() => process.exit(0), 120000)
`,
  )
  fs.mkdirSync(path.join(project, '.odex'), { recursive: true })
  fs.writeFileSync(path.join(project, '.odex', 'environments.toml'), ENVS)
  git(['add', '.'], project)
  git(['commit', '-q', '-m', 'environments'], project)
  projectId = await addProject(L.page, project)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('environments: project default and explicit selection, variables in setup, agent shell and !cmd', async () => {
  const { page, project } = L
  // set the project default to Beta in the project editor
  const dlg = await openProjectEditor(page)
  await dlg.getByRole('tab', { name: /^Environments/ }).click()
  await expect(dlg.getByRole('group', { name: 'Environment Alpha' })).toBeVisible()
  await dlg.getByRole('radio', { name: 'Default environment Beta' }).check()
  await dlg.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dlg).toHaveCount(0)
  await expect.poll(async () => (await rpc(page, 'project/list', {})).projects[0].defaultEnvironment).toBe('beta')

  // no environmentId: the project default
  const beta = await rpc(page, 'thread/start', { projectId, runMode: 'worktree', name: 'Beta thread', permissionMode: 'full-access' })
  ids.beta = beta.thread.id
  expect(await setupDone(page, ids.beta)).toBe('ok')
  const betaWt = (await thread(page, ids.beta)).worktree
  expect(fs.readFileSync(path.join(betaWt.path, 'setup.txt'), 'utf8').trim()).toBe('hello-beta|beta')
  expect(betaWt.setupLog).toBeTruthy()

  // explicit environmentId wins over the default
  const alpha = await rpc(page, 'thread/start', { projectId, runMode: 'worktree', name: 'Alpha thread', environmentId: 'alpha', permissionMode: 'full-access' })
  ids.alpha = alpha.thread.id
  expect(alpha.thread.environmentId).toBe('alpha')
  expect(await setupDone(page, ids.alpha)).toBe('ok')
  expect(fs.readFileSync(path.join((await thread(page, ids.alpha)).worktree.path, 'setup.txt'), 'utf8').trim()).toBe('hello-alpha|alpha')
  // unknown ids are refused
  await expect(rpc(page, 'thread/start', { projectId, runMode: 'worktree', environmentId: 'nope' })).rejects.toThrow(/unknown environment/)

  // the agent's shell and `!cmd` see the environment's variables
  await openThreadRow(page, 'Beta thread').click()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('please show greeting')
  await box.press('Enter')
  await expect(page.getByText('Greeting shown.')).toBeVisible()
  await box.fill(WIN ? '!Write-Output "bang-$env:GREETING"' : '!echo bang-$GREETING')
  await box.press('Enter')
  await expect
    .poll(async () => JSON.stringify((await rpc(page, 'thread/read', { threadId: ids.beta })).turns), { timeout: 30_000 })
    .toContain('bang-hello-beta')
  const turns = JSON.stringify((await rpc(page, 'thread/read', { threadId: ids.beta })).turns)
  expect(turns).toContain('agent-sees-hello-beta')
  expect(fs.existsSync(path.join(project, 'setup.txt'))).toBe(false)
})

test('per-OS setup scripts survive an in-app save; Settings → Local environments', async () => {
  const { page, project } = L
  const file = path.join(project, '.odex', 'environments.toml')
  const dlg = await openProjectEditor(page)
  await dlg.getByRole('tab', { name: /^Environments/ }).click()
  const alpha = dlg.getByRole('group', { name: 'Environment Alpha' })
  // the file's per-OS scripts are shown in their tabs
  await alpha.getByRole('tab', { name: 'Windows' }).click()
  await expect(alpha.getByLabel('Windows setup script')).toHaveValue(setupScripts.windows)
  await alpha.getByRole('tab', { name: 'Linux' }).click()
  await expect(alpha.getByLabel('Linux setup script')).toHaveValue(setupScripts.linux)
  await shoot(page, 'project-editor-environments', dlg)
  // edit only the name and the default script, then save
  await alpha.getByLabel('Environment name').fill('Alpha env')
  await alpha.getByRole('tab', { name: 'Default' }).click()
  await alpha.getByLabel('Setup script', { exact: true }).fill('exit 8')
  await dlg.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dlg).toHaveCount(0)
  let text = fs.readFileSync(file, 'utf8')
  expect(text).toContain('name = "Alpha env"')
  expect(text).toContain('exit 8')
  for (const os of ['windows', 'linux', 'macos'] as const) expect(text).toContain(`${os} = `)
  expect(text).toContain('GREETING = "hello-alpha"')
  // the engine still runs the per-OS script after the save
  const p = (await rpc(page, 'project/list', {})).projects[0]
  expect(p.environments[0].setupScripts.windows).toBe(setupScripts.windows)

  // Settings → Local environments edits the same file
  await page.getByRole('button', { name: 'Settings' }).click()
  await page.getByRole('button', { name: 'Local environments' }).click()
  const card = page.getByRole('group', { name: /^Environments of / })
  await expect(card.getByRole('group', { name: 'Environment Alpha env' })).toBeVisible()
  const beta = card.getByRole('group', { name: 'Environment Beta' })
  await beta.getByRole('tab', { name: 'macOS' }).click()
  await beta.getByLabel('macOS setup script').fill('echo mac-edited')
  await shoot(page, 'settings-local-environments')
  await card.getByRole('button', { name: 'Save', exact: true }).click()
  await expect.poll(() => fs.readFileSync(file, 'utf8')).toContain('echo mac-edited')
  text = fs.readFileSync(file, 'utf8')
  expect(text.match(/windows = /g)?.length).toBe(2)
  expect((await rpc(page, 'project/list', {})).projects[0].defaultEnvironment).toBe('beta')
})

test('move a local thread and its uncommitted changes into a worktree', async () => {
  const { page, project } = L
  git(['add', '-A'], project)
  git(['commit', '-q', '-m', 'edits from the previous tests'], project)
  const t = await rpc(page, 'thread/start', { projectId, runMode: 'local', name: 'Local mover', permissionMode: 'full-access' })
  ids.mover = t.thread.id
  fs.writeFileSync(path.join(project, 'README.md'), '# Demo project\n\nEdited locally before moving.\n')
  fs.writeFileSync(path.join(project, 'feature.txt'), 'new feature\n')

  await openThreadRow(page, 'Local mover').click({ button: 'right' })
  await page.getByRole('menuitem', { name: 'Move to worktree…' }).click()
  const dlg = page.getByRole('dialog', { name: 'Move to worktree' })
  const list = dlg.getByRole('list', { name: 'Changes to move' })
  await expect(list).toContainText('README.md')
  await expect(list).toContainText('feature.txt')
  await dlg.getByLabel('Environment').selectOption('alpha')
  await shoot(page, 'move-to-worktree', dlg)
  await dlg.getByRole('button', { name: 'Move to worktree' }).click()
  await expect(dlg).toHaveCount(0)
  await expect(page.getByText(/Moved 2 changed file\(s\) into a worktree/)).toBeVisible()

  const moved = await thread(page, ids.mover)
  expect(moved.runMode).toBe('worktree')
  expect(moved.environmentId).toBe('alpha')
  expect(fs.readFileSync(path.join(moved.worktree.path, 'README.md'), 'utf8')).toContain('Edited locally before moving.')
  expect(fs.readFileSync(path.join(moved.worktree.path, 'feature.txt'), 'utf8')).toContain('new feature')
  expect(git(['status', '--porcelain'], project).trim()).toBe('')
  expect(git(['stash', 'list'], project)).toContain('odex: moved to')
  expect(await setupDone(page, ids.mover)).toBe('ok')
  expect(fs.readFileSync(path.join(moved.worktree.path, 'setup.txt'), 'utf8').trim()).toBe('hello-alpha|alpha')
})

test('worktree retention removes the oldest archived worktrees beyond keep', async () => {
  const { page, home } = L
  await page.getByRole('button', { name: 'Settings' }).click()
  await page.getByRole('button', { name: 'Worktrees', exact: true }).click()
  const keep = page.getByLabel('Worktrees to keep')
  await expect(keep).toHaveValue('15')
  await keep.fill('1')
  await keep.press('Enter')
  await expect.poll(() => fs.readFileSync(path.join(home, 'config.toml'), 'utf8')).toMatch(/\[worktrees\][\s\S]*keep = 1/)
  await expect(page.getByRole('switch', { name: 'Clean up worktrees automatically' })).toHaveAttribute('aria-checked', 'true')

  const made: Array<{ id: string; path: string }> = []
  for (const name of ['Keep A', 'Keep B', 'Keep C']) {
    const r = await rpc(page, 'thread/start', { projectId, runMode: 'worktree', name, environmentId: '' })
    made.push({ id: r.thread.id, path: r.thread.worktree.path })
    expect(r.thread.worktree.setupStatus ?? null).toBeNull()
  }
  for (const m of made) expect(fs.existsSync(m.path)).toBe(true)
  // archiving A and B (keeping their worktrees) lets retention remove them: active ones stay
  await rpc(page, 'thread/archive', { threadId: made[0].id, removeWorktree: false })
  await rpc(page, 'thread/archive', { threadId: made[1].id, removeWorktree: false })
  await expect.poll(() => fs.existsSync(made[0].path) || fs.existsSync(made[1].path), { timeout: 30_000 }).toBe(false)
  expect(fs.existsSync(made[2].path)).toBe(true)
  for (const id of [ids.beta, ids.alpha, ids.mover]) expect(fs.existsSync((await thread(page, id)).worktree.path)).toBe(true)
  expect(git(['for-each-ref', '--format=%(refname)', 'refs/odex/archived'], L.project)).toContain(made[0].id)

  await page.getByRole('button', { name: 'Refresh worktrees' }).click()
  await expect(page.locator('.wt-item')).toHaveCount(4)
  await page.getByRole('button', { name: 'Clean up now' }).last().click()
  await expect(page.getByText('Nothing to clean up')).toBeVisible()
  await shoot(page, 'settings-worktrees-retention')

  // unarchiving restores the removed worktree
  await rpc(page, 'thread/unarchive', { threadId: made[0].id })
  expect(fs.existsSync(made[0].path)).toBe(true)
})

test('per-OS action command, dev-server URL from an action opens in the browser panel', async () => {
  const { page, project } = L
  await openThreadRow(page, 'Beta thread').click()
  const dlg = await openProjectEditor(page)
  await dlg.getByRole('tab', { name: /^Actions/ }).click()
  await dlg.getByRole('button', { name: 'Add action' }).click()
  const a1 = dlg.getByRole('group', { name: 'Action 1' })
  await a1.getByLabel('Action name').fill('Say hello')
  await dlg.getByRole('group', { name: 'Action Say hello' }).getByLabel('Action command', { exact: true }).fill('echo default-cmd')
  const say = dlg.getByRole('group', { name: 'Action Say hello' })
  await say.getByRole('button', { name: /Per-OS commands/ }).click()
  const os = WIN ? 'Windows' : process.platform === 'darwin' ? 'macOS' : 'Linux'
  await say.getByLabel(`${os} command`).fill(WIN ? 'Write-Output "per-os-$(40+2)"' : 'echo per-os-$((40+2))')
  await dlg.getByRole('button', { name: 'Add action' }).click()
  const a2 = dlg.getByRole('group', { name: 'Action 2' })
  await a2.getByLabel('Action name').fill('Serve')
  await dlg.getByRole('group', { name: 'Action Serve' }).getByLabel('Action command', { exact: true }).fill(`node "${serverJs}" action`)
  await shoot(page, 'project-editor-actions', dlg)
  await dlg.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dlg).toHaveCount(0)
  const toml = fs.readFileSync(path.join(project, '.odex', 'actions.toml'), 'utf8')
  expect(toml).toContain(`${os.toLowerCase()} = `)
  expect(toml).toContain('per-os-')

  const bar = page.locator('div[aria-label="Project actions"]').first()
  await bar.getByRole('button', { name: 'Say hello' }).click()
  const rows = page.locator('.bottom-panel .xterm-rows:visible')
  await expect(rows).toContainText('per-os-42', { timeout: 20_000 })

  // a dev server started by an action: its printed URL opens in the in-app browser
  await bar.getByRole('button', { name: 'Serve' }).click()
  await expect.poll(async () => (await browserTitles(page)).join('\n'), { timeout: 30_000 }).toContain('dev-server action')
})

test('editor command: {file}/{line} placeholders with quoting, and a per-project override', async () => {
  const { page, project } = L
  const editorJs = path.join(scratch, 'my tools', 'editor.js')
  fs.writeFileSync(editorJs, `require('fs').writeFileSync(process.argv[2], JSON.stringify(process.argv.slice(3)))\n`)
  const out1 = path.join(scratch, 'editor-global.json')
  const out2 = path.join(scratch, 'editor-project.json')
  const outside = path.join(path.dirname(project), 'outside file.txt')
  fs.writeFileSync(outside, 'x\n')
  const inside = path.join(scratch, 'my file.py')
  fs.writeFileSync(inside, 'print(1)\n')

  await page.evaluate((cmd) => (window as any).odex.settings.set({ editor: cmd }), `node "${editorJs}" "${out1}" --goto {file}:{line} --file {file}`)
  await page.evaluate(([f]) => (window as any).odex.shell.openInEditor(f, 7), [outside])
  await expect.poll(() => fs.existsSync(out1), { timeout: 20_000 }).toBe(true)
  expect(JSON.parse(fs.readFileSync(out1, 'utf8'))).toEqual(['--goto', `${outside}:7`, '--file', outside])

  // per-project override from the project editor
  const dlg = await openProjectEditor(page)
  await dlg.getByLabel('Open files with').fill(`node "${editorJs}" "${out2}" {file} {line}`)
  await dlg.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dlg).toHaveCount(0)
  await page.evaluate(([f]) => (window as any).odex.shell.openInEditor(f), [inside])
  await expect.poll(() => fs.existsSync(out2), { timeout: 20_000 }).toBe(true)
  expect(JSON.parse(fs.readFileSync(out2, 'utf8'))).toEqual([inside, '1'])
  // files of the project's worktrees use the override too
  fs.rmSync(out2)
  const wtFile = path.join((await thread(page, ids.beta)).worktree.path, 'README.md')
  await page.evaluate(([f]) => (window as any).odex.shell.openInEditor(f, 3), [wtFile])
  await expect.poll(() => fs.existsSync(out2), { timeout: 20_000 }).toBe(true)
  expect(JSON.parse(fs.readFileSync(out2, 'utf8'))).toEqual([wtFile, '3'])
})

test('agent dev server: URL auto-opens, Background list shows it and Kill stops it', async () => {
  const { page } = L
  await openThreadRow(page, 'Beta thread').click()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('please start server')
  await box.press('Enter')
  await expect(page.getByText('Server started.')).toBeVisible({ timeout: 30_000 })
  await expect.poll(async () => (await browserTitles(page)).join('\n'), { timeout: 30_000 }).toContain('dev-server agent')

  if (!(await page.locator('.bottom-panel').isVisible())) await page.keyboard.press('Control+J')
  const panel = page.locator('.bottom-panel')
  await panel.getByRole('button', { name: /^Background/ }).click()
  const region = panel.getByRole('region', { name: 'Background processes' })
  const row = region.locator('.bg-session', { hasText: 'server.js agent' })
  await expect(row).toContainText(/running · \d+s/)
  await shoot(page, 'terminal-background', panel)
  await row.getByRole('button', { name: /^Kill / }).click()
  await expect(row).toContainText('exited', { timeout: 20_000 })
  await expect(row.getByRole('button', { name: /^Kill / })).toHaveCount(0)
})

test('Ctrl+L clears the integrated terminal', async () => {
  const { page } = L
  const panel = page.locator('.bottom-panel')
  if (!(await panel.isVisible())) await page.keyboard.press('Control+J')
  await panel.getByRole('button', { name: 'New terminal' }).click()
  const rows = panel.locator('.xterm-rows:visible')
  await expect(rows).toContainText(/\S/, { timeout: 20_000 })
  await panel.locator('.xterm-helper-textarea').last().focus()
  // the output differs from the typed command line, so waiting for it waits for the command to finish
  await page.keyboard.type(WIN ? 'echo ("clear-" + "marker-1")\r' : 'echo clear-$((0+1))-marker\r')
  const marker = WIN ? 'clear-marker-1' : 'clear-1-marker'
  await expect(rows).toContainText(marker, { timeout: 20_000 })
  await page.waitForTimeout(300)
  await page.keyboard.press('Control+L')
  await expect(rows).not.toContainText(marker)
  // the shell still works afterwards
  await page.keyboard.type(WIN ? 'echo ("after-" + "clear-2")\r' : 'echo after-$((1+1))-clear\r')
  await expect(rows).toContainText(WIN ? 'after-clear-2' : 'after-2-clear', { timeout: 20_000 })
  await expect(rows).not.toContainText(marker)
})
