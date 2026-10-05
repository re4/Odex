import { test, expect } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'
import { engineReady, launch, startMock, type Launched, type Mock } from './harness'

let mock: Mock
let L: Launched

test.beforeAll(async () => {
  mock = await startMock([{ when: {}, reply: { kind: 'text', text: 'Hi there.' } }], { models: ['mock-coder', 'mock-small'] })
  // no config.toml: the engine reports needsOnboarding
  L = await launch({ onboarded: false })
  await engineReady(L.page)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('first run walks through endpoint, models, doctor, permissions and project', async () => {
  const { page, home } = L
  const dialog = page.getByRole('dialog')
  await expect(dialog.getByText('Set up Odex')).toBeVisible()

  // endpoint: typed without /v1, normalized on test
  const url = dialog.getByLabel('Base URL')
  await url.fill(mock.url)
  await dialog.getByRole('button', { name: 'Test connection' }).click()
  await expect(dialog.getByText(/Connected/)).toBeVisible()
  await expect(url).toHaveValue(`${mock.url}/v1`)
  await expect(dialog.getByText('mock-small').first()).toBeVisible()
  await dialog.getByRole('button', { name: 'Continue' }).click()

  // roles
  await expect(dialog.getByLabel('Main model (coding agent)')).toBeVisible()
  await dialog.getByLabel(/^Utility model/).selectOption('local:mock-small')
  await dialog.getByRole('button', { name: 'Continue' }).click()

  // doctor
  await expect(dialog.getByText('Doctor checks')).toBeVisible()
  await expect(dialog.getByText('Running checks…')).toBeHidden({ timeout: 60_000 })
  await dialog.getByRole('button', { name: 'Continue' }).click()

  // permissions
  await dialog.getByText('Read only', { exact: true }).click()
  await dialog.getByRole('button', { name: 'Continue' }).click()

  // project step, then done
  await expect(dialog.getByRole('button', { name: /Add project folder/ })).toBeVisible()
  await dialog.getByRole('button', { name: 'Done' }).click()
  await expect(dialog).toBeHidden()

  const cfg = fs.readFileSync(path.join(home, 'config.toml'), 'utf8')
  expect(cfg).toContain(`base_url = "${mock.url}/v1"`)
  expect(cfg).toMatch(/main = "local:mock-coder"/)
  expect(cfg).toMatch(/utility = "local:mock-small"/)
  expect(cfg).toMatch(/permission_mode = "read-only"/)
  const desk = JSON.parse(fs.readFileSync(path.join(home, 'desktop.json'), 'utf8'))
  expect(desk.onboarded).toBe(true)

  // the composer now offers the configured model and a chat works without a project
  await expect(page.getByRole('button', { name: /mock-coder/ })).toBeVisible()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('hello')
  await box.press('Enter')
  await expect(page.getByText('Hi there.')).toBeVisible()
})
