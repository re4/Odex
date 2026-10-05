import { CheckCircle2, Circle, CircleDot } from 'lucide-react'
import { useApp } from '@/store/app'

export function PlanPanel() {
  const ts = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId] : undefined))
  if (!ts) return <div className="empty">Open a thread to see its plan.</div>
  if (!ts.plan.length) return <div className="empty">No plan yet. The agent publishes one for multi-step work.</div>
  const done = ts.plan.filter((p) => p.status === 'completed').length
  return (
    <div style={{ padding: 12 }}>
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
  )
}
