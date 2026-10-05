import { useEffect, useMemo, useRef, useState } from 'react'
import { FileText, MessageSquare, Terminal } from 'lucide-react'
import type { FileMatch } from '@shared/index'
import { useApp } from '@/store/app'
import { call } from '@/lib/rpc'
import { SHORTCUTS, displayKeys } from '@/lib/shortcuts'
import * as A from '@/lib/actions'
import { openFileInPanel } from '@/views/items'

interface Entry {
  id: string
  label: string
  detail?: string
  hint?: string
  icon?: React.ReactNode
  run: () => void
}

function fuzzy(q: string, s: string): number {
  if (!q) return 1
  const a = q.toLowerCase()
  const b = s.toLowerCase()
  const i = b.indexOf(a)
  if (i >= 0) return 100 - i
  let j = 0
  for (const ch of b) if (ch === a[j]) j++
  return j === a.length ? 10 : 0
}

export function CommandPalette() {
  const mode = useApp((s) => s.ui.paletteMode)
  const setUi = useApp((s) => s.setUi)
  const threads = useApp((s) => s.threads)
  const order = useApp((s) => s.threadOrder)
  const shortcuts = useApp((s) => s.settings?.shortcuts ?? {})
  const [q, setQ] = useState('')
  const [active, setActive] = useState(0)
  const [files, setFiles] = useState<FileMatch[]>([])
  const listRef = useRef<HTMLDivElement>(null)
  const close = () => setUi({ paletteOpen: false })

  const fileMode = mode === 'files' || q.startsWith('@')
  const threadMode = mode === 'threads' || q.startsWith('#')
  const query = fileMode || threadMode ? q.replace(/^[@#]/, '') : q.replace(/^>/, '')

  useEffect(() => {
    if (!fileMode) return
    const s = useApp.getState()
    const t = s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread : undefined
    const p = s.projects.find((x) => x.id === (t?.projectId ?? s.ui.newThreadProjectId))
    const roots = t ? [t.worktree?.path ?? t.cwd] : (p?.folders ?? [])
    if (!roots.length) {
      setFiles([])
      return
    }
    let cancelled = false
    const h = setTimeout(() => {
      void call('fs/search', { roots, query, limit: 60 })
        .then((r) => !cancelled && setFiles(r.files))
        .catch(() => {})
    }, 50)
    return () => {
      cancelled = true
      clearTimeout(h)
    }
  }, [fileMode, query])

  const entries: Entry[] = useMemo(() => {
    if (fileMode) {
      return files.map((f) => {
        const rel = f.path.startsWith(f.root) ? f.path.slice(f.root.length).replace(/^[\\/]/, '') : f.path
        return { id: f.path, label: rel, icon: <FileText size={13} />, run: () => openFileInPanel(f.path) }
      })
    }
    if (threadMode) {
      return order
        .map((id) => threads[id]?.thread)
        .filter((t) => t && !t.archived)
        .map((t) => ({ id: t!.id, label: t!.name || t!.preview || 'New thread', detail: t!.cwd, icon: <MessageSquare size={13} />, run: () => void useApp.getState().selectThread(t!.id) }))
        .map((e) => ({ e, s: fuzzy(query, e.label) }))
        .filter((x) => x.s > 0)
        .sort((a, b) => b.s - a.s)
        .map((x) => x.e)
        .slice(0, 80)
    }
    const actions: Entry[] = SHORTCUTS.filter((s) => !/^(palette|goto|interrupt)/.test(s.id)).map((s) => ({
      id: `sc:${s.id}`,
      label: s.label,
      hint: displayKeys(shortcuts[s.id] ?? s.keys),
      run: () => window.dispatchEvent(new CustomEvent('odex:shortcut', { detail: s.id })),
    }))
    const slash: Entry[] = A.SLASH_COMMANDS.map((c) => ({
      id: `slash:${c.name}`,
      label: `/${c.name}`,
      detail: c.description,
      icon: <Terminal size={13} />,
      run: () => {
        if (c.args) {
          const tid = useApp.getState().selectedThreadId
          window.dispatchEvent(new CustomEvent('odex:attach', { detail: { type: 'text', text: `/${c.name} ` } }))
          if (!tid) setUi({ view: 'home' })
        } else void c.run(useApp.getState().selectedThreadId, '')
      },
    }))
    const settingsEntries: Entry[] = [
      ['general', 'Settings: General'],
      ['models', 'Settings: Models & Endpoints'],
      ['mcp', 'Settings: MCP servers'],
      ['skills', 'Settings: Skills'],
      ['hooks', 'Settings: Hooks'],
      ['memories', 'Settings: Memories'],
      ['shortcuts', 'Settings: Keyboard shortcuts'],
    ].map(([id, label]) => ({ id: `set:${id}`, label, run: () => A.openSettings(id) }))
    const extra: Entry[] = [
      { id: 'onboarding', label: 'Run setup', run: () => setUi({ onboardingOpen: true }) },
      { id: 'restart-engine', label: 'Restart engine', run: () => void window.odex.restartEngine() },
      { id: 'search', label: 'Search threads', run: () => setUi({ view: 'search' }) },
      { id: 'automations', label: 'Automations', run: () => setUi({ view: 'automations' }) },
    ]
    return [...actions, ...slash, ...settingsEntries, ...extra]
      .map((e) => ({ e, s: Math.max(fuzzy(query, e.label), e.detail ? fuzzy(query, e.detail) / 2 : 0) }))
      .filter((x) => x.s > 0)
      .sort((a, b) => b.s - a.s)
      .map((x) => x.e)
  }, [fileMode, threadMode, files, order, threads, query, shortcuts, setUi])

  useEffect(() => setActive(0), [q, mode])
  useEffect(() => {
    listRef.current?.querySelector(`[data-idx="${active}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [active])

  const choose = (e: Entry | undefined) => {
    if (!e) return
    close()
    e.run()
  }

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div className="modal" role="dialog" aria-label="Command palette" style={{ width: 'min(640px, 92vw)' }}>
        <input
          className="input"
          autoFocus
          style={{ border: 'none', borderBottom: '1px solid var(--border)', borderRadius: 0, height: 44, fontSize: 15, padding: '0 14px' }}
          placeholder={mode === 'files' ? 'Search files…' : mode === 'threads' ? 'Go to thread…' : 'Type a command, @ for files, # for threads'}
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Escape') close()
            else if (e.key === 'ArrowDown') {
              e.preventDefault()
              setActive((a) => Math.min(entries.length - 1, a + 1))
            } else if (e.key === 'ArrowUp') {
              e.preventDefault()
              setActive((a) => Math.max(0, a - 1))
            } else if (e.key === 'Enter') {
              e.preventDefault()
              choose(entries[active])
            }
          }}
          aria-label="Command"
        />
        <div ref={listRef} style={{ overflowY: 'auto', maxHeight: '55vh', padding: 4 }} role="listbox">
          {entries.length === 0 && <div className="empty">No matches</div>}
          {entries.map((e, i) => (
            <button key={e.id} data-idx={i} role="option" aria-selected={i === active} className="menu-item" data-active={i === active} style={{ width: '100%' }} onMouseEnter={() => setActive(i)} onClick={() => choose(e)}>
              {e.icon}
              <span className="ellipsis">{e.label}</span>
              {e.detail && <span className="xs subtle ellipsis grow">{e.detail}</span>}
              {!e.detail && <span className="grow" />}
              {e.hint && <span className="hint">{e.hint}</span>}
            </button>
          ))}
        </div>
      </div>
    </div>
  )
}
