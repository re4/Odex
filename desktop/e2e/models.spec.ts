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
      { when: { last_role: 'user', last_user_contains: 'paint a fox' }, reply: { kind: 'tool_calls', calls: [{ name: 'generate_image', arguments: { prompt: 'A red fox' } }] } },
      { when: { last_role: 'tool', last_user_contains: 'paint a fox' }, reply: { kind: 'text', text: 'Your fox is ready.' } },
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
  // imported workflows, ComfyUI's templates the server can run, then the ones saved in ComfyUI itself
  await expect(page.getByTestId('comfy-templates')).toContainText('ready on your ComfyUI')
  await expect(image.locator('option')).toHaveText([
    'Off',
    'flux',
    'Ideogram 4.0 (ComfyUI-Ideogram4 nodes)',
    'Ideogram v4 Int8: Text to Image',
    '1 more need a Comfy.org API key (ComfyUI section below)',
    '3d/image to mesh',
    'txt2img',
  ])
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

test('ComfyUI: use workflows saved in ComfyUI', async () => {
  const { page } = L
  await openPanel(page, 'Context')
  await openPanel(page, 'Models & Endpoints')
  const image = page.getByLabel('Image generation workflow')
  const model3d = page.getByLabel('3D generation workflow')
  await expect(image.locator('optgroup[label="Saved in ComfyUI"] option')).toHaveText(['3d/image to mesh', 'txt2img'])

  // picking one converts it (UI format → API format), finds its prompt box and assigns it
  await image.selectOption({ label: 'txt2img' })
  await expect(page.getByText('Imported txt2img from ComfyUI')).toBeVisible()
  await expect.poll(configText).toMatch(/image_workflow = "txt2img"/)
  await expect(image).toHaveValue('txt2img')
  await model3d.selectOption({ label: '3d/image to mesh' })
  await expect.poll(configText).toMatch(/model3d_workflow = "image to mesh"/)
  const row = (name: string) => page.locator('.row.small', { has: page.locator('b', { hasText: new RegExp(`^${name}$`) }) })
  await expect(row('txt2img')).toContainText('{{prompt}}')
  await expect(row('image to mesh')).toContainText('{{image}}')
  const saved = JSON.parse(fs.readFileSync(path.join(L.home, 'comfyui', 'txt2img.json'), 'utf8'))
  expect(saved['6'].inputs.text).toBe('{{prompt}}')
  expect(saved['3'].inputs.positive).toEqual(['6', 0])
  await page.getByRole('heading', { name: 'Roles' }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: path.join(SHOTS, 'comfyui-saved.png') })

  // and the agent generates with it
  await page.getByRole('button', { name: 'Back to app' }).click()
  const t = await rpc(page, 'thread/start', { cwd: L.project, name: 'Fox' })
  await page.evaluate((id) => (window as any).__odexStore.getState().selectThread(id), t.thread.id)
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('Please paint a fox')
  await box.press('Enter')
  await expect(page.getByText('Your fox is ready.')).toBeVisible()
  expect(fs.existsSync(path.join(L.project, 'generated', 'a-red-fox.png'))).toBe(true)
})

test('ComfyUI: remove an imported workflow', async () => {
  const { page } = L
  await openPanel(page, 'Context')
  await openPanel(page, 'Models & Endpoints')
  const model3d = page.getByLabel('3D generation workflow')
  await expect(model3d).toHaveValue('image to mesh')
  await page.getByRole('button', { name: 'Remove workflow image to mesh' }).click()
  const dialog = page.getByRole('dialog', { name: 'Remove workflow' })
  await expect(dialog).toContainText('3D generation goes back to Off')
  await dialog.getByRole('button', { name: 'Remove', exact: true }).click()
  await expect(page.locator('.row.small b', { hasText: /^image to mesh$/ })).toHaveCount(0)
  await expect(model3d).toHaveValue('')
  await expect.poll(configText).not.toMatch(/model3d_workflow/)
  expect(fs.existsSync(path.join(L.home, 'comfyui', 'image to mesh.json'))).toBe(false)
  // the other role keeps its workflow
  await expect(page.getByLabel('Image generation workflow')).toHaveValue('txt2img')
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
    await expect(page.getByTestId('comfy-saved')).toHaveText('2 workflows saved in ComfyUI: pick one under Image generation or 3D generation above.')
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

    // the Comfy.org key for partner nodes (Ideogram): encrypted the same way
    const org = page.getByLabel('Comfy.org API key', { exact: true })
    await org.fill('comfyui-org-key')
    await org.press('Enter')
    await expect(page.getByText('Comfy.org API key saved')).toBeVisible()
    await expect(org).toHaveAttribute('placeholder', /saved/)
    const stored = fs.readFileSync(path.join(l.home, 'secrets.json'), 'utf8')
    expect(stored).toContain('comfyui:comfy_org_api_key')
    expect(stored).not.toContain('comfyui-org-key')
    expect((await rpc(page, 'comfyui/status')).hasComfyOrgKey).toBe(true)

    await page.getByRole('button', { name: 'Remove ComfyUI API key' }).click()
    await expect(page.getByText('HTTP 401 Unauthorized: the server needs an API key')).toBeVisible()
  } finally {
    await l.close()
    m.stop()
  }
})

