import { ChildProcess, spawn } from 'node:child_process'
import { EventEmitter } from 'node:events'
import fs from 'node:fs'
import path from 'node:path'
import readline from 'node:readline'
import { app } from 'electron'
import { engineBinary, ensureDir, odexHome } from './paths'
import { allSecrets } from './secrets'

export type EngineState = 'starting' | 'ready' | 'restarting' | 'failed' | 'stopped'

interface Pending {
  resolve: (v: unknown) => void
  reject: (e: Error) => void
  method: string
}

export interface ServerRequestHandler {
  (method: string, params: unknown): Promise<unknown>
}

/**
 * Spawns `odex-engine app-server`, speaks JSON-RPC over its stdio, and
 * restarts it when it crashes (renderers then re-read their threads).
 */
export class EngineHost extends EventEmitter {
  private proc: ChildProcess | null = null
  private nextId = 1
  private pending = new Map<number, Pending>()
  private restarts = 0
  private stopping = false
  state: EngineState = 'stopped'
  init: unknown = null
  lastError: string | null = null
  serverRequestHandler: ServerRequestHandler | null = null

  async start(): Promise<void> {
    this.stopping = false
    this.setState(this.restarts > 0 ? 'restarting' : 'starting')
    const bin = engineBinary()
    if (!fs.existsSync(bin)) {
      this.lastError = `Engine binary not found at ${bin}. Build it with \`cargo build -p odex-engine\`.`
      this.setState('failed')
      return
    }
    const logDir = ensureDir(path.join(odexHome(), 'logs'))
    const log = fs.createWriteStream(path.join(logDir, 'engine.log'), { flags: 'a' })
    log.write(`\n--- ${new Date().toISOString()} starting ${bin}\n`)
    const proc = spawn(bin, ['app-server'], {
      env: { ...process.env, ODEX_HOME: odexHome(), ODEX_LOG: process.env.ODEX_LOG || 'warn,odex=info' },
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true,
    })
    this.proc = proc
    proc.stderr?.pipe(log)
    const rl = readline.createInterface({ input: proc.stdout!, crlfDelay: Infinity })
    rl.on('line', (line) => this.onLine(line))
    proc.on('exit', (code, signal) => {
      log.write(`--- engine exited code=${code} signal=${signal}\n`)
      this.proc = null
      for (const [, p] of this.pending) p.reject(new Error('engine stopped'))
      this.pending.clear()
      if (this.stopping) {
        this.setState('stopped')
        return
      }
      this.lastError = `Engine exited (code ${code ?? signal}).`
      this.restarts += 1
      if (this.restarts > 20) {
        this.setState('failed')
        return
      }
      const delay = Math.min(500 * 2 ** Math.min(this.restarts, 5), 10_000)
      this.setState('restarting')
      setTimeout(() => void this.start(), delay)
    })
    proc.on('error', (e) => {
      this.lastError = e.message
      this.setState('failed')
    })
    try {
      this.init = await this.request('initialize', {
        clientInfo: { name: 'odex-desktop', version: app.getVersion() },
        capabilities: { approvals: true, browser: true, elicitation: true, secrets: true },
        secrets: allSecrets(),
      })
      this.setState('ready')
      this.emit('initialized', this.init)
      // reset the backoff after a healthy minute
      setTimeout(() => {
        if (this.state === 'ready') this.restarts = 0
      }, 60_000)
    } catch (e) {
      this.lastError = (e as Error).message
    }
  }

  private setState(s: EngineState): void {
    this.state = s
    this.emit('state', { state: s, error: this.lastError, init: this.init })
  }

  private write(obj: unknown): void {
    if (!this.proc?.stdin?.writable) throw new Error('engine is not running')
    this.proc.stdin.write(JSON.stringify(obj) + '\n')
  }

  request<T = unknown>(method: string, params?: unknown): Promise<T> {
    const id = this.nextId++
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject, method })
      try {
        this.write({ jsonrpc: '2.0', id, method, params: params ?? {} })
      } catch (e) {
        this.pending.delete(id)
        reject(e as Error)
      }
    })
  }

  private onLine(line: string): void {
    if (!line.trim()) return
    let msg: any
    try {
      msg = JSON.parse(line)
    } catch {
      return
    }
    const hasId = msg.id !== undefined && msg.id !== null
    if (msg.method && hasId) {
      void this.handleServerRequest(msg.id, msg.method, msg.params)
    } else if (msg.method) {
      this.emit('notification', msg.method, msg.params)
    } else if (hasId) {
      const p = this.pending.get(msg.id)
      if (!p) return
      this.pending.delete(msg.id)
      if (msg.error) {
        const err = new Error(msg.error.message) as Error & { code?: number }
        err.code = msg.error.code
        p.reject(err)
      } else p.resolve(msg.result)
    }
  }

  private async handleServerRequest(id: number | string, method: string, params: unknown): Promise<void> {
    try {
      if (!this.serverRequestHandler) throw new Error('no handler')
      const result = await this.serverRequestHandler(method, params)
      this.write({ jsonrpc: '2.0', id, result: result ?? {} })
    } catch (e) {
      try {
        this.write({ jsonrpc: '2.0', id, error: { code: -32603, message: (e as Error).message } })
      } catch {}
    }
  }

  async stop(): Promise<void> {
    this.stopping = true
    const p = this.proc
    if (!p) return
    p.stdin?.end()
    await new Promise<void>((resolve) => {
      const t = setTimeout(() => {
        p.kill()
        resolve()
      }, 2500)
      p.once('exit', () => {
        clearTimeout(t)
        resolve()
      })
    })
  }
}
