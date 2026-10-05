import { memo, useMemo, useState, type ReactNode } from 'react'
import { ChevronDown, ChevronRight, Plus } from 'lucide-react'
import hljs from 'highlight.js/lib/common'
import type { DiffFile, DiffHunk, DiffLine } from '@shared/index'
import '@/styles/review.css'

/** Reusable diff renderer: unified or split, syntax + word-level highlighting, inline annotations. */

export type DiffSide = 'old' | 'new'
export interface LineAnchor {
  side: DiffSide
  line: number
}
export interface LineSelection {
  fileKey: string
  side: DiffSide
  start: number
  end: number
}

export interface DiffViewProps {
  files: DiffFile[]
  mode?: 'unified' | 'split'
  wrap?: boolean
  /** Stable key per file (multi-repo views prefix the repo). Default: the path. */
  fileKey?: (f: DiffFile) => string
  /** Controlled collapse state (key → collapsed). Uncontrolled when omitted. */
  collapsed?: Record<string, boolean>
  onToggleFile?: (key: string) => void
  fileActions?: (f: DiffFile, key: string) => ReactNode
  hunkActions?: (f: DiffFile, h: DiffHunk, key: string) => ReactNode
  /** Gutter click (comment). Omit to make gutters inert. */
  onLineClick?: (f: DiffFile, key: string, anchor: LineAnchor, e: React.MouseEvent) => void
  selection?: LineSelection | null
  /** fileKey → `${side}:${line}` → node rendered under that line. */
  annotations?: Record<string, Map<string, ReactNode>>
  /** Highlight matches of this text (case-insensitive). */
  query?: string
  /** Lines shown per file before "Show more". */
  initialLines?: number
  /** Optional title shown left of the path (e.g. repo name). */
  filePrefix?: (f: DiffFile) => ReactNode
}

export function anchorKey(side: DiffSide, line: number): string {
  return `${side}:${line}`
}

/** The comment anchor for a diff line (new side unless the line was deleted). */
export function lineAnchor(l: DiffLine): LineAnchor | null {
  if (l.kind === 'del') return l.oldNo != null ? { side: 'old', line: l.oldNo } : null
  if (l.kind === 'add' || l.kind === 'context') return l.newNo != null ? { side: 'new', line: l.newNo } : null
  return null
}

export function fileLineCount(f: DiffFile): number {
  let n = 0
  for (const h of f.hunks) n += h.lines.length
  return n
}

// ---------------------------------------------------------------- highlighting

interface Seg {
  text: string
  cls: string
}
interface Range {
  start: number
  end: number
}

const EXT_LANG: Record<string, string> = {
  ts: 'typescript', tsx: 'typescript', mts: 'typescript', cts: 'typescript',
  js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript',
  py: 'python', pyi: 'python', rs: 'rust', go: 'go', java: 'java', kt: 'kotlin', kts: 'kotlin',
  c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp', cs: 'csharp',
  rb: 'ruby', php: 'php', swift: 'swift', m: 'objectivec', lua: 'lua', pl: 'perl', r: 'r',
  css: 'css', scss: 'scss', less: 'less', html: 'xml', htm: 'xml', xml: 'xml', svg: 'xml', vue: 'xml',
  json: 'json', jsonc: 'json', md: 'markdown', markdown: 'markdown', yml: 'yaml', yaml: 'yaml',
  toml: 'ini', ini: 'ini', cfg: 'ini', sh: 'bash', bash: 'bash', zsh: 'bash', sql: 'sql',
  graphql: 'graphql', gql: 'graphql', mk: 'makefile', vb: 'vbnet', diff: 'diff', patch: 'diff',
}

export function languageFor(path: string): string | undefined {
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? ''
  if (name === 'makefile') return 'makefile'
  if (name === 'dockerfile') return undefined
  const ext = name.includes('.') ? name.split('.').pop()! : ''
  const lang = EXT_LANG[ext]
  return lang && hljs.getLanguage(lang) ? lang : undefined
}

