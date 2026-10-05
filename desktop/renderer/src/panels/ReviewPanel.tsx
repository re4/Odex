import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import {
  Bot,
  ChevronDown,
  ChevronRight,
  ChevronUp,
  ClipboardCheck,
  Columns2,
  ExternalLink,
  FileText,
  GitBranch,
  MessageSquare,
  Minus,
  MoreHorizontal,
  Pencil,
  PictureInPicture2,
  Pin,
  Plus,
  RefreshCw,
  Rows3,
  Search,
  Send,
  Trash2,
  Undo2,
  WrapText,
  X,
} from 'lucide-react'
import type { DiffFile, DiffHunk, DiffTarget, GitBranch as Branch, GitCommitInfo, GitStatus, RepoDiff, ReviewComment, ReviewFinding, Turn } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { confirmDialog, sendMessage } from '@/lib/actions'
import { Menu, basename, useMenu, type MenuItem } from '@/components/ui'
import { DiffView, StatusBadge, anchorKey, splitPath, type LineAnchor, type LineSelection } from '@/components/DiffView'
import { openFileInPanel } from '@/views/items'
import { firstChangedLine, friendlyGitError, isAbsolute, isDetachedReviewWindow, joinPath, normPath, relativeTo, samePath, takeReviewRequest, useRepoContext } from '@/panels/gitShared'
import '@/styles/review.css'

type Kind = 'uncommitted' | 'unstaged' | 'staged' | 'lastTurn' | 'base' | 'commit'
interface Sel {
  kind: Kind
  base?: string
  sha?: string
}
interface Prefs {
  mode: 'unified' | 'split'
  wrap: boolean
  ignoreWs: boolean
  showFiles: boolean
}

const KIND_LABEL: Record<Kind, string> = {
  uncommitted: 'Uncommitted changes',
  unstaged: 'Unstaged',
  staged: 'Staged',
  lastTurn: 'Last turn',
  base: 'Branch vs base',
  commit: 'Commit',
}

const EMPTY_TEXT: Record<Kind, string> = {
  uncommitted: 'No uncommitted changes.',
  unstaged: 'No unstaged changes.',
  staged: 'Nothing is staged.',
  lastTurn: 'The last turn made no changes.',
  base: 'No differences from the base branch.',
  commit: 'This commit has no textual changes.',
}

const EMPTY_COMMENTS: ReviewComment[] = []
const targetMemory = new Map<string, Sel>()

