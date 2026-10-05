import { test, expect } from '@playwright/test'
import { addProject, engineReady, launch, startMock, type Launched, type Mock } from './harness'

let mock: Mock
let L: Launched

const SUMMARY = {
  goal_and_requirements: ['Explore the demo project'],
  decisions: [],
  plan: [],
  files_changed: [],
  codebase_facts: ['main.py defines add(a, b)'],
  commands_and_tests: [],
  open_errors: [],
  next_steps: ['Wait for the next request'],
  important_refs: [],
}

test.beforeAll(async () => {
  mock = await startMock([
    { when: { structured_name: 'context_summary' }, reply: { kind: 'json', value: SUMMARY } },
    { when: { last_role: 'user', last_user_contains: 'read main' }, reply: { kind: 'tool_calls', calls: [{ name: 'read_file', arguments: { path: 'main.py' } }] } },
    { when: { last_role: 'tool' }, reply: { kind: 'text', text: 'main.py defines add(a, b) and prints 5.' } },
    { when: {}, reply: { kind: 'text', text: 'Noted.' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  await addProject(L.page, L.project)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('/compact summarizes the conversation and shows a notice', async () => {
  const { page } = L
  await page.getByRole('button', { name: 'No project', exact: true }).click()
  await page.getByRole('menuitem', { name: /^project/ }).click()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('please read main.py (read main)')
  await box.press('Enter')
  await expect(page.getByText('main.py defines add(a, b) and prints 5.')).toBeVisible()
  await box.fill('remember that the tests live in tests/')
  await box.press('Enter')
  await expect(page.getByText('Noted.')).toBeVisible()
  await box.fill('/compact keep file facts')
  await box.press('Enter')
  await expect(page.getByText(/context compacted .* \(summary #1, manual\)/)).toBeVisible()
  // the compactor got the focus instruction
  const reqs = await mock.requests()
  const comp = reqs.find((r) => r.body?.response_format?.json_schema?.name === 'context_summary')
  expect(comp).toBeTruthy()
  expect(JSON.stringify(comp.body.messages)).toContain('keep file facts')
  // the context view lists the compaction
  await page.getByText(/context compacted/).click()
  const dlg = page.getByRole('dialog')
  await expect(dlg.getByText('Compactions')).toBeVisible()
  await expect(dlg.getByText('model summary')).toBeVisible()
  await dlg.getByRole('button', { name: 'Close' }).click()
})

test('terminal panel runs a shell in the thread folder', async () => {
  const { page, project } = L
  await page.keyboard.press('Control+J')
  const rows = page.locator('.bottom-panel .xterm-rows')
  await expect(rows).toBeVisible()
  // wait for the shell prompt, then run a command
  await expect(rows).toContainText(/\S/, { timeout: 20_000 })
  await page.locator('.bottom-panel .xterm-helper-textarea').focus()
  const cmd = process.platform === 'win32' ? 'Write-Output ("odex-term-" + (2+3)); (Get-Location).Path' : 'echo odex-term-$((2+3)); pwd'
  await page.keyboard.type(`${cmd}\r`)
  await expect(rows).toContainText('odex-term-5', { timeout: 20_000 })
  // it starts in the project folder
  const leaf = project.split(/[\\/]/).pop()!
  await expect(rows).toContainText(leaf)
  // a second terminal tab
  await page.getByRole('button', { name: 'New terminal' }).click()
  await expect(page.locator('.bottom-panel [role="tab"]')).toHaveCount(2)
  // Ctrl+J inside the (new) terminal still toggles the panel
  await expect(page.locator('.bottom-panel .xterm-rows').last()).toContainText(/\S/, { timeout: 20_000 })
  await page.locator('.bottom-panel .xterm-helper-textarea').last().focus()
  await page.keyboard.press('Control+J')
  await expect(page.locator('.bottom-panel')).toBeHidden()
})
