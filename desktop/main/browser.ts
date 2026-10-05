import { BrowserWindow, WebContentsView, session } from 'electron'
import fs from 'node:fs'
import path from 'node:path'
import { odexHome } from './paths'

/**
 * The in-app browser: tabs are WebContentsViews in their own persistent
 * partition (separate from the app and from the user's browser). The agent
 * drives tabs over CDP (webContents.debugger) via `browser/execute`
 * requests from the engine; users browse, annotate and inspect them.
 */

export const PARTITION = 'persist:odex-browser'

interface Tab {
  id: string
  view: WebContentsView
  threadId: string | null
  events: Array<{ method: string; params: unknown }>
  attached: boolean
  windowId: number | null
}

interface HistoryEntry {
  url: string
  title: string
  at: number
}

const tabs = new Map<string, Tab>()
const activeByWindow = new Map<number, string>()
const boundsByWindow = new Map<number, Electron.Rectangle | null>()
const agentTabByThread = new Map<string, string>()
let counter = 0
let downloadAsk: ((info: { url: string; filename: string }) => Promise<string | null>) | null = null

export function setDownloadHandler(h: typeof downloadAsk): void {
  downloadAsk = h
}

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
  if (!url || url.startsWith('about:') || url.startsWith('devtools:')) return
  const h = readHistory()
  h.push({ url, title, at: Date.now() })
  fs.writeFileSync(historyFile(), JSON.stringify(h.slice(-5000)))
}

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

function tabInfo(t: Tab): { id: string; url: string; title: string; active: boolean; threadId: string | null; loading: boolean; canGoBack: boolean; canGoForward: boolean } {
  const wc = t.view.webContents
  const active = t.windowId != null && activeByWindow.get(t.windowId) === t.id
  return {
    id: t.id,
    url: wc.getURL(),
    title: wc.getTitle() || wc.getURL(),
    active,
    threadId: t.threadId,
    loading: wc.isLoading(),
    canGoBack: wc.navigationHistory.canGoBack(),
    canGoForward: wc.navigationHistory.canGoForward(),
  }
}

export function state(windowId?: number): unknown {
  return {
    tabs: [...tabs.values()].map(tabInfo),
    activeId: windowId != null ? (activeByWindow.get(windowId) ?? null) : null,
  }
}

function broadcastState(): void {
  for (const w of BrowserWindow.getAllWindows()) {
    if (!w.isDestroyed()) w.webContents.send('odex:browser-state', state(w.id))
  }
}

export function createTab(url: string, threadId: string | null = null, win?: BrowserWindow | null): string {
  setupSession()
  const id = `b${++counter}`
  const view = new WebContentsView({ webPreferences: { partition: PARTITION, sandbox: true, contextIsolation: true } })
  const t: Tab = { id, view, threadId, events: [], attached: false, windowId: null }
  tabs.set(id, t)
  const wc = view.webContents
  wc.on('did-navigate', () => {
    pushHistory(wc.getURL(), wc.getTitle())
    broadcastState()
  })
  wc.on('page-title-updated', broadcastState)
  wc.on('did-start-loading', broadcastState)
  wc.on('did-stop-loading', broadcastState)
  wc.setWindowOpenHandler(({ url: u }) => {
    createTab(u, t.threadId, t.windowId != null ? BrowserWindow.fromId(t.windowId) : null)
    return { action: 'deny' }
  })
  void wc.loadURL(url || 'about:blank').catch(() => {})
  if (win) showTab(win, id)
  broadcastState()
  return id
}

export function showTab(win: BrowserWindow, id: string | null): void {
  const prev = activeByWindow.get(win.id)
  if (prev && prev !== id) {
    const pt = tabs.get(prev)
    if (pt) {
      try {
        win.contentView.removeChildView(pt.view)
      } catch {}
      pt.windowId = null
    }
  }
  if (!id) {
    activeByWindow.delete(win.id)
    broadcastState()
    return
  }
  const t = tabs.get(id)
  if (!t) return
  // detach from another window if needed
  if (t.windowId != null && t.windowId !== win.id) {
    const other = BrowserWindow.fromId(t.windowId)
    try {
      other?.contentView.removeChildView(t.view)
    } catch {}
    activeByWindow.delete(t.windowId)
  }
  t.windowId = win.id
  activeByWindow.set(win.id, id)
  const b = boundsByWindow.get(win.id)
  if (b && b.width > 0 && b.height > 0) {
    win.contentView.addChildView(t.view)
    t.view.setBounds(b)
  }
  broadcastState()
}

