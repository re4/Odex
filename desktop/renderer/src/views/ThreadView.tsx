import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { AlertTriangle, ChevronDown, ChevronUp, ExternalLink, GitBranch, MoreHorizontal, Pause, Pencil, Pin, Play, Target, X } from 'lucide-react'
import type { Goal, ThreadItem } from '@shared/index'
import { threadItems, useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { ItemView } from '@/views/items'
import { ApprovalCard } from '@/views/ApprovalCard'
import { Composer } from '@/views/Composer'
import { ProjectActions } from '@/views/ProjectActions'
import { openReview } from '@/panels/gitShared'
import { PrBadge } from '@/panels/PrBadge'
import { Menu, Modal } from '@/components/ui'
import { threadMenu } from '@/views/Sidebar'
import * as A from '@/lib/actions'
import '@/styles/thread-nav.css'

const GOAL_BADGE: Record<string, string> = { active: 'accent', paused: 'warning', done: 'success', blocked: 'danger', budgetExhausted: 'warning' }
const GOAL_LABEL: Record<string, string> = { budgetExhausted: 'budget used up' }

function goalElapsed(goal: Goal): number | null {
  if (goal.status === 'active') return Math.max(0, Math.floor((Date.now() - goal.startedAt) / 1000))
  if (goal.status === 'paused' && goal.pausedAt) return Math.max(0, Math.floor((goal.pausedAt - goal.startedAt) / 1000))
  return null
}

function GoalRow({ threadId }: { threadId: string }) {
  const goal = useApp((s) => s.threads[threadId]?.thread.goal)
  const [editing, setEditing] = useState(false)
  const [, tick] = useState(0)
  const active = goal?.status === 'active'
  useEffect(() => {
    if (!active) return
    const t = setInterval(() => tick((x) => x + 1), 1000)
    return () => clearInterval(t)
  }, [active])
  if (!goal || goal.status === 'cleared') return null
  const secs = goalElapsed(goal)
  const run = (m: 'thread/goal/pause' | 'thread/goal/resume' | 'thread/goal/clear') => void call(m, { threadId }).catch((e: Error) => toast(e.message, 'error'))
  const parts = [
    secs != null ? `${A.formatDuration(secs)}${goal.timeBudgetSecs ? ` / ${A.formatDuration(goal.timeBudgetSecs)}` : ''}` : goal.timeBudgetSecs ? `${A.formatDuration(goal.timeBudgetSecs)} budget` : '',
    `${goal.turns} ${goal.turns === 1 ? 'turn' : 'turns'}`,
    `${A.formatTokenCount(goal.tokensUsed)}${goal.tokenBudget ? ` / ${A.formatTokenCount(goal.tokenBudget)}` : ''} tok`,
  ].filter(Boolean)
  return (
    <div className={`goal-row ${goal.status}`} role="status" aria-label="Goal">
      <Target size={14} color="var(--accent)" />
      <span className="ellipsis grow" title={goal.lastUpdate ? `${goal.objective}\n\nLast update: ${goal.lastUpdate}` : goal.objective}>
        <b>Goal:</b> {goal.objective}
      </span>
      <span className={`badge ${GOAL_BADGE[goal.status] ?? ''}`}>{GOAL_LABEL[goal.status] ?? goal.status}</span>
      <span className="xs subtle" style={{ whiteSpace: 'nowrap' }}>
        {parts.join(' · ')}
      </span>
      <span className="goal-actions">
        {goal.status === 'active' && (
          <button className="icon-btn sm" title="Pause goal (stops the turn; resume later)" aria-label="Pause goal" onClick={() => run('thread/goal/pause')}>
            <Pause size={13} />
          </button>
        )}
        {(goal.status === 'paused' || goal.status === 'blocked') && (
          <button className="icon-btn sm" title="Resume goal" aria-label="Resume goal" onClick={() => run('thread/goal/resume')}>
            <Play size={13} />
          </button>
        )}
        <button className="icon-btn sm" title="Edit goal and budgets" aria-label="Edit goal" onClick={() => setEditing(true)}>
          <Pencil size={13} />
        </button>
        <button className="icon-btn sm" title="Clear goal" aria-label="Clear goal" onClick={() => run('thread/goal/clear')}>
          <X size={13} />
        </button>
      </span>
      {editing && <GoalEditor threadId={threadId} goal={goal} onClose={() => setEditing(false)} />}
    </div>
  )
}

function GoalEditor({ threadId, goal, onClose }: { threadId: string; goal: Goal; onClose: () => void }) {
  const [objective, setObjective] = useState(goal.objective)
  const [time, setTime] = useState(goal.timeBudgetSecs ? A.formatDuration(goal.timeBudgetSecs) : '')
  const [tokens, setTokens] = useState(goal.tokenBudget ? A.formatTokenCount(goal.tokenBudget) : '')
  const [busy, setBusy] = useState(false)
  const timeSecs = time.trim() ? A.parseDuration(time) : null
  const tokenN = tokens.trim() ? A.parseTokenCount(tokens) : null
  const timeBad = !!time.trim() && timeSecs === null
  const tokensBad = !!tokens.trim() && tokenN === null
  const reopens = goal.status === 'done' || goal.status === 'blocked' || goal.status === 'budgetExhausted'
  const save = async () => {
    if (!objective.trim() || timeBad || tokensBad) return
    setBusy(true)
    try {
      await call('thread/goal/set', { threadId, objective: objective.trim(), timeBudgetSecs: timeSecs, tokenBudget: tokenN, edit: true })
      onClose()
    } catch (e) {
      toast((e as Error).message, 'error')
    } finally {
      setBusy(false)
    }
  }
  const secs = goalElapsed(goal)
  return (
    <Modal
      title="Edit goal"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" disabled={busy || !objective.trim() || timeBad || tokensBad} onClick={() => void save()}>
            {reopens ? 'Save and resume' : 'Save'}
          </button>
        </>
      }
    >
      <div className="goal-form" onKeyDown={(e) => e.key === 'Enter' && (e.ctrlKey || e.metaKey) && void save()}>
        <div className="field span2">
          <label htmlFor="goal-objective">Objective</label>
          <textarea id="goal-objective" className="textarea" style={{ minHeight: 80 }} value={objective} onChange={(e) => setObjective(e.target.value)} aria-label="Goal objective" />
        </div>
        <div className="field">
          <label htmlFor="goal-time">Time budget</label>
          <input id="goal-time" className="input" placeholder="none (e.g. 30m, 1h30m)" value={time} onChange={(e) => setTime(e.target.value)} aria-label="Time budget" aria-invalid={timeBad} />
          {timeBad ? <span className="err">Use a duration like 30m, 1h30m or 90s</span> : <span className="hint">Paused time does not count.</span>}
        </div>
        <div className="field">
          <label htmlFor="goal-tokens">Token budget</label>
          <input id="goal-tokens" className="input" placeholder="none (e.g. 200k, 1.5m)" value={tokens} onChange={(e) => setTokens(e.target.value)} aria-label="Token budget" aria-invalid={tokensBad} />
          {tokensBad ? <span className="err">Use a count like 200k or 1.5m</span> : <span className="hint">Total tokens across goal turns.</span>}
        </div>
        <div className="goal-progress span2">
          <span>Status: {GOAL_LABEL[goal.status] ?? goal.status}</span>
          {secs != null && <span>Elapsed: {A.formatDuration(secs)}</span>}
          <span>Turns: {goal.turns}</span>
          <span>Tokens used: {A.formatTokenCount(goal.tokensUsed)}</span>
        </div>
        {reopens && <div className="hint span2">Saving reopens the goal and the agent continues working on it.</div>}
      </div>
    </Modal>
  )
}

