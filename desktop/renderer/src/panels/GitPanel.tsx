import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import {
  ArrowDown,
  ArrowLeft,
  ArrowRightLeft,
  ArrowUp,
  Check,
  ChevronDown,
  ChevronRight,
  CircleDot,
  CircleX,
  Columns2,
  ExternalLink,
  FileText,
  FolderOpen,
  GitBranch,
  GitCommitHorizontal,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  GitPullRequestDraft,
  Inbox,
  MessageSquare,
  Minus,
  Plus,
  RefreshCw,
  Rows3,
  Send,
  Sparkles,
  Trash2,
  Undo2,
  Upload,
  Wrench,
} from 'lucide-react'
import type {
  DiffFile,
  DiffTarget,
  GitBranch as Branch,
  GitCommitInfo,
  GitFileStatus,
  GitStatus,
  HandoffResult,
  HandoffStrategy,
  PrCheck,
  PrListItem,
  PrReviewComment,
  PullRequest,
  ReviewComment,
  WorktreeInfo,
} from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { confirmDialog, openSettings, sendMessage } from '@/lib/actions'
import { Modal, Toggle, relativeTime } from '@/components/ui'
import { Markdown } from '@/components/Markdown'
import { DiffView, StatusBadge, anchorKey, splitPath, type LineAnchor, type LineSelection } from '@/components/DiffView'
import { openFileInPanel } from '@/views/items'
import { CommentComposer, PendingCard } from '@/panels/ReviewPanel'
import { PR_STATE_CLASS, firstChangedLine, fixCheckMessage, friendlyGitError, isGhSetupError, joinPath, normPath, showInReview, useRepoContext } from '@/panels/gitShared'
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
  // a pull request opened full-panel (files, review comments, submit review)
  const [prNumber, setPrNumber] = useState<number | null>(null)

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
    setPrNumber(null)
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
  if (prNumber != null) {
    return <PrView root={repoRoot} threadId={threadId} number={prNumber} branch={status.branch ?? null} onBack={() => setPrNumber(null)} />
  }
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

      <PrCard root={repoRoot} threadId={threadId} status={status} onPush={() => setPushOpen(true)} onOpen={setPrNumber} />

      {status.remoteUrl && <PrInbox root={repoRoot} current={status.branch ?? null} onOpen={setPrNumber} />}

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
  // Settings → Git → Allow force push (the engine refuses force pushes without it)
  const [allowForce, setAllowForce] = useState(false)
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<{ ok: boolean; output: string } | null>(null)
  useEffect(() => {
    void call('config/read', {})
      .then((c) => setAllowForce(!!c.effective.git?.allow_force_push))
      .catch(() => {})
  }, [])
  const push = async () => {
    setBusy(true)
    try {
      const r = await call('git/push', { cwd: root, remote: remote.trim() || null, branch: status.branch ?? null, setUpstream, forceWithLease: force && allowForce })
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
          <button className={`btn ${force && allowForce ? 'btn-danger' : 'btn-primary'}`} disabled={busy || !remote.trim()} onClick={() => void push()}>
            {busy && <span className="spinner" style={{ width: 11, height: 11 }} />}
            {force && allowForce ? 'Force push' : 'Push'}
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
        <label className="checkbox small" title={allowForce ? undefined : 'Turn on “Allow force push” in Settings → Git'}>
          <input type="checkbox" checked={force && allowForce} disabled={!allowForce} onChange={(e) => setForce(e.target.checked)} />
          Force with lease
        </label>
        {!allowForce && (
          <div className="xs subtle">
            Force pushes are off.{' '}
            <a
              href="#"
              onClick={(e) => {
                e.preventDefault()
                onClose()
                openSettings('git')
              }}
            >
              Settings → Git
            </a>
          </div>
        )}
        {force && allowForce && <div className="gp-note warn">Force-with-lease overwrites the remote branch if nobody else pushed since your last fetch. Use it after rebasing or amending.</div>}
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

function PrStateIcon({ state }: { state: string }) {
  const color = state === 'merged' ? 'var(--pr-merged)' : state === 'closed' ? 'var(--danger)' : state === 'draft' ? 'var(--fg-subtle)' : 'var(--success)'
  if (state === 'merged') return <GitMerge size={14} color={color} aria-label="merged" />
  if (state === 'closed') return <GitPullRequestClosed size={14} color={color} aria-label="closed" />
  if (state === 'draft') return <GitPullRequestDraft size={14} color={color} aria-label="draft" />
  return <GitPullRequest size={14} color={color} aria-label="open" />
}

function checkCounts(checks: PrCheck[]): string {
  const passed = checks.filter((c) => c.state === 'success').length
  const failed = checks.filter((c) => c.state === 'failure').length
  const pending = checks.filter((c) => c.state === 'pending').length
  return `Checks: ${passed} passed${failed ? `, ${failed} failed` : ''}${pending ? `, ${pending} pending` : ''}`
}

/** CI checks of a PR; a failing check offers "Fix": its log goes to the thread's agent. */
function ChecksList({ pr, root, threadId }: { pr: PullRequest; root: string; threadId: string | null }) {
  const [fixing, setFixing] = useState<string | null>(null)
  if (!pr.checks.length) return null
  const fix = async (c: PrCheck) => {
    if (!threadId) return
    setFixing(c.name)
    try {
      const r = await call('pr/checkLog', { cwd: root, name: c.name, checkId: c.id ?? null, url: c.url ?? null })
      await sendMessage(threadId, [{ type: 'text', text: fixCheckMessage(c.name, { number: pr.number, title: pr.title, head: pr.head }, r.text, r.truncated) }])
      toast(`Sent the “${c.name}” failure to the agent`, 'success')
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
    } finally {
      setFixing(null)
    }
  }
  return (
    <div role="list" aria-label="Checks">
      <div className="xs subtle" style={{ marginBottom: 2 }}>
        {checkCounts(pr.checks)}
      </div>
      {pr.checks.map((c, i) => (
        <div key={`${c.name}-${i}`} className="gp-check" role="listitem">
          <CheckIcon state={c.state} />
          <span className="ellipsis grow">{c.name}</span>
          {c.state === 'failure' && (
            <button
              className="btn btn-sm gp-fix"
              disabled={!threadId || fixing != null}
              title={threadId ? 'Send this check’s log to the agent and ask it to fix the failure' : 'Open a thread to ask the agent for a fix'}
              aria-label={`Fix ${c.name}`}
              onClick={() => void fix(c)}
            >
              {fixing === c.name ? <span className="spinner" style={{ width: 10, height: 10 }} /> : <Wrench size={11} />} Fix
            </button>
          )}
          {c.url && (
            <button className="icon-btn sm" aria-label={`Open check ${c.name}`} onClick={() => void window.odex.shell.openExternal(c.url!)}>
              <ExternalLink size={11} />
            </button>
          )}
        </div>
      ))}
    </div>
  )
}

/** The pull request of the current branch: summary, checks, open for review, or draft a new one. */
function PrCard({ root, threadId, status, onPush, onOpen }: { root: string; threadId: string | null; status: GitStatus; onPush: () => void; onOpen: (n: number) => void }) {
  const [loading, setLoading] = useState(false)
  const [pr, setPr] = useState<PullRequest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [checked, setChecked] = useState(false)
  const [draft, setDraft] = useState<{ title: string; body: string; base: string; draft: boolean } | null>(null)
  const [drafting, setDrafting] = useState(false)
  const [creating, setCreating] = useState(false)
  const [showDetails, setShowDetails] = useState(false)
  const hasRemote = !!status.remoteUrl

  const load = useCallback(async () => {
    if (!hasRemote) return
    setLoading(true)
    try {
      const r = await call('pr/view', { cwd: root, threadId })
      setPr(r.pr ?? null)
      setError(r.error ?? null)
    } catch (e) {
      setError(errMsg(e))
    } finally {
      setLoading(false)
      setChecked(true)
    }
  }, [root, hasRemote, threadId])

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
      const r = await call('pr/create', { cwd: root, title: draft.title.trim(), body: draft.body, base: draft.base.trim() || null, draft: draft.draft, threadId })
      toast(`Pull request created${r.number ? ` (#${r.number})` : ''}`, 'success')
      setDraft(null)
      void load()
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
    } finally {
      setCreating(false)
    }
  }

  return (
    <div className="gp-card" aria-label="Pull request">
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
            <div className="row" style={{ gap: 6 }}>
              {friendlyGitError(error) !== error && (
                <button className="btn btn-sm btn-ghost" onClick={() => setShowDetails(!showDetails)}>
                  {showDetails ? 'Hide details' : 'Details'}
                </button>
              )}
              {isGhSetupError(error) && (
                <button className="btn btn-sm btn-ghost" onClick={() => openSettings('git')}>
                  GitHub settings
                </button>
              )}
            </div>
            {showDetails && <pre className="gp-out">{error}</pre>}
          </>
        ) : !checked && loading ? (
          <div className="row xs subtle">
            <span className="spinner" /> Looking for a pull request…
          </div>
        ) : pr ? (
          <>
            <div className="row" style={{ gap: 6, alignItems: 'flex-start' }}>
              <span className={`badge ${PR_STATE_CLASS[pr.state] ?? ''}`}>{pr.state}</span>
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
            <div className="xs subtle row" style={{ gap: 6, flexWrap: 'wrap' }}>
              <span className="mono">{pr.head}</span> → <span className="mono">{pr.base}</span>
              <span>· {pr.author}</span>
              <span className="text-add">+{pr.additions}</span>
              <span className="text-del">-{pr.deletions}</span>
            </div>
            <ChecksList pr={pr} root={root} threadId={threadId} />
            <div className="row" style={{ gap: 6 }}>
              <span className="xs subtle grow">
                {pr.files.length} file{pr.files.length === 1 ? '' : 's'}
                {pr.reviewComments.length ? ` · ${pr.reviewComments.length} review comment${pr.reviewComments.length === 1 ? '' : 's'}` : ''}
              </span>
              <button className="btn btn-sm" onClick={() => onOpen(pr.number)}>
                <FileText size={12} /> Files & review
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

/** Open pull requests of the repository (the inbox); click one to view and review it. */
function PrInbox({ root, current, onOpen }: { root: string; current: string | null; onOpen: (n: number) => void }) {
  const [items, setItems] = useState<PrListItem[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [num, setNum] = useState('')
  const load = useCallback(async () => {
    setLoading(true)
    try {
      const r = await call('pr/list', { cwd: root })
      setItems(r.prs)
      setError(r.error ?? null)
    } catch (e) {
      setError(errMsg(e))
    } finally {
      setLoading(false)
    }
  }, [root])
  useEffect(() => {
    void load()
  }, [load])
  const n = Number(num.replace(/^#/, ''))
  const valid = Number.isInteger(n) && n > 0
  return (
    <div className="gp-card" aria-label="Pull requests">
      <div className="gp-card-head">
        <Inbox size={14} />
        <b className="small grow">Pull requests</b>
        {items && items.length > 0 && <span className="badge">{items.length} open</span>}
        <button className="icon-btn sm" title="Refresh pull requests" aria-label="Refresh pull requests" onClick={() => void load()}>
          {loading ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <RefreshCw size={13} />}
        </button>
      </div>
      {error ? (
        <div className="gp-card-body">
          <div className={`gp-note ${isGhSetupError(error) ? 'warn' : 'error'}`}>{friendlyGitError(error)}</div>
        </div>
      ) : items == null ? (
        <div className="gp-card-body xs subtle">{loading ? 'Loading…' : ''}</div>
      ) : items.length === 0 ? (
        <div className="gp-card-body xs subtle">No open pull requests.</div>
      ) : (
        <div role="list" style={{ maxHeight: 280, overflowY: 'auto' }}>
          {items.map((p) => (
            <button key={p.number} role="listitem" className="gp-inbox-item" title={`${p.title}\n${p.head} → ${p.base} · ${p.author}`} onClick={() => onOpen(p.number)}>
              <PrStateIcon state={p.state} />
              <span className="num">#{p.number}</span>
              <span className="ellipsis grow">{p.title}</span>
              {current && p.head === current && <span className="badge accent">this branch</span>}
              <span className="xs subtle" style={{ flex: 'none' }}>
                {p.author}
                {p.updatedAt ? ` · ${relativeTime(p.updatedAt)}` : ''}
              </span>
            </button>
          ))}
        </div>
      )}
      <form
        className="gp-inbox-open"
        style={{ paddingTop: items && items.length > 0 && !error ? 8 : 0 }}
        onSubmit={(e) => {
          e.preventDefault()
          if (valid) {
            onOpen(n)
            setNum('')
          }
        }}
      >
        <input className="input" aria-label="Pull request number" placeholder="PR number" inputMode="numeric" value={num} onChange={(e) => setNum(e.target.value)} />
        <button type="submit" className="btn btn-sm" disabled={!valid}>
          Review PR
        </button>
      </form>
    </div>
  )
}

// --------------------------------------------------------------------- PR view

const EVENTS: Array<{ id: 'comment' | 'approve' | 'requestChanges'; label: string }> = [
  { id: 'comment', label: 'Comment' },
  { id: 'approve', label: 'Approve' },
  { id: 'requestChanges', label: 'Request changes' },
]

/** Draft review comments per PR (`root#number`), kept while the panel switches views. */
const prDrafts = new Map<string, ReviewComment[]>()

interface PrComposer {
  path: string
  file: DiffFile
  side: 'old' | 'new'
  anchor: number
  start: number
  end: number
}

function snippet(f: DiffFile, side: 'old' | 'new', start: number, end: number): string {
  const out: string[] = []
  for (const h of f.hunks)
    for (const l of h.lines) {
      const n = side === 'old' ? (l.kind === 'add' ? null : l.oldNo) : l.kind === 'del' ? null : l.newNo
      if (n != null && n >= start && n <= end) out.push(`${l.kind === 'add' ? '+' : l.kind === 'del' ? '-' : ' '}${l.text.replace(/\r$/, '')}`)
    }
  return out.slice(0, 40).join('\n')
}

function RemoteComment({ c, reply }: { c: PrReviewComment; reply?: boolean }) {
  return (
    <div className={`prv-remote ${reply ? 'reply' : ''}`} onClick={(e) => e.stopPropagation()}>
      <div className="prv-remote-head xs">
        <MessageSquare size={11} className="subtle" />
        <b>{c.author}</b>
        <span className="spacer" />
        <span className="subtle">{c.at ? relativeTime(c.at) : ''}</span>
      </div>
      <div className="prv-remote-body">{c.body}</div>
    </div>
  )
}

/**
 * One pull request, full panel: summary, checks (with Fix), description, activity, the diff with
 * existing review comments on their lines, and a review you write inline and submit as
 * Comment / Approve / Request changes (after confirmation).
 */
function PrView({ root, threadId, number, branch, onBack }: { root: string; threadId: string | null; number: number; branch: string | null; onBack: () => void }) {
  const [pr, setPr] = useState<PullRequest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const key = `${normPath(root)}#${number}`
  const [drafts, setDraftsState] = useState<ReviewComment[]>(() => prDrafts.get(key) ?? [])
  const setDrafts = (list: ReviewComment[]) => {
    prDrafts.set(key, list)
    setDraftsState(list)
  }
  const [composer, setComposer] = useState<PrComposer | null>(null)
  const draftRef = useRef('')
  const [body, setBody] = useState('')
  const [event, setEvent] = useState<'comment' | 'approve' | 'requestChanges'>('comment')
  const [submitting, setSubmitting] = useState(false)
  const [mode, setMode] = useState<'unified' | 'split'>('unified')

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const r = await call('pr/view', { cwd: root, number, threadId })
      setPr(r.pr ?? null)
      setError(r.error ?? (r.pr ? null : `Pull request #${number} was not found in this repository.`))
    } catch (e) {
      setError(errMsg(e))
    } finally {
      setLoading(false)
    }
  }, [root, number, threadId])
  useEffect(() => {
    void load()
  }, [load])

  const onLineClick = (f: DiffFile, _key: string, a: LineAnchor, e: React.MouseEvent) => {
    if (e.shiftKey && composer && composer.path === f.path && composer.side === a.side) {
      setComposer({ ...composer, start: Math.min(composer.anchor, a.line), end: Math.max(composer.anchor, a.line) })
      return
    }
    if (!composer) draftRef.current = ''
    setComposer({ path: f.path, file: f, side: a.side, anchor: a.line, start: a.line, end: a.line })
  }

  const addDraft = (text: string) => {
    if (!composer || !text.trim()) return
    setDrafts([
      ...drafts,
      {
        path: composer.path,
        line: composer.start,
        endLine: composer.end !== composer.start ? composer.end : null,
        side: composer.side,
        body: text.trim(),
        snippet: snippet(composer.file, composer.side, composer.start, composer.end) || null,
      },
    ])
    setComposer(null)
    draftRef.current = ''
  }

  const annotations = useMemo(() => {
    const out: Record<string, Map<string, ReactNode[]>> = {}
    const put = (path: string, a: string, node: ReactNode) => {
      const m = (out[path] ??= new Map())
      m.set(a, [...(m.get(a) ?? []), node])
    }
    if (pr) {
      const replies = new Map<string, PrReviewComment[]>()
      for (const c of pr.reviewComments) if (c.inReplyTo) replies.set(c.inReplyTo, [...(replies.get(c.inReplyTo) ?? []), c])
      for (const c of pr.reviewComments) {
        if (c.inReplyTo || c.line == null) continue
        const side = c.side?.toUpperCase() === 'LEFT' ? 'old' : 'new'
        put(
          c.path,
          anchorKey(side, c.line),
          <div key={`r${c.id}`} className="prv-thread">
            <RemoteComment c={c} />
            {(replies.get(c.id) ?? []).map((r) => (
              <RemoteComment key={r.id} c={r} reply />
            ))}
          </div>,
        )
      }
    }
    drafts.forEach((c, i) => {
      if (c.line == null) return
      put(
        c.path,
        anchorKey(c.side === 'old' ? 'old' : 'new', c.endLine ?? c.line),
        <PendingCard key={`d${i}`} comment={c} onDelete={() => setDrafts(drafts.filter((_, k) => k !== i))} onEdit={(text) => setDrafts(drafts.map((d, k) => (k === i ? { ...d, body: text } : d)))} />,
      )
    })
    if (composer) {
      put(
        composer.path,
        anchorKey(composer.side, composer.end),
        <CommentComposer
          key="composer"
          label={`Review comment on ${composer.side === 'old' ? 'old ' : ''}line${composer.end !== composer.start ? `s ${composer.start}–${composer.end}` : ` ${composer.start} · Shift+click another line to select a range`}`}
          placeholder="Comment for the pull request… (Ctrl+Enter to add)"
          draftRef={draftRef}
          onSubmit={addDraft}
          onCancel={() => setComposer(null)}
        />,
      )
    }
    const res: Record<string, Map<string, ReactNode>> = {}
    for (const [k, m] of Object.entries(out)) res[k] = new Map([...m].map(([a, nodes]) => [a, <>{nodes}</>]))
    return res
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pr, drafts, composer])

  const selection: LineSelection | null = composer ? { fileKey: composer.path, side: composer.side, start: composer.start, end: composer.end } : null
  const canSubmit = !!pr && !submitting && (event === 'approve' || !!body.trim() || drafts.length > 0)

  const submit = async () => {
    if (!pr || !canSubmit) return
    const what = EVENTS.find((e) => e.id === event)!.label
    const n = drafts.length
    const ok = await confirmDialog(
      'Submit review',
      `Submit a “${what}” review to pull request #${pr.number} on GitHub${n ? ` with ${n} inline comment${n === 1 ? '' : 's'}` : ''}? Everyone with access to the repository will see it.`,
      'Submit review',
    )
    if (!ok) return
    setSubmitting(true)
    try {
      const r = await call('pr/comment', { cwd: root, number: pr.number, comments: drafts, body: body.trim() || null, event, confirmed: true })
      if (r.ok) {
        toast('Review submitted', 'success')
        setDrafts([])
        setBody('')
        setEvent('comment')
        void load()
      } else toast(friendlyGitError(r.output), 'error')
    } catch (e) {
      toast(friendlyGitError(errMsg(e)), 'error')
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="prv" aria-label={`Pull request #${number}`}>
      <div className="prv-head">
        <button className="icon-btn sm" aria-label="Back to git summary" title="Back" onClick={onBack}>
          <ArrowLeft size={14} />
        </button>
        {pr && <PrStateIcon state={pr.state} />}
        <b className="small ellipsis grow">
          #{number}
          {pr ? ` ${pr.title}` : ''}
        </b>
        {pr && (
          <button className="icon-btn sm" aria-label="Open on GitHub" title="Open on GitHub" onClick={() => void window.odex.shell.openExternal(pr.url)}>
            <ExternalLink size={13} />
          </button>
        )}
        <button className="icon-btn sm" title="Refresh" aria-label="Refresh pull request" onClick={() => void load()}>
          {loading ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <RefreshCw size={13} />}
        </button>
      </div>
      <div className="prv-scroll">
        {error ? (
          <div className={`gp-note ${isGhSetupError(error) ? 'warn' : 'error'}`} role="alert">
            {friendlyGitError(error)}
          </div>
        ) : !pr ? (
          <div className="empty">
            <span className="spinner" />
          </div>
        ) : (
          <>
            <div className="col" style={{ gap: 4 }}>
              <div className="row" style={{ gap: 6, flexWrap: 'wrap' }}>
                <span className={`badge ${PR_STATE_CLASS[pr.state] ?? ''}`}>{pr.state}</span>
                <span className="xs subtle">
                  <span className="mono">{pr.head}</span> → <span className="mono">{pr.base}</span> · {pr.author} · <span className="text-add">+{pr.additions}</span> <span className="text-del">-{pr.deletions}</span>
                </span>
                {branch && pr.head === branch && <span className="badge accent">this branch</span>}
              </div>
            </div>
            <ChecksList pr={pr} root={root} threadId={threadId} />
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
            {pr.timeline.length > 0 && (
              <details>
                <summary className="xs subtle" style={{ cursor: 'pointer' }}>
                  Activity ({pr.timeline.length})
                </summary>
                <div className="gp-timeline" style={{ marginTop: 6 }}>
                  {[...pr.timeline]
                    .sort((a, b) => b.at - a.at)
                    .slice(0, 30)
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
              </details>
            )}
            <div className="prv-files-head">
              <b className="small grow">
                Files changed <span className="badge">{pr.files.length}</span>
              </b>
              <button className="icon-btn sm" title={mode === 'unified' ? 'Split view' : 'Unified view'} aria-label={mode === 'unified' ? 'Switch to split view' : 'Switch to unified view'} onClick={() => setMode(mode === 'unified' ? 'split' : 'unified')}>
                {mode === 'unified' ? <Columns2 size={13} /> : <Rows3 size={13} />}
              </button>
            </div>
            {pr.files.length === 0 ? (
              <div className="xs subtle">No file changes.</div>
            ) : (
              <DiffView
                files={pr.files}
                mode={mode}
                onLineClick={onLineClick}
                selection={selection}
                annotations={annotations}
                fileActions={(f) =>
                  branch && pr.head === branch && f.status !== 'deleted' ? (
                    <button className="icon-btn sm" title="Open file at the first change" aria-label={`Open ${f.path}`} onClick={() => openFileInPanel(joinPath(root, f.path), firstChangedLine(f))}>
                      <FileText size={13} />
                    </button>
                  ) : null
                }
              />
            )}
          </>
        )}
      </div>
      {pr && (
        <div className="prv-reviewbar" aria-label="Your review">
          <div className="row" style={{ gap: 8 }}>
            <b className="small">Your review</b>
            {drafts.length > 0 && (
              <span className="badge accent">
                {drafts.length} inline comment{drafts.length === 1 ? '' : 's'}
              </span>
            )}
            <span className="spacer" />
            {drafts.length > 0 && (
              <button
                className="btn btn-sm btn-ghost"
                onClick={() =>
                  void (async () => {
                    if (await confirmDialog('Discard review comments', `Discard ${drafts.length} draft comment(s)?`, 'Discard', true)) setDrafts([])
                  })()
                }
              >
                <Trash2 size={12} /> Discard
              </button>
            )}
          </div>
          <textarea className="textarea" aria-label="Review summary" placeholder="Leave a summary (optional for Approve)…" value={body} onChange={(e) => setBody(e.target.value)} />
          <div className="row" style={{ gap: 8 }}>
            <div className="prv-events" role="radiogroup" aria-label="Review event">
              {EVENTS.map((ev) => (
                <label key={ev.id}>
                  <input type="radio" name={`prv-event-${number}`} checked={event === ev.id} onChange={() => setEvent(ev.id)} />
                  {ev.label}
                </label>
              ))}
            </div>
            <span className="spacer" />
            <button className="btn btn-sm btn-primary" disabled={!canSubmit} onClick={() => void submit()}>
              {submitting ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Send size={12} />} Submit review
            </button>
          </div>
          {drafts.length === 0 && <div className="xs subtle">Click a line number in the diff to add an inline comment.</div>}
        </div>
      )}
    </div>
  )
}
