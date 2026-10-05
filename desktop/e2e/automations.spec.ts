import { test, expect, type Page } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'
import { addProject, engineReady, launch, startMock, type Launched, type Mock } from './harness'

const REPLY = 'Automation report: all 3 checks passed.'
// Set ODEX_SHOTS=<dir> to save light/dark screenshots of both views.
const SHOTS = process.env.ODEX_SHOTS

let mock: Mock
let L: Launched

async function shot(page: Page, name: string): Promise<void> {
  if (!SHOTS) return
  fs.mkdirSync(SHOTS, { recursive: true })
  await page.waitForTimeout(250)
  await page.screenshot({ path: path.join(SHOTS, `${name}.png`) })
}

const nav = (page: Page, name: string) => page.locator('.sidebar').getByRole('button', { name: new RegExp(`^${name}`) })
const automationsBadge = (page: Page) => nav(page, 'Automations').locator('.badge')

/** Expand an automation card (its header button toggles the run history). */
async function expand(page: Page, name: string): Promise<void> {
  const header = page.getByRole('article', { name }).getByRole('button', { name })
  if ((await header.getAttribute('aria-expanded')) !== 'true') await header.click()
  await expect(header).toHaveAttribute('aria-expanded', 'true')
}

async function latestRunStatus(page: Page): Promise<string | undefined> {
  return page.evaluate(async () => {
    const r = await (window as any).odex.request('automation/runs', { unreadOnly: false, includeArchived: false, limit: 1 })
    return r.runs[0]?.status as string | undefined
  })
}

async function runNowAndWait(page: Page, runsBefore: number): Promise<void> {
  await nav(page, 'Automations').click()
  await page.getByRole('article', { name: 'Nightly check' }).getByRole('button', { name: 'Run now' }).click()
  await expect
    .poll(
      async () =>
        page.evaluate(async () => {
          const r = await (window as any).odex.request('automation/runs', { unreadOnly: false, includeArchived: true, limit: 50 })
          return r.runs.filter((x: any) => x.status !== 'running').length as number
        }),
      { timeout: 60_000 },
    )
    .toBe(runsBefore + 1)
  expect(await latestRunStatus(page)).toBe('completed')
  await expect(automationsBadge(page)).toHaveText('1')
}

