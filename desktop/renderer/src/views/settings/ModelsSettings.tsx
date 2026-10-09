import { useEffect, useState } from 'react'
import { CheckCircle2, CircleAlert, CircleMinus, Copy, FolderOpen, KeyRound, Plus, RefreshCw, RotateCcw, Stethoscope, Trash2, TriangleAlert, Upload } from 'lucide-react'
import type { CheckStatus, ComfyStatusResponse, ComfyTemplate, ComfyTemplatesResponse, DoctorReport, ModelInfo, ModelRole, PresetInfo, ProviderInfo } from '@shared/index'
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

const COMFY_KEY = 'comfyui:api_key'
const COMFY_ORG_KEY = 'comfyui:comfy_org_api_key'
/** Role dropdown value prefix for a workflow saved in ComfyUI (not imported yet). */
const SERVER = 'server:'
/** Role dropdown value prefix for a template from ComfyUI's library. */
const TEMPLATE = 'template:'

/** The template scan per server (it reads every candidate template and the node definitions): 10 minutes. */
let templateCache: { url: string; at: number; data: ComfyTemplatesResponse } | null = null
/** `3d/Image to mesh.json` → `3d/Image to mesh` */
const workflowName = (path: string) => path.replace(/\.json$/i, '')

/** A ComfyUI secret in the OS-encrypted store (never in config.toml); saving it re-tests the connection. */
function ComfySecret(props: { secret: string; id: string; label: string; name: string; hint: string; fromConfig: boolean; onChange: (s: ComfyStatusResponse) => void }) {
  const { secret, name } = props
  const [value, setValue] = useState('')
  const [stored, setStored] = useState(false)
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    void window.odex.secrets.has(secret).then(setStored)
  }, [secret])

  const save = async (v: string | null) => {
    setBusy(true)
    try {
      await window.odex.secrets.set(secret, v)
      setValue('')
      toast(v ? `${name} saved` : `${name} removed`, 'success')
      props.onChange(await call('comfyui/status', {}))
    } catch (e) {
      toast((e as Error).message, 'error')
    } finally {
      setStored(await window.odex.secrets.has(secret))
      setBusy(false)
    }
  }

  return (
    <form
      className="row"
      style={{ marginTop: 8 }}
      onSubmit={(e) => {
        e.preventDefault()
        if (value.trim()) void save(value.trim())
      }}
    >
      <label htmlFor={props.id} style={{ width: 200, flex: 'none' }}>
        {props.label}
      </label>
      <input
        id={props.id}
        className="input mono"
        type="password"
        autoComplete="off"
        spellCheck={false}
        style={{ maxWidth: 380 }}
        value={value}
        placeholder={stored ? '•••••••• (saved)' : props.fromConfig ? '•••••••• (from config.toml)' : props.hint}
        onChange={(e) => setValue(e.target.value)}
      />
      <button type="submit" className="btn btn-sm" disabled={busy || !value.trim()}>
        <KeyRound size={12} /> Save
      </button>
      {stored && (
        <button type="button" className="btn btn-sm btn-ghost" disabled={busy} aria-label={`Remove ${name}`} onClick={() => void save(null)}>
          <Trash2 size={12} /> Remove
        </button>
      )}
    </form>
  )
}

/**
 * Credentials: an API key for a server behind an authenticating proxy (and the header it goes in), and a
 * Comfy.org key for partner nodes such as Ideogram.
 */
