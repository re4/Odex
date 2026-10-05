import { useState } from 'react'
import { ChevronDown, ChevronRight, FileCode2, Layers } from 'lucide-react'
import type { Environment, Project } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { cleanEnvironments, EnvironmentsEditor } from '@/views/EnvironmentsEditor'
import '@/styles/environments.css'

function primaryFolder(p: Project): string {
  return p.folders[p.primary] ?? p.folders[0] ?? ''
}

/** One project's environments with Save / Revert. Remount (key) after a save to pick up the engine's copy. */
function ProjectEnvironments({ project }: { project: Project }) {
  const [envs, setEnvs] = useState<Environment[]>(project.environments)
  const [def, setDef] = useState(project.defaultEnvironment ?? '')
  const [busy, setBusy] = useState(false)
  const dirty = JSON.stringify(envs) !== JSON.stringify(project.environments) || def !== (project.defaultEnvironment ?? '')
  const save = async () => {
    setBusy(true)
    try {
      await call('project/update', { id: project.id, environments: cleanEnvironments(envs), defaultEnvironment: def || null })
      await useApp.getState().refreshProjects()
      toast(`Saved environments of ${project.name}`, 'success')
    } catch (e) {
      toast(`Could not save: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(false)
    }
  }
  const file = `${primaryFolder(project).replace(/[\\/]+$/, '')}${primaryFolder(project).includes('\\') ? '\\' : '/'}.odex${primaryFolder(project).includes('\\') ? '\\' : '/'}environments.toml`
  return (
    <div className="col" style={{ gap: 10 }}>
      <EnvironmentsEditor envs={envs} onChange={setEnvs} defaultEnv={def} onDefaultChange={setDef} trusted={project.trusted} showHint={false} />
      <div className="row" style={{ gap: 6 }}>
        {project.environments.length > 0 && (
          <button className="btn btn-sm btn-ghost" onClick={() => void window.odex.shell.openInEditor(file)} title={file}>
            <FileCode2 size={12} /> Open environments.toml
          </button>
        )}
        <span className="spacer" />
        <button
          className="btn btn-sm"
          disabled={!dirty || busy}
          onClick={() => {
            setEnvs(project.environments)
            setDef(project.defaultEnvironment ?? '')
          }}
        >
          Revert
        </button>
        <button className="btn btn-sm btn-primary" disabled={!dirty || busy || !project.trusted} onClick={() => void save()}>
          Save
        </button>
      </div>
    </div>
  )
}

/** Settings → Local environments: every project's `.odex/environments.toml`. */
export function EnvironmentsSettings() {
  const projects = useApp((s) => s.projects)
  const [open, setOpen] = useState<Record<string, boolean>>({})
  const isOpen = (p: Project, i: number) => open[p.id] ?? (i === 0 || p.environments.length > 0)
  return (
    <div>
      <p className="small muted" style={{ marginTop: 0 }}>
        Environments prepare a thread's worktree: a setup script (with per-OS variants) runs when the worktree is created, and their variables apply to the setup script, the agent's commands and the thread's terminals. Each project keeps them in <code>.odex/environments.toml</code>, so they can be committed and shared.
      </p>
      {projects.length === 0 && <div className="card empty small">No projects yet. Add a project folder from the sidebar.</div>}
      <div className="col" style={{ gap: 10 }}>
        {projects.map((p, i) => {
          const o = isOpen(p, i)
          const def = p.environments.find((e) => e.id === p.defaultEnvironment) ?? p.environments[0]
          return (
            <div key={p.id} className="card envs-project" role="group" aria-label={`Environments of ${p.name}`}>
              <button className="envs-project-header" aria-expanded={o} onClick={() => setOpen({ ...open, [p.id]: !o })}>
                {o ? <ChevronDown size={14} className="subtle" /> : <ChevronRight size={14} className="subtle" />}
                <Layers size={14} className="subtle" />
                <span style={{ fontWeight: 600 }}>{p.name}</span>
                <span className="xs subtle ellipsis grow" title={primaryFolder(p)}>
                  {primaryFolder(p)}
                </span>
                {!p.trusted && <span className="badge">untrusted</span>}
                <span className="xs muted" style={{ flex: 'none' }}>
                  {p.environments.length === 0 ? 'no environments' : `${p.environments.length} environment${p.environments.length > 1 ? 's' : ''} · default ${def?.name ?? '—'}`}
                </span>
              </button>
              {o && (
                <div className="envs-project-body">
                  <ProjectEnvironments key={`${JSON.stringify(p.environments)}|${p.defaultEnvironment ?? ''}|${p.trusted}`} project={p} />
                </div>
              )}
            </div>
          )
        })}
      </div>
    </div>
  )
}
