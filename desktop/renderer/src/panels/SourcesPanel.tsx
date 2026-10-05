import { Eye, Pencil } from 'lucide-react'
import { useApp } from '@/store/app'
import { relativeTime } from '@/components/ui'
import { openFileInPanel } from '@/views/items'

/** Files the agent read or edited in this thread. */
export function SourcesPanel() {
  const ts = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId] : undefined))
  if (!ts) return <div className="empty">Open a thread to see its sources.</div>
  if (!ts.sources.length) return <div className="empty">Files the agent reads or edits show up here.</div>
  const root = (ts.thread.worktree?.path ?? ts.thread.cwd).replace(/\\/g, '/')
  const sorted = [...ts.sources].sort((a, b) => b.lastTouched - a.lastTouched)
  return (
    <div style={{ padding: 6 }}>
      {sorted.map((s) => {
        const norm = s.path.replace(/\\/g, '/')
        const rel = norm.toLowerCase().startsWith(root.toLowerCase()) ? norm.slice(root.length).replace(/^\//, '') : norm
        return (
          <button key={s.path} className="nav-item" style={{ width: '100%' }} title={s.path} onClick={() => openFileInPanel(s.path)}>
            {s.edited ? <Pencil size={12} color="var(--accent)" /> : <Eye size={12} />}
            <span className="ellipsis grow mono xs" style={{ textAlign: 'left' }}>
              {rel}
            </span>
            <span className="xs subtle">{relativeTime(s.lastTouched)}</span>
          </button>
        )
      })}
    </div>
  )
}
