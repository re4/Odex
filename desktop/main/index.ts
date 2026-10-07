import { app, BrowserWindow, dialog, globalShortcut, ipcMain, Menu, nativeImage, nativeTheme, Notification, powerSaveBlocker, shell, Tray } from 'electron'
import fs from 'node:fs'
import path from 'node:path'
import * as browser from './browser'
import { EngineHost } from './engine-host'
import * as fsw from './fswatch'
import { odexHome, resourcePath } from './paths'
import { hasSecret, setSecret } from './secrets'
import { getSettings, onSettings, setSettings } from './settings'
import * as terminals from './terminals'
import { openInEditor } from './editor'
import { createWindow, mainWindow, openQuickChat, setQuitting, showMain, updateTitleBars } from './windows'
import { handlePreviewProtocol, registerPreviewIpc, registerPreviewScheme } from './preview'
import { checkForUpdates, downloadUpdate, initUpdater, installUpdate, onUpdateState, updateState } from './updater'

const engine = new EngineHost()
let tray: Tray | null = null
const threadStatus = new Map<string, { status: string; name: string }>()
const pendingServer = new Map<number, (r: { result?: unknown; error?: string }) => void>()
let serverReqId = 0
let keepAwakeId: number | null = null
let killSwitch = false

// ------------------------------------------------------------ single instance

// Tests and side-by-side dev runs get their own profile (and lock).
if (process.env.ODEX_USER_DATA) app.setPath('userData', process.env.ODEX_USER_DATA)

// HTML previews in the Files panel (must be registered before 'ready')
registerPreviewScheme()

if (!app.requestSingleInstanceLock()) {
  app.quit()
} else {
  app.on('second-instance', (_e, argv) => {
    const link = argv.find((a) => a.startsWith('odex://'))
    const w = showMain()
    if (link) w.webContents.send('odex:deeplink', link)
  })
}

// Registering odex:// writes to the OS (registry on Windows): packaged builds only,
// unless a dev build opts in.
if (app.isPackaged) {
  app.setAsDefaultProtocolClient('odex')
} else if (process.env.ODEX_REGISTER_PROTOCOL && process.argv.length >= 2) {
  app.setAsDefaultProtocolClient('odex', process.execPath, [path.resolve(process.argv[1])])
}
app.on('open-url', (e, url) => {
  e.preventDefault()
  showMain().webContents.send('odex:deeplink', url)
})

if (process.platform === 'win32') app.setAppUserModelId('dev.odex.app')

// ------------------------------------------------------------------- helpers

function broadcast(channel: string, payload: unknown): void {
  for (const w of BrowserWindow.getAllWindows()) if (!w.isDestroyed()) w.webContents.send(channel, payload)
}

function anyFocused(): boolean {
  return BrowserWindow.getAllWindows().some((w) => w.isFocused())
}

/** Ask the renderer (approvals, elicitation, downloads). */
function askRenderer(method: string, params: unknown): Promise<unknown> {
  const id = ++serverReqId
  return new Promise((resolve, reject) => {
    pendingServer.set(id, (r) => (r.error ? reject(new Error(r.error)) : resolve(r.result)))
    let wins = BrowserWindow.getAllWindows()
    if (wins.length === 0) wins = [createWindow()]
    for (const w of wins) w.webContents.send('odex:server-request', { id, method, params })
  })
}

function updateTray(): void {
  if (!tray) return
  const all = [...threadStatus.values()]
  const running = all.filter((t) => t.status === 'running' || t.status === 'compacting' || t.status === 'reconnecting').length
  const waiting = all.filter((t) => t.status === 'waitingApproval').length
  const usageLabel = usageToday == null ? null : `${formatTokenCount(usageToday)} tokens today`
  const update = updateState()
  const tooltip = `Odex — ${running} running${waiting ? `, ${waiting} need approval` : ''}${usageLabel ? ` · ${usageLabel}` : ''}`
  tray.setToolTip(tooltip)
  // read by the e2e tests (app.evaluate)
  ;(globalThis as { __odexTray?: unknown }).__odexTray = { tooltip, usageLabel, running, waiting }
  const menu = Menu.buildFromTemplate([
    { label: 'Show Odex', click: () => showMain() },
    { label: 'New thread', click: () => showMain().webContents.send('odex:command', { command: 'newThread' }) },
    { label: 'Quick chat', click: () => openQuickChat() },
    { type: 'separator' },
    { label: `Running: ${running}`, enabled: false },
    ...(usageLabel ? [{ label: `Usage: ${usageLabel}`, enabled: false }] : []),
    { label: `Needs approval: ${waiting}`, enabled: waiting > 0, click: () => showMain().webContents.send('odex:command', { command: 'nextAttention' }) },
    { type: 'separator' },
    { label: killSwitch ? 'Release kill switch' : 'Kill switch (stop computer & browser use)', click: () => void toggleKillSwitch() },
    { type: 'separator' },
    ...(update.status === 'downloaded' ? [{ label: `Restart to update to ${update.version}`, click: () => installUpdate() }] : []),
    { label: 'Quit Odex', click: () => quit() },
  ])
  tray.setContextMenu(menu)
  // keep the machine awake while threads run
  const s = getSettings()
  if (s.keepAwake && running > 0 && keepAwakeId == null) keepAwakeId = powerSaveBlocker.start('prevent-app-suspension')
  if ((!s.keepAwake || running === 0) && keepAwakeId != null) {
    powerSaveBlocker.stop(keepAwakeId)
    keepAwakeId = null
  }
}

