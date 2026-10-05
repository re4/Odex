import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import {
  ArrowDown,
  ArrowRightLeft,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  CircleDot,
  CircleX,
  ExternalLink,
  FolderOpen,
  GitBranch,
  GitCommitHorizontal,
  GitPullRequest,
  Minus,
  Plus,
  RefreshCw,
  Sparkles,
  Trash2,
  Undo2,
  Upload,
} from 'lucide-react'
import type { DiffTarget, GitBranch as Branch, GitCommitInfo, GitFileStatus, GitStatus, HandoffResult, HandoffStrategy, PullRequest, WorktreeInfo } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { confirmDialog } from '@/lib/actions'
import { Modal, Toggle, basename, relativeTime } from '@/components/ui'
import { Markdown } from '@/components/Markdown'
import { StatusBadge, splitPath } from '@/components/DiffView'
import { openFileInPanel } from '@/views/items'
import { friendlyGitError, isGhSetupError, joinPath, showInReview, useRepoContext } from '@/panels/gitShared'
import '@/styles/review.css'

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

const CODE_STATUS: Record<string, string> = { M: 'modified', A: 'added', D: 'deleted', R: 'renamed', C: 'copied', T: 'modified', U: 'conflict', '?': 'untracked' }

function statusOf(f: GitFileStatus, staged: boolean): string {
  if (f.untracked) return 'untracked'
  if (f.conflicted) return 'conflict'
  const c = (staged ? f.code[0] : f.code[1]) ?? 'M'
  return CODE_STATUS[c] ?? 'modified'
}

