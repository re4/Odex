import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { ChevronDown, ChevronRight, Copy, Eye, FolderOpen, Info, LogIn, LogOut, Pencil, Plus, RefreshCw, RotateCw, ScrollText, Trash2 } from 'lucide-react'
import type { JsonValue, McpResourceInfo, McpServerState, McpServerStatus, McpServerToml, McpToolInfo } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Modal, Toggle, formatTokens } from '@/components/ui'
import { KeyValueEditor, ListEditor, SectionHead, Segmented, pairsToRecord, parseList, recordToPairs, type Pairs } from '@/views/settings/IntegrationsShared'

const STATES: Record<McpServerState, { label: string; dot: string; badge: string }> = {
  ready: { label: 'Ready', dot: 'success', badge: 'success' },
  starting: { label: 'Starting', dot: 'starting', badge: 'accent' },
  failed: { label: 'Failed', dot: 'danger', badge: 'danger' },
  needsAuth: { label: 'Sign-in required', dot: 'warning', badge: 'warning' },
  disabled: { label: 'Disabled', dot: '', badge: '' },
  stopped: { label: 'Stopped', dot: '', badge: '' },
}

function stateInfo(s: McpServerState) {
  return STATES[s] ?? { label: s, dot: '', badge: '' }
}

function emptyServer(): McpServerToml {
  return {
    command: null,
    args: [],
    env: {},
    cwd: null,
    url: null,
    bearer_token: null,
    bearer_token_env_var: null,
    headers: {},
    oauth: null,
    startup_timeout_ms: null,
    tool_timeout_ms: null,
    enabled: null,
    enabled_tools: null,
    disabled_tools: [],
    auto_approve_tools: [],
  }
}

/** `config/read` omits empty fields; fill them in. */
function normalize(t: Partial<McpServerToml> | undefined): McpServerToml {
  return {
    ...emptyServer(),
    ...(t ?? {}),
    args: t?.args ?? [],
    env: t?.env ?? {},
    headers: t?.headers ?? {},
    disabled_tools: t?.disabled_tools ?? [],
    auto_approve_tools: t?.auto_approve_tools ?? [],
  }
}

/** Connection-relevant config as a stable string (tool filters excluded). */
function connectionKey(s: McpServerToml): string {
  const { enabled_tools: _a, disabled_tools: _b, auto_approve_tools: _c, ...rest } = s
  const clean = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.length ? v : undefined
    if (v && typeof v === 'object') {
      const entries = Object.entries(v as Record<string, unknown>)
        .map(([k, x]) => [k, clean(x)] as const)
        .filter(([, x]) => x !== undefined)
        .sort(([a], [b]) => a.localeCompare(b))
      return entries.length ? Object.fromEntries(entries) : undefined
    }
    return v === null || v === '' ? undefined : v
  }
  return JSON.stringify(clean(rest) ?? {})
}

function setStoreServers(servers: McpServerStatus[]) {
  useApp.setState({ mcp: [...servers].sort((a, b) => a.name.localeCompare(b.name)) })
}

