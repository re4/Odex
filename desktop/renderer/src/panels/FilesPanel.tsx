import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { create } from 'zustand'
import { useVirtualizer } from '@tanstack/react-virtual'
import type { EditorState, Text } from '@codemirror/state'
import type { EditorView, ViewUpdate } from '@codemirror/view'
import {
  ArrowLeft,
  ArrowRight,
  AtSign,
  BookOpen,
  ChevronDown,
  ChevronRight,
  ChevronsDownUp,
  Code,
  Copy,
  Download,
  Ellipsis,
  ExternalLink,
  Eye,
  EyeOff,
  File as FileIcon,
  FileBraces,
  FileCode,
  FileImage,
  FileText,
  Folder,
  FolderOpen,
  FolderSearch,
  FolderTree,
  GitBranch,
  History,
  ListTree,
  RefreshCw,
  Save,
  Search,
  TextWrap,
  X,
} from 'lucide-react'
import type { GitFileStatus, Turn } from '@shared/index'
import { isRunning, useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { confirmDialog, copy } from '@/lib/actions'
import { Menu, ResizeHandle, basename, type MenuItem } from '@/components/ui'
import { Markdown } from '@/components/Markdown'
import { CodeEditor, createEditorState, extOf, forgetEditorScroll, languageName, withEditorOptions, type LineAnnotation } from '@/components/CodeEditor'
import { saveFileCopy } from '@/panels/SourcesPanel'
import '@/styles/files.css'
import '@/styles/sidepanel.css'

/*
 * Files side-panel tab: a lazily expanding file tree rooted at the thread's
 * working directory (or the project chosen for new threads) above tabbed
 * CodeMirror editors. Tree/tab/editor state lives in a module store so it
 * survives switching side-panel tabs (the panel unmounts) and threads.
 */

// ------------------------------------------------------------------ paths

const WIN = typeof window !== 'undefined' && window.odex?.platform === 'win32'
const SEP = WIN ? '\\' : '/'

export function normPath(p: string): string {
  let s = p.trim().replace(/^\\\\\?\\/, '')
  if (/^file:\/\//i.test(s)) s = decodeURIComponent(s.replace(/^file:\/\/\/?/i, WIN ? '' : '/'))
  if (WIN) s = s.replace(/\//g, '\\')
  let prefix = ''
  if (WIN) {
    const m = /^([a-zA-Z]):(\\|$)/.exec(s)
    if (m) {
      prefix = `${m[1].toUpperCase()}:\\`
      s = s.slice(m[0].length)
    } else if (s.startsWith('\\\\')) {
      prefix = '\\\\'
      s = s.slice(2)
    } else if (s.startsWith('\\')) {
      prefix = '\\'
      s = s.slice(1)
    }
  } else if (s.startsWith('/')) {
    prefix = '/'
    s = s.slice(1)
  }
  const out: string[] = []
  for (const part of s.split(SEP)) {
    if (!part || part === '.') continue
    if (part === '..') {
      if (out.length && out[out.length - 1] !== '..') out.pop()
      else if (!prefix) out.push('..')
      continue
    }
    out.push(part)
  }
  return prefix + out.join(SEP)
}

/** Map key for a path (case-insensitive on Windows). */
const k = (p: string): string => (WIN ? normPath(p).toLowerCase() : normPath(p))
const isAbs = (p: string): boolean => (WIN ? /^([a-zA-Z]:[\\/]|\\\\|\/\/)/.test(p.trim()) : p.trim().startsWith('/'))
const join = (a: string, b: string): string => normPath(`${a}${SEP}${b}`)
const parentOf = (p: string): string => normPath(`${p}${SEP}..`)

function under(root: string, p: string): boolean {
  const r = k(root)
  const q = k(p)
  return q === r || q.startsWith(r.endsWith(SEP) ? r : r + SEP)
}

/** Root-relative path with forward slashes (or the full path when outside). */
function relPath(root: string | null, p: string): string {
  if (!root || !under(root, p)) return normPath(p)
  return normPath(p).slice(normPath(root).length).replace(/^[\\/]/, '').replace(/\\/g, '/')
}

// ------------------------------------------------------------------ store

interface Entry {
  name: string
  path: string
  isDir: boolean
  isSymlink: boolean
  size: number
  mtime: number
}

interface DirState {
  /** Display path (keys are case-folded on Windows). */
  path: string
  entries?: Entry[]
  loading?: boolean
  error?: string
}

type GitKind = 'modified' | 'added' | 'untracked' | 'deleted' | 'renamed' | 'conflict'

interface GitInfo {
  repo: boolean
  branch?: string | null
  files: Record<string, GitKind>
  dirs: Record<string, GitKind>
}

type Kind = 'idle' | 'loading' | 'text' | 'image' | 'pdf' | 'binary' | 'tooLarge' | 'missing' | 'error'

interface FileTab {
  path: string
  kind: Kind
  size?: number
  /** Disk mtime when the editor content was last loaded or saved. */
  mtime?: number
  dataUrl?: string
  dirty: boolean
  /** Changed on disk while there were unsaved edits. */
  diskChanged: boolean
  deleted: boolean
  eol: '\n' | '\r\n'
  bom: boolean
  notUtf8: boolean
  allowEdit: boolean
  error?: string
  reveal: { line: number; at: number } | null
  /** Markdown / SVG: rendered preview or source. */
  mode: 'source' | 'preview'
}

interface Session {
  tabs: string[]
  active: string | null
  expanded: string[]
}

interface FilesState {
  files: Record<string, FileTab>
  sessions: Record<string, Session>
  dirs: Record<string, DirState>
  git: Record<string, GitInfo>
  showHidden: boolean
  treeCollapsed: boolean
  treeHeight: number
  wrap: boolean
  /** Back/forward history of opened files per root (paths). */
  nav: Record<string, { stack: string[]; index: number }>
  /** Recently opened files per root, most recent first (paths). */
  recent: Record<string, string[]>
}

const PERSIST_KEY = 'odex.files.v1'
const RECENT_MAX = 20
const emptySession: Session = { tabs: [], active: null, expanded: [] }

function newTab(path: string): FileTab {
  const ext = extOf(path)
  return {
    path: normPath(path),
    kind: 'idle',
    dirty: false,
    diskChanged: false,
    deleted: false,
    eol: '\n',
    bom: false,
    notUtf8: false,
    allowEdit: false,
    reveal: null,
    // SVG and HTML open rendered; Markdown opens as source
    mode: ext === 'svg' || isHtmlExt(ext) ? 'preview' : 'source',
  }
}

const isHtmlExt = (ext: string): boolean => ext === 'html' || ext === 'htm' || ext === 'xhtml'

function loadPersisted(): FilesState {
  const base: FilesState = { files: {}, sessions: {}, dirs: {}, git: {}, showHidden: false, treeCollapsed: false, treeHeight: 280, wrap: false, nav: {}, recent: {} }
  try {
    const raw = JSON.parse(localStorage.getItem(PERSIST_KEY) || '{}') as {
      sessions?: Record<string, { tabs: string[]; active: string | null; expanded: string[] }>
      showHidden?: boolean
      treeCollapsed?: boolean
      treeHeight?: number
      wrap?: boolean
      recent?: Record<string, string[]>
    }
    for (const [root, list] of Object.entries(raw.recent ?? {})) if (Array.isArray(list)) base.recent[root] = list.filter((p) => typeof p === 'string').slice(0, RECENT_MAX)
    for (const [root, s] of Object.entries(raw.sessions ?? {})) {
      const tabs: string[] = []
      for (const p of s.tabs ?? []) {
        const key = k(p)
        base.files[key] ??= newTab(p)
        tabs.push(key)
      }
      base.sessions[root] = { tabs, active: s.active ? k(s.active) : null, expanded: s.expanded ?? [] }
    }
    base.showHidden = !!raw.showHidden
    base.treeCollapsed = !!raw.treeCollapsed
    base.treeHeight = typeof raw.treeHeight === 'number' ? raw.treeHeight : base.treeHeight
    base.wrap = !!raw.wrap
  } catch {}
  return base
}

const useFiles = create<FilesState>(() => loadPersisted())

let persistTimer: ReturnType<typeof setTimeout> | undefined
useFiles.subscribe((s, prev) => {
  if (
    s.sessions === prev.sessions &&
    s.showHidden === prev.showHidden &&
    s.treeCollapsed === prev.treeCollapsed &&
    s.treeHeight === prev.treeHeight &&
    s.wrap === prev.wrap &&
    s.recent === prev.recent
  )
    return
  clearTimeout(persistTimer)
  persistTimer = setTimeout(() => {
    const st = useFiles.getState()
    const sessions: Record<string, { tabs: string[]; active: string | null; expanded: string[] }> = {}
    for (const [root, ss] of Object.entries(st.sessions).slice(-40)) {
      sessions[root] = { tabs: ss.tabs.map((t) => st.files[t]?.path ?? t), active: ss.active ? (st.files[ss.active]?.path ?? null) : null, expanded: ss.expanded.slice(-300) }
    }
    try {
      const recent = Object.fromEntries(Object.entries(st.recent).slice(-40))
      localStorage.setItem(PERSIST_KEY, JSON.stringify({ sessions, showHidden: st.showHidden, treeCollapsed: st.treeCollapsed, treeHeight: st.treeHeight, wrap: st.wrap, recent }))
    } catch {}
  }, 300)
})

// ------------------------------------------------------------------ back/forward + recent files

/** Set while back/forward re-opens a file, so the move is not recorded as a new location. */
let navigating = false

// every change of a root's active tab is a location (history) and a recent file
useFiles.subscribe((s, prev) => {
  if (s.sessions === prev.sessions) return
  let nav = s.nav
  let recent = s.recent
  for (const [root, ss] of Object.entries(s.sessions)) {
    if (!ss.active || ss.active === prev.sessions[root]?.active) continue
    const path = s.files[ss.active]?.path
    if (!path) continue
    const list = recent[root] ?? []
    recent = { ...recent, [root]: [path, ...list.filter((p) => k(p) !== k(path))].slice(0, RECENT_MAX) }
    if (navigating) continue
    const h = nav[root] ?? { stack: [], index: -1 }
    if (h.stack[h.index] && k(h.stack[h.index]) === k(path)) continue
    const stack = [...h.stack.slice(0, h.index + 1), path].slice(-50)
    nav = { ...nav, [root]: { stack, index: stack.length - 1 } }
  }
  if (nav !== s.nav || recent !== s.recent) useFiles.setState({ nav, recent })
})

/** Alt+Left / Alt+Right: go back / forward between the files opened in a root. */
function navigateFiles(root: string, dir: -1 | 1): void {
  const h = useFiles.getState().nav[root]
  if (!h) return
  const i = h.index + dir
  const path = h.stack[i]
  if (!path) return
  useFiles.setState({ nav: { ...useFiles.getState().nav, [root]: { ...h, index: i } } })
  navigating = true
  try {
    openFileInSession(root, path)
  } finally {
    navigating = false
  }
}

/** Editor states and last-saved documents per file key (not reactive). */
const editorStates = new Map<string, EditorState>()
const savedDocs = new Map<string, Text>()
const active: { key: string | null; view: EditorView | null } = { key: null, view: null }
const realRoots = new Map<string, string>()

function patchFile(key: string, patch: Partial<FileTab>): void {
  const cur = useFiles.getState().files[key]
  if (!cur) return
  useFiles.setState({ files: { ...useFiles.getState().files, [key]: { ...cur, ...patch } } })
}

function patchSession(root: string, fn: (s: Session) => Session): void {
  const st = useFiles.getState()
  useFiles.setState({ sessions: { ...st.sessions, [root]: fn(st.sessions[root] ?? emptySession) } })
}

// ------------------------------------------------------------------ hidden entries

const HIDDEN = new Set([
  '.git',
  '.hg',
  '.svn',
  'node_modules',
  'target',
  '__pycache__',
  '.venv',
  'venv',
  '.mypy_cache',
  '.pytest_cache',
  '.ruff_cache',
  '.tox',
  '.next',
  '.nuxt',
  '.svelte-kit',
  '.turbo',
  '.parcel-cache',
  '.cache',
  '.gradle',
  '.idea',
  '.vs',
  '.DS_Store',
  'Thumbs.db',
  'desktop.ini',
])

const isHiddenName = (name: string): boolean => HIDDEN.has(name)
const hasHiddenSegment = (rel: string): boolean => rel.split('/').some(isHiddenName)

function sortEntries(list: Entry[]): Entry[] {
  return [...list].sort((a, b) => (a.isDir !== b.isDir ? (a.isDir ? -1 : 1) : a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' })))
}

// ------------------------------------------------------------------ directory + git loading

async function loadDir(path: string): Promise<void> {
  const key = k(path)
  const shown = normPath(path)
  const cur = useFiles.getState().dirs[key]
  useFiles.setState({ dirs: { ...useFiles.getState().dirs, [key]: { ...cur, path: shown, loading: true } } })
  try {
    const list = await window.odex.fs.list(shown)
    const entries = sortEntries(list.map((e) => ({ ...e, path: normPath(e.path) })))
    useFiles.setState({ dirs: { ...useFiles.getState().dirs, [key]: { path: shown, entries } } })
  } catch (e) {
    useFiles.setState({ dirs: { ...useFiles.getState().dirs, [key]: { path: shown, entries: [], error: (e as Error).message } } })
  }
}

function gitKind(f: GitFileStatus): GitKind {
  if (f.conflicted) return 'conflict'
  if (f.untracked || f.code === '??') return 'untracked'
  const c = f.code
  if (c.includes('D')) return 'deleted'
  if (c.includes('A')) return 'added'
  if (c.includes('R') || c.includes('C')) return 'renamed'
  return 'modified'
}

const GIT_RANK: Record<GitKind, number> = { untracked: 1, added: 2, deleted: 3, renamed: 3, modified: 4, conflict: 5 }
const GIT_LETTER: Record<GitKind, string> = { modified: 'M', added: 'A', untracked: 'U', deleted: 'D', renamed: 'R', conflict: '!' }
const GIT_LABEL: Record<GitKind, string> = { modified: 'Modified', added: 'Added', untracked: 'Untracked', deleted: 'Deleted', renamed: 'Renamed', conflict: 'Conflict' }

async function refreshGit(root: string): Promise<void> {
  const rk = k(root)
  try {
    let real = realRoots.get(rk)
    if (!real) {
      real = normPath((await window.odex.fs.stat(root)).real || root)
      realRoots.set(rk, real)
    }
    const st = await call('git/status', { cwd: root })
    const info: GitInfo = { repo: st.isRepo, branch: st.branch, files: {}, dirs: {} }
    if (st.isRepo && st.repoRoot) {
      const repo = normPath(st.repoRoot)
      for (const f of st.files) {
        const abs = join(repo, f.path)
        const p = under(root, abs) ? abs : under(real, abs) ? join(root, relPath(real, abs)) : null
        if (!p) continue
        const kind = gitKind(f)
        info.files[k(p)] = kind
        for (let d = parentOf(p); under(root, d) && k(d) !== rk; d = parentOf(d)) {
          const prev = info.dirs[k(d)]
          if (!prev || GIT_RANK[kind] > GIT_RANK[prev]) info.dirs[k(d)] = kind
        }
      }
    }
    useFiles.setState({ git: { ...useFiles.getState().git, [rk]: info } })
  } catch {
    useFiles.setState({ git: { ...useFiles.getState().git, [rk]: { repo: false, files: {}, dirs: {} } } })
  }
}

const gitTimers = new Map<string, ReturnType<typeof setTimeout>>()
function scheduleGit(root: string | null, delay = 400): void {
  if (!root) return
  const rk = k(root)
  clearTimeout(gitTimers.get(rk))
  gitTimers.set(rk, setTimeout(() => void refreshGit(root), delay))
}

// ------------------------------------------------------------------ file loading + saving

function decodeDataUrl(url: string): string {
  const b64 = url.slice(url.indexOf(',') + 1)
  const bin = atob(b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return new TextDecoder().decode(bytes)
}

function textFields(raw: string): { text: string; bom: boolean; eol: '\n' | '\r\n'; notUtf8: boolean } {
  const bom = raw.charCodeAt(0) === 0xfeff
  const text = bom ? raw.slice(1) : raw
  const crlf = (text.match(/\r\n/g) ?? []).length
  const lf = (text.match(/\n/g) ?? []).length - crlf
  return { text, bom, eol: crlf > lf ? '\r\n' : '\n', notUtf8: text.includes('\uFFFD') }
}

function installText(key: string, path: string, raw: string, extra: Partial<FileTab>): void {
  const f = textFields(raw)
  const state = createEditorState(f.text, path, { wrap: useFiles.getState().wrap, readOnly: f.notUtf8 })
  editorStates.set(key, state)
  savedDocs.set(key, state.doc)
  patchFile(key, { kind: 'text', bom: f.bom, eol: f.eol, notUtf8: f.notUtf8, allowEdit: false, dirty: false, diskChanged: false, deleted: false, error: undefined, ...extra })
}

async function loadFile(key: string, maxBytes?: number): Promise<void> {
  const t = useFiles.getState().files[key]
  if (!t || t.kind === 'loading') return
  patchFile(key, { kind: 'loading' })
  try {
    const r = (await window.odex.fs.read(t.path, maxBytes)) as { kind: string; text?: string; size?: number; mtime?: number; dataUrl?: string; mime?: string }
    if (r.kind === 'text') {
      installText(key, t.path, r.text ?? '', { size: r.size, mtime: r.mtime })
    } else if (r.kind === 'media') {
      const st = await window.odex.fs.stat(t.path)
      if (r.mime === 'image/svg+xml') installText(key, t.path, decodeDataUrl(r.dataUrl!), { size: r.size, mtime: st.mtime })
      else if (r.mime === 'application/pdf') patchFile(key, { kind: 'pdf', dataUrl: r.dataUrl, size: r.size, mtime: st.mtime })
      else patchFile(key, { kind: 'image', dataUrl: r.dataUrl, size: r.size, mtime: st.mtime })
    } else if (r.kind === 'dir') {
      patchFile(key, { kind: 'error', error: 'This is a folder.' })
    } else {
      patchFile(key, { kind: r.kind === 'tooLarge' ? 'tooLarge' : 'binary', size: r.size })
    }
  } catch (e) {
    const st = await window.odex.fs.stat(t.path).catch(() => null)
    patchFile(key, st && st.exists ? { kind: 'error', error: (e as Error).message } : { kind: 'missing' })
  }
}

/** Replace a file's editor content (keeps undo history), e.g. after it changed on disk. */
function replaceDoc(key: string, raw: string, mtime: number | undefined): void {
  const st = editorStates.get(key)
  const f = textFields(raw)
  if (!st) return
  const patch: Partial<FileTab> = { mtime, eol: f.eol, bom: f.bom, dirty: false, diskChanged: false, deleted: false }
  if (st.doc.toString() === f.text) {
    savedDocs.set(key, st.doc)
    patchFile(key, patch)
    return
  }
  const spec = { changes: { from: 0, to: st.doc.length, insert: f.text } }
  if (active.key === key && active.view) {
    const tr = active.view.state.update(spec)
    savedDocs.set(key, tr.state.doc)
    patchFile(key, patch)
    active.view.dispatch(tr)
  } else {
    const next = st.update(spec).state
    editorStates.set(key, next)
    savedDocs.set(key, next.doc)
    patchFile(key, patch)
  }
}

async function reloadFromDisk(key: string): Promise<void> {
  const t = useFiles.getState().files[key]
  if (!t) return
  if (t.kind !== 'text') {
    patchFile(key, { kind: 'idle' })
    return loadFile(key)
  }
  try {
    const r = (await window.odex.fs.read(t.path)) as { kind: string; text?: string; mtime?: number; dataUrl?: string; mime?: string }
    if (r.kind === 'text') replaceDoc(key, r.text ?? '', r.mtime)
    else if (r.kind === 'media' && r.mime === 'image/svg+xml') replaceDoc(key, decodeDataUrl(r.dataUrl!), (await window.odex.fs.stat(t.path)).mtime)
    else {
      patchFile(key, { kind: 'idle' })
      await loadFile(key)
    }
  } catch (e) {
    toast(`Could not reload ${basename(t.path)}: ${(e as Error).message}`, 'error')
  }
}

/** Compare a loaded file with the disk: reload clean files, flag dirty ones. */
async function checkDisk(key: string): Promise<void> {
  const t = useFiles.getState().files[key]
  if (!t || (t.kind !== 'text' && t.kind !== 'image' && t.kind !== 'pdf')) return
  const st = await window.odex.fs.stat(t.path)
  const cur = useFiles.getState().files[key]
  if (!cur) return
  if (!st.exists) {
    if (!cur.deleted) patchFile(key, { deleted: true })
    return
  }
  if (cur.mtime !== undefined && Math.abs(st.mtime - cur.mtime) < 1 && !cur.deleted) return
  if (cur.kind === 'text' && cur.dirty) patchFile(key, { diskChanged: true, deleted: false })
  else await reloadFromDisk(key)
}

function currentDoc(key: string): Text | undefined {
  if (active.key === key && active.view) return active.view.state.doc
  return editorStates.get(key)?.doc
}

async function saveFile(key: string, threadRunning: boolean): Promise<boolean> {
  const t = useFiles.getState().files[key]
  const doc = currentDoc(key)
  if (!t || t.kind !== 'text' || !doc) return false
  if (t.notUtf8 && !t.allowEdit) return false
  if (threadRunning) {
    const ok = await confirmDialog('The agent is working', `The agent is running in this thread and may be editing ${basename(t.path)}. Save your changes anyway?`, 'Save')
    if (!ok) return false
  }
  const st = await window.odex.fs.stat(t.path)
  if (st.exists && t.mtime !== undefined && Math.abs(st.mtime - t.mtime) >= 1) {
    const ok = await confirmDialog('File changed on disk', `${basename(t.path)} was changed on disk after you opened it. Overwrite it with your version?`, 'Overwrite', true)
    if (!ok) return false
  }
  const latest = currentDoc(key) ?? doc
  try {
    const text = (t.bom ? '\uFEFF' : '') + latest.sliceString(0, latest.length, t.eol)
    const mtime = await window.odex.fs.write(t.path, text)
    savedDocs.set(key, latest)
    const now = currentDoc(key)
    patchFile(key, { mtime, size: text.length, dirty: !!now && !now.eq(latest), diskChanged: false, deleted: false })
    return true
  } catch (e) {
    toast(`Could not save ${basename(t.path)}: ${(e as Error).message}`, 'error')
    return false
  }
}

// ------------------------------------------------------------------ tabs

export function openFileInSession(root: string, path: string, line?: number): string {
  const key = k(path)
  const st = useFiles.getState()
  if (!st.files[key]) useFiles.setState({ files: { ...st.files, [key]: newTab(path) } })
  patchSession(root, (s) => {
    if (s.tabs.includes(key)) return { ...s, active: key }
    const i = s.active ? s.tabs.indexOf(s.active) : -1
    const tabs = i >= 0 ? [...s.tabs.slice(0, i + 1), key, ...s.tabs.slice(i + 1)] : [...s.tabs, key]
    return { ...s, tabs, active: key }
  })
  if (line && line > 0) patchFile(key, { reveal: { line, at: Date.now() } })
  const t = useFiles.getState().files[key]
  if (t.kind === 'idle' || t.kind === 'missing' || t.kind === 'error') void loadFile(key)
  return key
}

function dropFileIfUnused(key: string): void {
  const st = useFiles.getState()
  if (Object.values(st.sessions).some((s) => s.tabs.includes(key))) return
  const { [key]: _gone, ...files } = st.files
  useFiles.setState({ files })
  editorStates.delete(key)
  savedDocs.delete(key)
  forgetEditorScroll(key)
}

async function closeTabs(root: string, keys: string[]): Promise<void> {
  const st = useFiles.getState()
  const dirty = keys.filter((x) => st.files[x]?.dirty)
  if (dirty.length) {
    const names = dirty.map((x) => basename(st.files[x].path)).join(', ')
    const ok = await confirmDialog('Discard unsaved changes?', `${names} ${dirty.length === 1 ? 'has' : 'have'} unsaved changes that will be lost.`, 'Discard', true)
    if (!ok) return
    for (const x of dirty) {
      // closing discards edits: revert so a reopen starts from the disk version
      patchFile(x, { kind: 'idle', dirty: false })
      editorStates.delete(x)
    }
  }
  patchSession(root, (s) => {
    const tabs = s.tabs.filter((t) => !keys.includes(t))
    let act = s.active
    if (act && keys.includes(act)) {
      const i = s.tabs.indexOf(act)
      act = tabs[Math.min(i, tabs.length - 1)] ?? null
    }
    return { ...s, tabs, active: act }
  })
  for (const x of keys) dropFileIfUnused(x)
}

// ------------------------------------------------------------------ fuzzy filter

function fuzzy(q: string, s: string): { score: number; idx: number[] } | null {
  if (!q) return { score: 0, idx: [] }
  const ql = q.toLowerCase()
  const sl = s.toLowerCase()
  const base = sl.lastIndexOf('/') + 1
  const range = (i: number) => Array.from({ length: ql.length }, (_, j) => i + j)
  let i = sl.indexOf(ql, base)
  if (i >= 0) return { score: 3000 - (i - base) * 4 - (sl.length - base - ql.length), idx: range(i) }
  i = sl.indexOf(ql)
  if (i >= 0) return { score: 2000 - i - sl.length / 10, idx: range(i) }
  const idx: number[] = []
  let score = 1000
  let j = 0
  let prev = -2
  for (let c = 0; c < sl.length && j < ql.length; c++) {
    if (sl[c] !== ql[j]) continue
    if (c === prev + 1) score += 8
    if (c === 0 || '/._- '.includes(s[c - 1])) score += 6
    if (c >= base) score += 3
    idx.push(c)
    prev = c
    j++
  }
  if (j < ql.length) return null
  return { score: score - (idx[idx.length - 1] - idx[0]), idx }
}

function Highlighted({ text, idx, offset = 0 }: { text: string; idx: number[]; offset?: number }) {
  if (!idx.length) return <>{text}</>
  const set = new Set(idx.map((i) => i - offset))
  const out: React.ReactNode[] = []
  let buf = ''
  let on = false
  const flush = (n: number) => {
    if (buf) out.push(on ? <mark key={n}>{buf}</mark> : buf)
    buf = ''
  }
  for (let i = 0; i < text.length; i++) {
    const m = set.has(i)
    if (m !== on) {
      flush(i)
      on = m
    }
    buf += text[i]
  }
  flush(text.length)
  return <>{out}</>
}

// ------------------------------------------------------------------ helpers

function fileIcon(name: string, size = 14) {
  const ext = extOf(name)
  if (['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'bmp', 'ico'].includes(ext)) return <FileImage size={size} />
  if (['json', 'jsonc', 'json5', 'toml', 'yaml', 'yml', 'lock'].includes(ext)) return <FileBraces size={size} />
  if (['md', 'markdown', 'mdx', 'txt', 'rst'].includes(ext)) return <FileText size={size} />
  if (
    ['ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'py', 'rs', 'go', 'java', 'kt', 'c', 'h', 'cpp', 'hpp', 'cs', 'rb', 'php', 'swift', 'css', 'scss', 'html', 'vue', 'svelte', 'sh', 'ps1', 'sql'].includes(
      ext,
    )
  )
    return <FileCode size={size} />
  return <FileIcon size={size} />
}

function formatSize(n?: number): string {
  if (n === undefined) return ''
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${(n / 1024 / 1024).toFixed(1)} MB`
}

const revealLabel = WIN ? 'Reveal in File Explorer' : window.odex?.platform === 'darwin' ? 'Reveal in Finder' : 'Reveal in file manager'

function mention(path: string): void {
  window.dispatchEvent(new CustomEvent('odex:attach', { detail: { type: 'file', path } }))
}

function useRoots(): { roots: string[]; threadRunning: boolean; threadKey: string } {
  const tid = useApp((s) => s.selectedThreadId)
  const cwd = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread.cwd : undefined))
  const wt = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread.worktree?.path : undefined))
  const tProject = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread.projectId : undefined))
  const running = useApp((s) => isRunning(s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread : undefined))
  const diff = useApp((s) => {
    const d = s.selectedThreadId ? s.threads[s.selectedThreadId]?.diffStats : undefined
    return d ? `${d.filesChanged}/${d.additions}/${d.deletions}` : ''
  })
  const newProject = useApp((s) => s.ui.newThreadProjectId)
  const projects = useApp((s) => s.projects)
  const roots = useMemo(() => {
    const ordered = (id: string | null | undefined) => {
      const p = projects.find((x) => x.id === id)
      if (!p) return []
      const primary = p.folders[p.primary] ?? p.folders[0]
      return primary ? [primary, ...p.folders.filter((f) => f !== primary)] : []
    }
    if (tid && cwd) {
      const main = wt ?? cwd
      const extra = wt ? [] : ordered(tProject).filter((f) => k(f) !== k(main))
      return [main, ...extra].map(normPath)
    }
    return ordered(newProject).map(normPath)
  }, [tid, cwd, wt, tProject, newProject, projects])
  return { roots, threadRunning: running, threadKey: `${tid ?? ''}:${running}:${diff}` }
}

// ------------------------------------------------------------------ panel

export function FilesPanel() {
  const { roots, threadRunning, threadKey } = useRoots()
  const [rootPick, setRootPick] = useState<string | null>(null)
  const root = roots.find((r) => rootPick && k(r) === k(rootPick)) ?? roots[0] ?? null
  const sk = root ? k(root) : ''
  const session = useFiles((s) => s.sessions[sk]) ?? emptySession
  const treeCollapsed = useFiles((s) => s.treeCollapsed)
  const treeHeight = useFiles((s) => s.treeHeight)
  const panelRef = useRef<HTMLDivElement>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const runningRef = useRef(threadRunning)
  runningRef.current = threadRunning

  // consume files opened from elsewhere (chat links, palette, sources)
  const fileToOpen = useApp((s) => s.fileToOpen)
  useEffect(() => {
    if (!fileToOpen) return
    useApp.setState({ fileToOpen: null })
    const raw = fileToOpen.path.replace(/^file:\/\//i, '')
    const p = isAbs(raw) || !root ? normPath(raw) : join(root, raw)
    const line = fileToOpen.line
    void window.odex.fs.stat(p).then((st) => {
      const inTree = !!root && under(root, p)
      if (st.isDir) {
        // a folder: reveal and expand it in the tree instead of opening a tab
        if (inTree && k(p) !== k(root)) {
          void revealInTree(sk, root, p).then(() => {
            patchSession(sk, (s) => (s.expanded.some((d) => k(d) === k(p)) ? s : { ...s, expanded: [...s.expanded, p] }))
            void loadDir(p)
          })
          setSelected(k(p))
        }
        return
      }
      const key = openFileInSession(sk, p, line)
      if (inTree) {
        void revealInTree(sk, root, p)
        setSelected(key)
      }
    })
  }, [fileToOpen, root, sk])

  // initial load + git for the root
  useEffect(() => {
    if (!root) return
    void loadDir(root)
    for (const d of useFiles.getState().sessions[sk]?.expanded ?? []) if (under(root, d)) void loadDir(d)
    void refreshGit(root)
  }, [root, sk])

  // agent activity → refresh decorations
  useEffect(() => {
    scheduleGit(root, 800)
  }, [threadKey, root])

  // load the active tab lazily
  const activeKey = session.active
  const activeKind = useFiles((s) => (activeKey ? s.files[activeKey]?.kind : undefined))
  useEffect(() => {
    if (activeKey && activeKind === 'idle') void loadFile(activeKey)
  }, [activeKey, activeKind])

  // watchers: the root (recursive where the OS supports it, else plus each
  // expanded folder) and every loaded file (polled)
  const [rootWatch, setRootWatch] = useState<'recursive' | 'flat' | null>(null)
  useEffect(() => {
    if (!root) return
    let alive = true
    void window.odex.fs.watch(root, [...HIDDEN]).then((kind) => alive && setRootWatch(kind === 'recursive' ? 'recursive' : 'flat'))
    return () => {
      alive = false
      setRootWatch(null)
      void window.odex.fs.unwatch(root)
    }
  }, [root])
  const loadedFiles = useFiles((s) =>
    session.tabs
      .filter((t) => ['text', 'image', 'pdf'].includes(s.files[t]?.kind ?? ''))
      .map((t) => s.files[t].path)
      .join('\n'),
  )
  const watchList = useMemo(() => {
    const out = new Map<string, string>()
    if (root && rootWatch === 'flat') for (const d of session.expanded) if (under(root, d)) out.set(k(d), d)
    for (const p of loadedFiles ? loadedFiles.split('\n') : []) out.set(k(p), p)
    return out
  }, [root, rootWatch, session.expanded, loadedFiles])
  const watched = useRef(new Map<string, string>())
  useEffect(() => {
    const cur = watched.current
    for (const [key, p] of cur) if (!watchList.has(key)) void window.odex.fs.unwatch(p)
    for (const [key, p] of watchList) if (!cur.has(key)) void window.odex.fs.watch(p, [...HIDDEN])
    watched.current = new Map(watchList)
  }, [watchList])
  useEffect(
    () => () => {
      for (const p of watched.current.values()) void window.odex.fs.unwatch(p)
      watched.current = new Map()
    },
    [],
  )

  // disk changes
  const rootRef = useRef(root)
  rootRef.current = root
  useEffect(() => {
    const off = window.odex.fs.onChange(({ path, names, all }) => {
      const key = k(path)
      const st = useFiles.getState()
      if (st.files[key] && !st.dirs[key]) {
        void checkDisk(key)
        scheduleGit(rootRef.current)
        return
      }
      const dirs = new Set<string>()
      const files = new Set<string>()
      if (all) {
        for (const d of Object.keys(st.dirs)) if (under(path, d) && st.dirs[d].entries) dirs.add(d)
        for (const f of Object.keys(st.files)) if (under(path, f)) files.add(f)
      } else {
        if (st.dirs[key]?.entries) dirs.add(key)
        for (const n of names) {
          const full = k(join(path, n))
          const parent = k(parentOf(full))
          if (st.dirs[parent]?.entries) dirs.add(parent)
          if (st.dirs[full]?.entries) dirs.add(full)
          if (st.files[full]) files.add(full)
        }
      }
      for (const d of dirs) void loadDir(st.dirs[d].path)
      for (const f of files) void checkDisk(f)
      scheduleGit(rootRef.current)
    })
    // re-check everything when the window regains focus or the panel remounts
    const recheck = () => {
      const st = useFiles.getState()
      for (const t of st.sessions[k(rootRef.current ?? '')]?.tabs ?? st.sessions['']?.tabs ?? []) void checkDisk(t)
    }
    recheck()
    window.addEventListener('focus', recheck)
    return () => {
      off()
      window.removeEventListener('focus', recheck)
    }
  }, [])

  const refresh = useCallback(() => {
    if (!root) return
    const st = useFiles.getState()
    void loadDir(root)
    for (const d of st.sessions[sk]?.expanded ?? []) if (under(root, d)) void loadDir(d)
    for (const t of st.sessions[sk]?.tabs ?? []) void checkDisk(t)
    void refreshGit(root)
  }, [root, sk])

  const hasTabs = session.tabs.length > 0
  const [panelH, setPanelH] = useState(700)
  useEffect(() => {
    const el = panelRef.current
    if (!el) return
    const ro = new ResizeObserver(() => setPanelH(el.clientHeight || 700))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])
  const treeMax = Math.max(120, panelH - 140)
  const shownTree = Math.min(treeHeight, treeMax)

  const onKeyDown = (e: React.KeyboardEvent) => {
    if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 's' && session.active) {
      e.preventDefault()
      e.stopPropagation()
      void saveFile(session.active, runningRef.current)
    } else if (e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      // back / forward between opened files (text fields keep their own Alt+arrows)
      const t = e.target as HTMLElement
      if (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA') return
      e.preventDefault()
      e.stopPropagation()
      navigateFiles(sk, e.key === 'ArrowLeft' ? -1 : 1)
    }
  }

  return (
    <div className="files-panel" ref={panelRef} onKeyDown={onKeyDown}>
      <section
        className={`files-tree ${!hasTabs && !treeCollapsed ? 'fill' : ''}`}
        style={hasTabs && !treeCollapsed ? { height: shownTree } : undefined}
        aria-label="File tree"
      >
        <TreeHeader root={root} roots={roots} sk={sk} onPickRoot={setRootPick} onRefresh={refresh} />
        {!treeCollapsed &&
          (root ? (
            <TreeBody root={root} sk={sk} selected={selected} setSelected={setSelected} />
          ) : (
            <div className="empty small">
              <FolderTree size={20} />
              Choose a project or open a thread to browse its files.
              <button className="btn btn-sm" onClick={() => window.dispatchEvent(new CustomEvent('odex:open-picker', { detail: 'project' }))}>
                Choose project
              </button>
            </div>
          ))}
      </section>
      {hasTabs && !treeCollapsed && (
        <ResizeHandle axis="y" value={shownTree} min={80} max={treeMax} onChange={(v) => useFiles.setState({ treeHeight: Math.round(v) })} label="Resize file tree" />
      )}
      {hasTabs && <EditorArea root={root} sk={sk} threadRunning={threadRunning} onRevealInTree={(p) => root && (void revealInTree(sk, root, p), setSelected(k(p)))} />}
    </div>
  )
}

async function revealInTree(sk: string, root: string, p: string): Promise<void> {
  const chain: string[] = []
  for (let d = parentOf(p); under(root, d) && k(d) !== k(root); d = parentOf(d)) chain.unshift(d)
  if (useFiles.getState().treeCollapsed) useFiles.setState({ treeCollapsed: false })
  patchSession(sk, (s) => {
    const have = new Set(s.expanded.map(k))
    const add = chain.filter((d) => !have.has(k(d)))
    return add.length ? { ...s, expanded: [...s.expanded, ...add] } : s
  })
  for (const d of chain) if (!useFiles.getState().dirs[k(d)]?.entries) await loadDir(d)
  window.dispatchEvent(new CustomEvent('odex:files-scroll-to', { detail: k(p) }))
}

// ------------------------------------------------------------------ tree header

function TreeHeader(props: { root: string | null; roots: string[]; sk: string; onPickRoot: (r: string) => void; onRefresh: () => void }) {
  const collapsed = useFiles((s) => s.treeCollapsed)
  const showHidden = useFiles((s) => s.showHidden)
  const branch = useFiles((s) => (props.root ? s.git[k(props.root)]?.branch : undefined))
  const recent = useFiles((s) => s.recent[props.sk]) ?? []
  const [recentAnchor, setRecentAnchor] = useState<HTMLElement | null>(null)
  const recentItems: MenuItem[] = [
    { label: 'Recent files', header: true },
    ...(recent.length
      ? recent.map((p) => ({
          label: <span title={p}>{relPath(props.root, p)}</span>,
          icon: fileIcon(p, 13),
          onSelect: () => void openFileInSession(props.sk, p),
        }))
      : [{ label: 'No recent files yet', disabled: true }]),
    ...(recent.length ? [{ separator: true, label: '' }, { label: 'Clear recent files', onSelect: () => useFiles.setState({ recent: { ...useFiles.getState().recent, [props.sk]: [] } }) }] : []),
  ]
  return (
    <div className="panel-header files-head">
      <button
        className="icon-btn sm"
        aria-label={collapsed ? 'Expand file tree' : 'Collapse file tree'}
        aria-expanded={!collapsed}
        title={collapsed ? 'Show file tree' : 'Hide file tree'}
        onClick={() => useFiles.setState({ treeCollapsed: !collapsed })}
      >
        {collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
      </button>
      {props.roots.length > 1 ? (
        <select className="select files-root-select" aria-label="Folder" value={props.root ?? ''} onChange={(e) => props.onPickRoot(e.target.value)} title={props.root ?? ''}>
          {props.roots.map((r) => (
            <option key={r} value={r}>
              {basename(r)}
            </option>
          ))}
        </select>
      ) : (
        <span className="panel-title ellipsis" title={props.root ?? ''}>
          {props.root ? basename(props.root) : 'Files'}
        </span>
      )}
      {branch && (
        <span className="files-branch xs subtle ellipsis" title={`Branch ${branch}`}>
          <GitBranch size={11} />
          {branch}
        </span>
      )}
      <span className="spacer" />
      {props.root && (
        <>
          <button className={`icon-btn sm ${recentAnchor ? 'active' : ''}`} aria-label="Recent files" title="Recent files" onClick={(e) => setRecentAnchor(recentAnchor ? null : e.currentTarget)}>
            <History size={13} />
          </button>
          {recentAnchor && <Menu anchor={recentAnchor} items={recentItems} onClose={() => setRecentAnchor(null)} align="right" minWidth={260} />}
          <button
            className={`icon-btn sm ${showHidden ? 'active' : ''}`}
            aria-label="Show ignored files"
            aria-pressed={showHidden}
            title={showHidden ? 'Hide .git, node_modules, target, …' : 'Show .git, node_modules, target, …'}
            onClick={() => useFiles.setState({ showHidden: !showHidden })}
          >
            {showHidden ? <Eye size={13} /> : <EyeOff size={13} />}
          </button>
          <button className="icon-btn sm" aria-label="Collapse all folders" title="Collapse all folders" onClick={() => patchSession(props.sk, (s) => ({ ...s, expanded: [] }))}>
            <ChevronsDownUp size={13} />
          </button>
          <button className="icon-btn sm" aria-label="Refresh files" title="Refresh" onClick={props.onRefresh}>
            <RefreshCw size={13} />
          </button>
        </>
      )}
    </div>
  )
}

// ------------------------------------------------------------------ tree body

interface Row {
  entry: Entry
  depth: number
  loading?: boolean
}

const ROW_H = 24

function TreeBody(props: { root: string; sk: string; selected: string | null; setSelected: (k: string | null) => void }) {
  const { root, sk, selected, setSelected } = props
  const dirs = useFiles((s) => s.dirs)
  const expandedList = useFiles((s) => s.sessions[sk]?.expanded) ?? emptySession.expanded
  const showHidden = useFiles((s) => s.showHidden)
  const git = useFiles((s) => s.git[k(root)])
  const activeKey = useFiles((s) => s.sessions[sk]?.active ?? null)
  const [filter, setFilter] = useState('')
  const [allMode, setAllMode] = useState(false)
  const [menu, setMenu] = useState<{ x: number; y: number; entry: Entry } | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const expanded = useMemo(() => new Set(expandedList.map(k)), [expandedList])

  const rows = useMemo(() => {
    const out: Row[] = []
    const walk = (dir: string, depth: number) => {
      const ds = dirs[k(dir)]
      if (!ds?.entries) {
        if (ds?.loading || !ds) out.push({ entry: { name: 'Loading…', path: `${dir}${SEP}\0loading`, isDir: false, isSymlink: false, size: 0, mtime: 0 }, depth, loading: true })
        return
      }
      for (const e of ds.entries) {
        if (!showHidden && isHiddenName(e.name)) continue
        out.push({ entry: e, depth })
        if (e.isDir && expanded.has(k(e.path))) walk(e.path, depth + 1)
      }
    }
    walk(root, 0)
    return out
  }, [dirs, root, expanded, showHidden])

  const toggleDir = useCallback(
    (e: Entry, open?: boolean) => {
      const key = k(e.path)
      const isOpen = expanded.has(key)
      const want = open ?? !isOpen
      if (want === isOpen) return
      patchSession(sk, (s) => ({ ...s, expanded: want ? [...s.expanded, e.path] : s.expanded.filter((d) => k(d) !== key && !under(e.path, d)) }))
      if (want) void loadDir(e.path)
    },
    [expanded, sk],
  )

  const open = useCallback(
    (e: Entry) => {
      setSelected(k(e.path))
      if (e.isDir) toggleDir(e)
      else openFileInSession(sk, e.path)
    },
    [sk, toggleDir, setSelected],
  )

  const virt = useVirtualizer({ count: rows.length, getScrollElement: () => scrollRef.current, estimateSize: () => ROW_H, overscan: 16 })
  const rowsRef = useRef(rows)
  rowsRef.current = rows

  // scroll requests (reveal in tree)
  useEffect(() => {
    const h = (ev: Event) => {
      const key = (ev as CustomEvent<string>).detail
      setSelected(key)
      requestAnimationFrame(() => {
        const i = rowsRef.current.findIndex((r) => k(r.entry.path) === key)
        if (i >= 0) virt.scrollToIndex(i, { align: 'auto' })
      })
    }
    window.addEventListener('odex:files-scroll-to', h)
    return () => window.removeEventListener('odex:files-scroll-to', h)
  }, [virt, setSelected])

  const onKeyDown = (ev: React.KeyboardEvent) => {
    if (ev.altKey) return // Alt+arrows: file back/forward (panel handler)
    const i = rows.findIndex((r) => k(r.entry.path) === selected)
    const row = rows[i]
    const go = (n: number) => {
      const r = rows[Math.max(0, Math.min(rows.length - 1, n))]
      if (!r || r.loading) return
      setSelected(k(r.entry.path))
      virt.scrollToIndex(rows.indexOf(r), { align: 'auto' })
    }
    switch (ev.key) {
      case 'ArrowDown':
        ev.preventDefault()
        go(i < 0 ? 0 : i + 1)
        break
      case 'ArrowUp':
        ev.preventDefault()
        go(i < 0 ? 0 : i - 1)
        break
      case 'Home':
        ev.preventDefault()
        go(0)
        break
      case 'End':
        ev.preventDefault()
        go(rows.length - 1)
        break
      case 'ArrowRight':
        if (!row?.entry.isDir) break
        ev.preventDefault()
        if (!expanded.has(k(row.entry.path))) toggleDir(row.entry, true)
        else go(i + 1)
        break
      case 'ArrowLeft': {
        if (!row) break
        ev.preventDefault()
        if (row.entry.isDir && expanded.has(k(row.entry.path))) toggleDir(row.entry, false)
        else {
          const parent = k(parentOf(row.entry.path))
          const pi = rows.findIndex((r) => k(r.entry.path) === parent)
          if (pi >= 0) go(pi)
        }
        break
      }
      case 'Enter':
      case ' ':
        if (!row || row.loading) break
        ev.preventDefault()
        open(row.entry)
        break
      case 'ContextMenu':
        if (!row || row.loading) break
        ev.preventDefault()
        {
          const el = scrollRef.current?.querySelector<HTMLElement>(`[data-key="${CSS.escape(k(row.entry.path))}"]`)
          const r = el?.getBoundingClientRect()
          setMenu({ x: r ? r.left + 24 : 100, y: r ? r.bottom : 100, entry: row.entry })
        }
        break
    }
  }

  return (
    <>
      <FilterBox value={filter} onChange={setFilter} allMode={allMode} setAllMode={setAllMode} />
      {filter.trim() ? (
        <FilterResults
          root={root}
          sk={sk}
          query={filter.trim()}
          allMode={allMode}
          setAllMode={setAllMode}
          onPickDir={(p) => {
            setFilter('')
            const ent: Entry = { name: basename(p), path: p, isDir: true, isSymlink: false, size: 0, mtime: 0 }
            void revealInTree(sk, root, p).then(() => toggleDir(ent, true))
          }}
          onMenu={(x, y, entry) => setMenu({ x, y, entry })}
        />
      ) : (
        <div
          ref={scrollRef}
          className="ft-scroll"
          role="tree"
          aria-label="Files"
          tabIndex={0}
          onKeyDown={onKeyDown}
          onFocus={() => {
            if (!selected && rows[0] && !rows[0].loading) setSelected(k(rows[0].entry.path))
          }}
        >
          {rows.length === 0 && dirs[k(root)]?.entries && <div className="empty xs">{dirs[k(root)]?.error ? `Cannot read folder: ${dirs[k(root)]?.error}` : 'This folder is empty.'}</div>}
          <div style={{ height: virt.getTotalSize(), position: 'relative' }}>
            {virt.getVirtualItems().map((vi) => {
              const r = rows[vi.index]
              if (!r) return null
              const e = r.entry
              const key = k(e.path)
              if (r.loading)
                return (
                  <div key={key} className="ft-row ft-loading" style={{ transform: `translateY(${vi.start}px)`, paddingLeft: 10 + r.depth * 12 + 18 }}>
                    <span className="spinner" style={{ width: 10, height: 10 }} /> <span className="xs subtle">Loading…</span>
                  </div>
                )
              const isOpen = e.isDir && expanded.has(key)
              const g = e.isDir ? git?.dirs[key] : git?.files[key]
              return (
                <div
                  key={key}
                  data-key={key}
                  role="treeitem"
                  aria-label={e.name}
                  aria-level={r.depth + 1}
                  aria-expanded={e.isDir ? isOpen : undefined}
                  aria-selected={selected === key}
                  data-git={g}
                  data-hidden={isHiddenName(e.name) || undefined}
                  data-active={activeKey === key || undefined}
                  className={`ft-row ${e.isDir ? 'dir' : 'file'}`}
                  style={{ transform: `translateY(${vi.start}px)`, paddingLeft: 6 + r.depth * 12 }}
                  title={`${relPath(root, e.path)}${g ? ` · ${GIT_LABEL[g]}` : ''}${e.isDir ? '' : ` · ${formatSize(e.size)}`}`}
                  onClick={() => open(e)}
                  onContextMenu={(ev) => {
                    ev.preventDefault()
                    setSelected(key)
                    setMenu({ x: ev.clientX, y: ev.clientY, entry: e })
                  }}
                  draggable
                  onDragStart={(ev) => ev.dataTransfer.setData('text/plain', e.path)}
                >
                  {r.depth > 0 && <span className="ft-guides" style={{ width: r.depth * 12 }} aria-hidden />}
                  <span className="ft-twisty" aria-hidden>
                    {e.isDir ? isOpen ? <ChevronDown size={12} /> : <ChevronRight size={12} /> : null}
                  </span>
                  <span className="ft-icon" aria-hidden>
                    {e.isDir ? isOpen ? <FolderOpen size={14} /> : <Folder size={14} /> : fileIcon(e.name)}
                  </span>
                  <span className="ft-name ellipsis">{e.name}</span>
                  {g && (e.isDir ? <span className="ft-dot" aria-hidden /> : <span className="ft-badge">{GIT_LETTER[g]}</span>)}
                </div>
              )
            })}
          </div>
        </div>
      )}
      {menu && <EntryMenu root={root} sk={sk} entry={menu.entry} at={{ x: menu.x, y: menu.y }} expanded={expanded.has(k(menu.entry.path))} onToggle={() => toggleDir(menu.entry)} onClose={() => setMenu(null)} />}
    </>
  )
}

function FilterBox(props: { value: string; onChange: (v: string) => void; allMode: boolean; setAllMode: (v: boolean) => void }) {
  const ref = useRef<HTMLInputElement>(null)
  return (
    <div className="files-filter">
      <Search size={12} className="subtle" aria-hidden />
      <input
        ref={ref}
        className="files-filter-input"
        placeholder={props.allMode ? 'Search all files' : 'Filter files'}
        aria-label="Filter files"
        value={props.value}
        spellCheck={false}
        onChange={(e) => props.onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Escape' && props.value) {
            e.stopPropagation()
            props.onChange('')
          } else if (e.key === 'ArrowDown' || e.key === 'Enter') {
            e.preventDefault()
            const first = document.querySelector<HTMLElement>('.files-results [role="option"]')
            if (e.key === 'Enter') first?.click()
            else first?.focus()
          }
        }}
      />
      {props.value && (
        <button className="icon-btn sm" aria-label="Clear filter" onClick={() => (props.onChange(''), ref.current?.focus())}>
          <X size={12} />
        </button>
      )}
      <button
        className={`files-all-btn ${props.allMode ? 'on' : ''}`}
        aria-pressed={props.allMode}
        aria-label="All files"
        title="Search every file in the folder, not only expanded folders"
        onClick={() => {
          props.setAllMode(!props.allMode)
          ref.current?.focus()
        }}
      >
        <FolderSearch size={12} /> All
      </button>
    </div>
  )
}

interface ResultRow {
  path: string
  rel: string
  isDir: boolean
  idx: number[]
  score: number
}

function FilterResults(props: {
  root: string
  sk: string
  query: string
  allMode: boolean
  setAllMode: (v: boolean) => void
  onPickDir: (p: string) => void
  onMenu: (x: number, y: number, e: Entry) => void
}) {
  const { root, sk, query, allMode } = props
  const dirs = useFiles((s) => s.dirs)
  const showHidden = useFiles((s) => s.showHidden)
  const git = useFiles((s) => s.git[k(root)])
  const [remote, setRemote] = useState<ResultRow[] | null>(null)
  const [searching, setSearching] = useState(false)
  const q = query.replace(/\\/g, '/').replace(/\s+/g, '')

  const loaded = useMemo(() => {
    if (allMode) return []
    const out: ResultRow[] = []
    for (const [dk, ds] of Object.entries(dirs)) {
      if (!ds.entries || !under(root, dk)) continue
      for (const e of ds.entries) {
        const rel = relPath(root, e.path)
        if (!showHidden && hasHiddenSegment(rel)) continue
        const m = fuzzy(q, rel)
        if (m) out.push({ path: e.path, rel, isDir: e.isDir, idx: m.idx, score: m.score })
      }
    }
    return out.sort((a, b) => b.score - a.score || a.rel.length - b.rel.length).slice(0, 200)
  }, [dirs, root, q, showHidden, allMode])

  useEffect(() => {
    if (!allMode) return
    let cancelled = false
    setSearching(true)
    const h = setTimeout(() => {
      void call('fs/search', { roots: [root], query: q, limit: 200 })
        .then((r) => {
          if (cancelled) return
          const rows = r.files
            .map((f) => {
              // engine paths are relative to their search root
              const abs = isAbs(f.path) ? normPath(f.path) : join(f.root || root, f.path)
              const rel = relPath(root, abs)
              const m = fuzzy(q, rel)
              return { path: abs, rel, isDir: false, idx: m?.idx ?? [], score: f.score }
            })
            .filter((x) => showHidden || !hasHiddenSegment(x.rel))
          setRemote(rows)
        })
        .catch(() => !cancelled && setRemote([]))
        .finally(() => !cancelled && setSearching(false))
    }, 120)
    return () => {
      cancelled = true
      clearTimeout(h)
    }
  }, [allMode, q, root, showHidden])

  const rows = allMode ? (remote ?? []) : loaded
  const onKey = (e: React.KeyboardEvent<HTMLElement>) => {
    const el = e.currentTarget
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      ;(el.nextElementSibling as HTMLElement | null)?.focus()
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      const prev = el.previousElementSibling as HTMLElement | null
      if (prev) prev.focus()
      else document.querySelector<HTMLInputElement>('.files-filter-input')?.focus()
    }
  }
  return (
    <div className="files-results" role="listbox" aria-label="Matching files">
      {rows.map((r) => {
        const name = r.rel.split('/').pop() ?? r.rel
        const dir = r.rel.slice(0, r.rel.length - name.length).replace(/\/$/, '')
        const g = r.isDir ? git?.dirs[k(r.path)] : git?.files[k(r.path)]
        const entry: Entry = { name, path: r.path, isDir: r.isDir, isSymlink: false, size: 0, mtime: 0 }
        return (
          <button
            key={r.path}
            role="option"
            aria-selected={false}
            aria-label={r.rel}
            className="files-result"
            data-git={g}
            title={r.rel}
            onKeyDown={onKey}
            onClick={() => (r.isDir ? props.onPickDir(r.path) : openFileInSession(sk, r.path))}
            onContextMenu={(e) => {
              e.preventDefault()
              props.onMenu(e.clientX, e.clientY, entry)
            }}
          >
            <span className="ft-icon">{r.isDir ? <Folder size={13} /> : fileIcon(name, 13)}</span>
            <span className="ft-name">
              <Highlighted text={name} idx={r.idx} offset={r.rel.length - name.length} />
            </span>
            <span className="files-result-dir ellipsis">
              <Highlighted text={dir} idx={r.idx.filter((i) => i < dir.length)} />
            </span>
            {g && !r.isDir && <span className="ft-badge">{GIT_LETTER[g]}</span>}
          </button>
        )
      })}
      {allMode && searching && !rows.length && (
        <div className="empty xs">
          <span className="spinner" /> Searching…
        </div>
      )}
      {!rows.length && !(allMode && searching) && (
        <div className="empty xs">
          {allMode ? 'No files match.' : 'No loaded files match.'}
          {!allMode && (
            <button className="btn btn-sm" onClick={() => props.setAllMode(true)}>
              <FolderSearch size={12} /> Search all files
            </button>
          )}
        </div>
      )}
      {!allMode && rows.length > 0 && (
        <button className="files-result files-result-more xs" onClick={() => props.setAllMode(true)}>
          <FolderSearch size={12} /> Search all files for “{query}”
        </button>
      )}
    </div>
  )
}

function EntryMenu(props: { root: string; sk: string; entry: Entry; at: { x: number; y: number }; expanded: boolean; onToggle: () => void; onClose: () => void }) {
  const { root, sk, entry: e } = props
  const items: MenuItem[] = [
    e.isDir
      ? { label: props.expanded ? 'Collapse' : 'Expand', icon: props.expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />, onSelect: props.onToggle }
      : { label: 'Open', icon: <FileText size={13} />, onSelect: () => openFileInSession(sk, e.path) },
    { label: 'Mention in composer', icon: <AtSign size={13} />, onSelect: () => mention(e.path) },
    { separator: true, label: '' },
    { label: 'Copy path', icon: <Copy size={13} />, onSelect: () => copy(e.path, 'Path copied') },
    { label: 'Copy relative path', icon: <Copy size={13} />, onSelect: () => copy(relPath(root, e.path), 'Relative path copied') },
    { separator: true, label: '' },
    ...(e.isDir ? [] : [{ label: 'Open in external editor', icon: <ExternalLink size={13} />, onSelect: () => void window.odex.shell.openInEditor(e.path) }]),
    { label: revealLabel, icon: <FolderOpen size={13} />, onSelect: () => void window.odex.shell.showItem(e.path) },
  ]
  return <Menu anchor={props.at} items={items} onClose={props.onClose} minWidth={220} />
}

// ------------------------------------------------------------------ editor area

const useCursor = create<{ line: number; col: number; sel: number }>(() => ({ line: 1, col: 1, sel: 0 }))

function EditorArea(props: { root: string | null; sk: string; threadRunning: boolean; onRevealInTree: (p: string) => void }) {
  const { root, sk } = props
  const session = useFiles((s) => s.sessions[sk]) ?? emptySession
  const files = useFiles((s) => s.files)
  const wrap = useFiles((s) => s.wrap)
  const git = useFiles((s) => (root ? s.git[k(root)] : undefined))
  const key = session.active
  const t = key ? files[key] : undefined
  const [tabMenu, setTabMenu] = useState<{ x: number; y: number; key: string } | null>(null)
  const [more, setMore] = useState<HTMLElement | null>(null)
  const dragFrom = useRef<number | null>(null)
  const tabsRef = useRef<HTMLDivElement>(null)
  const runningRef = useRef(props.threadRunning)
  runningRef.current = props.threadRunning

  // keep the active tab visible
  useEffect(() => {
    tabsRef.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest', inline: 'nearest' })
  }, [key])

  const onUpdate = useCallback(
    (u: ViewUpdate) => {
      if (!key) return
      editorStates.set(key, u.state)
      if (u.docChanged) {
        const saved = savedDocs.get(key)
        const dirty = !saved || u.state.doc.length !== saved.length || !u.state.doc.eq(saved)
        if (dirty !== useFiles.getState().files[key]?.dirty) patchFile(key, { dirty })
      }
      if (u.docChanged || u.selectionSet) {
        const head = u.state.selection.main.head
        const line = u.state.doc.lineAt(head)
        const sel = u.state.selection.ranges.reduce((n, r) => n + (r.to - r.from), 0)
        useCursor.setState({ line: line.number, col: head - line.from + 1, sel })
      }
    },
    [key],
  )

  const onView = useCallback(
    (v: EditorView | null) => {
      active.view = v
      active.key = v ? key : null
      if (v && key) {
        const head = v.state.selection.main.head
        const line = v.state.doc.lineAt(head)
        useCursor.setState({ line: line.number, col: head - line.from + 1, sel: 0 })
      }
    },
    [key],
  )
  const save = useCallback(() => {
    if (key) void saveFile(key, runningRef.current)
  }, [key])

  const tabItems = (tk: string): MenuItem[] => {
    const p = files[tk]?.path ?? tk
    const others = session.tabs.filter((x) => x !== tk)
    return [
      { label: 'Close', onSelect: () => void closeTabs(sk, [tk]) },
      { label: 'Close others', disabled: !others.length, onSelect: () => void closeTabs(sk, others) },
      { label: 'Close all', onSelect: () => void closeTabs(sk, session.tabs) },
      { separator: true, label: '' },
      { label: 'Mention in composer', icon: <AtSign size={13} />, onSelect: () => mention(p) },
      { label: 'Copy path', icon: <Copy size={13} />, onSelect: () => copy(p, 'Path copied') },
      { label: 'Copy relative path', icon: <Copy size={13} />, onSelect: () => copy(relPath(root, p), 'Relative path copied') },
      { separator: true, label: '' },
      { label: 'Reveal in file tree', icon: <ListTree size={13} />, disabled: !root || !under(root, p), onSelect: () => props.onRevealInTree(p) },
      { label: revealLabel, icon: <FolderOpen size={13} />, onSelect: () => void window.odex.shell.showItem(p) },
    ]
  }

  const line = () => (active.view && active.key === key ? active.view.state.doc.lineAt(active.view.state.selection.main.head).number : undefined)
  const ext = t ? extOf(t.path) : ''
  const previewable = t?.kind === 'text' && (ext === 'md' || ext === 'markdown' || ext === 'mdx' || ext === 'svg' || isHtmlExt(ext))
  const g = t && key ? git?.files[key] : undefined
  const readOnly = !!t && t.notUtf8 && !t.allowEdit
  const nav = useFiles((s) => s.nav[sk])
  const canBack = !!nav && nav.index > 0
  const canForward = !!nav && nav.index < nav.stack.length - 1
  const annotations = useAgentEdits(t?.kind === 'text' ? t.path : undefined, t?.mtime)

  const moreItems: MenuItem[] = t
    ? [
        { label: 'Open in external editor', icon: <ExternalLink size={13} />, onSelect: () => void window.odex.shell.openInEditor(t.path, line()) },
        { label: revealLabel, icon: <FolderOpen size={13} />, onSelect: () => void window.odex.shell.showItem(t.path) },
        { label: 'Save a copy as…', icon: <Download size={13} />, disabled: t.deleted, onSelect: () => void saveFileCopy(t.path) },
        { label: 'Reveal in file tree', icon: <ListTree size={13} />, disabled: !root || !under(root, t.path), onSelect: () => props.onRevealInTree(t.path) },
        { separator: true, label: '' },
        { label: 'Mention in composer', icon: <AtSign size={13} />, onSelect: () => mention(t.path) },
        { label: 'Copy path', icon: <Copy size={13} />, onSelect: () => copy(t.path, 'Path copied') },
        { label: 'Copy relative path', icon: <Copy size={13} />, onSelect: () => copy(relPath(root, t.path), 'Relative path copied') },
        { separator: true, label: '' },
        {
          label: 'Reload from disk',
          icon: <RefreshCw size={13} />,
          onSelect: async () => {
            if (t.dirty && !(await confirmDialog('Reload from disk?', `Your unsaved changes to ${basename(t.path)} will be replaced by the version on disk.`, 'Reload', true))) return
            void reloadFromDisk(key!)
          },
        },
      ]
    : []

  return (
    <section className="files-editor" aria-label="Editor">
      <div
        className="files-tabs"
        role="tablist"
        aria-label="Open files"
        ref={tabsRef}
        onWheel={(e) => {
          if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) e.currentTarget.scrollLeft += e.deltaY
        }}
      >
        {session.tabs.map((tk, i) => {
          const f = files[tk]
          const name = basename(f?.path ?? tk)
          const tg = git?.files[tk]
          return (
            <div
              key={tk}
              role="tab"
              aria-selected={tk === key}
              aria-label={name}
              tabIndex={0}
              className="files-tab"
              data-dirty={f?.dirty || undefined}
              data-git={tg}
              title={f?.path}
              draggable
              onDragStart={(e) => {
                dragFrom.current = i
                e.dataTransfer.effectAllowed = 'move'
              }}
              onDragOver={(e) => {
                if (dragFrom.current !== null) e.preventDefault()
              }}
              onDrop={(e) => {
                e.preventDefault()
                const from = dragFrom.current
                dragFrom.current = null
                if (from === null || from === i) return
                patchSession(sk, (s) => {
                  const tabs = [...s.tabs]
                  const [m] = tabs.splice(from, 1)
                  tabs.splice(i, 0, m)
                  return { ...s, tabs }
                })
              }}
              onClick={() => patchSession(sk, (s) => ({ ...s, active: tk }))}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault()
                  patchSession(sk, (s) => ({ ...s, active: tk }))
                }
              }}
              onAuxClick={(e) => {
                if (e.button === 1) void closeTabs(sk, [tk])
              }}
              onContextMenu={(e) => {
                e.preventDefault()
                setTabMenu({ x: e.clientX, y: e.clientY, key: tk })
              }}
            >
              <span className="ft-icon">{fileIcon(name, 13)}</span>
              <span className="files-tab-name ellipsis">{name}</span>
              <button
                className="files-tab-close"
                aria-label={`Close ${name}`}
                title={f?.dirty ? 'Unsaved changes. Close' : 'Close'}
                onClick={(e) => {
                  e.stopPropagation()
                  void closeTabs(sk, [tk])
                }}
              >
                <span className="files-dirty-dot" aria-hidden />
                <X size={12} className="files-x" />
              </button>
            </div>
          )
        })}
      </div>
      {t && key && (
        <>
          <div className="files-toolbar">
            <button className="icon-btn sm" aria-label="Go back" title="Back (Alt+Left)" disabled={!canBack} onClick={() => navigateFiles(sk, -1)}>
              <ArrowLeft size={13} />
            </button>
            <button className="icon-btn sm" aria-label="Go forward" title="Forward (Alt+Right)" disabled={!canForward} onClick={() => navigateFiles(sk, 1)}>
              <ArrowRight size={13} />
            </button>
            <Crumb path={relPath(root, t.path)} full={t.path} />
            {g && (
              <span className="files-git-chip xs" data-git={g}>
                {GIT_LABEL[g]}
              </span>
            )}
            {t.dirty && (
              <span className="xs files-unsaved" aria-label="Unsaved changes">
                Unsaved
              </span>
            )}
            <span className="spacer" />
            {previewable && (
              <button
                className={`icon-btn sm ${t.mode === 'preview' ? 'active' : ''}`}
                aria-label={t.mode === 'preview' ? 'Show source' : 'Show preview'}
                aria-pressed={t.mode === 'preview'}
                title={t.mode === 'preview' ? 'Show source' : 'Show preview'}
                onClick={() => patchFile(key, { mode: t.mode === 'preview' ? 'source' : 'preview' })}
              >
                {t.mode === 'preview' ? <Code size={13} /> : <BookOpen size={13} />}
              </button>
            )}
            {t.kind === 'text' && (
              <button
                className={`icon-btn sm ${wrap ? 'active' : ''}`}
                aria-label="Wrap lines"
                aria-pressed={wrap}
                title="Wrap long lines"
                onClick={() => {
                  useFiles.setState({ wrap: !wrap })
                  for (const [sk2, st] of editorStates) editorStates.set(sk2, withEditorOptions(st, { wrap: !wrap }))
                }}
              >
                <TextWrap size={13} />
              </button>
            )}
            {t.kind === 'text' && (
              <button className="icon-btn sm" aria-label="Save" title="Save (Ctrl+S)" disabled={!t.dirty || readOnly} onClick={save}>
                <Save size={13} />
              </button>
            )}
            <button className="icon-btn sm" aria-label="More file actions" title="More" onClick={(e) => setMore(more ? null : e.currentTarget)}>
              <Ellipsis size={14} />
            </button>
          </div>
          {t.diskChanged && (
            <div className="files-banner warn" role="status">
              <span className="grow">This file changed on disk.</span>
              <button className="btn btn-sm" onClick={() => void reloadFromDisk(key)}>
                Reload
              </button>
              <button
                className="btn btn-sm btn-ghost"
                onClick={async () => {
                  const st = await window.odex.fs.stat(t.path)
                  patchFile(key, { diskChanged: false, mtime: st.mtime })
                }}
              >
                Keep mine
              </button>
            </div>
          )}
          {t.deleted && !t.diskChanged && (
            <div className="files-banner danger" role="status">
              <span className="grow">This file was deleted on disk.{t.kind === 'text' ? ' Saving will recreate it.' : ''}</span>
            </div>
          )}
          {t.kind === 'text' && t.notUtf8 && !t.allowEdit && (
            <div className="files-banner warn" role="status">
              <span className="grow">This file is not valid UTF-8; it is read-only to avoid corrupting it.</span>
              <button className="btn btn-sm btn-ghost" onClick={() => patchFile(key, { allowEdit: true })}>
                Edit anyway
              </button>
            </div>
          )}
          <div className="files-body">
            <FileBody
              fileKey={key}
              t={t}
              wrap={wrap}
              readOnly={readOnly}
              onUpdate={onUpdate}
              onSave={save}
              onView={onView}
              annotations={annotations}
              onNavigate={(dir) => navigateFiles(sk, dir)}
              onOpenLink={(p, ln) => {
                const target = isAbs(p) ? normPath(p) : join(parentOf(t.path), p)
                openFileInSession(sk, target, ln)
              }}
            />
          </div>
          {t.kind === 'text' && t.mode === 'source' && <StatusLine t={t} />}
        </>
      )}
      {tabMenu && <Menu anchor={{ x: tabMenu.x, y: tabMenu.y }} items={tabItems(tabMenu.key)} onClose={() => setTabMenu(null)} minWidth={200} />}
      {more && <Menu anchor={more} items={moreItems} onClose={() => setMore(null)} align="right" minWidth={220} />}
    </section>
  )
}

/** Root-relative path: the folder part truncates first so the file name stays visible. */
function Crumb({ path, full }: { path: string; full: string }) {
  const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf(SEP))
  return (
    <span className="files-crumb mono" title={full}>
      {i >= 0 && <span className="files-crumb-dir">{path.slice(0, i + 1)}</span>}
      <span className="files-crumb-name">{path.slice(i + 1)}</span>
    </span>
  )
}

function StatusLine({ t }: { t: FileTab }) {
  const c = useCursor()
  return (
    <div className="files-status xs" aria-label="Editor status">
      <span>
        Ln {c.line}, Col {c.col}
        {c.sel ? ` (${c.sel} selected)` : ''}
      </span>
      <span className="spacer" />
      <span>{languageName(t.path)}</span>
      <span>{t.eol === '\r\n' ? 'CRLF' : 'LF'}</span>
      <span>{t.bom ? 'UTF-8 BOM' : 'UTF-8'}</span>
    </div>
  )
}

function FileBody(props: {
  fileKey: string
  t: FileTab
  wrap: boolean
  readOnly: boolean
  onUpdate: (u: ViewUpdate) => void
  onSave: () => void
  onView: (v: EditorView | null) => void
  onOpenLink: (p: string, line?: number) => void
  annotations?: LineAnnotation[]
  onNavigate?: (dir: -1 | 1) => void
}) {
  const { fileKey: key, t } = props
  const getState = useCallback(() => editorStates.get(key) ?? createEditorState('', t.path), [key, t.path])
  const onRevealed = useCallback(() => patchFile(key, { reveal: null }), [key])

  if (t.kind === 'idle' || t.kind === 'loading')
    return (
      <div className="empty">
        <span className="spinner" />
      </div>
    )
  if (t.kind === 'missing')
    return (
      <div className="empty files-guard">
        <FileIcon size={22} />
        <div>This file does not exist.</div>
        <div className="xs subtle mono selectable">{t.path}</div>
      </div>
    )
  if (t.kind === 'error')
    return (
      <div className="empty files-guard">
        <FileIcon size={22} />
        <div>Could not open this file.</div>
        <div className="xs subtle selectable">{t.error}</div>
        <button className="btn btn-sm" onClick={() => (patchFile(key, { kind: 'idle' }), void loadFile(key))}>
          Retry
        </button>
      </div>
    )
  if (t.kind === 'binary' || t.kind === 'tooLarge')
    return (
      <div className="empty files-guard">
        <FileIcon size={22} />
        <div>{t.kind === 'binary' ? 'Binary file' : 'This file is too large to open here'}</div>
        <div className="xs subtle">{formatSize(t.size)}</div>
        <div className="row" style={{ gap: 6 }}>
          <button className="btn btn-sm" onClick={() => void window.odex.shell.openPath(t.path)}>
            <ExternalLink size={12} /> Open externally
          </button>
          <button className="btn btn-sm btn-ghost" onClick={() => void window.odex.shell.showItem(t.path)}>
            <FolderOpen size={12} /> {revealLabel}
          </button>
          {t.kind === 'tooLarge' && (t.size ?? 0) <= 50 * 1024 * 1024 && (
            <button className="btn btn-sm btn-ghost" onClick={() => (patchFile(key, { kind: 'idle' }), void loadFile(key, 50 * 1024 * 1024))}>
              Open anyway
            </button>
          )}
        </div>
      </div>
    )
  if (t.kind === 'image') return <ImagePreview src={t.dataUrl!} size={t.size} name={basename(t.path)} path={t.path} />
  if (t.kind === 'pdf') return <PdfPreview dataUrl={t.dataUrl!} />
  if (t.mode === 'preview') {
    const doc = editorStates.get(key)?.doc.toString() ?? ''
    if (extOf(t.path) === 'svg') return <ImagePreview src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(doc)}`} size={t.size} name={basename(t.path)} path={t.path} />
    if (isHtmlExt(extOf(t.path))) return <HtmlPreview path={t.path} version={t.mtime} dirty={t.dirty} />
    return (
      <div className="files-md-preview">
        <Markdown text={doc} onOpenFile={props.onOpenLink} />
      </div>
    )
  }
  return (
    <CodeEditor
      docKey={key}
      getState={getState}
      onUpdate={props.onUpdate}
      onSave={props.onSave}
      onView={props.onView}
      reveal={t.reveal}
      onRevealed={onRevealed}
      wrap={props.wrap}
      readOnly={props.readOnly}
      label={`Editor: ${basename(t.path)}`}
      annotations={props.annotations}
      onNavigate={props.onNavigate}
    />
  )
}

/**
 * Live preview of a saved HTML file in a sandboxed iframe (scripts run, no
 * same-origin access to the app). Relative assets load from the file's folder.
 * Reloads when the file is saved or changes on disk.
 */
function HtmlPreview({ path, version, dirty }: { path: string; version?: number; dirty: boolean }) {
  const [url, setUrl] = useState<string | null>(null)
  const [nonce, setNonce] = useState(0)
  useEffect(() => {
    let cancelled = false
    setUrl(null)
    void window.odex.preview
      .url(path)
      .then((u) => !cancelled && setUrl(u))
      .catch(() => !cancelled && setUrl(''))
    return () => {
      cancelled = true
    }
  }, [path])
  if (url === null)
    return (
      <div className="empty">
        <span className="spinner" />
      </div>
    )
  if (!url) return <div className="empty">Preview unavailable.</div>
  return (
    <div className="files-html">
      <div className="files-html-bar xs">
        <span className="grow ellipsis subtle">{dirty ? 'Showing the saved file. Save (Ctrl+S) to refresh the preview.' : 'Live preview · scripts run sandboxed'}</span>
        <button className="icon-btn sm" aria-label="Reload preview" title="Reload preview" onClick={() => setNonce((n) => n + 1)}>
          <RefreshCw size={12} />
        </button>
      </div>
      <iframe className="files-html-frame" title="HTML preview" sandbox="allow-scripts" src={`${url}?v=${Math.round(version ?? 0)}.${nonce}`} />
    </div>
  )
}

/** Lines the agent added or changed in a file during the selected thread (from its file-change diffs). */
function useAgentEdits(path: string | undefined, version: number | undefined): LineAnnotation[] | undefined {
  const turns = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.turns : undefined))
  const cwd = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread.cwd : undefined))
  return useMemo(() => (path && turns && cwd ? agentEditAnnotations(turns, cwd, path) : undefined),
    // `version` re-applies the markers after the document is reloaded from disk
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [turns, cwd, path, version])
}

/** Hunks of a unified diff. */
function parseHunks(diff: string): Array<{ oldStart: number; oldLen: number; lines: string[] }> {
  const out: Array<{ oldStart: number; oldLen: number; lines: string[] }> = []
  let cur: { oldStart: number; oldLen: number; lines: string[] } | null = null
  for (const line of diff.split('\n')) {
    const m = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line)
    if (m) {
      cur = { oldStart: Number(m[1]), oldLen: m[2] === undefined ? 1 : Number(m[2]), lines: [] }
      out.push(cur)
    } else if (cur && (line.startsWith(' ') || line.startsWith('+') || line.startsWith('-'))) {
      cur.lines.push(line)
    }
  }
  return out
}

/** Carry line marks (line → label) through one diff and mark its added lines. */
function applyDiff(marks: Map<number, string>, diff: string, label: string): Map<number, string> {
  const hunks = parseHunks(diff)
  const next = new Map<number, string>()
  // old line → new line for lines outside every hunk
  const shift = (old: number): number | null => {
    let delta = 0
    for (const h of hunks) {
      // a pure insertion (`-n,0`) goes after old line n and covers no old lines
      const lastOld = h.oldLen === 0 ? h.oldStart : h.oldStart + h.oldLen - 1
      if (h.oldLen > 0 && old >= h.oldStart && old <= lastOld) return null // inside a hunk: handled below
      if (old > lastOld) delta += h.lines.filter((l) => l[0] === '+').length - h.lines.filter((l) => l[0] === '-').length
    }
    return old + delta
  }
  for (const [old, lbl] of marks) {
    const n = shift(old)
    if (n !== null) next.set(n, lbl)
  }
  let delta = 0
  for (const h of hunks) {
    let oldLine = h.oldLen === 0 ? h.oldStart + 1 : h.oldStart
    let newLine = oldLine + delta
    for (const l of h.lines) {
      if (l[0] === ' ') {
        const prev = marks.get(oldLine)
        if (prev) next.set(newLine, prev)
        oldLine++
        newLine++
      } else if (l[0] === '-') {
        oldLine++
      } else {
        next.set(newLine, label)
        newLine++
      }
    }
    delta += h.lines.filter((l) => l[0] === '+').length - h.lines.filter((l) => l[0] === '-').length
  }
  return next
}

/** Gutter annotations for the lines the agent edited in `path`, oldest edit first. */
export function agentEditAnnotations(turns: Turn[], cwd: string, path: string): LineAnnotation[] {
  const target = k(path)
  let marks = new Map<number, string>()
  turns.forEach((turn, ti) => {
    for (const item of turn.items) {
      if (item.type !== 'fileChange' || item.status !== 'completed') continue
      for (const c of item.changes) {
        const p = isAbs(c.path) ? c.path : join(cwd, c.path)
        const moved = c.movePath ? (isAbs(c.movePath) ? c.movePath : join(cwd, c.movePath)) : null
        if (k(p) !== target && (!moved || k(moved) !== target)) continue
        if (c.kind === 'delete') {
          marks = new Map()
          continue
        }
        marks = applyDiff(marks, c.diff, `Changed by the agent (turn ${ti + 1})`)
      }
    }
  })
  return [...marks].map(([line, label]) => ({ line, label }))
}

function ImagePreview(props: { src: string; size?: number; name: string; path: string }) {
  const [dims, setDims] = useState<{ w: number; h: number } | null>(null)
  const [actual, setActual] = useState(false)
  return (
    <div className="files-image">
      <div className={`files-image-stage ${actual ? 'actual' : ''}`} onClick={() => setActual(!actual)} title={actual ? 'Fit to panel' : 'Actual size'}>
        <img src={props.src} alt={props.name} onLoad={(e) => setDims({ w: e.currentTarget.naturalWidth, h: e.currentTarget.naturalHeight })} draggable={false} />
      </div>
      <div className="files-image-meta xs subtle">
        <span>
          {dims ? `${dims.w} × ${dims.h}` : ''}
          {props.size !== undefined ? ` · ${formatSize(props.size)}` : ''}
          {` · ${actual ? 'actual size' : 'fit'}`}
        </span>
        <button className="btn btn-sm btn-ghost files-image-save" aria-label={`Save ${props.name} as`} title="Save a copy as…" onClick={() => void saveFileCopy(props.path)}>
          <Download size={12} /> Save as…
        </button>
      </div>
    </div>
  )
}

function PdfPreview({ dataUrl }: { dataUrl: string }) {
  const [url, setUrl] = useState<string | null>(null)
  useEffect(() => {
    let u: string | null = null
    let cancelled = false
    void fetch(dataUrl)
      .then((r) => r.blob())
      .then((b) => {
        if (cancelled) return
        u = URL.createObjectURL(new Blob([b], { type: 'application/pdf' }))
        setUrl(u)
      })
    return () => {
      cancelled = true
      if (u) URL.revokeObjectURL(u)
    }
  }, [dataUrl])
  return url ? <iframe className="files-pdf" src={url} title="PDF preview" /> : <div className="empty"><span className="spinner" /></div>
}
