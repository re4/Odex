import { useId, useState } from 'react'
import { Plus, Trash2 } from 'lucide-react'
import type { Environment, PerOs } from '@shared/index'
import { currentOs, envLines, OS_LABEL, parseEnvLines, type OsKey } from '@/lib/environments'
import '@/styles/environments.css'

const newId = () => `env_${Math.random().toString(36).slice(2, 8)}`
type ScriptTab = 'default' | OsKey
const TABS: ScriptTab[] = ['default', 'windows', 'macos', 'linux']

/** Read the script shown in a tab. */
function scriptFor(e: Environment, tab: ScriptTab): string {
  return tab === 'default' ? (e.setupScript ?? '') : (e.setupScripts?.[tab] ?? '')
}

/** Environment patch for editing one tab's script (blank per-OS entries are dropped). */
function withScript(e: Environment, tab: ScriptTab, text: string): Partial<Environment> {
  if (tab === 'default') return { setupScript: text || null }
  const next: PerOs = { ...(e.setupScripts ?? {}), [tab]: text || null }
  // always send the object (even empty) so the engine knows per-OS scripts were edited
  return { setupScripts: next }
}

/**
 * Edit a project's environments (`.odex/environments.toml`): name, default, setup script (default +
 * per-OS tabs) and variables. Shared by the project editor and Settings → Local environments.
 */
export function EnvironmentsEditor({
  envs,
  onChange,
  defaultEnv,
  onDefaultChange,
  trusted,
  showHint = true,
}: {
  envs: Environment[]
  onChange: (envs: Environment[]) => void
  defaultEnv: string
  onDefaultChange: (id: string) => void
  trusted: boolean
  /** The explanation above the list (Settings shows its own). */
  showHint?: boolean
}) {
  const group = useId()
  const [tabs, setTabs] = useState<Record<string, ScriptTab>>({})
  // variables are edited as text; parsed on every change but displayed as typed
  const [varText, setVarText] = useState<Record<string, string>>({})
  const os = currentOs()
  const setEnv = (i: number, patch: Partial<Environment>) => onChange(envs.map((e, k) => (k === i ? { ...e, ...patch } : e)))
  const effectiveDefault = envs.some((e) => e.id === defaultEnv) ? defaultEnv : (envs[0]?.id ?? '')

  return (
    <div className="col env-editor" style={{ gap: 10 }}>
      {!trusted && (
        <div className="banner info" style={{ borderRadius: 'var(--radius)' }}>
          This folder is not trusted, so its .odex configuration is ignored and can't be edited. Trust it from the project menu first.
        </div>
      )}
      {showHint && (
        <div className="xs muted">
          An environment's setup script runs in the agent's shell when a worktree is created for a thread, with <code>ODEX_WORKTREE_PATH</code> and <code>ODEX_SOURCE_TREE_PATH</code> set. Its variables apply to the setup script, the agent's commands and the thread's terminals. New threads use the default environment (else the first). Saved to <code>.odex/environments.toml</code>.
        </div>
      )}
      {envs.map((e, i) => {
        const tab = tabs[e.id] ?? 'default'
        const effective = (e.setupScripts?.[os] ?? '').trim() ? os : 'default'
        return (
          <div key={e.id} className="card env-card" aria-label={`Environment ${e.name || e.id}`} role="group">
            <div className="row">
              <input className="input" style={{ maxWidth: 220 }} placeholder="Name" value={e.name} onChange={(ev) => setEnv(i, { name: ev.target.value })} aria-label="Environment name" />
              <label className="checkbox xs">
                <input type="radio" name={`default-env-${group}`} checked={effectiveDefault === e.id} onChange={() => onDefaultChange(e.id)} aria-label={`Default environment ${e.name || e.id}`} /> default
              </label>
              <span className="spacer" />
              <span className="xs subtle mono" title="Environment id">
                {e.id}
              </span>
              <button className="icon-btn sm" aria-label="Delete environment" title="Delete environment" onClick={() => onChange(envs.filter((_, k) => k !== i))}>
                <Trash2 size={13} />
              </button>
            </div>
            <div className="env-script">
              <div className="env-tabs" role="tablist" aria-label="Setup script per OS">
                <span className="env-label">Setup script</span>
                {TABS.map((t) => {
                  const has = !!scriptFor(e, t).trim()
                  return (
                    <button key={t} role="tab" className="env-tab" aria-selected={tab === t} onClick={() => setTabs({ ...tabs, [e.id]: t })} title={t === effective ? 'Runs on this computer' : undefined}>
                      {t === 'default' ? 'Default' : OS_LABEL[t]}
                      {has && <span className={`dot ${t === effective ? 'accent' : ''}`} aria-hidden />}
                    </button>
                  )
                })}
                <span className="spacer" />
                <span className="xs subtle">{effective === 'default' ? 'Default script runs here' : `${OS_LABEL[os]} script runs here`}</span>
              </div>
              <textarea
                className="textarea mono"
                style={{ minHeight: 70 }}
                placeholder={tab === 'default' ? 'Setup script, e.g.\nnpm ci' : `Replaces the default script on ${OS_LABEL[tab]} (leave empty to use the default)`}
                value={scriptFor(e, tab)}
                onChange={(ev) => setEnv(i, withScript(e, tab, ev.target.value))}
                aria-label={tab === 'default' ? 'Setup script' : `${OS_LABEL[tab]} setup script`}
              />
            </div>
            <span className="env-label">Variables</span>
            <textarea
              className="textarea mono"
              style={{ minHeight: 44, marginTop: -4 }}
              placeholder="KEY=value per line"
              value={varText[e.id] ?? envLines(e.env)}
              onChange={(ev) => {
                setVarText({ ...varText, [e.id]: ev.target.value })
                setEnv(i, { env: parseEnvLines(ev.target.value) })
              }}
              aria-label="Environment variables"
            />
          </div>
        )
      })}
      {envs.length === 0 && <div className="card empty small">No environments yet.</div>}
      <div>
        <button className="btn btn-sm" disabled={!trusted} onClick={() => onChange([...envs, { id: newId(), name: '', setupScript: null, setupScripts: null, env: {} }])}>
          <Plus size={13} /> Add environment
        </button>
      </div>
    </div>
  )
}

/** Environments ready to save: unnamed ones get their id as name. */
export function cleanEnvironments(envs: Environment[]): Environment[] {
  return envs.map((e) => ({ ...e, name: e.name.trim() || e.id }))
}
