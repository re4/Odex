import { Download, Eye, FolderOpen, Pencil } from 'lucide-react'
import { useApp } from '@/store/app'
import { toast } from '@/lib/rpc'
import { basename, relativeTime } from '@/components/ui'
import { openFileInPanel } from '@/views/items'
import '@/styles/sidepanel.css'

const revealLabel = window.odex?.platform === 'win32' ? 'Reveal in File Explorer' : window.odex?.platform === 'darwin' ? 'Reveal in Finder' : 'Reveal in file manager'

/** Sources are recorded relative to the thread's folder when inside it. */
function absolutePath(cwd: string, p: string): string {
  if (/^([a-zA-Z]:[\\/]|[\\/])/.test(p)) return p
  const sep = cwd.includes('\\') ? '\\' : '/'
  return `${cwd.replace(/[\\/]+$/, '')}${sep}${p.replace(/[\\/]/g, sep)}`
}

/** "Save as…": copy a file somewhere the user picks. */
export async function saveFileCopy(path: string): Promise<void> {
  try {
    const dest = await window.odex.dialog.saveCopy(path)
    if (dest) toast(`Saved a copy of ${basename(path)} to ${dest}`, 'success')
  } catch (e) {
    toast(`Could not save ${basename(path)}: ${(e as Error).message}`, 'error')
  }
}

/** Files the agent read or edited in this thread. */
export function SourcesPanel() {
  const ts = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId] : undefined))
  if (!ts) return <div className="empty">Open a thread to see its sources.</div>
  if (!ts.sources.length) return <div className="empty">Files the agent reads or edits show up here.</div>
  const root = (ts.thread.worktree?.path ?? ts.thread.cwd).replace(/\\/g, '/')
  const sorted = [...ts.sources].sort((a, b) => b.lastTouched - a.lastTouched)
  return (
    <div style={{ padding: 6 }} role="list" aria-label="Sources">
      {sorted.map((s) => {
        const norm = s.path.replace(/\\/g, '/')
        const rel = norm.toLowerCase().startsWith(root.toLowerCase()) ? norm.slice(root.length).replace(/^\//, '') : norm
        const abs = absolutePath(ts.thread.cwd, s.path)
        return (
          <div key={s.path} className="source-row" role="listitem" aria-label={rel}>
            <button className="nav-item grow" title={`Open ${abs}`} onClick={() => openFileInPanel(abs)}>
              {s.edited ? <Pencil size={12} color="var(--accent)" aria-label="edited" /> : <Eye size={12} aria-label="read" />}
              <span className="ellipsis grow mono xs" style={{ textAlign: 'left' }}>
                {rel}
              </span>
              <span className="xs subtle source-when">{relativeTime(s.lastTouched)}</span>
            </button>
            <span className="source-actions">
              <button className="icon-btn sm" aria-label={`Save a copy of ${rel}`} title="Save as…" onClick={() => void saveFileCopy(abs)}>
                <Download size={12} />
              </button>
              <button className="icon-btn sm" aria-label={`Reveal ${rel}`} title={revealLabel} onClick={() => void window.odex.shell.showItem(abs)}>
                <FolderOpen size={12} />
              </button>
            </span>
          </div>
        )
      })}
    </div>
  )
}
