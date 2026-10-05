import { useCallback, useEffect, useState } from 'react'
import { ArchiveRestore, ExternalLink, GitBranch, RefreshCw, Search, Trash2 } from 'lucide-react'
import type { Thread } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { confirmDialog } from '@/lib/actions'
import { basename, relativeTime } from '@/components/ui'
import { Section } from '@/views/settings/ConfigSettings'

function title(t: Thread): string {
  return t.name || t.preview || 'Untitled thread'
}

export function ArchivedSettings() {
  const projects = useApp((s) => s.projects)
  const [threads, setThreads] = useState<Thread[] | null>(null)
  const [query, setQuery] = useState('')
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      setThreads((await call('thread/list', { archived: true, limit: 2000 })).threads)
    } catch (e) {
      toast(`Could not list archived threads: ${(e as Error).message}`, 'error')
      setThreads([])
    }
  }, [])
  useEffect(() => {
    void load()
  }, [load])

  const unarchive = async (t: Thread) => {
    setBusy(t.id)
    try {
      const r = await call('thread/unarchive', { threadId: t.id })
      useApp.getState().applyNotification('thread/updated', { thread: r.thread })
      setThreads((cur) => (cur ?? []).filter((x) => x.id !== t.id))
      toast(`Restored “${title(t)}”`, 'success')
    } catch (e) {
      toast(`Could not restore: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(null)
    }
  }
  const remove = async (t: Thread) => {
    if (!(await confirmDialog('Delete thread permanently', `Delete “${title(t)}” and its history? This cannot be undone.`, 'Delete', true))) return
    setBusy(t.id)
    try {
      await call('thread/delete', { threadId: t.id })
      setThreads((cur) => (cur ?? []).filter((x) => x.id !== t.id))
    } catch (e) {
      toast(`Could not delete: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(null)
    }
  }
  const open = (t: Thread) => void useApp.getState().selectThread(t.id)

  const q = query.trim().toLowerCase()
  const projectName = (t: Thread) => projects.find((p) => p.id === t.projectId)?.name ?? (t.cwd ? basename(t.cwd) : '')
  const visible = (threads ?? []).filter((t) => !q || title(t).toLowerCase().includes(q) || t.preview.toLowerCase().includes(q) || projectName(t).toLowerCase().includes(q))

  return (
    <div className="sx-panel">
      <Section desc="Archived threads are hidden from the sidebar but keep their full history. Restore one to continue it.">
        <div className="sx-filters">
          <div className="sx-search">
            <Search size={13} aria-hidden />
            <input className="input" placeholder={threads?.length ? `Search ${threads.length} archived thread${threads.length === 1 ? '' : 's'}` : 'Search archived threads'} value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search archived threads" />
          </div>
          <button className="icon-btn" aria-label="Refresh archived threads" title="Refresh" onClick={() => void load()}>
            <RefreshCw size={14} />
          </button>
        </div>
        <div className="sx-list" role="list" aria-label="Archived threads">
          {threads == null ? (
            <div className="sx-list-empty">Loading…</div>
          ) : visible.length === 0 ? (
            <div className="sx-list-empty">{threads.length ? 'No archived threads match.' : 'No archived threads.'}</div>
          ) : (
            visible.map((t) => (
              <div key={t.id} className="sx-list-row" role="listitem" aria-label={title(t)}>
                <div className="grow" style={{ minWidth: 0 }}>
                  <div className="sx-thread-title ellipsis" title={title(t)}>
                    {title(t)}
                  </div>
                  <div className="xs subtle row" style={{ gap: 6, marginTop: 2 }}>
                    {projectName(t) && <span className="ellipsis">{projectName(t)}</span>}
                    {t.worktree && (
                      <span className="row" style={{ gap: 3 }} title={t.worktree.path}>
                        <GitBranch size={11} aria-hidden /> worktree
                      </span>
                    )}
                    <span>· {relativeTime(t.updatedAt)}</span>
                    {t.name && t.preview && <span className="ellipsis">· {t.preview}</span>}
                  </div>
                </div>
                <button className="btn btn-sm btn-ghost" onClick={() => open(t)} aria-label={`Open ${title(t)}`}>
                  <ExternalLink size={13} /> Open
                </button>
                <button className="btn btn-sm" disabled={busy === t.id} onClick={() => void unarchive(t)} aria-label={`Unarchive ${title(t)}`}>
                  <ArchiveRestore size={13} /> Unarchive
                </button>
                <button className="icon-btn sm" disabled={busy === t.id} onClick={() => void remove(t)} aria-label={`Delete ${title(t)} permanently`} title="Delete permanently">
                  <Trash2 size={13} />
                </button>
              </div>
            ))
          )}
        </div>
      </Section>
    </div>
  )
}
