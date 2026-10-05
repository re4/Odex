import { app, BaseWindow, BrowserWindow, WebContentsView, session } from 'electron'
import fs from 'node:fs'
import path from 'node:path'
import { odexHome } from './paths'

/**
 * The in-app browser: tabs are WebContentsViews in their own persistent
 * partition (separate from the app and from the user's browser). The agent
 * drives tabs over CDP (webContents.debugger) via `browser/execute`
 * requests from the engine; users browse, annotate and inspect them.
 *
 * The renderer reports where the browser panel is (`setBounds`, CSS pixels)
 * and which tab it shows (`showTab`); the active tab's view is attached to
 * the window only while the panel is visible and nothing overlays it.
 */

export const PARTITION = 'persist:odex-browser'

/** A tab counts as "agent active" this long after its last agent command. */
const AGENT_ACTIVE_MS = 4000
/** Size given to tabs that have never been shown, so pages lay out for the agent. */
const DEFAULT_SIZE = { width: 1280, height: 800 }

interface Tab {
  id: string
  view: WebContentsView
  threadId: string | null
  events: Array<{ method: string; params: unknown }>
  attached: boolean
  listening: boolean
  windowId: number | null
  agentAt: number
  crashed: boolean
}

interface HistoryEntry {
  url: string
  title: string
  at: number
}

export interface TabInfo {
  id: string
  url: string
  title: string
  active: boolean
  threadId: string | null
  agentActive: boolean
  loading: boolean
  crashed: boolean
  canGoBack: boolean
  canGoForward: boolean
}

export interface BrowserState {
  tabs: TabInfo[]
  activeId: string | null
  /** The active tab's view is attached to this window (not hidden). */
  visible: boolean
  /** Where the view is placed, in window DIPs (null = hidden). */
  bounds: Electron.Rectangle | null
}

const tabs = new Map<string, Tab>()
const activeByWindow = new Map<number, string>()
const boundsByWindow = new Map<number, Electron.Rectangle | null>()
const visibleByWindow = new Set<number>()
const trackedWindows = new Set<number>()
const agentTabByThread = new Map<string, string>()
let counter = 0
let killed = false
let agentTimer: NodeJS.Timeout | null = null
let downloadAsk: ((info: { url: string; filename: string }) => Promise<string | null>) | null = null

export function setDownloadHandler(h: typeof downloadAsk): void {
  downloadAsk = h
}

// ------------------------------------------------------------ history

function historyFile(): string {
  return path.join(odexHome(), 'browser-history.json')
}

export function readHistory(): HistoryEntry[] {
  try {
    return JSON.parse(fs.readFileSync(historyFile(), 'utf8')) as HistoryEntry[]
  } catch {
    return []
  }
}

function pushHistory(url: string, title: string): void {
  if (!url || url.startsWith('about:') || url.startsWith('devtools:') || url.startsWith('data:')) return
  const h = readHistory()
  h.push({ url, title, at: Date.now() })
  try {
    fs.writeFileSync(historyFile(), JSON.stringify(h.slice(-5000)))
  } catch {}
}

/** Update the title of the latest entry for `url` (titles arrive after navigation). */
function titleHistory(url: string, title: string): void {
  if (!title || !url) return
  const h = readHistory()
  for (let i = h.length - 1; i >= Math.max(0, h.length - 20); i--) {
    if (h[i].url === url) {
      if (h[i].title === title) return
      h[i].title = title
      try {
        fs.writeFileSync(historyFile(), JSON.stringify(h))
      } catch {}
      return
    }
  }
}

/** Clear all history, or only entries visited at or after `sinceMs`. */
export function clearHistory(sinceMs?: number): void {
  if (!sinceMs) {
    fs.writeFileSync(historyFile(), '[]')
  } else {
    fs.writeFileSync(historyFile(), JSON.stringify(readHistory().filter((e) => e.at < sinceMs)))
  }
}

export async function clearBrowsingData(): Promise<void> {
  await session.fromPartition(PARTITION).clearStorageData()
  await session.fromPartition(PARTITION).clearCache()
}

