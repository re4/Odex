import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { ExternalLink, GitBranch, MoreHorizontal, Pause, Target, X } from 'lucide-react'
import { threadItems, useApp } from '@/store/app'
import { call } from '@/lib/rpc'
import { ItemView } from '@/views/items'
import { ApprovalCard } from '@/views/ApprovalCard'
import { Composer } from '@/views/Composer'
import { Menu } from '@/components/ui'
import { threadMenu } from '@/views/Sidebar'
import * as A from '@/lib/actions'

function GoalRow({ threadId }: { threadId: string }) {
  const goal = useApp((s) => s.threads[threadId]?.thread.goal)
  const [, tick] = useState(0)
  useEffect(() => {
    const t = setInterval(() => tick((x) => x + 1), 1000)
    return () => clearInterval(t)
  }, [])
  if (!goal || goal.status === 'cleared') return null
  const secs = Math.floor((Date.now() - goal.startedAt) / 1000)
  const fmt = `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m ${secs % 60}s`
  return (
    <div className="goal-row" role="status">
      <Target size={14} color="var(--accent)" />
      <span className="ellipsis grow" title={goal.objective}>
        <b>Goal:</b> {goal.objective}
      </span>
      <span className={`badge ${goal.status === 'done' ? 'success' : goal.status === 'active' ? 'accent' : 'warning'}`}>{goal.status}</span>
      <span className="xs subtle">
        {goal.status === 'active' ? fmt : ''} · {goal.turns} turns · {Math.round(goal.tokensUsed / 1000)}k tok
        {goal.timeBudgetSecs ? ` / ${Math.round(goal.timeBudgetSecs / 60)}m` : ''}
      </span>
      {goal.status === 'active' && (
        <button className="icon-btn sm" title="Pause goal (stop the turn)" aria-label="Pause goal" onClick={() => void A.interrupt(threadId)}>
          <Pause size={13} />
        </button>
      )}
      <button className="icon-btn sm" title="Clear goal" aria-label="Clear goal" onClick={() => void call('thread/goal/clear', { threadId })}>
        <X size={13} />
      </button>
    </div>
  )
}

function Queued({ threadId }: { threadId: string }) {
  const queued = useApp((s) => s.threads[threadId]?.queued ?? [])
  if (!queued.length) return null
  const set = (q: typeof queued) => void call('thread/queue/set', { threadId, queued: q })
  return (
    <div className="queued" aria-label="Queued messages">
      {queued.map((q, i) => {
        const text = q.map((c) => (c.type === 'text' ? c.text : `[${c.type}]`)).join(' ')
        return (
          <div key={i} className="queued-item">
            <span className="xs subtle">queued</span>
            <span className="ellipsis grow">{text}</span>
            {i > 0 && (
              <button className="icon-btn sm" aria-label="Move up" onClick={() => set([...queued.slice(0, i - 1), queued[i], queued[i - 1], ...queued.slice(i + 1)])}>
                ↑
              </button>
            )}
            <button
              className="btn btn-sm btn-ghost"
              onClick={async () => {
                const edited = await A.promptText('Edit queued message', text)
                if (edited != null) set(queued.map((x, k) => (k === i ? [{ type: 'text', text: edited }] : x)))
              }}
            >
              Edit
            </button>
            <button
              className="btn btn-sm btn-ghost"
              title="Send now (steer the running turn)"
              onClick={() => {
                set(queued.filter((_, k) => k !== i))
                void call('turn/steer', { threadId, input: q }).catch(() => void A.sendMessage(threadId, q))
              }}
            >
              Send now
            </button>
            <button className="icon-btn sm" aria-label="Delete queued message" onClick={() => set(queued.filter((_, k) => k !== i))}>
              <X size={12} />
            </button>
          </div>
        )
      })}
    </div>
  )
}

function FindBar({ onClose, onQuery }: { onClose: () => void; onQuery: (q: string) => void }) {
  const [q, setQ] = useState('')
  return (
    <div className="find-bar">
      <input
        className="input"
        style={{ height: 26, maxWidth: 300 }}
        autoFocus
        placeholder="Find in thread"
        value={q}
        onChange={(e) => {
          setQ(e.target.value)
          onQuery(e.target.value)
        }}
        onKeyDown={(e) => {
          if (e.key === 'Escape') onClose()
          if (e.key === 'Enter') window.dispatchEvent(new CustomEvent('odex:find-next', { detail: e.shiftKey ? -1 : 1 }))
        }}
      />
      <button className="icon-btn sm" aria-label="Close find" onClick={onClose}>
        <X size={13} />
      </button>
    </div>
  )
}

