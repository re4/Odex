import { useEffect, useMemo, useState } from 'react'
import { Archive, Bell, Check, CheckCheck, CircleAlert, Clock, MessageSquare } from 'lucide-react'
import type { AutomationRun, Thread } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { relativeTime } from '@/components/ui'
import { BACKGROUND_NOTE, RunStatusBadge, RunStatusIcon, ago, archiveRuns, duration, markRunsRead, openRun, refreshRunsStore, threadTitle } from '@/views/AutomationsView'
import '@/styles/automations.css'

type Filter = 'unread' | 'all' | 'archived'

const FILTERS: Array<{ id: Filter; label: string }> = [
  { id: 'unread', label: 'Unread' },
  { id: 'all', label: 'All' },
  { id: 'archived', label: 'Archived' },
]

function attentionReason(t: Thread): { label: string; kind: 'warning' | 'danger' | 'accent' } | null {
  if (t.status === 'waitingApproval') return { label: 'Needs approval', kind: 'warning' }
  if (t.status === 'error') return { label: 'Error', kind: 'danger' }
  if (t.unread) return { label: 'Unread', kind: 'accent' }
  return null
}

function AttentionRow({ t }: { t: Thread }) {
  const project = useApp((s) => s.projects.find((p) => p.id === t.projectId))
  const reason = attentionReason(t)!
  const open = () => void useApp.getState().selectThread(t.id)
  return (
    <li className={`act-item ${t.unread ? 'unread' : ''}`}>
      <span className="act-icon">
        {reason.kind === 'warning' ? (
          <CircleAlert size={14} color="var(--warning)" aria-hidden />
        ) : reason.kind === 'danger' ? (
          <CircleAlert size={14} color="var(--danger)" aria-hidden />
        ) : (
          <span className="dot accent" aria-hidden />
        )}
      </span>
      <button className="act-main" onClick={open} title="Open thread">
        <span className="row" style={{ gap: 6 }}>
          <span className="act-title ellipsis">{threadTitle(t)}</span>
          {t.kind === 'automation' && <Clock size={12} className="subtle" aria-label="automation thread" />}
          <span className={`badge ${reason.kind}`}>{reason.label}</span>
        </span>
        <span className="act-sub ellipsis">
          {project ? `${project.name} · ` : ''}
          {t.status === 'error' && t.lastError ? t.lastError : t.preview || 'No messages yet'}
        </span>
      </button>
      <span className="act-time" title={new Date(t.updatedAt).toLocaleString()}>
        {relativeTime(t.updatedAt)}
      </span>
      <div className="act-actions">
        <button className="btn btn-sm" onClick={open}>
          Open
        </button>
      </div>
    </li>
  )
}

function RunRow({ r }: { r: AutomationRun }) {
  const thread = useApp((s) => (r.threadId ? s.threads[r.threadId]?.thread : undefined))
  const project = useApp((s) => (thread?.projectId ? s.projects.find((p) => p.id === thread.projectId) : undefined))
  const text = r.error || r.summary || (r.status === 'running' ? 'Running…' : 'No output.')
  // where it ran: the woken thread's title, else the project of the new thread
  const where = thread && threadTitle(thread) !== r.automationName ? threadTitle(thread) : project ? project.name : r.threadId ? '' : 'No thread'
  const meta = [where, r.finishedAt ? `${r.status === 'failed' ? 'failed after' : 'took'} ${duration(r)}` : ''].filter(Boolean).join(' · ')
  return (
    <li className={`act-item ${r.unread ? 'unread' : ''}`} aria-label={`${r.automationName} run`}>
      <span className="act-icon">
        <RunStatusIcon status={r.status} />
      </span>
      <button className="act-main" onClick={() => void openRun(r)} title={r.threadId ? 'Open thread' : undefined} disabled={!r.threadId && !r.unread}>
        <span className="row" style={{ gap: 6 }}>
          {r.unread && <span className="dot accent" aria-label="Unread" />}
          <span className="act-title ellipsis">{r.automationName}</span>
          <RunStatusBadge status={r.status} />
          {r.archived && <span className="badge">archived</span>}
        </span>
        <span className={`act-summary ${r.error ? 'error' : ''}`}>{text}</span>
        {meta && <span className="act-sub ellipsis">{meta}</span>}
      </button>
      <span className="act-time" title={new Date(r.startedAt).toLocaleString()}>
        {ago(r.startedAt)}
      </span>
      <div className="act-actions">
        {r.threadId && (
          <button className="btn btn-sm" onClick={() => void openRun(r)}>
            <MessageSquare size={12} /> Open thread
          </button>
        )}
        {r.unread && (
          <button className="icon-btn sm" aria-label="Mark read" title="Mark read" onClick={() => void markRunsRead([r.id])}>
            <Check size={14} />
          </button>
        )}
        {!r.archived && r.status !== 'running' && (
          <button className="icon-btn sm" aria-label="Archive" title="Archive" onClick={() => void archiveRuns([r.id])}>
            <Archive size={13} />
          </button>
        )}
      </div>
    </li>
  )
}

