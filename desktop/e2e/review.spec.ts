import { test, expect, type Locator } from '@playwright/test'
import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { addProject, desktopDir, engineReady, launch, startMock, type Launched, type Mock } from './harness'

const SHOTS = process.env.ODEX_SHOTS_DIR || path.join(desktopDir, 'test-results', 'review-shots')

const NEW_MAIN = 'def add(a, b):\n    """Add two numbers."""\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n\n\nprint(add(2, 3))\nprint(sub(5, 1))\n'
const COMMENT = 'Please add a unit test for sub()'
const FINDING = 'sub() has no test coverage'

let mock: Mock
let L: Launched
let panel: Locator

function git(args: string[], cwd: string): string {
  return execFileSync('git', args, { cwd, encoding: 'utf8' })
}

test.beforeAll(async () => {
  fs.mkdirSync(SHOTS, { recursive: true })
  mock = await startMock([
    { when: { structured_name: 'commit_message' }, reply: { kind: 'text', text: '{"subject":"Add sub helper","body":"Adds sub() and a docstring for add()."}' } },
    { when: { structured: true }, reply: { kind: 'text', text: '{"title":"Refactor main"}' } },
    {
      when: { system_contains: 'You are reviewing code changes' },
      reply: {
        kind: 'text',
        text: JSON.stringify({
          summary: 'The refactor is fine; one gap.',
          overall_correctness: 'correct',
          findings: [{ title: FINDING, body: 'Add a test that exercises sub() with negative numbers.', priority: 1, confidence: 0.8, path: 'main.py', line_start: 6, line_end: 7 }],
        }),
      },
    },
    {
      when: { last_role: 'user', last_user_contains: 'refactor main' },
      reply: {
        kind: 'tool_calls',
        text: 'Editing the files.',
        calls: [
          { name: 'write_file', arguments: { path: 'main.py', content: NEW_MAIN } },
          { name: 'write_file', arguments: { path: 'notes.md', content: '# Notes\n\nAdded sub().\n' } },
        ],
      },
    },
    { when: { last_user_contains: 'Review comments on the current changes' }, reply: { kind: 'text', text: 'Thanks, I will address the review comments.' } },
    {
      when: { last_role: 'user', last_user_contains: 'worktree change' },
      reply: { kind: 'tool_calls', calls: [{ name: 'write_file', arguments: { path: 'CHANGELOG.md', content: '# Changelog\n\n- Added sub().\n' } }] },
    },
    { when: { last_role: 'tool', last_user_contains: 'worktree change' }, reply: { kind: 'text', text: 'Done. I wrote the changelog.' } },
    { when: { last_role: 'tool' }, reply: { kind: 'text', text: 'Done. I refactored main.py.' } },
    { when: {}, reply: { kind: 'text', text: 'OK.' } },
  ])
  L = await launch({ mockUrl: mock.url })
  await engineReady(L.page)
  panel = L.page.getByRole('complementary', { name: 'Side panel' })
})

test.afterAll(async () => {
  await L?.close()
  mock?.stop()
})

test('the agent edit shows up in the review panel', async () => {
  const { page, project } = L
  await addProject(page, project)
  await page.getByRole('button', { name: 'No project', exact: true }).click()
  await page.getByRole('menuitem', { name: /^project/ }).click()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('Please refactor main (refactor main)')
  await box.press('Enter')
  await expect(page.getByText('Done. I refactored main.py.')).toBeVisible()
  expect(fs.readFileSync(path.join(project, 'main.py'), 'utf8')).toBe(NEW_MAIN)

  // the diff-stats chip opens the review tab
  await page.getByRole('button', { name: /2 files/ }).click()
  await expect(panel).toBeVisible()
  await expect(panel.getByRole('combobox', { name: 'Diff target' })).toHaveValue('uncommitted')
  const main = panel.locator('[data-file$="::main.py"]')
  await expect(main).toBeVisible()
  await expect(main.getByText('def sub(a, b):')).toBeVisible()
  await expect(panel.locator('[data-file$="::notes.md"]')).toBeVisible()
  await expect(panel.getByText(/2\s*files/).first()).toBeVisible()

  // split view renders both sides, then back to unified
  await panel.getByRole('button', { name: 'Switch to split view' }).click()
  await expect(main.locator('.dv-srow').first()).toBeVisible()
  await panel.getByRole('button', { name: 'Switch to unified view' }).click()
  await expect(main.locator('.dv-row').first()).toBeVisible()

  // search inside the diff
  await panel.getByRole('button', { name: 'Search in diff' }).click()
  await panel.getByRole('textbox', { name: 'Find in diff' }).fill('sub(')
  await expect(main.locator('.dv-q-start')).toHaveCount(2)
  await expect(panel.getByText('3 matches')).toBeVisible() // two in main.py, one in notes.md
  await panel.getByRole('textbox', { name: 'Find in diff' }).press('Enter')
  await expect(main.locator('.dv-q-start.current')).toHaveCount(1)
  await expect(panel.getByText('1/3')).toBeVisible()
  await panel.getByRole('button', { name: 'Close search' }).click()
  await expect(main.locator('.dv-q')).toHaveCount(0)

  // other targets: last turn shows the same edit
  await panel.getByRole('combobox', { name: 'Diff target' }).selectOption('lastTurn')
  await expect(panel.locator('[data-file$="::main.py"]').getByText('def sub(a, b):')).toBeVisible()
  await panel.getByRole('combobox', { name: 'Diff target' }).selectOption('uncommitted')
  await expect(main.getByText('def sub(a, b):')).toBeVisible()
})

