import { useState } from 'react'
import { FolderPlus, Plus, Star, Trash2 } from 'lucide-react'
import type { Environment, Project, ProjectAction } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { Modal } from '@/components/ui'

const ICONS = ['play', 'test', 'build', 'lint', 'server']
const newId = (p: string) => `${p}_${Math.random().toString(36).slice(2, 8)}`

/** Edit a project: name, folders (primary), actions and environments. */
export function ProjectEditor({ project, onClose }: { project: Project; onClose: () => void }) {
  const [name, setName] = useState(project.name)
  const [folders, setFolders] = useState(project.folders)
  const [primary, setPrimary] = useState(project.primary)
  const [actions, setActions] = useState<ProjectAction[]>(project.actions)
  const [envs, setEnvs] = useState<Environment[]>(project.environments)
  const [defaultEnv, setDefaultEnv] = useState(project.defaultEnvironment ?? '')
  const [tab, setTab] = useState<'general' | 'actions' | 'environments'>('general')
  const [busy, setBusy] = useState(false)

  const actionsChanged = JSON.stringify(actions) !== JSON.stringify(project.actions)
  const envsChanged = JSON.stringify(envs) !== JSON.stringify(project.environments)
  const untrustedNote = !project.trusted && (
    <div className="banner info" style={{ borderRadius: 'var(--radius)' }}>
      This folder is not trusted, so its .odex configuration is ignored and can't be edited. Trust it from the project menu first.
    </div>
  )

  const save = async () => {
    setBusy(true)
    try {
      await call('project/update', {
        id: project.id,
        name: name.trim() || project.name,
        folders,
        primary: Math.min(primary, folders.length - 1),
        // the engine only writes .odex/*.toml for trusted projects, so send these only when edited
        actions: actionsChanged ? actions.filter((a) => a.name.trim() && a.command.trim()) : undefined,
        environments: envsChanged ? envs.filter((e) => e.name.trim()) : undefined,
        defaultEnvironment: defaultEnv || null,
      })
      await useApp.getState().refreshProjects()
      onClose()
    } catch (e) {
      toast(`Could not save project: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(false)
    }
  }

  const setAction = (i: number, patch: Partial<ProjectAction>) => setActions(actions.map((a, k) => (k === i ? { ...a, ...patch } : a)))
  const setEnv = (i: number, patch: Partial<Environment>) => setEnvs(envs.map((e, k) => (k === i ? { ...e, ...patch } : e)))

  return (
    <Modal
      title={`Edit ${project.name}`}
      onClose={onClose}
      wide
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" disabled={busy || folders.length === 0} onClick={() => void save()}>
            Save
          </button>
        </>
      }
    >
      <div className="tabs" role="tablist" style={{ marginBottom: 12, padding: 0 }}>
        {(['general', 'actions', 'environments'] as const).map((t) => (
          <button key={t} role="tab" className="tab" aria-selected={tab === t} onClick={() => setTab(t)}>
            {t === 'general' ? 'General' : t === 'actions' ? `Actions (${actions.length})` : `Environments (${envs.length})`}
          </button>
        ))}
      </div>

      {tab === 'general' && (
        <div className="col" style={{ gap: 12 }}>
          <div className="field">
            <label htmlFor="pe-name">Name</label>
            <input id="pe-name" className="input" value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <div className="field">
            <label>Folders</label>
            {folders.map((f, i) => (
              <div key={f} className="row">
                <button className={`icon-btn sm ${i === primary ? 'active' : ''}`} title={i === primary ? 'Primary folder' : 'Make primary'} aria-label={i === primary ? 'Primary folder' : `Make ${f} primary`} onClick={() => setPrimary(i)}>
                  <Star size={13} fill={i === primary ? 'currentColor' : 'none'} />
                </button>
                <span className="mono xs ellipsis grow selectable">{f}</span>
                <button
                  className="icon-btn sm"
                  aria-label={`Remove ${f}`}
                  disabled={folders.length === 1}
                  onClick={() => {
                    setFolders(folders.filter((_, k) => k !== i))
                    if (primary >= i && primary > 0) setPrimary(primary - 1)
                  }}
                >
                  <Trash2 size={13} />
                </button>
              </div>
            ))}
            <div>
              <button
                className="btn btn-sm"
                onClick={async () => {
                  const picked = await window.odex.dialog.openFolder({ multi: true })
                  setFolders([...folders, ...picked.filter((p) => !folders.includes(p))])
                }}
              >
                <FolderPlus size={13} /> Add folder
              </button>
            </div>
            <span className="hint">The primary folder is the default working directory. Other folders are writable roots for the agent.</span>
          </div>
        </div>
      )}

      {tab === 'actions' && (
        <div className="col" style={{ gap: 10 }}>
          {untrustedNote}
          <div className="xs muted">Actions appear in the thread header and run in the integrated terminal. Saved to the project's <code>.odex/actions.toml</code>.</div>
          {actions.map((a, i) => (
            <div key={a.id} className="card" style={{ padding: 10 }}>
              <div className="row">
                <input className="input" style={{ maxWidth: 180 }} placeholder="Name" value={a.name} onChange={(e) => setAction(i, { name: e.target.value })} aria-label="Action name" />
                <select className="select" style={{ maxWidth: 110 }} value={a.icon ?? 'play'} onChange={(e) => setAction(i, { icon: e.target.value })} aria-label="Action icon">
                  {ICONS.map((ic) => (
                    <option key={ic} value={ic}>
                      {ic}
                    </option>
                  ))}
                </select>
                <span className="spacer" />
                {i === 0 && <span className="xs subtle">Ctrl+Shift+D</span>}
                <button className="icon-btn sm" aria-label="Delete action" onClick={() => setActions(actions.filter((_, k) => k !== i))}>
                  <Trash2 size={13} />
                </button>
              </div>
              <input className="input mono" style={{ marginTop: 6 }} placeholder="Command, e.g. npm run dev" value={a.command} onChange={(e) => setAction(i, { command: e.target.value })} aria-label="Action command" />
              <div className="row" style={{ marginTop: 6 }}>
                <input className="input mono" placeholder="Working dir (optional, relative)" value={a.cwd ?? ''} onChange={(e) => setAction(i, { cwd: e.target.value || null })} aria-label="Action working directory" />
                <input className="input mono" placeholder="Open URL when running (optional)" value={a.openUrl ?? ''} onChange={(e) => setAction(i, { openUrl: e.target.value || null })} aria-label="Action URL" />
              </div>
            </div>
          ))}
          <div>
            <button className="btn btn-sm" onClick={() => setActions([...actions, { id: newId('act'), name: '', command: '', icon: 'play', cwd: null, openUrl: null }])}>
              <Plus size={13} /> Add action
            </button>
          </div>
        </div>
      )}

      {tab === 'environments' && (
        <div className="col" style={{ gap: 10 }}>
          {untrustedNote}
          <div className="xs muted">Environments run a setup script when a worktree is created for a thread (PowerShell on Windows, sh elsewhere) and add environment variables. Saved to <code>.odex/environments.toml</code>.</div>
          {envs.map((e, i) => (
            <div key={e.id} className="card" style={{ padding: 10 }}>
              <div className="row">
                <input className="input" style={{ maxWidth: 220 }} placeholder="Name" value={e.name} onChange={(ev) => setEnv(i, { name: ev.target.value })} aria-label="Environment name" />
                <label className="checkbox xs">
                  <input type="radio" name="default-env" checked={defaultEnv === e.id} onChange={() => setDefaultEnv(e.id)} /> default
                </label>
                <span className="spacer" />
                <button className="icon-btn sm" aria-label="Delete environment" onClick={() => setEnvs(envs.filter((_, k) => k !== i))}>
                  <Trash2 size={13} />
                </button>
              </div>
              <textarea className="textarea mono" style={{ marginTop: 6, minHeight: 70 }} placeholder={'Setup script, e.g.\nnpm ci'} value={e.setupScript ?? ''} onChange={(ev) => setEnv(i, { setupScript: ev.target.value || null })} aria-label="Setup script" />
              <textarea
                className="textarea mono"
                style={{ marginTop: 6, minHeight: 44 }}
                placeholder="KEY=value per line"
                value={Object.entries(e.env)
                  .map(([k, v]) => `${k}=${v ?? ''}`)
                  .join('\n')}
                onChange={(ev) =>
                  setEnv(i, {
                    env: Object.fromEntries(
                      ev.target.value
                        .split('\n')
                        .map((l) => l.split('='))
                        .filter((kv) => kv[0]?.trim())
                        .map(([k, ...v]) => [k.trim(), v.join('=')]),
                    ),
                  })
                }
                aria-label="Environment variables"
              />
            </div>
          ))}
          <div>
            <button className="btn btn-sm" onClick={() => setEnvs([...envs, { id: newId('env'), name: '', setupScript: null, env: {} }])}>
              <Plus size={13} /> Add environment
            </button>
          </div>
        </div>
      )}
    </Modal>
  )
}