/** Inbox: automation runs to review and threads that need attention. */
export function ActivityView() {
  const runsLive = useApp((s) => s.automationRuns)
  const unreadCount = useApp((s) => s.automationUnread)
  const threadsMap = useApp((s) => s.threads)
  const order = useApp((s) => s.threadOrder)
  const [filter, setFilter] = useState<Filter>('all')
  const [archived, setArchived] = useState<AutomationRun[] | null>(null)

  // refresh the inbox from the engine when the view opens
  useEffect(() => {
    void refreshRunsStore().catch(() => {})
  }, [])

  useEffect(() => {
    if (filter !== 'archived') return
    let cancelled = false
    void call('automation/runs', { unreadOnly: false, includeArchived: true, limit: 200 })
      .then((r) => !cancelled && setArchived(r.runs.filter((x) => x.archived)))
      .catch(() => !cancelled && setArchived([]))
    return () => {
      cancelled = true
    }
  }, [filter, runsLive])

  const attention = useMemo(
    () =>
      order
        .map((id) => threadsMap[id]?.thread)
        .filter((t): t is Thread => !!t && !t.archived && !t.ephemeral && t.kind !== 'subagent' && t.kind !== 'side' && !!attentionReason(t))
        .sort((a, b) => {
          const rank = (t: Thread) => (t.status === 'waitingApproval' ? 0 : t.status === 'error' ? 1 : 2)
          return rank(a) - rank(b) || b.updatedAt - a.updatedAt
        }),
    [order, threadsMap],
  )

  const runs = useMemo(() => {
    if (filter === 'archived') return archived ?? []
    const list = runsLive.filter((r) => !r.archived && (filter === 'all' || r.unread))
    return [...list].sort((a, b) => Number(b.unread) - Number(a.unread) || b.startedAt - a.startedAt)
  }, [filter, runsLive, archived])

  const markAllRead = async () => {
    try {
      const r = await call('automation/runs', { unreadOnly: true, includeArchived: false, limit: 0 })
      await markRunsRead(r.runs.map((x) => x.id))
    } catch (e) {
      toast(`Could not mark all read: ${(e as Error).message}`, 'error')
    }
  }

  const empty = attention.length === 0 && runsLive.length === 0 && filter !== 'archived'

  return (
    <div className="auto-page">
      <div className="auto-inner">
        <header className="auto-header">
          <div className="grow">
            <h1>Activity</h1>
            <p>Automation results to review and threads waiting on you.</p>
          </div>
          <button className="btn" disabled={unreadCount === 0} onClick={() => void markAllRead()}>
            <CheckCheck size={14} /> Mark all read
          </button>
        </header>

        {empty ? (
          <div className="auto-empty card">
            <span className="auto-empty-icon">
              <Bell size={22} />
            </span>
            <h2>You’re all caught up</h2>
            <p>
              Results from automation runs and threads that need your approval show up here.
              <br />
              {BACKGROUND_NOTE}
            </p>
            <button className="btn" onClick={() => useApp.getState().setUi({ view: 'automations' })}>
              <Clock size={14} /> Go to Automations
            </button>
          </div>
        ) : (
          <>
            {attention.length > 0 && (
              <section className="act-section" aria-label="Needs attention">
                <div className="act-section-head">
                  <span className="section-title">Needs attention</span>
                  <span className="badge">{attention.length}</span>
                </div>
                <ul className="act-list">
                  {attention.map((t) => (
                    <AttentionRow key={t.id} t={t} />
                  ))}
                </ul>
              </section>
            )}

            <section className="act-section" aria-label="Automation runs">
              <div className="act-section-head">
                <span className="section-title">Automation runs</span>
                {unreadCount > 0 && <span className="badge accent">{unreadCount} unread</span>}
                <span className="spacer" />
                <div className="seg" role="tablist" aria-label="Filter runs">
                  {FILTERS.map((f) => (
                    <button key={f.id} role="tab" aria-selected={filter === f.id} onClick={() => setFilter(f.id)}>
                      {f.label}
                    </button>
                  ))}
                </div>
              </div>
              {runs.length ? (
                <ul className="act-list">
                  {runs.map((r) => (
                    <RunRow key={r.id} r={r} />
                  ))}
                </ul>
              ) : (
                <div className="act-empty small subtle">
                  {filter === 'unread'
                    ? 'No unread runs.'
                    : filter === 'archived'
                      ? archived === null
                        ? 'Loading…'
                        : 'No archived runs.'
                      : `No automation runs yet. ${BACKGROUND_NOTE}`}
                </div>
              )}
            </section>
          </>
        )}
      </div>
    </div>
  )
}