test('an inline comment (line range) is sent to the agent', async () => {
  const { page } = L
  const main = panel.locator('[data-file$="::main.py"]')
  await main.getByRole('button', { name: 'Comment on new line 6' }).click()
  await main.getByRole('button', { name: 'Comment on new line 7' }).click({ modifiers: ['Shift'] })
  await expect(main.getByText('Comment on lines 6–7')).toBeVisible()
  await main.getByRole('textbox', { name: 'Review comment' }).fill(COMMENT)
  await main.getByRole('button', { name: 'Add comment' }).click()
  await expect(main.getByText(COMMENT)).toBeVisible()
  const send = panel.getByRole('button', { name: 'Send 1 comment to agent' })
  await expect(send).toBeVisible()
  await panel.screenshot({ path: path.join(SHOTS, 'review-comment-light.png') })

  const before = (await mock.requests()).length
  await send.click()
  await expect(page.getByText('Thanks, I will address the review comments.')).toBeVisible()
  await expect(send).toHaveCount(0)
  const reqs = (await mock.requests()).slice(before)
  const users = reqs.flatMap((r) => (r.body?.messages ?? []).filter((m: any) => m.role === 'user'))
  const text = (m: any) => (typeof m.content === 'string' ? m.content : JSON.stringify(m.content))
  const hit = users.find((m: any) => text(m).includes(COMMENT))
  expect(hit, 'the next model request carries the review comment').toBeTruthy()
  expect(text(hit)).toContain('main.py:6-7')
  expect(text(hit)).toContain('def sub(a, b):')
})

test('ask the agent to review: findings arrive in the thread and inline', async () => {
  const { page } = L
  await panel.getByRole('button', { name: 'Ask agent to review' }).click()
  // the review item lands in the thread
  await expect(page.locator('.thread-view').getByText(FINDING)).toBeVisible()
  // and the finding is anchored on its line in the diff
  const main = panel.locator('[data-file$="::main.py"]')
  await expect(main.locator('.rv-comment.finding').getByText(FINDING)).toBeVisible()
  await expect(panel.getByText('Agent review')).toBeVisible()

  await page.screenshot({ path: path.join(SHOTS, 'review-light.png') })
  await panel.screenshot({ path: path.join(SHOTS, 'review-panel-light.png') })
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'dark' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  await page.waitForTimeout(300)
  await page.screenshot({ path: path.join(SHOTS, 'review-dark.png') })
  await panel.screenshot({ path: path.join(SHOTS, 'review-panel-dark.png') })
  await panel.getByRole('button', { name: 'Switch to split view' }).click()
  await panel.screenshot({ path: path.join(SHOTS, 'review-split-dark.png') })
  await panel.getByRole('button', { name: 'Switch to unified view' }).click()
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
})

