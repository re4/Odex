import { test, expect, type Locator, type Page } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import path from 'node:path'
import { addProject, desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

/**
 * Settings → Git / Code review, the GitHub token, and the pull-request flows (badge, inbox, review,
 * Fix on a failing check) against a small fake of the GitHub REST API (`ODEX_GITHUB_API`).
 */

const SHOTS = process.env.ODEX_SHOTS_DIR || path.join(desktopDir, 'test-results', 'git-pr-shots')
const TOKEN = 'ghp_e2eTokenThatMustNeverBeLogged0123'
const RULE = 'REVIEW-RULE-42: flag every TODO comment'
const LOG_ERR = '##[error]AssertionError: add(2, 3) returned 5, expected 6'
const NEW_MAIN = 'def add(a, b):\n    # TODO: validate inputs\n    return a + b\n\nprint(add(2, 3))\n'
const PR_DIFF =
  'diff --git a/main.py b/main.py\nindex 1111111..2222222 100644\n--- a/main.py\n+++ b/main.py\n@@ -1,4 +1,5 @@\n def add(a, b):\n+    # TODO: validate inputs\n     return a + b\n \n print(add(2, 3))\n'

// ------------------------------------------------------------------ fake GitHub

interface Hit {
  method: string
  path: string
  query: string
  auth?: string
  body: any
}
const hits: Hit[] = []
let gh: http.Server

function prJson(n: number) {
  const seven = n === 7
  return {
    number: n,
    title: seven ? 'Validate add() inputs' : 'Docs tweak',
    body: seven ? 'Adds a **TODO** before validating.' : '',
    state: 'open',
    draft: !seven,
    merged: false,
    merged_at: null,
    html_url: `https://github.com/o/r/pull/${n}`,
    user: { login: seven ? 'alice' : 'bob' },
    head: { ref: seven ? 'feature' : 'docs', sha: seven ? 'abc123' : 'def456' },
    base: { ref: 'main' },
    additions: 1,
    deletions: 0,
    created_at: '2026-10-01T10:00:00Z',
    updated_at: seven ? '2026-10-03T10:00:00Z' : '2026-09-01T10:00:00Z',
  }
}

function startFakeGithub(): Promise<string> {
  gh = http.createServer((req, res) => {
    const u = new URL(req.url ?? '/', 'http://fake')
    let data = ''
    req.on('data', (c) => (data += c))
    req.on('end', () => {
      let body: any = null
      try {
        body = JSON.parse(data)
      } catch {
        /* not json */
      }
      hits.push({ method: req.method ?? '', path: u.pathname, query: u.search, auth: req.headers.authorization, body })
      const json = (v: unknown, code = 200) => {
        res.writeHead(code, { 'content-type': 'application/json' })
        res.end(JSON.stringify(v))
      }
      const text = (t: string) => {
        res.writeHead(200, { 'content-type': 'text/plain' })
        res.end(t)
      }
      if (!u.pathname.startsWith('/repos/o/r')) return json({ message: 'Not Found' }, 404)
      const p = u.pathname.slice('/repos/o/r'.length)
      const get = req.method === 'GET'
      const accept = String(req.headers.accept ?? '')
      if (get && p === '') return json({ default_branch: 'main' })
      if (get && p === '/pulls') {
        const head = u.searchParams.get('head')
        if (head) return json(head === 'o:feature' ? [prJson(7)] : [])
        return json([prJson(3), prJson(7)])
      }
      if (get && (p === '/pulls/7' || p === '/pulls/3')) {
        const n = Number(p.split('/').pop())
        return accept.includes('diff') ? text(n === 7 ? PR_DIFF : '') : json(prJson(n))
      }
      if (get && p === '/pulls/7/comments')
        return json([{ id: 701, user: { login: 'carol' }, body: 'Why a TODO here?', path: 'main.py', line: 2, side: 'RIGHT', created_at: '2026-10-02T09:00:00Z' }])
      if (get && p === '/commits/abc123/check-runs')
        return json({
          check_runs: [
            { id: 55, name: 'test', status: 'completed', conclusion: 'failure', details_url: 'https://github.com/o/r/actions/runs/5/job/55' },
            { id: 56, name: 'lint', status: 'completed', conclusion: 'success', details_url: 'https://github.com/o/r/actions/runs/5/job/56' },
          ],
        })
      if (get && p.endsWith('/status')) return json({ statuses: [] })
      if (get && p === '/check-runs/55') return json({ id: 55, output: { title: '1 test failed', summary: 'tests/test_math.py::test_add failed' } })
      if (get && p === '/actions/jobs/55/logs') {
        const lines = Array.from({ length: 600 }, (_, i) => `2026-10-03T10:00:00.0000000Z collected item ${i}`)
        return text([...lines, `2026-10-03T10:00:01.0000000Z ${LOG_ERR}`].join('\n'))
      }
      if (req.method === 'POST' && p === '/pulls/7/reviews') return json({ id: 9001, html_url: 'https://github.com/o/r/pull/7#pullrequestreview-9001' })
      if (get && (p.startsWith('/issues/') || p.startsWith('/pulls/') || p.startsWith('/commits/'))) return json([])
      return json({ message: 'Not Found' }, 404)
    })
  })
  return new Promise((resolve) => gh.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${(gh.address() as AddressInfo).port}`)))
}

// ------------------------------------------------------------------ setup

let mock: Mock
let L: Launched
let panel: Locator
let projectId = ''
let prThreadId = ''

function git(args: string[], cwd: string): string {
  return execFileSync('git', args, { cwd, encoding: 'utf8' })
}

function config(): string {
  return fs.readFileSync(path.join(L.home, 'config.toml'), 'utf8')
}

async function req<T = any>(page: Page, method: string, params: unknown): Promise<T> {
  return page.evaluate(([m, p]) => (window as any).odex.request(m, p), [method, params] as const) as Promise<T>
}

async function shot(name: string, target: Page | Locator = L.page): Promise<void> {
  await target.screenshot({ path: path.join(SHOTS, `${name}.png`) })
}

async function setTheme(theme: 'light' | 'dark'): Promise<void> {
  await L.page.evaluate((t) => (window as any).odex.settings.set({ theme: t }), theme)
  await expect(L.page.locator('html')).toHaveAttribute('data-theme', theme)
  await L.page.waitForTimeout(250)
}

async function openSettingsPanel(label: string): Promise<void> {
  const page = L.page
  if (!(await page.getByRole('heading', { name: label, exact: true }).isVisible().catch(() => false))) {
    if (!(await page.locator('.settings-nav, [aria-label="Settings sections"]').first().isVisible().catch(() => false))) await page.getByRole('button', { name: 'Settings' }).first().click()
    await page.getByRole('button', { name: label, exact: true }).click()
  }
}

async function openThread(title: string): Promise<void> {
  await L.page.locator('.thread-row', { hasText: title }).first().click()
  await expect(L.page.locator('.thread-header')).toContainText(title)
  if (!(await panel.isVisible())) await L.page.getByRole('button', { name: 'Toggle side panel' }).click()
}

test.beforeAll(async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  const ghUrl = await startFakeGithub()
  mock = await startMock([
    { when: { system_contains: 'You are reviewing code changes' }, reply: { kind: 'text', text: JSON.stringify({ summary: 'Looks fine.', overall_correctness: 'correct', findings: [] }) } },
    { when: { last_user_contains: 'The CI check' }, reply: { kind: 'text', text: 'I will fix the failing test.' } },
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Thread"}' } },
    { when: {}, reply: { kind: 'text', text: 'OK.' } },
  ])
  process.env.ODEX_GITHUB_API = ghUrl
  try {
    L = await launch({ mockUrl: mock.url })
  } finally {
    delete process.env.ODEX_GITHUB_API
  }
  await engineReady(L.page)
  panel = L.page.getByRole('complementary', { name: 'Side panel' })
  const { project } = L
  // a GitHub origin (served by the fake API) and a local bare remote for real pushes
  const bare = path.join(path.dirname(project), 'backup.git')
  git(['init', '-q', '--bare', bare], project)
  git(['remote', 'add', 'origin', 'https://github.com/o/r.git'], project)
  git(['remote', 'add', 'backup', bare], project)
  git(['checkout', '-q', '-b', 'feature'], project)
  projectId = await addProject(L.page, project)
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
  gh?.close()
})

test('Settings → Git: the branch prefix names new worktree branches', async () => {
  const { page } = L
  await openSettingsPanel('Git')
  const prefix = page.getByRole('textbox', { name: 'Branch prefix' })
  await expect(prefix).toHaveValue('odex/')
  await prefix.fill('team/')
  await prefix.press('Enter')
  await expect.poll(config).toMatch(/\[git\][^[]*branch_prefix = "team\/"/)
  await shot('settings-git-light')
  await setTheme('dark')
  await shot('settings-git-dark')
  await setTheme('light')

  const r = await req(page, 'thread/start', { projectId, runMode: 'worktree', name: 'Prefix check' })
  expect(r.thread.worktree.branch).toMatch(/^team\/prefix-check-/)
  expect(git(['branch', '--list', 'team/*'], L.project)).toContain('team/prefix-check-')
})

test('force push is refused until Settings → Git allows it', async () => {
  const { page, project } = L
  const push = (force: boolean) =>
    page.evaluate(
      async ([cwd, f]) => {
        try {
          return await (window as any).odex.request('git/push', { cwd, remote: 'backup', branch: 'feature', setUpstream: false, forceWithLease: f })
        } catch (e) {
          return { ok: false, output: `refused: ${(e as Error).message}` }
        }
      },
      [project, force] as const,
    )
  const refused = await push(true)
  expect(refused.output).toContain('force push is disabled')
  expect((await push(false)).ok).toBe(true)

  const toggle = page.getByRole('switch', { name: 'Allow force push' })
  await expect(toggle).toHaveAttribute('aria-checked', 'false')
  await toggle.click()
  await expect(toggle).toHaveAttribute('aria-checked', 'true')
  await expect.poll(config).toMatch(/allow_force_push = true/)
  const forced = await push(true)
  expect(forced.ok, forced.output).toBe(true)
})

test('Settings → Code review: the guidelines reach the review request', async () => {
  const { page, project } = L
  await openSettingsPanel('Code review')
  const box = page.getByRole('textbox', { name: 'Review instructions' })
  await box.fill(RULE)
  await box.blur()
  await expect.poll(config).toContain('REVIEW-RULE-42')
  await expect(page.getByRole('combobox', { name: 'Reviewer model' })).toBeVisible()
  await expect(page.getByRole('combobox', { name: 'Review delivery' })).toHaveValue('inline')
  await shot('settings-code-review-light')
  await setTheme('dark')
  await shot('settings-code-review-dark')
  await setTheme('light')

  fs.writeFileSync(path.join(project, 'main.py'), NEW_MAIN)
  const t = await req(page, 'thread/start', { projectId, name: 'PR thread' })
  prThreadId = t.thread.id
  const before = (await mock.requests()).length
  await req(page, 'review/start', { threadId: prThreadId, target: { type: 'uncommitted' } })
  const text = (m: any) => (typeof m.content === 'string' ? m.content : JSON.stringify(m.content))
  await expect
    .poll(async () => {
      const reqs = (await mock.requests()).slice(before)
      const review = reqs.find((r) => (r.body?.messages ?? []).some((m: any) => m.role === 'system' && text(m).includes('You are reviewing code changes')))
      return review ? (review.body.messages as any[]).filter((m) => m.role === 'user').some((m) => text(m).includes(RULE)) : null
    })
    .toBe(true)
})

test('the GitHub token is stored encrypted and used without a restart', async () => {
  const { page, home, project } = L
  await openSettingsPanel('Git')
  await page.getByRole('textbox', { name: 'GitHub token' }).fill(TOKEN)
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByText('A token is stored, encrypted by the operating system.', { exact: false })).toBeVisible()
  expect(await page.evaluate(() => (window as any).odex.secrets.has('github:token'))).toBe(true)
  expect(config()).not.toContain(TOKEN)
  const secrets = fs.readFileSync(path.join(home, 'secrets.json'), 'utf8')
  expect(secrets).toContain('github:token')
  expect(secrets).not.toContain(TOKEN)

  hits.length = 0
  const r = await req(page, 'pr/list', { cwd: project })
  expect(r.error ?? null).toBeNull()
  expect(r.prs.map((p: any) => p.number)).toEqual([7, 3])
  const list = hits.find((h) => h.path === '/repos/o/r/pulls')
  expect(list?.auth).toBe(`Bearer ${TOKEN}`)
  const log = path.join(home, 'logs', 'engine.log')
  if (fs.existsSync(log)) expect(fs.readFileSync(log, 'utf8')).not.toContain(TOKEN)
})

test('PR badge on the thread, inbox, and Fix on a failing check', async () => {
  const { page } = L
  await page.getByRole('button', { name: 'Back to app' }).click().catch(() => {})
  await openThread('PR thread')
  await panel.getByRole('tab', { name: 'Git' }).click()
  const card = panel.getByLabel('Pull request', { exact: true })
  await expect(card.getByText('#7 Validate add() inputs')).toBeVisible()
  await expect(card.getByText('Checks: 1 passed, 1 failed')).toBeVisible()

  // the summary is stored on the thread: sidebar row and thread header badges
  const row = page.locator('.thread-row', { hasText: 'PR thread' })
  await expect(row.locator('.pr-badge.failing')).toContainText('#7')
  await expect(page.locator('.thread-header .pr-badge')).toContainText('open')
  const read = await req(page, 'thread/read', { threadId: prThreadId })
  expect(read.thread.pr).toMatchObject({ number: 7, state: 'open', checks: 'failure', failedChecks: 1 })

  // inbox: open PRs, most recently updated first
  const inbox = panel.getByLabel('Pull requests', { exact: true })
  await expect(inbox.locator('.gp-inbox-item')).toHaveCount(2)
  await expect(inbox.locator('.gp-inbox-item').first()).toContainText('#7')
  await expect(inbox.locator('.gp-inbox-item').first()).toContainText('this branch')
  await shot('git-pr-light')
  await shot('git-pr-panel-light', panel)
  await setTheme('dark')
  await shot('git-pr-panel-dark', panel)
  await shot('git-pr-sidebar-dark', page.locator('.thread-row', { hasText: 'PR thread' }))
  await setTheme('light')

  // Fix: the failing check's log goes to the agent
  const before = (await mock.requests()).length
  await card.getByRole('button', { name: 'Fix test' }).click()
  await expect(page.getByText('I will fix the failing test.')).toBeVisible()
  const users = (await mock.requests())
    .slice(before)
    .flatMap((r) => (r.body?.messages ?? []).filter((m: any) => m.role === 'user'))
    .map((m: any) => (typeof m.content === 'string' ? m.content : JSON.stringify(m.content)))
  const fix = users.find((t: string) => t.includes('The CI check \\"test\\" failed') || t.includes('The CI check "test" failed'))
  expect(fix, 'the fix request reaches the model').toBeTruthy()
  expect(fix).toContain('AssertionError: add(2, 3) returned 5, expected 6')
  expect(fix).toContain('1 test failed')
  expect(fix).not.toContain('2026-10-03T10:00:00.0000000Z')
})

test('review a PR: inline comment on its diff, request changes', async () => {
  const { page } = L
  const inbox = panel.getByLabel('Pull requests', { exact: true })
  await inbox.locator('.gp-inbox-item', { hasText: '#7' }).click()
  const view = panel.getByLabel('Pull request #7')
  await expect(view.getByText('#7 Validate add() inputs')).toBeVisible()
  const file = view.locator('[data-file="main.py"]')
  await expect(file.getByText('# TODO: validate inputs')).toBeVisible()
  // the existing review comment sits on its line
  await expect(file.locator('.prv-remote').getByText('Why a TODO here?')).toBeVisible()

  await file.getByRole('button', { name: 'Comment on new line 3' }).click()
  await file.getByRole('textbox', { name: 'Review comment' }).fill('Validate before returning.')
  await file.getByRole('button', { name: 'Add comment' }).click()
  await expect(file.getByText('Validate before returning.')).toBeVisible()
  await view.getByText('Request changes', { exact: true }).click()
  await view.getByRole('textbox', { name: 'Review summary' }).fill('Please drop the TODO.')
  await shot('git-pr-review-light', panel)
  await setTheme('dark')
  await shot('git-pr-review-dark', panel)
  await setTheme('light')

  hits.length = 0
  await view.getByRole('button', { name: 'Submit review' }).click()
  const dlg = page.getByRole('dialog', { name: 'Submit review' })
  await expect(dlg).toContainText('Request changes')
  await dlg.getByRole('button', { name: 'Submit review' }).click()
  await expect(page.getByText('Review submitted')).toBeVisible()
  const post = hits.find((h) => h.method === 'POST' && h.path === '/repos/o/r/pulls/7/reviews')
  expect(post?.auth).toBe(`Bearer ${TOKEN}`)
  expect(post?.body).toEqual({
    event: 'REQUEST_CHANGES',
    body: 'Please drop the TODO.',
    comments: [{ path: 'main.py', body: 'Validate before returning.', side: 'RIGHT', line: 3 }],
  })
  await expect(file.locator('.rv-comment')).toHaveCount(0)

  // back, then open by number
  await view.getByRole('button', { name: 'Back to git summary' }).click()
  await panel.getByRole('textbox', { name: 'Pull request number' }).fill('7')
  await panel.getByRole('button', { name: 'Review PR' }).click()
  await expect(panel.getByLabel('Pull request #7').getByText('#7 Validate add() inputs')).toBeVisible()
  await panel.getByRole('button', { name: 'Back to git summary' }).click()
})

test('the push dialog offers force with lease only when allowed', async () => {
  const { page } = L
  await panel.getByRole('button', { name: /^Push/ }).first().click()
  let dlg = page.getByRole('dialog', { name: /^Push/ })
  await expect(dlg.getByRole('checkbox', { name: 'Force with lease' })).toBeEnabled()
  await dlg.getByRole('button', { name: 'Cancel' }).click()
  await req(page, 'config/write', { edits: [{ keyPath: 'git.allow_force_push', value: null }] })
  await panel.getByRole('button', { name: /^Push/ }).first().click()
  dlg = page.getByRole('dialog', { name: /^Push/ })
  await expect(dlg.getByRole('checkbox', { name: 'Force with lease' })).toBeDisabled()
  await expect(dlg.getByText('Force pushes are off.')).toBeVisible()
  await dlg.getByRole('button', { name: 'Cancel' }).click()
})

test('review file buttons open at the first change; detached review window', async () => {
  const { page, app, project } = L
  await panel.getByRole('tab', { name: 'Review' }).click()
  const main = panel.locator('[data-file$="::main.py"]')
  await expect(main).toBeVisible()

  // external editor at the first changed line (line 2)
  const out = path.join(path.dirname(project), 'editor-out.txt')
  // an "editor" that records the {file}:{line} it was asked to open
  const recorder = `node -e "require('fs').writeFileSync(process.argv[1],process.argv[2])" "${out}" {file}:{line}`
  await page.evaluate((cmd) => (window as any).odex.settings.set({ editor: cmd }), recorder)
  await main.getByRole('button', { name: 'Open main.py in external editor' }).click()
  await expect.poll(() => (fs.existsSync(out) ? fs.readFileSync(out, 'utf8') : '')).toMatch(/main\.py"?:2/)

  // in-app: the Files tab opens the file
  await main.getByRole('button', { name: 'Open main.py', exact: true }).click()
  await expect(panel.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'true')

  // detached delivery: Ctrl+Shift+G pops out a review-only window for the thread
  await openSettingsPanel('Code review')
  await page.getByRole('combobox', { name: 'Review delivery' }).selectOption('detached')
  await expect.poll(() => page.evaluate(async () => (await (window as any).odex.settings.get()).reviewDelivery)).toBe('detached')
  await openThread('PR thread')
  await page.locator('.thread-header .name').click()
  const [win] = await Promise.all([app.waitForEvent('window'), page.keyboard.press('Control+Shift+G')])
  await win.waitForLoadState('domcontentloaded')
  expect(win.url()).toContain('panel=review')
  await expect(win.getByRole('combobox', { name: 'Diff target' })).toBeVisible({ timeout: 30_000 })
  await expect(win.locator('[data-file$="::main.py"]')).toBeVisible()
  await expect(win.getByRole('complementary', { name: 'Side panel' })).toHaveCount(0)
  await win.setViewportSize({ width: 900, height: 700 }).catch(() => {})
  await win.screenshot({ path: path.join(SHOTS, 'review-detached-light.png') })
  await win.close()
})
