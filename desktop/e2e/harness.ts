import { _electron as electron, type ElectronApplication, type Page } from '@playwright/test'
import { spawn, execFileSync, type ChildProcess } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
export const desktopDir = path.resolve(here, '..')
export const repoDir = path.resolve(desktopDir, '..')
const exe = process.platform === 'win32' ? '.exe' : ''

function binary(name: string): string {
  for (const profile of ['debug', 'release']) {
    const p = path.join(repoDir, 'engine', 'target', profile, `${name}${exe}`)
    if (fs.existsSync(p)) return p
  }
  throw new Error(`${name} not built: run \`cargo build -p odex-engine -p odex-mock-vllm\` in engine/`)
}

export interface MockRule {
  when?: Record<string, unknown>
  reply: Record<string, unknown>
  times?: number
}

export interface Mock {
  url: string
  /** Mock ComfyUI root, when started with `{ comfy: true }`. */
  comfyUrl?: string
  proc: ChildProcess
  requests(): Promise<any[]>
  stop(): void
}

/** Start `odex-mock-vllm` on a free port with a rule policy (plus a mock ComfyUI with `comfy`). */
export async function startMock(rules: MockRule[], opts: { maxModelLen?: number; models?: string[]; comfy?: boolean; comfyApiKey?: string; comfyNoSaved?: boolean } = {}): Promise<Mock> {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'odex-mock-'))
  const rulesPath = path.join(dir, 'rules.json')
  fs.writeFileSync(rulesPath, JSON.stringify({ rules }))
  const args = ['--port', '0', '--rules', rulesPath, '--max-model-len', String(opts.maxModelLen ?? 32768), '--delay-ms', '1']
  for (const m of opts.models ?? ['mock-coder']) args.push('--model', m)
  if (opts.comfy) args.push('--comfy-port', '0')
  if (opts.comfyApiKey) args.push('--comfy-api-key', opts.comfyApiKey)
  if (opts.comfyNoSaved) args.push('--comfy-no-saved')
  const proc = spawn(binary('odex-mock-vllm'), args, { stdio: ['ignore', 'pipe', 'pipe'] })
  const [url, comfyUrl] = await new Promise<[string, string | undefined]>((resolve, reject) => {
    let buf = ''
    const t = setTimeout(() => reject(new Error(`mock did not start: ${buf}`)), 15_000)
    proc.stdout!.on('data', (d) => {
      buf += String(d)
      const m = /vllm listening on (\S+)/.exec(buf)
      const c = /comfyui listening on (\S+)/.exec(buf)
      if (m && (c || !opts.comfy)) {
        clearTimeout(t)
        resolve([m[1].replace(/\/$/, '').replace(/\/v1$/, ''), c?.[1]])
      }
    })
    proc.on('exit', (c) => reject(new Error(`mock exited ${c}: ${buf}`)))
  })
  return {
    url,
    comfyUrl,
    proc,
    requests: async () => (await (await fetch(`${url}/__mock/requests`)).json()) as any[],
    stop: () => proc.kill(),
  }
}

export interface Launched {
  app: ElectronApplication
  page: Page
  home: string
  project: string
  close(): Promise<void>
}

/** Launch the built app with an isolated ~/.odex and profile. */
export async function launch(
  opts: { mockUrl?: string; onboarded?: boolean; theme?: 'light' | 'dark'; git?: boolean; settings?: Record<string, unknown>; env?: Record<string, string> } = {},
): Promise<Launched> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'odex-e2e-'))
  const home = path.join(root, 'home')
  const project = path.join(root, 'project')
  fs.mkdirSync(home, { recursive: true })
  fs.mkdirSync(project, { recursive: true })
  fs.writeFileSync(path.join(project, 'README.md'), '# Demo project\n\nA tiny project for Odex tests.\n')
  fs.writeFileSync(path.join(project, 'main.py'), 'def add(a, b):\n    return a + b\n\nprint(add(2, 3))\n')
  if (opts.git !== false) {
    try {
      execFileSync('git', ['init', '-q', '-b', 'main'], { cwd: project })
      execFileSync('git', ['-c', 'user.email=e2e@odex.test', '-c', 'user.name=e2e', 'add', '.'], { cwd: project })
      execFileSync('git', ['-c', 'user.email=e2e@odex.test', '-c', 'user.name=e2e', 'commit', '-q', '-m', 'init'], { cwd: project })
    } catch {
      /* git missing: tests that need it will fail loudly */
    }
  }
  if (opts.mockUrl) {
    fs.writeFileSync(
      path.join(home, 'config.toml'),
      [
        'permission_mode = "auto"',
        '',
        '[model_providers.mock]',
        'name = "Mock vLLM"',
        `base_url = "${opts.mockUrl}/v1"`,
        '',
        '[roles]',
        'main = "mock:mock-coder"',
        '',
        '[features]',
        'follow_up_suggestions = false',
        '',
      ].join('\n'),
    )
  }
  fs.writeFileSync(
    path.join(home, 'desktop.json'),
    JSON.stringify({ onboarded: opts.onboarded ?? true, theme: opts.theme ?? 'light', notifyTurnComplete: 'never', notifyApprovals: false, keepAwake: false, keepRunningInTray: false, reducedMotion: 'on', ...opts.settings }),
  )
  const app = await electron.launch({
    args: [path.join(desktopDir, process.env.ODEX_OUT || 'out', 'main', 'index.js')],
    env: {
      ...process.env,
      ODEX_HOME: home,
      ODEX_USER_DATA: path.join(root, 'userdata'),
      ODEX_ENGINE_PATH: process.env.ODEX_ENGINE_BIN || binary('odex-engine'),
      ODEX_E2E: '1',
      ODEX_LOG: 'warn',
      ...opts.env,
    } as Record<string, string>,
  })
  const page = await app.firstWindow()
  await page.setViewportSize({ width: 1280, height: 820 }).catch(() => {})
  await page.waitForFunction(() => (window as any).odex !== undefined)
  return {
    app,
    page,
    home,
    project,
    close: async () => {
      await app.close().catch(() => {})
      try {
        fs.rmSync(root, { recursive: true, force: true, maxRetries: 3 })
      } catch {
        /* the engine may still hold the sqlite file briefly on Windows */
      }
    },
  }
}

/** Wait until the engine reports ready. */
export async function engineReady(page: Page): Promise<void> {
  await page.waitForFunction(async () => (await (window as any).odex.engineInfo()).state === 'ready', null, { timeout: 30_000 })
}

/** Register the test project through the engine (no native folder dialog). */
export async function addProject(page: Page, folder: string): Promise<string> {
  return page.evaluate(async (f) => {
    const w = window as any
    await w.odex.request('trust/set', { path: f, trusted: true })
    const r = await w.odex.request('project/add', { folders: [f], create: false })
    return r.project.id as string
  }, folder)
}