/** Warn when the thread's model changed under it (engine check) or is not served right now. */
function ModelWarning({ threadId }: { threadId: string }) {
  const thread = useApp((s) => s.threads[threadId]?.thread)
  const models = useApp((s) => s.models)
  const roles = useApp((s) => s.roles)
  const providers = useApp((s) => s.providers)
  const [dismissed, setDismissed] = useState<number | null>(null)
  if (!thread) return null
  const w = thread.modelWarning
  const switchModel = () => window.dispatchEvent(new CustomEvent('odex:open-picker', { detail: 'model' }))
  if (w && dismissed !== w.at) {
    const alt = w.servedModel ? models.find((m) => m.available && (m.modelId === w.servedModel || m.key === w.servedModel)) : undefined
    return (
      <div className="banner model-warning" role="alert" aria-label="Model warning">
        <AlertTriangle size={14} className="mw-icon" />
        <span className="grow small selectable">
          <b>{w.code === 'windowShrank' ? 'Context window shrank' : w.code === 'modelChanged' ? 'Model changed' : 'Model not served'}.</b> {w.message}
        </span>
        <span className="mw-actions">
          {alt && w.code === 'notServed' && (
            <button className="btn btn-sm" onClick={() => void call('thread/update', { threadId, model: alt.key }).catch((e: Error) => toast(e.message, 'error'))}>
              Use {alt.displayName || alt.modelId}
            </button>
          )}
          <button className="btn btn-sm" onClick={switchModel}>
            Switch model
          </button>
          <button className="btn btn-sm btn-ghost" onClick={() => setDismissed(w.at)}>
            Dismiss
          </button>
        </span>
      </div>
    )
  }
  const key = thread.model ?? roles.main ?? null
  if (!key || providers.length === 0) return null
  const m = models.find((x) => x.key === key || x.modelId === key)
  const prov = providers.find((p) => p.id === (m?.providerId ?? key.split(':')[0]))
  if (m?.available && prov?.health !== 'unreachable') return null
  const why = !prov ? 'its endpoint is not configured' : prov.health === 'unreachable' ? `${prov.name} is unreachable` : 'the endpoint does not serve it'
  return (
    <div className="banner info" style={{ borderRadius: 'var(--radius)', marginBottom: 8 }} role="status">
      <span className="grow small">
        Model <b>{m?.displayName ?? key}</b> is unavailable: {why}.
      </span>
      <button className="btn btn-sm" onClick={switchModel}>
        Switch model
      </button>
      <button className="btn btn-sm btn-ghost" onClick={() => void useApp.getState().refreshModels(true)}>
        Retry
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

function FindBar({ onClose, onQuery, count, index }: { onClose: () => void; onQuery: (q: string) => void; count: number; index: number }) {
  const [q, setQ] = useState('')
  const step = (d: number) => window.dispatchEvent(new CustomEvent('odex:find-next', { detail: d }))
  return (
    <div className="find-bar" role="search">
      <input
        className="input"
        style={{ height: 26, maxWidth: 300 }}
        autoFocus
        placeholder="Find in thread"
        aria-label="Find in thread"
        value={q}
        onChange={(e) => {
          setQ(e.target.value)
          onQuery(e.target.value)
        }}
        onKeyDown={(e) => {
          if (e.key === 'Escape') onClose()
          if (e.key === 'Enter') step(e.shiftKey ? -1 : 1)
        }}
      />
      <span className="xs subtle" style={{ minWidth: 54 }} aria-live="polite">
        {q ? (count ? `${index + 1} of ${count}` : 'No results') : ''}
      </span>
      <button className="icon-btn sm" aria-label="Previous match" title="Previous (Shift+Enter)" disabled={!count} onClick={() => step(-1)}>
        <ChevronUp size={13} />
      </button>
      <button className="icon-btn sm" aria-label="Next match" title="Next (Enter)" disabled={!count} onClick={() => step(1)}>
        <ChevronDown size={13} />
      </button>
      <button className="icon-btn sm" aria-label="Close find" onClick={onClose}>
        <X size={13} />
      </button>
    </div>
  )
}

/** Searchable text of an item (what the user sees, not ids). */
function itemText(item: ThreadItem): string {
  switch (item.type) {
    case 'userMessage':
      return item.content.map((c) => (c.type === 'text' ? c.text : '')).join(' ')
    case 'agentMessage':
    case 'reasoning':
      return item.text
    case 'commandExecution':
      return `${item.command}\n${item.output}`
    case 'fileChange':
      return item.changes.map((c) => c.path).join(' ')
    case 'toolCall':
      return `${item.tool} ${item.summary ?? ''}`
    case 'mcpToolCall':
      return `${item.server} ${item.tool}`
    case 'plan':
      return item.steps.map((p) => p.step).join(' ')
    case 'proposedPlan':
      return item.markdown
    case 'review':
      return `${item.summary} ${item.findings.map((f) => `${f.title} ${f.body}`).join(' ')}`
    case 'notice':
    case 'error':
      return item.message
    default:
      return ''
  }
}

/** Highlight every occurrence of `q` under `root` (CSS Custom Highlight API). */
function paintHighlights(root: HTMLElement | null, q: string, currentRow: number | null): void {
  const reg = (CSS as unknown as { highlights?: Map<string, unknown> }).highlights
  if (!reg) return
  reg.delete('odex-find')
  reg.delete('odex-find-current')
  if (!root || !q) return
  const needle = q.toLowerCase()
  const all: Range[] = []
  const current: Range[] = []
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT)
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const text = (n.nodeValue ?? '').toLowerCase()
    let i = text.indexOf(needle)
    if (i < 0) continue
    const row = (n.parentElement?.closest('[data-index]') as HTMLElement | null)?.dataset.index
    while (i >= 0) {
      const r = new Range()
      r.setStart(n, i)
      r.setEnd(n, i + needle.length)
      if (currentRow != null && row === String(currentRow)) current.push(r)
      else all.push(r)
      i = text.indexOf(needle, i + needle.length)
    }
  }
  const H = (window as unknown as { Highlight: new (...r: Range[]) => unknown }).Highlight
  reg.set('odex-find', new H(...all))
  reg.set('odex-find-current', new H(...current))
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
  const [onTop, setOnTop] = useState(false)
  const [findIdx, setFindIdx] = useState(0)

  const rows = useMemo(() => {
    // a plan turn's final message is shown by its proposed-plan card instead
    const planTexts = new Set<string>()
    for (const t of ts?.turns ?? []) for (const i of t.items) if (i.type === 'proposedPlan') planTexts.add(i.markdown.trim())
    // a review turn's structured reply is shown by its review card instead
    const reviewTurns = new Set((ts?.turns ?? []).filter((t) => t.mode === 'review' && t.items.some((i) => i.type === 'review')).map((t) => t.id))
    const r = threadItems(ts).filter(({ item, turn }) => {
      if (item.type === 'agentMessage' && (!item.text.trim() || planTexts.has(item.text.trim()) || reviewTurns.has(turn.id))) return false
      if (item.type === 'reasoning' && !item.text.trim()) return false
      return true
    })
    return r
  }, [ts])

  const matches = useMemo(() => {
    if (!find) return [] as number[]
    const q = find.toLowerCase()
    const out: number[] = []
    rows.forEach(({ item }, i) => {
      if (itemText(item).toLowerCase().includes(q)) out.push(i)
    })
    return out
  }, [rows, find])

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

  // find: jump between matching rows and highlight the text
  useEffect(() => setFindIdx(0), [find])
  useEffect(() => {
    const onNext = (e: Event) => {
      if (!matches.length) return
      const d = (e as CustomEvent<number>).detail ?? 1
      setFindIdx((i) => (i + d + matches.length) % matches.length)
    }
    window.addEventListener('odex:find-next', onNext)
    return () => window.removeEventListener('odex:find-next', onNext)
  }, [matches])
  const currentRow = matches.length ? matches[Math.min(findIdx, matches.length - 1)] : null
  useEffect(() => {
    if (currentRow != null) {
      stick.current = false
      virt.scrollToIndex(currentRow, { align: 'center' })
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentRow])
  useEffect(() => {
    let raf2 = 0
    // paint after the virtualizer has rendered the target rows
    const raf = requestAnimationFrame(() => {
      raf2 = requestAnimationFrame(() => paintHighlights(ui.findOpen ? scrollRef.current : null, find, currentRow))
    })
    return () => {
      cancelAnimationFrame(raf)
      cancelAnimationFrame(raf2)
    }
  })
  useEffect(() => () => paintHighlights(null, '', null), [])

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
        {t.pr && <PrBadge pr={t.pr} onClick={() => setUi({ sidePanelOpen: true, sidePanelTab: 'git' })} />}
        <span className="spacer" />
        <ProjectActions projectId={t.projectId} threadId={t.id} cwd={t.worktree?.path ?? t.cwd} />
        {t.diffStats && t.diffStats.filesChanged > 0 && (
          <button className="chip" onClick={() => openReview()} title="Open review (Ctrl+Shift+G)">
            {t.diffStats.filesChanged} {t.diffStats.filesChanged === 1 ? 'file' : 'files'} <span className="text-add">+{t.diffStats.additions}</span> <span className="text-del">-{t.diffStats.deletions}</span>
          </button>
        )}
        {t.parentThreadId && (
          <button className="btn btn-sm btn-ghost" onClick={() => void useApp.getState().selectThread(t.parentThreadId!)}>
            Parent thread
          </button>
        )}
        {ui.popout ? (
          <button
            className={`icon-btn ${onTop ? 'active' : ''}`}
            aria-label="Keep window on top"
            aria-pressed={onTop}
            title="Always on top"
            onClick={() => {
              void window.odex.win.alwaysOnTop(!onTop)
              setOnTop(!onTop)
            }}
          >
            <Pin size={14} />
          </button>
        ) : (
          <button className="icon-btn" aria-label="Open in new window" title="Pop out" onClick={() => void window.odex.win.newWindow(id)}>
            <ExternalLink size={14} />
          </button>
        )}
        <button className="icon-btn" aria-label="Thread actions" onClick={(e) => setMenu(e.currentTarget)}>
          <MoreHorizontal size={15} />
        </button>
        {menu && <Menu anchor={menu} items={threadMenu(t)} align="right" onClose={() => setMenu(null)} />}
      </div>
      {ui.findOpen && (
        <FindBar
          count={matches.length}
          index={Math.min(findIdx, Math.max(0, matches.length - 1))}
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
          <ModelWarning threadId={id} />
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
