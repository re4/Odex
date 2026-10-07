import { useEffect, useState } from 'react'
import { CheckCircle2, CircleAlert, CircleMinus, Copy, FolderOpen, Plus, RefreshCw, RotateCcw, Stethoscope, Trash2, TriangleAlert, Upload } from 'lucide-react'
import type { CheckStatus, ComfyStatusResponse, DoctorReport, ModelInfo, ModelRole, PresetInfo, ProviderInfo } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Modal, formatTokens } from '@/components/ui'

const ROLES: Array<{ role: ModelRole; label: string; hint: string }> = [
  { role: 'main', label: 'Main', hint: 'The coding agent' },
  { role: 'compactor', label: 'Compactor', hint: 'Summarizes history when context fills up' },
  { role: 'reviewer', label: 'Reviewer', hint: '/review and automatic approval review' },
  { role: 'vision', label: 'Vision', hint: 'Screenshots and images when the main model has no vision' },
  { role: 'utility', label: 'Utility', hint: 'Titles, commit messages, follow-up suggestions' },
]

/** Generation roles: a ComfyUI workflow each, not a chat model. */
const GENERATION: Array<{ key: 'image_workflow' | 'model3d_workflow'; field: 'imageWorkflow' | 'model3dWorkflow'; label: string; hint: string }> = [
  { key: 'image_workflow', field: 'imageWorkflow', label: 'Image generation', hint: 'ComfyUI workflow for the generate_image tool' },
  { key: 'model3d_workflow', field: 'model3dWorkflow', label: '3D generation', hint: 'ComfyUI workflow for the generate_3d tool' },
]