test('ComfyUI templates: the ones the server can run, picked in one step', async () => {
  const m = await startMock(
    [
      { when: { last_role: 'user', last_user_contains: 'dog image' }, reply: { kind: 'tool_calls', calls: [{ name: 'generate_image', arguments: { prompt: 'A happy dog' } }] } },
      { when: { last_role: 'tool', last_user_contains: 'dog image' }, reply: { kind: 'text', text: 'Here is your dog.' } },
      { when: {}, reply: { kind: 'text', text: 'OK' } },
    ],
    { comfy: true, comfyNoSaved: true },
  )
  const l = await launch({ mockUrl: m.url })
  try {
    const { page } = l
    await engineReady(page)
    await openPanel(page, 'Models & Endpoints')
    await page.getByLabel('Server URL').fill(m.comfyUrl!)
    await page.getByLabel('Server URL').press('Enter')
    await expect(page.getByText('Connected · ComfyUI 0.3.60-mock')).toBeVisible()
    await expect(page.getByTestId('comfy-saved')).toContainText('Nothing is saved in ComfyUI yet')
    const summary = page.getByTestId('comfy-templates')
    await expect(summary).toContainText('5 ready on your ComfyUI, 2 templates need models or nodes it doesn’t have.')

    // nothing saved: the dropdowns offer the library's templates the server has everything for
    const image = page.getByLabel('Image generation workflow')
    const model3d = page.getByLabel('3D generation workflow')
    await expect(image.locator('optgroup[label="Saved in ComfyUI"]')).toHaveCount(0)
    await expect(image.locator('optgroup[label="Ready on your ComfyUI"] option')).toHaveText(['Ideogram 4.0 (ComfyUI-Ideogram4 nodes)', 'Ideogram v4 Int8: Text to Image'])
    await expect(image.locator('optgroup[label="Comfy.org partners (credits)"] option')).toHaveText(['1 more need a Comfy.org API key (ComfyUI section below)'])
    await expect(model3d.locator('optgroup[label="Ready on your ComfyUI"] option')).toHaveText(['Pixal3D (ComfyUI_RH_Pixal3D nodes)', 'Pixal3D & TRELLIS.2: Image to Model'])
    await summary.getByText('What the other templates need').click()
    await expect(summary).toContainText('Flux.1 Dev: Text to Image: flux1-dev.safetensors')
    await expect(summary).toContainText('Tripo: Image to Model: node TripoImageToModelNode')
    await page.getByRole('heading', { name: 'Roles' }).scrollIntoViewIfNeeded()
    await page.screenshot({ path: path.join(SHOTS, 'comfyui-templates.png') })

    // the node packs' own workflows (what a server with ComfyUI-Ideogram4 and ComfyUI_RH_Pixal3D runs)
    await image.selectOption({ label: 'Ideogram 4.0 (ComfyUI-Ideogram4 nodes)' })
    await expect(page.getByText('Image generation now uses Ideogram 4.0 (ComfyUI-Ideogram4 nodes)')).toBeVisible()
    await expect(image).toHaveValue('Ideogram 4.0 (ComfyUI-Ideogram4 nodes)')
    await model3d.selectOption({ label: 'Pixal3D (ComfyUI_RH_Pixal3D nodes)' })
    await expect(model3d).toHaveValue('Pixal3D (ComfyUI_RH_Pixal3D nodes)')
    const config = () => fs.readFileSync(path.join(l.home, 'config.toml'), 'utf8')
    await expect.poll(config).toMatch(/image_workflow = "Ideogram 4.0 \(ComfyUI-Ideogram4 nodes\)"/)
    await expect.poll(config).toMatch(/model3d_workflow = "Pixal3D \(ComfyUI_RH_Pixal3D nodes\)"/)

    // with a Comfy.org key the partner templates show up too
    const org = page.getByLabel('Comfy.org API key', { exact: true })
    await org.fill('comfyui-org-key')
    await org.press('Enter')
    await expect(image.locator('optgroup[label="Comfy.org partners (credits)"] option')).toHaveText(['Ideogram v4: Text to Image (API)'])

    // and "generate me a dog image" runs Ideogram 4.0 on ComfyUI
    await page.getByRole('button', { name: 'Back to app' }).click()
    const t = await rpc(page, 'thread/start', { cwd: l.project, name: 'Dog' })
    await page.evaluate((id) => (window as any).__odexStore.getState().selectThread(id), t.thread.id)
    const box = page.getByRole('textbox', { name: 'Message' })
    await box.fill('generate me a dog image')
    await box.press('Enter')
    await expect(page.getByText('Here is your dog.')).toBeVisible()
    expect(fs.existsSync(path.join(l.project, 'generated', 'a-happy-dog.png'))).toBe(true)
  } finally {
    await l.close()
    m.stop()
  }
})
