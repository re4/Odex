import { useCallback, useEffect, useState } from 'react'
import { FolderOpen, GitBranch, RefreshCw, Trash2 } from 'lucide-react'
import type { WorktreeInfo } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { confirmDialog } from '@/lib/actions'
import { basename } from '@/components/ui'
import { Row } from '@/views/settings/GeneralSettings'
import { samePath } from '@/panels/gitShared'
import '@/styles/review.css'

/** Settings → Worktrees: where worktrees live, and every thread worktree with remove / open. */
export function WorktreesSettings() {
  const threads = useApp((s) => s.threads)
  const [list, setList] = useState<WorktreeInfo[] | null>(null)
  const [owners, setOwners] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const [dir, setDir] = useState<{ value: string | null; home: string } | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const [w, c] = await Promise.all([call('worktree/list', {}), call('config/read', {}).catch(() => null)])
      setList(w.worktrees)
      setOwners(Object.fromEntries(w.worktrees.map((wt, i) => [wt.path, w.threadIds[i]]).filter(([, id]) => !!id)))
      setError(null)
      if (c) setDir({ value: c.effective.worktrees_dir ?? null, home: c.odexHome })
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

  const effectiveDir = dir?.value || (dir ? `${dir.home.replace(/[\\/]+$/, '')}${dir.home.includes('\\') ? '\\' : '/'}worktrees` : '')

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
                    <span className="muted">No thread</span>
                  )}
                  {t?.archived && <span className="badge">archived</span>}
                  {wt.setupStatus && wt.setupStatus !== 'ok' && <span className={`badge ${wt.setupStatus.startsWith('failed') ? 'danger' : ''}`}>setup {wt.setupStatus.startsWith('failed') ? 'failed' : wt.setupStatus}</span>}
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
              <button className="icon-btn" title="Open folder" aria-label={`Open folder ${wt.path}`} onClick={() => void window.odex.shell.openPath(wt.path)}>
                <FolderOpen size={14} />
              </button>
              <button className="btn btn-sm btn-ghost" style={{ color: 'var(--danger)' }} disabled={!(t || owners[wt.path]) || busy === wt.path} title={t || owners[wt.path] ? 'Remove this worktree' : 'Not linked to a thread'} onClick={() => void remove(wt)}>
                {busy === wt.path ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <Trash2 size={12} />} Remove
              </button>
            </div>
          )
        })}
      </div>
    </div>
  )
}