/** Git summary: branch, changes, commit, push, history, pull request, worktree hand-off. */
export function GitPanel() {
  const { threadId, thread, root } = useRepoContext()
  const ts = useApp((s) => (threadId ? s.threads[threadId] : undefined))
  const stats = ts?.diffStats ?? thread?.diffStats
  const lastTurn = ts?.turns[ts.turns.length - 1]
  const refreshKey = `${stats?.filesChanged}:${stats?.additions}:${stats?.deletions}:${lastTurn?.id}:${lastTurn?.status}`
  const [status, setStatus] = useState<GitStatus | null>(null)
  const [log, setLog] = useState<GitCommitInfo[]>([])
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [busy, setBusy] = useState(false)
  const [pushOpen, setPushOpen] = useState(false)
  const [handoffOpen, setHandoffOpen] = useState(false)

  const seq = useRef(0)
  const load = useCallback(async () => {
    if (!root) return
    const my = ++seq.current
    setLoading(true)
    try {
      const [st, l] = await Promise.all([call('git/status', { cwd: root }), call('git/log', { cwd: root, limit: 30 }).catch(() => null)])
      if (my !== seq.current) return
      setStatus(st)
      setLog(l?.commits ?? [])
      setError(null)
    } catch (e) {
      if (my === seq.current) setError(friendlyGitError(errMsg(e)))
    } finally {
      if (my === seq.current) setLoading(false)
    }
  }, [root])

  const [shownRoot, setShownRoot] = useState(root)
  if (shownRoot !== root) {
    setShownRoot(root)
    setStatus(null)
    setLog([])
  }
  useEffect(() => {
    void load()
  }, [load, refreshKey])
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

  if (!root) {
    return (
      <div className="empty">
        <GitBranch size={20} />
        Open a thread or pick a project to see its git status.
      </div>
    )
  }
  if (error && !status) {
    return (
      <div className="rv-error" role="alert">
        <div className="small">{error}</div>
        <button className="btn btn-sm" onClick={() => void load()}>
          Retry
        </button>
      </div>
    )
  }
  if (!status) {
    return (
      <div className="empty">
        <span className="spinner" />
      </div>
    )
  }
  if (!status.isRepo) {
    return (
      <div className="empty">
        <GitBranch size={20} />
        <div>This folder is not a git repository.</div>
        <button
          className="btn btn-sm"
          onClick={() =>
            void (async () => {
              if (threadId) await call('thread/shellCommand', { threadId, command: 'git init' }).catch((e) => toast(errMsg(e), 'error'))
              else await window.odex.terminals.run({ cwd: root, command: 'git init', title: 'git init' })
              for (const ms of [800, 2000, 4500]) setTimeout(() => void load(), ms)
            })()
          }
        >
          Initialize git repository
        </button>
      </div>
    )
  }

  const repoRoot = status.repoRoot ?? root
  const staged = status.files.filter((f) => f.staged && !f.untracked)
  const unstaged = status.files.filter((f) => f.unstaged && !f.untracked && !f.conflicted)
  const untracked = status.files.filter((f) => f.untracked)
  const conflicted = status.files.filter((f) => f.conflicted)

  return (
    <div className="gp">
      {/* branch */}
      <div className="gp-card">
        <div className="gp-card-head">
          <GitBranch size={14} />
          <span className="gp-branch ellipsis" title={status.branch ?? undefined}>
            {status.branch ?? `detached at ${status.head?.slice(0, 7) ?? '?'}`}
          </span>
          {status.upstream ? (
            <span className="xs subtle row ellipsis" style={{ gap: 4 }} title={`Tracking ${status.upstream}`}>
              <span className={status.ahead ? 'text-add' : ''} aria-label={`${status.ahead} ahead`}>
                <ArrowUp size={11} />
                {status.ahead}
              </span>
              <span className={status.behind ? 'text-del' : ''} aria-label={`${status.behind} behind`}>
                <ArrowDown size={11} />
                {status.behind}
              </span>
              <span className="ellipsis">{status.upstream}</span>
            </span>
          ) : (
            <span className="badge">no upstream</span>
          )}
          <span className="spacer" />
          <button className="icon-btn sm" title="Refresh" aria-label="Refresh git status" onClick={() => void load()}>
            {loading ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <RefreshCw size={13} />}
          </button>
          <button className="btn btn-sm" disabled={!status.branch || busy} onClick={() => setPushOpen(true)} title={status.remoteUrl ? `Push to ${status.remoteUrl}` : 'No remote configured'}>
            <Upload size={12} /> Push{status.ahead ? ` ${status.ahead}` : ''}
          </button>
        </div>
        {(status.remoteUrl || status.stashCount > 0) && (
          <div className="gp-card-body xs subtle" style={{ paddingTop: 0 }}>
            {status.remoteUrl && <span className="ellipsis" title={status.remoteUrl}>origin: {status.remoteUrl}</span>}
            {status.stashCount > 0 && <span>{status.stashCount} stash entr{status.stashCount === 1 ? 'y' : 'ies'}</span>}
          </div>
        )}
      </div>

      {thread?.worktree && threadId && <WorktreeCard wt={thread.worktree} onHandoff={() => setHandoffOpen(true)} />}

      {/* changes */}
      <div className="gp-card">
        <div className="gp-card-head">
          <b className="small grow">Changes</b>
          <span className="xs subtle">{status.files.length ? `${status.files.length} file(s)` : 'clean'}</span>
        </div>
        {status.files.length === 0 ? (
          <div className="gp-card-body xs subtle">Working tree clean.</div>
        ) : (
          <div>
            {conflicted.length > 0 && (
              <FileGroup title="Conflicts" files={conflicted} staged={false} repoRoot={repoRoot} busy={busy} view="unstaged" actions={() => null} />
            )}
            {staged.length > 0 && (
              <FileGroup
                title="Staged"
                files={staged}
                staged
                repoRoot={repoRoot}
                busy={busy}
                view="staged"
                headActions={
                  <button className="btn btn-sm btn-ghost" disabled={busy} onClick={() => void run(() => call('git/unstage', { cwd: repoRoot, paths: [] }))}>
                    Unstage all
                  </button>
                }
                actions={(f) => (
                  <button className="icon-btn sm" disabled={busy} title="Unstage" aria-label={`Unstage ${f.path}`} onClick={() => void run(() => call('git/unstage', { cwd: repoRoot, paths: pathsOf(f) }))}>
                    <Minus size={13} />
                  </button>
                )}
              />
            )}
            {unstaged.length > 0 && (
              <FileGroup
                title="Unstaged"
                files={unstaged}
                staged={false}
                repoRoot={repoRoot}
                busy={busy}
                view="unstaged"
                headActions={
                  <>
                    <button className="btn btn-sm btn-ghost" disabled={busy} onClick={() => void run(() => call('git/stage', { cwd: repoRoot, paths: unstaged.flatMap(pathsOf) }))}>
                      Stage all
                    </button>
                    <button
                      className="btn btn-sm btn-ghost"
                      disabled={busy}
                      onClick={() =>
                        void (async () => {
                          if (await confirmDialog('Discard changes', `Discard unstaged changes in ${unstaged.length} file(s)? This cannot be undone.`, 'Discard', true))
                            await run(() => call('git/revert', { cwd: repoRoot, paths: unstaged.map((f) => f.path) }))
                        })()
                      }
                    >
                      Discard all
                    </button>
                  </>
                }
                actions={(f) => (
                  <>
                    <button className="icon-btn sm" disabled={busy} title="Stage" aria-label={`Stage ${f.path}`} onClick={() => void run(() => call('git/stage', { cwd: repoRoot, paths: pathsOf(f) }))}>
                      <Plus size={13} />
                    </button>
                    <button
                      className="icon-btn sm"
                      disabled={busy}
                      title="Revert"
                      aria-label={`Revert ${f.path}`}
                      onClick={() =>
                        void (async () => {
                          if (await confirmDialog('Revert file', `Discard unstaged changes to ${f.path}? This cannot be undone.`, 'Revert', true)) await run(() => call('git/revert', { cwd: repoRoot, paths: [f.path] }))
                        })()
                      }
                    >
                      <Undo2 size={13} />
                    </button>
                  </>
                )}
              />
            )}
            {untracked.length > 0 && (
              <FileGroup
                title="Untracked"
                files={untracked}
                staged={false}
                repoRoot={repoRoot}
                busy={busy}
                view="unstaged"
                headActions={
                  <button className="btn btn-sm btn-ghost" disabled={busy} onClick={() => void run(() => call('git/stage', { cwd: repoRoot, paths: untracked.map((f) => f.path) }))}>
                    Stage all
                  </button>
                }
                actions={(f) => (
                  <>
                    <button className="icon-btn sm" disabled={busy} title="Stage" aria-label={`Stage ${f.path}`} onClick={() => void run(() => call('git/stage', { cwd: repoRoot, paths: [f.path] }))}>
                      <Plus size={13} />
                    </button>
                    <button
                      className="icon-btn sm"
                      disabled={busy}
                      title="Delete"
                      aria-label={`Delete ${f.path}`}
                      onClick={() =>
                        void (async () => {
                          if (await confirmDialog('Delete file', `Delete the untracked file ${f.path}? This cannot be undone.`, 'Delete', true)) await run(() => call('git/revert', { cwd: repoRoot, paths: [f.path] }))
                        })()
                      }
                    >
                      <Trash2 size={12} />
                    </button>
                  </>
                )}
              />
            )}
          </div>
        )}
      </div>

      <CommitCard root={repoRoot} threadId={threadId} status={status} lastSubject={log[0]?.subject} onDone={() => void load()} />

      <LogCard log={log} />

      <PrCard root={repoRoot} threadId={threadId} status={status} onPush={() => setPushOpen(true)} />

      {pushOpen && <PushDialog root={repoRoot} status={status} onClose={() => setPushOpen(false)} onDone={() => void load()} />}
      {handoffOpen && threadId && thread?.worktree && <HandoffDialog threadId={threadId} wt={thread.worktree} dirty={status.files.length > 0} onClose={() => setHandoffOpen(false)} onDone={() => void load()} />}
    </div>
  )
}

