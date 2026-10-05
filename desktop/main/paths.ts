import { app } from 'electron'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

export const mainDir = path.dirname(fileURLToPath(import.meta.url))

/** ~/.odex (or $ODEX_HOME). */
export function odexHome(): string {
  const env = process.env.ODEX_HOME
  if (env && env.trim()) return env
  return path.join(os.homedir(), '.odex')
}

export function ensureDir(p: string): string {
  fs.mkdirSync(p, { recursive: true })
  return p
}

const exe = process.platform === 'win32' ? '.exe' : ''

/** Locate the `odex-engine` binary: env override, packaged resources, then the dev build. */
export function engineBinary(): string {
  const env = process.env.ODEX_ENGINE_PATH
  if (env && fs.existsSync(env)) return env
  if (app.isPackaged) {
    return path.join(process.resourcesPath, 'bin', `odex-engine${exe}`)
  }
  const repo = path.resolve(mainDir, '..', '..', '..')
  const candidates = ['debug', 'release'].map((p) => path.join(repo, 'engine', 'target', p, `odex-engine${exe}`))
  const existing = candidates.filter((c) => fs.existsSync(c))
  if (existing.length === 0) return candidates[0]
  // newest build wins
  return existing.sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs)[0]
}

export function resourcePath(name: string): string {
  if (app.isPackaged) return path.join(process.resourcesPath, name)
  return path.resolve(mainDir, '..', '..', 'resources', name)
}

export function preloadPath(): string {
  return path.resolve(mainDir, '..', 'preload', 'index.cjs')
}

export function rendererUrl(query: Record<string, string> = {}): { url?: string; file?: string; query: Record<string, string> } {
  const dev = process.env.ELECTRON_RENDERER_URL
  if (dev) {
    const q = new URLSearchParams(query).toString()
    return { url: q ? `${dev}?${q}` : dev, query }
  }
  return { file: path.resolve(mainDir, '..', 'renderer', 'index.html'), query }
}
