import { useCallback, useEffect, useState } from 'react'
import { FileText, FolderOpen, GitBranch, RefreshCw, RotateCw, Trash2 } from 'lucide-react'
import type { WorktreeInfo, WorktreesToml } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { confirmDialog } from '@/lib/actions'
import { basename, Toggle } from '@/components/ui'
import { Row } from '@/views/settings/GeneralSettings'
import { samePath } from '@/panels/gitShared'
import '@/styles/review.css'
import '@/styles/environments.css'

const DEFAULT_KEEP = 15

/** Setup-script state of a worktree: badge, log and rerun. */
export function SetupState({ threadId, wt }: { threadId: string | undefined; wt: WorktreeInfo }) {
  const status = wt.setupStatus
  if (!status) return null
  const failed = status.startsWith('failed')
  const rerun = async () => {
    if (!threadId) return
    try {
      await call('worktree/setup', { threadId })
      toast('Setup script started')
    } catch (e) {
      toast(`Could not run the setup script: ${(e as Error).message}`, 'error')
    }
  }
  return (
    <span className="wt-setup">
      {status === 'running' ? (
        <span className="badge" title="The environment's setup script is running; the agent starts when it finishes">
          <span className="spinner" style={{ width: 9, height: 9 }} /> setup running
        </span>
      ) : (
        <span className={`badge ${failed ? 'danger' : 'success'}`} title={status}>
          setup {failed ? 'failed' : 'ok'}
        </span>
      )}
      {wt.setupLog && (
        <button className="icon-btn sm" title="Open the setup log" aria-label="Open setup log" onClick={() => void window.odex.shell.openPath(wt.setupLog!)}>
          <FileText size={12} />
        </button>
      )}
      {status !== 'running' && threadId && (
        <button className="icon-btn sm" title="Run the setup script again" aria-label="Rerun setup" onClick={() => void rerun()}>
          <RotateCw size={12} />
        </button>
      )}
    </span>
  )
}

