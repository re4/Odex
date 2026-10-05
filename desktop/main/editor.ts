import { shell } from 'electron'
import { spawn } from 'node:child_process'
import path from 'node:path'
import type { DesktopSettings } from '@shared/desktop'

/** Editors that open `file:line` with `-g` when the command has no `{file}` placeholder. */
const GOTO_EDITORS = /^(code|code-insiders|cursor|windsurf|codium|vscodium)(\.cmd|\.exe)?$/i

/** Split a command line into words, honoring "double" and 'single' quotes (backslashes stay literal for Windows paths). */
export function splitCommand(cmd: string): string[] {
  const out: string[] = []
  let cur = ''
  let quote: '"' | "'" | null = null
  let started = false
  for (const ch of cmd) {
    if (quote) {
      if (ch === quote) quote = null
      else cur += ch
    } else if (ch === '"' || ch === "'") {
      quote = ch
      started = true
    } else if (/\s/.test(ch)) {
      if (started || cur) out.push(cur)
      cur = ''
      started = false
    } else {
      cur += ch
    }
  }
  if (started || cur) out.push(cur)
  return out
}

/**
 * argv for an editor command. `{file}`, `{line}` and `{col}` are replaced in each word (a word
 * stays one argument even when the path has spaces). Without `{file}`, VS Code-style editors
 * get `-g file:line` and others get the file appended.
 */
export function editorArgv(template: string, file: string, line?: number): string[] {
  const words = splitCommand(template.trim())
  if (!words.length) return []
  const ln = String(line && line > 0 ? Math.floor(line) : 1)
  const hasFile = words.some((w) => w.includes('{file}'))
  const out = words.map((w) => w.replaceAll('{file}', file).replaceAll('{line}', ln).replaceAll('{col}', '1'))
  if (!hasFile) {
    if (GOTO_EDITORS.test(path.basename(words[0]))) out.push('-g', `${file}:${ln}`)
    else out.push(file)
  }
  return out
}

/** Quote one argument for a cmd.exe command line. */
export function quoteWin(arg: string): string {
  if (arg === '') return '""'
  if (!/[\s"&|<>^(),;=!]/.test(arg)) return arg
  return `"${arg.replace(/"/g, '""')}"`
}

type Request = (method: string, params: unknown) => Promise<any>

function inside(file: string, dir: string): boolean {
  const norm = (p: string) => {
    const r = path.resolve(p).replace(/[\\/]+$/, '')
    return process.platform === 'win32' ? r.toLowerCase() : r
  }
  const f = norm(file)
  const d = norm(dir)
  return f === d || f.startsWith(d + path.sep)
}

/** The per-project editor override for a file: by project folder, else by the worktree of a thread in that project. */
async function projectEditor(file: string, s: DesktopSettings, request: Request): Promise<string | null> {
  const map = s.projectEditors ?? {}
  const ids = Object.keys(map).filter((k) => map[k]?.trim())
  if (!ids.length) return null
  try {
    const { projects } = (await request('project/list', {})) as { projects: Array<{ id: string; folders: string[] }> }
    let best: { id: string; len: number } | null = null
    for (const p of projects) {
      if (!ids.includes(p.id)) continue
      for (const f of p.folders) if (inside(file, f) && f.length > (best?.len ?? -1)) best = { id: p.id, len: f.length }
    }
    if (best) return map[best.id]
    const { threads } = (await request('thread/list', { limit: 5000 })) as { threads: Array<{ projectId?: string | null; worktree?: { path: string } | null }> }
    const t = threads.find((t) => t.projectId && ids.includes(t.projectId) && t.worktree && inside(file, t.worktree.path))
    return t?.projectId ? map[t.projectId] : null
  } catch {
    return null
  }
}

/** `shell:openInEditor`: open a file (at a line) with the configured editor command. Returns '' or an error message. */
export async function openInEditor(file: string, line: number | undefined, s: DesktopSettings, request: Request): Promise<string> {
  const editor = ((await projectEditor(file, s, request)) ?? s.editor ?? '').trim()
  if (!editor || editor === 'system') return shell.openPath(file)
  const argv = editorArgv(editor, file, line)
  try {
    const child =
      process.platform === 'win32'
        ? // cmd.exe resolves .cmd shims (code.cmd) and PATHEXT; every argument is quoted for it
          spawn(argv.map(quoteWin).join(' '), { shell: true, detached: true, stdio: 'ignore', windowsHide: true })
        : spawn(argv[0], argv.slice(1), { detached: true, stdio: 'ignore' })
    child.on('error', () => {})
    child.unref()
    return ''
  } catch (e) {
    return (e as Error).message
  }
}
