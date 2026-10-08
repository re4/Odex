import { test, expect, type Page } from '@playwright/test'
import fs from 'node:fs'
import path from 'node:path'
import { desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

// Models & Endpoints: removing and restoring models, and the ComfyUI image /
// 3D generation roles end to end (mock vLLM + mock ComfyUI).

const SHOTS = path.join(desktopDir, 'test-results', process.env.ODEX_OUT || 'out', 'models-shots')

let mock: Mock
let L: Launched

const rpc = (page: Page, method: string, params: unknown = {}) => page.evaluate(([m, p]) => (window as any).odex.request(m, p), [method, params] as const) as Promise<any>
const configText = () => fs.readFileSync(path.join(L.home, 'config.toml'), 'utf8')

async function openPanel(page: Page, label: string): Promise<void> {
  const nav = page.getByRole('navigation', { name: 'Settings sections' })
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

const TXT2IMG = {
  '6': { class_type: 'CLIPTextEncode', inputs: { text: '{{prompt}}', clip: ['4', 1] } },
  '3': { class_type: 'KSampler', inputs: { seed: 1, model: ['4', 0] } },
  '9': { class_type: 'SaveImage', inputs: { filename_prefix: 'odex', images: ['8', 0] } },
}

test.beforeAll(async () => {
  mock = await startMock(
    [
      { when: { last_role: 'user', last_user_contains: 'draw a lighthouse' }, reply: { kind: 'tool_calls', calls: [{ name: 'generate_image', arguments: { prompt: 'A lighthouse at dusk' } }] } },
      { when: { last_role: 'tool', last_user_contains: 'draw a lighthouse' }, reply: { kind: 'text', text: 'Your lighthouse is ready.' } },
      { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Lighthouse"}' } },
      { when: {}, reply: { kind: 'text', text: 'OK' } },
    ],
    { models: ['mock-coder', 'mock-small'], comfy: true },
  )
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  fs.mkdirSync(SHOTS, { recursive: true })
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('remove a model, then restore it', async () => {
  const { page } = L
  await openPanel(page, 'Models & Endpoints')
  const row = page.locator('tr', { hasText: 'mock:mock-small' })
  await expect(row).toBeVisible()
  await page.getByRole('button', { name: 'Remove mock:mock-small' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Remove', exact: true }).click()
  await expect(row).toHaveCount(0)
  await expect.poll(configText).toMatch(/hidden_models = \["mock:mock-small"\]/)
  expect((await rpc(page, 'model/list')).models.map((m: any) => m.key)).toEqual(['mock:mock-coder'])
  await expect(page.getByLabel('Compactor model').locator('option', { hasText: 'mock-small' })).toHaveCount(0)
  await page.screenshot({ path: path.join(SHOTS, 'removed.png'), fullPage: true })

  await page.getByText('Removed models (1)').click()
  await page.getByRole('button', { name: 'Restore mock:mock-small' }).click()
  await expect(page.locator('tr', { hasText: 'mock:mock-small' })).toBeVisible()
  await expect.poll(configText).not.toMatch(/mock-small/)
})

test('ComfyUI: connect, pick a workflow, generate an image', async () => {
  const { page } = L
  const dir = path.join(L.home, 'comfyui')
  fs.mkdirSync(dir, { recursive: true })
  fs.writeFileSync(path.join(dir, 'flux.json'), JSON.stringify(TXT2IMG))
  fs.writeFileSync(path.join(dir, 'ui-export.json'), JSON.stringify({ nodes: [], links: [] }))
  await openPanel(page, 'Context')
  await openPanel(page, 'Models & Endpoints')

  const image = page.getByLabel('Image generation workflow')
  await expect(image).toBeDisabled()
  await page.getByLabel('Server URL').fill(mock.comfyUrl!)
  await page.getByLabel('Server URL').press('Enter')
  await expect(page.getByText('Connected · ComfyUI 0.3.60-mock')).toBeVisible()
  await expect.poll(configText).toContain(`url = "${mock.comfyUrl}"`)
  await expect(image).toBeEnabled()
  await expect(page.getByText(/UI-format workflow/)).toBeVisible()
  await expect(image.locator('option')).toHaveText(['Off', 'flux'])
  await image.selectOption('flux')
  await expect.poll(configText).toMatch(/image_workflow = "flux"/)
  await expect(page.locator('.badge', { hasText: 'Image generation' })).toBeVisible()
  await page.getByRole('heading', { name: 'ComfyUI' }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: path.join(SHOTS, 'comfyui.png'), fullPage: true })

  // the agent gets generate_image and the result shows in the thread
  // (a thread in the temp project: chats without one run in the real home folder)
  await page.getByRole('button', { name: 'Back to app' }).click()
  const t = await rpc(page, 'thread/start', { cwd: L.project, name: 'Lighthouse' })
  await page.evaluate((id) => (window as any).__odexStore.getState().selectThread(id), t.thread.id)
  const box = page.getByRole('textbox', { name: 'Message' })
  await expect(box).toBeVisible()
  await box.fill('Please draw a lighthouse')
  await box.press('Enter')
  await expect(page.getByText('Your lighthouse is ready.')).toBeVisible()
  await expect(page.getByText('Generated image generated/a-lighthouse-at-dusk.png')).toBeVisible()
  const reqs = await mock.requests()
  expect(reqs.some((r: any) => (r.body?.tools ?? []).some((t: any) => t.function.name === 'generate_image'))).toBe(true)
  const toolTexts = reqs.flatMap((r: any) => (r.body?.messages ?? []).filter((m: any) => m.role === 'tool').map((m: any) => JSON.stringify(m.content)))
  expect(toolTexts.some((t: string) => t.includes('saved generated/a-lighthouse-at-dusk.png'))).toBe(true)
  expect(fs.readFileSync(path.join(L.project, 'generated', 'a-lighthouse-at-dusk.png')).subarray(0, 4).toString('latin1')).toBe('\x89PNG')
  await page.screenshot({ path: path.join(SHOTS, 'generated.png') })
})

test('ComfyUI behind an auth proxy: the API key is stored encrypted and sent', async () => {
  const m = await startMock([{ when: {}, reply: { kind: 'text', text: 'OK' } }], { comfy: true, comfyApiKey: 's3cret-key' })
  const l = await launch({ mockUrl: m.url })
  try {
    const { page } = l
    await engineReady(page)
    await openPanel(page, 'Models & Endpoints')
    await page.getByLabel('Server URL').fill(m.comfyUrl!)
    await page.getByLabel('Server URL').press('Enter')
    await expect(page.getByText('HTTP 401 Unauthorized: the server needs an API key')).toBeVisible()

    const key = page.getByLabel('API key', { exact: true })
    await key.fill('s3cret-key')
    await key.press('Enter')
    await expect(page.getByText('Connected · ComfyUI 0.3.60-mock')).toBeVisible()
    await expect(key).toHaveAttribute('placeholder', /saved/)
    await page.getByRole('heading', { name: 'ComfyUI' }).scrollIntoViewIfNeeded()
    await page.screenshot({ path: path.join(SHOTS, 'comfyui-api-key.png') })
    // encrypted in secrets.json, never in config.toml
    expect(fs.readFileSync(path.join(l.home, 'config.toml'), 'utf8')).not.toContain('s3cret-key')
    const secrets = fs.readFileSync(path.join(l.home, 'secrets.json'), 'utf8')
    expect(secrets).toContain('comfyui:api_key')
    expect(secrets).not.toContain('s3cret-key')

    // another header: the mock wants Bearer, so the key is rejected there
    await page.getByLabel('Key header').fill('X-API-Key')
    await page.getByLabel('Key header').press('Enter')
    await expect(page.getByText('HTTP 401 Unauthorized: the server rejected the API key')).toBeVisible()
    await expect.poll(() => fs.readFileSync(path.join(l.home, 'config.toml'), 'utf8')).toContain('api_key_header = "X-API-Key"')
    await page.getByLabel('Key header').fill('')
    await page.getByLabel('Key header').press('Enter')
    await expect(page.getByText('Connected · ComfyUI 0.3.60-mock')).toBeVisible()

    await page.getByRole('button', { name: 'Remove ComfyUI API key' }).click()
    await expect(page.getByText('HTTP 401 Unauthorized: the server needs an API key')).toBeVisible()
  } finally {
    await l.close()
    m.stop()
  }
})