test('stage a file and commit from the git panel', async () => {
  const { page, project } = L
  await panel.getByRole('tab', { name: 'Git' }).click()
  const unstaged = panel.getByRole('group', { name: 'Unstaged files' })
  await expect(unstaged.getByText('main.py')).toBeVisible()
  await expect(panel.getByRole('group', { name: 'Untracked files' }).getByText('notes.md')).toBeVisible()
  await expect(panel.locator('.gp-log-item').first()).toContainText('init')
  await unstaged.getByRole('button', { name: 'Stage main.py' }).click()
  const staged = panel.getByRole('group', { name: 'Staged files' })
  await expect(staged.getByText('main.py')).toBeVisible()
  await expect(panel.getByRole('group', { name: 'Unstaged files' })).toHaveCount(0)
  await page.screenshot({ path: path.join(SHOTS, 'git-light.png') })

  await panel.getByRole('button', { name: 'Generate message' }).click()
  const msg = panel.getByRole('textbox', { name: 'Commit message' })
  await expect(msg).toHaveValue(/^Add sub helper/)
  const message = (await msg.inputValue()).trim()
  await panel.getByRole('button', { name: 'Commit', exact: true }).click()
  await expect(page.getByText(/Committed [0-9a-f]{7}/)).toBeVisible()
  await expect(msg).toHaveValue('')

  const subject = git(['log', '-1', '--format=%s'], project).trim()
  expect(subject).toBe(message.split('\n')[0])
  expect(git(['show', '--name-only', '--format=', 'HEAD'], project)).toContain('main.py')
  // only the staged file was committed
  expect(git(['status', '--porcelain'], project)).toContain('?? notes.md')
  await expect(panel.getByText(subject).first()).toBeVisible()
  await expect(panel.getByRole('group', { name: 'Staged files' })).toHaveCount(0)
})

test('per-hunk stage / unstage and file revert in the unstaged and staged views', async () => {
  const { page, project } = L
  const lines = Array.from({ length: 40 }, (_, i) => `line ${i + 1}`)
  fs.writeFileSync(path.join(project, 'long.txt'), lines.join('\n') + '\n')
  git(['add', 'long.txt'], project)
  git(['-c', 'user.email=e2e@odex.test', '-c', 'user.name=e2e', 'commit', '-q', '-m', 'add long.txt'], project)
  const changed = [...lines]
  changed[1] = 'line 2 changed'
  changed[34] = 'line 35 changed'
  fs.writeFileSync(path.join(project, 'long.txt'), changed.join('\n') + '\n')

  await panel.getByRole('tab', { name: 'Review' }).click()
  await panel.getByRole('combobox', { name: 'Diff target' }).selectOption('unstaged')
  await panel.getByRole('button', { name: 'Refresh diff' }).click()
  const long = panel.locator('[data-file$="::long.txt"]')
  await expect(long.locator('.dv-hunk')).toHaveCount(2)
  await long.getByRole('button', { name: 'Stage hunk' }).first().click()
  await expect(long.locator('.dv-hunk')).toHaveCount(1)
  const cached = git(['diff', '--cached', '--', 'long.txt'], project)
  expect(cached).toContain('line 2 changed')
  expect(cached).not.toContain('line 35 changed')

  // revert the rest of the file (unstaged part) after confirming
  await long.getByRole('button', { name: 'Revert long.txt' }).click()
  const dlg = page.getByRole('dialog', { name: 'Revert changes' })
  await expect(dlg).toBeVisible()
  await dlg.getByRole('button', { name: 'Revert' }).click()
  await expect(long).toHaveCount(0)
  expect(git(['diff', '--', 'long.txt'], project)).toBe('')
  expect(fs.readFileSync(path.join(project, 'long.txt'), 'utf8')).toContain('line 2 changed')

  // the staged hunk can be unstaged hunk by hunk
  await panel.getByRole('combobox', { name: 'Diff target' }).selectOption('staged')
  await expect(long.locator('.dv-hunk')).toHaveCount(1)
  await long.getByRole('button', { name: 'Unstage hunk' }).click()
  await expect(long).toHaveCount(0)
  expect(git(['diff', '--cached'], project)).toBe('')
  git(['checkout', '--', 'long.txt'], project)
})

test('large diffs render capped with show more', async () => {
  const { project } = L
  fs.writeFileSync(path.join(project, 'big.txt'), Array.from({ length: 2600 }, (_, i) => `generated line ${i + 1} with some words`).join('\n') + '\n')
  await panel.getByRole('combobox', { name: 'Diff target' }).selectOption('uncommitted')
  await panel.getByRole('button', { name: 'Refresh diff' }).click()
  const big = panel.locator('[data-file$="::big.txt"]')
  await expect(big.getByRole('button', { name: 'Show 1000 more lines' })).toBeVisible()
  expect(await big.locator('.dv-row').count()).toBeLessThanOrEqual(400)
  await big.getByRole('button', { name: /Show all/ }).click()
  await expect(big.getByText('generated line 2600 with some words')).toBeAttached()
  await expect(big.getByRole('button', { name: /more lines/ })).toHaveCount(0)
  // collapse it again via its header
  await big.locator('.dv-file-head').click()
  await expect(big.locator('.dv-row')).toHaveCount(0)
  fs.rmSync(path.join(project, 'big.txt'))
})

