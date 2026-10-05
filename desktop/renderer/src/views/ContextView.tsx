import { useEffect, useState } from 'react'
import { Copy } from 'lucide-react'
import type { ContextStatus, Thread } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { Modal, formatTokens } from '@/components/ui'
import * as A from '@/lib/actions'
import '@/styles/thread-nav.css'

const CATS: Array<[keyof ContextStatus['breakdown'], string, string]> = [
  ['system', 'System prompt', '#64748b'],
  ['tools', 'Tool schemas', '#7c3aed'],
  ['agentsMd', 'AGENTS.md', '#0d9488'],
  ['memories', 'Memories', '#16a34a'],
  ['summary', 'Summaries', '#ea580c'],
  ['pinned', 'Pinned', '#db2777'],
  ['history', 'Conversation', '#2f6feb'],
  ['toolOutputs', 'Tool output', '#ca8a04'],
  ['images', 'Images', '#0891b2'],
]

/** `/status` header: thread id, model, endpoint, server version, token usage. */
function StatusHeader({ thread, ctxModel }: { thread: Thread; ctxModel?: string | null }) {
  const models = useApp((s) => s.models)
  const roles = useApp((s) => s.roles)
  const providers = useApp((s) => s.providers)
  const key = ctxModel ?? thread.model ?? roles.main ?? null
  const m = key ? models.find((x) => x.key === key || x.modelId === key) : undefined
  const prov = providers.find((p) => p.id === (m?.providerId ?? key?.split(':')[0]))
  const u = thread.usage
  return (
    <dl className="ctx-status" aria-label="Thread status">
      <dt>Thread</dt>
      <dd>
        <span className="mono selectable" title={thread.id}>
          {thread.id}
        </span>
        <button className="icon-btn sm" aria-label="Copy thread id" title="Copy thread id" onClick={() => A.copy(thread.id, 'Thread id copied')}>
          <Copy size={12} />
        </button>
      </dd>
      <dt>Model</dt>
      <dd>
        <span className="selectable">{m?.displayName && m.displayName !== m.modelId ? `${m.displayName} (${m.modelId})` : (m?.modelId ?? key ?? 'none')}</span>
        {thread.model ? <span className="badge">thread</span> : <span className="badge">default</span>}
        {m && !m.available && <span className="badge warning">not served</span>}
      </dd>
      <dt>Endpoint</dt>
      <dd>
        {prov ? (
          <>
            <span className="mono selectable" title={prov.baseUrl}>
              {prov.baseUrl}
            </span>
            <span className={`badge ${prov.health === 'healthy' ? 'success' : prov.health === 'unreachable' ? 'danger' : prov.health === 'degraded' ? 'warning' : ''}`}>{prov.health}</span>
          </>
        ) : (
          <span className="muted">not configured</span>
        )}
      </dd>
      <dt>Server</dt>
      <dd className="selectable">{prov ? `${prov.name}${prov.version ? ` · vLLM ${prov.version}` : ' · version unknown'}` : '—'}</dd>
      <dt>Tokens used</dt>
      <dd>
        <span className="ctx-usage" aria-label="Token usage">
          <span>
            input <b>{formatTokens(u.inputTokens)}</b>
          </span>
          <span>
            cached <b>{formatTokens(u.cachedInputTokens)}</b>
          </span>
          <span>
            output <b>{formatTokens(u.outputTokens)}</b>
          </span>
          {u.reasoningTokens > 0 && (
            <span>
              reasoning <b>{formatTokens(u.reasoningTokens)}</b>
            </span>
          )}
          <span className="subtle">total {formatTokens(u.totalTokens)}</span>
        </span>
      </dd>
    </dl>
  )
}

