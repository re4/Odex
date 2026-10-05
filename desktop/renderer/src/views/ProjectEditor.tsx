import { useState } from 'react'
import { ChevronDown, ChevronRight, FolderPlus, Plus, Star, Trash2 } from 'lucide-react'
import type { Environment, PerOs, Project, ProjectAction } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { Modal } from '@/components/ui'
import { currentOs, OS_LABEL, type OsKey } from '@/lib/environments'
import { cleanEnvironments, EnvironmentsEditor } from '@/views/EnvironmentsEditor'
import '@/styles/environments.css'

const ICONS = ['play', 'test', 'build', 'lint', 'server']
const OSES: OsKey[] = ['windows', 'macos', 'linux']
const newId = (p: string) => `${p}_${Math.random().toString(36).slice(2, 8)}`
const hasPerOs = (c: PerOs | null | undefined) => OSES.some((o) => !!c?.[o]?.trim())

/** Edit a project: name, folders (primary), editor override, actions and environments. */
export function ProjectEditor({ project, onClose, initialTab = 'general' }: { project: Project; onClose: () => void; initialTab?: 'general' | 'actions' | 'environments' }) {
  const [name, setName] = useState(project.name)
  const [folders, setFolders] = useState(project.folders)
  const [primary, setPrimary] = useState(project.primary)
  const [actions, setActions] = useState<ProjectAction[]>(project.actions)
  const [envs, setEnvs] = useState<Environment[]>(project.environments)
  const [defaultEnv, setDefaultEnv] = useState(project.defaultEnvironment ?? '')
  const globalEditor = useApp((s) => s.settings?.editor ?? '')
  const savedEditor = useApp((s) => s.settings?.projectEditors?.[project.id] ?? '')
  const [editor, setEditor] = useState(savedEditor)
  const [tab, setTab] = useState<'general' | 'actions' | 'environments'>(initialTab)
  const [perOsOpen, setPerOsOpen] = useState<Record<string, boolean>>({})
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
        actions: actionsChanged ? actions.filter((a) => a.name.trim() && (a.command.trim() || hasPerOs(a.commands))) : undefined,
        environments: envsChanged ? cleanEnvironments(envs) : undefined,
        defaultEnvironment: defaultEnv || null,
      })
      if (editor.trim() !== savedEditor) {
        const map = { ...(useApp.getState().settings?.projectEditors ?? {}) }
        if (editor.trim()) map[project.id] = editor.trim()
        else delete map[project.id]
        await useApp.getState().setSettings({ projectEditors: map })
      }
      await useApp.getState().refreshProjects()
      onClose()
    } catch (e) {
      toast(`Could not save project: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(false)
    }
  }

  const setAction = (i: number, patch: Partial<ProjectAction>) => setActions(actions.map((a, k) => (k === i ? { ...a, ...patch } : a)))
  const setActionOs = (i: number, os: OsKey, value: string) => setAction(i, { commands: { ...(actions[i].commands ?? {}), [os]: value || null } })
  const os = currentOs()

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
          <div className="field">
            <label htmlFor="pe-editor">Open files with</label>
            <input id="pe-editor" className="input mono" value={editor} onChange={(e) => setEditor(e.target.value)} placeholder={globalEditor ? `${globalEditor} (Settings → General)` : 'code -g {file}:{line}'} />
            <span className="hint">Editor command for files of this project (and its worktrees), overriding Settings → General. {'{file}'} and {'{line}'} are replaced; leave empty to use the global editor. Stored in this app's settings, not in the repository.</span>
          </div>
        </div>
      )}

      {tab === 'actions' && (
        <div className="col" style={{ gap: 10 }}>
          {untrustedNote}
          <div className="xs muted">Actions appear in the thread header and run in the integrated terminal. A per-OS command replaces the default on that OS. Saved to the project's <code>.odex/actions.toml</code>.</div>
          {actions.map((a, i) => {
            const open = perOsOpen[a.id] ?? hasPerOs(a.commands)
            return (
              <div key={a.id} className="card" style={{ padding: 10 }} role="group" aria-label={`Action ${a.name || i + 1}`}>
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
                <button className="btn btn-sm btn-ghost" style={{ marginTop: 4, paddingLeft: 2 }} aria-expanded={open} onClick={() => setPerOsOpen({ ...perOsOpen, [a.id]: !open })}>
                  {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />} Per-OS commands{hasPerOs(a.commands) ? ` (${OSES.filter((o) => a.commands?.[o]?.trim()).map((o) => OS_LABEL[o]).join(', ')})` : ''}
                </button>
                {open && (
                  <div className="os-commands">
                    {OSES.map((o) => (
                      <div key={o} style={{ display: 'contents' }}>
                        <label htmlFor={`pe-cmd-${a.id}-${o}`}>
                          {OS_LABEL[o]}
                          {o === os ? ' (this computer)' : ''}
                        </label>
                        <input id={`pe-cmd-${a.id}-${o}`} className="input mono" placeholder="Same as the command above" value={a.commands?.[o] ?? ''} onChange={(e) => setActionOs(i, o, e.target.value)} aria-label={`${OS_LABEL[o]} command`} />
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )
          })}
          <div>
            <button className="btn btn-sm" onClick={() => setActions([...actions, { id: newId('act'), name: '', command: '', icon: 'play', cwd: null, openUrl: null, commands: null }])}>
              <Plus size={13} /> Add action
            </button>
          </div>
        </div>
      )}

      {tab === 'environments' && <EnvironmentsEditor envs={envs} onChange={setEnvs} defaultEnv={defaultEnv} onDefaultChange={setDefaultEnv} trusted={project.trusted} />}
    </Modal>
  )
}
