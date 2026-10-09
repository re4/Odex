import { contextBridge, ipcRenderer, IpcRendererEvent, webUtils } from 'electron'
import type { UpdateState } from '@shared/desktop'

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
    create: (opts: { threadId?: string | null; cwd?: string; shell?: string; cols?: number; rows?: number; title?: string; env?: Record<string, string> }) => ipcRenderer.invoke('term:create', opts),
    run: (opts: { threadId?: string | null; cwd: string; command: string; title?: string; env?: Record<string, string>; detectUrls?: boolean }) => ipcRenderer.invoke('term:run', opts),
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
    pickCancel: (id: string) => ipcRenderer.invoke('browser:pickCancel', id),
    /** App shortcuts pressed while a page had focus (e.g. "Mod+K"). */
    onKey: (cb: Listener<{ tabId: string; keys: string }>) => on('odex:browser-key', cb),
    history: () => ipcRenderer.invoke('browser:history'),
    clearHistory: (since?: number) => ipcRenderer.invoke('browser:clearHistory', since),
    clearData: () => ipcRenderer.invoke('browser:clearData'),
    onState: (cb: Listener<any>) => on('odex:browser-state', cb),
  },
  dialog: {
    openFolder: (opts?: { multi?: boolean }) => ipcRenderer.invoke('dialog:openFolder', opts) as Promise<string[]>,
    openFiles: () => ipcRenderer.invoke('dialog:openFiles') as Promise<string[]>,
    /** Save a data URL (or text) to a file the user picks; resolves to the path, or null if cancelled. */
    saveFile: (opts: { defaultName: string; dataUrl?: string; text?: string }) => ipcRenderer.invoke('dialog:saveFile', opts) as Promise<string | null>,
    /** "Save as…": copy a file on disk where the user picks; resolves to the new path, or null if cancelled. */
    saveCopy: (src: string, opts?: { title?: string }) => ipcRenderer.invoke('dialog:saveCopy', src, opts) as Promise<string | null>,
  },
  /** Live HTML previews: a sandboxable URL that serves the file and its folder's assets. */
  preview: {
    url: (file: string) => ipcRenderer.invoke('preview:url', file) as Promise<string>,
  },
  shell: {
    openExternal: (url: string) => ipcRenderer.invoke('shell:openExternal', url),
    openPath: (p: string) => ipcRenderer.invoke('shell:openPath', p),
    showItem: (p: string) => ipcRenderer.invoke('shell:showItem', p),
    /** Move a file to the Recycle Bin / Trash. */
    trashItem: (p: string) => ipcRenderer.invoke('shell:trashItem', p) as Promise<void>,
    openInEditor: (p: string, line?: number) => ipcRenderer.invoke('shell:openInEditor', p, line),
  },
  fs: {
    /** Absolute path of a dropped/pasted `File` ('' for in-memory files). Replaces the removed `File.path`. */
    pathForFile: (file: File): string => {
      try {
        return webUtils.getPathForFile(file)
      } catch {
        return ''
      }
    },
    read: (p: string, maxBytes?: number) => ipcRenderer.invoke('fs:read', p, maxBytes),
    write: (p: string, text: string) => ipcRenderer.invoke('fs:write', p, text) as Promise<number>,
    exists: (p: string) => ipcRenderer.invoke('fs:exists', p) as Promise<boolean>,
    /** Directory entries (unfiltered) with size/mtime. */
    list: (dir: string) => ipcRenderer.invoke('fs:list', dir) as Promise<Array<{ name: string; path: string; isDir: boolean; isSymlink: boolean; size: number; mtime: number }>>,
    stat: (p: string) => ipcRenderer.invoke('fs:stat', p) as Promise<{ exists: boolean; isDir: boolean; size: number; mtime: number; real: string }>,
    /** Watch a directory (recursive where native) or file (polled); ref-counted; events arrive via `onChange`. */
    watch: (p: string, ignore?: string[]) => ipcRenderer.invoke('fs:watch', p, ignore) as Promise<'recursive' | 'flat' | 'file' | null>,
    unwatch: (p: string) => ipcRenderer.invoke('fs:unwatch', p) as Promise<void>,
    onChange: (cb: Listener<{ path: string; names: string[]; all?: boolean }>) => on('odex:fs-changed', cb),
  },
  win: {
    /** Pop-out window for a thread; `panel: 'review'` shows only its review panel (detached review). */
    newWindow: (threadId?: string, panel?: 'review') => ipcRenderer.invoke('win:new', threadId, panel),
    /** Open (or focus) the Quick Chat window. */
    quickChat: () => ipcRenderer.invoke('win:quickChat'),
    /** Show a thread in the main window. */
    openInMain: (threadId: string) => ipcRenderer.invoke('win:openInMain', threadId),
    alwaysOnTop: (on: boolean) => ipcRenderer.invoke('win:alwaysOnTop', on),
    close: () => ipcRenderer.invoke('win:close'),
    zoom: (factor: number) => ipcRenderer.invoke('win:zoom', factor),
    fullscreen: () => ipcRenderer.invoke('win:fullscreen'),
  },
  app: {
    info: () => ipcRenderer.invoke('app:info'),
    quit: () => ipcRenderer.invoke('app:quit'),
    killSwitch: (on?: boolean) => ipcRenderer.invoke('app:killSwitch', on),
    killSwitchState: () => ipcRenderer.invoke('app:killSwitchState') as Promise<boolean>,
    theme: () => ipcRenderer.invoke('app:theme') as Promise<'dark' | 'light'>,
  },
  /** App updates from the GitHub releases page (main/updater.ts). */
  updates: {
    state: () => ipcRenderer.invoke('update:state') as Promise<UpdateState>,
    /** Check now; resolves once the check is done (a download may continue in the background). */
    check: () => ipcRenderer.invoke('update:check') as Promise<UpdateState>,
    download: () => ipcRenderer.invoke('update:download') as Promise<void>,
    /** Quit, install the downloaded update and relaunch. */
    install: () => ipcRenderer.invoke('update:install') as Promise<void>,
    onState: (cb: Listener<UpdateState>) => on('odex:update-state', cb),
  },
  platform: process.platform,
}

export type OdexApi = typeof api

contextBridge.exposeInMainWorld('odex', api)