function loadPrefs(): Prefs {
  const d: Prefs = { mode: 'unified', wrap: false, ignoreWs: false, showFiles: true }
  try {
    return { ...d, ...(JSON.parse(localStorage.getItem('odex.review') || '{}') as Partial<Prefs>) }
  } catch {
    return d
  }
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

function fileKeyOf(repoRoot: string, f: DiffFile): string {
  return `${normPath(repoRoot)}::${f.path}`
}

function repoOfKey(key: string): string {
  return key.slice(0, key.indexOf('::'))
}

function opPaths(f: DiffFile): string[] {
  return f.oldPath && f.oldPath !== f.path ? [f.path, f.oldPath] : [f.path]
}

/** Text of the diff lines in [start, end] on one side, for the comment snippet. */
function snippetFor(f: DiffFile, side: 'old' | 'new', start: number, end: number): string {
  const out: string[] = []
  for (const h of f.hunks)
    for (const l of h.lines) {
      const n = side === 'old' ? (l.kind === 'add' ? null : l.oldNo) : l.kind === 'del' ? null : l.newNo
      if (n != null && n >= start && n <= end) out.push(`${l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '}${l.text.replace(/\r$/, '')}`)
    }
  return out.slice(0, 40).join('\n')
}

function latestReview(turns: Turn[] | undefined): { id: string; summary: string; findings: ReviewFinding[]; overallCorrectness?: string | null } | null {
  if (!turns) return null
  for (let t = turns.length - 1; t >= 0; t--) {
    const items = turns[t].items
    for (let i = items.length - 1; i >= 0; i--) {
      const it = items[i]
      if (it.type === 'review') return it
    }
  }
  return null
}

interface Composer {
  fileKey: string
  path: string
  file: DiffFile
  side: 'old' | 'new'
  anchor: number
  start: number
  end: number
}

export function ReviewPanel() {
  const ctx = useRepoContext()
  const { threadId, thread, root } = ctx
  const cwdsKey = ctx.cwds.join('\n')
  const ts = useApp((s) => (threadId ? s.threads[threadId] : undefined))
  const patchThread = useApp((s) => s.patchThread)
  const stats = ts?.diffStats ?? thread?.diffStats
  const lastTurn = ts?.turns[ts.turns.length - 1]
  const refreshKey = `${stats?.filesChanged}:${stats?.additions}:${stats?.deletions}:${lastTurn?.id}:${lastTurn?.status}`
  const pending = ts?.pendingComments ?? EMPTY_COMMENTS
  const reviewing = lastTurn?.mode === 'review' && lastTurn.status === 'inProgress'
  const review = useMemo(() => latestReview(ts?.turns), [ts?.turns])

  const memKey = threadId ?? root ?? ''
  const [sel, setSelState] = useState<Sel>(() => targetMemory.get(memKey) ?? { kind: 'uncommitted' })
  const [prevKey, setPrevKey] = useState(memKey)
  if (prevKey !== memKey) {
    setPrevKey(memKey)
    setSelState(targetMemory.get(memKey) ?? { kind: 'uncommitted' })
  }
  const setSel = useCallback(
    (s: Sel) => {
      targetMemory.set(memKey, s)
      setSelState(s)
    },
    [memKey],
  )

  const [prefs, setPrefsState] = useState<Prefs>(loadPrefs)
  const setPrefs = (p: Partial<Prefs>) => {
    const next = { ...prefs, ...p }
    setPrefsState(next)
    try {
      localStorage.setItem('odex.review', JSON.stringify(next))
    } catch {
      /* storage unavailable */
    }
  }

  const [repos, setRepos] = useState<RepoDiff[] | null>(null)
  const [statusByRepo, setStatusByRepo] = useState<Record<string, GitStatus>>({})
  const [isRepo, setIsRepo] = useState<boolean | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({})
  const [repoFilter, setRepoFilter] = useState<string | null>(null)
  const [composer, setComposer] = useState<Composer | null>(null)
  // the draft lives outside React state so typing doesn't re-render the diff
  const draftRef = useRef('')
  const [search, setSearch] = useState<{ open: boolean; q: string }>({ open: false, q: '' })
  const [branches, setBranches] = useState<Branch[]>([])
  const [commits, setCommits] = useState<GitCommitInfo[]>([])
  const [busy, setBusy] = useState(false)
  const [findingsOpen, setFindingsOpen] = useState(true)
  const [dismissedReview, setDismissedReview] = useState<string | null>(null)
  const [showPending, setShowPending] = useState(false)
  const [scrollTo, setScrollTo] = useState<string | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)
  const matchIdx = useRef(-1)
  const [matchInfo, setMatchInfo] = useState('')
  const seq = useRef(0)
  const [menuAnchor, openMenu, closeMenu] = useMenu()

  // ---------------------------------------------------------------- target
  const defaultBase = useMemo(() => {
    const local = branches.filter((b) => !b.remote && !b.current).map((b) => b.name)
    const wanted = [thread?.worktree?.baseBranch, 'main', 'master', 'develop'].filter(Boolean) as string[]
    return wanted.find((w) => local.includes(w)) ?? local[0] ?? branches.find((b) => b.remote && !b.name.endsWith('/HEAD'))?.name
  }, [branches, thread?.worktree?.baseBranch])

  const target: DiffTarget | null = useMemo(() => {
    switch (sel.kind) {
      case 'lastTurn':
        return threadId ? { type: 'lastTurn', threadId } : null
      case 'base': {
        const b = sel.base ?? defaultBase
        return b ? { type: 'base', branch: b } : null
      }
      case 'commit': {
        const s = sel.sha ?? commits[0]?.sha
        return s ? { type: 'commit', sha: s } : null
      }
      default:
        return { type: sel.kind }
    }
  }, [sel, threadId, defaultBase, commits])
  const targetKey = JSON.stringify(target)

  // ---------------------------------------------------------------- loading
  const load = useCallback(async () => {
    const cwds = cwdsKey ? cwdsKey.split('\n') : []
    if (!root || !cwds.length) return
    const my = ++seq.current
    setLoading(true)
    try {
      const sts = await Promise.all(cwds.map((c) => call('git/status', { cwd: c }).catch(() => null)))
      if (my !== seq.current) return
      const byRepo: Record<string, GitStatus> = {}
      for (const s of sts) if (s?.isRepo && s.repoRoot) byRepo[normPath(s.repoRoot)] = s
      setStatusByRepo(byRepo)
      const anyRepo = sts.some((s) => s?.isRepo)
      setIsRepo(anyRepo)
      if (!anyRepo) {
        setRepos([])
        setError(null)
        return
      }
      if (!target) {
        setRepos(null)
        return
      }
      const r = await call('git/diff', { cwds, target, ignoreWhitespace: prefs.ignoreWs })
      if (my !== seq.current) return
      setRepos(r.repos)
      setError(null)
    } catch (e) {
      if (my === seq.current) setError(friendlyGitError(errMsg(e)))
    } finally {
      if (my === seq.current) setLoading(false)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cwdsKey, root, targetKey, prefs.ignoreWs])

  useEffect(() => {
    void load()
  }, [load, refreshKey])

  // branches + commits for the pickers
  const loadRefs = useCallback(async () => {
    if (!root) return
    const [b, l] = await Promise.all([call('git/branches', { cwd: root }).catch(() => null), call('git/log', { cwd: root, limit: 40 }).catch(() => null)])
    setBranches(b?.branches ?? [])
    setCommits(l?.commits ?? [])
  }, [root])
  useEffect(() => {
    void loadRefs()
  }, [loadRefs, lastTurn?.id])

  // refresh when the window regains focus (edits made outside Odex)
  useEffect(() => {
    let last = 0
    const onFocus = () => {
      if (Date.now() - last < 2000) return
      last = Date.now()
      void load()
    }
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
  }, [load])

  // requests from the git panel ("show this file's diff")
  const applyRequest = useCallback(
    (r: { target: DiffTarget; path?: string } | null) => {
      if (!r) return
      const t = r.target
      if (t.type === 'base') setSel({ kind: 'base', base: t.branch })
      else if (t.type === 'commit') setSel({ kind: 'commit', sha: t.sha })
      else setSel({ kind: t.type })
      if (r.path) setScrollTo(r.path)
    },
    [setSel],
  )
  useEffect(() => {
    applyRequest(takeReviewRequest())
    const h = () => applyRequest(takeReviewRequest())
    window.addEventListener('odex:review-target', h)
    return () => window.removeEventListener('odex:review-target', h)
  }, [applyRequest])

  // ---------------------------------------------------------------- derived
  const shownRepos = useMemo(() => (repos ?? []).filter((r) => !repoFilter || normPath(r.repoRoot) === repoFilter), [repos, repoFilter])
  const files = useMemo(() => shownRepos.flatMap((r) => r.files.map((f) => ({ repo: r, file: f, key: fileKeyOf(r.repoRoot, f) }))), [shownRepos])
  const byKey = useMemo(() => new Map(files.map((x) => [x.key, x])), [files])
  const keyByFile = useMemo(() => new Map(files.map((x) => [x.file, x.key])), [files])
  const totals = useMemo(() => files.reduce((a, x) => ({ add: a.add + x.file.additions, del: a.del + x.file.deletions }), { add: 0, del: 0 }), [files])
  const multiRepo = (repos?.length ?? 0) > 1

  useEffect(() => {
    if (!scrollTo || !files.length) return
    const hit = files.find((x) => x.file.path === scrollTo || samePath(joinPath(x.repo.repoRoot, x.file.path), scrollTo))
    setScrollTo(null)
    if (hit) {
      setCollapsed((c) => ({ ...c, [hit.key]: false }))
      requestAnimationFrame(() => scrollRef.current?.querySelector(`[data-file="${CSS.escape(hit.key)}"]`)?.scrollIntoView({ block: 'start' }))
    }
  }, [scrollTo, files])

  /** Comment path: relative to the thread folder when inside it, else absolute. */
  const commentPath = (repoRoot: string, f: DiffFile) => {
    const abs = joinPath(repoRoot, f.path)
    return (root && relativeTo(root, abs)) || abs
  }
  const absOfComment = (p: string) => normPath(isAbsolute(p) ? p : joinPath(root ?? '', p)).toLowerCase()

  // ---------------------------------------------------------------- comments
  const onLineClick = (f: DiffFile, key: string, a: LineAnchor, e: React.MouseEvent) => {
    if (e.shiftKey && composer && composer.fileKey === key && composer.side === a.side) {
      setComposer({ ...composer, start: Math.min(composer.anchor, a.line), end: Math.max(composer.anchor, a.line) })
      return
    }
    const x = byKey.get(key)
    if (!x) return
    if (!composer) draftRef.current = ''
    setComposer({ fileKey: key, path: commentPath(x.repo.repoRoot, f), file: f, side: a.side, anchor: a.line, start: a.line, end: a.line })
  }

  const setPending = (list: ReviewComment[]) => threadId && patchThread(threadId, { pendingComments: list })

  const addComment = (body: string) => {
    if (!composer || !body.trim() || !threadId) return
    const c: ReviewComment = {
      path: composer.path,
      line: composer.start,
      endLine: composer.end !== composer.start ? composer.end : null,
      side: composer.side,
      body: body.trim(),
      snippet: snippetFor(composer.file, composer.side, composer.start, composer.end) || null,
    }
    setPending([...pending, c])
    setComposer(null)
    draftRef.current = ''
  }

  const sendComments = async () => {
    if (!threadId || !pending.length) return
    const comments = pending
    setPending([])
    setShowPending(false)
    await sendMessage(threadId, [{ type: 'reviewComments', comments }])
  }

  const selection: LineSelection | null = composer ? { fileKey: composer.fileKey, side: composer.side, start: composer.start, end: composer.end } : null

  const annotations = useMemo(() => {
    const out: Record<string, Map<string, ReactNode[]>> = {}
    const put = (key: string, a: string, node: ReactNode) => {
      const m = (out[key] ??= new Map())
      m.set(a, [...(m.get(a) ?? []), node])
    }
    const keyByAbs = new Map(files.map((x) => [normPath(joinPath(x.repo.repoRoot, x.file.path)).toLowerCase(), x]))
    pending.forEach((c, i) => {
      const x = keyByAbs.get(absOfComment(c.path))
      if (!x || c.line == null) return
      const side = c.side === 'old' ? 'old' : 'new'
      put(x.key, anchorKey(side, c.endLine ?? c.line), <PendingCard key={`c${i}`} comment={c} onDelete={() => setPending(pending.filter((_, k) => k !== i))} onEdit={(body) => setPending(pending.map((p, k) => (k === i ? { ...p, body } : p)))} />)
    })
    if (review && review.id !== dismissedReview) {
      review.findings.forEach((f, i) => {
        if (!f.path) return
        const cands = [normPath(isAbsolute(f.path) ? f.path : joinPath(root ?? '', f.path)).toLowerCase()]
        let x = cands.map((c) => keyByAbs.get(c)).find(Boolean)
        if (!x) x = files.find((y) => normPath(y.file.path).toLowerCase() === normPath(f.path!).replace(/^\.\//, '').toLowerCase())
        if (!x) return
        const present = new Set<number>()
        for (const h of x.file.hunks) for (const l of h.lines) if (l.kind !== 'del' && l.newNo != null) present.add(l.newNo)
        const line = [f.lineEnd, f.lineStart].find((n) => n != null && present.has(n))
        if (line != null) put(x.key, anchorKey('new', line), <FindingCard key={`f${i}`} finding={f} />)
      })
    }
    if (composer) {
      put(
        composer.fileKey,
        anchorKey(composer.side, composer.end),
        <CommentComposer
          key="composer"
          label={`Comment on ${composer.side === 'old' ? 'old ' : ''}line${composer.end !== composer.start ? `s ${composer.start}–${composer.end}` : ` ${composer.start} · Shift+click another line to select a range`}`}
          draftRef={draftRef}
          onSubmit={addComment}
          onCancel={() => setComposer(null)}
        />,
      )
    }
    const res: Record<string, Map<string, ReactNode>> = {}
    for (const [k, m] of Object.entries(out)) res[k] = new Map([...m].map(([a, nodes]) => [a, <>{nodes}</>]))
    return res
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [files, pending, review, dismissedReview, composer, root])

  // ---------------------------------------------------------------- git ops
  const run = async (fn: () => Promise<unknown>, done?: string) => {
    setBusy(true)
    try {
      await fn()
      if (done) toast(done, 'success')
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
    } finally {
      setBusy(false)
      void load()
    }
  }

  const fileStatus = (repoRoot: string, path: string) => statusByRepo[normPath(repoRoot)]?.files.find((s) => s.path === path)
  const workingView = sel.kind === 'uncommitted' || sel.kind === 'unstaged' || sel.kind === 'staged'

  const revertFiles = async (repoRoot: string, list: DiffFile[]) => {
    const what = list.length === 1 ? list[0].path : `${list.length} files`
    const scope = sel.kind === 'unstaged' ? 'unstaged changes' : 'changes (staged and unstaged)'
    if (!(await confirmDialog('Revert changes', `Discard all ${scope} to ${what}? New files are deleted. This cannot be undone.`, 'Revert', true))) return
    const paths = list.flatMap(opPaths)
    await run(async () => {
      if (sel.kind === 'uncommitted') {
        const staged = list.filter((f) => fileStatus(repoRoot, f.path)?.staged)
        if (staged.length) await call('git/unstage', { cwd: repoRoot, paths: staged.flatMap(opPaths) })
      }
      await call('git/revert', { cwd: repoRoot, paths })
    }, `Reverted ${what}`)
  }

  const fileActions = (f: DiffFile, key: string) => {
    const repoRoot = byKey.get(key)?.repo.repoRoot ?? repoOfKey(key)
    const st = fileStatus(repoRoot, f.path)
    const canStage = sel.kind === 'unstaged' || (sel.kind === 'uncommitted' && (!st || st.unstaged || st.untracked))
    const canUnstage = sel.kind === 'staged' || (sel.kind === 'uncommitted' && !!st?.staged)
    const canRevert = sel.kind === 'unstaged' || sel.kind === 'uncommitted'
    return (
      <>
        {canStage && (
          <button className="icon-btn sm" disabled={busy} title="Stage file" aria-label={`Stage ${f.path}`} onClick={() => void run(() => call('git/stage', { cwd: repoRoot, paths: opPaths(f) }))}>
            <Plus size={13} />
          </button>
        )}
        {canUnstage && (
          <button className="icon-btn sm" disabled={busy} title="Unstage file" aria-label={`Unstage ${f.path}`} onClick={() => void run(() => call('git/unstage', { cwd: repoRoot, paths: opPaths(f) }))}>
            <Minus size={13} />
          </button>
        )}
        {canRevert && (
          <button className="icon-btn sm" disabled={busy} title="Revert file" aria-label={`Revert ${f.path}`} onClick={() => void revertFiles(repoRoot, [f])}>
            <Undo2 size={13} />
          </button>
        )}
        <button
          className="icon-btn sm"
          title="Open file at the first change"
          aria-label={`Open ${f.path}`}
          disabled={f.status === 'deleted'}
          onClick={() => openFileInPanel(joinPath(repoRoot, f.path), firstChangedLine(f))}
        >
          <FileText size={13} />
        </button>
        <button
          className="icon-btn sm"
          title="Open in external editor at the first change"
          aria-label={`Open ${f.path} in external editor`}
          disabled={f.status === 'deleted'}
          onClick={() => void window.odex.shell.openInEditor(joinPath(repoRoot, f.path), firstChangedLine(f))}
        >
          <ExternalLink size={13} />
        </button>
      </>
    )
  }

  const hunkActions = (f: DiffFile, h: DiffHunk, key: string) => {
    if (sel.kind !== 'unstaged' && sel.kind !== 'staged') return null
    const repoRoot = byKey.get(key)?.repo.repoRoot ?? repoOfKey(key)
    const hunk = { path: f.path, hunkIndex: h.index, target: { type: sel.kind }, ignoreWhitespace: prefs.ignoreWs } as const
    if (sel.kind === 'staged') {
      return (
        <button className="btn btn-sm btn-ghost" disabled={busy} onClick={() => void run(() => call('git/unstage', { cwd: repoRoot, paths: [], hunk }))}>
          <Minus size={12} /> Unstage hunk
        </button>
      )
    }
    return (
      <>
        <button className="btn btn-sm btn-ghost" disabled={busy} onClick={() => void run(() => call('git/stage', { cwd: repoRoot, paths: [], hunk }))}>
          <Plus size={12} /> Stage hunk
        </button>
        <button
          className="btn btn-sm btn-ghost"
          disabled={busy}
          onClick={() =>
            void (async () => {
              if (await confirmDialog('Revert hunk', `Discard this change in ${f.path}? This cannot be undone.`, 'Revert', true)) await run(() => call('git/revert', { cwd: repoRoot, paths: [], hunk }))
            })()
          }
        >
          <Undo2 size={12} /> Revert
        </button>
      </>
    )
  }

  const bulk = (op: 'stage' | 'unstage' | 'revert') => {
    const groups = new Map<string, DiffFile[]>()
    for (const x of files) groups.set(x.repo.repoRoot, [...(groups.get(x.repo.repoRoot) ?? []), x.file])
    if (op === 'revert') {
      void (async () => {
        for (const [repoRoot, list] of groups) await revertFiles(repoRoot, list)
      })()
      return
    }
    void run(async () => {
      for (const [repoRoot, list] of groups) await call(op === 'stage' ? 'git/stage' : 'git/unstage', { cwd: repoRoot, paths: list.flatMap(opPaths) })
    })
  }

  const askReview = async () => {
    if (!threadId || !target) return
    try {
      await call('review/start', { threadId, target })
      setDismissedReview(null)
      toast('Review started. Findings arrive in the thread as a review and show inline here.')
    } catch (e) {
      toast(errMsg(e), 'error')
    }
  }

  const initRepo = async () => {
    if (!root) return
    if (threadId) {
      await call('thread/shellCommand', { threadId, command: 'git init' }).catch((e) => toast(errMsg(e), 'error'))
    } else {
      await window.odex.terminals.run({ cwd: root, command: 'git init', title: 'git init' })
    }
    for (const ms of [800, 2000, 4500]) setTimeout(() => void load(), ms)
  }

  // ---------------------------------------------------------------- search
  const openSearch = () => {
    const s = window.getSelection()?.toString().trim() ?? ''
    setSearch({ open: true, q: s && s.length < 80 && !s.includes('\n') ? s : search.q })
    matchIdx.current = -1
    requestAnimationFrame(() => searchRef.current?.select())
  }
  const nextMatch = (dir: 1 | -1) => {
    const root = scrollRef.current
    const starts = Array.from(root?.querySelectorAll<HTMLElement>('.dv-q-start') ?? [])
    if (!root || !starts.length) return
    root.querySelectorAll('.dv-q.current').forEach((e) => e.classList.remove('current'))
    matchIdx.current = (matchIdx.current + dir + starts.length) % starts.length
    const el = starts[matchIdx.current]
    // a match split across syntax tokens has several pieces sharing its dv-qmN class
    const m = Array.from(el.classList).find((c) => /^dv-qm\d+$/.test(c))
    const pieces = m ? Array.from(el.closest('.dv-code')?.getElementsByClassName(m) ?? []) : [el]
    pieces.forEach((p) => p.classList.add('current'))
    setMatchInfo(`${matchIdx.current + 1}/${starts.length}`)
    el.scrollIntoView({ block: 'center' })
  }

  useEffect(() => {
    if (!search.open || !search.q) {
      setMatchInfo('')
      return
    }
    const id = requestAnimationFrame(() => {
      const n = scrollRef.current?.querySelectorAll('.dv-q-start').length ?? 0
      setMatchInfo(n ? `${n} match${n === 1 ? '' : 'es'}` : 'No matches')
    })
    return () => cancelAnimationFrame(id)
  }, [search.open, search.q, repos, collapsed])

  // ---------------------------------------------------------------- render
  if (!root) {
    return (
      <div className="empty">
        <GitBranch size={20} />
        Open a thread or pick a project to review its changes.
      </div>
    )
  }

  const menuItems: MenuItem[] = [
    { label: 'Stage all', icon: <Plus size={13} />, disabled: !workingView || sel.kind === 'staged' || !files.length, onSelect: () => bulk('stage') },
    { label: 'Unstage all', icon: <Minus size={13} />, disabled: !(sel.kind === 'staged' || sel.kind === 'uncommitted') || !files.length, onSelect: () => bulk('unstage') },
    { label: 'Revert all…', icon: <Undo2 size={13} />, danger: true, disabled: !(sel.kind === 'unstaged' || sel.kind === 'uncommitted') || !files.length, onSelect: () => bulk('revert') },
    { separator: true, label: '' },
    { label: 'Expand all files', onSelect: () => setCollapsed({}) },
    { label: 'Collapse all files', onSelect: () => setCollapsed(Object.fromEntries(files.map((x) => [x.key, true]))) },
    { separator: true, label: '' },
    { label: 'Ignore whitespace', checked: prefs.ignoreWs, onSelect: () => setPrefs({ ignoreWs: !prefs.ignoreWs }) },
    { label: 'Show file list', checked: prefs.showFiles, onSelect: () => setPrefs({ showFiles: !prefs.showFiles }) },
  ]

  const showFindings = review && review.id !== dismissedReview && (review.findings.length > 0 || review.summary)
  const notRepo = isRepo === false

  return (
    <div
      className="rv"
      onKeyDown={(e) => {
        if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'f') {
          e.preventDefault()
          e.stopPropagation()
          openSearch()
        }
      }}
    >
      <div className="rv-toolbar">
        <select
          className="select rv-target"
          aria-label="Diff target"
          value={sel.kind}
          onChange={(e) => {
            setComposer(null)
            setSel({ kind: e.target.value as Kind })
          }}
        >
          {(Object.keys(KIND_LABEL) as Kind[]).map((k) => (
            <option key={k} value={k} disabled={k === 'lastTurn' && !threadId}>
              {KIND_LABEL[k]}
            </option>
          ))}
        </select>
        {sel.kind === 'base' && (
          <select className="select rv-sub" aria-label="Base branch" value={sel.base ?? defaultBase ?? ''} onChange={(e) => setSel({ kind: 'base', base: e.target.value })}>
            {!branches.length && <option value="">No branches</option>}
            {branches
              .filter((b) => !b.current && !b.name.endsWith('/HEAD'))
              .map((b) => (
                <option key={b.name} value={b.name}>
                  {b.name}
                </option>
              ))}
          </select>
        )}
        {sel.kind === 'commit' && (
          <select className="select rv-sub" aria-label="Commit" value={sel.sha ?? commits[0]?.sha ?? ''} onChange={(e) => setSel({ kind: 'commit', sha: e.target.value })}>
            {!commits.length && <option value="">No commits</option>}
            {commits.map((c) => (
              <option key={c.sha} value={c.sha}>
                {c.shortSha} {c.subject.slice(0, 60)}
              </option>
            ))}
          </select>
        )}
        <span className="spacer" />
        <button className={`icon-btn sm ${search.open ? 'active' : ''}`} title="Search in diff (Ctrl+F)" aria-label="Search in diff" onClick={() => (search.open ? setSearch({ open: false, q: '' }) : openSearch())}>
          <Search size={13} />
        </button>
        <button className="icon-btn sm" title={prefs.mode === 'unified' ? 'Split view' : 'Unified view'} aria-label={prefs.mode === 'unified' ? 'Switch to split view' : 'Switch to unified view'} onClick={() => setPrefs({ mode: prefs.mode === 'unified' ? 'split' : 'unified' })}>
          {prefs.mode === 'unified' ? <Columns2 size={13} /> : <Rows3 size={13} />}
        </button>
        <button className={`icon-btn sm ${prefs.wrap ? 'active' : ''}`} title="Wrap long lines" aria-label="Wrap lines" aria-pressed={prefs.wrap} onClick={() => setPrefs({ wrap: !prefs.wrap })}>
          <WrapText size={13} />
        </button>
        <button className="icon-btn sm" title="Refresh" aria-label="Refresh diff" onClick={() => (void load(), void loadRefs())}>
          {loading ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <RefreshCw size={13} />}
        </button>
        {threadId && !isDetachedReviewWindow() && (
          <button className="icon-btn sm" title="Open review in a separate window" aria-label="Pop out review" onClick={() => void window.odex.win.newWindow(threadId, 'review')}>
            <PictureInPicture2 size={13} />
          </button>
        )}
        <button className="icon-btn sm" title="More" aria-label="More review actions" onClick={openMenu}>
          <MoreHorizontal size={13} />
        </button>
        {menuAnchor && <Menu anchor={menuAnchor} items={menuItems} onClose={closeMenu} align="right" />}
      </div>

      {search.open && (
        <div className="rv-search">
          <Search size={12} className="subtle" />
          <input
            ref={searchRef}
            className="input"
            placeholder="Find in diff"
            aria-label="Find in diff"
            value={search.q}
            autoFocus
            onChange={(e) => {
              matchIdx.current = -1
              setSearch({ open: true, q: e.target.value })
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter') nextMatch(e.shiftKey ? -1 : 1)
              if (e.key === 'Escape') {
                e.stopPropagation()
                setSearch({ open: false, q: '' })
              }
            }}
          />
          <span className="xs subtle" style={{ flex: 'none' }} aria-live="polite">
            {matchInfo}
          </span>
          <button className="icon-btn sm" aria-label="Previous match" title="Previous match (Shift+Enter)" onClick={() => nextMatch(-1)}>
            <ChevronUp size={12} />
          </button>
          <button className="icon-btn sm" aria-label="Next match" title="Next match (Enter)" onClick={() => nextMatch(1)}>
            <ChevronDown size={12} />
          </button>
          <button className="icon-btn sm" aria-label="Close search" onClick={() => setSearch({ open: false, q: '' })}>
            <X size={12} />
          </button>
        </div>
      )}

      {!notRepo && (
        <div className="rv-summary">
          <span className="small">
            <b>{files.length}</b> {files.length === 1 ? 'file' : 'files'} <span className="text-add">+{totals.add}</span> <span className="text-del">-{totals.del}</span>
          </span>
          {multiRepo && (
            <select className="select rv-sub" aria-label="Repository" value={repoFilter ?? ''} onChange={(e) => setRepoFilter(e.target.value || null)}>
              <option value="">All repos</option>
              {(repos ?? []).map((r) => (
                <option key={r.repoRoot} value={normPath(r.repoRoot)}>
                  {basename(r.repoRoot)}
                </option>
              ))}
            </select>
          )}
          <span className="spacer" />
          {threadId && (
            <button className="btn btn-sm" disabled={!target || reviewing || !files.length} onClick={() => void askReview()} title="Run a review turn with the reviewer model">
              {reviewing ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Bot size={13} />}
              {reviewing ? 'Reviewing…' : 'Ask agent to review'}
            </button>
          )}
        </div>
      )}

      <div className="rv-scroll" ref={scrollRef}>
        {notRepo ? (
          <div className="empty">
            <GitBranch size={20} />
            <div>This folder is not a git repository.</div>
            <div className="xs">Review, staging and commits need git.</div>
            <button className="btn btn-sm" onClick={() => void initRepo()}>
              Initialize git repository
            </button>
          </div>
        ) : error ? (
          <div className="rv-error" role="alert">
            <div className="small">{error}</div>
            <button className="btn btn-sm" onClick={() => void load()}>
              Retry
            </button>
          </div>
        ) : repos == null ? (
          <div className="empty">{loading ? <span className="spinner" /> : sel.kind === 'base' ? 'Pick a base branch.' : sel.kind === 'commit' ? 'Pick a commit.' : null}</div>
        ) : (
          <>
            {showFindings && (
              <div className="rv-findings">
                <div className="rv-findings-head" onClick={() => setFindingsOpen(!findingsOpen)}>
                  {findingsOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                  <Bot size={13} />
                  <b className="small">Agent review</b>
                  {review.overallCorrectness && <span className={`badge ${review.overallCorrectness === 'correct' ? 'success' : 'warning'}`}>{review.overallCorrectness}</span>}
                  <span className="xs subtle">{review.findings.length} finding(s)</span>
                  <span className="spacer" />
                  <button
                    className="icon-btn sm"
                    aria-label="Dismiss review findings"
                    onClick={(e) => {
                      e.stopPropagation()
                      setDismissedReview(review.id)
                    }}
                  >
                    <X size={12} />
                  </button>
                </div>
                {findingsOpen && (
                  <div className="rv-findings-body">
                    {review.summary && <div className="small muted selectable">{review.summary}</div>}
                    {review.findings.map((f, i) => (
                      <div key={i} className="rv-finding-row">
                        <span className={`badge ${f.priority === 0 ? 'danger' : f.priority === 1 ? 'warning' : ''}`}>P{f.priority}</span>
                        <span className="small grow ellipsis" title={f.body}>
                          {f.title}
                        </span>
                        {f.path && (
                          <a
                            href="#"
                            className="xs mono ellipsis"
                            onClick={(e) => {
                              e.preventDefault()
                              openFileInPanel(isAbsolute(f.path!) ? f.path! : joinPath(root, f.path!), f.lineStart ?? undefined)
                            }}
                          >
                            {basename(f.path)}
                            {f.lineStart ? `:${f.lineStart}` : ''}
                          </a>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}
            {files.length === 0 ? (
              <div className="empty">
                <GitBranch size={20} />
                {EMPTY_TEXT[sel.kind]}
                {sel.kind === 'uncommitted' && threadId && <span className="xs">Changes the agent makes show up here.</span>}
              </div>
            ) : (
              <>
                {prefs.showFiles && files.length > 1 && (
                  <div className="rv-files" aria-label="Changed files">
                    {files.map((x) => {
                      const [dir, base] = splitPath(x.file.path)
                      return (
                        <button
                          key={x.key}
                          className="rv-file-item"
                          title={x.file.path}
                          onClick={() => {
                            setCollapsed((c) => ({ ...c, [x.key]: false }))
                            requestAnimationFrame(() => scrollRef.current?.querySelector(`[data-file="${CSS.escape(x.key)}"]`)?.scrollIntoView({ block: 'start' }))
                          }}
                        >
                          <StatusBadge status={x.file.status} />
                          <span className="ellipsis grow">
                            {multiRepo && <span className="subtle">{basename(x.repo.repoRoot)}: </span>}
                            {base} <span className="subtle xs">{dir}</span>
                          </span>
                          <span className="xs">
                            <span className="text-add">+{x.file.additions}</span> <span className="text-del">-{x.file.deletions}</span>
                          </span>
                        </button>
                      )
                    })}
                  </div>
                )}
                <DiffView
                  files={files.map((x) => x.file)}
                  fileKey={(f) => keyByFile.get(f) ?? f.path}
                  mode={prefs.mode}
                  wrap={prefs.wrap}
                  collapsed={collapsed}
                  onToggleFile={(k) => setCollapsed((c) => ({ ...c, [k]: !c[k] }))}
                  fileActions={fileActions}
                  hunkActions={hunkActions}
                  onLineClick={threadId ? onLineClick : undefined}
                  selection={selection}
                  annotations={annotations}
                  query={search.open ? search.q : ''}
                  filePrefix={multiRepo ? (f) => <span className="badge">{basename(repoOfKey(keyByFile.get(f) ?? ''))}</span> : undefined}
                />
              </>
            )}
          </>
        )}
      </div>

      {threadId && pending.length > 0 && (
        <div className="rv-sendbar">
          {showPending && (
            <div className="rv-pending-list">
              {pending.map((c, i) => (
                <div key={i} className="row xs" style={{ gap: 6 }}>
                  <MessageSquare size={11} className="subtle" />
                  <span className="mono ellipsis" style={{ maxWidth: 160 }}>
                    {basename(c.path)}:{c.line}
                    {c.endLine ? `-${c.endLine}` : ''}
                  </span>
                  <span className="ellipsis grow">{c.body}</span>
                  <button className="icon-btn sm" aria-label="Delete comment" onClick={() => setPending(pending.filter((_, k) => k !== i))}>
                    <Trash2 size={11} />
                  </button>
                </div>
              ))}
            </div>
          )}
          <div className="row" style={{ gap: 6 }}>
            <button className="btn btn-sm btn-ghost" onClick={() => setShowPending(!showPending)} aria-expanded={showPending}>
              <MessageSquare size={12} /> {pending.length} pending
            </button>
            <span className="spacer" />
            <button
              className="btn btn-sm btn-ghost"
              onClick={() =>
                void (async () => {
                  if (await confirmDialog('Discard comments', `Discard ${pending.length} pending comment(s)?`, 'Discard', true)) setPending([])
                })()
              }
            >
              Discard
            </button>
            <button className="btn btn-sm btn-primary" onClick={() => void sendComments()}>
              <Send size={12} /> Send {pending.length} {pending.length === 1 ? 'comment' : 'comments'} to agent
            </button>
          </div>
        </div>
      )}
      {!threadId && root && !notRepo && <div className="rv-hint xs subtle">Open a thread to comment on lines and ask the agent for a review.</div>}
    </div>
  )
}

export function CommentComposer({ label, draftRef, onSubmit, onCancel, placeholder }: { label: string; draftRef: { current: string }; onSubmit: (body: string) => void; onCancel: () => void; placeholder?: string }) {
  const [text, setText] = useState(draftRef.current)
  const update = (v: string) => {
    draftRef.current = v
    setText(v)
  }
  return (
    <div className="rv-composer" onClick={(e) => e.stopPropagation()}>
      <div className="xs subtle">{label}</div>
      <textarea
        className="textarea"
        autoFocus
        aria-label="Review comment"
        placeholder={placeholder ?? 'Leave a comment for the agent… (Ctrl+Enter to add)'}
        value={text}
        onChange={(e) => update(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
            e.preventDefault()
            onSubmit(text)
          } else if (e.key === 'Escape') {
            e.stopPropagation()
            onCancel()
          }
        }}
      />
      <div className="row" style={{ justifyContent: 'flex-end', gap: 6 }}>
        <button className="btn btn-sm btn-ghost" onClick={onCancel}>
          Cancel
        </button>
        <button className="btn btn-sm btn-primary" disabled={!text.trim()} onClick={() => onSubmit(text)}>
          Add comment
        </button>
      </div>
    </div>
  )
}

export function PendingCard({ comment, onDelete, onEdit }: { comment: ReviewComment; onDelete: () => void; onEdit: (body: string) => void }) {
  const [open, setOpen] = useState(true)
  const [editing, setEditing] = useState(false)
  const [text, setText] = useState(comment.body)
  return (
    <div className="rv-comment" onClick={(e) => e.stopPropagation()}>
      <div className="rv-comment-head" onClick={() => setOpen(!open)}>
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        <MessageSquare size={12} />
        <span className="xs">
          Pending comment{comment.endLine ? ` · lines ${comment.line}–${comment.endLine}` : ` · line ${comment.line}`}
        </span>
        <span className="spacer" />
        {!open && <span className="xs subtle ellipsis" style={{ maxWidth: 180 }}>{comment.body}</span>}
        <button
          className="icon-btn sm"
          aria-label="Edit comment"
          onClick={(e) => {
            e.stopPropagation()
            setOpen(true)
            setEditing(true)
            setText(comment.body)
          }}
        >
          <Pencil size={11} />
        </button>
        <button
          className="icon-btn sm"
          aria-label="Delete comment"
          onClick={(e) => {
            e.stopPropagation()
            onDelete()
          }}
        >
          <Trash2 size={11} />
        </button>
      </div>
      {open &&
        (editing ? (
          <div className="col" style={{ gap: 6, padding: '6px 8px 8px' }}>
            <textarea className="textarea" value={text} onChange={(e) => setText(e.target.value)} aria-label="Edit comment text" autoFocus />
            <div className="row" style={{ justifyContent: 'flex-end', gap: 6 }}>
              <button className="btn btn-sm btn-ghost" onClick={() => setEditing(false)}>
                Cancel
              </button>
              <button
                className="btn btn-sm btn-primary"
                disabled={!text.trim()}
                onClick={() => {
                  onEdit(text.trim())
                  setEditing(false)
                }}
              >
                Save
              </button>
            </div>
          </div>
        ) : (
          <div className="rv-comment-body small selectable">{comment.body}</div>
        ))}
    </div>
  )
}

function FindingCard({ finding }: { finding: ReviewFinding }) {
  const [open, setOpen] = useState(true)
  return (
    <div className="rv-comment finding" onClick={(e) => e.stopPropagation()}>
      <div className="rv-comment-head" onClick={() => setOpen(!open)}>
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        <Bot size={12} />
        <span className={`badge ${finding.priority === 0 ? 'danger' : finding.priority === 1 ? 'warning' : ''}`}>P{finding.priority}</span>
        <span className="xs ellipsis grow" style={{ fontWeight: 600 }}>
          {finding.title}
        </span>
      </div>
      {open && finding.body && <div className="rv-comment-body small selectable">{finding.body}</div>}
    </div>
  )
}

/** The detached review window (`?thread=<id>&panel=review`): only the thread's review panel. */
export function DetachedReview() {
  const thread = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread : undefined))
  const ready = useApp((s) => s.ui.view === 'thread' && !!s.selectedThreadId)
  const [onTop, setOnTop] = useState(false)
  const mac = window.odex.platform === 'darwin'
  const title = thread ? thread.name || thread.preview || 'Thread' : ''
  useEffect(() => {
    document.title = title ? `Review: ${title}` : 'Review'
  }, [title])
  return (
    <>
      <div className={`titlebar ${mac ? 'mac' : ''}`}>
        <ClipboardCheck size={14} className="subtle" aria-hidden />
        <span className="small" style={{ fontWeight: 600 }}>
          Review
        </span>
        <span className="title ellipsis">{title}</span>
        <span className="spacer" />
        <button
          className={`icon-btn ${onTop ? 'active' : ''}`}
          aria-label="Keep window on top"
          aria-pressed={onTop}
          title="Always on top"
          onClick={() => {
            void window.odex.win.alwaysOnTop(!onTop)
            setOnTop(!onTop)
          }}
        >
          <Pin size={14} />
        </button>
      </div>
      <div className="main rv-detached" aria-label="Review window">
        {ready ? (
          <ReviewPanel />
        ) : (
          <div className="empty">
            <span className="spinner" />
          </div>
        )}
      </div>
    </>
  )
}