const ENTITIES: Record<string, string> = { '&amp;': '&', '&lt;': '<', '&gt;': '>', '&quot;': '"', '&#x27;': "'", '&#39;': "'" }
function unescapeHtml(s: string): string {
  return s.replace(/&(amp|lt|gt|quot|#x27|#39);/g, (m) => ENTITIES[m] ?? m)
}

/** hljs HTML → flat (text, class) runs. hljs only emits `<span class>` and escaped text. */
function htmlToSegs(html: string): Seg[] {
  const out: Seg[] = []
  const stack: string[] = []
  const re = /<span class="([^"]*)">|<\/span>|([^<]+)/g
  let m: RegExpExecArray | null
  while ((m = re.exec(html))) {
    if (m[1] !== undefined) stack.push(m[1])
    else if (m[0] === '</span>') stack.pop()
    else if (m[2]) out.push({ text: unescapeHtml(m[2]), cls: stack.join(' ') })
  }
  return out
}

const hlCache = new Map<string, Seg[]>()
function highlight(text: string, lang: string | undefined): Seg[] {
  if (!lang || !text || text.length > 800) return [{ text, cls: '' }]
  const key = `${lang}\u0000${text}`
  const hit = hlCache.get(key)
  if (hit) return hit
  let segs: Seg[]
  try {
    segs = htmlToSegs(hljs.highlight(text, { language: lang, ignoreIllegals: true }).value)
  } catch {
    segs = [{ text, cls: '' }]
  }
  if (hlCache.size > 8000) hlCache.clear()
  hlCache.set(key, segs)
  return segs
}

function tokenize(s: string): string[] {
  return s.match(/\w+|\s+|[^\w\s]/g) ?? []
}

/** Changed character ranges of a paired (deleted, added) line, or null when they barely relate. */
function wordDiff(a: string, b: string): [Range[], Range[]] | null {
  if (a.length > 600 || b.length > 600) return null
  const ta = tokenize(a)
  const tb = tokenize(b)
  const n = ta.length
  const m = tb.length
  if (!n || !m || n * m > 60_000) return null
  // LCS table (suffix form)
  const dp: Uint16Array[] = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1))
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) dp[i][j] = ta[i] === tb[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1])
  const keepA = new Array<boolean>(n).fill(false)
  const keepB = new Array<boolean>(m).fill(false)
  let i = 0
  let j = 0
  let common = 0
  while (i < n && j < m) {
    if (ta[i] === tb[j]) {
      keepA[i] = keepB[j] = true
      common += ta[i].length
      i++
      j++
    } else if (dp[i + 1][j] >= dp[i][j + 1]) i++
    else j++
  }
  if (common < Math.max(a.trim().length, b.trim().length) * 0.35) return null
  const ranges = (toks: string[], keep: boolean[]): Range[] => {
    const out: Range[] = []
    let pos = 0
    toks.forEach((t, k) => {
      if (!keep[k] && t.trim()) {
        const last = out[out.length - 1]
        if (last && last.end === pos) last.end = pos + t.length
        else out.push({ start: pos, end: pos + t.length })
      } else if (!keep[k] && out.length && out[out.length - 1].end === pos) {
        // whitespace between two changed tokens joins them
        const nextChanged = k + 1 < toks.length && !keep[k + 1] && toks[k + 1].trim()
        if (nextChanged) out[out.length - 1].end = pos + t.length
      }
      pos += t.length
    })
    return out
  }
  const ra = ranges(ta, keepA)
  const rb = ranges(tb, keepB)
  if (!ra.length && !rb.length) return null
  return [ra, rb]
}

/** Word-level ranges for every paired del/add line of a hunk (keyed by line index). */
function hunkWordRanges(h: DiffHunk): Map<number, Range[]> {
  const out = new Map<number, Range[]>()
  const ls = h.lines
  let i = 0
  while (i < ls.length) {
    if (ls[i].kind !== 'del') {
      i++
      continue
    }
    const delStart = i
    while (i < ls.length && ls[i].kind === 'del') i++
    const addStart = i
    while (i < ls.length && ls[i].kind === 'add') i++
    const nd = addStart - delStart
    const na = i - addStart
    if (!na || Math.abs(nd - na) > Math.max(nd, na) / 2 + 1) continue
    for (let k = 0; k < Math.min(nd, na); k++) {
      const r = wordDiff(clean(ls[delStart + k].text), clean(ls[addStart + k].text))
      if (r) {
        out.set(delStart + k, r[0])
        out.set(addStart + k, r[1])
      }
    }
  }
  return out
}

function queryRanges(text: string, q: string): Range[] {
  if (!q) return []
  const out: Range[] = []
  const hay = text.toLowerCase()
  const needle = q.toLowerCase()
  let at = hay.indexOf(needle)
  while (at >= 0 && out.length < 50) {
    out.push({ start: at, end: at + needle.length })
    at = hay.indexOf(needle, at + needle.length)
  }
  return out
}

