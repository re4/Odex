import { useState } from 'react'
import { ShieldAlert } from 'lucide-react'
import type { ElicitationRequestParams } from '@shared/index'
import { useApp, type ServerRequest } from '@/store/app'
import { Modal } from '@/components/ui'

type JsonSchema = {
  type?: string
  title?: string
  description?: string
  properties?: Record<string, JsonSchema>
  required?: string[]
  enum?: unknown[]
  default?: unknown
  format?: string
}

/** Form generated from an MCP elicitation's (flat) JSON schema. */
function ElicitationModal({ req }: { req: ServerRequest }) {
  const p = req.params as ElicitationRequestParams
  const schema = (p.requestedSchema ?? {}) as JsonSchema
  const props = schema.properties ?? {}
  const [values, setValues] = useState<Record<string, unknown>>(() => Object.fromEntries(Object.entries(props).map(([k, v]) => [k, v.default ?? (v.type === 'boolean' ? false : '')])))
  const resolve = useApp((s) => s.resolveServerRequest)
  const missing = (schema.required ?? []).filter((k) => values[k] === '' || values[k] == null)
  const respond = (action: string) => {
    const content =
      action === 'accept'
        ? Object.fromEntries(
            Object.entries(values).map(([k, v]) => {
              const t = props[k]?.type
              return [k, t === 'number' || t === 'integer' ? Number(v) : v]
            }),
          )
        : null
    void resolve(req.id, { action, content })
  }
  return (
    <Modal
      title={`${p.server} asks for input`}
      onClose={() => respond('cancel')}
      footer={
        <>
          <button className="btn" onClick={() => respond('decline')}>
            Decline
          </button>
          <button className="btn btn-primary" disabled={missing.length > 0} onClick={() => respond('accept')}>
            Submit
          </button>
        </>
      }
    >
      <p className="selectable" style={{ marginTop: 0, whiteSpace: 'pre-wrap' }}>
        {p.message}
      </p>
      <div className="col" style={{ gap: 10 }}>
        {Object.entries(props).map(([k, s]) => (
          <div key={k} className="field">
            <label htmlFor={`el-${k}`}>
              {s.title ?? k}
              {schema.required?.includes(k) ? ' *' : ''}
            </label>
            {s.enum ? (
              <select id={`el-${k}`} className="select" value={String(values[k] ?? '')} onChange={(e) => setValues({ ...values, [k]: e.target.value })}>
                <option value="" />
                {s.enum.map((o) => (
                  <option key={String(o)} value={String(o)}>
                    {String(o)}
                  </option>
                ))}
              </select>
            ) : s.type === 'boolean' ? (
              <label className="checkbox">
                <input id={`el-${k}`} type="checkbox" checked={!!values[k]} onChange={(e) => setValues({ ...values, [k]: e.target.checked })} /> {s.description}
              </label>
            ) : (
              <input
                id={`el-${k}`}
                className="input"
                type={s.type === 'number' || s.type === 'integer' ? 'number' : s.format === 'email' ? 'email' : 'text'}
                value={String(values[k] ?? '')}
                onChange={(e) => setValues({ ...values, [k]: e.target.value })}
              />
            )}
            {s.description && s.type !== 'boolean' && <span className="hint">{s.description}</span>}
          </div>
        ))}
      </div>
    </Modal>
  )
}

/** Approvals for threads that aren't on screen, plus modal server requests. */
export function ServerRequests() {
  const reqs = useApp((s) => s.serverRequests)
  const selected = useApp((s) => s.selectedThreadId)
  const view = useApp((s) => s.ui.view)
  const threads = useApp((s) => s.threads)
  const offscreen = reqs.filter((r) => r.method === 'approval/request' && (r.params.threadId !== selected || view !== 'thread'))
  const elicit = reqs.find((r) => r.method === 'elicitation/request')
  return (
    <>
      {elicit && <ElicitationModal key={elicit.id} req={elicit} />}
      {offscreen.length > 0 && (
        <div className="toasts" style={{ bottom: 70 }} aria-live="polite">
          {offscreen.slice(0, 4).map((r) => {
            const t = threads[r.params.threadId]?.thread
            const a = r.params.approval
            const what = a.kind === 'exec' ? a.command : a.kind === 'patch' ? `${a.changes.length} file edit(s)` : a.kind === 'mcp' ? `${a.server} · ${a.tool}` : a.kind === 'computerUse' ? `${a.app}: ${a.action}` : a.kind === 'download' ? a.filename : a.kind
            return (
              <div key={r.id} className="toast" style={{ display: 'flex', flexDirection: 'column', alignItems: 'stretch', gap: 6, minWidth: 300 }}>
                <div className="row">
                  <ShieldAlert size={14} color="var(--warning)" />
                  <b className="ellipsis grow">{t?.name || t?.preview || 'A thread'} needs approval</b>
                </div>
                <code className="xs ellipsis">{what}</code>
                <div className="row">
                  <button className="btn btn-sm btn-primary" onClick={() => void useApp.getState().resolveServerRequest(r.id, { decision: { type: 'approve' } })}>
                    Approve
                  </button>
                  <button className="btn btn-sm" onClick={() => void useApp.getState().resolveServerRequest(r.id, { decision: { type: 'deny', feedback: null } })}>
                    Deny
                  </button>
                  <span className="spacer" />
                  {r.params.threadId && (
                    <button className="btn btn-sm btn-ghost" onClick={() => void useApp.getState().selectThread(r.params.threadId)}>
                      Open
                    </button>
                  )}
                </div>
              </div>
            )
          })}
          {offscreen.length > 4 && <div className="toast">+{offscreen.length - 4} more waiting</div>}
        </div>
      )}
    </>
  )
}