// Today's local token usage for the tray (refreshed every few minutes and after turns).
let usageToday: number | null = null
let usageTimer: ReturnType<typeof setTimeout> | null = null

function formatTokenCount(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 10_000) return `${Math.round(n / 1000)}k`
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`
  return String(n)
}

async function refreshTrayUsage(): Promise<void> {
  if (engine.state !== 'ready') return
  const d = new Date()
  const today = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
  try {
    const r = await engine.request<{ rows: Array<{ date: string; usage: { totalTokens: number } }> }>('usage/stats', { since: today })
    usageToday = r.rows.filter((x) => x.date === today).reduce((n, x) => n + (x.usage?.totalTokens ?? 0), 0)
    updateTray()
  } catch {}
}

function scheduleTrayUsage(delay = 2000): void {
  if (usageTimer) clearTimeout(usageTimer)
  usageTimer = setTimeout(() => void refreshTrayUsage(), delay)
}
setInterval(() => void refreshTrayUsage(), 5 * 60_000).unref?.()

async function toggleKillSwitch(force?: boolean): Promise<void> {
  killSwitch = force ?? !killSwitch
  browser.setKillSwitch(killSwitch)
  try {
    await engine.request('computerUse/killSwitch', { engaged: killSwitch })
  } catch {}
  broadcast('odex:kill-switch', killSwitch)
  updateTray()
}

function notify(title: string, body: string, threadId?: string): void {
  if (!Notification.isSupported()) return
  const n = new Notification({ title, body, icon: resourcePath('icon.png'), silent: false })
  n.on('click', () => {
    const w = showMain()
    if (threadId) w.webContents.send('odex:command', { command: 'openThread', threadId })
  })
  n.show()
}

function quit(): void {
  setQuitting()
  app.quit()
}

// -------------------------------------------------------- engine events

engine.on('state', (s) => {
  broadcast('odex:engine-state', s)
  if (s?.state === 'ready') scheduleTrayUsage(500)
})
engine.on('notification', (method: string, params: any) => {
  // dev-server URLs printed by agent processes open in the browser panel only when enabled
  if (method === 'openUrl' && params?.source === 'devServer' && !getSettings().openDevServerUrls) return
  broadcast('odex:notification', { method, params })
  if (method === 'thread/updated' || method === 'thread/started') {
    const t = params.thread
    threadStatus.set(t.id, { status: t.status, name: t.name || t.preview || 'Thread' })
    updateTray()
  } else if (method === 'turn/completed') {
    scheduleTrayUsage()
    const s = getSettings()
    const t = threadStatus.get(params.threadId)
    const pref = s.notifyTurnComplete
    if (pref === 'always' || (pref === 'background' && !anyFocused())) {
      const status = params.turn?.status
      notify(status === 'failed' ? 'Turn failed' : 'Turn complete', t?.name || 'Thread', params.threadId)
    }
  } else if (method === 'openUrl') {
    if (params.target === 'external' && /^https?:/.test(params.url)) void shell.openExternal(params.url)
  }
})

engine.serverRequestHandler = async (method, params: any) => {
  switch (method) {
    case 'browser/execute':
      return browser.execute(params)
    case 'terminal/read':
      return terminals.readTerminalForAgent(params)
    case 'secrets/store':
      setSecret(params.key, params.value ?? null)
      return {}
    case 'approval/request': {
      if (getSettings().notifyApprovals && !anyFocused()) {
        const t = threadStatus.get(params.threadId)
        notify('Approval needed', t?.name || 'A thread is waiting for you', params.threadId)
      }
      return askRenderer(method, params)
    }
    default:
      return askRenderer(method, params)
  }
}

browser.setDownloadHandler(async (info) => {
  try {
    // The save dialog is the approval: nothing downloads unless the user picks a location.
    let host = info.url
    try {
      host = new URL(info.url).host || info.url
    } catch {}
    const opts = { title: `Download ${info.filename} from ${host}?`, buttonLabel: 'Download', defaultPath: path.join(app.getPath('downloads'), info.filename) }
    const w = BrowserWindow.getFocusedWindow() ?? mainWindow
    const res = w && !w.isDestroyed() ? await dialog.showSaveDialog(w, opts) : await dialog.showSaveDialog(opts)
    return res.canceled ? null : (res.filePath ?? null)
  } catch {
    return null
  }
})

// ------------------------------------------------------------------- IPC

function registerIpc(): void {
  ipcMain.handle('odex:request', async (_e, method: string, params: unknown) => {
    try {
      return { ok: true, result: await engine.request(method, params) }
    } catch (e) {
      const err = e as Error & { code?: number }
      return { ok: false, error: err.message, code: err.code }
    }
  })
  ipcMain.handle('odex:server-response', (_e, id: number, result: unknown, error?: string) => {
    const r = pendingServer.get(id)
    if (r) {
      pendingServer.delete(id)
      r({ result, error })
      // other windows can drop their copy of the prompt
      broadcast('odex:server-request-resolved', id)
    }
  })
  ipcMain.handle('odex:engine-info', () => ({ state: engine.state, error: engine.lastError, init: engine.init }))
  ipcMain.handle('odex:engine-restart', async () => {
    await engine.stop()
    await engine.start()
  })

  ipcMain.handle('settings:get', () => getSettings())
  ipcMain.handle('settings:set', (_e, patch) => setSettings(patch))
  ipcMain.handle('secrets:set', async (_e, key: string, value: string | null) => {
    setSecret(key, value)
    // the running engine picks the change up without a restart (it never logs values)
    if (engine.state === 'ready') await engine.request('secrets/set', { key, value: value || null }).catch(() => {})
  })
  ipcMain.handle('secrets:has', (_e, key: string) => hasSecret(key))

  ipcMain.handle('term:create', (_e, opts) => terminals.createTerminal({ ...opts, shell: opts.shell || getSettings().defaultTerminalShell }))
  ipcMain.handle('term:run', (_e, opts) => terminals.runInTerminal({ ...opts, shell: opts.shell || getSettings().defaultTerminalShell }))
  ipcMain.handle('term:write', (_e, id: string, data: string) => terminals.writeTerminal(id, data))
  ipcMain.handle('term:resize', (_e, id: string, cols: number, rows: number) => terminals.resizeTerminal(id, cols, rows))
  ipcMain.handle('term:kill', (_e, id: string) => terminals.killTerminal(id))
  ipcMain.handle('term:list', (_e, threadId?: string | null) => terminals.listTerminals(threadId))
  ipcMain.handle('term:buffer', (_e, id: string) => terminals.terminalBuffer(id))

  ipcMain.handle('browser:state', (e) => browser.state(BrowserWindow.fromWebContents(e.sender)?.id))
  ipcMain.handle('browser:new', (e, url: string) => browser.createTab(url || getSettings().browserHome, null, BrowserWindow.fromWebContents(e.sender)))
  ipcMain.handle('browser:show', (e, id: string | null) => {
    const w = BrowserWindow.fromWebContents(e.sender)
    if (w) browser.showTab(w, id)
  })
  ipcMain.handle('browser:bounds', (e, b: Electron.Rectangle | null) => {
    const w = BrowserWindow.fromWebContents(e.sender)
    if (w) browser.setBounds(w, b)
  })
  ipcMain.handle('browser:navigate', (_e, id: string, url: string) => browser.navigate(id, url))
  ipcMain.handle('browser:command', (_e, id: string, cmd) => browser.command(id, cmd))
  ipcMain.handle('browser:close', (_e, id: string) => browser.closeTab(id))
  ipcMain.handle('browser:pick', (_e, id: string) => browser.pickForComment(id))
  ipcMain.handle('browser:pickCancel', (_e, id: string) => browser.cancelPick(id))
  ipcMain.handle('app:killSwitchState', () => killSwitch)
  ipcMain.handle('browser:history', () => browser.readHistory())
  ipcMain.handle('browser:clearHistory', (_e, since?: number) => browser.clearHistory(since))
  ipcMain.handle('browser:clearData', () => browser.clearBrowsingData())

  ipcMain.handle('dialog:openFolder', async (e, opts?: { multi?: boolean }) => {
    const w = BrowserWindow.fromWebContents(e.sender)!
    const r = await dialog.showOpenDialog(w, { properties: ['openDirectory', 'createDirectory', ...(opts?.multi ? (['multiSelections'] as const) : [])] })
    return r.canceled ? [] : r.filePaths
  })
  ipcMain.handle('dialog:openFiles', async (e) => {
    const w = BrowserWindow.fromWebContents(e.sender)!
    const r = await dialog.showOpenDialog(w, { properties: ['openFile', 'multiSelections'] })
    return r.canceled ? [] : r.filePaths
  })
  // save renderer content (e.g. a chat image) where the user picks; the dialog is the approval
  ipcMain.handle('dialog:saveFile', async (e, opts: { defaultName: string; dataUrl?: string; text?: string }) => {
    const w = BrowserWindow.fromWebContents(e.sender)!
    const ext = path.extname(opts.defaultName).slice(1)
    const r = await dialog.showSaveDialog(w, {
      defaultPath: path.join(app.getPath('downloads'), path.basename(opts.defaultName)),
      filters: ext ? [{ name: ext.toUpperCase(), extensions: [ext] }, { name: 'All files', extensions: ['*'] }] : undefined,
    })
    if (r.canceled || !r.filePath) return null
    const m = opts.dataUrl ? /^data:[^;,]*(;base64)?,(.*)$/s.exec(opts.dataUrl) : null
    const data = m ? (m[1] ? Buffer.from(m[2], 'base64') : Buffer.from(decodeURIComponent(m[2]))) : Buffer.from(opts.text ?? '')
    await fs.promises.writeFile(r.filePath, data)
    return r.filePath
  })
  ipcMain.handle('shell:openExternal', (_e, url: string) => {
    if (/^(https?|mailto):/.test(url)) void shell.openExternal(url)
  })
  ipcMain.handle('shell:openPath', (_e, p: string) => shell.openPath(p))
  ipcMain.handle('shell:showItem', (_e, p: string) => shell.showItemInFolder(p))
  // command templates (`code -g {file}:{line}`, `vim +{line} {file}`), per-argument quoting and
  // per-project overrides (Edit project → General) live in editor.ts
  ipcMain.handle('shell:openInEditor', (_e, p: string, line?: number) => openInEditor(p, line, getSettings(), (m, params) => engine.request(m, params)))

  ipcMain.handle('fs:read', async (_e, p: string, maxBytes = 5 * 1024 * 1024) => {
    const st = await fs.promises.stat(p)
    if (st.isDirectory()) return { kind: 'dir', entries: (await fs.promises.readdir(p, { withFileTypes: true })).map((d) => ({ name: d.name, dir: d.isDirectory() })) }
    const ext = path.extname(p).toLowerCase()
    const mime: Record<string, string> = { '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.webp': 'image/webp', '.svg': 'image/svg+xml', '.pdf': 'application/pdf', '.bmp': 'image/bmp', '.ico': 'image/x-icon' }
    if (mime[ext]) {
      if (st.size > 30 * 1024 * 1024) return { kind: 'binary', size: st.size }
      const b = await fs.promises.readFile(p)
      return { kind: 'media', mime: mime[ext], dataUrl: `data:${mime[ext]};base64,${b.toString('base64')}`, size: st.size }
    }
    if (st.size > maxBytes) return { kind: 'tooLarge', size: st.size }
    const buf = await fs.promises.readFile(p)
    if (buf.subarray(0, 8000).includes(0)) return { kind: 'binary', size: st.size }
    return { kind: 'text', text: buf.toString('utf8'), size: st.size, mtime: st.mtimeMs }
  })
  ipcMain.handle('fs:write', async (_e, p: string, text: string) => {
    await fs.promises.writeFile(p, text, 'utf8')
    return (await fs.promises.stat(p)).mtimeMs
  })
  ipcMain.handle('fs:exists', (_e, p: string) => fs.existsSync(p))
  ipcMain.handle('fs:list', (_e, dir: string) => fsw.listDir(dir))
  ipcMain.handle('fs:stat', (_e, p: string) => fsw.statPath(p))
  ipcMain.handle('fs:watch', (e, p: string, ignore?: string[]) => fsw.watchPath(e.sender, p, ignore))
  ipcMain.handle('fs:unwatch', (e, p: string) => fsw.unwatchPath(e.sender, p))

  ipcMain.handle('win:new', (_e, threadId?: string, panel?: string) => {
    createWindow({ threadId, popout: true, panel: panel === 'review' ? panel : undefined })
  })
  ipcMain.handle('win:quickChat', () => {
    openQuickChat()
  })
  // e.g. "Open in Odex" from the Quick Chat window
  ipcMain.handle('win:openInMain', (_e, threadId: string) => {
    showMain().webContents.send('odex:command', { command: 'openThread', threadId })
  })
  registerPreviewIpc()
  ipcMain.handle('win:alwaysOnTop', (e, on: boolean) => BrowserWindow.fromWebContents(e.sender)?.setAlwaysOnTop(on))
  ipcMain.handle('win:close', (e) => BrowserWindow.fromWebContents(e.sender)?.close())
  ipcMain.handle('win:zoom', (e, factor: number) => {
    BrowserWindow.fromWebContents(e.sender)?.webContents.setZoomFactor(factor)
    setSettings({ zoom: factor })
  })
  ipcMain.handle('win:fullscreen', (e) => {
    const w = BrowserWindow.fromWebContents(e.sender)
    if (w) w.setFullScreen(!w.isFullScreen())
  })
  ipcMain.handle('app:info', () => ({ version: app.getVersion(), platform: process.platform, odexHome: odexHome(), isPackaged: app.isPackaged, logs: path.join(odexHome(), 'logs') }))
  ipcMain.handle('app:quit', () => quit())
  ipcMain.handle('app:killSwitch', (_e, on?: boolean) => toggleKillSwitch(on))
  ipcMain.handle('app:theme', () => (nativeTheme.shouldUseDarkColors ? 'dark' : 'light'))

  ipcMain.handle('update:state', () => updateState())
  ipcMain.handle('update:check', () => checkForUpdates())
  ipcMain.handle('update:download', () => downloadUpdate())
  ipcMain.handle('update:install', () => installUpdate())
}

function registerShortcuts(): void {
  globalShortcut.unregisterAll()
  const s = getSettings()
  if (s.killSwitchHotkey) {
    try {
      globalShortcut.register(s.killSwitchHotkey, () => void toggleKillSwitch(true))
    } catch {}
  }
  if (s.appshotHotkey) {
    try {
      globalShortcut.register(s.appshotHotkey, async () => {
        try {
          const r = await engine.request<{ appshot: unknown }>('appshot/capture', { includeUiTree: true })
          const w = showMain()
          w.webContents.send('odex:appshot', r.appshot)
        } catch (e) {
          notify('Appshot failed', (e as Error).message)
        }
      })
    } catch {}
  }
  if (s.quickChatHotkey) {
    try {
      globalShortcut.register(s.quickChatHotkey, () => void openQuickChat())
    } catch {}
  }
}

// -------------------------------------------------------------------- app

app.whenReady().then(async () => {
  Menu.setApplicationMenu(null)
  handlePreviewProtocol()
  registerIpc()
  nativeTheme.on('updated', () => {
    updateTitleBars()
    broadcast('odex:native-theme', nativeTheme.shouldUseDarkColors ? 'dark' : 'light')
  })
  onSettings(() => {
    updateTitleBars()
    registerShortcuts()
    updateTray()
  })
  void engine.start()
  initUpdater((u) => {
    if (!anyFocused()) notify('Update ready', `Odex ${u.version} has been downloaded. Open Odex to restart and install it.`)
  })
  // the tray offers "Restart to update" once an update is downloaded
  let updateStatus = updateState().status
  onUpdateState((u) => {
    if (u.status !== updateStatus) updateTray()
    updateStatus = u.status
  })
  createWindow()
  try {
    const img = nativeImage.createFromPath(resourcePath(process.platform === 'darwin' ? 'tray-16.png' : 'icon-32.png'))
    tray = new Tray(img)
    tray.on('click', () => showMain())
    updateTray()
  } catch {}
  registerShortcuts()
  const link = process.argv.find((a) => a.startsWith('odex://'))
  if (link) setTimeout(() => mainWindow?.webContents.send('odex:deeplink', link), 1500)
})

app.on('window-all-closed', () => {
  if (!getSettings().keepRunningInTray || process.env.ODEX_E2E) quit()
})

app.on('before-quit', () => {
  setQuitting()
})

app.on('will-quit', async (e) => {
  globalShortcut.unregisterAll()
  if (engine.state !== 'stopped') {
    e.preventDefault()
    terminals.killAllTerminals()
    await engine.stop()
    app.exit(0)
  }
})

app.on('activate', () => showMain())
