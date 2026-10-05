import { test, expect } from '@playwright/test'
import { addProject, engineReady, launch, startMock } from './harness'

// Baselines are recorded on Windows (the primary platform); fonts differ elsewhere.
test.skip(process.platform !== 'win32', 'visual baselines exist for Windows only')

const RULES = [
  {
    when: { last_role: 'user', last_user_contains: 'visual' },
    reply: { kind: 'tool_calls', text: 'Let me look at the code first.', calls: [{ name: 'read_file', arguments: { path: 'main.py' } }] },
  },
  {
    when: { last_role: 'tool', tool_results_since_user: 1 },
    reply: { kind: 'tool_calls', calls: [{ name: 'edit_file', arguments: { path: 'main.py', old_string: 'return a + b', new_string: 'return int(a) + int(b)' } }] },
  },
  {
    when: { last_role: 'tool' },
    reply: { kind: 'text', text: 'I updated `add` in **main.py** to coerce its inputs:\n\n```python\ndef add(a, b):\n    return int(a) + int(b)\n```\n\n- Strings like `"2"` now work\n- Behaviour for ints is unchanged' },
  },
  { when: {}, reply: { kind: 'text', text: 'Untitled' } },
]

for (const theme of ['light', 'dark'] as const) {
  test(`home and thread look right (${theme})`, async () => {
    const mock = await startMock(RULES)
    const L = await launch({ mockUrl: mock.url, theme })
    try {
      const { page } = L
      await engineReady(page)
      await addProject(page, L.project)
      await page.getByRole('button', { name: 'No project', exact: true }).click()
      await page.getByRole('menuitem', { name: /^project/ }).click()
      await page.mouse.move(0, 0)
      await expect(page).toHaveScreenshot(`home-${theme}.png`, { mask: [page.locator('.ctx-ring')] })

      const box = page.getByRole('textbox', { name: 'Message' })
      await box.fill('Make add() accept strings (visual)')
      await box.press('Enter')
      await expect(page.getByText('Behaviour for ints is unchanged')).toBeVisible()
      await page.mouse.move(0, 0)
      await page.waitForTimeout(300)
      await expect(page).toHaveScreenshot(`thread-${theme}.png`, {
        // timings, relative times and titles vary run to run
        mask: [page.locator('.thread-row .xs'), page.locator('.titlebar .title'), page.locator('.thread-header .name'), page.locator('.thread-row .ellipsis'), page.getByText(/^\d+(\.\d+)?m?s$/)],
      })
    } finally {
      await L.close()
      mock.stop()
    }
  })
}
