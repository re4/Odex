import { test, expect } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'
import { addProject, engineReady, launch, startMock, type Launched, type Mock } from './harness'

let mock: Mock
let L: Launched
let projectId = ''

test.beforeAll(async () => {
  mock = await startMock([
    { when: { last_role: 'user', last_user_contains: 'apples' }, reply: { kind: 'text', text: 'Apples are red or green.' } },
    { when: { last_role: 'user', last_user_contains: 'pears' }, reply: { kind: 'text', text: 'Pears are often green.' } },
    { when: {}, reply: { kind: 'text', text: 'Fruit noted.' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  projectId = await addProject(L.page, L.project)
  await L.page.getByRole('heading', { name: /What should we/ }).waitFor()
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

async function startThread(text: string, reply: string) {
  const { page } = L
  await page.getByRole('button', { name: 'New thread' }).first().click()
  const chip = page.getByRole('button', { name: 'No project', exact: true })
  if (await chip.isVisible()) {
    await chip.click()
    await page.getByRole('menuitem', { name: /^project/ }).click()
  }
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill(text)
  await box.press('Enter')
  await expect(page.getByText(reply)).toBeVisible()
}

test('!cmd runs a user shell command in the thread', async () => {
  const { page } = L
  await startThread('tell me about apples', 'Apples are red or green.')
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill(process.platform === 'win32' ? '!Write-Output ("bang-" + (40+2))' : '!echo bang-$((40+2))')
  await box.press('Enter')
  await expect(page.getByText('bang-42').first()).toBeVisible()
})

test('edit and resend replaces the message and later turns', async () => {
  const { page } = L
  const bubble = page.locator('.user-bubble', { hasText: 'tell me about apples' })
  await bubble.hover()
  await page.getByRole('button', { name: 'Edit and resend' }).first().click()
  const editor = page.getByRole('textbox', { name: 'Edit message' })
  await editor.fill('tell me about pears')
  await page.getByRole('button', { name: 'Resend' }).click()
  await expect(page.getByText('Pears are often green.')).toBeVisible()
  await expect(page.getByText('Apples are red or green.')).toHaveCount(0)
  await expect(page.locator('.user-bubble', { hasText: 'apples' })).toHaveCount(0)
})

test('find in thread counts matches and steps through them', async () => {
  const { page } = L
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('more about pears please')
  await box.press('Enter')
  await expect(page.getByText('Pears are often green.')).toHaveCount(2)
  await page.locator('.thread-scroll').click({ position: { x: 5, y: 5 } })
  await page.keyboard.press('Control+F')
  const find = page.getByRole('textbox', { name: 'Find in thread' })
  await find.fill('pears')
  await expect(page.getByText(/1 of \d+/)).toBeVisible()
  const total = Number((await page.getByText(/1 of \d+/).textContent())!.split(' of ')[1])
  expect(total).toBeGreaterThanOrEqual(4)
  await find.press('Enter')
  await expect(page.getByText(`2 of ${total}`)).toBeVisible()
  await find.press('Shift+Enter')
  await expect(page.getByText(`1 of ${total}`)).toBeVisible()
  await find.fill('zzz-nothing')
  await expect(page.getByText('No results')).toBeVisible()
  await find.press('Escape')
  await expect(find).toBeHidden()
})

test('project actions run in the terminal', async () => {
  const { page, project } = L
  await page.evaluate(
    async ({ id, command }) => {
      await (window as any).odex.request('project/update', {
        id,
        actions: [{ id: 'act_hello', name: 'Say hello', command, icon: 'play', cwd: null, openUrl: null }],
      })
    },
    { id: projectId, command: process.platform === 'win32' ? 'Write-Output action-ran-ok' : 'echo action-ran-ok' },
  )
  expect(fs.existsSync(path.join(project, '.odex', 'actions.toml'))).toBe(true)
  const btn = page.getByRole('button', { name: 'Say hello' })
  await expect(btn).toBeVisible()
  await btn.click()
  await expect(page.locator('.bottom-panel .xterm-rows')).toContainText('action-ran-ok', { timeout: 20_000 })
  await page.getByRole('button', { name: 'Close panel' }).click()
})

test('archive can be undone with Ctrl+Z', async () => {
  const { page } = L
  const title = (await page.locator('.thread-header .name').textContent())!.trim()
  await page.locator('.thread-scroll').click({ position: { x: 5, y: 5 } })
  await page.keyboard.press('Control+Shift+A')
  await expect(page.getByRole('heading', { name: /What should we/ })).toBeVisible()
  await expect(page.locator('.thread-row', { hasText: title })).toHaveCount(0)
  await page.locator('.home').click({ position: { x: 5, y: 5 } })
  await page.keyboard.press('Control+Z')
  await expect(page.locator('.thread-header .name')).toHaveText(title)
  await expect(page.locator('.thread-row', { hasText: title })).toHaveCount(1)
})

test('odex://threads/new deep link prefills the composer without sending', async () => {
  const { app, page } = L
  const before = (await mock.requests()).length
  await app.evaluate(({ BrowserWindow }, url) => BrowserWindow.getAllWindows()[0].webContents.send('odex:deeplink', url), `odex://threads/new?prompt=${encodeURIComponent('Draft from a link')}`)
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveValue('Draft from a link')
  await page.waitForTimeout(500)
  expect((await mock.requests()).length).toBe(before)
})