/** Renderer reports where the browser panel is (null = hidden). */
export function setBounds(win: BrowserWindow, b: Electron.Rectangle | null): void {
  boundsByWindow.set(win.id, b)
  const id = activeByWindow.get(win.id)
  const t = id ? tabs.get(id) : undefined
  if (!t) return
  if (!b || b.width <= 0 || b.height <= 0) {
    try {
      win.contentView.removeChildView(t.view)
    } catch {}
    return
  }
  win.contentView.addChildView(t.view)
  t.view.setBounds({ x: Math.round(b.x), y: Math.round(b.y), width: Math.round(b.width), height: Math.round(b.height) })
}

export function closeTab(id: string): void {
  const t = tabs.get(id)
  if (!t) return
  if (t.windowId != null) {
    const w = BrowserWindow.fromId(t.windowId)
    try {
      w?.contentView.removeChildView(t.view)
    } catch {}
    if (activeByWindow.get(t.windowId) === id) activeByWindow.delete(t.windowId)
  }
  for (const [th, tid] of agentTabByThread) if (tid === id) agentTabByThread.delete(th)
  try {
    t.view.webContents.close()
  } catch {}
  tabs.delete(id)
  broadcastState()
}

export function navigate(id: string, url: string): void {
  const t = tabs.get(id)
  if (!t) return
  let u = url.trim()
  if (!/^[a-z]+:/i.test(u)) {
    u = /^(localhost|127\.0\.0\.1|\[::1\])(:\d+)?/.test(u) || /\.[a-z]{2,}(\/|$)/i.test(u) ? `http://${u}` : `https://duckduckgo.com/?q=${encodeURIComponent(u)}`
  }
  void t.view.webContents.loadURL(u).catch(() => {})
}

export function command(id: string, cmd: 'back' | 'forward' | 'reload' | 'hardReload' | 'stop' | 'devtools'): void {
  const t = tabs.get(id)
  if (!t) return
  const wc = t.view.webContents
  switch (cmd) {
    case 'back':
      wc.navigationHistory.goBack()
      break
    case 'forward':
      wc.navigationHistory.goForward()
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
      wc.openDevTools({ mode: 'detach' })
      break
  }
}

// ---------------------------------------------------------------- CDP

async function ensureDebugger(t: Tab): Promise<void> {
  const dbg = t.view.webContents.debugger
  if (t.attached && dbg.isAttached()) return
  if (!dbg.isAttached()) dbg.attach('1.3')
  t.attached = true
  dbg.on('message', (_e, method, params) => {
    t.events.push({ method, params })
    if (t.events.length > 800) t.events.splice(0, t.events.length - 800)
  })
  dbg.on('detach', () => {
    t.attached = false
  })
  for (const d of ['Page.enable', 'Runtime.enable', 'Network.enable', 'DOM.enable']) {
    try {
      await dbg.sendCommand(d)
    } catch {}
  }
}

function agentTab(threadId: string): Tab {
  const existing = agentTabByThread.get(threadId)
  if (existing && tabs.has(existing)) return tabs.get(existing)!
  const win = BrowserWindow.getFocusedWindow() || BrowserWindow.getAllWindows()[0] || null
  const id = createTab('about:blank', threadId, win)
  agentTabByThread.set(threadId, id)
  return tabs.get(id)!
}

