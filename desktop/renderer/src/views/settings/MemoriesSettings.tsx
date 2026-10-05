import { useCallback, useEffect, useMemo, useState } from 'react'
import { Check, Pencil, Plus, Search, Sparkles, Trash2, X } from 'lucide-react'
import type { Memory, Project } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { confirmDialog } from '@/lib/actions'
import { Modal, Toggle, basename, relativeTime } from '@/components/ui'
import { Row } from '@/views/settings/GeneralSettings'
import { Section, useEngineConfig } from '@/views/settings/ConfigSettings'

const CATEGORIES = ['preference', 'convention', 'stack', 'other'] as const
const CATEGORY_LABEL: Record<string, string> = { preference: 'Preference', convention: 'Convention', stack: 'Stack', other: 'Other' }

const norm = (p: string) => p.replace(/[\\/]+$/, '').replace(/\//g, '\\').toLowerCase()

function projectPath(p: Project): string {
  return p.folders[p.primary] ?? p.folders[0] ?? ''
}

function scopeLabel(m: Memory, projects: Project[]): string {
  if (m.scope !== 'project' || !m.projectPath) return 'Global'
  const p = projects.find((x) => x.folders.some((f) => norm(f) === norm(m.projectPath!)))
  return p ? p.name : basename(m.projectPath)
}

function MemoryRow({ m, projects, onSave, onDelete }: { m: Memory; projects: Project[]; onSave: (m: Memory) => Promise<void>; onDelete: (m: Memory) => Promise<void> }) {
  const [editing, setEditing] = useState(false)
  const [text, setText] = useState(m.text)
  const [category, setCategory] = useState(m.category)
  const proposed = m.status !== 'approved'
  return (
    <div className="sx-list-row top" role="listitem" aria-label={m.text}>
      <div className="grow" style={{ minWidth: 0 }}>
        {editing ? (
          <div className="col" style={{ gap: 6 }}>
            <textarea className="textarea" rows={2} value={text} onChange={(e) => setText(e.target.value)} aria-label="Memory text" autoFocus style={{ minHeight: 56 }} />
            <div className="row" style={{ gap: 6 }}>
              <select className="select" style={{ width: 150 }} value={category} onChange={(e) => setCategory(e.target.value)} aria-label="Memory category">
                {CATEGORIES.map((c) => (
                  <option key={c} value={c}>
                    {CATEGORY_LABEL[c]}
                  </option>
                ))}
              </select>
              <span className="spacer" />
              <button
                className="btn btn-sm"
                onClick={() => {
                  setEditing(false)
                  setText(m.text)
                  setCategory(m.category)
                }}
              >
                Cancel
              </button>
              <button
                className="btn btn-sm btn-primary"
                disabled={!text.trim()}
                onClick={async () => {
                  await onSave({ ...m, text, category })
                  setEditing(false)
                }}
              >
                Save
              </button>
            </div>
          </div>
        ) : (
          <>
            <div className="sx-mem-text">{m.text}</div>
            <div className="sx-mem-meta">
              {proposed && <span className="badge warning">suggested</span>}
              <span className="badge">{CATEGORY_LABEL[m.category] ?? m.category}</span>
              <span>{scopeLabel(m, projects)}</span>
              <span>· {relativeTime(m.updatedAt || m.createdAt)}</span>
            </div>
          </>
        )}
      </div>
      {!editing && (
        <div className="row" style={{ gap: 2, flex: 'none' }}>
          {proposed && (
            <button className="btn btn-sm btn-primary" aria-label={`Approve memory: ${m.text}`} onClick={() => void onSave({ ...m, status: 'approved' })}>
              <Check size={13} /> Approve
            </button>
          )}
          <button className="icon-btn sm" aria-label={`Edit memory: ${m.text}`} title="Edit" onClick={() => setEditing(true)}>
            <Pencil size={13} />
          </button>
          <button className="icon-btn sm" aria-label={`${proposed ? 'Dismiss' : 'Delete'} memory: ${m.text}`} title={proposed ? 'Dismiss' : 'Delete'} onClick={() => void onDelete(m)}>
            {proposed ? <X size={13} /> : <Trash2 size={13} />}
          </button>
        </div>
      )}
    </div>
  )
}

function AddMemory({ projects, onAdd }: { projects: Project[]; onAdd: (m: Memory) => Promise<boolean> }) {
  const [text, setText] = useState('')
  const [scope, setScope] = useState('global')
  const [category, setCategory] = useState<string>('preference')
  const [busy, setBusy] = useState(false)
  const submit = async () => {
    if (!text.trim()) return
    setBusy(true)
    const project = scope === 'global' ? null : (projects.find((p) => p.id === scope) ?? null)
    const ok = await onAdd({
      id: '',
      text: text.trim(),
      scope: project ? 'project' : 'global',
      projectPath: project ? projectPath(project) : null,
      status: 'approved',
      category,
      sourceThreadId: null,
      createdAt: 0,
      updatedAt: 0,
    })
    setBusy(false)
    if (ok) setText('')
  }
  return (
    <div className="card" style={{ padding: 10 }}>
      <input
        className="input"
        value={text}
        placeholder="e.g. I use pnpm, not npm. Tests run with vitest."
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => e.key === 'Enter' && void submit()}
        aria-label="New memory"
      />
      <div className="row" style={{ gap: 6, marginTop: 8 }}>
        <select className="select" style={{ width: 'auto', minWidth: 150 }} value={scope} onChange={(e) => setScope(e.target.value)} aria-label="New memory scope">
          <option value="global">Global</option>
          {projects.map((p) => (
            <option key={p.id} value={p.id}>
              Project: {p.name}
            </option>
          ))}
        </select>
        <select className="select" style={{ width: 'auto', minWidth: 130 }} value={category} onChange={(e) => setCategory(e.target.value)} aria-label="New memory category">
          {CATEGORIES.map((c) => (
            <option key={c} value={c}>
              {CATEGORY_LABEL[c]}
            </option>
          ))}
        </select>
        <span className="spacer" />
        <button className="btn btn-sm btn-primary" disabled={busy || !text.trim()} onClick={() => void submit()}>
          <Plus size={13} /> Add memory
        </button>
      </div>
    </div>
  )
}

