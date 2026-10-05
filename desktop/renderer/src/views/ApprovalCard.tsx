import { useEffect, useRef, useState } from 'react'
import { ShieldAlert } from 'lucide-react'
import type { ApprovalDecision, ApprovalRequestParams } from '@shared/index'
import { useApp, type ServerRequest } from '@/store/app'
import { MiniDiff } from '@/views/items'

function describe(p: ApprovalRequestParams): { title: string; detail: React.ReactNode } {
  const a = p.approval
  switch (a.kind) {
    case 'exec': {
      const why: Record<string, string> = {
        escalation: 'The agent asks to run this outside the sandbox',
        network: 'This command probably needs network access',
        policy: 'A command rule requires approval',
        readOnly: 'Read-only mode: this command may change files',
        sandboxUnavailable: 'The sandbox is unavailable, so this would run unsandboxed',
        sandboxFailure: 'The command failed inside the sandbox; retry without it?',
      }
      return {
        title: why[a.reason] ?? 'Run command?',
        detail: (
          <>
            <pre className="selectable" style={{ margin: '4px 0', padding: '8px 10px', background: 'var(--code-bg)', borderRadius: 6, whiteSpace: 'pre-wrap', wordBreak: 'break-all' }}>
              {a.command}
            </pre>
            <div className="xs subtle">in {a.cwd}</div>
            {a.justification && <div className="small" style={{ marginTop: 4 }}>“{a.justification}”</div>}
            {a.sandboxOutput && (
              <pre className="xs selectable" style={{ maxHeight: 120, overflow: 'auto', background: 'var(--code-bg)', padding: 6, borderRadius: 6 }}>
                {a.sandboxOutput}
              </pre>
            )}
          </>
        ),
      }
    }
    case 'patch':
      return {
        title: a.reason === 'readOnly' ? 'Allow these edits? (read-only mode)' : 'Allow edits outside the workspace?',
        detail: (
          <>
            {a.outsideWorkspace.length > 0 && <div className="small" style={{ color: 'var(--warning)' }}>Outside the workspace: {a.outsideWorkspace.join(', ')}</div>}
            {a.changes.map((c) => (
              <div key={c.path} className="cell" style={{ marginTop: 6 }}>
                <div className="cell-head" style={{ cursor: 'default' }}>
                  {c.path} <span className="spacer" />
                  <span className="xs text-add">+{c.additions}</span> <span className="xs text-del">-{c.deletions}</span>
                </div>
                <div className="cell-body" style={{ maxHeight: 220 }}>
                  <MiniDiff diff={c.diff} maxLines={120} />
                </div>
              </div>
            ))}
          </>
        ),
      }
    case 'mcp':
      return {
        title: `Call MCP tool ${a.server} · ${a.tool}?`,
        detail: (
          <>
            {a.description && <div className="small muted">{a.description}</div>}
            <pre className="selectable xs" style={{ margin: '4px 0', padding: 8, background: 'var(--code-bg)', borderRadius: 6, maxHeight: 200, overflow: 'auto' }}>
              {JSON.stringify(a.arguments, null, 2)}
            </pre>
          </>
        ),
      }
    case 'computerUse':
      return {
        title: `Control ${a.app}: ${a.action}?`,
        detail: (
          <>
            <pre className="selectable xs" style={{ margin: '4px 0', padding: 8, background: 'var(--code-bg)', borderRadius: 6 }}>
              {JSON.stringify(a.arguments, null, 2)}
            </pre>
            {a.screenshot && <img src={a.screenshot} alt="window" style={{ maxWidth: '100%', maxHeight: 200, borderRadius: 6, border: '1px solid var(--border)' }} />}
          </>
        ),
      }
    case 'browser':
      return { title: `Let the agent use ${a.site}?`, detail: <div className="small">Action: {a.action}</div> }
    case 'download':
      return { title: `Download ${a.filename}?`, detail: <div className="small selectable">{a.url}</div> }
    case 'hook':
      return { title: 'Run hook?', detail: <pre className="xs">{a.command}</pre> }
  }
}

