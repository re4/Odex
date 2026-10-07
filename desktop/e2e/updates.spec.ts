import { test, expect, type Page } from '@playwright/test'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import type { AddressInfo } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { launch, type Launched } from './harness'

// App updates (main/updater.ts) against a local release feed (ODEX_UPDATE_URL): check, download with
// SHA-512 verification, the "Update ready" prompt, and checksum failures. Nothing is ever installed.
// Windows only: the NSIS updater is what ships there; macOS updates need a signed build.
test.skip(process.platform !== 'win32', 'NSIS updater (Windows)')

const VERSION = '99.0.0'
const INSTALLER = `Odex-Setup-${VERSION}-x64.exe`

interface Feed {
  url: string
  /** LOCALAPPDATA for the app: electron-updater caches downloads under it. */
  cache: string
  hits: string[]
  close(): void
}

/** Serve latest.yml and a fake installer; `corrupt` advertises a checksum the file doesn't match. */
async function startFeed(opts: { corrupt?: boolean } = {}): Promise<Feed> {
  const body = crypto.randomBytes(512 * 1024)
  const sha512 = crypto
    .createHash('sha512')
    .update(opts.corrupt ? Buffer.from('something else') : body)
    .digest('base64')
  const yml = [
    `version: ${VERSION}`,
    'files:',
    `  - url: ${INSTALLER}`,
    `    sha512: ${sha512}`,
    `    size: ${body.length}`,
    `path: ${INSTALLER}`,
    `sha512: ${sha512}`,
    "releaseDate: '2026-10-01T00:00:00.000Z'",
    '',
  ].join('\n')
  const hits: string[] = []
  const server = http.createServer((req, res) => {
    const p = new URL(req.url ?? '/', 'http://feed').pathname
    hits.push(p)
    if (p === '/latest.yml') {
      res.writeHead(200, { 'content-type': 'text/yaml' })
      res.end(yml)
    } else if (p === `/${INSTALLER}`) {
      res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': body.length })
      res.end(body)
    } else {
      res.writeHead(404)
      res.end()
    }
  })
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r))
  const cache = fs.mkdtempSync(path.join(os.tmpdir(), 'odex-update-cache-'))
  return {
    url: `http://127.0.0.1:${(server.address() as AddressInfo).port}/`,
    cache,
    hits,
    close: () => {
      server.close()
      fs.rmSync(cache, { recursive: true, force: true, maxRetries: 3 })
    },
  }
}

async function openAbout(page: Page): Promise<void> {
  const nav = page.getByRole('navigation', { name: 'Settings sections' })
  await expect(async () => {
    if (!(await nav.isVisible())) {
      await page.locator('body').focus().catch(() => {})
      await page.keyboard.press('Control+,')
    }
    await expect(nav).toBeVisible({ timeout: 2000 })
  }).toPass({ timeout: 20_000 })
  await nav.getByRole('button', { name: 'About & data', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Updates' })).toBeVisible()
}

let feed: Feed
let L: Launched

test.afterEach(async () => {
  await L?.close()
  feed?.close()
})

test('downloads an update in the background and asks to restart', async () => {
  feed = await startFeed()
  L = await launch({ env: { ODEX_UPDATE_URL: feed.url, LOCALAPPDATA: feed.cache } })
  const { page } = L
  await openAbout(page)
  const status = page.getByTestId('update-status')
  await expect(status).toContainText('Not checked yet')
  await status.getByRole('button', { name: 'Check for updates' }).click()

  // automatic updates are on by default: found, downloaded and verified, then the prompt
  const dialog = page.getByRole('dialog', { name: 'Update ready' })
  await expect(dialog).toBeVisible({ timeout: 30_000 })
  await expect(dialog).toContainText(`Odex ${VERSION} has been downloaded`)
  expect(feed.hits).toContain(`/${INSTALLER}`)
  expect(fs.existsSync(path.join(feed.cache, 'odex-desktop-updater', 'pending', INSTALLER))).toBe(true)

  // "Later" keeps the offer in a banner and in About
  await dialog.getByRole('button', { name: 'Later' }).click()
  await expect(dialog).toBeHidden()
  const banner = page.locator('.banner', { hasText: `Odex ${VERSION} is ready to install` })
  await expect(banner).toBeVisible()
  await expect(banner.getByRole('button', { name: 'Restart and update' })).toBeVisible()
  await expect(status).toContainText(`Odex ${VERSION} is downloaded and ready to install`)
  await expect(status.getByRole('button', { name: 'Restart and update' })).toBeVisible()
  await banner.getByRole('button', { name: 'Hide update banner' }).click()
  await expect(banner).toBeHidden()
})

test('with automatic updates off, a check only offers the download', async () => {
  feed = await startFeed()
  L = await launch({ settings: { autoUpdate: false }, env: { ODEX_UPDATE_URL: feed.url, LOCALAPPDATA: feed.cache } })
  const { page } = L
  await openAbout(page)
  await expect(page.getByRole('switch', { name: 'Update automatically' })).toHaveAttribute('aria-checked', 'false')
  const status = page.getByTestId('update-status')
  await status.getByRole('button', { name: 'Check for updates' }).click()
  await expect(status).toContainText(`Odex ${VERSION} is available`)
  expect(feed.hits).not.toContain(`/${INSTALLER}`)

  await status.getByRole('button', { name: 'Download' }).click()
  await expect(page.getByRole('dialog', { name: 'Update ready' })).toBeVisible({ timeout: 30_000 })
})

test('a download that fails its checksum is reported, not offered', async () => {
  feed = await startFeed({ corrupt: true })
  L = await launch({ env: { ODEX_UPDATE_URL: feed.url, LOCALAPPDATA: feed.cache } })
  const { page } = L
  await openAbout(page)
  const status = page.getByTestId('update-status')
  await status.getByRole('button', { name: 'Check for updates' }).click()
  await expect(status).toContainText('Couldn’t update', { timeout: 30_000 })
  await expect(page.getByRole('region', { name: 'Updates' }).locator('.sx-callout.danger')).toContainText(/sha512 checksum mismatch/i)
  await expect(page.getByRole('dialog', { name: 'Update ready' })).toHaveCount(0)
  await expect(page.locator('.banner', { hasText: 'ready to install' })).toHaveCount(0)
})
