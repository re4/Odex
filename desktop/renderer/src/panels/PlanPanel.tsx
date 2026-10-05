import { useEffect, useState } from 'react'
import { CheckCircle2, Circle, CircleDot, ScrollText } from 'lucide-react'
import type { ContextSummary } from '@shared/index'
import { useApp } from '@/store/app'
import { call } from '@/lib/rpc'
import { relativeTime } from '@/components/ui'
import { openFileInPanel } from '@/views/items'
import '@/styles/sidepanel.css'

/** The thread's latest compaction summary (refetched after each compaction). */
function useSummary(threadId: string | undefined, compactions: number): ContextSummary | null {
  const [summary, setSummary] = useState<{ id: string; s: ContextSummary | null } | null>(null)
  useEffect(() => {
    if (!threadId) return
    let cancelled = false
    void call('thread/context', { threadId })
      .then((r) => !cancelled && setSummary({ id: threadId, s: r.summary ?? null }))
      .catch(() => !cancelled && setSummary({ id: threadId, s: null }))
    return () => {
      cancelled = true
    }
  }, [threadId, compactions])
  return summary && summary.id === threadId ? summary.s : null
}

function SummaryCard({ s, cwd }: { s: ContextSummary; cwd: string }) {
  const open = (p: string) => openFileInPanel(/^([a-zA-Z]:[\\/]|[\\/])/.test(p) ? p : `${cwd.replace(/[\\/]+$/, '')}/${p}`)
  return (
    <section className="summary-card" aria-label="Thread summary">
      <div className="summary-head">
        <ScrollText size={14} color="var(--accent)" />
        <b className="grow">Summary</b>
        <span className="xs subtle" title={s.llm ? 'Written by the compactor model' : 'Extractive summary (the compactor model was unavailable)'}>
          #{s.number} · {s.llm ? 'model summary' : 'extractive'}
          {s.at ? ` · ${relativeTime(s.at)}` : ''}
        </span>
      </div>
      {s.goalAndRequirements.length > 0 && (
        <>
          <h4>Goal &amp; requirements</h4>
          <ul className="selectable">
            {s.goalAndRequirements.map((g, i) => (
              <li key={i}>{g}</li>
            ))}
          </ul>
        </>
      )}
      {s.decisions.length > 0 && (
        <>
          <h4>Decisions</h4>
          <ul className="selectable">
            {s.decisions.map((d, i) => (
              <li key={i}>
                {d.decision}
                {d.reason && <span className="subtle"> — {d.reason}</span>}
              </li>
            ))}
          </ul>
        </>
      )}
      {s.filesChanged.length > 0 && (
        <>
          <h4>Files changed</h4>
          <ul>
            {s.filesChanged.map((f, i) => (
              <li key={i}>
                <button className="summary-file" title={`Open ${f.path}`} onClick={() => open(f.path)}>
                  {f.path}
                </button>{' '}
                <span className="small selectable">
                  {f.purpose}
                  {f.state && <span className="subtle"> ({f.state})</span>}
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
      {s.openErrors.length > 0 && (
        <>
          <h4>Open errors</h4>
          <ul className="selectable">
            {s.openErrors.map((e, i) => (
              <li key={i}>{e}</li>
            ))}
          </ul>
        </>
      )}
      {s.nextSteps.length > 0 && (
        <>
          <h4>Next steps</h4>
          <ul className="selectable">
            {s.nextSteps.map((n, i) => (
              <li key={i}>{n}</li>
            ))}
          </ul>
        </>
      )}
    </section>
  )
}

export function PlanPanel() {
  const ts = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId] : undefined))
  const summary = useSummary(ts?.thread.id, ts?.context?.compactions.length ?? 0)
  if (!ts) return <div className="empty">Open a thread to see its plan.</div>
  const done = ts.plan.filter((p) => p.status === 'completed').length
  const card = summary && <SummaryCard s={summary} cwd={ts.thread.worktree?.path ?? ts.thread.cwd} />
  if (!ts.plan.length)
    return (
      <div style={{ overflow: 'auto' }}>
        {card}
        <div className="empty">No plan yet. The agent publishes one for multi-step work.</div>
      </div>
    )
  return (
    <div style={{ overflow: 'auto' }}>
      <div className="plan-section" style={{ padding: 12 }}>
        <div className="row" style={{ marginBottom: 8 }}>
          <b className="grow">Plan</b>
          <span className="xs subtle">
            {done}/{ts.plan.length} done
          </span>
        </div>
        {ts.planExplanation && <p className="small muted selectable" style={{ marginTop: 0 }}>{ts.planExplanation}</p>}
        <ul className="plan-list">
          {ts.plan.map((p, i) => (
            <li key={i} className={p.status}>
              {p.status === 'completed' ? <CheckCircle2 size={14} color="var(--success)" /> : p.status === 'inProgress' ? <CircleDot size={14} color="var(--accent)" /> : <Circle size={14} color="var(--fg-subtle)" />}
              <span className="selectable">{p.step}</span>
            </li>
          ))}
        </ul>
      </div>
      {card}
    </div>
  )
}