function ComfySection({ status, onChange }: { status: ComfyStatusResponse | null; onChange: (s: ComfyStatusResponse) => void }) {
  const [url, setUrl] = useState(status?.url ?? '')
  const [busy, setBusy] = useState(false)
  useEffect(() => setUrl(status?.url ?? ''), [status?.url])
  const dirty = url.trim().replace(/\/+$/, '') !== (status?.url ?? '')

  const save = async () => {
    setBusy(true)
    try {
      if (dirty) await call('config/write', { edits: [{ keyPath: 'comfyui.url', value: url.trim() || null }] })
      onChange(await call('comfyui/status', {}))
    } catch (e) {
      toast((e as Error).message, 'error')
    } finally {
      setBusy(false)
    }
  }

  const importWorkflows = async () => {
    const paths = await window.odex.dialog.openFiles()
    let next: ComfyStatusResponse | null = null
    for (const path of paths) {
      try {
        next = await call('comfyui/import', { path })
      } catch (e) {
        toast(`${path.split(/[\\/]/).pop()}: ${(e as Error).message}`, 'error')
      }
    }
    if (next) onChange(next)
  }

  const usedFor = (name: string) =>
    GENERATION.filter((g) => status?.[g.field] === name)
      .map((g) => g.label)
      .join(', ')

  return (
    <section>
      <div className="row" style={{ marginBottom: 8 }}>
        <h3 className="grow" style={{ margin: 0 }}>
          ComfyUI
        </h3>
        <button className="btn btn-sm" onClick={() => void importWorkflows()}>
          <Upload size={13} /> Import workflow
        </button>
        <button className="btn btn-sm" disabled={!status} onClick={() => status && void window.odex.shell.openPath(status.workflowsDir)}>
          <FolderOpen size={13} /> Open folder
        </button>
      </div>
      <div className="xs muted" style={{ marginBottom: 8 }}>
        Image and 3D generation run your ComfyUI workflows. In ComfyUI, type <code>{'{{prompt}}'}</code> into the prompt box (optionally <code>{'{{negative_prompt}}'}</code>, <code>{'{{width}}'}</code>,{' '}
        <code>{'{{height}}'}</code>, <code>{'{{seed}}'}</code>, or <code>{'{{image}}'}</code> in a Load Image node for image-to-3D), export it with Workflow → Export (API), then import the file here.
      </div>
      <div className="row">
        <label htmlFor="comfy-url" style={{ width: 200, flex: 'none' }}>
          Server URL
        </label>
        <input
          id="comfy-url"
          className="input mono"
          style={{ maxWidth: 380 }}
          value={url}
          placeholder="http://127.0.0.1:8188"
          onChange={(e) => setUrl(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && void save()}
        />
        <button className="btn btn-sm" disabled={busy || (!dirty && !status?.url)} onClick={() => void save()}>
          {busy ? 'Checking…' : dirty ? 'Save' : 'Test'}
        </button>
      </div>
      {status?.url && !dirty && (
        <div className="row xs" style={{ marginTop: 4, paddingLeft: 208 }}>
          <span className={`dot ${status.reachable ? 'success' : 'danger'}`} aria-hidden />
          <span className={`selectable ${status.reachable ? 'subtle' : ''}`} style={status.reachable ? undefined : { color: 'var(--danger)' }}>
            {status.reachable ? `Connected · ComfyUI ${status.version ?? ''}` : status.error}
          </span>
        </div>
      )}
      {status && status.workflows.length === 0 && <div className="muted small" style={{ marginTop: 8 }}>No workflows yet.</div>}
      {status && status.workflows.length > 0 && (
        <div className="col" style={{ gap: 4, marginTop: 8 }}>
          {status.workflows.map((w) => (
            <div key={w.path} className="row small" style={{ padding: '4px 0', borderBottom: '1px solid var(--border)' }}>
              <b className="ellipsis" style={{ width: 200, flex: 'none' }} title={w.path}>
                {w.name}
              </b>
              {w.error ? (
                <span className="xs grow" style={{ color: 'var(--danger)' }}>
                  {w.error}
                </span>
              ) : (
                <span className="xs subtle grow">
                  {w.nodes} nodes · {w.placeholders.length ? w.placeholders.map((p) => `{{${p}}}`).join(' ') : 'no placeholders'}
                </span>
              )}
              {usedFor(w.name) && <span className="badge">{usedFor(w.name)}</span>}
            </div>
          ))}
        </div>
      )}
    </section>
  )
}

function StatusIcon({ s }: { s: CheckStatus }) {
  if (s === 'pass') return <CheckCircle2 size={14} color="var(--success)" aria-label="pass" />
  if (s === 'warn') return <TriangleAlert size={14} color="var(--warning)" aria-label="warning" />
  if (s === 'fail') return <CircleAlert size={14} color="var(--danger)" aria-label="fail" />
  return <CircleMinus size={14} color="var(--fg-subtle)" aria-label="skipped" />
}

export function DoctorReportView({ report }: { report: DoctorReport }) {
  return (
    <div className="card" style={{ padding: 10 }}>
      <div className="row" style={{ marginBottom: 6 }}>
        <b className="grow ellipsis">{report.modelId}</b>
        <span className="xs subtle">
          {report.serverVersion ? `vLLM ${report.serverVersion} · ` : ''}
          {new Date(report.ranAt).toLocaleTimeString()}
        </span>
      </div>
      {report.checks.map((c) => (
        <div key={c.id} className="row small" style={{ alignItems: 'flex-start', padding: '2px 0' }}>
          <StatusIcon s={c.status} />
          <span style={{ width: 170, flex: 'none' }}>{c.name}</span>
          <span className="muted selectable grow" style={{ wordBreak: 'break-word' }}>
            {c.detail}
            {c.fixFlags.length > 0 && (
              <>
                {' '}
                Fix: <code>{c.fixFlags.join(' ')}</code>
              </>
            )}
          </span>
          <span className="xs subtle">{c.durationMs} ms</span>
        </div>
      ))}
      {report.suggestedCommand && (
        <div style={{ marginTop: 8 }}>
          <div className="row xs subtle">
            <span className="grow">Suggested launch command</span>
            <button className="icon-btn sm" aria-label="Copy command" onClick={() => A.copy(report.suggestedCommand!, 'Command copied')}>
              <Copy size={12} />
            </button>
          </div>
          <pre className="selectable xs" style={{ margin: 0, padding: 8, background: 'var(--code-bg)', borderRadius: 6, whiteSpace: 'pre-wrap' }}>
            {report.suggestedCommand}
          </pre>
        </div>
      )}
    </div>
  )
}

function ProviderEditor({ initial, onClose }: { initial?: ProviderInfo; onClose: () => void }) {
  const [id, setId] = useState(initial?.id ?? '')
  const [name, setName] = useState(initial?.name ?? '')
  const [url, setUrl] = useState(initial?.baseUrl ?? 'http://localhost:8000/v1')
  const [key, setKey] = useState('')
  const [maxConc, setMaxConc] = useState(initial?.maxConcurrentRequests ?? 8)
  const [testMsg, setTestMsg] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const def = () => ({ name: name || id, base_url: A.normalizeBaseUrl(url), headers: {}, query_params: {}, max_concurrent_requests: maxConc, enabled: true })
  return (
    <Modal
      title={initial ? `Edit ${initial.name}` : 'Add endpoint'}
      onClose={onClose}
      footer={
        <>
          <button
            className="btn"
            disabled={busy}
            onClick={async () => {
              setBusy(true)
              const r = await call('provider/test', { provider: def(), apiKey: key || null }).catch((e: Error) => ({ ok: false, error: e.message, models: [], latencyMs: 0 }))
              setTestMsg(r.ok ? `Connected · ${r.models.length} model(s) · ${r.latencyMs} ms` : `Failed: ${r.error}`)
              setBusy(false)
            }}
          >
            Test
          </button>
          <button
            className="btn btn-primary"
            disabled={busy || !id || !url}
            onClick={async () => {
              setBusy(true)
              try {
                await call('provider/upsert', { id, provider: def(), apiKey: key || null })
                await useApp.getState().refreshModels(true)
                onClose()
              } catch (e) {
                setTestMsg((e as Error).message)
              } finally {
                setBusy(false)
              }
            }}
          >
            Save
          </button>
        </>
      }
    >
      <div className="col" style={{ gap: 10 }}>
        <div className="field">
          <label htmlFor="pe-id">Id</label>
          <input id="pe-id" className="input" value={id} disabled={!!initial} onChange={(e) => setId(e.target.value.replace(/[^\w-]/g, ''))} placeholder="local" />
        </div>
        <div className="field">
          <label htmlFor="pe-name">Display name</label>
          <input id="pe-name" className="input" value={name} onChange={(e) => setName(e.target.value)} />
        </div>
        <div className="field">
          <label htmlFor="pe-url">Base URL</label>
          <input id="pe-url" className="input mono" value={url} onChange={(e) => setUrl(e.target.value)} onBlur={() => setUrl(A.normalizeBaseUrl(url))} />
        </div>
        <div className="field">
          <label htmlFor="pe-key">API key</label>
          <input id="pe-key" className="input" type="password" value={key} onChange={(e) => setKey(e.target.value)} placeholder={initial?.hasApiKey ? '•••••• (unchanged)' : 'optional'} autoComplete="off" />
        </div>
        <div className="field">
          <label htmlFor="pe-conc">Max concurrent requests</label>
          <input id="pe-conc" className="input" type="number" min={1} max={256} value={maxConc} onChange={(e) => setMaxConc(Number(e.target.value) || 1)} style={{ maxWidth: 120 }} />
          <span className="hint">vLLM batches concurrent requests; subagents and utility calls share this limit.</span>
        </div>
        {testMsg && <div className="small selectable">{testMsg}</div>}
      </div>
    </Modal>
  )
}

export function ModelsSettings() {
  const providers = useApp((s) => s.providers)
  const models = useApp((s) => s.models)
  const roles = useApp((s) => s.roles)
  const hiddenModels = useApp((s) => s.hiddenModels)
  const [editing, setEditing] = useState<ProviderInfo | 'new' | null>(null)
  const [reports, setReports] = useState<DoctorReport[]>([])
  const [doctorBusy, setDoctorBusy] = useState<string | null>(null)
  const [presets, setPresets] = useState<PresetInfo[]>([])
  const [refreshing, setRefreshing] = useState(false)
  const [comfy, setComfy] = useState<ComfyStatusResponse | null>(null)

  useEffect(() => {
    void call('preset/list', {})
      .then((r) => setPresets(r.presets))
      .catch(() => {})
    void call('comfyui/status', {})
      .then(setComfy)
      .catch(() => {})
  }, [])

  const runDoctor = async (providerId: string | null, model: string | null, quick: boolean) => {
    setDoctorBusy(model ?? providerId ?? 'all')
    try {
      const r = await call('doctor/run', { providerId, model, quick })
      setReports((cur) => [...r.reports, ...cur.filter((x) => !r.reports.some((y) => y.providerId === x.providerId && y.modelId === x.modelId))])
      await useApp.getState().refreshModels()
    } catch (e) {
      toast(`Doctor failed: ${(e as Error).message}`, 'error')
    } finally {
      setDoctorBusy(null)
    }
  }

  const setRole = async (role: ModelRole, key: string) => {
    await call('config/write', { edits: [{ keyPath: `roles.${role}`, value: key || null }] })
    await useApp.getState().refreshModels()
  }

  const setWorkflow = async (key: (typeof GENERATION)[number]['key'], name: string) => {
    await call('config/write', { edits: [{ keyPath: `comfyui.${key}`, value: name || null }] })
    setComfy(await call('comfyui/status', {}))
  }

  const removeModel = async (m: ModelInfo) => {
    const configured = m.key !== `${m.providerId}:${m.modelId}`
    const what = [
      configured ? 'Its [models] entry is deleted from config.toml.' : '',
      m.available ? `${m.providerId} still serves it; you can restore it under Removed models.` : '',
      m.roles.length ? `Roles using it (${m.roles.join(', ')}) go back to their default.` : '',
    ]
      .filter(Boolean)
      .join(' ')
    if (!(await A.confirmDialog('Remove model', `Remove ${m.displayName} (${m.key}) from the model list? ${what}`, 'Remove', true))) return
    try {
      await call('model/remove', { key: m.key })
      await useApp.getState().refreshModels()
    } catch (e) {
      toast(`Could not remove ${m.displayName}: ${(e as Error).message}`, 'error')
    }
  }

  const restoreModel = async (key: string) => {
    await call('config/write', { edits: [{ keyPath: 'hidden_models', value: hiddenModels.filter((k) => k !== key) }] })
    await useApp.getState().refreshModels()
  }

  return (
    <div className="col" style={{ gap: 18 }}>
      <section>
        <div className="row" style={{ marginBottom: 8 }}>
          <h3 className="grow" style={{ margin: 0 }}>
            Endpoints
          </h3>
          <button
            className="btn btn-sm"
            disabled={refreshing}
            onClick={async () => {
              setRefreshing(true)
              await useApp.getState().refreshModels(true).finally(() => setRefreshing(false))
            }}
          >
            <RefreshCw size={13} /> Refresh
          </button>
          <button className="btn btn-sm" onClick={() => setEditing('new')}>
            <Plus size={13} /> Add endpoint
          </button>
        </div>
        {providers.length === 0 && <div className="muted small">No endpoints yet.</div>}
        <div className="col" style={{ gap: 8 }}>
          {providers.map((p) => (
            <div key={p.id} className="card" style={{ padding: 10 }}>
              <div className="row">
                <span className={`dot ${p.health === 'healthy' ? 'success' : p.health === 'unreachable' ? 'danger' : p.health === 'degraded' ? 'warning' : ''}`} aria-label={p.health} />
                <b>{p.name}</b>
                <span className="mono xs subtle ellipsis grow">{p.baseUrl}</span>
                {p.version && <span className="badge">vLLM {p.version}</span>}
                <span className="xs subtle">
                  {p.inFlight} running · {p.queued} queued · max {p.maxConcurrentRequests}
                </span>
                <button className="btn btn-sm" disabled={!!doctorBusy} onClick={() => void runDoctor(p.id, null, false)}>
                  <Stethoscope size={13} /> {doctorBusy === p.id ? 'Running…' : 'Doctor'}
                </button>
                <button className="btn btn-sm btn-ghost" onClick={() => setEditing(p)}>
                  Edit
                </button>
                <button
                  className="icon-btn sm"
                  aria-label={`Remove ${p.name}`}
                  onClick={async () => {
                    if (await A.confirmDialog('Remove endpoint', `Remove ${p.name}? Threads using its models will need another model.`, 'Remove', true)) {
                      await call('provider/remove', { id: p.id })
                      await useApp.getState().refreshModels()
                    }
                  }}
                >
                  <Trash2 size={13} />
                </button>
              </div>
              {p.error && <div className="small selectable" style={{ color: 'var(--danger)', marginTop: 4 }}>{p.error}</div>}
              {p.models.length > 0 && (
                <div className="xs muted" style={{ marginTop: 6 }}>
                  {p.models.map((m) => (
                    <span key={m.id} className="mono" style={{ marginRight: 12 }}>
                      {m.id}
                      {m.maxModelLen ? ` (${formatTokens(m.maxModelLen)})` : ''}
                    </span>
                  ))}
                </div>
              )}
            </div>
          ))}
        </div>
      </section>

      <section>
        <h3 style={{ margin: '0 0 8px' }}>Roles</h3>
        <div className="col" style={{ gap: 8 }}>
          {ROLES.map((r) => (
            <div key={r.role} className="row">
              <div style={{ width: 200 }}>
                <div>{r.label}</div>
                <div className="xs subtle">{r.hint}</div>
              </div>
              <select className="select" style={{ maxWidth: 380 }} value={roles[r.role] ?? ''} onChange={(e) => void setRole(r.role, e.target.value)} aria-label={`${r.label} model`}>
                <option value="">{r.role === 'main' ? '(none)' : 'Same as main'}</option>
                {models.map((m) => (
                  <option key={m.key} value={m.key}>
                    {m.displayName}
                  </option>
                ))}
              </select>
            </div>
          ))}
          {GENERATION.map((g) => {
            const current = comfy?.[g.field] ?? ''
            const usable = comfy?.workflows.filter((w) => !w.error) ?? []
            return (
              <div key={g.key} className="row">
                <div style={{ width: 200 }}>
                  <div>{g.label}</div>
                  <div className="xs subtle">{g.hint}</div>
                </div>
                <select
                  className="select"
                  style={{ maxWidth: 380 }}
                  value={current}
                  disabled={!comfy?.url}
                  onChange={(e) => void setWorkflow(g.key, e.target.value)}
                  aria-label={`${g.label} workflow`}
                >
                  <option value="">{comfy?.url ? 'Off' : 'Set a ComfyUI server below'}</option>
                  {current && !usable.some((w) => w.name === current) && <option value={current}>{current} (missing)</option>}
                  {usable.map((w) => (
                    <option key={w.name} value={w.name}>
                      {w.name}
                    </option>
                  ))}
                </select>
              </div>
            )
          })}
        </div>
      </section>

      <ComfySection status={comfy} onChange={setComfy} />

      <section>
        <h3 style={{ margin: '0 0 8px' }}>Models</h3>
        {models.length === 0 && <div className="muted small">No models discovered.</div>}
        <table className="small" style={{ borderCollapse: 'collapse', width: '100%' }}>
          <tbody>
            {models.map((m) => (
              <tr key={m.key} style={{ borderBottom: '1px solid var(--border)' }}>
                <td style={{ padding: '6px 4px' }}>
                  <div>{m.displayName}</div>
                  <div className="xs subtle mono">{m.key}</div>
                </td>
                <td className="xs">{formatTokens(m.contextWindow)} ctx</td>
                <td className="xs">
                  {[m.capabilities.tools && 'tools', m.capabilities.vision && 'vision', m.capabilities.reasoning && 'reasoning', m.capabilities.parallel_tools && 'parallel'].filter(Boolean).join(' · ')}
                </td>
                <td className="xs subtle">{m.preset ?? 'generic'}</td>
                <td className="xs">{m.roles.join(', ')}</td>
                <td>{!m.available && <span className="badge warning">offline</span>}</td>
                <td>
                  <button className="btn btn-sm btn-ghost" disabled={!!doctorBusy} onClick={() => void runDoctor(m.providerId, m.modelId, false)}>
                    {doctorBusy === m.modelId ? 'Running…' : 'Doctor'}
                  </button>
                </td>
                <td>
                  <button className="icon-btn sm" title="Remove from the list" aria-label={`Remove ${m.key}`} onClick={() => void removeModel(m)}>
                    <Trash2 size={13} />
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {hiddenModels.length > 0 && (
          <details style={{ marginTop: 8 }}>
            <summary className="small">Removed models ({hiddenModels.length})</summary>
            {hiddenModels.map((k) => (
              <div key={k} className="row small" style={{ padding: '4px 0' }}>
                <span className="mono xs ellipsis grow">{k}</span>
                <button className="btn btn-sm btn-ghost" aria-label={`Restore ${k}`} onClick={() => void restoreModel(k)}>
                  <RotateCcw size={12} /> Restore
                </button>
              </div>
            ))}
          </details>
        )}
      </section>

      {reports.length > 0 && (
        <section>
          <h3 style={{ margin: '0 0 8px' }}>Doctor</h3>
          <div className="col" style={{ gap: 8 }}>
            {reports.map((r) => (
              <DoctorReportView key={`${r.providerId}:${r.modelId}`} report={r} />
            ))}
          </div>
        </section>
      )}

      {presets.length > 0 && (
        <section>
          <h3 style={{ margin: '0 0 8px' }}>Presets</h3>
          <div className="xs muted" style={{ marginBottom: 6 }}>
            Presets match served model ids and supply sampling, reasoning and tool-call settings. Override per model under <code>[models.&lt;key&gt;]</code> in config.toml.
          </div>
          <details>
            <summary className="small">{presets.length} presets</summary>
            {presets.map((p) => (
              <div key={p.id} style={{ padding: '6px 0', borderBottom: '1px solid var(--border)' }}>
                <div className="row small">
                  <b>{p.displayName}</b>
                  <span className="xs subtle mono">{p.matchPatterns.join(', ')}</span>
                  <span className="spacer" />
                  <button className="icon-btn sm" aria-label="Copy serve command" onClick={() => A.copy(p.serveCommand, 'Serve command copied')}>
                    <Copy size={12} />
                  </button>
                </div>
                <code className="xs selectable" style={{ wordBreak: 'break-all' }}>
                  {p.serveCommand}
                </code>
                {p.notes && <div className="xs subtle">{p.notes}</div>}
              </div>
            ))}
          </details>
        </section>
      )}
      {editing && <ProviderEditor initial={editing === 'new' ? undefined : editing} onClose={() => setEditing(null)} />}
    </div>
  )
}