/** Pick a thread and ask the utility model to propose memories from it (`memory/propose`). */
function SuggestFromThread({ onClose, onProposed }: { onClose: () => void; onProposed: (m: Memory[]) => void }) {
  const threads = useApp((s) => s.threads)
  const order = useApp((s) => s.threadOrder)
  const projects = useApp((s) => s.projects)
  const selected = useApp((s) => s.selectedThreadId)
  const list = useMemo(
    () =>
      order
        .map((id) => threads[id]?.thread)
        .filter((t): t is NonNullable<typeof t> => !!t && !t.archived && t.kind !== 'subagent')
        .sort((a, b) => b.updatedAt - a.updatedAt)
        .slice(0, 100),
    [threads, order],
  )
  const [tid, setTid] = useState(() => (selected && list.some((t) => t.id === selected) ? selected : (list[0]?.id ?? '')))
  const [busy, setBusy] = useState(false)
  const run = async () => {
    if (!tid) return
    setBusy(true)
    try {
      const r = await call('memory/propose', { threadId: tid })
      onProposed(r.memories)
      // new suggestions also arrive as a `memory/proposed` notification (with its own toast)
      if (!r.memories.length) toast('No new memories suggested for that thread')
      onClose()
    } catch (e) {
      toast(`Could not suggest memories: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      title="Suggest memories from a thread"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" disabled={!tid || busy} onClick={() => void run()}>
            {busy ? <span className="spinner" /> : <Sparkles size={13} />} Suggest
          </button>
        </>
      }
    >
      <p className="small muted" style={{ marginTop: 0 }}>
        The utility model reads the thread and proposes short facts about how you work. Nothing is saved until you approve it.
      </p>
      {list.length ? (
        <select className="select" style={{ width: '100%' }} value={tid} onChange={(e) => setTid(e.target.value)} aria-label="Thread to learn from">
          {list.map((t) => (
            <option key={t.id} value={t.id}>
              {(t.name || t.preview || 'Untitled thread').slice(0, 80)}
              {t.projectId ? ` · ${projects.find((p) => p.id === t.projectId)?.name ?? ''}` : ''}
            </option>
          ))}
        </select>
      ) : (
        <div className="small subtle">There are no threads yet.</div>
      )}
    </Modal>
  )
}

export function MemoriesSettings() {
  const { cfg, write } = useEngineConfig()
  const projects = useApp((s) => s.projects)
  const proposedLive = useApp((s) => s.proposedMemories)
  const [list, setList] = useState<Memory[] | null>(null)
  const [scopeFilter, setScopeFilter] = useState('all')
  const [catFilter, setCatFilter] = useState('all')
  const [query, setQuery] = useState('')
  const [suggesting, setSuggesting] = useState(false)

  const load = useCallback(async () => {
    try {
      setList((await call('memory/list', {})).memories)
    } catch (e) {
      toast(`Could not load memories: ${(e as Error).message}`, 'error')
      setList([])
    }
  }, [])
  useEffect(() => {
    void load()
  }, [load, proposedLive.length])

  const all = useMemo(() => {
    const byId = new Map<string, Memory>()
    for (const m of proposedLive) byId.set(m.id, m)
    for (const m of list ?? []) byId.set(m.id, m)
    return [...byId.values()].sort((a, b) => (b.updatedAt || b.createdAt) - (a.updatedAt || a.createdAt))
  }, [list, proposedLive])

  const dropProposed = (id: string) => useApp.setState((s) => ({ proposedMemories: s.proposedMemories.filter((x) => x.id !== id) }))

  const save = async (m: Memory) => {
    try {
      const r = await call('memory/upsert', { memory: m })
      if (r.memory.status === 'approved') dropProposed(r.memory.id)
      setList((cur) => [r.memory, ...(cur ?? []).filter((x) => x.id !== r.memory.id)])
      return true
    } catch (e) {
      toast(`Could not save the memory: ${(e as Error).message}`, 'error')
      return false
    }
  }
  const remove = async (m: Memory) => {
    if (m.status === 'approved' && !(await confirmDialog('Delete memory', `Forget “${m.text}”?`, 'Delete', true))) return
    try {
      await call('memory/delete', { id: m.id })
      dropProposed(m.id)
      setList((cur) => (cur ?? []).filter((x) => x.id !== m.id))
    } catch (e) {
      toast(`Could not delete the memory: ${(e as Error).message}`, 'error')
    }
  }

  if (!cfg) return <div className="spinner" aria-label="Loading" />
  const mem = cfg.user.memories ?? {}
  const enabled = !!mem.enabled
  const generate = mem.generate ?? enabled

  const q = query.trim().toLowerCase()
  const matches = (m: Memory) =>
    (scopeFilter === 'all' || (scopeFilter === 'global' ? m.scope !== 'project' : m.scope === 'project' && projects.find((p) => p.id === scopeFilter)?.folders.some((f) => norm(f) === norm(m.projectPath ?? '')))) &&
    (catFilter === 'all' || m.category === catFilter) &&
    (!q || m.text.toLowerCase().includes(q))
  const proposed = all.filter((m) => m.status !== 'approved')
  const approved = all.filter((m) => m.status === 'approved' && matches(m))
  const approvedTotal = all.filter((m) => m.status === 'approved').length

  return (
    <div className="sx-panel">
      <Section title="Options" desc="Short facts about how you work, kept on this computer and added to the prompt (within the memories budget under Context). Secrets are redacted before anything is stored.">
        <Row label="Use memories" hint="Add approved memories to every thread's prompt. Use /memories in a thread to turn them off there.">
          <Toggle checked={enabled} onChange={(v) => void write([{ keyPath: 'memories.enabled', value: v }])} label="Use memories" />
        </Row>
        <Row label="Suggest new memories" hint="After a thread ends, the utility model proposes memories. Nothing is saved until you approve it.">
          <Toggle checked={generate} onChange={(v) => void write([{ keyPath: 'memories.generate', value: v }])} label="Suggest new memories" />
        </Row>
        <Row label="Suggest from a thread" hint="Ask the utility model to propose memories from one thread now.">
          <button className="btn btn-sm" onClick={() => setSuggesting(true)}>
            <Sparkles size={13} /> Suggest memories from a thread…
          </button>
        </Row>
      </Section>
      {suggesting && (
        <SuggestFromThread
          onClose={() => setSuggesting(false)}
          onProposed={(ms) => setList((cur) => [...ms, ...(cur ?? []).filter((x) => !ms.some((m) => m.id === x.id))])}
        />
      )}

      {proposed.length > 0 && (
        <Section title={`Suggestions (${proposed.length})`} desc="Review what the model proposed. Approve, edit or dismiss each one.">
          <div className="sx-list" role="list" aria-label="Suggested memories">
            {proposed.map((m) => (
              <MemoryRow key={`${m.id}:${m.updatedAt}`} m={m} projects={projects} onSave={async (x) => void (await save(x))} onDelete={remove} />
            ))}
          </div>
        </Section>
      )}

      <Section title="Add a memory">
        <AddMemory projects={projects} onAdd={save} />
      </Section>

      <Section title={`Saved memories${approvedTotal ? ` (${approvedTotal})` : ''}`}>
        <div className="sx-filters">
          <div className="sx-search">
            <Search size={13} aria-hidden />
            <input className="input" value={query} placeholder="Filter memories" onChange={(e) => setQuery(e.target.value)} aria-label="Filter memories" />
          </div>
          <select className="select" value={scopeFilter} onChange={(e) => setScopeFilter(e.target.value)} aria-label="Filter by scope">
            <option value="all">All scopes</option>
            <option value="global">Global</option>
            {projects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
          <select className="select" value={catFilter} onChange={(e) => setCatFilter(e.target.value)} aria-label="Filter by category">
            <option value="all">All categories</option>
            {CATEGORIES.map((c) => (
              <option key={c} value={c}>
                {CATEGORY_LABEL[c]}
              </option>
            ))}
          </select>
        </div>
        <div className="sx-list" role="list" aria-label="Saved memories">
          {list == null ? (
            <div className="sx-list-empty">Loading…</div>
          ) : approved.length === 0 ? (
            <div className="sx-list-empty">{approvedTotal ? 'No memories match the filter.' : 'No memories yet. Add one above or approve a suggestion.'}</div>
          ) : (
            approved.map((m) => <MemoryRow key={`${m.id}:${m.updatedAt}`} m={m} projects={projects} onSave={async (x) => void (await save(x))} onDelete={remove} />)
          )}
        </div>
      </Section>
    </div>
  )
}
