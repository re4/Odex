import { contextBridge, ipcRenderer, IpcRendererEvent } from 'electron'

type Listener<T> = (payload: T) => void

function on<T>(channel: string, cb: Listener<T>): () => void {
  const h = (_e: IpcRendererEvent, payload: T) => cb(payload)
  ipcRenderer.on(channel, h)
  return () => ipcRenderer.removeListener(channel, h)
}

const api = {
  /** Call an engine JSON-RPC method. */
  async request(method: string, params?: unknown): Promise<unknown> {
    const r = (await ipcRenderer.invoke('odex:request', method, params ?? {})) as { ok: boolean; result?: unknown; error?: string; code?: number }
    if (!r.ok) {
      const e = new Error(r.error || 'request failed') as Error & { code?: number }
      e.code = r.code
      throw e
    }
    return r.result
  },
  onNotification: (cb: Listener<{ method: string; params: any }>) => on('odex:notification', cb),
  onServerRequest: (cb: Listener<{ id: number; method: string; params: any }>) => on('odex:server-request', cb),
  onServerRequestResolved: (cb: Listener<number>) => on('odex:server-request-resolved', cb),
  respond: (id: number, result?: unknown, error?: string) => ipcRenderer.invoke('odex:server-response', id, result, error),
  onEngineState: (cb: Listener<{ state: string; error?: string | null; init?: any }>) => on('odex:engine-state', cb),
  engineInfo: () => ipcRenderer.invoke('odex:engine-info'),
  restartEngine: () => ipcRenderer.invoke('odex:engine-restart'),
  onCommand: (cb: Listener<{ command: string; threadId?: string }>) => on('odex:command', cb),
  onDeepLink: (cb: Listener<string>) => on('odex:deeplink', cb),
  onAppshot: (cb: Listener<any>) => on('odex:appshot', cb),
  onKillSwitch: (cb: Listener<boolean>) => on('odex:kill-switch', cb),
  onNativeTheme: (cb: Listener<'dark' | 'light'>) => on('odex:native-theme', cb),

  settings: {
    get: () => ipcRenderer.invoke('settings:get'),
    set: (patch: Record<string, unknown>) => ipcRenderer.invoke('settings:set', patch),
    onChange: (cb: Listener<any>) => on('odex:settings', cb),
  },
  secrets: {
    set: (key: string, value: string | null) => ipcRenderer.invoke('secrets:set', key, value),
    has: (key: string) => ipcRenderer.invoke('secrets:has', key) as Promise<boolean>,
  },
  terminals: {
    create: (opts: { threadId?: string | null; cwd?: string; shell?: string; cols?: number; rows?: number; title?: string }) => ipcRenderer.invoke('term:create', opts),
    run: (opts: { threadId?: string | null; cwd: string; command: string; title?: string }) => ipcRenderer.invoke('term:run', opts),
    write: (id: string, data: string) => ipcRenderer.invoke('term:write', id, data),
    resize: (id: string, cols: number, rows: number) => ipcRenderer.invoke('term:resize', id, cols, rows),
    kill: (id: string) => ipcRenderer.invoke('term:kill', id),
    list: (threadId?: string | null) => ipcRenderer.invoke('term:list', threadId),
    buffer: (id: string) => ipcRenderer.invoke('term:buffer', id) as Promise<string>,
    onData: (cb: Listener<{ id: string; data: string }>) => on('odex:terminal-data', cb),
    onExit: (cb: Listener<{ id: string; exitCode: number }>) => on('odex:terminal-exit', cb),
  },
  browser: {
    state: () => ipcRenderer.invoke('browser:state'),
    newTab: (url?: string) => ipcRenderer.invoke('browser:new', url ?? '') as Promise<string>,
    show: (id: string | null) => ipcRenderer.invoke('browser:show', id),
    setBounds: (b: { x: number; y: number; width: number; height: number } | null) => ipcRenderer.invoke('browser:bounds', b),
    navigate: (id: string, url: string) => ipcRenderer.invoke('browser:navigate', id, url),
    command: (id: string, cmd: 'back' | 'forward' | 'reload' | 'hardReload' | 'stop' | 'devtools') => ipcRenderer.invoke('browser:command', id, cmd),
    close: (id: string) => ipcRenderer.invoke('browser:close', id),
    pick: (id: string) => ipcRenderer.invoke('browser:pick', id),
    history: () => ipcRenderer.invoke('browser:history'),
    clearHistory: (since?: number) => ipcRenderer.invoke('browser:clearHistory', since),
    clearData: () => ipcRenderer.invoke('browser:clearData'),
    onState: (cb: Listener<any>) => on('odex:browser-state', cb),
  },
  dialog: {
    openFolder: (opts?: { multi?: boolean }) => ipcRenderer.invoke('dialog:openFolder', opts) as Promise<string[]>,
    openFiles: () => ipcRenderer.invoke('dialog:openFiles') as Promise<string[]>,
  },
  shell: {
    openExternal: (url: string) => ipcRenderer.invoke('shell:openExternal', url),
    openPath: (p: string) => ipcRenderer.invoke('shell:openPath', p),
    showItem: (p: string) => ipcRenderer.invoke('shell:showItem', p),
    openInEditor: (p: string, line?: number) => ipcRenderer.invoke('shell:openInEditor', p, line),
  },
  fs: {
    read: (p: string) => ipcRenderer.invoke('fs:read', p),
    write: (p: string, text: string) => ipcRenderer.invoke('fs:write', p, text) as Promise<number>,
    exists: (p: string) => ipcRenderer.invoke('fs:exists', p) as Promise<boolean>,
  },
  win: {
    newWindow: (threadId?: string) => ipcRenderer.invoke('win:new', threadId),
    alwaysOnTop: (on: boolean) => ipcRenderer.invoke('win:alwaysOnTop', on),
    close: () => ipcRenderer.invoke('win:close'),
    zoom: (factor: number) => ipcRenderer.invoke('win:zoom', factor),
    fullscreen: () => ipcRenderer.invoke('win:fullscreen'),
  },
  app: {
    info: () => ipcRenderer.invoke('app:info'),
    quit: () => ipcRenderer.invoke('app:quit'),
    killSwitch: (on?: boolean) => ipcRenderer.invoke('app:killSwitch', on),
    theme: () => ipcRenderer.invoke('app:theme') as Promise<'dark' | 'light'>,
  },
  platform: process.platform,
}

export type OdexApi = typeof api

contextBridge.exposeInMainWorld('odex', api)