test('a folder that is not a git repository offers git init', async () => {
  const { page } = L
  const plain = fs.mkdtempSync(path.join(os.tmpdir(), 'odex-plain-'))
  fs.writeFileSync(path.join(plain, 'hello.txt'), 'hello\n')
  await addProject(page, plain)
  await page.getByRole('button', { name: 'New thread' }).first().click()
  // the composer's project picker (sidebar project headers come first in the DOM)
  await page.getByRole('button', { name: /^(No project|project)$/ }).last().click()
  await page.getByRole('menuitem', { name: new RegExp(`^${path.basename(plain)}`) }).click()
  if (!(await panel.isVisible())) await page.getByRole('button', { name: 'Toggle side panel' }).click()
  await panel.getByRole('tab', { name: 'Review' }).click()
  await expect(panel.getByText('This folder is not a git repository.')).toBeVisible()
  await panel.getByRole('button', { name: 'Initialize git repository' }).click()
  await expect.poll(() => fs.existsSync(path.join(plain, '.git')), { timeout: 20_000 }).toBe(true)
  await expect(panel.locator('[data-file$="::hello.txt"]')).toBeVisible()
  await expect(panel.getByText('Open a thread to comment on lines and ask the agent for a review.')).toBeVisible()
  await panel.screenshot({ path: path.join(SHOTS, 'review-no-thread-light.png') })
})

test('worktree thread: hand off to the local checkout, then remove it in settings', async () => {
  const { page, project } = L
  await page.getByRole('button', { name: 'New thread' }).first().click()
  await page.getByRole('button', { name: /^(No project|project|odex-plain-\w+)$/ }).last().click()
  await page.getByRole('menuitem', { name: /^project/ }).click()
  await page.getByRole('button', { name: 'Local', exact: true }).click()
  await page.getByRole('menuitem', { name: /^Worktree/ }).click()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('Add a changelog (worktree change)')
  await box.press('Enter')
  await expect(page.getByText('Done. I wrote the changelog.')).toBeVisible()
  expect(fs.existsSync(path.join(project, 'CHANGELOG.md'))).toBe(false)

  if (!(await panel.isVisible())) await page.getByRole('button', { name: 'Toggle side panel' }).click()
  await panel.getByRole('tab', { name: 'Git' }).click()
  await expect(panel.getByText('Worktree', { exact: true })).toBeVisible()
  await expect(panel.getByRole('group', { name: 'Untracked files' }).getByText('CHANGELOG.md')).toBeVisible()
  await page.screenshot({ path: path.join(SHOTS, 'git-worktree-light.png') })
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'dark' }))
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
  await page.waitForTimeout(300) // let the theme transition finish
  await panel.screenshot({ path: path.join(SHOTS, 'git-worktree-dark.png') })
  await page.evaluate(() => (window as any).odex.settings.set({ theme: 'light' }))
  await panel.getByRole('button', { name: 'Hand off…' }).click()
  const dlg = page.getByRole('dialog', { name: 'Hand off worktree' })
  await expect(dlg).toBeVisible()
  await expect(dlg.getByRole('radio', { name: /Merge into a local branch/ })).toBeChecked()
  await expect(dlg.getByLabel('Target branch', { exact: true })).toHaveValue('main')
  await dlg.getByPlaceholder('Commit message').fill('Add changelog')
  await dlg.getByRole('button', { name: 'Hand off', exact: true }).click()
  await expect(dlg).toHaveCount(0)
  expect(fs.readFileSync(path.join(project, 'CHANGELOG.md'), 'utf8')).toContain('Added sub()')
  expect(git(['log', 'main', '--format=%s', '-n', '5'], project)).toContain('Add changelog')

  // Settings → Worktrees lists it; remove after confirming
  await page.getByRole('button', { name: 'Settings' }).click()
  await page.getByRole('button', { name: 'Worktrees' }).click()
  await expect(page.getByRole('heading', { name: 'Worktrees', exact: true })).toBeVisible()
  const item = page.locator('.wt-item')
  await expect(item).toHaveCount(1)
  await expect(item).toContainText('odex/')
  await page.screenshot({ path: path.join(SHOTS, 'settings-worktrees-light.png') })
  const wtPath = (await item.locator('.mono.ellipsis').last().textContent())!.trim()
  expect(fs.existsSync(wtPath)).toBe(true)
  await item.getByRole('button', { name: 'Remove' }).click()
  const confirm = page.getByRole('dialog', { name: 'Remove worktree' })
  await confirm.getByRole('button', { name: 'Remove worktree' }).click()
  await expect(page.getByText('No worktrees.')).toBeVisible()
  expect(fs.existsSync(wtPath)).toBe(false)
})