function ComfyAuth({ status, onChange }: { status: ComfyStatusResponse | null; onChange: (s: ComfyStatusResponse) => void }) {
  const [header, setHeader] = useState(status?.apiKeyHeader ?? '')
  useEffect(() => setHeader(status?.apiKeyHeader ?? ''), [status?.apiKeyHeader])
  const saveHeader = async () => {
    const h = header.trim()
    if (h === (status?.apiKeyHeader ?? '')) return
    try {
      await call('config/write', { edits: [{ keyPath: 'comfyui.api_key_header', value: h || null }] })
      // re-test the connection with the new header
      onChange(await call('comfyui/status', {}))
    } catch (e) {
      toast((e as Error).message, 'error')
    }
  }

  return (
    <>
      <ComfySecret secret={COMFY_KEY} id="comfy-key" label="API key" name="ComfyUI API key" hint="optional: for servers behind an auth proxy" fromConfig={!!status?.hasApiKey} onChange={onChange} />
      <div className="row" style={{ marginTop: 8 }}>
        <label htmlFor="comfy-key-header" style={{ width: 200, flex: 'none' }}>
          Key header
        </label>
        <input
          id="comfy-key-header"
          className="input mono"
          style={{ maxWidth: 380 }}
          value={header}
          placeholder="Authorization: Bearer <key>"
          onChange={(e) => setHeader(e.target.value)}
          onBlur={() => void saveHeader()}
          onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
        />
      </div>
      <div className="xs subtle" style={{ marginTop: 4, paddingLeft: 208 }}>
        Sent with every request as <code>Authorization: Bearer &lt;key&gt;</code>. Name a header (e.g. <code>X-API-Key</code>) to send the key there as-is instead. The key is encrypted by the operating system, never written to config.toml.
      </div>
      <ComfySecret
        secret={COMFY_ORG_KEY}
        id="comfy-org-key"
        label="Comfy.org API key"
        name="Comfy.org API key"
        hint="optional: for partner nodes such as Ideogram"
        fromConfig={!!status?.hasComfyOrgKey}
        onChange={onChange}
      />
      <div className="xs subtle" style={{ marginTop: 4, paddingLeft: 208 }}>
        Partner (API) nodes such as Ideogram run on Comfy.org and need this key when Odex queues the workflow (ComfyUI&apos;s page uses your sign-in instead). Create one at{' '}
        <a
          href="https://platform.comfy.org/login"
          onClick={(e) => {
            e.preventDefault()
            void window.odex.shell.openExternal('https://platform.comfy.org/login')
          }}
        >
          platform.comfy.org
        </a>
        . It is sent with each run and encrypted by the operating system.
      </div>
    </>
  )
}