/** Split runs at range boundaries and add `cls` inside ranges (a function gets the range index and whether this is its first piece). */
function splitSegs(segs: Seg[], ranges: Range[], cls: string | ((k: number, first: boolean) => string)): Seg[] {
  if (!ranges.length) return segs
  const out: Seg[] = []
  let pos = 0
  for (const s of segs) {
    const start = pos
    const end = pos + s.text.length
    let cur = start
    for (let k = 0; k < ranges.length; k++) {
      const r = ranges[k]
      if (r.end <= cur || r.start >= end) continue
      const a = Math.max(r.start, cur)
      const b = Math.min(r.end, end)
      if (a > cur) out.push({ text: s.text.slice(cur - start, a - start), cls: s.cls })
      const c = typeof cls === 'string' ? cls : cls(k, a === r.start)
      out.push({ text: s.text.slice(a - start, b - start), cls: s.cls ? `${s.cls} ${c}` : c })
      cur = b
    }
    if (cur < end) out.push({ text: s.text.slice(cur - start), cls: s.cls })
    pos = end
  }
  return out
}

function clean(t: string): string {
  return t.endsWith('\r') ? t.slice(0, -1) : t
}

function Code({ text, lang, words, query }: { text: string; lang?: string; words?: Range[]; query?: string }) {
  const t = clean(text)
  let segs = highlight(t, lang)
  if (words?.length) segs = splitSegs(segs, words, 'dv-word')
  if (query) segs = splitSegs(segs, queryRanges(t, query), (k, first) => `dv-q dv-qm${k}${first ? ' dv-q-start' : ''}`)
  if (!t) return <span className="dv-code"> </span>
  return (
    <span className="dv-code">
      {segs.map((s, i) => (s.cls ? <span key={i} className={s.cls}>{s.text}</span> : s.text))}
    </span>
  )
}

// ---------------------------------------------------------------- rows

interface RowCtx {
  file: DiffFile
  fileKey: string
  lang?: string
  query?: string
  selection?: LineSelection | null
  annotations?: Map<string, ReactNode>
  onLineClick?: DiffViewProps['onLineClick']
}

function isSelected(ctx: RowCtx, a: LineAnchor | null): boolean {
  const s = ctx.selection
  return !!(s && a && s.fileKey === ctx.fileKey && s.side === a.side && a.line >= s.start && a.line <= s.end)
}

function Gutter({ ctx, anchor, children }: { ctx: RowCtx; anchor: LineAnchor | null; children: ReactNode }) {
  if (!ctx.onLineClick || !anchor) return <span className="dv-gutter">{children}</span>
  return (
    <button
      type="button"
      className="dv-gutter clickable"
      aria-label={`Comment on ${anchor.side} line ${anchor.line}`}
      title="Add a comment (Shift+click to select a range)"
      onClick={(e) => ctx.onLineClick!(ctx.file, ctx.fileKey, anchor, e)}
    >
      <Plus size={11} className="dv-plus" aria-hidden />
      {children}
    </button>
  )
}

function Annotation({ node }: { node: ReactNode }) {
  return <div className="dv-annot">{node}</div>
}

function UnifiedLine({ ctx, line, words }: { ctx: RowCtx; line: DiffLine; words?: Range[] }) {
  const a = lineAnchor(line)
  if (line.kind === 'meta') {
    return (
      <div className="dv-row meta">
        <span className="dv-gutter" />
        <span className="dv-code">{line.text}</span>
      </div>
    )
  }
  const sign = line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : ' '
  const note = a ? ctx.annotations?.get(anchorKey(a.side, a.line)) : undefined
  return (
    <>
      <div className={`dv-row ${line.kind}${isSelected(ctx, a) ? ' selected' : ''}`}>
        <Gutter ctx={ctx} anchor={a}>
          <span className="dv-num">{line.oldNo ?? ''}</span>
          <span className="dv-num">{line.newNo ?? ''}</span>
        </Gutter>
        <span className="dv-sign" aria-hidden>
          {sign}
        </span>
        <Code text={line.text} lang={ctx.lang} words={words} query={ctx.query} />
      </div>
      {note && <Annotation node={note} />}
    </>
  )
}

interface SplitPair {
  left?: { line: DiffLine; idx: number }
  right?: { line: DiffLine; idx: number }
}

