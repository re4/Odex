import { test, expect } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'
import { addProject, engineReady, launch, startMock, type Launched, type Mock } from './harness'

let mock: Mock
let L: Launched

test.beforeAll(async () => {
  mock = await startMock([
    {
      when: { last_role: 'user', last_user_contains: 'create hello' },
      reply: { kind: 'tool_calls', text: 'Creating the file.', calls: [{ name: 'write_file', arguments: { path: 'hello.txt', content: 'hello from odex\n' } }] },
    },
    {
      when: { last_role: 'user', last_user_contains: 'install deps' },
      reply: { kind: 'tool_calls', calls: [{ name: 'shell', arguments: { command: 'echo installing', escalated: true, justification: 'needs network to download packages' } }] },
    },
    { when: { last_role: 'tool', last_user_contains: 'install deps' }, reply: { kind: 'text', text: 'Dependencies step finished.' } },
    { when: { last_role: 'tool' }, reply: { kind: 'text', text: 'Done. I created **hello.txt**.' } },
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Create hello file"}' } },
    { when: { last_role: 'user', last_user_contains: 'think' }, reply: { kind: 'reasoning', reasoning: 'Let me consider the request carefully.', text: 'Here is my considered answer.' } },
    { when: {}, reply: { kind: 'text', text: 'Hello! How can I help with this project?' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('home view renders with the configured model', async () => {
  const { page } = L
  await expect(page.getByRole('heading', { name: /What should we/ })).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeVisible()
  await expect(page.getByRole('button', { name: /mock-coder/ })).toBeVisible()
})

test('a thread works end to end: tool call edits a file', async () => {
  const { page, project } = L
  const pid = await addProject(page, project)
  expect(pid).toBeTruthy()
  // pick the project in the composer
  await page.getByRole('button', { name: 'No project', exact: true }).click()
  await page.getByRole('menuitem', { name: /^project/ }).click()
  await expect(page.getByRole('button', { name: 'project', exact: true }).last()).toBeVisible()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('Please create hello.txt for me (create hello)')
  await box.press('Enter')
  await expect(page.getByText('Done. I created')).toBeVisible()
  expect(fs.readFileSync(path.join(project, 'hello.txt'), 'utf8')).toBe('hello from odex\n')
  // the file change shows up as a diff summary
  await expect(page.getByText('hello.txt').first()).toBeVisible()
  // the mock received the tool result in the second request
  const reqs = await mock.requests()
  const withToolResult = reqs.filter((r) => (r.body?.messages ?? []).some((m: any) => m.role === 'tool' && String(m.content).includes('hello.txt')))
  expect(withToolResult.length).toBeGreaterThanOrEqual(1)
})

test('follow-up in the same thread and reasoning display', async () => {
  const { page } = L
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('think about it')
  await box.press('Enter')
  await expect(page.getByText('Here is my considered answer.')).toBeVisible()
})

test('escalated command asks for approval and runs after approve', async () => {
  const { page } = L
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('install deps please')
  await box.press('Enter')
  const card = page.getByRole('alertdialog')
  await expect(card).toBeVisible()
  await expect(card.getByText('echo installing')).toBeVisible()
  await card.getByRole('button', { name: 'Approve', exact: true }).click()
  await expect(page.getByText('Dependencies step finished.')).toBeVisible()
})

test('command palette and settings open', async () => {
  const { page } = L
  await page.keyboard.press('Control+K')
  const palette = page.getByRole('dialog', { name: 'Command palette' })
  await expect(palette).toBeVisible()
  await palette.getByRole('textbox').fill('Models')
  await page.keyboard.press('Enter')
  await expect(page.getByRole('heading', { name: 'Models & Endpoints' })).toBeVisible()
  await expect(page.getByText('Mock vLLM')).toBeVisible()
})