function pathsOf(f: GitFileStatus): string[] {
  return f.origPath && f.origPath !== f.path ? [f.path, f.origPath] : [f.path]
}

function FileGroup(props: {
  title: string
  files: GitFileStatus[]
  staged: boolean
  repoRoot: string
  busy: boolean
  view: 'staged' | 'unstaged'
  headActions?: ReactNode
  actions: (f: GitFileStatus) => ReactNode
}) {
  const [open, setOpen] = useState(true)
  return (
    <div className="gp-group" role="group" aria-label={`${props.title} files`}>
      <div className="gp-group-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
        <span className="small" style={{ fontWeight: 600 }}>
          {props.title}
        </span>
        <span className="badge">{props.files.length}</span>
        <span className="spacer" />
        <span onClick={(e) => e.stopPropagation()} className="row" style={{ gap: 2 }}>
          {props.headActions}
        </span>
      </div>
      {open &&
        props.files.map((f) => {
          const [dir, base] = splitPath(f.path)
          return (
            <div
              key={f.path}
              className="gp-file"
              title={`${f.path} — show diff`}
              onClick={() => showInReview({ type: props.view } as DiffTarget, f.path)}
              onDoubleClick={() => openFileInPanel(joinPath(props.repoRoot, f.path))}
            >
              <StatusBadge status={statusOf(f, props.staged)} />
              <span className="ellipsis grow">
                {base} <span className="xs subtle">{dir}</span>
              </span>
              <span className="gp-file-actions" onClick={(e) => e.stopPropagation()}>
                {props.actions(f)}
              </span>
            </div>
          )
        })}
    </div>
  )
}