function ComfySection({
  status,
  onChange,
  templates,
  reloadTemplates,
}: {
  status: ComfyStatusResponse | null
  onChange: (s: ComfyStatusResponse) => void
  templates: ComfyTemplatesResponse | 'loading' | null
  reloadTemplates: () => void
}) {
  const [url, setUrl] = useState(status?.url ?? '')
  const [busy, setBusy] = useState(false)
  useEffect(() => setUrl(status?.url ?? ''), [status?.url])

  /** An imported workflow: to the Recycle Bin / Trash, and roles that used it go back to Off. */
  const removeWorkflow = async (name: string, path: string) => {
    const roles = GENERATION.filter((g) => status?.[g.field] === name)
    const bin = window.odex.platform === 'win32' ? 'Recycle Bin' : 'Trash'
    const note = roles.length ? ` ${roles.map((g) => g.label).join(' and ')} ${roles.length === 1 ? 'goes' : 'go'} back to Off.` : ''
    if (!(await A.confirmDialog('Remove workflow', `Move ${name} to the ${bin}?${note}`, 'Remove', true))) return
    try {
      await window.odex.shell.trashItem(path)
      if (roles.length) await call('config/write', { edits: roles.map((g) => ({ keyPath: `comfyui.${g.key}`, value: null })) })
      onChange(await call('comfyui/status', {}))
    } catch (e) {
      toast(`Could not remove ${name}: ${(e as Error).message}`, 'error')
    }
  }
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
        Image and 3D generation run your ComfyUI workflows. Under Image generation and 3D generation above, pick one of ComfyUI&apos;s templates your server has the models for, a workflow saved in ComfyUI, or one you imported. Odex puts the agent&apos;s prompt into the positive prompt box and its image into the Load Image node. To
        choose other widgets, type <code>{'{{prompt}}'}</code>, <code>{'{{negative_prompt}}'}</code>, <code>{'{{width}}'}</code>, <code>{'{{height}}'}</code>, <code>{'{{seed}}'}</code> or <code>{'{{image}}'}</code> into them in ComfyUI before saving.
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
      <ComfyAuth status={status} onChange={onChange} />
      {status?.url && !dirty && (
        <div className="row xs" style={{ marginTop: 6, paddingLeft: 208 }}>
          <span className={`dot ${status.reachable ? 'success' : 'danger'}`} aria-hidden />
          <span className={`selectable ${status.reachable ? 'subtle' : ''}`} style={status.reachable ? undefined : { color: 'var(--danger)' }}>
            {status.reachable ? `Connected · ComfyUI ${status.version ?? ''}` : status.error}
          </span>
        </div>
      )}
      {status?.reachable && !dirty && (
        <div className="xs" style={{ marginTop: 4, paddingLeft: 208 }} data-testid="comfy-saved">
          {status.serverWorkflowsError ? (
            <span className="selectable" style={{ color: 'var(--warning)' }}>
              Couldn’t list the workflows saved in ComfyUI: {status.serverWorkflowsError}
            </span>
          ) : status.serverWorkflows.length > 0 ? (
            <span className="subtle">
              {status.serverWorkflows.length} {status.serverWorkflows.length === 1 ? 'workflow' : 'workflows'} saved in ComfyUI: pick one under Image generation or 3D generation above.
            </span>
          ) : (
            <span className="subtle">
              Nothing is saved in ComfyUI yet. Open your workflow in ComfyUI and save it (Workflow → Save, or Ctrl+S): open tabs live only in your browser. It shows up here when you come back.
            </span>
          )}
        </div>
      )}
      {status?.reachable && !dirty && templates && (
        <div className="xs" style={{ marginTop: 4, paddingLeft: 208 }} data-testid="comfy-templates">
          {templates === 'loading' ? (
            <span className="subtle">Checking what your ComfyUI can run…</span>
          ) : templates.error ? (
            <span className="selectable" style={{ color: 'var(--warning)' }}>
              Couldn’t read ComfyUI&apos;s templates: {templates.error}
            </span>
          ) : (
            <>
              <span className="subtle">
                {templates.image.length + templates.model3d.length} ready on your ComfyUI
                {templates.unavailable.length ? `, ${templates.unavailable.length} templates need models or nodes it doesn’t have` : ''}.{' '}
              </span>
              <button className="btn btn-sm btn-ghost" style={{ padding: '0 6px', height: 20 }} onClick={reloadTemplates}>
                Check again
              </button>
              {templates.unavailable.length > 0 && (
                <details style={{ marginTop: 4 }}>
                  <summary className="subtle">What the other templates need</summary>
                  <ul className="selectable" style={{ margin: '4px 0 0', paddingLeft: 18 }}>
                    {templates.unavailable.map((u) => (
                      <li key={u.title}>
                        {u.title}: {u.missing.join(', ')}
                      </li>
                    ))}
                  </ul>
                </details>
              )}
            </>
          )}
        </div>
      )}
      {status && status.workflows.length === 0 && (
        <div className="muted small" style={{ marginTop: 8 }}>
          No workflows imported yet.
        </div>
      )}
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
              <button className="icon-btn sm" title="Remove workflow" aria-label={`Remove workflow ${w.name}`} onClick={() => void removeWorkflow(w.name, w.path)}>
                <Trash2 size={13} />
              </button>
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
  const [templates, setTemplates] = useState<ComfyTemplatesResponse | 'loading' | null>(null)
  const comfyUrl = comfy?.reachable ? comfy.url : null
  const loadTemplates = (force: boolean) => {
    if (!comfyUrl) return
    if (!force && templateCache?.url === comfyUrl && Date.now() - templateCache.at < 10 * 60_000) {
      setTemplates(templateCache.data)
      return
    }
    setTemplates('loading')
    void call('comfyui/templates', {})
      .then((data) => {
        templateCache = { url: comfyUrl, at: Date.now(), data }
        setTemplates(data)
      })
      .catch((e: Error) => setTemplates({ image: [], model3d: [], unavailable: [], error: e.message }))
  }
  useEffect(() => {
    if (comfyUrl) loadTemplates(false)
    else setTemplates(null)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [comfyUrl])

  useEffect(() => {
    void call('preset/list', {})
      .then((r) => setPresets(r.presets))
      .catch(() => {})
    let loadedAt = 0
    const loadComfy = () => {
      loadedAt = Date.now()
      void call('comfyui/status', {})
        .then(setComfy)
        .catch(() => {})
    }
    loadComfy()
    // back from ComfyUI (say, after saving a workflow or adding a model): look again, at most every 10 s
    const onFocus = () => Date.now() - loadedAt > 10_000 && loadComfy()
    window.addEventListener('focus', onFocus)
    return () => window.removeEventListener('focus', onFocus)
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

  const [converting, setConverting] = useState<string | null>(null)
  const setWorkflow = async (key: (typeof GENERATION)[number]['key'], value: string) => {
    const role = key === 'image_workflow' ? 'image' : 'model3d'
    if (value.startsWith(TEMPLATE)) {
      const t = templates && templates !== 'loading' ? [...templates.image, ...templates.model3d].find((x) => x.name === value.slice(TEMPLATE.length)) : null
      if (!t) return
      setConverting(key)
      try {
        setComfy(await call('comfyui/useTemplate', { name: t.name, role }))
        toast(`${GENERATION.find((g) => g.key === key)!.label} now uses ${t.title}`, 'success')
      } catch (e) {
        toast(`${t.title}: ${(e as Error).message}`, 'error')
      } finally {
        setConverting(null)
      }
      return
    }
    // "server:<path>": a workflow saved in ComfyUI, converted and imported first
    const server = value.startsWith(SERVER) ? value.slice(SERVER.length) : null
    if (!server) {
      await call('config/write', { edits: [{ keyPath: `comfyui.${key}`, value: value || null }] })
      setComfy(await call('comfyui/status', {}))
      return
    }
    const label = workflowName(server)
    setConverting(key)
    try {
      setComfy(await call('comfyui/importServer', { path: server, role }))
      toast(`Imported ${label} from ComfyUI`, 'success')
    } catch (e) {
      toast(`${label}: ${(e as Error).message}`, 'error')
    } finally {
      setConverting(null)
    }
  }

  const templateOptions = (role: 'image' | 'model3d') => {
    if (templates === 'loading') {
      return (
        <optgroup label="Your ComfyUI">
          <option disabled>Checking what it can run…</option>
        </optgroup>
      )
    }
    if (!templates || templates.error) return null
    const all: ComfyTemplate[] = role === 'image' ? templates.image : templates.model3d
    const local = all.filter((t) => !t.partner)
    const partner = all.filter((t) => t.partner)
    const option = (t: ComfyTemplate) => (
      <option key={t.name} value={TEMPLATE + t.name}>
        {t.title}
      </option>
    )
    return (
      <>
        {local.length > 0 && <optgroup label="Ready on your ComfyUI">{local.map(option)}</optgroup>}
        {partner.length > 0 && (
          <optgroup label="Comfy.org partners (credits)">
            {comfy?.hasComfyOrgKey ? partner.map(option) : <option disabled>{`${partner.length} more need a Comfy.org API key (ComfyUI section below)`}</option>}
          </optgroup>
        )}
      </>
    )
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
                  disabled={!comfy?.url || converting != null}
                  onChange={(e) => void setWorkflow(g.key, e.target.value)}
                  aria-label={`${g.label} workflow`}
                >
                  <option value="">{comfy?.url ? 'Off' : 'Set a ComfyUI server below'}</option>
                  {current && !usable.some((w) => w.name === current) && <option value={current}>{current} (missing)</option>}
                  {usable.length > 0 && (
                    <optgroup label="Imported">
                      {usable.map((w) => (
                        <option key={w.name} value={w.name}>
                          {w.name}
                        </option>
                      ))}
                    </optgroup>
                  )}
                  {comfy?.reachable && templateOptions(g.key === 'image_workflow' ? 'image' : 'model3d')}
                  {(comfy?.serverWorkflows.length ?? 0) > 0 && (
                    <optgroup label="Saved in ComfyUI">
                      {comfy!.serverWorkflows.map((p) => (
                        <option key={p} value={SERVER + p}>
                          {workflowName(p)}
                        </option>
                      ))}
                    </optgroup>
                  )}
                </select>
                {converting === g.key && <span className="xs subtle">Converting…</span>}
              </div>
            )
          })}
        </div>
      </section>

      <ComfySection status={comfy} onChange={setComfy} templates={templates} reloadTemplates={() => loadTemplates(true)} />

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
