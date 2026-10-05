import { test, expect } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'
import { addProject, engineReady, launch, startMock, type Launched, type Mock } from './harness'

let mock: Mock
let L: Launched

const PLAN = '## Plan\n\n1. Create `notes.md` with a heading\n2. Report back\n'

test.beforeAll(async () => {
  mock = await startMock([
    // planning turn (plan-mode system prompt)
    { when: { last_role: 'user', system_contains: 'Planning mode' }, reply: { kind: 'text', text: PLAN } },
    // execution turn after approval
    { when: { last_role: 'user', any_contains: 'notes.md' }, reply: { kind: 'tool_calls', calls: [{ name: 'write_file', arguments: { path: 'notes.md', content: '# Notes\n' } }] } },
    { when: { last_role: 'tool' }, reply: { kind: 'text', text: 'Plan executed.' } },
    { when: {}, reply: { kind: 'text', text: 'OK' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  await addProject(L.page, L.project)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('plan mode proposes a plan; approving runs it', async () => {
  const { page, project } = L
  await page.getByRole('button', { name: 'No project', exact: true }).click()
  await page.getByRole('menuitem', { name: /^project/ }).click()
  await page.getByRole('button', { name: 'Plan', exact: true }).click()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('Add a notes file')
  await box.press('Enter')
  await expect(page.getByText('Proposed plan')).toBeVisible()
  await expect(page.getByText('Create notes.md with a heading')).toBeVisible()
  // the plan turn must not have written anything
  expect(fs.existsSync(path.join(project, 'notes.md'))).toBe(false)
  await page.getByRole('button', { name: 'Approve and run' }).click()
  await expect(page.getByText('Plan executed.')).toBeVisible()
  await expect(page.getByText('approved', { exact: true })).toBeVisible()
  expect(fs.readFileSync(path.join(project, 'notes.md'), 'utf8')).toBe('# Notes\n')
})

test('Shift+Tab toggles plan mode in the composer', async () => {
  const { page } = L
  const plan = page.getByRole('button', { name: 'Plan', exact: true })
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.focus()
  const before = await plan.getAttribute('aria-pressed')
  await page.keyboard.press('Shift+Tab')
  await expect(plan).toHaveAttribute('aria-pressed', before === 'true' ? 'false' : 'true')
})