export function ApprovalCard({ req }: { req: ServerRequest }) {
  const p = req.params as ApprovalRequestParams
  const resolve = useApp((s) => s.resolveServerRequest)
  const [feedback, setFeedback] = useState('')
  const [showFeedback, setShowFeedback] = useState(false)
  const [command, setCommand] = useState(p.approval.kind === 'exec' ? p.approval.command : '')
  const ref = useRef<HTMLDivElement>(null)
  const { title, detail } = describe(p)
  const decide = (decision: ApprovalDecision) => void resolve(req.id, { decision })

  useEffect(() => {
    ref.current?.focus()
  }, [])

  const sessionLabel =
    p.approval.kind === 'exec'
      ? `Always allow \`${p.approval.prefix.join(' ') || 'this'}\` this session`
      : p.approval.kind === 'mcp'
        ? "Don't ask again for this tool"
        : p.approval.kind === 'computerUse'
          ? `Always allow ${p.approval.app} this session`
          : p.approval.kind === 'browser'
            ? `Allow ${p.approval.site} this session`
            : 'Approve for this session'

  return (
    <div
      className="approval"
      ref={ref}
      tabIndex={-1}
      role="alertdialog"
      aria-label={title}
      onKeyDown={(e) => {
        if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
          e.preventDefault()
          decide({ type: 'custom', feedback: feedback || null, command: p.approval.kind === 'exec' && command !== p.approval.command ? command : null })
        } else if (e.key === 'Enter' && (e.target as HTMLElement).tagName !== 'TEXTAREA' && (e.target as HTMLElement).tagName !== 'INPUT') {
          e.preventDefault()
          decide({ type: 'approve' })
        } else if (e.key === 'Escape') {
          e.preventDefault()
          decide({ type: 'deny', feedback: feedback || null })
        }
      }}
    >
      <div className="approval-head">
        <ShieldAlert size={15} color="var(--warning)" />
        <span className="grow">{title}</span>
        {p.autoReview && <span className="badge warning" title={p.autoReview.reason}>auto-review: {p.autoReview.risk} risk</span>}
      </div>
      <div className="approval-body">
        {detail}
        {p.autoReview?.reason && <div className="xs subtle" style={{ marginTop: 4 }}>Reviewer: {p.autoReview.reason}</div>}
        {showFeedback && (
          <div className="col" style={{ marginTop: 8 }}>
            {p.approval.kind === 'exec' && <input className="input mono" value={command} onChange={(e) => setCommand(e.target.value)} aria-label="Command to run" />}
            <textarea className="textarea" style={{ minHeight: 50 }} placeholder="Instructions for the agent (optional)…" value={feedback} onChange={(e) => setFeedback(e.target.value)} />
          </div>
        )}
      </div>
      <div className="approval-actions">
        <button className="btn btn-primary btn-sm" onClick={() => decide({ type: 'approve' })} title="Enter">
          Approve
        </button>
        <button className="btn btn-sm" onClick={() => decide({ type: 'approveForSession' })}>
          {sessionLabel}
        </button>
        <button className="btn btn-sm" onClick={() => decide({ type: 'deny', feedback: feedback || null })} title="Esc">
          Deny
        </button>
        {showFeedback ? (
          <button
            className="btn btn-sm"
            onClick={() => decide({ type: 'custom', feedback: feedback || null, command: p.approval.kind === 'exec' && command !== p.approval.command ? command : null })}
            title="Ctrl+Enter"
          >
            Approve with changes
          </button>
        ) : (
          <button className="btn btn-ghost btn-sm" onClick={() => setShowFeedback(true)}>
            Custom…
          </button>
        )}
        <span className="spacer" />
        <button className="btn btn-ghost btn-sm" onClick={() => decide({ type: 'abort' })}>
          Stop turn
        </button>
      </div>
    </div>
  )
}