function WorktreeCard({ wt, onHandoff }: { wt: WorktreeInfo; onHandoff: () => void }) {
  return (
    <div className="gp-card">
      <div className="gp-card-head">
        <ArrowRightLeft size={14} />
        <b className="small grow">Worktree</b>
        {wt.setupStatus && wt.setupStatus !== 'ok' && <span className={`badge ${wt.setupStatus.startsWith('failed') ? 'danger' : ''}`}>setup {wt.setupStatus.startsWith('failed') ? 'failed' : wt.setupStatus}</span>}
        <button className="btn btn-sm" onClick={onHandoff} title="Bring this worktree's work into your local checkout">
          Hand off…
        </button>
      </div>
      <div className="gp-card-body xs">
        <div className="gp-kv">
          <span className="subtle">Branch</span>
          <span className="mono ellipsis">
            {wt.branch}
            {wt.baseBranch && <span className="subtle"> from {wt.baseBranch}</span>}
          </span>
          <span />
          <span className="subtle">Path</span>
          <span className="mono ellipsis selectable" title={wt.path}>
            {wt.path}
          </span>
          <button className="icon-btn sm" aria-label="Open worktree folder" title="Open folder" onClick={() => void window.odex.shell.openPath(wt.path)}>
            <FolderOpen size={12} />
          </button>
          <span className="subtle">Local</span>
          <span className="mono ellipsis selectable" title={`Local checkout: ${wt.repoRoot}`}>
            {wt.repoRoot}
          </span>
          <button className="icon-btn sm" aria-label="Open local checkout folder" title="Open folder" onClick={() => void window.odex.shell.openPath(wt.repoRoot)}>
            <FolderOpen size={12} />
          </button>
        </div>
      </div>
    </div>
  )
}

function CommitCard({ root, threadId, status, lastSubject, onDone }: { root: string; threadId: string | null; status: GitStatus; lastSubject?: string; onDone: () => void }) {
  const [message, setMessage] = useState('')
  const [amend, setAmend] = useState(false)
  const [generating, setGenerating] = useState(false)
  const [committing, setCommitting] = useState(false)
  const stagedCount = status.files.filter((f) => f.staged && !f.untracked).length
  const changes = status.files.filter((f) => !f.conflicted).length
  const all = stagedCount === 0
  const canCommit = !!message.trim() && !committing && (amend || changes > 0)

  const generate = async () => {
    setGenerating(true)
    try {
      const r = await call('git/commitMessage', { cwd: root, threadId, stagedOnly: stagedCount > 0 ? true : null })
      setMessage(r.message)
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
    } finally {
      setGenerating(false)
    }
  }

  const commit = async () => {
    if (!canCommit) return
    setCommitting(true)
    try {
      const r = await call('git/commit', { cwd: root, message: message.trim(), all: all && changes > 0, amend })
      toast(`Committed ${r.sha.slice(0, 7)}: ${r.summary}`, 'success')
      setMessage('')
      setAmend(false)
      onDone()
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
    } finally {
      setCommitting(false)
    }
  }

  return (
    <div className="gp-card gp-commit-box">
      <div className="gp-card-head">
        <GitCommitHorizontal size={14} />
        <b className="small grow">Commit</b>
        <button className="btn btn-sm btn-ghost" disabled={generating || (changes === 0 && !amend)} onClick={() => void generate()} title="Write a message from the diff with the utility model">
          {generating ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Sparkles size={12} />} Generate message
        </button>
      </div>
      <div className="gp-card-body">
        <textarea
          className="textarea"
          aria-label="Commit message"
          placeholder={changes ? 'Commit message (Ctrl+Enter to commit)' : 'Nothing to commit'}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
              e.preventDefault()
              void commit()
            }
          }}
        />
        <div className="row" style={{ gap: 8 }}>
          <label className="checkbox xs">
            <input
              type="checkbox"
              checked={amend}
              onChange={(e) => {
                setAmend(e.target.checked)
                if (e.target.checked && !message.trim() && lastSubject) setMessage(lastSubject)
              }}
            />
            Amend last commit
          </label>
          <span className="spacer" />
          <button className="btn btn-primary btn-sm" disabled={!canCommit} onClick={() => void commit()}>
            {committing ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Check size={12} />}
            {amend ? 'Amend' : all && changes > 0 ? 'Commit all' : 'Commit'}
          </button>
        </div>
        {all && changes > 0 && <div className="xs subtle">Nothing is staged, so all changes (including untracked files) will be committed.</div>}
      </div>
    </div>
  )
}

function LogCard({ log }: { log: GitCommitInfo[] }) {
  const [more, setMore] = useState(false)
  const shown = more ? log : log.slice(0, 8)
  return (
    <div className="gp-card">
      <div className="gp-card-head">
        <b className="small grow">Recent commits</b>
      </div>
      {log.length === 0 ? (
        <div className="gp-card-body xs subtle">No commits yet.</div>
      ) : (
        <div style={{ paddingBottom: 6 }}>
          {shown.map((c) => (
            <div key={c.sha} className="gp-log-item" title={`${c.sha}\n${c.author} · ${new Date(c.timestamp).toLocaleString()}\nClick to review this commit`} onClick={() => showInReview({ type: 'commit', sha: c.sha })}>
              <span className="gp-sha">{c.shortSha}</span>
              <span className="ellipsis grow">{c.subject}</span>
              <span className="xs subtle" style={{ flex: 'none' }}>
                {c.author.split(' ')[0]} · {relativeTime(c.timestamp > 1e12 ? c.timestamp : c.timestamp * 1000)}
              </span>
            </div>
          ))}
          {log.length > 8 && (
            <button className="btn btn-sm btn-ghost" style={{ marginLeft: 6 }} onClick={() => setMore(!more)}>
              {more ? 'Show less' : `Show ${log.length - 8} more`}
            </button>
          )}
        </div>
      )}
    </div>
  )
}