let sessionReady = false
function setupSession(): void {
  if (sessionReady) return
  sessionReady = true
  const ses = session.fromPartition(PARTITION)
  ses.on('will-download', (event, item) => {
    item.pause()
    const ask = downloadAsk
    if (!ask) {
      item.cancel()
      return
    }
    void ask({ url: item.getURL(), filename: item.getFilename() }).then((savePath) => {
      if (savePath) {
        item.setSavePath(savePath)
        item.resume()
      } else item.cancel()
    })
    void event
  })
  // deny permission prompts (camera, geolocation, …) by default
  ses.setPermissionRequestHandler((_wc, perm, cb) => cb(perm === 'clipboard-sanitized-write' || perm === 'fullscreen'))
}

// -------------------------------------------------------------- input

/**
 * Turn what the user typed into a URL: `3000` / `:3000` → localhost, loopback
 * and LAN hosts → http, `example.com` → https, anything else → a web search.
 */
export function normalizeUrl(raw: string): string {
  const u = raw.trim()
  if (!u) return 'about:blank'
  if (/^:?\d{2,5}([/?#].*)?$/.test(u)) return `http://localhost${u.startsWith(':') ? '' : ':'}${u}`
  if (/^(localhost|[\w-]+\.localhost|0\.0\.0\.0|\[[0-9a-f:]+\]|(\d{1,3}\.){3}\d{1,3})(:\d{1,5})?([/?#].*)?$/i.test(u)) return `http://${u}`
  if (/^(https?|file|about|data|view-source|blob):/i.test(u)) return u
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(u)) return u
  if (!/\s/.test(u) && /^[\w-]+(\.[\w-]+)*\.[a-z]{2,}(:\d{1,5})?([/?#].*)?$/i.test(u)) return `https://${u}`
  return `https://duckduckgo.com/?q=${encodeURIComponent(u)}`
}

/** App shortcuts that keep working while the page has focus (forwarded to the UI). */
const FORWARDED_KEYS = new Set(['Mod+K', 'Mod+Shift+P', 'Mod+T', 'Mod+W', 'Mod+L', 'Mod+N', 'Mod+B', 'Mod+J', 'Mod+,', 'Mod+/', 'Mod+`', 'Mod+Shift+B', 'Mod+Shift+E', 'F11'])

function keyString(input: Electron.Input): string {
  const mac = process.platform === 'darwin'
  const parts: string[] = []
  if (mac ? input.meta : input.control) parts.push('Mod')
  if (input.alt) parts.push('Alt')
  if (input.shift) parts.push('Shift')
  let k = input.key
  if (input.code === 'Backquote') k = '`'
  else if (input.code === 'Comma') k = ','
  else if (input.code === 'Slash') k = '/'
  else if (k.length === 1) k = k.toUpperCase()
  parts.push(k)
  return parts.join('+')
}

function handleKeys(t: Tab): void {
  const wc = t.view.webContents
  wc.on('before-input-event', (e, input) => {
    if (input.type !== 'keyDown') return
    const mod = process.platform === 'darwin' ? input.meta : input.control
    const k = input.key.length === 1 ? input.key.toLowerCase() : input.key
    if (k === 'F5' || (mod && !input.alt && k === 'r')) {
      e.preventDefault()
      command(t.id, input.shift ? 'hardReload' : 'reload')
      return
    }
    if (k === 'F12' || (mod && input.shift && k === 'i')) {
      e.preventDefault()
      command(t.id, 'devtools')
      return
    }
    if (input.alt && !mod && (k === 'ArrowLeft' || k === 'ArrowRight')) {
      e.preventDefault()
      command(t.id, k === 'ArrowLeft' ? 'back' : 'forward')
      return
    }
    const keys = keyString(input)
    if (!FORWARDED_KEYS.has(keys)) return
    const win = t.windowId != null ? BrowserWindow.fromId(t.windowId) : null
    if (!win || win.isDestroyed()) return
    e.preventDefault()
    win.webContents.focus()
    win.webContents.send('odex:browser-key', { tabId: t.id, keys })
  })
}

// --------------------------------------------------------------- tabs

function alive(t: Tab): boolean {
  return !t.view.webContents.isDestroyed()
}

function tabInfo(t: Tab): TabInfo {
  const wc = t.view.webContents
  const active = t.windowId != null && activeByWindow.get(t.windowId) === t.id
  const agentActive = t.threadId != null && Date.now() - t.agentAt < AGENT_ACTIVE_MS
  if (!alive(t)) {
    return { id: t.id, url: '', title: 'Closed', active, threadId: t.threadId, agentActive: false, loading: false, crashed: true, canGoBack: false, canGoForward: false }
  }
  const url = wc.getURL()
  return {
    id: t.id,
    url,
    title: wc.getTitle() || url,
    active,
    threadId: t.threadId,
    agentActive,
    loading: wc.isLoading(),
    crashed: t.crashed,
    canGoBack: wc.navigationHistory.canGoBack(),
    canGoForward: wc.navigationHistory.canGoForward(),
  }
}

export function state(windowId?: number): BrowserState {
  return {
    tabs: [...tabs.values()].map(tabInfo),
    activeId: windowId != null ? (activeByWindow.get(windowId) ?? null) : null,
    visible: windowId != null && visibleByWindow.has(windowId),
    bounds: windowId != null && visibleByWindow.has(windowId) ? (boundsByWindow.get(windowId) ?? null) : null,
  }
}

function broadcastState(): void {
  for (const w of BrowserWindow.getAllWindows()) {
    if (!w.isDestroyed()) w.webContents.send('odex:browser-state', state(w.id))
  }
}

/** Forget a window's tabs when it closes (tabs survive and can be shown elsewhere). */
function trackWindow(win: BrowserWindow): void {
  if (trackedWindows.has(win.id)) return
  const wid = win.id
  trackedWindows.add(wid)
  win.once('closed', () => {
    trackedWindows.delete(wid)
    activeByWindow.delete(wid)
    boundsByWindow.delete(wid)
    visibleByWindow.delete(wid)
    for (const t of tabs.values()) {
      if (t.windowId === wid) {
        t.windowId = null
        park(t)
      }
    }
    // the hidden host window must not keep the app alive
    if (!BrowserWindow.getAllWindows().some((w) => w.id !== wid && !w.isDestroyed()) && parking && !parking.isDestroyed()) {
      parking.destroy()
      parking = null
    }
    broadcastState()
  })
}
app.on('browser-window-created', (_e, w) => trackWindow(w))

/**
 * Tabs that are not on screen live in a hidden host window so they keep a
 * compositor: pages lay out at a real size and can still be captured.
 */
let parking: BaseWindow | null = null

function park(t: Tab): void {
  if (!alive(t)) return
  if (!parking || parking.isDestroyed()) {
    if (!BrowserWindow.getAllWindows().length) return
    parking = new BaseWindow({ show: false, width: DEFAULT_SIZE.width, height: DEFAULT_SIZE.height, skipTaskbar: true, focusable: false, title: 'Odex browser' })
  }
  try {
    parking.contentView.addChildView(t.view)
  } catch {}
}

function unpark(t: Tab): void {
  try {
    if (parking && !parking.isDestroyed()) parking.contentView.removeChildView(t.view)
  } catch {}
}

function detach(win: BrowserWindow, t: Tab): void {
  try {
    win.contentView.removeChildView(t.view)
  } catch {}
  park(t)
}

/** Whether the tab is currently on screen in an app window. */
function onScreen(t: Tab): boolean {
  return t.windowId != null && visibleByWindow.has(t.windowId) && activeByWindow.get(t.windowId) === t.id
}

function withTimeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  return Promise.race([p, new Promise<T>((_r, reject) => setTimeout(() => reject(new Error(`${what} timed out`)), ms))])
}

/**
 * `Page.captureScreenshot` for a tab that is not on screen: CDP would wait for
 * a frame that never comes, so capture the page directly (viewport only).
 */
async function hiddenScreenshot(t: Tab, p: { clip?: { x: number; y: number; width: number; height: number }; captureBeyondViewport?: boolean } | undefined): Promise<{ data: string }> {
  const wc = t.view.webContents
  let rect: Electron.Rectangle | undefined
  if (p?.clip && !p.captureBeyondViewport) {
    const [sx, sy] = (await wc.executeJavaScript('[scrollX, scrollY]', true)) as [number, number]
    rect = { x: Math.max(0, Math.round(p.clip.x - sx)), y: Math.max(0, Math.round(p.clip.y - sy)), width: Math.max(1, Math.round(p.clip.width)), height: Math.max(1, Math.round(p.clip.height)) }
  }
  const img = await withTimeout(wc.capturePage(rect, { stayHidden: true }), 15_000, 'screenshot')
  return { data: img.toPNG().toString('base64') }
}

/** Attach the window's active tab if the panel has room for it, else detach it. */
function place(win: BrowserWindow): void {
  if (win.isDestroyed()) return
  const id = activeByWindow.get(win.id)
  const t = id ? tabs.get(id) : undefined
  const b = boundsByWindow.get(win.id)
  if (!t || !alive(t) || !b || b.width <= 0 || b.height <= 0) {
    if (t) detach(win, t)
    visibleByWindow.delete(win.id)
    return
  }
  unpark(t)
  win.contentView.addChildView(t.view)
  t.view.setBounds(b)
  visibleByWindow.add(win.id)
}

export function createTab(url: string, threadId: string | null = null, win?: BrowserWindow | null): string {
  setupSession()
  const id = `b${++counter}`
  const view = new WebContentsView({ webPreferences: { partition: PARTITION, sandbox: true, contextIsolation: true } })
  const t: Tab = { id, view, threadId, events: [], attached: false, listening: false, windowId: null, agentAt: 0, crashed: false }
  tabs.set(id, t)
  // give never-shown tabs a real viewport so the agent can lay out and click
  const last = win ? boundsByWindow.get(win.id) : null
  view.setBounds({ x: 0, y: 0, width: last?.width || DEFAULT_SIZE.width, height: last?.height || DEFAULT_SIZE.height })
  park(t)
  const wc = view.webContents
  wc.on('did-navigate', () => {
    t.crashed = false
    pushHistory(wc.getURL(), wc.getTitle())
    broadcastState()
  })
  wc.on('did-navigate-in-page', (_e, _url, isMainFrame) => {
    if (isMainFrame) broadcastState()
  })
  wc.on('page-title-updated', (_e, title) => {
    titleHistory(wc.getURL(), title)
    broadcastState()
  })
  wc.on('did-start-loading', broadcastState)
  wc.on('did-stop-loading', broadcastState)
  wc.on('render-process-gone', () => {
    t.crashed = true
    broadcastState()
  })
  wc.setWindowOpenHandler(({ url: u }) => {
    createTab(u, t.threadId, t.windowId != null ? BrowserWindow.fromId(t.windowId) : null)
    return { action: 'deny' }
  })
  handleKeys(t)
  void wc.loadURL(url && url !== 'about:blank' ? normalizeUrl(url) : 'about:blank').catch(() => {})
  if (win) showTab(win, id)
  broadcastState()
  return id
}

export function showTab(win: BrowserWindow, id: string | null): void {
  trackWindow(win)
  const prev = activeByWindow.get(win.id)
  if (prev && prev !== id) {
    const pt = tabs.get(prev)
    if (pt) {
      detach(win, pt)
      if (pt.windowId === win.id) pt.windowId = null
    }
  }
  if (!id) {
    activeByWindow.delete(win.id)
    visibleByWindow.delete(win.id)
    broadcastState()
    return
  }
  const t = tabs.get(id)
  if (!t) return
  // move it out of another window if needed
  if (t.windowId != null && t.windowId !== win.id) {
    const other = BrowserWindow.fromId(t.windowId)
    if (other && !other.isDestroyed()) {
      detach(other, t)
      visibleByWindow.delete(other.id)
    }
    activeByWindow.delete(t.windowId)
  }
  t.windowId = win.id
  activeByWindow.set(win.id, id)
  place(win)
  broadcastState()
}

/**
 * Renderer reports where the browser panel is, in CSS pixels (null = hidden).
 * Views are positioned in window DIPs, so scale by the UI zoom factor.
 */
export function setBounds(win: BrowserWindow, b: Electron.Rectangle | null): void {
  trackWindow(win)
  const wasVisible = visibleByWindow.has(win.id)
  if (!b || b.width <= 0 || b.height <= 0) {
    boundsByWindow.set(win.id, null)
  } else {
    const z = win.webContents.getZoomFactor() || 1
    const x = Math.round(b.x * z)
    const y = Math.round(b.y * z)
    boundsByWindow.set(win.id, { x, y, width: Math.round((b.x + b.width) * z) - x, height: Math.round((b.y + b.height) * z) - y })
  }
  place(win)
  if (wasVisible !== visibleByWindow.has(win.id)) broadcastState()
}

export function closeTab(id: string): void {
  const t = tabs.get(id)
  if (!t) return
  if (t.windowId != null) {
    const w = BrowserWindow.fromId(t.windowId)
    if (w && !w.isDestroyed()) detach(w, t)
    if (activeByWindow.get(t.windowId) === id) {
      activeByWindow.delete(t.windowId)
      visibleByWindow.delete(t.windowId)
    }
  }
  for (const [th, tid] of agentTabByThread) if (tid === id) agentTabByThread.delete(th)
  unpark(t)
  tabs.delete(id)
  try {
    t.view.webContents.close()
  } catch {}
  broadcastState()
}

export function navigate(id: string, url: string): void {
  const t = tabs.get(id)
  if (!t || !alive(t)) return
  void t.view.webContents.loadURL(normalizeUrl(url)).catch(() => {})
}

export function command(id: string, cmd: 'back' | 'forward' | 'reload' | 'hardReload' | 'stop' | 'devtools'): void {
  const t = tabs.get(id)
  if (!t || !alive(t)) return
  const wc = t.view.webContents
  switch (cmd) {
    case 'back':
      if (wc.navigationHistory.canGoBack()) wc.navigationHistory.goBack()
      break
    case 'forward':
      if (wc.navigationHistory.canGoForward()) wc.navigationHistory.goForward()
      break
    case 'reload':
      wc.reload()
      break
    case 'hardReload':
      wc.reloadIgnoringCache()
      break
    case 'stop':
      wc.stop()
      break
    case 'devtools':
      if (wc.isDevToolsOpened()) wc.closeDevTools()
      else wc.openDevTools({ mode: 'detach' })
      break
  }
}

// ---------------------------------------------------------------- CDP

async function ensureDebugger(t: Tab): Promise<void> {
  const dbg = t.view.webContents.debugger
  if (t.attached && dbg.isAttached()) return
  if (!dbg.isAttached()) dbg.attach('1.3')
  t.attached = true
  if (!t.listening) {
    t.listening = true
    dbg.on('message', (_e, method, params) => {
      t.events.push({ method, params })
      if (t.events.length > 800) t.events.splice(0, t.events.length - 800)
    })
    dbg.on('detach', () => {
      t.attached = false
    })
    // keep timers and rendering going while the agent works in a hidden tab
    t.view.webContents.setBackgroundThrottling(false)
  }
  for (const d of ['Page.enable', 'Runtime.enable', 'Network.enable', 'DOM.enable']) {
    try {
      await dbg.sendCommand(d)
    } catch {}
  }
}

/** Note agent activity on a tab (drives the "agent is using this tab" indicator). */
function markAgent(t: Tab): void {
  const was = Date.now() - t.agentAt < AGENT_ACTIVE_MS
  t.agentAt = Date.now()
  if (!was) broadcastState()
  if (!agentTimer) {
    const tick = () => {
      const now = Date.now()
      const next = [...tabs.values()].filter((x) => now - x.agentAt < AGENT_ACTIVE_MS).map((x) => x.agentAt + AGENT_ACTIVE_MS - now)
      broadcastState()
      agentTimer = next.length ? setTimeout(tick, Math.min(...next) + 50) : null
    }
    agentTimer = setTimeout(tick, AGENT_ACTIVE_MS + 50)
  }
}

function agentTab(threadId: string): Tab {
  const existing = agentTabByThread.get(threadId)
  if (existing && tabs.has(existing) && alive(tabs.get(existing)!)) return tabs.get(existing)!
  const win = BrowserWindow.getFocusedWindow() || BrowserWindow.getAllWindows()[0] || null
  const id = createTab('about:blank', threadId, win)
  agentTabByThread.set(threadId, id)
  return tabs.get(id)!
}

/** Kill switch: stop and refuse all agent browser actions. */
export function setKillSwitch(on: boolean): void {
  killed = on
  if (!on) return
  for (const t of tabs.values()) {
    if (!alive(t)) continue
    if (t.threadId) {
      try {
        t.view.webContents.stop()
      } catch {}
    }
    if (t.attached) {
      try {
        t.view.webContents.debugger.detach()
      } catch {}
      t.attached = false
    }
    t.agentAt = 0
  }
  broadcastState()
}

/** Handle the engine's `browser/execute` server request (CDP transport). */
export async function execute(params: { threadId: string; action: string; args: any }): Promise<unknown> {
  const ok = (value: unknown) => ({ ok: true, text: JSON.stringify(value ?? {}) })
  if (killed) return { ok: false, error: 'the kill switch is engaged; browser actions are stopped' }
  const args = params.args ?? {}
  try {
    switch (params.action) {
      case 'cdp': {
        const t = agentTab(params.threadId)
        await ensureDebugger(t)
        markAgent(t)
        if (args.method === 'Page.captureScreenshot' && !onScreen(t)) return ok(await hiddenScreenshot(t, args.params))
        const r = await t.view.webContents.debugger.sendCommand(args.method, args.params || {})
        return ok(r)
      }
      case 'events': {
        const t = agentTab(params.threadId)
        const ev = t.events.splice(0, t.events.length)
        return ok(ev)
      }
      case 'tabs': {
        const mine = [...tabs.values()].filter((t) => t.threadId === params.threadId && alive(t))
        const current = agentTabByThread.get(params.threadId)
        return ok(mine.map((t) => ({ id: t.id, url: t.view.webContents.getURL(), title: t.view.webContents.getTitle(), active: t.id === current })))
      }
      case 'new_tab': {
        const win = BrowserWindow.getFocusedWindow() || BrowserWindow.getAllWindows()[0] || null
        const id = createTab(args.url || 'about:blank', params.threadId, win)
        agentTabByThread.set(params.threadId, id)
        const t = tabs.get(id)!
        await ensureDebugger(t)
        markAgent(t)
        return ok({ id, url: args.url || 'about:blank', title: '', active: true })
      }
      case 'select_tab': {
        const t = tabs.get(args.id)
        if (!t || !alive(t)) throw new Error('no such tab')
        agentTabByThread.set(params.threadId, args.id)
        markAgent(t)
        return ok({})
      }
      case 'close_tab': {
        closeTab(args.id)
        return ok({})
      }
      default:
        throw new Error(`unknown browser action ${params.action}`)
    }
  } catch (e) {
    return { ok: false, error: (e as Error).message }
  }
}

// ------------------------------------------------------ comment mode

const PICKER = `(() => new Promise((resolve) => {
  if (window.__odexPickCancel) window.__odexPickCancel();
  const box = document.createElement('div');
  box.style.cssText = 'position:fixed;pointer-events:none;z-index:2147483647;border:2px solid #4f6bed;background:rgba(79,107,237,.12);border-radius:3px;transition:all .05s;left:-10px;top:-10px;width:0;height:0';
  document.documentElement.appendChild(box);
  const prevCursor = document.documentElement.style.cursor;
  document.documentElement.style.cursor = 'crosshair';
  let start = null;
  const sel = (el) => {
    if (!el || el === document.body) return 'body';
    if (el.id) return '#' + CSS.escape(el.id);
    const parts = [];
    while (el && el.nodeType === 1 && parts.length < 5 && el !== document.body) {
      let p = el.tagName.toLowerCase();
      if (el.id) { parts.unshift('#' + CSS.escape(el.id)); break; }
      if (el.classList.length) p += '.' + [...el.classList].slice(0, 2).map(c => CSS.escape(c)).join('.');
      const sib = el.parentElement ? [...el.parentElement.children].filter(c => c.tagName === el.tagName) : [];
      if (sib.length > 1) p += ':nth-of-type(' + (sib.indexOf(el) + 1) + ')';
      parts.unshift(p); el = el.parentElement;
    }
    return parts.join(' > ');
  };
  const move = (e) => {
    if (start) { const x = Math.min(start.x, e.clientX), y = Math.min(start.y, e.clientY);
      Object.assign(box.style, { left: x + 'px', top: y + 'px', width: Math.abs(e.clientX - start.x) + 'px', height: Math.abs(e.clientY - start.y) + 'px' }); return; }
    const el = document.elementFromPoint(e.clientX, e.clientY); if (!el) return;
    const r = el.getBoundingClientRect();
    Object.assign(box.style, { left: r.left + 'px', top: r.top + 'px', width: r.width + 'px', height: r.height + 'px' });
  };
  const block = (e) => { e.preventDefault(); e.stopPropagation(); };
  const down = (e) => { block(e); start = { x: e.clientX, y: e.clientY }; };
  const up = (e) => {
    block(e);
    const dragged = start && (Math.abs(e.clientX - start.x) > 6 || Math.abs(e.clientY - start.y) > 6);
    let result;
    if (dragged) {
      const x = Math.min(start.x, e.clientX), y = Math.min(start.y, e.clientY);
      result = { selector: null, bounds: { x, y, width: Math.abs(e.clientX - start.x), height: Math.abs(e.clientY - start.y) }, text: '' };
    } else {
      const el = document.elementFromPoint(e.clientX, e.clientY) || document.body;
      const r = el.getBoundingClientRect();
      result = { selector: sel(el), bounds: { x: r.left, y: r.top, width: r.width, height: r.height }, text: (el.innerText || '').slice(0, 300) };
    }
    cleanup(); resolve(result);
  };
  const key = (e) => { if (e.key === 'Escape') { block(e); cleanup(); resolve(null); } };
  const cleanup = () => {
    box.remove(); document.documentElement.style.cursor = prevCursor; delete window.__odexPickCancel;
    removeEventListener('mousemove', move, true); removeEventListener('mousedown', down, true); removeEventListener('mouseup', up, true);
    removeEventListener('keydown', key, true);
    // the click that follows the final mouseup must not reach the page either
    setTimeout(() => removeEventListener('click', block, true), 300);
  };
  window.__odexPickCancel = () => { cleanup(); resolve(null); };
  addEventListener('mousemove', move, true); addEventListener('mousedown', down, true); addEventListener('mouseup', up, true);
  addEventListener('click', block, true); addEventListener('keydown', key, true);
}))()`

/** Let the user pick an element or drag a region; returns selector, bounds and a screenshot. */
export async function pickForComment(id: string): Promise<unknown> {
  const t = tabs.get(id)
  if (!t || !alive(t)) return null
  const wc = t.view.webContents
  wc.focus()
  let stop: () => void = () => {}
  const interrupted = new Promise<null>((resolve) => {
    const nav = (e: Electron.Event<Electron.WebContentsDidStartNavigationEventParams>) => {
      if (e.isMainFrame && !e.isSameDocument) resolve(null)
    }
    const gone = () => resolve(null)
    wc.on('did-start-navigation', nav)
    wc.once('destroyed', gone)
    stop = () => {
      wc.removeListener('did-start-navigation', nav)
      wc.removeListener('destroyed', gone)
    }
  })
  let picked: { selector: string | null; bounds: Electron.Rectangle; text: string } | null
  try {
    picked = await Promise.race([wc.executeJavaScript(PICKER, true) as Promise<typeof picked>, interrupted])
  } catch {
    picked = null
  } finally {
    stop()
  }
  if (!picked || wc.isDestroyed()) return null
  const pad = 8
  const rect = {
    x: Math.max(0, Math.floor(picked.bounds.x - pad)),
    y: Math.max(0, Math.floor(picked.bounds.y - pad)),
    width: Math.max(1, Math.ceil(picked.bounds.width + pad * 2)),
    height: Math.max(1, Math.ceil(picked.bounds.height + pad * 2)),
  }
  let shot: string | null = null
  try {
    shot = (await wc.capturePage(rect)).toDataURL()
  } catch {}
  return { url: wc.getURL(), title: wc.getTitle(), selector: picked.selector, bounds: picked.bounds, text: picked.text, screenshotUrl: shot }
}

/** Abort a running pick (the page script resolves with null). */
export function cancelPick(id: string): void {
  const t = tabs.get(id)
  if (!t || !alive(t)) return
  void t.view.webContents.executeJavaScript('window.__odexPickCancel && window.__odexPickCancel()', true).catch(() => {})
}

export function tabForThread(threadId: string): string | null {
  return agentTabByThread.get(threadId) ?? null
}
