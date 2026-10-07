import { app, BrowserWindow } from 'electron'
import type { AppUpdater, ProgressInfo, UpdateInfo } from 'electron-updater'
import fs from 'node:fs'
import { createRequire } from 'node:module'
import path from 'node:path'
import type { UpdateState } from '@shared/desktop'
import { odexHome } from './paths'
import { getSettings, onSettings } from './settings'
import { setQuitting } from './windows'

const require = createRequire(import.meta.url)

// Updates come from the GitHub releases page. Keep in sync with `publish` in electron-builder.yml, which
// writes resources/app-update.yml for electron-updater and the latest*.yml files each release must carry.
const RELEASES = 'https://github.com/re4/Odex/releases'
const FIRST_CHECK_MS = 15_000
const CHECK_EVERY_MS = 4 * 60 * 60_000

let updater: AppUpdater | null = null
let state: UpdateState = { status: 'idle', currentVersion: app.getVersion(), releaseUrl: RELEASES }
let timer: ReturnType<typeof setInterval> | null = null
const listeners: Array<(s: UpdateState) => void> = []

function set(patch: Partial<UpdateState>): void {
  state = { ...state, ...patch }
  for (const w of BrowserWindow.getAllWindows()) if (!w.isDestroyed()) w.webContents.send('odex:update-state', state)
  listeners.forEach((l) => l(state))
}

export function updateState(): UpdateState {
  return state
}

export function onUpdateState(l: (s: UpdateState) => void): void {
  listeners.push(l)
}

/** a > b for x.y.z versions (pre-release suffixes ignored). */
function newer(a: string, b: string): boolean {
  const pa = a.split(/[.+-]/).map(Number)
  const pb = b.split(/[.+-]/).map(Number)
  for (let i = 0; i < 3; i++) {
    const x = pa[i] || 0
    const y = pb[i] || 0
    if (x !== y) return x > y
  }
  return false
}

function releaseUrl(version: string): string {
  return process.env.ODEX_UPDATE_URL ? RELEASES : `${RELEASES}/tag/v${version}`
}

/** electron-updater logs every step; keep them in ~/.odex/logs/updater.log for failed updates. */
function fileLogger(): AppUpdater['logger'] {
  const file = path.join(odexHome(), 'logs', 'updater.log')
  try {
    if (fs.statSync(file).size > 1024 * 1024) fs.renameSync(file, `${file}.old`)
  } catch {}
  const write = (level: string) => (msg?: unknown) => {
    try {
      fs.mkdirSync(path.dirname(file), { recursive: true })
      fs.appendFileSync(file, `${new Date().toISOString()} ${level} ${msg instanceof Error ? msg.stack || msg.message : String(msg)}\n`)
    } catch {}
  }
  return { info: write('INFO'), warn: write('WARN'), error: write('ERROR'), debug: () => {} }
}

function onError(e: Error & { code?: string }): void {
  // A release without latest.yml. If it isn't newer than this build there is nothing to offer anyway.
  if (e.code === 'ERR_UPDATER_CHANNEL_FILE_NOT_FOUND') {
    const tag = /\/download\/v?([^/]+)\/latest[^/]*\.yml/.exec(e.message)?.[1]
    if (tag && !newer(tag, state.currentVersion)) {
      set({ status: 'not-available', version: undefined, error: undefined, checkedAt: Date.now() })
      return
    }
    set({ status: 'error', error: `The latest release${tag ? ` (${tag})` : ''} has no update information (latest.yml), so it can't be installed from here. Download it from the releases page.`, checkedAt: Date.now() })
    return
  }
  const first = (e.message || String(e)).split('\n')[0].trim()
  set({ status: 'error', error: first.length > 300 ? `${first.slice(0, 300)}…` : first, progress: undefined, checkedAt: Date.now() })
}