function PushDialog({ root, status, onClose, onDone }: { root: string; status: GitStatus; onClose: () => void; onDone: () => void }) {
  const [remote, setRemote] = useState('origin')
  const [setUpstream, setSetUpstream] = useState(!status.upstream)
  const [force, setForce] = useState(false)
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<{ ok: boolean; output: string } | null>(null)
  const push = async () => {
    setBusy(true)
    try {
      const r = await call('git/push', { cwd: root, remote: remote.trim() || null, branch: status.branch ?? null, setUpstream, forceWithLease: force })
      if (r.ok) {
        toast(`Pushed ${status.branch} to ${remote}`, 'success')
        onDone()
        onClose()
      } else setResult(r)
    } catch (e) {
      setResult({ ok: false, output: errMsg(e) })
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      title={`Push ${status.branch ?? ''}`}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className={`btn ${force ? 'btn-danger' : 'btn-primary'}`} disabled={busy || !remote.trim()} onClick={() => void push()}>
            {busy && <span className="spinner" style={{ width: 11, height: 11 }} />}
            {force ? 'Force push' : 'Push'}
          </button>
        </>
      }
    >
      <div className="col" style={{ gap: 10 }}>
        <div className="small muted">
          {status.ahead ? `${status.ahead} commit(s) ahead` : 'No new commits'}
          {status.upstream ? ` of ${status.upstream}.` : '. This branch has no upstream yet.'}
        </div>
        <div className="field">
          <label htmlFor="gp-remote">Remote</label>
          <input id="gp-remote" className="input" value={remote} onChange={(e) => setRemote(e.target.value)} />
        </div>
        <label className="checkbox small">
          <input type="checkbox" checked={setUpstream} onChange={(e) => setSetUpstream(e.target.checked)} />
          Set upstream (track {remote || 'origin'}/{status.branch})
        </label>
        <label className="checkbox small">
          <input type="checkbox" checked={force} onChange={(e) => setForce(e.target.checked)} />
          Force with lease
        </label>
        {force && <div className="gp-note warn">Force-with-lease overwrites the remote branch if nobody else pushed since your last fetch. Use it after rebasing or amending.</div>}
        {result && !result.ok && (
          <>
            <div className="gp-note error">{friendlyGitError(result.output)}</div>
            <pre className="gp-out">{result.output}</pre>
          </>
        )}
      </div>
    </Modal>
  )
}

const STRATEGIES: Array<{ id: HandoffStrategy; label: string; hint: string }> = [
  { id: 'merge', label: 'Merge into a local branch', hint: 'Merges the worktree branch into the target branch of your local checkout (merge commit).' },
  { id: 'squash', label: 'Squash into a local branch', hint: 'Applies all worktree commits as one staged change on the target branch; you commit it.' },
  { id: 'cherryPick', label: 'Cherry-pick commits', hint: 'Replays each worktree commit on top of the target branch.' },
  { id: 'checkout', label: 'Check out the branch locally', hint: 'Your local checkout switches to the worktree branch and the thread continues there.' },
]