test.beforeAll(async () => {
  mock = await startMock([
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Nightly check"}' } },
    { when: { last_role: 'user', last_user_contains: 'nightly check' }, reply: { kind: 'text', text: REPLY } },
    { when: {}, reply: { kind: 'text', text: 'OK.' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  await addProject(L.page, L.project)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('empty states explain where automations run', async () => {
  const { page } = L
  await nav(page, 'Automations').click()
  await expect(page.getByRole('heading', { name: 'No automations yet' })).toBeVisible()
  await expect(page.getByText(/run on this machine while Odex is open or running in the tray/).first()).toBeVisible()
  await shot(page, 'automations-empty-light')
  await nav(page, 'Activity').click()
  await expect(page.getByRole('heading', { name: /all caught up/ })).toBeVisible()
  await shot(page, 'activity-empty-light')
})

test('create an automation with a project target and a custom schedule', async () => {
  const { page } = L
  await nav(page, 'Automations').click()
  await page.getByRole('button', { name: 'New automation' }).first().click()
  const dlg = page.getByRole('dialog', { name: 'New automation' })
  await expect(dlg).toBeVisible()
  await dlg.getByLabel('Name').fill('Nightly check')
  await dlg.getByLabel('Prompt').fill('Run the nightly check and report the results.')
  await dlg.getByLabel('Project').selectOption({ label: 'project' })

  // the default preset previews too
  await expect(dlg.getByLabel('Schedule preview')).toContainText('Daily at 09:00')

  await dlg.getByRole('radio', { name: 'Custom' }).click()
  const cron = dlg.getByLabel('Cron expression')
  await expect(cron).toHaveValue('0 9 * * *')
  await cron.fill('every blue moon')
  await expect(dlg.getByRole('alert')).toContainText('unrecognized schedule')
  await expect(dlg.getByRole('button', { name: 'Create' })).toBeDisabled()

  await cron.fill('0 8 22 * *')
  const preview = dlg.getByLabel('Schedule preview')
  await expect(preview).toContainText('Monthly on the 22nd at 08:00')
  await expect(preview.getByRole('list', { name: 'Next runs' }).getByRole('listitem')).toHaveCount(3)
  // worktree is offered because the test project is a git repo
  await expect(dlg.getByLabel('Run in').locator('option[value="worktree"]')).toBeEnabled()
  await shot(page, 'automation-editor-light')
  if (SHOTS) {
    await dlg.locator('.modal-body').evaluate((el) => el.scrollTo(0, el.scrollHeight))
    await shot(page, 'automation-editor-bottom-light')
  }

  await dlg.getByRole('button', { name: 'Create' }).click()
  await expect(dlg).toBeHidden()
  const card = page.getByRole('article', { name: 'Nightly check' })
  await expect(card).toBeVisible()
  await expect(card).toContainText('Monthly on the 22nd at 08:00')
  await expect(card).toContainText('project')
  await expect(card).toContainText(/Next (in \d+d|on )/)
  await expect(card).toContainText('Never run')

  const saved = await page.evaluate(async () => (await (window as any).odex.request('automation/list', {})).automations)
  expect(saved).toHaveLength(1)
  expect(saved[0]).toMatchObject({ name: 'Nightly check', schedule: '0 8 22 * *', target: 'project', permissionMode: 'auto', runMode: 'local', enabled: true })
  expect(saved[0].id).toBeTruthy()

  // editing maps the stored schedule back into the editor
  await card.getByRole('button', { name: 'Edit' }).click()
  const edit = page.getByRole('dialog', { name: 'Edit automation' })
  await expect(edit.getByRole('radio', { name: 'Custom' })).toHaveAttribute('aria-checked', 'true')
  await expect(edit.getByLabel('Cron expression')).toHaveValue('0 8 22 * *')
  await edit.getByRole('button', { name: 'Cancel' }).click()
  await expect(edit).toBeHidden()
})

test('run now: the run lands in Activity, mark read clears the badge, the thread has the reply', async () => {
  const { page } = L
  await expect(automationsBadge(page)).toHaveCount(0)
  await runNowAndWait(page, 0)

  // the automation row shows the last run, and its history lists it
  const card = page.getByRole('article', { name: 'Nightly check' })
  await expect(card).toContainText(/Completed (just now|\dm ago)/)
  await expand(page, 'Nightly check')
  const history = card.getByRole('list', { name: 'Run history' })
  await expect(history.getByRole('listitem')).toHaveCount(1)
  await expect(history).toContainText(REPLY)
  await shot(page, 'automations-light')

  await nav(page, 'Activity').click()
  const runs = page.getByRole('region', { name: 'Automation runs' })
  const row = runs.getByRole('listitem', { name: 'Nightly check run' })
  await expect(row).toHaveCount(1)
  await expect(row).toContainText('Completed')
  await expect(row).toContainText(REPLY)
  await expect(row.getByLabel('Unread')).toBeVisible()
  // the automation's new thread finished in the background, so it is unread too
  await expect(page.getByRole('region', { name: 'Needs attention' })).toContainText('Nightly check')
  await shot(page, 'activity-light')

  // unread filter shows it; mark read clears the sidebar badge
  await runs.getByRole('tab', { name: 'Unread' }).click()
  await expect(row).toHaveCount(1)
  await row.getByRole('button', { name: 'Mark read' }).click()
  await expect(automationsBadge(page)).toHaveCount(0)
  await expect(row).toHaveCount(0)
  await expect(runs).toContainText('No unread runs.')
  await runs.getByRole('tab', { name: 'All' }).click()
  await expect(row).toHaveCount(1)
  await expect(row.getByLabel('Unread')).toHaveCount(0)

  // opening the run shows its thread with the mock's reply
  await row.getByRole('button', { name: 'Open thread' }).click()
  await expect(page.getByText(REPLY)).toBeVisible()
  await expect(page.locator('.titlebar .title')).toHaveText('Nightly check')

  const read = await page.evaluate(async () => (await (window as any).odex.request('automation/runs', { unreadOnly: true, includeArchived: false })).unreadCount)
  expect(read).toBe(0)
})

test('opening an unread run marks it read; archive moves it to Archived', async () => {
  const { page } = L
  await runNowAndWait(page, 1)
  await nav(page, 'Activity').click()
  const runs = page.getByRole('region', { name: 'Automation runs' })
  const rows = runs.getByRole('listitem', { name: 'Nightly check run' })
  await expect(rows).toHaveCount(2)
  // unread first
  await expect(rows.first().getByLabel('Unread')).toBeVisible()
  await rows.first().getByRole('button', { name: 'Open thread' }).click()
  await expect(page.getByText(REPLY)).toBeVisible()
  await expect(automationsBadge(page)).toHaveCount(0)

  await nav(page, 'Activity').click()
  await expect(rows).toHaveCount(2)
  await expect(runs.getByLabel('Unread')).toHaveCount(0)
  await rows.first().getByRole('button', { name: 'Archive' }).click()
  await expect(rows).toHaveCount(1)
  await runs.getByRole('tab', { name: 'Archived' }).click()
  await expect(rows).toHaveCount(1)
  await expect(rows.first()).toContainText('archived')
  await runs.getByRole('tab', { name: 'All' }).click()
})

test('dark theme screenshots', async () => {
  test.skip(!SHOTS, 'set ODEX_SHOTS to capture screenshots')
  const { page } = L
  await page.evaluate(async () => (window as any).odex.settings.set({ theme: 'dark' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  await nav(page, 'Activity').click()
  await shot(page, 'activity-dark')
  await nav(page, 'Automations').click()
  const card = page.getByRole('article', { name: 'Nightly check' })
  await expand(page, 'Nightly check')
  await shot(page, 'automations-dark')
  await card.getByRole('button', { name: 'Edit' }).click()
  const dlg = page.getByRole('dialog', { name: 'Edit automation' })
  await shot(page, 'automation-editor-dark')
  await dlg.getByRole('radio', { name: 'Wake an existing thread' }).click()
  await dlg.getByRole('radio', { name: 'Weekdays' }).click()
  await expect(dlg.getByLabel('Schedule preview')).toContainText('Weekdays at 09:00')
  await dlg.locator('.modal-body').evaluate((el) => el.scrollTo(0, el.scrollHeight))
  await shot(page, 'automation-editor-thread-dark')
  await page.keyboard.press('Escape')
  await page.evaluate(async () => (window as any).odex.settings.set({ theme: 'light' }))
})

test('pause and delete an automation', async () => {
  const { page } = L
  await nav(page, 'Automations').click()
  const card = page.getByRole('article', { name: 'Nightly check' })
  await card.getByRole('switch', { name: 'Enabled' }).click()
  await expect(card).toContainText('Paused')
  const a = await page.evaluate(async () => (await (window as any).odex.request('automation/list', {})).automations[0])
  expect(a.enabled).toBe(false)
  expect(a.nextRunAt ?? null).toBeNull()

  await card.getByRole('button', { name: 'Delete' }).click()
  const confirm = page.getByRole('dialog', { name: 'Delete automation' })
  await confirm.getByRole('button', { name: 'Delete' }).click()
  await expect(card).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'No automations yet' })).toBeVisible()
  // run history is kept in Activity
  await nav(page, 'Activity').click()
  await expect(page.getByRole('region', { name: 'Automation runs' }).getByRole('listitem', { name: 'Nightly check run' })).toHaveCount(1)
})
