import { useEffect, useState } from 'react'
import { Search } from 'lucide-react'
import type { SearchHit } from '@shared/index'
import { useApp } from '@/store/app'
import { call } from '@/lib/rpc'
import { relativeTime } from '@/components/ui'

/** Full-text search across thread titles, content and branches. */
export function SearchView() {
  const [q, setQ] = useState('')
  const [hits, setHits] = useState<SearchHit[]>([])
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    if (!q.trim()) {
      setHits([])
      return
    }
    let cancelled = false
    setBusy(true)
    const t = setTimeout(() => {
      void call('thread/search', { query: q, limit: 100 })
        .then((r) => !cancelled && setHits(r.hits))
        .catch(() => !cancelled && setHits([]))
        .finally(() => !cancelled && setBusy(false))
    }, 120)
    return () => {
      cancelled = true
      clearTimeout(t)
    }
  }, [q])
  return (
    <div style={{ flex: 1, overflowY: 'auto' }}>
      <div style={{ maxWidth: 780, margin: '0 auto', padding: '24px 20px' }}>
        <div className="row" style={{ position: 'relative', marginBottom: 14 }}>
          <Search size={15} style={{ position: 'absolute', left: 10, color: 'var(--fg-subtle)' }} />
          <input className="input" autoFocus style={{ paddingLeft: 32, height: 36 }} placeholder="Search threads" value={q} onChange={(e) => setQ(e.target.value)} aria-label="Search threads" />
          {busy && <span className="spinner" style={{ position: 'absolute', right: 10 }} />}
        </div>
        {q && !busy && hits.length === 0 && <div className="empty">No results</div>}
        {hits.map((h, i) => (
          <button key={`${h.threadId}-${i}`} className="card" style={{ display: 'block', width: '100%', textAlign: 'left', padding: 10, marginBottom: 6, cursor: 'pointer' }} onClick={() => void useApp.getState().selectThread(h.threadId)}>
            <div className="row">
              <b className="ellipsis grow">{h.title || 'Untitled'}</b>
              <span className="badge">{h.field}</span>
              <span className="xs subtle">{relativeTime(h.updatedAt)}</span>
            </div>
            <div className="small muted" style={{ marginTop: 4 }} dangerouslySetInnerHTML={{ __html: highlight(h.snippet) }} />
          </button>
        ))}
      </div>
    </div>
  )
}

function highlight(snippet: string): string {
  const esc = snippet.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  // the engine marks matches with [[ ]]
  return esc.replace(/\[\[(.*?)\]\]/g, '<mark class="find-hit">$1</mark>')
}
