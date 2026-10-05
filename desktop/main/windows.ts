import { BrowserWindow, nativeTheme, screen, shell } from 'electron'
import fs from 'node:fs'
import { preloadPath, rendererUrl, resourcePath } from './paths'
import { getSettings, windowStateFile } from './settings'

interface Bounds {
  x?: number
  y?: number
  width: number
  height: number
  maximized?: boolean
}

function loadBounds(): Bounds {
  try {
    const b = JSON.parse(fs.readFileSync(windowStateFile(), 'utf8')) as Bounds
    // ensure on-screen
    const visible = screen.getAllDisplays().some((d) => {
      const a = d.workArea
      return b.x != null && b.y != null && b.x < a.x + a.width - 50 && b.y < a.y + a.height - 50 && b.x + b.width > a.x + 50 && b.y + b.height > a.y + 20
    })
    return visible ? b : { width: b.width || 1360, height: b.height || 880 }
  } catch {
    return { width: 1360, height: 880 }
  }
}

function saveBounds(w: BrowserWindow): void {
  try {
    const b = w.getNormalBounds()
    fs.writeFileSync(windowStateFile(), JSON.stringify({ ...b, maximized: w.isMaximized() }))
  } catch {}
}

export function isDark(): boolean {
  const t = getSettings().theme
  return t === 'dark' || (t === 'system' && nativeTheme.shouldUseDarkColors)
}

function overlayColors(): { color: string; symbolColor: string; height: number } {
  return isDark() ? { color: '#00000000', symbolColor: '#c9ccd6', height: 38 } : { color: '#00000000', symbolColor: '#3c4150', height: 38 }
}

export let mainWindow: BrowserWindow | null = null

export function createWindow(opts: { threadId?: string; popout?: boolean; panel?: string } = {}): BrowserWindow {
  const isMain = !opts.popout && !mainWindow
  const b = isMain ? loadBounds() : { width: 900, height: 800 }
  const w = new BrowserWindow({
    ...b,
    minWidth: 520,
    minHeight: 400,
    show: false,
    title: 'Odex',
    icon: resourcePath('icon.png'),
    backgroundColor: isDark() ? '#16171c' : '#fbfbfd',
    titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'hidden',
    titleBarOverlay: process.platform === 'darwin' ? undefined : overlayColors(),
    webPreferences: {
      preload: preloadPath(),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      spellcheck: true,
    },
  })
  if (isMain && b.maximized) w.maximize()
  const q: Record<string, string> = {}
  if (opts.threadId) q.thread = opts.threadId
  if (opts.popout) q.popout = '1'
  if (opts.panel) q.panel = opts.panel
  const target = rendererUrl(q)
  if (target.url) void w.loadURL(target.url)
  else void w.loadFile(target.file!, { query: target.query })
  w.once('ready-to-show', () => {
    if (!process.env.ODEX_HIDDEN) w.show()
  })
  w.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:/.test(url)) void shell.openExternal(url)
    return { action: 'deny' }
  })
  // never navigate the app window away from the UI
  w.webContents.on('will-navigate', (e, url) => {
    if (!url.startsWith('http://localhost') && !url.startsWith('file:')) {
      e.preventDefault()
      if (/^https?:/.test(url)) void shell.openExternal(url)
    }
  })
  if (isMain) {
    mainWindow = w
    w.on('close', (e) => {
      saveBounds(w)
      if (getSettings().keepRunningInTray && !quitting) {
        e.preventDefault()
        w.hide()
      }
    })
    w.on('closed', () => {
      mainWindow = null
    })
  }
  w.webContents.setZoomFactor(getSettings().zoom || 1)
  return w
}

export let quitting = false
export function setQuitting(): void {
  quitting = true
}

export function updateTitleBars(): void {
  if (process.platform === 'darwin') return
  for (const w of BrowserWindow.getAllWindows()) {
    try {
      w.setTitleBarOverlay(overlayColors())
      w.setBackgroundColor(isDark() ? '#16171c' : '#fbfbfd')
    } catch {}
  }
}

export let quickChatWindow: BrowserWindow | null = null

/** The small Quick Chat window (one at a time): a projectless chat, thread view + composer only. */
export function openQuickChat(): BrowserWindow {
  if (quickChatWindow && !quickChatWindow.isDestroyed()) {
    if (quickChatWindow.isMinimized()) quickChatWindow.restore()
    quickChatWindow.show()
    quickChatWindow.focus()
    return quickChatWindow
  }
  const s = getSettings()
  const w = new BrowserWindow({
    width: 520,
    height: 640,
    minWidth: 380,
    minHeight: 360,
    show: false,
    title: 'Odex Quick Chat',
    icon: resourcePath('icon.png'),
    alwaysOnTop: !!s.quickChatOnTop,
    backgroundColor: isDark() ? '#16171c' : '#fbfbfd',
    titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'hidden',
    titleBarOverlay: process.platform === 'darwin' ? undefined : overlayColors(),
    webPreferences: {
      preload: preloadPath(),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      spellcheck: true,
    },
  })
  const target = rendererUrl({ quickchat: '1' })
  if (target.url) void w.loadURL(target.url)
  else void w.loadFile(target.file!, { query: target.query })
  w.once('ready-to-show', () => {
    if (!process.env.ODEX_HIDDEN) w.show()
  })
  w.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:/.test(url)) void shell.openExternal(url)
    return { action: 'deny' }
  })
  w.webContents.on('will-navigate', (e, url) => {
    if (!url.startsWith('http://localhost') && !url.startsWith('file:')) {
      e.preventDefault()
      if (/^https?:/.test(url)) void shell.openExternal(url)
    }
  })
  w.on('closed', () => {
    if (quickChatWindow === w) quickChatWindow = null
  })
  w.webContents.setZoomFactor(s.zoom || 1)
  quickChatWindow = w
  return w
}

export function showMain(): BrowserWindow {
  const w = mainWindow ?? createWindow()
  if (w.isMinimized()) w.restore()
  w.show()
  w.focus()
  return w
}