/** Context engine status: usage bar, breakdown, thresholds, compaction log. */
export function ContextView() {
  const id = useApp((s) => s.selectedThreadId)
  const live = useApp((s) => (id ? s.threads[id]?.context : undefined))
  const thread = useApp((s) => (id ? s.threads[id]?.thread : undefined))
  const setUi = useApp((s) => s.setUi)
  const [ctx, setCtx] = useState<ContextStatus | undefined>(live)
  const [focus, setFocus] = useState('')

  useEffect(() => {
    if (!id) return
    void call('thread/context', { threadId: id })
      .then((r) => setCtx(r.context))
      .catch(() => {})
  }, [id])
  useEffect(() => {
    if (live) setCtx(live)
  }, [live])

  const close = () => setUi({ contextViewOpen: false })
  if (!id || !ctx) {
    return (
      <Modal title="Status" onClose={close}>
        {thread ? <StatusHeader thread={thread} /> : <div className="muted">Open a thread to see its status and context.</div>}
      </Modal>
    )
  }
  const total = Math.max(1, ctx.window)
  const b = ctx.breakdown
  const markers: Array<[number, string]> = [
    [(ctx.budget * ctx.pruneAt) / total, 'prune'],
    [(ctx.budget * ctx.compactAt) / total, 'compact'],
    [ctx.budget / total, 'budget'],
  ]
  return (
    <Modal
      title="Status & context"
      onClose={close}
      wide
      footer={
        <>
          <input className="input" style={{ maxWidth: 320 }} placeholder="Focus for the summary (optional)" value={focus} onChange={(e) => setFocus(e.target.value)} aria-label="Compaction focus" />
          <button
            className="btn"
            onClick={async () => {
              try {
                await call('thread/compact', { threadId: id, focus: focus || null })
                toast('Compacting…')
              } catch (e) {
                toast((e as Error).message, 'error')
              }
            }}
          >
            Compact now
          </button>
          <button className="btn btn-primary" onClick={close}>
            Close
          </button>
        </>
      }
    >
      {thread && <StatusHeader thread={thread} ctxModel={ctx.model} />}
      <div className="row small" style={{ marginBottom: 8 }}>
        <b>{formatTokens(ctx.used)}</b>
        <span className="muted">
          of {formatTokens(ctx.window)} tokens ({Math.round((ctx.used / total) * 100)}%){ctx.exact ? '' : ', estimated'}
        </span>
        <span className="spacer" />
        <span className="xs subtle">
          {ctx.model ?? thread?.model ?? ''} · output reserve {formatTokens(ctx.window - ctx.budget)}
        </span>
      </div>
      <div style={{ position: 'relative', height: 18, borderRadius: 6, overflow: 'hidden', background: 'var(--bg-sunken)', border: '1px solid var(--border)' }} aria-label="Context usage">
        <div style={{ display: 'flex', height: '100%' }}>
          {CATS.map(([k, label, color]) => (b[k] > 0 ? <div key={k} title={`${label}: ${formatTokens(b[k])}`} style={{ width: `${(b[k] / total) * 100}%`, background: color }} /> : null))}
        </div>
        {markers.map(([x, l]) => (
          <div key={l} title={l} style={{ position: 'absolute', top: 0, bottom: 0, left: `${Math.min(100, x * 100)}%`, width: 2, background: l === 'budget' ? 'var(--fg)' : l === 'compact' ? 'var(--danger)' : 'var(--warning)', opacity: 0.7 }} />
        ))}
      </div>
      <div className="row xs subtle" style={{ gap: 14, marginTop: 4 }}>
        <span>▏prune at {Math.round(ctx.pruneAt * 100)}% of budget</span>
        <span>▏compact at {Math.round(ctx.compactAt * 100)}%</span>
        <span>▏budget {formatTokens(ctx.budget)}</span>
      </div>

      <table className="small" style={{ width: '100%', marginTop: 14, borderCollapse: 'collapse' }}>
        <tbody>
          {CATS.map(([k, label, color]) => (
            <tr key={k}>
              <td style={{ padding: '3px 0', width: 18 }}>
                <span style={{ display: 'inline-block', width: 10, height: 10, borderRadius: 2, background: color }} />
              </td>
              <td>{label}</td>
              <td style={{ textAlign: 'right' }}>{formatTokens(b[k])}</td>
              <td style={{ textAlign: 'right', width: 60 }} className="subtle">
                {Math.round((b[k] / total) * 100)}%
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <div className="row xs subtle" style={{ marginTop: 10, gap: 14 }}>
        <span>{ctx.prunes} prune pass(es)</span>
        {ctx.lazyTools && <span>MCP tools load lazily (schemas over budget)</span>}
      </div>

      <h4 style={{ margin: '16px 0 6px' }}>Compactions</h4>
      {ctx.compactions.length === 0 ? (
        <div className="small muted">None yet.</div>
      ) : (
        <table className="small" style={{ width: '100%', borderCollapse: 'collapse' }}>
          <tbody>
            {ctx.compactions.map((c) => (
              <tr key={c.summaryNumber} style={{ borderBottom: '1px solid var(--border)' }}>
                <td style={{ padding: '3px 0' }}>#{c.summaryNumber}</td>
                <td>{new Date(c.at).toLocaleTimeString()}</td>
                <td>
                  {formatTokens(c.tokensBefore)} → {formatTokens(c.tokensAfter)}
                </td>
                <td>{c.trigger}</td>
                <td className="subtle">{c.llm ? 'model summary' : 'extractive fallback'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Modal>
  )
}
