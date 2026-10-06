import { test, expect, type Page } from '@playwright/test'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { addProject, desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

// Settings → MCP servers, Skills, Hooks and Plugins against the real engine.
// Set ODEX_SHOTS=<dir> to collect light/dark screenshots of each panel.

const FIXTURE = path.join(desktopDir, 'e2e', 'fixtures', 'mcp-server.mjs')
const NODE = process.execPath

let mock: Mock
let L: Launched

test.beforeAll(async () => {
  mock = await startMock([{ when: {}, reply: { kind: 'text', text: 'ok' } }])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  await addProject(L.page, L.project)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

async function openPanel(page: Page, label: string): Promise<void> {
  const nav = page.getByRole('navigation', { name: 'Settings sections' })
  if (!(await nav.isVisible().catch(() => false))) await page.getByRole('button', { name: 'Settings', exact: true }).click()
  await nav.getByRole('button', { name: label, exact: true }).click()
  await expect(page.getByRole('heading', { level: 2, name: label })).toBeVisible()
}

/** Screenshot the current view in light and dark (only when ODEX_SHOTS is set). */
async function shoot(page: Page, name: string): Promise<void> {
  const dir = process.env.ODEX_SHOTS
  if (!dir) return
  fs.mkdirSync(dir, { recursive: true })
  for (const theme of ['light', 'dark'] as const) {
    await page.evaluate((t) => (window as any).odex.settings.set({ theme: t }), theme)
    await expect(page.locator('html')).toHaveAttribute('data-theme', theme)
    await page.waitForTimeout(250)
    await page.screenshot({ path: path.join(dir, `${name}-${theme}.png`) })
  }
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
}

test('MCP: add a stdio server, test it, inspect tools, resources and logs', async () => {
  const { page } = L
  await openPanel(page, 'MCP servers')
  await expect(page.getByText('No MCP servers yet')).toBeVisible()
  await page.getByRole('button', { name: 'Add server' }).click()
  const dlg = page.getByRole('dialog', { name: 'Add MCP server' })
  await expect(dlg).toBeVisible()
  // validation: nothing filled in
  await dlg.getByRole('button', { name: 'Save' }).click()
  await expect(dlg.getByRole('alert')).toContainText('Name is required')
  await dlg.getByLabel('Name', { exact: true }).fill('fixture')
  await dlg.getByLabel('Command to launch').fill(NODE)
  await dlg.getByRole('button', { name: 'Add argument' }).click()
  await dlg.getByLabel('Argument 1', { exact: true }).fill(FIXTURE)
  await dlg.getByRole('button', { name: 'Add variable' }).click()
  await dlg.getByLabel('Environment variable name 1').fill('FIXTURE_MODE')
  await dlg.getByLabel('Environment variable value 1').fill('e2e')
  await shoot(page, 'mcp-editor')
  // Test = save + start, then show the resulting status
  await dlg.getByRole('button', { name: 'Test' }).click()
  const result = dlg.getByRole('status', { name: 'Test result' })
  await expect(result).toContainText('Ready', { timeout: 30_000 })
  await expect(result).toContainText('2 tools')
  await expect(result).toContainText('odex-e2e-fixture 1.2.3')
  // Test again restarts the unchanged server and still reaches ready
  await dlg.getByRole('button', { name: 'Test' }).click()
  await expect(result).toContainText('Ready', { timeout: 30_000 })
  await dlg.getByRole('button', { name: 'Close' }).click()
  await expect(dlg).toBeHidden()

  const card = page.getByTestId('mcp-server-fixture')
  await expect(card.getByTestId('mcp-state')).toHaveText('Ready')
  await expect(card).toContainText('2 tools')
  await expect(card).toContainText('1 resource')
  await expect(card).toContainText('1 prompt')
  // the config landed in config.toml
  const toml = fs.readFileSync(path.join(L.home, 'config.toml'), 'utf8')
  expect(toml).toContain('[mcp_servers.fixture]')
  expect(toml).toContain('FIXTURE_MODE')

  // expand: tools with read-only hint, resources, prompts
  await card.getByRole('button', { name: 'Expand fixture' }).click()
  const tools = card.getByRole('table', { name: 'fixture tools' })
  await expect(tools).toContainText('echo')
  await expect(tools).toContainText('Echo text back')
  await expect(tools).toContainText('read-only')
  await expect(tools).toContainText('add')
  await expect(card.getByRole('table', { name: 'fixture prompts' })).toContainText('greet')
  await shoot(page, 'mcp')

  // disable one tool (filter change, no restart), then re-enable it
  await card.getByLabel('Enable tool add').uncheck()
  await expect(card).toContainText('1/2 tools')
  expect(fs.readFileSync(path.join(L.home, 'config.toml'), 'utf8')).toMatch(/disabled_tools\s*=\s*\[\s*"add"\s*\]/)
  await card.getByLabel('Enable tool add').check()
  await expect(card).toContainText('2 tools')
  await expect(card).not.toContainText('1/2 tools')

  // resource preview through mcp/readResource
  await card.getByRole('button', { name: 'Preview readme' }).click()
  const preview = page.getByRole('dialog', { name: 'readme' })
  await expect(preview).toContainText('This resource comes from the Odex e2e MCP fixture.')
  await preview.getByRole('button', { name: 'Close' }).click()

  // logs modal (stderr of the server process)
  await card.getByRole('button', { name: 'Logs for fixture' }).click()
  const logs = page.getByRole('dialog', { name: 'Logs · fixture' })
  await expect(logs.getByLabel('fixture logs')).toContainText('fixture MCP server started')
  await shoot(page, 'mcp-logs')
  await logs.getByRole('button', { name: 'Close' }).click()

  // disable and re-enable the whole server
  await card.getByRole('switch', { name: 'Enable fixture' }).click()
  await expect(card.getByTestId('mcp-state')).toHaveText('Disabled')
  await card.getByRole('switch', { name: 'Enable fixture' }).click()
  await expect(card.getByTestId('mcp-state')).toHaveText('Ready', { timeout: 30_000 })

  // edit keeps the values
  await card.getByRole('button', { name: 'Edit fixture' }).click()
  const edit = page.getByRole('dialog', { name: 'Edit fixture' })
  await expect(edit.getByLabel('Command to launch')).toHaveValue(NODE)
  await expect(edit.getByLabel('Argument 1', { exact: true })).toHaveValue(FIXTURE)
  await edit.getByText('Advanced', { exact: true }).click()
  await expect(edit.getByLabel('Tool approval')).toBeVisible()
  await shoot(page, 'mcp-editor-advanced')
  await edit.getByRole('button', { name: 'Cancel' }).click()
  await expect(edit).toBeHidden()
})

test('MCP: HTTP form validation, a failing server shows its error and can be removed', async () => {
  const { page } = L
  await openPanel(page, 'MCP servers')
  await page.getByRole('button', { name: 'Add server' }).click()
  const dlg = page.getByRole('dialog', { name: 'Add MCP server' })
  await dlg.getByRole('radio', { name: 'Streamable HTTP' }).click()
  await dlg.getByLabel('Name', { exact: true }).fill('remote')
  await dlg.getByLabel('URL').fill('not-a-url')
  await expect(dlg.getByLabel('Bearer token environment variable')).toBeVisible()
  await dlg.getByRole('button', { name: 'Save' }).click()
  await expect(dlg.getByRole('alert')).toContainText('URL must start with http')
  await shoot(page, 'mcp-editor-http')
  // switch back to stdio with a command that does not exist
  await dlg.getByRole('radio', { name: 'STDIO' }).click()
  await dlg.getByLabel('Name', { exact: true }).fill('broken')
  await dlg.getByLabel('Command to launch').fill('odex-no-such-command-xyz')
  await dlg.getByRole('button', { name: 'Test' }).click()
  await expect(dlg.getByRole('status', { name: 'Test result' })).toContainText('Failed', { timeout: 30_000 })
  await dlg.getByRole('button', { name: 'Close' }).click()

  const card = page.getByTestId('mcp-server-broken')
  await expect(card.getByTestId('mcp-state')).toHaveText('Failed')
  await expect(card.locator('.int-item-error')).not.toBeEmpty()
  await shoot(page, 'mcp-failed')
  await card.getByRole('button', { name: 'Remove broken' }).click()
  await page.getByRole('dialog', { name: 'Remove MCP server' }).getByRole('button', { name: 'Remove' }).click()
  await expect(card).toBeHidden()
  expect(fs.readFileSync(path.join(L.home, 'config.toml'), 'utf8')).not.toContain('[mcp_servers.broken]')
  await expect(page.getByTestId('mcp-server-fixture')).toBeVisible()
})

test('MCP: Roblox Studio and IDA Pro templates fill in the server form', async () => {
  const { page } = L
  await openPanel(page, 'MCP servers')
  await page.getByRole('button', { name: 'Add server' }).click()
  const dlg = page.getByRole('dialog', { name: 'Add MCP server' })
  const args = () => dlg.getByRole('group', { name: 'Argument' }).locator('input').evaluateAll((els) => els.map((e) => (e as HTMLInputElement).value))
  const name = dlg.getByLabel('Name', { exact: true })

  await dlg.getByRole('button', { name: 'Roblox Studio' }).click()
  await expect(name).toHaveValue('roblox-studio')
  if (process.platform === 'win32') {
    await expect(dlg.getByLabel('Command to launch')).toHaveValue('cmd.exe')
    expect(await args()).toEqual(['/c', '%LOCALAPPDATA%\\Roblox\\mcp.bat'])
  } else {
    await expect(dlg.getByLabel('Command to launch')).toHaveValue('/Applications/RobloxStudio.app/Contents/MacOS/StudioMCP')
    expect(await args()).toEqual([])
  }

  // a template only names an unnamed server
  await name.fill('')
  await dlg.getByRole('button', { name: 'IDA Pro' }).click()
  await expect(name).toHaveValue('ida')
  await expect(dlg.getByLabel('Command to launch')).toHaveValue('uvx')
  expect(await args()).toEqual(['ida-nexus', 'mcp', '--agent=odex'])
  // the first uvx run downloads the server, so the template allows a slow start
  await dlg.locator('summary', { hasText: 'Advanced' }).click()
  await expect(dlg.getByLabel('Startup timeout (seconds)')).toHaveValue('120')

  // nothing is saved (or launched) until the user saves
  await dlg.getByRole('button', { name: 'Cancel' }).click()
  expect(fs.readFileSync(path.join(L.home, 'config.toml'), 'utf8')).not.toMatch(/mcp_servers\.(ida|roblox-studio)/)
})

test('Skills: create, validate, edit and disable a skill', async () => {
  const { page } = L
  await openPanel(page, 'Skills')
  await page.getByRole('button', { name: 'New skill' }).click()
  const dlg = page.getByRole('dialog', { name: 'New skill' })
  const editor = dlg.getByRole('textbox', { name: 'SKILL.md' })
  await expect(editor).toHaveValue(/name: my-skill/)
  // front matter validation
  await editor.fill('---\nname: e2e-skill\n---\n\nDo the thing.\n')
  await expect(dlg.getByTestId('skill-validation')).toContainText('needs a description')
  await expect(dlg.getByRole('button', { name: 'Create skill' })).toBeDisabled()
  await editor.fill('# no front matter')
  await expect(dlg.getByTestId('skill-validation')).toContainText('must start with a front matter block')
  await editor.fill('---\nname: e2e-skill\ndescription: Checks the e2e flow end to end.\n---\n\n# E2E\n\n1. Run the tests.\n')
  await expect(dlg.getByTestId('skill-validation')).toContainText('Front matter is valid')
  await shoot(page, 'skills-editor')
  await dlg.getByRole('button', { name: 'Create skill' }).click()
  await expect(dlg).toBeHidden()

  const row = page.getByTestId('skill-e2e-skill')
  await expect(row).toContainText('$e2e-skill')
  await expect(row).toContainText('Checks the e2e flow end to end.')
  await expect(page.getByRole('group', { name: 'User', exact: true })).toContainText('$e2e-skill')
  const file = path.join(L.home, 'skills', 'e2e-skill', 'SKILL.md')
  expect(fs.readFileSync(file, 'utf8')).toContain('description: Checks the e2e flow end to end.')

  // edit: the file text round-trips, extra keys survive
  await row.getByRole('button', { name: 'Edit e2e-skill' }).click()
  const edit = page.getByRole('dialog', { name: 'Edit $e2e-skill' })
  const box = edit.getByRole('textbox', { name: 'SKILL.md' })
  await expect(box).toHaveValue(/Run the tests/)
  await box.fill('---\nname: e2e-skill\ndescription: Updated description.\nversion: 2\n---\n\n# E2E\n\nUpdated body.\n')
  await edit.getByRole('button', { name: 'Save' }).click()
  await expect(edit).toBeHidden()
  await expect(row).toContainText('Updated description.')
  expect(fs.readFileSync(file, 'utf8')).toContain('version: 2')

  // disable
  await row.getByRole('switch', { name: 'Enable e2e-skill' }).click()
  await expect(row).toContainText('disabled')
  await shoot(page, 'skills')
  await row.getByRole('switch', { name: 'Enable e2e-skill' }).click()
  await expect(row).not.toContainText('disabled')

  // a project-scoped skill lands in <project>/.odex/skills and can be deleted again
  await page.getByRole('button', { name: 'New skill' }).click()
  const pdlg = page.getByRole('dialog', { name: 'New skill' })
  await pdlg.getByLabel('Save to').selectOption({ label: 'Project project (.odex/skills)' })
  await pdlg.getByRole('textbox', { name: 'SKILL.md' }).fill('---\nname: proj-skill\ndescription: Only for this project.\n---\n\nSteps.\n')
  await pdlg.getByRole('button', { name: 'Create skill' }).click()
  await expect(pdlg).toBeHidden()
  const projGroup = page.getByRole('group', { name: 'Project · project' })
  await expect(projGroup.getByTestId('skill-proj-skill')).toBeVisible()
  const projFile = path.join(L.project, '.odex', 'skills', 'proj-skill', 'SKILL.md')
  expect(fs.existsSync(projFile)).toBe(true)
  await projGroup.getByRole('button', { name: 'Delete proj-skill' }).click()
  await page.getByRole('dialog', { name: 'Delete skill' }).getByRole('button', { name: 'Delete' }).click()
  await expect(page.getByTestId('skill-proj-skill')).toBeHidden()
  expect(fs.existsSync(projFile)).toBe(false)
})

test('Hooks: a new project hook needs review, then is trusted; editing its script asks again', async () => {
  const { page, project } = L
  const cfgDir = path.join(project, '.odex')
  fs.mkdirSync(cfgDir, { recursive: true })
  // the hook hash covers the scripts a command references, so editing guard.js needs a new review
  fs.writeFileSync(path.join(cfgDir, 'guard.js'), 'process.exit(0)\n')
  fs.writeFileSync(path.join(cfgDir, 'config.toml'), ['[[hooks.pre_tool_use]]', 'name = "e2e guard"', 'matcher = "shell"', 'command = "node .odex/guard.js"', ''].join('\n'))
  await openPanel(page, 'Hooks')

  const review = page.getByRole('region', { name: 'Hooks needing review' })
  await expect(review).toContainText('e2e guard', { timeout: 20_000 })
  await expect(review).toContainText('New hook')
  await expect(review.getByLabel('Hook command')).toHaveText('node .odex/guard.js')
  await expect(review).toContainText('Runs for tools matching shell')
  const group = page.getByRole('group', { name: 'Before tool use' })
  await expect(group).toContainText('Untrusted')
  await expect(group).toContainText('Project · project')
  // the app-wide banner links here
  await expect(page.getByText(/need your review before they can run/)).toBeVisible()
  await shoot(page, 'hooks-review')

  await review.getByRole('button', { name: 'Trust' }).click()
  await expect(review).toBeHidden()
  await expect(group).toContainText('Trusted')
  await expect(page.getByText(/need your review before they can run/)).toBeHidden()
  const listed = await page.evaluate((cwd) => (window as any).odex.request('hooks/list', { cwd }), project)
  expect(listed.hooks.find((h: any) => h.name === 'e2e guard')?.trust).toBe('trusted')
  await shoot(page, 'hooks')

  // edit the script → "changed", then keep it disabled
  fs.writeFileSync(path.join(cfgDir, 'guard.js'), 'process.exit(2)\n')
  await page.getByRole('button', { name: 'Refresh' }).click()
  await expect(review).toContainText('Changed since trusted')
  await expect(group).toContainText('Changed')
  await review.getByRole('button', { name: 'Keep disabled' }).click()
  await expect(review).toBeHidden()
  await expect(group).toContainText('Disabled')
  await expect(page.getByText(/need your review before they can run/)).toBeHidden()
})

test('Plugins: install from a folder, review what it adds, trust and enable', async () => {
  const { page } = L
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'odex-plugin-'))
  fs.writeFileSync(
    path.join(dir, 'odex-plugin.toml'),
    [
      'name = "e2e-plugin"',
      'version = "0.3.0"',
      'description = "Plugin used by the integrations e2e test"',
      '',
      '[mcp_servers.fx]',
      `command = '${NODE}'`,
      `args = ['${FIXTURE}']`,
      '',
      '[[hooks.stop]]',
      'name = "plugin stop hook"',
      `command = 'node -e "console.log(1)"'`,
      '',
    ].join('\n'),
  )
  fs.mkdirSync(path.join(dir, 'skills', 'plugin-skill'), { recursive: true })
  fs.writeFileSync(path.join(dir, 'skills', 'plugin-skill', 'SKILL.md'), '---\nname: plugin-skill\ndescription: Comes from the e2e plugin.\n---\n\nBody.\n')

  await openPanel(page, 'Plugins')
  await expect(page.getByText('No plugins installed.')).toBeVisible()
  await page.getByLabel('Plugin folder or git URL').fill(dir)
  await page.getByRole('button', { name: 'Install', exact: true }).click()
  const dlg = page.getByRole('dialog', { name: 'Review e2e-plugin' })
  await expect(dlg).toBeVisible()
  await expect(dlg.getByRole('region', { name: 'MCP servers' })).toContainText('fx')
  await expect(dlg.getByRole('region', { name: 'MCP servers' })).toContainText('mcp-server.mjs')
  await expect(dlg.getByRole('region', { name: 'Hooks' })).toContainText('node -e "console.log(1)"')
  await shoot(page, 'plugins-review')
  await dlg.getByRole('button', { name: 'Trust and enable' }).click()
  await expect(dlg).toBeHidden()

  const card = page.getByTestId('plugin-e2e-plugin')
  await expect(card).toContainText('Trusted')
  await expect(card).toContainText('1 MCP server')
  await expect(card).toContainText('1 hook')
  await expect(card.getByRole('switch', { name: 'Enable e2e-plugin' })).toHaveAttribute('aria-checked', 'true')
  await shoot(page, 'plugins')

  // its MCP server and skill show up in the other panels
  await openPanel(page, 'MCP servers')
  const server = page.getByTestId('mcp-server-e2e-plugin-fx')
  await expect(server).toContainText('plugin')
  await expect(server.getByTestId('mcp-state')).toHaveText('Ready', { timeout: 30_000 })
  await openPanel(page, 'Skills')
  await expect(page.getByRole('group', { name: 'Plugin · e2e-plugin' })).toContainText('$plugin-skill')
  // the reviewed hook was trusted together with the plugin
  await openPanel(page, 'Hooks')
  const stop = page.getByRole('group', { name: 'Stop' })
  await expect(stop).toContainText('plugin stop hook')
  await expect(stop).toContainText('Plugin · e2e-plugin')
  await expect(stop).toContainText('Trusted')

  // disable, then remove
  await openPanel(page, 'Plugins')
  await card.getByRole('switch', { name: 'Enable e2e-plugin' }).click()
  await expect(card.getByRole('switch', { name: 'Enable e2e-plugin' })).toHaveAttribute('aria-checked', 'false')
  await openPanel(page, 'MCP servers')
  await expect(page.getByTestId('mcp-server-e2e-plugin-fx')).toBeHidden()
  await openPanel(page, 'Plugins')
  await card.getByRole('button', { name: 'Remove e2e-plugin' }).click()
  await page.getByRole('dialog', { name: 'Remove plugin' }).getByRole('button', { name: 'Remove' }).click()
  await expect(card).toBeHidden()
  await expect(page.getByText('No plugins installed.')).toBeVisible()
  fs.rmSync(dir, { recursive: true, force: true })
})
