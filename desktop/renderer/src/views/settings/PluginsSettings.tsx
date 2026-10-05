import { useEffect, useState } from 'react'
import { Download, FolderOpen, Info, Package, RefreshCw, ShieldAlert, ShieldCheck, Trash2 } from 'lucide-react'
import type { HookToml, HooksToml, PluginInfo, PluginManifest } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Modal, Toggle } from '@/components/ui'
import { SectionHead } from '@/views/settings/IntegrationsShared'

const HOOK_KEYS: Array<[keyof HooksToml, string]> = [
  ['session_start', 'Session start'],
  ['user_prompt_submit', 'User prompt submit'],
  ['pre_tool_use', 'Before tool use'],
  ['post_tool_use', 'After tool use'],
  ['stop', 'Stop'],
  ['notification', 'Notification'],
]

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

function pluginHooks(m: PluginManifest): Array<{ event: string; hook: HookToml }> {
  const out: Array<{ event: string; hook: HookToml }> = []
  for (const [k, label] of HOOK_KEYS) for (const hook of (m.hooks?.[k] as HookToml[] | undefined) ?? []) out.push({ event: label, hook })
  return out
}

function quoteArg(a: string): string {
  return /[\s"]/.test(a) ? `"${a.replace(/"/g, '\\"')}"` : a
}

/** The review covered the plugin's hook commands, so trust them along with the plugin. */
async function trustPluginHooks(id: string): Promise<void> {
  const { hooks } = await call('hooks/list', { cwd: null })
  for (const h of hooks) if (h.source === `plugin:${id}` && h.trust !== 'trusted') await call('hooks/trust', { id: h.id, hash: h.hash, trusted: true })
  const pending = useApp.getState().hooksNeedingReview.filter((h) => h.source !== `plugin:${id}`)
  useApp.setState({ hooksNeedingReview: pending })
}

async function refreshMcp(): Promise<void> {
  await call('mcp/list', {})
    .then((r) => useApp.setState({ mcp: r.servers }))
    .catch(() => {})
}

/** Everything the plugin adds, so the user can decide before it runs. */
function PluginReview(props: { plugin: PluginInfo; onClose: () => void; onChanged: () => void }) {
  const p = props.plugin
  const m = p.manifest
  const servers = Object.entries(m.mcpServers ?? {})
  const hooks = pluginHooks(m)
  const skillDirs = m.skills?.length ? m.skills : ['skills']
  const actions = m.actions ?? []
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const decide = async (trusted: boolean) => {
    setBusy(true)
    setError(null)
    try {
      await call('plugins/trust', { id: p.id, hash: p.hash, trusted })
      if (trusted) await trustPluginHooks(p.id)
      await refreshMcp()
      toast(trusted ? `${m.name} is trusted and enabled` : `${m.name} stays disabled`, trusted ? 'success' : 'info')
      props.onChanged()
      props.onClose()
    } catch (e) {
      setError(errText(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      wide
      title={`Review ${m.name}`}
      onClose={props.onClose}
      footer={
        <>
          <button className="btn" disabled={busy} onClick={() => void decide(false)}>
            Keep disabled
          </button>
          <button className="btn btn-primary" disabled={busy} onClick={() => void decide(true)}>
            <ShieldCheck size={13} /> Trust and enable
          </button>
        </>
      }
    >
      <div className="int-form">
        <div>
          <div className="row" style={{ gap: 6 }}>
            <b>{m.name}</b>
            {m.version && <span className="badge">v{m.version}</span>}
            {m.author && <span className="xs subtle">by {m.author}</span>}
          </div>
          {m.description && <div className="small muted" style={{ marginTop: 4 }}>{m.description}</div>}
          <div className="xs subtle mono selectable" style={{ marginTop: 4, wordBreak: 'break-all' }}>
            From {p.source}
          </div>
        </div>
        <div className="int-review">
          <div className="int-review-head">
            <ShieldAlert size={16} color="var(--warning)" />
            <b>Only trust plugins from sources you trust.</b>
          </div>
          <div className="small muted">Trusting lets Odex run this plugin’s MCP servers and hooks (listed below) on your computer with your permissions, and adds its skills and actions. Nothing runs until you trust it.</div>
        </div>

        <section aria-label="MCP servers">
          <div className="int-sub-title">MCP servers ({servers.length})</div>
          {servers.length === 0 ? (
            <div className="xs subtle">None</div>
          ) : (
            <ul className="int-review-list">
              {servers.map(([name, s]) => (
                <li key={name}>
                  <span className="small">
                    <b className="mono">{name}</b> <span className="badge">{s?.url ? 'HTTP' : 'stdio'}</span>
                    {s?.env && Object.keys(s.env).length > 0 && <span className="xs subtle"> · env {Object.keys(s.env).join(', ')}</span>}
                  </span>
                  <pre className="int-code">{s?.url ? s.url : [s?.command ?? '', ...(s?.args ?? [])].map(quoteArg).join(' ')}</pre>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section aria-label="Hooks">
          <div className="int-sub-title">Hooks ({hooks.length})</div>
          {hooks.length === 0 ? (
            <div className="xs subtle">None</div>
          ) : (
            <ul className="int-review-list">
              {hooks.map(({ event, hook }, i) => (
                <li key={i}>
                  <span className="small">
                    <b>{event}</b>
                    {hook.name ? ` · ${hook.name}` : ''}
                    {hook.matcher ? (
                      <span className="xs subtle">
                        {' '}
                        · tools matching <code>{hook.matcher}</code>
                      </span>
                    ) : null}
                  </span>
                  <pre className="int-code">{hook.command}</pre>
                </li>
              ))}
            </ul>
          )}
        </section>

        {actions.length > 0 && (
          <section aria-label="Project actions">
            <div className="int-sub-title">Project actions ({actions.length})</div>
            <ul className="int-review-list">
              {actions.map((a) => (
                <li key={a.id}>
                  <span className="small">
                    <b>{a.name}</b>
                  </span>
                  <pre className="int-code">{a.command}</pre>
                </li>
              ))}
            </ul>
          </section>
        )}

        <section aria-label="Skills">
          <div className="int-sub-title">Skills</div>
          <div className="xs muted">
            Instructions loaded from {skillDirs.map((d, i) => (
              <span key={d}>
                {i > 0 ? ', ' : ''}
                <code>{d}/</code>
              </span>
            ))}{' '}
            in the plugin folder. Their names and descriptions join the prompt.
          </div>
        </section>
        <div className="xs subtle">
          Reviewed content hash <span className="mono">{p.hash}</span>. If any plugin file changes, it needs review again.
        </div>
        {error && (
          <div className="int-item-error" style={{ margin: 0 }} role="alert">
            {error}
          </div>
        )}
      </div>
    </Modal>
  )
}

export function PluginsSettings() {
  const [plugins, setPlugins] = useState<PluginInfo[] | null>(null)
  const [source, setSource] = useState('')
  const [installing, setInstalling] = useState(false)
  const [installError, setInstallError] = useState<string | null>(null)
  const [review, setReview] = useState<PluginInfo | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const load = async () => {
    try {
      setPlugins((await call('plugins/list', {})).plugins)
    } catch (e) {
      toast(`Plugins: ${errText(e)}`, 'error')
      setPlugins((p) => p ?? [])
    }
  }
  useEffect(() => {
    void load()
  }, [])

  const install = async () => {
    const src = source.trim()
    if (!src) return
    setInstalling(true)
    setInstallError(null)
    try {
      const r = await call('plugins/install', { source: src })
      setSource('')
      await load()
      setReview(r.plugin)
    } catch (e) {
      setInstallError(errText(e))
    } finally {
      setInstalling(false)
    }
  }

  const setEnabled = async (p: PluginInfo, enabled: boolean) => {
    if (enabled && !p.trusted) {
      setReview(p)
      return
    }
    setBusy(p.id)
    try {
      setPlugins((await call('plugins/setEnabled', { name: p.id, enabled })).plugins)
      await refreshMcp()
    } catch (e) {
      toast(errText(e), 'error')
    } finally {
      setBusy(null)
    }
  }

  const remove = async (p: PluginInfo) => {
    if (!(await A.confirmDialog('Remove plugin', `Remove ${p.manifest.name}? Its skills, MCP servers, hooks and actions go away and its folder is deleted.`, 'Remove', true))) return
    setBusy(p.id)
    try {
      await call('plugins/remove', { id: p.id })
      await Promise.all([load(), refreshMcp()])
      toast(`Removed ${p.manifest.name}`)
    } catch (e) {
      toast(errText(e), 'error')
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className="int-panel">
      <p className="int-intro">
        Plugins are local bundles of skills, MCP servers, hooks and project actions described by an <code>odex-plugin.toml</code> manifest. Install one from a folder or a git URL; nothing in it runs until you review and trust it.
      </p>
      <section>
        <SectionHead title="Install a plugin" />
        <div className="row" style={{ gap: 6 }}>
          <input
            className="input mono"
            value={source}
            onChange={(e) => setSource(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && void install()}
            placeholder="Folder path or git URL (https://…/plugin.git)"
            aria-label="Plugin folder or git URL"
            autoComplete="off"
          />
          <button
            className="btn btn-sm"
            onClick={async () => {
              const [dir] = await window.odex.dialog.openFolder()
              if (dir) setSource(dir)
            }}
          >
            <FolderOpen size={13} /> Browse
          </button>
          <button className="btn btn-sm btn-primary" disabled={installing || !source.trim()} onClick={() => void install()}>
            <Download size={13} /> {installing ? 'Installing…' : 'Install'}
          </button>
        </div>
        {installError && (
          <div className="int-item-error" style={{ margin: '8px 0 0' }} role="alert">
            {installError}
          </div>
        )}
      </section>

      <section>
        <SectionHead title={`Installed${plugins?.length ? ` (${plugins.length})` : ''}`}>
          <button className="btn btn-sm" onClick={() => void load()}>
            <RefreshCw size={13} /> Refresh
          </button>
        </SectionHead>
        {plugins == null ? (
          <span className="spinner" />
        ) : plugins.length === 0 ? (
          <div className="int-empty">No plugins installed.</div>
        ) : (
          <div className="int-list">
            {plugins.map((p) => {
              const m = p.manifest
              const nServers = Object.keys(m.mcpServers ?? {}).length
              const nHooks = pluginHooks(m).length
              const nActions = (m.actions ?? []).length
              return (
                <div key={p.id} className={`int-item ${p.trusted ? '' : 'attention'}`} data-testid={`plugin-${p.id}`}>
                  <div className="int-item-head">
                    <Package size={16} style={{ flex: 'none', color: 'var(--fg-muted)', marginLeft: 1 }} />
                    <span className="int-name">{m.name}</span>
                    <div className="int-item-meta">
                      {m.version && <span>v{m.version}</span>}
                      {m.author && <span className="ellipsis">· {m.author}</span>}
                    </div>
                    {p.trusted ? (
                      <span className="badge success">Trusted</span>
                    ) : (
                      <button className="btn btn-sm" onClick={() => setReview(p)}>
                        <ShieldAlert size={13} color="var(--warning)" /> Review
                      </button>
                    )}
                    <div className="int-item-actions">
                      {busy === p.id && <span className="spinner" />}
                      {p.trusted && (
                        <button className="icon-btn sm" title="Review contents" aria-label={`Review ${m.name}`} onClick={() => setReview(p)}>
                          <ShieldCheck size={13} />
                        </button>
                      )}
                      <button className="icon-btn sm" title="Show folder" aria-label={`Show ${m.name} folder`} onClick={() => void window.odex.shell.openPath(p.path)}>
                        <FolderOpen size={13} />
                      </button>
                      <button className="icon-btn sm" title="Remove" aria-label={`Remove ${m.name}`} disabled={busy === p.id} onClick={() => void remove(p)}>
                        <Trash2 size={13} />
                      </button>
                      <Toggle checked={p.enabled && p.trusted} disabled={busy === p.id} label={`Enable ${m.name}`} onChange={(v) => void setEnabled(p, v)} />
                    </div>
                  </div>
                  <div className="col" style={{ gap: 6, padding: '0 12px 10px 35px' }}>
                    {m.description && <div className="small muted">{m.description}</div>}
                    <div className="int-provides" aria-label="Provides">
                      <span className="badge">{(m.skills?.length ?? 0) > 0 ? `${m.skills.length} skill folder${m.skills.length === 1 ? '' : 's'}` : 'skills/ folder'}</span>
                      <span className="badge">
                        {nServers} MCP server{nServers === 1 ? '' : 's'}
                      </span>
                      <span className="badge">
                        {nHooks} hook{nHooks === 1 ? '' : 's'}
                      </span>
                      {nActions > 0 && (
                        <span className="badge">
                          {nActions} action{nActions === 1 ? '' : 's'}
                        </span>
                      )}
                    </div>
                    {!p.trusted && <div className="xs int-warning-text">Not trusted: review what it adds before enabling it. A plugin whose files changed since review needs a new review.</div>}
                    <div className="xs subtle mono ellipsis" title={p.source}>
                      {p.source}
                    </div>
                  </div>
                </div>
              )
            })}
          </div>
        )}
      </section>

      <div className="int-note">
        <Info size={15} />
        <div>
          A plugin folder contains <code>odex-plugin.toml</code> with <code>name</code>, <code>version</code> and <code>description</code>, plus optional <code>skills</code> folders, <code>[mcp_servers.&lt;name&gt;]</code> tables, <code>[[hooks.&lt;event&gt;]]</code> entries and <code>[[actions]]</code>.
        </div>
      </div>
      {review && <PluginReview plugin={review} onClose={() => setReview(null)} onChanged={() => void load()} />}
    </div>
  )
}
