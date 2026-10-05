import { BrowserWindow } from 'electron'
import type { IPty } from '@lydell/node-pty'
import { createRequire } from 'node:module'
import os from 'node:os'
import { getSettings } from './settings'

const require = createRequire(import.meta.url)

export interface TerminalInfo {
  id: string
  threadId: string | null
  title: string
  cwd: string
  shell: string
  running: boolean
  exitCode: number | null
}

interface Term {
  info: TerminalInfo
  pty: IPty
  buffer: string
  lastActive: number
  /** Dev-server URL detection (project action terminals). */
  urls?: { partial: string; seen: Set<string> }
}

const DEV_URL = /https?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]):(\d{2,5})(?:\/[^\s'"<>()[\]`]*)?/g

/** Local dev-server URLs (with a port) in terminal output; `0.0.0.0` becomes `localhost`. */
export function devServerUrls(text: string): Array<{ port: string; url: string }> {
  const out: Array<{ port: string; url: string }> = []
  for (const m of text.replace(ANSI, '').matchAll(DEV_URL)) {
    out.push({ port: m[1], url: m[0].replace(/[.,;:]+$/, '').replace('0.0.0.0', 'localhost').replace('[::]', 'localhost') })
  }
  return out
}

/** Feed output to a terminal's URL detector; new ports open in the in-app browser (setting permitting). */
function scanUrls(term: Term, data: string): void {
  const u = term.urls
  if (!u) return
  u.partial += data
  const nl = u.partial.lastIndexOf('\n')
  let complete = ''
  if (nl >= 0) {
    complete = u.partial.slice(0, nl + 1)
    u.partial = u.partial.slice(nl + 1)
  } else if (u.partial.length > 4096) {
    complete = u.partial
    u.partial = ''
  }
  if (!complete) return
  for (const { port, url } of devServerUrls(complete)) {
    if (u.seen.has(port)) continue
    u.seen.add(port)
    // the renderer opens `openUrl` notifications with target inApp in the browser panel
    if (getSettings().openDevServerUrls) broadcast('odex:notification', { method: 'openUrl', params: { url, target: 'inApp', source: 'devServer' } })
  }
}

const MAX_BUFFER = 256 * 1024
const terms = new Map<string, Term>()
let counter = 0

function broadcast(channel: string, payload: unknown): void {
  for (const w of BrowserWindow.getAllWindows()) {
    if (!w.isDestroyed()) w.webContents.send(channel, payload)
  }
}

export function shellCommand(shell: string): { file: string; args: string[] } {
  const s = (shell || '').toLowerCase()
  if (process.platform === 'win32') {
    if (s === 'pwsh') return { file: 'pwsh.exe', args: ['-NoLogo'] }
    if (s === 'cmd') return { file: 'cmd.exe', args: [] }
    if (s === 'wsl') return { file: 'wsl.exe', args: [] }
    if (s === 'gitbash' || s === 'bash') return { file: 'C:\\Program Files\\Git\\bin\\bash.exe', args: ['--login', '-i'] }
    return { file: 'powershell.exe', args: ['-NoLogo'] }
  }
  return { file: shell || process.env.SHELL || '/bin/bash', args: ['-l'] }
}

export function createTerminal(opts: {
  threadId?: string | null
  cwd?: string
  shell?: string
  cols?: number
  rows?: number
  title?: string
  /** Extra variables (the thread's environment). */
  env?: Record<string, string>
  /** Open local dev-server URLs printed by this terminal. */
  detectUrls?: boolean
}): TerminalInfo {
  const pty = require('@lydell/node-pty') as typeof import('@lydell/node-pty')
  const id = `t${++counter}`
  const cwd = opts.cwd || os.homedir()
  const { file, args } = shellCommand(opts.shell || '')
  const p = pty.spawn(file, args, {
    name: 'xterm-256color',
    cols: opts.cols || 120,
    rows: opts.rows || 30,
    cwd,
    env: { ...process.env, ...(opts.env ?? {}), ODEX_TERMINAL: '1' } as Record<string, string>,
  })
  const info: TerminalInfo = {
    id,
    threadId: opts.threadId ?? null,
    title: opts.title || file.split(/[\\/]/).pop() || 'terminal',
    cwd,
    shell: file,
    running: true,
    exitCode: null,
  }
  const term: Term = { info, pty: p, buffer: '', lastActive: Date.now(), urls: opts.detectUrls ? { partial: '', seen: new Set() } : undefined }
  terms.set(id, term)
  p.onData((data) => {
    term.buffer += data
    if (term.buffer.length > MAX_BUFFER) term.buffer = term.buffer.slice(term.buffer.length - MAX_BUFFER)
    broadcast('odex:terminal-data', { id, data })
    scanUrls(term, data)
  })
  p.onExit(({ exitCode }) => {
    info.running = false
    info.exitCode = exitCode
    broadcast('odex:terminal-exit', { id, exitCode })
  })
  return info
}

/** Run a command line in a new terminal (project actions). */
export function runInTerminal(opts: { threadId?: string | null; cwd: string; command: string; title?: string; shell?: string; env?: Record<string, string>; detectUrls?: boolean }): TerminalInfo {
  const info = createTerminal({ threadId: opts.threadId, cwd: opts.cwd, title: opts.title, shell: opts.shell, env: opts.env, detectUrls: opts.detectUrls ?? true })
  const t = terms.get(info.id)!
  setTimeout(() => t.pty.write(opts.command + '\r'), 300)
  return info
}

export function writeTerminal(id: string, data: string): void {
  const t = terms.get(id)
  if (!t) return
  t.lastActive = Date.now()
  t.pty.write(data)
}

export function resizeTerminal(id: string, cols: number, rows: number): void {
  const t = terms.get(id)
  if (t && t.info.running && cols > 0 && rows > 0) {
    try {
      t.pty.resize(cols, rows)
    } catch {}
  }
}

export function killTerminal(id: string): void {
  const t = terms.get(id)
  if (!t) return
  try {
    t.pty.kill()
  } catch {}
  terms.delete(id)
}

export function listTerminals(threadId?: string | null): TerminalInfo[] {
  return [...terms.values()].filter((t) => threadId === undefined || t.info.threadId === threadId).map((t) => t.info)
}

export function terminalBuffer(id: string): string {
  return terms.get(id)?.buffer ?? ''
}

export function killAllTerminals(): void {
  for (const id of [...terms.keys()]) killTerminal(id)
}

// eslint-disable-next-line no-control-regex -- stripping terminal escape sequences
const ANSI = /\x1b\[[0-9;?<=>]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-Z\\-_]|\r/g

/** `terminal/read` for the agent's read_terminal tool. */
export function readTerminalForAgent(params: { threadId: string; terminalId?: string | null; lines?: number | null }): unknown {
  const mine = [...terms.values()].filter((t) => t.info.threadId === params.threadId)
  const pool = mine.length ? mine : [...terms.values()]
  const pick = params.terminalId ? pool.find((t) => t.info.id === params.terminalId) : pool.sort((a, b) => b.lastActive - a.lastActive)[0]
  const n = params.lines || 200
  const text = pick ? pick.buffer.replace(ANSI, '').split('\n').slice(-n).join('\n') : ''
  return {
    terminals: pool.map((t) => ({ id: t.info.id, title: t.info.title, cwd: t.info.cwd, running: t.info.running })),
    terminalId: pick?.info.id ?? null,
    text: pick ? text : 'No integrated terminal is open for this thread.',
  }
}