/** Set up the updater. `onDownloaded` runs when an update is ready to install. */
export function initUpdater(onDownloaded: (s: UpdateState) => void): void {
  // ODEX_UPDATE_URL points at another feed (a folder with latest.yml and the installers): mirrors and tests.
  const feed = process.env.ODEX_UPDATE_URL
  if (!app.isPackaged && !feed) {
    state = { ...state, status: 'unsupported', reason: 'Development builds don’t update themselves.' }
    return
  }
  if (process.windowsStore) {
    state = { ...state, status: 'unsupported', reason: 'Windows updates the MSIX package (Microsoft Store or App Installer).' }
    return
  }
  const { autoUpdater } = require('electron-updater') as typeof import('electron-updater')
  const u = autoUpdater
  u.logger = fileLogger()
  if (feed) {
    const file = path.join(app.getPath('userData'), 'update-feed.yml')
    fs.mkdirSync(path.dirname(file), { recursive: true })
    // JSON is valid YAML
    fs.writeFileSync(file, JSON.stringify({ provider: 'generic', url: feed, updaterCacheDirName: 'odex-desktop-updater' }))
    u.updateConfigPath = file
    u.forceDevUpdateConfig = !app.isPackaged
  }
  if (!u.isUpdaterActive()) {
    state = { ...state, status: 'unsupported', reason: 'This package format can’t update itself. Download new versions from the releases page.' }
    return
  }
  // Downloading is driven below (only with automatic updates on); installing always asks first.
  u.autoDownload = false
  u.autoInstallOnAppQuit = false
  u.allowDowngrade = false
  u.disableWebInstaller = true
  u.on('checking-for-update', () => set({ status: 'checking', error: undefined }))
  u.on('update-available', (info: UpdateInfo) => {
    set({ status: 'available', version: info.version, releaseDate: info.releaseDate, releaseUrl: releaseUrl(info.version), error: undefined, checkedAt: Date.now() })
    if (getSettings().autoUpdate) void downloadUpdate()
  })
  u.on('update-not-available', () => set({ status: 'not-available', version: undefined, progress: undefined, error: undefined, checkedAt: Date.now() }))
  u.on('download-progress', (p: ProgressInfo) =>
    set({ status: 'downloading', progress: { percent: p.percent, transferred: p.transferred, total: p.total, bytesPerSecond: p.bytesPerSecond } }),
  )
  u.on('update-downloaded', (info: UpdateInfo) => {
    set({ status: 'downloaded', version: info.version, releaseUrl: releaseUrl(info.version), progress: undefined, error: undefined })
    onDownloaded(state)
  })
  u.on('error', (e: Error) => onError(e))
  updater = u

  let auto = getSettings().autoUpdate
  schedule()
  setTimeout(() => {
    if (getSettings().autoUpdate) void checkForUpdates()
  }, FIRST_CHECK_MS).unref?.()
  onSettings((s) => {
    if (s.autoUpdate === auto) return
    auto = s.autoUpdate
    schedule()
    if (auto && state.status === 'available') void downloadUpdate()
    else if (auto && state.status !== 'downloading' && state.status !== 'downloaded') void checkForUpdates()
  })
}

function schedule(): void {
  if (timer) clearInterval(timer)
  timer = null
  if (!updater || !getSettings().autoUpdate) return
  timer = setInterval(() => void checkForUpdates(), CHECK_EVERY_MS)
  timer.unref?.()
}

export async function checkForUpdates(): Promise<UpdateState> {
  const busy = state.status === 'checking' || state.status === 'downloading' || state.status === 'downloaded'
  if (!updater || busy) return state
  set({ status: 'checking', error: undefined })
  try {
    await updater.checkForUpdates()
  } catch {
    // reported through the 'error' event
  }
  return state
}

export async function downloadUpdate(): Promise<void> {
  if (!updater || state.status !== 'available') return
  set({ status: 'downloading', progress: { percent: 0, transferred: 0, total: 0, bytesPerSecond: 0 } })
  try {
    // verifies the SHA-512 from latest.yml (and, on Windows, the publisher of signed builds)
    await updater.downloadUpdate()
  } catch {
    // reported through the 'error' event
  }
}

/** Quit and run the downloaded installer, then start the new version. */
export function installUpdate(): void {
  if (!updater || state.status !== 'downloaded') return
  setQuitting()
  // Silent: the installer reuses the current install folder and options. Force-run: relaunch afterwards.
  updater.quitAndInstall(true, true)
}