export function ThreadView() {
  const id = useApp((s) => s.selectedThreadId)
  const ts = useApp((s) => (id ? s.threads[id] : undefined))
  const requests = useApp((s) => s.serverRequests)
  const ui = useApp((s) => s.ui)
  const setUi = useApp((s) => s.setUi)
  const patchThread = useApp((s) => s.patchThread)
  const scrollRef = useRef<HTMLDivElement>(null)
  const stick = useRef(true)
  const [menu, setMenu] = useState<HTMLElement | null>(null)
  const [find, setFind] = useState('')

  const rows = useMemo(() => {
    const r = threadItems(ts).filter(({ item }) => {
      if (item.type === 'agentMessage' && !item.text.trim()) return false
      if (item.type === 'reasoning' && !item.text.trim()) return false
      return true
    })
    if (!find) return r
    const q = find.toLowerCase()
    return r.filter(({ item }) => JSON.stringify(item).toLowerCase().includes(q))
  }, [ts, find])

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 80,
    overscan: 8,
    getItemKey: (i) => rows[i].item.id,
  })

  // restore scroll per thread, else stick to bottom
  useLayoutEffect(() => {
    const el = scrollRef.current
    if (!el || !ts) return
    if (ts.scrollTop != null) {
      el.scrollTop = ts.scrollTop
      stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40
    } else {
      stick.current = true
      requestAnimationFrame(() => virt.scrollToIndex(Math.max(0, rows.length - 1), { align: 'end' }))
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, ts?.loaded])

  useEffect(() => {
    if (stick.current && rows.length) {
      requestAnimationFrame(() => {
        const el = scrollRef.current
        if (el) el.scrollTop = el.scrollHeight
      })
    }
  }, [rows, ts?.turns])

  if (!id || !ts) return null
  const t = ts.thread
  const pending = requests.filter((r) => r.method === 'approval/request' && r.params.threadId === id)
  const running = t.status === 'running' || t.status === 'waitingApproval' || t.status === 'compacting' || t.status === 'reconnecting'

  return (
    <div className="thread-view">
      <div className="thread-header">
        <span className="name ellipsis" title={t.name ?? ''}>
          {t.name || t.preview || 'New thread'}
        </span>
        {t.kind === 'side' && <span className="badge">side chat</span>}
        {t.kind === 'subagent' && <span className="badge">subagent</span>}
        {t.worktree && (
          <span className="badge accent" title={t.worktree.path}>
            <GitBranch size={11} style={{ marginRight: 3 }} /> {t.worktree.branch}
          </span>
        )}
        {!t.worktree && t.branch && (
          <span className="xs subtle row" style={{ gap: 3 }}>
            <GitBranch size={11} /> {t.branch}
          </span>
        )}
        {t.worktree?.setupStatus && t.worktree.setupStatus !== 'ok' && (
          <span className={`badge ${t.worktree.setupStatus === 'running' ? '' : 'danger'}`} title={t.worktree.setupStatus}>
            setup {t.worktree.setupStatus.startsWith('failed') ? 'failed' : t.worktree.setupStatus}
          </span>
        )}
        <span className="spacer" />
        {t.diffStats && t.diffStats.filesChanged > 0 && (
          <button className="chip" onClick={() => setUi({ sidePanelOpen: true, sidePanelTab: 'review' })} title="Open review (Ctrl+Shift+G)">
            {t.diffStats.filesChanged} files <span className="text-add">+{t.diffStats.additions}</span> <span className="text-del">-{t.diffStats.deletions}</span>
          </button>
        )}
        {t.parentThreadId && (
          <button className="btn btn-sm btn-ghost" onClick={() => void useApp.getState().selectThread(t.parentThreadId!)}>
            Parent thread
          </button>
        )}
        <button className="icon-btn" aria-label="Open in new window" title="Pop out" onClick={() => void window.odex.win.newWindow(id)}>
          <ExternalLink size={14} />
        </button>
        <button className="icon-btn" aria-label="Thread actions" onClick={(e) => setMenu(e.currentTarget)}>
          <MoreHorizontal size={15} />
        </button>
        {menu && <Menu anchor={menu} items={threadMenu(t)} align="right" onClose={() => setMenu(null)} />}
      </div>
      {ui.findOpen && (
        <FindBar
          onClose={() => {
            setUi({ findOpen: false })
            setFind('')
          }}
          onQuery={setFind}
        />
      )}
      <div
        className="thread-scroll"
        ref={scrollRef}
        onScroll={(e) => {
          const el = e.currentTarget
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 60
          patchThread(id, { scrollTop: stick.current ? undefined : el.scrollTop })
        }}
        role="log"
        aria-live="polite"
        aria-label="Conversation"
      >
        <div className="thread-inner">
          {!ts.loaded && <div className="empty"><span className="spinner" /> Loading…</div>}
          {ts.loaded && rows.length === 0 && <div className="empty">Send a message to start.</div>}
          <div style={{ height: virt.getTotalSize(), position: 'relative' }}>
            {virt.getVirtualItems().map((v) => {
              const { item, turn } = rows[v.index]
              return (
                <div key={v.key} data-index={v.index} ref={virt.measureElement} style={{ position: 'absolute', top: 0, left: 0, right: 0, transform: `translateY(${v.start}px)` }}>
                  <ItemView item={item} turn={turn} threadId={id} />
                </div>
              )
            })}
          </div>
          {running && pending.length === 0 && (
            <div className="row small muted" style={{ padding: '6px 0' }}>
              <span className="spinner" />
              {t.status === 'compacting' ? 'Compacting context…' : t.status === 'reconnecting' ? 'Reconnecting…' : 'Working…'}
            </div>
          )}
          {pending.map((r) => (
            <ApprovalCard key={r.id} req={r} />
          ))}
        </div>
      </div>
      <div className="composer-area">
        <div className="inner">
          <GoalRow threadId={id} />
          <Queued threadId={id} />
          {!running && ts.followups.length > 0 && (
            <div className="followups" aria-label="Suggested follow-ups">
              {ts.followups.map((f) => (
                <button key={f} className="chip" onClick={() => void A.sendMessage(id, [{ type: 'text', text: f }])}>
                  {f}
                </button>
              ))}
            </div>
          )}
          <Composer threadId={id} />
        </div>
      </div>
    </div>
  )
}