function patchServer(name: string, f: (s: McpServerStatus) => McpServerStatus) {
  useApp.setState((st) => ({ mcp: st.mcp.map((s) => (s.name === name ? f(s) : s)) }))
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/** Popular local servers to start from (adapted "recommended servers"). */
const TEMPLATES: Array<{ id: string; label: string; command: string; args: string[]; hint: string }> = [
  { id: 'filesystem', label: 'Filesystem', command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem', '.'], hint: 'Read and write files under the given folders' },
  { id: 'playwright', label: 'Playwright', command: 'npx', args: ['-y', '@playwright/mcp@latest'], hint: 'Drive a browser' },
  { id: 'fetch', label: 'Fetch', command: 'uvx', args: ['mcp-server-fetch'], hint: 'Fetch web pages as markdown' },
  { id: 'git', label: 'Git', command: 'uvx', args: ['mcp-server-git'], hint: 'Inspect git repositories' },
]

// ------------------------------------------------------------------ editor

function StatusLine({ status }: { status: McpServerStatus | { error: string } }) {
  if ('error' in status && !('state' in status)) {
    return (
      <span className="int-test-result" role="status">
        <span className="dot danger int-dot" />
        <span className="ellipsis" style={{ color: 'var(--danger)' }} title={status.error}>
          {status.error}
        </span>
      </span>
    )
  }
  const s = status as McpServerStatus
  const st = stateInfo(s.state)
  return (
    <span className="int-test-result" role="status" aria-label="Test result">
      <span className={`dot int-dot ${st.dot}`} />
      <b>{st.label}</b>
      {s.state === 'ready' && (
        <span className="muted">
          {s.tools.length} tool{s.tools.length === 1 ? '' : 's'}
          {s.serverName ? ` · ${s.serverName}${s.serverVersion ? ` ${s.serverVersion}` : ''}` : ''}
        </span>
      )}
      {s.error && s.state !== 'ready' && (
        <span className="ellipsis" style={{ color: 'var(--danger)' }} title={s.error}>
          {s.error}
        </span>
      )}
    </span>
  )
}

function McpServerEditor(props: { name: string | null; initial: McpServerToml; existing: string[]; onClose: () => void; onSaved: (name: string, server: McpServerToml, servers: McpServerStatus[]) => void }) {
  const init = props.initial
  const isNew = props.name == null
  const [name, setName] = useState(props.name ?? '')
  const [type, setType] = useState<'stdio' | 'http'>(init.url ? 'http' : 'stdio')
  const [command, setCommand] = useState(init.command ?? '')
  const [args, setArgs] = useState<string[]>(init.args)
  const [env, setEnv] = useState<Pairs>(recordToPairs(init.env))
  const [cwd, setCwd] = useState(init.cwd ?? '')
  const [url, setUrl] = useState(init.url ?? '')
  const [tokenEnv, setTokenEnv] = useState(init.bearer_token_env_var ?? '')
  const [headers, setHeaders] = useState<Pairs>(recordToPairs(init.headers))
  const [oauth, setOauth] = useState(!!init.oauth)
  const [startup, setStartup] = useState(init.startup_timeout_ms != null ? String(init.startup_timeout_ms / 1000) : '')
  const [toolTimeout, setToolTimeout] = useState(init.tool_timeout_ms != null ? String(init.tool_timeout_ms / 1000) : '')
  const [enabledTools, setEnabledTools] = useState((init.enabled_tools ?? []).join(', '))
  const [disabledTools, setDisabledTools] = useState(init.disabled_tools.join(', '))
  const [approval, setApproval] = useState<'ask' | 'all' | 'list'>(init.auto_approve_tools.includes('*') ? 'all' : init.auto_approve_tools.length ? 'list' : 'ask')
  const [autoList, setAutoList] = useState(init.auto_approve_tools.filter((t) => t !== '*').join(', '))
  const [busy, setBusy] = useState<'test' | 'save' | null>(null)
  const [result, setResult] = useState<McpServerStatus | { error: string } | null>(null)
  const [attempted, setAttempted] = useState(false)
  const [saved, setSaved] = useState(false)
  /** Last config written for this name (null: not saved yet). */
  const savedKey = useRef<string | null>(isNew ? null : connectionKey(init))
  const advancedOpen = !!(init.startup_timeout_ms || init.tool_timeout_ms || init.enabled_tools || init.disabled_tools.length || init.auto_approve_tools.length)

  const errors: string[] = []
  if (!name.trim()) errors.push('Name is required.')
  else if (!/^[A-Za-z0-9_-]+$/.test(name)) errors.push('Name can only use letters, digits, - and _.')
  else if (isNew && props.existing.includes(name)) errors.push(`A server named “${name}” already exists.`)
  if (type === 'stdio' && !command.trim()) errors.push('Command is required.')
  if (type === 'http' && !/^https?:\/\/\S+$/.test(url.trim())) errors.push('URL must start with http:// or https://.')
  for (const [label, v] of [
    ['Startup timeout', startup],
    ['Tool timeout', toolTimeout],
  ] as const) {
    if (v.trim() && !(Number(v) > 0)) errors.push(`${label} must be a positive number of seconds.`)
  }

  const build = (): McpServerToml => {
    const ms = (s: string) => (s.trim() ? Math.round(Number(s) * 1000) : null)
    const allow = parseList(enabledTools)
    const common = {
      startup_timeout_ms: ms(startup),
      tool_timeout_ms: ms(toolTimeout),
      enabled: init.enabled ?? null,
      enabled_tools: allow.length ? allow : null,
      disabled_tools: parseList(disabledTools),
      auto_approve_tools: approval === 'all' ? ['*'] : approval === 'list' ? parseList(autoList) : [],
    }
    if (type === 'stdio') return { ...emptyServer(), ...common, command: command.trim(), args: args.filter((a) => a.length > 0), env: pairsToRecord(env), cwd: cwd.trim() || null }
    return { ...emptyServer(), ...common, url: url.trim(), bearer_token: init.bearer_token ?? null, bearer_token_env_var: tokenEnv.trim() || null, headers: pairsToRecord(headers), oauth: oauth || null }
  }

  const persist = async (restartIfUnchanged: boolean): Promise<McpServerStatus[]> => {
    const server = build()
    const key = connectionKey(server)
    let servers = (await call('mcp/upsert', { name, server })).servers
    // upsert only reconnects when the connection settings changed
    if (restartIfUnchanged && savedKey.current === key) servers = (await call('mcp/restart', { name })).servers
    savedKey.current = key
    setSaved(true)
    props.onSaved(name, server, servers)
    return servers
  }

  const run = async (what: 'test' | 'save') => {
    setAttempted(true)
    if (errors.length) return
    setBusy(what)
    if (what === 'test') setResult(null)
    try {
      const servers = await persist(what === 'test')
      if (what === 'save') props.onClose()
      else setResult(servers.find((s) => s.name === name) ?? { error: 'The server did not appear after saving.' })
    } catch (e) {
      setResult({ error: errText(e) })
    } finally {
      setBusy(null)
    }
  }

  const applyTemplate = (t: (typeof TEMPLATES)[number]) => {
    setType('stdio')
    setCommand(t.command)
    setArgs(t.args)
    if (!name.trim()) setName(t.id)
  }

  return (
    <Modal
      wide
      title={isNew ? 'Add MCP server' : `Edit ${props.name}`}
      onClose={props.onClose}
      footer={
        <>
          {busy === 'test' ? (
            <span className="int-test-result">
              <span className="spinner" /> Saving and starting…
            </span>
          ) : result ? (
            <StatusLine status={result} />
          ) : null}
          <button className="btn" onClick={props.onClose}>
            {saved ? 'Close' : 'Cancel'}
          </button>
          <button className="btn" disabled={!!busy} onClick={() => void run('test')} title="Save, (re)start the server and show its status">
            Test
          </button>
          <button className="btn btn-primary" disabled={!!busy} onClick={() => void run('save')}>
            {busy === 'save' ? 'Saving…' : 'Save'}
          </button>
        </>
      }
    >
      <div className="int-form">
        {isNew && (
          <div className="field">
            <label>Start from a template</label>
            <div className="row" style={{ gap: 6, flexWrap: 'wrap' }}>
              {TEMPLATES.map((t) => (
                <button key={t.id} type="button" className="chip" title={`${t.hint}: ${t.command} ${t.args.join(' ')}`} onClick={() => applyTemplate(t)}>
                  {t.label}
                </button>
              ))}
            </div>
          </div>
        )}
        <div className="int-form-grid">
          <div className="field">
            <label htmlFor="mcp-name">Name</label>
            <input id="mcp-name" className="input mono" value={name} disabled={!isNew} onChange={(e) => setName(e.target.value.replace(/\s/g, '-'))} placeholder="my-server" autoComplete="off" />
            <span className="hint">
              Tools appear to the model as <code>mcp__{name || 'name'}__tool</code>.
            </span>
          </div>
          <div className="field">
            <label>Type</label>
            <Segmented
              label="Transport"
              value={type}
              options={[
                ['stdio', 'STDIO'],
                ['http', 'Streamable HTTP'],
              ]}
              onChange={setType}
            />
          </div>
        </div>

        {type === 'stdio' ? (
          <>
            <div className="field">
              <label htmlFor="mcp-command">Command to launch</label>
              <input id="mcp-command" className="input mono" value={command} onChange={(e) => setCommand(e.target.value)} placeholder="npx" autoComplete="off" />
            </div>
            <div className="field">
              <label>Arguments</label>
              <ListEditor label="Argument" values={args} onChange={setArgs} placeholder="--flag or value" addLabel="Add argument" />
            </div>
            <div className="field">
              <label>Environment variables</label>
              <KeyValueEditor label="Environment variable" pairs={env} onChange={setEnv} keyPlaceholder="API_KEY" addLabel="Add variable" />
              <span className="hint">Passed to the server process in addition to your environment. Stored in config.toml.</span>
            </div>
            <div className="field">
              <label htmlFor="mcp-cwd">Working directory</label>
              <div className="row" style={{ gap: 6 }}>
                <input id="mcp-cwd" className="input mono" value={cwd} onChange={(e) => setCwd(e.target.value)} placeholder="(default)" />
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={async () => {
                    const [dir] = await window.odex.dialog.openFolder()
                    if (dir) setCwd(dir)
                  }}
                >
                  <FolderOpen size={13} /> Browse
                </button>
              </div>
            </div>
          </>
        ) : (
          <>
            <div className="field">
              <label htmlFor="mcp-url">URL</label>
              <input id="mcp-url" className="input mono" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://example.com/mcp" autoComplete="off" />
            </div>
            <div className="field">
              <label htmlFor="mcp-token-env">Bearer token environment variable</label>
              <input id="mcp-token-env" className="input mono" value={tokenEnv} onChange={(e) => setTokenEnv(e.target.value)} placeholder="MY_SERVER_TOKEN" autoComplete="off" />
              <span className="hint">The token is read from this variable when connecting, so it never lands in config.toml.</span>
            </div>
            <div className="field">
              <label>Headers</label>
              <KeyValueEditor label="Header" pairs={headers} onChange={setHeaders} keyPlaceholder="X-Header" addLabel="Add header" />
            </div>
            <label className="checkbox small">
              <Toggle checked={oauth} onChange={setOauth} label="Use OAuth" />
              Sign in with OAuth (for servers that require it; use <b>Sign in</b> after saving)
            </label>
          </>
        )}

        <details className="int-details" open={advancedOpen}>
          <summary>Advanced</summary>
          <div className="int-form">
            <div className="int-form-grid">
              <div className="field">
                <label htmlFor="mcp-startup">Startup timeout (seconds)</label>
                <input id="mcp-startup" className="input" type="number" min={1} value={startup} onChange={(e) => setStartup(e.target.value)} placeholder="default" />
              </div>
              <div className="field">
                <label htmlFor="mcp-tooltimeout">Tool call timeout (seconds)</label>
                <input id="mcp-tooltimeout" className="input" type="number" min={1} value={toolTimeout} onChange={(e) => setToolTimeout(e.target.value)} placeholder="default" />
              </div>
            </div>
            <div className="int-form-grid">
              <div className="field">
                <label htmlFor="mcp-enabled-tools">Enabled tools</label>
                <input id="mcp-enabled-tools" className="input mono" value={enabledTools} onChange={(e) => setEnabledTools(e.target.value)} placeholder="all tools" />
                <span className="hint">Comma-separated allow-list. Leave empty to allow every tool.</span>
              </div>
              <div className="field">
                <label htmlFor="mcp-disabled-tools">Disabled tools</label>
                <input id="mcp-disabled-tools" className="input mono" value={disabledTools} onChange={(e) => setDisabledTools(e.target.value)} placeholder="none" />
                <span className="hint">Comma-separated; these are never offered to the model.</span>
              </div>
            </div>
            <div className="field">
              <label htmlFor="mcp-approval">Tool approval</label>
              <select id="mcp-approval" className="select" value={approval} onChange={(e) => setApproval(e.target.value as typeof approval)} style={{ maxWidth: 360 }}>
                <option value="ask">Follow the permission mode (ask when needed)</option>
                <option value="list">Auto-approve the tools listed below</option>
                <option value="all">Auto-approve every tool from this server</option>
              </select>
              {approval === 'list' && <input className="input mono" aria-label="Auto-approved tools" value={autoList} onChange={(e) => setAutoList(e.target.value)} placeholder="tool_a, tool_b" />}
              {approval === 'all' && <span className="hint int-warning-text">Calls from this server will run without asking. Only use this for servers you trust.</span>}
            </div>
          </div>
        </details>
        {attempted && errors.length > 0 && (
          <div className="int-validation" role="alert">
            {errors.map((e) => (
              <span key={e} className="err">
                {e}
              </span>
            ))}
          </div>
        )}
      </div>
    </Modal>
  )
}

// ------------------------------------------------------------------ modals

function LogsModal({ name, onClose }: { name: string; onClose: () => void }) {
  const [lines, setLines] = useState<string[]>([])
  const [auto, setAuto] = useState(true)
  const ref = useRef<HTMLPreElement>(null)
  const stick = useRef(true)
  useEffect(() => {
    let alive = true
    const load = () =>
      call('mcp/logs', { name })
        .then((r) => alive && setLines(r.lines))
        .catch(() => {})
    void load()
    const t = auto ? setInterval(load, 1500) : undefined
    return () => {
      alive = false
      if (t) clearInterval(t)
    }
  }, [name, auto])
  useLayoutEffect(() => {
    if (stick.current && ref.current) ref.current.scrollTop = ref.current.scrollHeight
  }, [lines])
  return (
    <Modal
      wide
      title={`Logs · ${name}`}
      onClose={onClose}
      footer={
        <>
          <label className="checkbox small" style={{ marginRight: 'auto' }}>
            <Toggle checked={auto} onChange={setAuto} label="Auto-refresh" /> Auto-refresh
          </label>
          <button className="btn" onClick={() => A.copy(lines.join('\n'), 'Logs copied')} disabled={lines.length === 0}>
            <Copy size={13} /> Copy
          </button>
          <button className="btn btn-primary" onClick={onClose}>
            Close
          </button>
        </>
      }
    >
      <pre
        ref={ref}
        className="int-code int-logs"
        aria-label={`${name} logs`}
        onScroll={(e) => {
          const el = e.currentTarget
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24
        }}
      >
        {lines.length ? lines.join('\n') : 'No output yet.'}
      </pre>
    </Modal>
  )
}

function ResourceContents({ value }: { value: JsonValue }) {
  const v = value as any
  const list: any[] | null = Array.isArray(v) ? v : v && typeof v === 'object' && Array.isArray(v.contents) ? v.contents : null
  if (!list) return <pre className="int-code">{JSON.stringify(value, null, 2)}</pre>
  if (list.length === 0) return <div className="muted small">The resource is empty.</div>
  return (
    <div className="col int-resource-preview" style={{ gap: 8 }}>
      {list.map((c, i) => {
        const mime = String(c?.mimeType ?? '')
        if (typeof c?.text === 'string') return <pre key={i} className="int-code">{c.text}</pre>
        if (typeof c?.blob === 'string' && mime.startsWith('image/')) return <img key={i} src={`data:${mime};base64,${c.blob}`} alt={String(c.uri ?? 'resource')} />
        if (typeof c?.blob === 'string') return <div key={i} className="muted small">Binary content ({mime || 'unknown type'}, {Math.round((c.blob.length * 3) / 4 / 1024)} KB)</div>
        return <pre key={i} className="int-code">{JSON.stringify(c, null, 2)}</pre>
      })}
    </div>
  )
}

function ResourcePreview({ server, resource, onClose }: { server: string; resource: McpResourceInfo; onClose: () => void }) {
  const [state, setState] = useState<{ value?: JsonValue; error?: string } | null>(null)
  useEffect(() => {
    let alive = true
    call('mcp/readResource', { server, uri: resource.uri })
      .then((r) => alive && setState({ value: r.contents }))
      .catch((e) => alive && setState({ error: errText(e) }))
    return () => {
      alive = false
    }
  }, [server, resource.uri])
  return (
    <Modal
      wide
      title={resource.name || resource.uri}
      onClose={onClose}
      footer={
        <button className="btn btn-primary" onClick={onClose}>
          Close
        </button>
      }
    >
      <div className="xs subtle mono selectable" style={{ marginBottom: 8 }}>
        {resource.uri}
        {resource.mimeType ? ` · ${resource.mimeType}` : ''}
      </div>
      {!state ? <span className="spinner" /> : state.error ? <div className="int-item-error" style={{ margin: 0 }}>{state.error}</div> : <ResourceContents value={state.value ?? null} />}
    </Modal>
  )
}

// ------------------------------------------------------------------ server rows

function ServerDetails(props: { s: McpServerStatus; editable: boolean; onTool: (t: McpToolInfo, field: 'enabled' | 'autoApprove', on: boolean) => void; onPreview: (r: McpResourceInfo) => void }) {
  const { s } = props
  if (s.tools.length === 0 && s.resources.length === 0 && s.prompts.length === 0) {
    return (
      <div className="int-item-body">
        <span className="xs subtle">{s.state === 'ready' ? 'This server exposes no tools, resources or prompts.' : s.state === 'disabled' ? 'Enable the server to see its tools.' : 'Tools appear once the server is ready.'}</span>
      </div>
    )
  }
  return (
    <div className="int-item-body">
      {s.tools.length > 0 && (
        <div>
          <div className="int-sub-title">Tools ({s.tools.length})</div>
          <table className="int-table" aria-label={`${s.name} tools`}>
            <thead>
              <tr>
                <th>Tool</th>
                <th className="int-check">Enabled</th>
                <th className="int-check">Auto-approve</th>
              </tr>
            </thead>
            <tbody>
              {s.tools.map((t) => (
                <tr key={t.name}>
                  <td>
                    <div className="row" style={{ gap: 6, flexWrap: 'wrap' }}>
                      <span className="mono">{t.name}</span>
                      {t.readOnlyHint && (
                        <span className="badge success" title="The server marks this tool as read-only">
                          read-only
                        </span>
                      )}
                      <span className="xs subtle" title="Schema size in the prompt">
                        {formatTokens(t.schemaTokens)} tok
                      </span>
                    </div>
                    {t.description && <div className="int-desc">{t.description}</div>}
                  </td>
                  <td className="int-check">
                    <input type="checkbox" checked={t.enabled} disabled={!props.editable} aria-label={`Enable tool ${t.name}`} onChange={(e) => props.onTool(t, 'enabled', e.target.checked)} />
                  </td>
                  <td className="int-check">
                    <input type="checkbox" checked={t.autoApprove} disabled={!props.editable || !t.enabled} aria-label={`Auto-approve tool ${t.name}`} onChange={(e) => props.onTool(t, 'autoApprove', e.target.checked)} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {s.resources.length > 0 && (
        <div>
          <div className="int-sub-title">Resources ({s.resources.length})</div>
          <table className="int-table" aria-label={`${s.name} resources`}>
            <tbody>
              {s.resources.map((r) => (
                <tr key={r.uri}>
                  <td>
                    <div className="row" style={{ gap: 6 }}>
                      <span className="mono">{r.name}</span>
                      {r.mimeType && <span className="badge">{r.mimeType}</span>}
                    </div>
                    <div className="int-desc mono">{r.uri}</div>
                    {r.description && <div className="int-desc">{r.description}</div>}
                  </td>
                  <td style={{ width: 90, textAlign: 'right' }}>
                    <button className="btn btn-sm btn-ghost" onClick={() => props.onPreview(r)} aria-label={`Preview ${r.name}`}>
                      <Eye size={13} /> Preview
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {s.prompts.length > 0 && (
        <div>
          <div className="int-sub-title">Prompts ({s.prompts.length})</div>
          <table className="int-table" aria-label={`${s.name} prompts`}>
            <tbody>
              {s.prompts.map((p) => (
                <tr key={p.name}>
                  <td>
                    <span className="mono">{p.name}</span>
                    {p.description && <div className="int-desc">{p.description}</div>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {(s.resources.length > 0 || s.prompts.length > 0) && <div className="xs subtle">Mention resources and prompts with @ in the composer.</div>}
    </div>
  )
}

// ------------------------------------------------------------------ panel

export function McpSettings() {
  const servers = useApp((s) => s.mcp)
  const models = useApp((s) => s.models)
  const mainKey = useApp((s) => s.roles.main)
  const threadLazy = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.context?.lazyTools : undefined))
  const [cfg, setCfg] = useState<Record<string, McpServerToml>>({})
  const [lazy, setLazy] = useState('auto')
  const [ratio, setRatio] = useState(0.15)
  const [editing, setEditing] = useState<{ name: string | null; server: McpServerToml } | null>(null)
  const [logsFor, setLogsFor] = useState<string | null>(null)
  const [preview, setPreview] = useState<{ server: string; resource: McpResourceInfo } | null>(null)
  const [expanded, setExpanded] = useState<Record<string, boolean>>({})
  const [busy, setBusy] = useState<Record<string, string | undefined>>({})
  const [refreshing, setRefreshing] = useState(false)

  const reloadConfig = async () => {
    const r = await call('config/read', {})
    const user = (r.user.mcp_servers ?? {}) as Record<string, Partial<McpServerToml> | undefined>
    setCfg(Object.fromEntries(Object.entries(user).map(([k, v]) => [k, normalize(v)])))
    setLazy(r.user.mcp?.lazy_tools ?? 'auto')
    setRatio(r.effective.context?.mcp_tool_budget_ratio ?? 0.15)
  }
  const refresh = async () => {
    setRefreshing(true)
    try {
      const [list] = await Promise.all([call('mcp/list', {}), reloadConfig()])
      setStoreServers(list.servers)
    } catch (e) {
      toast(`MCP: ${errText(e)}`, 'error')
    } finally {
      setRefreshing(false)
    }
  }
  useEffect(() => {
    void refresh()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const act = async (name: string, what: string, fn: () => Promise<void>) => {
    setBusy((b) => ({ ...b, [name]: what }))
    try {
      await fn()
    } catch (e) {
      toast(`${what} ${name}: ${errText(e)}`, 'error')
      await call('mcp/list', {})
        .then((r) => setStoreServers(r.servers))
        .catch(() => {})
    } finally {
      setBusy((b) => ({ ...b, [name]: undefined }))
    }
  }

  const upsert = async (name: string, server: McpServerToml) => {
    const r = await call('mcp/upsert', { name, server })
    setStoreServers(r.servers)
    setCfg((c) => ({ ...c, [name]: server }))
  }

  const setTool = (s: McpServerStatus, t: McpToolInfo, field: 'enabled' | 'autoApprove', on: boolean) => {
    const base = cfg[s.name]
    if (!base) return
    const next: McpServerToml = { ...base, disabled_tools: [...base.disabled_tools], auto_approve_tools: [...base.auto_approve_tools], enabled_tools: base.enabled_tools ? [...base.enabled_tools] : null }
    if (field === 'enabled') {
      if (on) {
        next.disabled_tools = next.disabled_tools.filter((n) => n !== t.name)
        if (next.disabled_tools.includes('*')) next.disabled_tools = s.tools.filter((x) => x.name !== t.name && !x.enabled).map((x) => x.name)
        if (next.enabled_tools && !next.enabled_tools.includes(t.name) && !next.enabled_tools.includes('*')) next.enabled_tools.push(t.name)
      } else if (!next.disabled_tools.includes(t.name)) next.disabled_tools.push(t.name)
    } else if (on) {
      if (!next.auto_approve_tools.includes(t.name)) next.auto_approve_tools.push(t.name)
    } else {
      next.auto_approve_tools = next.auto_approve_tools.includes('*') ? s.tools.filter((x) => x.name !== t.name).map((x) => x.name) : next.auto_approve_tools.filter((n) => n !== t.name)
    }
    // optimistic: the engine confirms with the new status
    patchServer(s.name, (x) => ({ ...x, tools: x.tools.map((tt) => (tt.name === t.name ? { ...tt, [field]: on } : tt)) }))
    void act(s.name, 'Update', () => upsert(s.name, next))
  }

  const setServerEnabled = (s: McpServerStatus, conf: McpServerToml, on: boolean) => {
    patchServer(s.name, (x) => ({ ...x, enabled: on, state: on ? 'starting' : 'disabled', error: null }))
    void act(s.name, on ? 'Enable' : 'Disable', () => upsert(s.name, { ...conf, enabled: on ? null : false }))
  }

  const mainModel = models.find((m) => m.key === mainKey) ?? models[0]
  const window_ = mainModel?.contextWindow ?? 32768
  const schemaTokens = useMemo(() => servers.filter((s) => s.state === 'ready').reduce((n, s) => n + s.tools.filter((t) => t.enabled).reduce((m, t) => m + t.schemaTokens, 0), 0), [servers])
  const budget = Math.round(window_ * ratio)
  const lazyActive = lazy === 'always' || (lazy !== 'never' && schemaTokens > budget)

  return (
    <div className="int-panel">
      <p className="int-intro">
        MCP servers give the agent extra tools, resources and prompts. Their tools reach the model as <code>mcp__server__tool</code> and every call goes through your approval settings. Servers start in parallel; a failing server never blocks a thread.
      </p>
      <section>
        <SectionHead title="Servers">
          <button className="btn btn-sm" disabled={refreshing} onClick={() => void refresh()}>
            <RefreshCw size={13} /> Refresh
          </button>
          <button className="btn btn-sm btn-primary" onClick={() => setEditing({ name: null, server: emptyServer() })}>
            <Plus size={13} /> Add server
          </button>
        </SectionHead>
        {servers.length === 0 && <div className="int-empty">No MCP servers yet. Add one to give the agent more tools.</div>}
        <div className="int-list">
          {servers.map((s) => {
            const st = stateInfo(s.state)
            const conf = cfg[s.name]
            const plugin = !conf
            const open = !!expanded[s.name]
            const isHttp = s.transport === 'http'
            const showLogin = isHttp && (s.state === 'needsAuth' || (!!conf?.oauth && !s.authenticated))
            const enabledTools = s.tools.filter((t) => t.enabled).length
            return (
              <div key={s.name} className={`int-item ${s.state === 'failed' || s.state === 'needsAuth' ? 'attention' : ''}`} data-testid={`mcp-server-${s.name}`}>
                <div className="int-item-head">
                  <button className="int-expander" aria-expanded={open} aria-label={`${open ? 'Collapse' : 'Expand'} ${s.name}`} onClick={() => setExpanded((x) => ({ ...x, [s.name]: !open }))}>
                    {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                  </button>
                  <span className={`dot int-dot ${st.dot}`} aria-hidden />
                  <span className="int-name">{s.name}</span>
                  <div className="int-item-meta">
                    <span className="badge">{isHttp ? 'HTTP' : 'stdio'}</span>
                    {plugin && (
                      <span className="badge accent" title="Provided by an installed plugin">
                        plugin
                      </span>
                    )}
                    {s.state === 'ready' && (
                      <span className="ellipsis">
                        {enabledTools === s.tools.length ? `${s.tools.length} tool${s.tools.length === 1 ? '' : 's'}` : `${enabledTools}/${s.tools.length} tools`}
                        {s.resources.length ? ` · ${s.resources.length} resource${s.resources.length === 1 ? '' : 's'}` : ''}
                        {s.prompts.length ? ` · ${s.prompts.length} prompt${s.prompts.length === 1 ? '' : 's'}` : ''}
                        {s.serverName ? ` · ${s.serverName}${s.serverVersion ? ` ${s.serverVersion}` : ''}` : ''}
                      </span>
                    )}
                  </div>
                  <span className={`badge ${st.badge}`} data-testid="mcp-state">
                    {st.label}
                  </span>
                  <div className="int-item-actions">
                    {busy[s.name] && <span className="spinner" aria-label={`${busy[s.name]} in progress`} />}
                    {showLogin && (
                      <button
                        className="btn btn-sm"
                        disabled={!!busy[s.name]}
                        onClick={() =>
                          void act(s.name, 'Sign in', async () => {
                            const r = await call('mcp/login', { name: s.name })
                            toast(r.message || 'Complete sign-in in your browser.')
                          })
                        }
                      >
                        <LogIn size={13} /> Sign in
                      </button>
                    )}
                    {isHttp && s.authenticated && (
                      <button
                        className="btn btn-sm btn-ghost"
                        disabled={!!busy[s.name]}
                        onClick={() =>
                          void act(s.name, 'Sign out', async () => {
                            await call('mcp/logout', { name: s.name })
                            setStoreServers((await call('mcp/list', {})).servers)
                            toast(`Signed out of ${s.name}`)
                          })
                        }
                      >
                        <LogOut size={13} /> Sign out
                      </button>
                    )}
                    <button
                      className="icon-btn sm"
                      title="Restart"
                      aria-label={`Restart ${s.name}`}
                      disabled={!!busy[s.name] || !s.enabled}
                      onClick={() =>
                        void act(s.name, 'Restart', async () => {
                          setStoreServers((await call('mcp/restart', { name: s.name })).servers)
                        })
                      }
                    >
                      <RotateCw size={13} />
                    </button>
                    <button className="icon-btn sm" title="Logs" aria-label={`Logs for ${s.name}`} onClick={() => setLogsFor(s.name)}>
                      <ScrollText size={13} />
                    </button>
                    {!plugin && (
                      <button className="icon-btn sm" title="Edit" aria-label={`Edit ${s.name}`} onClick={() => setEditing({ name: s.name, server: conf })}>
                        <Pencil size={13} />
                      </button>
                    )}
                    {!plugin && (
                      <button
                        className="icon-btn sm"
                        title="Remove"
                        aria-label={`Remove ${s.name}`}
                        onClick={async () => {
                          if (!(await A.confirmDialog('Remove MCP server', `Remove ${s.name}? Its tools will no longer be available to the agent.`, 'Remove', true))) return
                          void act(s.name, 'Remove', async () => {
                            const r = await call('mcp/remove', { name: s.name })
                            setStoreServers(r.servers)
                            setCfg((c) => {
                              const { [s.name]: _gone, ...rest } = c
                              return rest
                            })
                          })
                        }}
                      >
                        <Trash2 size={13} />
                      </button>
                    )}
                    <span title={plugin ? 'Enable or disable the plugin in Plugins' : undefined}>
                      <Toggle
                        checked={s.enabled}
                        disabled={plugin || !!busy[s.name]}
                        label={`Enable ${s.name}`}
                        onChange={(v) => setServerEnabled(s, conf, v)}
                      />
                    </span>
                  </div>
                </div>
                {s.error && s.state !== 'ready' && s.state !== 'disabled' && <div className="int-item-error selectable">{s.error}</div>}
                {open && <ServerDetails s={s} editable={!plugin} onTool={(t, f, on) => setTool(s, t, f, on)} onPreview={(r) => setPreview({ server: s.name, resource: r })} />}
              </div>
            )
          })}
        </div>
      </section>

      <section>
        <SectionHead title="Tool loading" />
        <div className="int-note">
          <Info size={15} />
          <div className="grow">
            <div>
              Enabled MCP tool schemas use about <b>{formatTokens(schemaTokens)}</b> tokens. The budget is <b>{Math.round(ratio * 100)}%</b> of the {mainModel ? `${mainModel.displayName} ` : ''}context window ({formatTokens(budget)} of {formatTokens(window_)} tokens).{' '}
              {lazyActive ? (
                <>
                  Tools are <b>loaded lazily</b>: the model gets a <code>search_tools</code> tool and activates the MCP tools it needs on demand.
                </>
              ) : (
                <>All enabled tools are sent with every request. Above the budget, Odex switches to lazy loading through <code>search_tools</code>.</>
              )}
              {threadLazy != null && <> The open thread {threadLazy ? 'is using lazy loading' : 'sends tools directly'}.</>}
            </div>
            <div className="row" style={{ marginTop: 8, gap: 8 }}>
              <label htmlFor="mcp-lazy" className="small">
                Lazy loading
              </label>
              <select
                id="mcp-lazy"
                className="select"
                style={{ width: 220 }}
                value={lazy}
                onChange={async (e) => {
                  const v = e.target.value
                  setLazy(v)
                  await call('config/write', { edits: [{ keyPath: 'mcp.lazy_tools', value: v === 'auto' ? null : v }] }).catch((err) => toast(errText(err), 'error'))
                }}
              >
                <option value="auto">Automatic (over budget)</option>
                <option value="always">Always</option>
                <option value="never">Never</option>
              </select>
            </div>
          </div>
        </div>
      </section>

      {editing && (
        <McpServerEditor
          name={editing.name}
          initial={editing.server}
          existing={servers.map((s) => s.name)}
          onClose={() => setEditing(null)}
          onSaved={(name, server, list) => {
            setStoreServers(list)
            setCfg((c) => ({ ...c, [name]: server }))
          }}
        />
      )}
      {logsFor && <LogsModal name={logsFor} onClose={() => setLogsFor(null)} />}
      {preview && <ResourcePreview server={preview.server} resource={preview.resource} onClose={() => setPreview(null)} />}
    </div>
  )
}