function pairLines(lines: DiffLine[], from: number, to: number): SplitPair[] {
  const out: SplitPair[] = []
  let i = from
  while (i < to) {
    const l = lines[i]
    if (l.kind === 'context' || l.kind === 'meta') {
      out.push({ left: { line: l, idx: i }, right: { line: l, idx: i } })
      i++
      continue
    }
    const dels: number[] = []
    const adds: number[] = []
    while (i < to && lines[i].kind === 'del') dels.push(i++)
    while (i < to && lines[i].kind === 'add') adds.push(i++)
    for (let k = 0; k < Math.max(dels.length, adds.length); k++) {
      out.push({ left: dels[k] != null ? { line: lines[dels[k]], idx: dels[k] } : undefined, right: adds[k] != null ? { line: lines[adds[k]], idx: adds[k] } : undefined })
    }
  }
  return out
}

function SplitHalf({ ctx, cell, side, words }: { ctx: RowCtx; cell?: { line: DiffLine; idx: number }; side: DiffSide; words?: Map<number, Range[]> }) {
  if (!cell) return <div className="dv-half dv-blank" />
  const l = cell.line
  if (l.kind === 'meta') {
    return (
      <div className="dv-half meta">
        <span className="dv-gutter" />
        <span className="dv-code">{l.text}</span>
      </div>
    )
  }
  const no = side === 'old' ? l.oldNo : l.newNo
  const a: LineAnchor | null = no != null ? { side, line: no } : null
  const kind = l.kind === 'context' ? 'context' : l.kind
  return (
    <div className={`dv-half ${kind}${isSelected(ctx, a) ? ' selected' : ''}`} title={clean(l.text).length > 80 ? clean(l.text) : undefined}>
      <Gutter ctx={ctx} anchor={a}>
        <span className="dv-num">{no ?? ''}</span>
      </Gutter>
      <Code text={l.text} lang={ctx.lang} words={words?.get(cell.idx)} query={ctx.query} />
    </div>
  )
}

const HunkView = memo(function HunkView(props: { ctx: RowCtx; hunk: DiffHunk; mode: 'unified' | 'split'; limit: number; actions?: ReactNode }) {
  const { ctx, hunk, mode, limit, actions } = props
  const words = useMemo(() => hunkWordRanges(hunk), [hunk])
  const end = Math.min(hunk.lines.length, limit)
  return (
    <div className="dv-hunk">
      <div className="dv-hunk-head">
        <span className="dv-hunk-title mono ellipsis" title={hunk.header}>
          {hunk.header}
        </span>
        {actions && <span className="dv-hunk-actions">{actions}</span>}
      </div>
      {mode === 'unified'
        ? hunk.lines.slice(0, end).map((l, i) => <UnifiedLine key={i} ctx={ctx} line={l} words={words.get(i)} />)
        : pairLines(hunk.lines, 0, end).map((p, i) => {
            const la = p.left ? lineAnchor(p.left.line) : null
            const ra = p.right ? lineAnchor(p.right.line) : null
            const notes = [la && la.side === 'old' ? ctx.annotations?.get(anchorKey('old', la.line)) : undefined, ra ? ctx.annotations?.get(anchorKey(ra.side, ra.line)) : undefined].filter(Boolean)
            return (
              <div key={i}>
                <div className="dv-srow">
                  <SplitHalf ctx={ctx} cell={p.left} side="old" words={words} />
                  <SplitHalf ctx={ctx} cell={p.right} side="new" words={words} />
                </div>
                {notes.map((n, k) => (
                  <Annotation key={k} node={n} />
                ))}
              </div>
            )
          })}
    </div>
  )
})

const STATUS_LABEL: Record<string, string> = { added: 'A', deleted: 'D', modified: 'M', renamed: 'R', untracked: 'U', binary: 'B', copied: 'C' }

export function StatusBadge({ status }: { status: string }) {
  return (
    <span className={`dv-status s-${status}`} title={status}>
      {STATUS_LABEL[status] ?? status.slice(0, 1).toUpperCase()}
    </span>
  )
}

export function splitPath(p: string): [string, string] {
  const norm = p.replace(/\\/g, '/')
  const i = norm.lastIndexOf('/')
  return i < 0 ? ['', norm] : [norm.slice(0, i + 1), norm.slice(i + 1)]
}