/** Settings → Worktrees: where worktrees live, retention, and every thread worktree with remove / open. */
export function WorktreesSettings() {
  const threads = useApp((s) => s.threads)
  const [list, setList] = useState<WorktreeInfo[] | null>(null)
  const [owners, setOwners] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const [dir, setDir] = useState<{ value: string | null; home: string } | null>(null)
  const [retention, setRetention] = useState<WorktreesToml>({})
  const [keepDraft, setKeepDraft] = useState('')
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const [w, c] = await Promise.all([call('worktree/list', {}), call('config/read', {}).catch(() => null)])
      setList(w.worktrees)
      setOwners(Object.fromEntries(w.worktrees.map((wt, i) => [wt.path, w.threadIds[i]]).filter(([, id]) => !!id)))
      setError(null)
      if (c) {
        setDir({ value: c.effective.worktrees_dir ?? null, home: c.odexHome })
        const r = c.effective.worktrees ?? {}
        setRetention(r)
        setKeepDraft(String(r.keep ?? DEFAULT_KEEP))
      }
    } catch (e) {
      setError((e as Error).message)
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  const threadFor = (wt: WorktreeInfo) => Object.values(threads).find((t) => t.thread.worktree && samePath(t.thread.worktree.path, wt.path))?.thread

  const remove = async (wt: WorktreeInfo) => {
    const t = threadFor(wt)
    const threadId = t?.id ?? owners[wt.path]
    if (!threadId) return
    const ok = await confirmDialog(
      'Remove worktree',
      `Remove the worktree at ${wt.path}? Uncommitted work is saved as a snapshot ref first, and ${t ? `the thread "${t.name || t.preview || 'Untitled'}"` : 'its thread'} continues in the local checkout.`,
      'Remove worktree',
      true,
    )
    if (!ok) return
    setBusy(wt.path)
    try {
      await call('worktree/remove', { threadId })
      toast('Worktree removed', 'success')
    } catch (e) {
      toast(`Could not remove the worktree: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(null)
      void load()
    }
  }

  const setWorktreesDir = async (value: string | null) => {
    try {
      await call('config/write', { edits: [{ keyPath: 'worktrees_dir', value }] })
      toast(value ? `New worktrees go to ${value}` : 'Worktree folder reset to the default')
      void load()
    } catch (e) {
      toast((e as Error).message, 'error')
    }
  }

  const writeRetention = async (key: 'keep' | 'auto_cleanup', value: number | boolean) => {
    try {
      await call('config/write', { edits: [{ keyPath: `worktrees.${key}`, value }] })
      setRetention((r) => ({ ...r, [key]: value }))
    } catch (e) {
      toast((e as Error).message, 'error')
    }
  }

  const commitKeep = () => {
    const n = Math.floor(Number(keepDraft))
    if (!Number.isFinite(n) || n < 0) {
      setKeepDraft(String(retention.keep ?? DEFAULT_KEEP))
      return
    }
    if (n !== (retention.keep ?? DEFAULT_KEEP)) void writeRetention('keep', n)
  }

  const cleanUpNow = async () => {
    setBusy('prune')
    try {
      const r = await call('worktree/prune', {})
      toast(r.removed.length ? `Removed ${r.removed.length} worktree(s) of archived threads` : 'Nothing to clean up', r.removed.length ? 'success' : undefined)
    } catch (e) {
      toast((e as Error).message, 'error')
    } finally {
      setBusy(null)
      void load()
    }
  }

  const effectiveDir = dir?.value || (dir ? `${dir.home.replace(/[\\/]+$/, '')}${dir.home.includes('\\') ? '\\' : '/'}worktrees` : '')
  const keep = retention.keep ?? DEFAULT_KEEP
  const auto = retention.auto_cleanup ?? true

  return (
    <div>
      <h3 className="section-title">Location</h3>
      <Row label="Worktree folder" hint={<span className="mono">{effectiveDir || '…'}</span>}>
        <div className="row" style={{ gap: 6 }}>
          <button
            className="btn btn-sm"
            onClick={() =>
              void (async () => {
                const picked = await window.odex.dialog.openFolder()
                if (picked[0]) await setWorktreesDir(picked[0])
              })()
            }
          >
            Change…
          </button>
          {dir?.value && (
            <button className="btn btn-sm btn-ghost" onClick={() => void setWorktreesDir(null)}>
              Reset
            </button>
          )}
        </div>
      </Row>
      <p className="xs subtle">Threads started in worktree mode get their own checkout under this folder, on a branch named odex/&lt;thread&gt;. Archiving a thread offers to remove its worktree.</p>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Retention
      </h3>
      <Row label="Worktrees to keep" hint="When there are more, the oldest worktrees of archived threads are removed. Worktrees of active threads are never removed.">
        <input
          className="input"
          type="number"
          min={0}
          max={1000}
          value={keepDraft}
          onChange={(e) => setKeepDraft(e.target.value)}
          onBlur={commitKeep}
          onKeyDown={(e) => e.key === 'Enter' && (e.currentTarget as HTMLInputElement).blur()}
          style={{ width: 80 }}
          aria-label="Worktrees to keep"
        />
      </Row>
      <Row label="Clean up automatically" hint={auto ? `After archiving a thread or creating a worktree, beyond ${keep}` : 'Off: use “Clean up now”'}>
        <Toggle checked={auto} onChange={(v) => void writeRetention('auto_cleanup', v)} label="Clean up worktrees automatically" />
      </Row>
      <Row label="Clean up now" hint="Uncommitted work is snapshotted first (refs/odex/archived/…); unarchiving a thread restores its worktree.">
        <button className="btn btn-sm" disabled={busy === 'prune'} onClick={() => void cleanUpNow()}>
          {busy === 'prune' ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Trash2 size={12} />} Clean up now
        </button>
      </Row>

      <div className="row" style={{ marginTop: 20, marginBottom: 8 }}>
        <h3 className="section-title grow" style={{ margin: 0 }}>
          Worktrees {list ? `(${list.length})` : ''}
        </h3>
        <button className="btn btn-sm btn-ghost" onClick={() => void load()} aria-label="Refresh worktrees">
          <RefreshCw size={12} /> Refresh
        </button>
      </div>
      {error && <div className="gp-note error">{error}</div>}
      {list && list.length === 0 && <div className="card empty small">No worktrees. Start a thread in worktree mode to create one.</div>}
      <div className="wt-list">
        {list?.map((wt) => {
          const t = threadFor(wt)
          const threadId = t?.id ?? owners[wt.path]
          // the store's copy is live (setup status updates arrive as notifications)
          const live = t?.worktree && samePath(t.worktree.path, wt.path) ? t.worktree : wt
          return (
            <div key={wt.path} className="card wt-item">
              <GitBranch size={16} className="subtle" style={{ flex: 'none' }} />
              <div className="grow" style={{ minWidth: 0 }}>
                <div className="row" style={{ gap: 6 }}>
                  {t ? (
                    <a
                      href="#"
                      className="ellipsis"
                      style={{ fontWeight: 600 }}
                      onClick={(e) => {
                        e.preventDefault()
                        void useApp.getState().selectThread(t.id)
                      }}
                    >
                      {t.name || t.preview || 'Untitled thread'}
                    </a>
                  ) : (
                    <span className="muted">{threadId ? 'Archived thread' : 'No thread'}</span>
                  )}
                  {t?.archived && <span className="badge">archived</span>}
                  <SetupState threadId={threadId} wt={live} />
                </div>
                <div className="meta xs subtle">
                  <span>
                    branch <span className="mono">{wt.branch}</span>
                    {wt.baseBranch && (
                      <>
                        {' '}
                        from <span className="mono">{wt.baseBranch}</span>
                      </>
                    )}
                  </span>
                  <span title={wt.repoRoot}>repo {basename(wt.repoRoot)}</span>
                </div>
                <div className="xs mono subtle ellipsis selectable" title={wt.path}>
                  {wt.path}
                </div>
              </div>
              <div className="wt-actions">
                <button className="icon-btn" title="Open folder" aria-label={`Open folder ${wt.path}`} onClick={() => void window.odex.shell.openPath(wt.path)}>
                  <FolderOpen size={14} />
                </button>
                <button className="btn btn-sm btn-ghost" style={{ color: 'var(--danger)' }} disabled={!threadId || busy === wt.path} title={threadId ? 'Remove this worktree' : 'Not linked to a thread'} onClick={() => void remove(wt)}>
                  {busy === wt.path ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Trash2 size={12} />} Remove
                </button>
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}