function HandoffDialog({ threadId, wt, dirty, onClose, onDone }: { threadId: string; wt: WorktreeInfo; dirty: boolean; onClose: () => void; onDone: () => void }) {
  const [strategy, setStrategy] = useState<HandoffStrategy>('merge')
  const [branches, setBranches] = useState<Branch[]>([])
  const [target, setTarget] = useState(wt.baseBranch ?? '')
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  const [generating, setGenerating] = useState(false)
  const [result, setResult] = useState<HandoffResult | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    void call('git/branches', { cwd: wt.repoRoot })
      .then((r) => {
        const local = r.branches.filter((b) => !b.remote && b.name !== wt.branch)
        setBranches(local)
        setTarget((t) => t || local.find((b) => b.current)?.name || local[0]?.name || '')
      })
      .catch(() => {})
  }, [wt.repoRoot, wt.branch])

  const go = async () => {
    setBusy(true)
    setError(null)
    setResult(null)
    try {
      const r = await call('worktree/handoff', { threadId, strategy, targetBranch: strategy === 'checkout' ? null : target || null, commitMessage: dirty ? message.trim() || null : null })
      if (r.ok) {
        toast(r.message, 'success')
        onDone()
        onClose()
      } else setResult(r)
    } catch (e) {
      setError(friendlyGitError(errMsg(e)))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      title="Hand off worktree"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            {result ? 'Close' : 'Cancel'}
          </button>
          <button className="btn btn-primary" disabled={busy || (dirty && !message.trim()) || (strategy !== 'checkout' && !target)} onClick={() => void go()}>
            {busy && <span className="spinner" style={{ width: 11, height: 11 }} />}
            Hand off
          </button>
        </>
      }
    >
      <div className="col" style={{ gap: 10 }}>
        <div className="small muted">
          Move the work on <span className="mono">{wt.branch}</span> into your local checkout at <span className="mono">{wt.repoRoot}</span>.
        </div>
        <div role="radiogroup" aria-label="Hand-off strategy">
          {STRATEGIES.map((s) => (
            <label key={s.id} className="gp-radio">
              <input type="radio" name="handoff" checked={strategy === s.id} onChange={() => setStrategy(s.id)} />
              <span>
                <div className="small">{s.label}</div>
                <div className="xs subtle">{s.hint}</div>
              </span>
            </label>
          ))}
        </div>
        {strategy !== 'checkout' && (
          <div className="field">
            <label htmlFor="gp-target">Target branch</label>
            <select id="gp-target" className="select" value={target} onChange={(e) => setTarget(e.target.value)}>
              {branches.map((b) => (
                <option key={b.name} value={b.name}>
                  {b.name}
                  {b.current ? ' (checked out)' : ''}
                </option>
              ))}
            </select>
          </div>
        )}
        {dirty && (
          <div className="field">
            <label htmlFor="gp-handoff-msg">The worktree has uncommitted changes. Commit them first with:</label>
            <div className="row" style={{ gap: 6 }}>
              <input id="gp-handoff-msg" className="input" placeholder="Commit message" value={message} onChange={(e) => setMessage(e.target.value)} />
              <button
                className="btn btn-sm"
                disabled={generating}
                onClick={() =>
                  void (async () => {
                    setGenerating(true)
                    try {
                      setMessage((await call('git/commitMessage', { cwd: wt.path, threadId })).message.split('\n')[0])
                    } catch (e) {
                      toast(friendlyGitError(errMsg(e)), 'error')
                    } finally {
                      setGenerating(false)
                    }
                  })()
                }
              >
                {generating ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Sparkles size={12} />}
                Generate
              </button>
            </div>
          </div>
        )}
        {error && <div className="gp-note error">{error}</div>}
        {result && !result.ok && (
          <div className="gp-note error">
            <div>{result.message}</div>
            {result.conflicts.length > 0 && (
              <ul style={{ margin: '6px 0 0', paddingLeft: 18 }}>
                {result.conflicts.map((c) => (
                  <li key={c}>
                    <a href="#" className="mono xs" onClick={(e) => (e.preventDefault(), openFileInPanel(joinPath(wt.repoRoot, c)))}>
                      {c}
                    </a>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>
    </Modal>
  )
}

function CheckIcon({ state }: { state: string }) {
  if (state === 'success') return <Check size={13} color="var(--success)" aria-label="passed" />
  if (state === 'failure') return <CircleX size={13} color="var(--danger)" aria-label="failed" />
  if (state === 'pending') return <CircleDot size={13} color="var(--warning)" aria-label="pending" />
  return <Minus size={13} color="var(--fg-subtle)" aria-label={state} />
}

const PR_STATE_BADGE: Record<string, string> = { open: 'success', draft: '', merged: 'accent', closed: 'danger' }

function PrCard({ root, threadId, status, onPush }: { root: string; threadId: string | null; status: GitStatus; onPush: () => void }) {
  const [loading, setLoading] = useState(false)
  const [pr, setPr] = useState<PullRequest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [checked, setChecked] = useState(false)
  const [draft, setDraft] = useState<{ title: string; body: string; base: string; draft: boolean } | null>(null)
  const [drafting, setDrafting] = useState(false)
  const [creating, setCreating] = useState(false)
  const [comment, setComment] = useState('')
  const [posting, setPosting] = useState(false)
  const [showDetails, setShowDetails] = useState(false)
  const hasRemote = !!status.remoteUrl

  const load = useCallback(async () => {
    if (!hasRemote) return
    setLoading(true)
    try {
      const r = await call('pr/view', { cwd: root })
      setPr(r.pr ?? null)
      setError(r.error ?? null)
    } catch (e) {
      setError(errMsg(e))
    } finally {
      setLoading(false)
      setChecked(true)
    }
  }, [root, hasRemote])

  useEffect(() => {
    setPr(null)
    setError(null)
    setChecked(false)
    setDraft(null)
    void load()
  }, [load, status.branch])

  const startDraft = async () => {
    setDrafting(true)
    try {
      const r = await call('pr/draft', { cwd: root, threadId })
      setDraft({ title: r.title, body: r.body, base: '', draft: false })
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
      setDraft({ title: '', body: '', base: '', draft: false })
    } finally {
      setDrafting(false)
    }
  }

  const create = async () => {
    if (!draft) return
    setCreating(true)
    try {
      const r = await call('pr/create', { cwd: root, title: draft.title.trim(), body: draft.body, base: draft.base.trim() || null, draft: draft.draft })
      toast(`Pull request created${r.number ? ` (#${r.number})` : ''}`, 'success')
      setDraft(null)
      void load()
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
    } finally {
      setCreating(false)
    }
  }

  const post = async () => {
    if (!pr || !comment.trim()) return
    if (!(await confirmDialog('Post comment', `Post this comment to pull request #${pr.number} on GitHub? Everyone with access to the repository will see it.`, 'Post comment'))) return
    setPosting(true)
    try {
      const r = await call('pr/comment', { cwd: root, number: pr.number, comments: [], body: comment.trim(), confirmed: true })
      if (r.ok) {
        toast('Comment posted', 'success')
        setComment('')
        void load()
      } else toast(friendlyGitError(r.output), 'error')
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
    } finally {
      setPosting(false)
    }
  }

  const passed = pr?.checks.filter((c) => c.state === 'success').length ?? 0
  const failed = pr?.checks.filter((c) => c.state === 'failure').length ?? 0
  const pendingChecks = pr?.checks.filter((c) => c.state === 'pending').length ?? 0

  return (
    <div className="gp-card">
      <div className="gp-card-head">
        <GitPullRequest size={14} />
        <b className="small grow">Pull request</b>
        {hasRemote && (
          <button className="icon-btn sm" title="Refresh pull request" aria-label="Refresh pull request" onClick={() => void load()}>
            {loading ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <RefreshCw size={13} />}
          </button>
        )}
      </div>
      <div className="gp-card-body">
        {!hasRemote ? (
          <div className="gp-note">This repository has no remote named origin. Add a GitHub remote to create and view pull requests.</div>
        ) : error ? (
          <>
            <div className={`gp-note ${isGhSetupError(error) ? 'warn' : 'error'}`}>{friendlyGitError(error)}</div>
            {friendlyGitError(error) !== error && (
              <button className="btn btn-sm btn-ghost" style={{ alignSelf: 'flex-start' }} onClick={() => setShowDetails(!showDetails)}>
                {showDetails ? 'Hide details' : 'Details'}
              </button>
            )}
            {showDetails && <pre className="gp-out">{error}</pre>}
          </>
        ) : !checked && loading ? (
          <div className="row xs subtle">
            <span className="spinner" /> Looking for a pull request…
          </div>
        ) : pr ? (
          <>
            <div className="row" style={{ gap: 6, alignItems: 'flex-start' }}>
              <span className={`badge ${PR_STATE_BADGE[pr.state] ?? ''}`}>{pr.state}</span>
              <a
                href="#"
                className="small grow"
                style={{ fontWeight: 600 }}
                onClick={(e) => {
                  e.preventDefault()
                  void window.odex.shell.openExternal(pr.url)
                }}
              >
                #{pr.number} {pr.title} <ExternalLink size={11} />
              </a>
            </div>
            <div className="xs subtle row" style={{ gap: 6 }}>
              <span className="mono">{pr.head}</span> → <span className="mono">{pr.base}</span>
              <span>· {pr.author}</span>
              <span className="text-add">+{pr.additions}</span>
              <span className="text-del">-{pr.deletions}</span>
            </div>
            {pr.checks.length > 0 && (
              <div>
                <div className="xs subtle" style={{ marginBottom: 2 }}>
                  Checks: {passed} passed{failed ? `, ${failed} failed` : ''}
                  {pendingChecks ? `, ${pendingChecks} pending` : ''}
                </div>
                {pr.checks.map((c, i) => (
                  <div key={i} className="gp-check">
                    <CheckIcon state={c.state} />
                    <span className="ellipsis grow">{c.name}</span>
                    {c.url && (
                      <button className="icon-btn sm" aria-label={`Open check ${c.name}`} onClick={() => void window.odex.shell.openExternal(c.url!)}>
                        <ExternalLink size={11} />
                      </button>
                    )}
                  </div>
                ))}
              </div>
            )}
            {pr.body && (
              <details>
                <summary className="xs subtle" style={{ cursor: 'pointer' }}>
                  Description
                </summary>
                <div className="small">
                  <Markdown text={pr.body} />
                </div>
              </details>
            )}
            {pr.reviewComments.length > 0 && (
              <div>
                <div className="xs subtle" style={{ marginBottom: 4 }}>
                  {pr.reviewComments.length} review comment(s)
                </div>
                <div className="gp-timeline">
                  {pr.reviewComments.slice(0, 20).map((c) => (
                    <div key={c.id} className="gp-event">
                      <div className="row xs" style={{ gap: 6 }}>
                        <b>{c.author}</b>
                        <a href="#" className="mono ellipsis" onClick={(e) => (e.preventDefault(), openFileInPanel(joinPath(root, c.path), c.line ?? undefined))}>
                          {basename(c.path)}
                          {c.line ? `:${c.line}` : ''}
                        </a>
                        <span className="spacer" />
                        <span className="subtle">{c.at ? relativeTime(c.at) : ''}</span>
                      </div>
                      <div className="body">{c.body}</div>
                    </div>
                  ))}
                </div>
              </div>
            )}
            {pr.timeline.length > 0 && (
              <div>
                <div className="xs subtle" style={{ marginBottom: 4 }}>
                  Activity
                </div>
                <div className="gp-timeline">
                  {[...pr.timeline]
                    .sort((a, b) => b.at - a.at)
                    .slice(0, 15)
                    .map((ev, i) => (
                      <div key={i} className="gp-event">
                        <div className="row xs" style={{ gap: 6 }}>
                          <b>{ev.author ?? 'someone'}</b>
                          <span className="subtle">{ev.kind}</span>
                          <span className="spacer" />
                          <span className="subtle">{ev.at ? relativeTime(ev.at) : ''}</span>
                        </div>
                        {ev.body && <div className="body">{ev.body}</div>}
                      </div>
                    ))}
                </div>
              </div>
            )}
            <div className="col" style={{ gap: 6 }}>
              <textarea className="textarea" style={{ minHeight: 56 }} aria-label="Pull request comment" placeholder="Comment on this pull request" value={comment} onChange={(e) => setComment(e.target.value)} />
              <button className="btn btn-sm" style={{ alignSelf: 'flex-end' }} disabled={!comment.trim() || posting} onClick={() => void post()}>
                {posting && <span className="spinner" style={{ width: 11, height: 11 }} />}
                Comment
              </button>
            </div>
          </>
        ) : draft ? (
          <div className="col" style={{ gap: 8 }}>
            {(!status.upstream || status.ahead > 0) && (
              <div className="gp-note warn row" style={{ gap: 8 }}>
                <span className="grow">{status.upstream ? `${status.ahead} commit(s) are not pushed yet.` : 'This branch is not on the remote yet.'} Push it before creating the pull request.</span>
                <button className="btn btn-sm" onClick={onPush}>
                  <Upload size={12} /> Push
                </button>
              </div>
            )}
            <div className="field">
              <label htmlFor="gp-pr-title">Title</label>
              <input id="gp-pr-title" className="input" value={draft.title} onChange={(e) => setDraft({ ...draft, title: e.target.value })} />
            </div>
            <div className="field">
              <label htmlFor="gp-pr-body">Description</label>
              <textarea id="gp-pr-body" className="textarea" style={{ minHeight: 140 }} value={draft.body} onChange={(e) => setDraft({ ...draft, body: e.target.value })} />
            </div>
            <div className="field">
              <label htmlFor="gp-pr-base">Base branch</label>
              <input id="gp-pr-base" className="input" placeholder="Repository default" value={draft.base} onChange={(e) => setDraft({ ...draft, base: e.target.value })} />
            </div>
            <div className="row" style={{ gap: 8 }}>
              <Toggle checked={draft.draft} onChange={(v) => setDraft({ ...draft, draft: v })} label="Create as draft" />
              <span className="small">Create as draft</span>
              <span className="spacer" />
              <button className="btn btn-sm btn-ghost" onClick={() => setDraft(null)}>
                Cancel
              </button>
              <button className="btn btn-sm btn-primary" disabled={!draft.title.trim() || creating} onClick={() => void create()}>
                {creating && <span className="spinner" style={{ width: 11, height: 11 }} />}
                Create pull request
              </button>
            </div>
          </div>
        ) : (
          <div className="row" style={{ gap: 8 }}>
            <span className="small muted grow">No pull request for {status.branch ?? 'this branch'}.</span>
            <button className="btn btn-sm" disabled={drafting} onClick={() => void startDraft()}>
              {drafting ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Sparkles size={12} />}
              Draft pull request
            </button>
          </div>
        )}
      </div>
    </div>
  )
}