function FileBlock(props: {
  file: DiffFile
  fileKey: string
  mode: 'unified' | 'split'
  collapsed: boolean
  onToggle: () => void
  initialLines: number
  p: DiffViewProps
}) {
  const { file: f, fileKey, mode, collapsed, onToggle, p } = props
  const [limit, setLimit] = useState(props.initialLines)
  const total = fileLineCount(f)
  const lang = useMemo(() => languageFor(f.path), [f.path])
  const ctx: RowCtx = { file: f, fileKey, lang, query: p.query, selection: p.selection, annotations: p.annotations?.[fileKey], onLineClick: p.onLineClick }
  const [dir, base] = splitPath(f.path)
  const maxNo = f.hunks.reduce((m, h) => Math.max(m, h.oldStart + h.oldLines, h.newStart + h.newLines), 0)
  const numW = `${Math.max(2, String(maxNo).length) + 1}ch`
  let budget = limit
  return (
    <section className={`dv-file ${collapsed ? 'collapsed' : ''}`} data-file={fileKey} style={{ ['--dv-num-w' as string]: numW }}>
      <div className="dv-file-head" onClick={onToggle}>
        <button
          type="button"
          className="dv-toggle"
          aria-expanded={!collapsed}
          aria-label={`${collapsed ? 'Expand' : 'Collapse'} ${f.path}`}
          onClick={(e) => {
            e.stopPropagation()
            onToggle()
          }}
        >
          {collapsed ? <ChevronRight size={13} /> : <ChevronDown size={13} />}
        </button>
        <StatusBadge status={f.status} />
        {p.filePrefix?.(f)}
        <span className="dv-path ellipsis" title={f.oldPath && f.oldPath !== f.path ? `${f.oldPath} → ${f.path}` : f.path}>
          {f.oldPath && f.oldPath !== f.path && <span className="subtle">{f.oldPath} → </span>}
          <span className="subtle">{dir}</span>
          <b>{base}</b>
        </span>
        <span className="dv-counts xs">
          <span className="text-add">+{f.additions}</span> <span className="text-del">-{f.deletions}</span>
        </span>
        {p.fileActions && (
          <span className="dv-file-actions" onClick={(e) => e.stopPropagation()}>
            {p.fileActions(f, fileKey)}
          </span>
        )}
      </div>
      {!collapsed && (
        <div className={`dv-body ${mode} ${p.wrap ? 'wrap' : mode === 'split' ? 'clip' : 'nowrap'}`}>
          <div className="dv-lines">
            {f.binary ? (
              <div className="dv-note">Binary file not shown.</div>
            ) : f.hunks.length === 0 ? (
              <div className="dv-note">{f.status === 'renamed' ? 'File renamed without changes.' : 'No textual changes.'}</div>
            ) : (
              f.hunks.map((h) => {
                if (budget <= 0) return null
                const lim = budget
                budget -= h.lines.length
                return <HunkView key={h.index} ctx={ctx} hunk={h} mode={mode} limit={lim} actions={p.hunkActions?.(f, h, fileKey)} />
              })
            )}
          </div>
          {total > limit && (
            <div className="dv-more">
              <button className="btn btn-sm" onClick={() => setLimit(limit + 1000)}>
                Show {Math.min(1000, total - limit)} more lines
              </button>
              {total - limit > 1000 && (
                <button className="btn btn-sm btn-ghost" onClick={() => setLimit(total)}>
                  Show all ({total - limit} remaining)
                </button>
              )}
            </div>
          )}
        </div>
      )}
    </section>
  )
}

export function DiffView(props: DiffViewProps) {
  const [localCollapsed, setLocalCollapsed] = useState<Record<string, boolean>>({})
  const keyOf = props.fileKey ?? ((f: DiffFile) => f.path)
  const mode = props.mode ?? 'unified'
  const initial = props.initialLines ?? 400
  return (
    <div className="dv">
      {props.files.map((f) => {
        const k = keyOf(f)
        const collapsed = props.collapsed ? !!props.collapsed[k] : (localCollapsed[k] ?? fileLineCount(f) > 2000)
        const toggle = props.onToggleFile ? () => props.onToggleFile!(k) : () => setLocalCollapsed((c) => ({ ...c, [k]: !collapsed }))
        return <FileBlock key={k} file={f} fileKey={k} mode={mode} collapsed={collapsed} onToggle={toggle} initialLines={initial} p={props} />
      })}
    </div>
  )
}
