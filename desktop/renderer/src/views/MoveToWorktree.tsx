import { useEffect, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { GitBranch } from 'lucide-react'
import type { GitStatus, Thread } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { Modal } from '@/components/ui'
import { threadEnvironment } from '@/lib/environments'
import '@/styles/environments.css'

/** Can this thread move from the local checkout into a worktree? */
export function canMoveToWorktree(t: Thread): boolean {
  if (t.worktree || t.kind === 'quickChat') return false
  const p = useApp.getState().projects.find((x) => x.id === t.projectId)
  return !!p?.isGit
}

function MoveToWorktreeDialog({ thread, onClose }: { thread: Thread; onClose: () => void }) {
  const project = useApp((s) => s.projects.find((p) => p.id === thread.projectId))
  const [status, setStatus] = useState<GitStatus | null>(null)
  const [error, setError] = useState<string | null>(null)
  const envs = project?.trusted ? project.environments : []
  const [envId, setEnvId] = useState(threadEnvironment(thread, project)?.id ?? '')
  const [keepLocal, setKeepLocal] = useState(false)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    call('git/status', { cwd: thread.cwd })
      .then(setStatus)
      .catch((e: Error) => setError(e.message))
  }, [thread.cwd])

  const files = status?.files ?? []
  const move = async () => {
    setBusy(true)
    try {
      const r = await call('worktree/fromLocal', { threadId: thread.id, environmentId: envs.length ? envId : null, keepLocal })
      toast(r.message, 'success')
      onClose()
    } catch (e) {
      toast(`Could not move the thread: ${(e as Error).message}`, 'error')
      setBusy(false)
    }
  }

  return (
    <Modal
      title="Move to worktree"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" disabled={busy || !!error} onClick={() => void move()}>
            {busy ? <span className="spinner" style={{ width: 11, height: 11 }} /> : <GitBranch size={13} />} Move to worktree
          </button>
        </>
      }
    >
      <div className="col" style={{ gap: 12 }}>
        <p className="small" style={{ margin: 0 }}>
          “{thread.name || thread.preview || 'This thread'}” continues in a new worktree on its own branch, starting from the current commit{status?.branch ? ` of ${status.branch}` : ''}. The uncommitted changes of the local checkout move with it.
        </p>
        {error && <div className="gp-note error">{error}</div>}
        {!status && !error && <span className="spinner" style={{ width: 14, height: 14 }} />}
        {status && (
          <div className="field">
            <label>{files.length ? `Uncommitted changes (${files.length})` : 'Uncommitted changes'}</label>
            {files.length ? (
              <ul className="mtw-files" aria-label="Changes to move">
                {files.slice(0, 200).map((f) => (
                  <li key={f.path}>
                    <span className="code">{f.code.trim() || '•'}</span>
                    <span className="ellipsis">{f.path}</span>
                  </li>
                ))}
              </ul>
            ) : (
              <span className="hint">None: the worktree starts clean.</span>
            )}
          </div>
        )}
        {envs.length > 0 && (
          <div className="field">
            <label htmlFor="mtw-env">Environment</label>
            <select id="mtw-env" className="select" value={envId} onChange={(e) => setEnvId(e.target.value)}>
              {envs.map((e) => (
                <option key={e.id} value={e.id}>
                  {e.name}
                  {e.id === (project?.defaultEnvironment || envs[0]?.id) ? ' (default)' : ''}
                </option>
              ))}
              <option value="">No environment</option>
            </select>
            <span className="hint">Its setup script runs in the new worktree before the agent's next turn; its variables apply to the thread's commands.</span>
          </div>
        )}
        {files.length > 0 && (
          <label className="checkbox small">
            <input type="checkbox" checked={keepLocal} onChange={(e) => setKeepLocal(e.target.checked)} /> Keep a copy of the changes in the local checkout
          </label>
        )}
        {files.length > 0 && !keepLocal && <span className="xs subtle">The local checkout ends clean; a copy of the changes is kept in git stash (“odex: moved to …”).</span>}
      </div>
    </Modal>
  )
}

/** Show the "Move to worktree" dialog for a local thread. */
export function openMoveToWorktree(thread: Thread): void {
  const host = document.createElement('div')
  document.body.appendChild(host)
  const root = createRoot(host)
  const close = () => {
    root.unmount()
    host.remove()
  }
  root.render(<MoveToWorktreeDialog thread={thread} onClose={close} />)
}
