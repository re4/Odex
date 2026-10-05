// File-system helpers for the Files side panel: directory listings, stat and
// lightweight change watchers (one non-recursive fs.watch per watched path,
// ref-counted per renderer).
import fs from 'node:fs'
import path from 'node:path'
import type { WebContents } from 'electron'

export interface DirEntry {
  name: string
  path: string
  isDir: boolean
  isSymlink: boolean
  size: number
  mtime: number
}

export interface PathStat {
  exists: boolean
  isDir: boolean
  size: number
  mtime: number
  /** Canonical path (symlinks and Windows short names resolved). */
  real: string
}

/** Every entry of `dir` (nothing is filtered: the renderer decides what to hide). */
export async function listDir(dir: string): Promise<DirEntry[]> {
  const ents = await fs.promises.readdir(dir, { withFileTypes: true })
  return Promise.all(
    ents.map(async (d) => {
      const full = path.join(dir, d.name)
      const out: DirEntry = { name: d.name, path: full, isDir: d.isDirectory(), isSymlink: d.isSymbolicLink(), size: 0, mtime: 0 }
      try {
        const st = await fs.promises.stat(full)
        out.isDir = st.isDirectory()
        out.size = st.size
        out.mtime = st.mtimeMs
      } catch {
        /* broken symlink or locked system file: keep the dirent info */
      }
      return out
    }),
  )
}

export async function statPath(p: string): Promise<PathStat> {
  try {
    const st = await fs.promises.stat(p)
    const real = await new Promise<string>((resolve) => fs.realpath.native(p, (err, r) => resolve(err ? p : r)))
    return { exists: true, isDir: st.isDirectory(), size: st.size, mtime: st.mtimeMs, real }
  } catch {
    return { exists: false, isDir: false, size: 0, mtime: 0, real: p }
  }
}

export type WatchKind = 'recursive' | 'flat' | 'file'

interface Watch {
  kind: WatchKind | null
  refs: number
  close: () => void
  timer?: NodeJS.Timeout
  names: Set<string>
  all: boolean
  ignore: Set<string>
}

const watchers = new Map<number, Map<string, Watch>>()
const MAX_NAMES = 500

function closeWatch(ent: Watch): void {
  if (ent.timer) clearTimeout(ent.timer)
  ent.timer = undefined
  try {
    ent.close()
  } catch {}
  ent.close = () => {}
  ent.kind = null
}

/** Batch events: flush 150 ms after the first one (no reset, so steady churn still flushes). */
function schedule(wc: WebContents, key: string, ent: Watch): void {
  if (ent.timer) return
  ent.timer = setTimeout(() => {
    ent.timer = undefined
    if (wc.isDestroyed()) return
    wc.send('odex:fs-changed', { path: key, names: ent.all ? [] : [...ent.names], all: ent.all })
    ent.names.clear()
    ent.all = false
  }, 150)
}

function arm(wc: WebContents, key: string, ent: Watch): void {
  let st: fs.Stats
  try {
    st = fs.statSync(key)
  } catch {
    return
  }
  if (!st.isDirectory()) {
    // Files are polled (stat only): no handle is held, so the agent can still
    // rename or delete folders on Windows, and atomic replaces are seen.
    const listener = (cur: fs.Stats, prev: fs.Stats) => {
      if (cur.mtimeMs === prev.mtimeMs && cur.size === prev.size && cur.ino === prev.ino) return
      schedule(wc, key, ent)
    }
    fs.watchFile(key, { interval: 1000, persistent: false }, listener)
    ent.kind = 'file'
    ent.close = () => fs.unwatchFile(key, listener)
    return
  }
  // One recursive watcher per root where the OS supports it natively (a single
  // handle on the root keeps sub-folders renameable on Windows); flat elsewhere.
  const recursive = process.platform === 'win32' || process.platform === 'darwin'
  try {
    const w = fs.watch(key, { persistent: false, recursive }, (_ev, name) => {
      const n = name == null ? '' : String(name)
      if (n && n.split(/[\\/]/).some((seg) => ent.ignore.has(seg))) return
      if (!n || ent.names.size >= MAX_NAMES) ent.all = true
      else ent.names.add(n)
      schedule(wc, key, ent)
    })
    w.on('error', () => {
      closeWatch(ent)
      if (!wc.isDestroyed()) wc.send('odex:fs-changed', { path: key, names: [], all: true })
    })
    ent.kind = recursive ? 'recursive' : 'flat'
    ent.close = () => w.close()
  } catch {
    ent.kind = null
  }
}

/**
 * Watch a file or directory for `wc`; changes arrive as `odex:fs-changed`
 * {path, names (relative to path), all}. Path segments in `ignore` (e.g.
 * node_modules, target) are skipped. Returns how the path is watched.
 */
export function watchPath(wc: WebContents, p: string, ignore?: string[]): WatchKind | null {
  const key = path.resolve(p)
  let m = watchers.get(wc.id)
  if (!m) {
    m = new Map()
    watchers.set(wc.id, m)
    const id = wc.id
    wc.once('destroyed', () => {
      for (const ent of watchers.get(id)?.values() ?? []) closeWatch(ent)
      watchers.delete(id)
    })
  }
  const cur = m.get(key)
  if (cur) {
    cur.refs++
    if (!cur.kind) arm(wc, key, cur)
    return cur.kind
  }
  const ent: Watch = { kind: null, refs: 1, close: () => {}, names: new Set(), all: false, ignore: new Set(['.git', ...(ignore ?? [])]) }
  arm(wc, key, ent)
  m.set(key, ent)
  return ent.kind
}

export function unwatchPath(wc: WebContents, p: string): void {
  const key = path.resolve(p)
  const m = watchers.get(wc.id)
  const ent = m?.get(key)
  if (!m || !ent) return
  if (--ent.refs > 0) return
  closeWatch(ent)
  m.delete(key)
}