/** Handle the engine's `browser/execute` server request (CDP transport). */
export async function execute(params: { threadId: string; action: string; args: any }): Promise<unknown> {
  const ok = (value: unknown) => ({ ok: true, text: JSON.stringify(value ?? {}) })
  try {
    switch (params.action) {
      case 'cdp': {
        const t = agentTab(params.threadId)
        await ensureDebugger(t)
        const r = await t.view.webContents.debugger.sendCommand(params.args.method, params.args.params || {})
        return ok(r)
      }
      case 'events': {
        const t = agentTab(params.threadId)
        const ev = t.events.splice(0, t.events.length)
        return ok(ev)
      }
      case 'tabs': {
        const mine = [...tabs.values()].filter((t) => t.threadId === params.threadId)
        const current = agentTabByThread.get(params.threadId)
        return ok(mine.map((t) => ({ id: t.id, url: t.view.webContents.getURL(), title: t.view.webContents.getTitle(), active: t.id === current })))
      }
      case 'new_tab': {
        const win = BrowserWindow.getFocusedWindow() || BrowserWindow.getAllWindows()[0] || null
        const id = createTab(params.args.url || 'about:blank', params.threadId, win)
        agentTabByThread.set(params.threadId, id)
        const t = tabs.get(id)!
        await ensureDebugger(t)
        return ok({ id, url: params.args.url || 'about:blank', title: '', active: true })
      }
      case 'select_tab': {
        if (!tabs.has(params.args.id)) throw new Error('no such tab')
        agentTabByThread.set(params.threadId, params.args.id)
        return ok({})
      }
      case 'close_tab': {
        closeTab(params.args.id)
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
  const box = document.createElement('div');
  box.style.cssText = 'position:fixed;pointer-events:none;z-index:2147483647;border:2px solid #4f6bed;background:rgba(79,107,237,.12);border-radius:3px;transition:all .05s';
  document.documentElement.appendChild(box);
  let start = null;
  const sel = (el) => {
    if (!el || el === document.body) return 'body';
    if (el.id) return '#' + CSS.escape(el.id);
    const parts = [];
    while (el && el.nodeType === 1 && parts.length < 5 && el !== document.body) {
      let p = el.tagName.toLowerCase();
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
  const down = (e) => { e.preventDefault(); e.stopPropagation(); start = { x: e.clientX, y: e.clientY }; };
  const up = (e) => {
    e.preventDefault(); e.stopPropagation();
    const dragged = start && (Math.abs(e.clientX - start.x) > 6 || Math.abs(e.clientY - start.y) > 6);
    let result;
    if (dragged) {
      const x = Math.min(start.x, e.clientX), y = Math.min(start.y, e.clientY);
      result = { selector: null, bounds: { x, y, width: Math.abs(e.clientX - start.x), height: Math.abs(e.clientY - start.y) }, text: '' };
    } else {
      const el = document.elementFromPoint(e.clientX, e.clientY);
      const r = el.getBoundingClientRect();
      result = { selector: sel(el), bounds: { x: r.left, y: r.top, width: r.width, height: r.height }, text: (el.innerText || '').slice(0, 300) };
    }
    cleanup(); resolve(result);
  };
  const key = (e) => { if (e.key === 'Escape') { cleanup(); resolve(null); } };
  const cleanup = () => { box.remove(); removeEventListener('mousemove', move, true); removeEventListener('mousedown', down, true); removeEventListener('mouseup', up, true); removeEventListener('keydown', key, true); };
  addEventListener('mousemove', move, true); addEventListener('mousedown', down, true); addEventListener('mouseup', up, true); addEventListener('keydown', key, true);
}))()`

/** Let the user pick an element or drag a region; returns selector, bounds and a screenshot. */
export async function pickForComment(id: string): Promise<unknown> {
  const t = tabs.get(id)
  if (!t) return null
  const wc = t.view.webContents
  const picked = (await wc.executeJavaScript(PICKER, true)) as { selector: string | null; bounds: Electron.Rectangle; text: string } | null
  if (!picked) return null
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
  return { url: wc.getURL(), selector: picked.selector, bounds: picked.bounds, text: picked.text, screenshotUrl: shot }
}

export function tabForThread(threadId: string): string | null {
  return agentTabByThread.get(threadId) ?? null
}
