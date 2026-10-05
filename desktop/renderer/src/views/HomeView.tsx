import { useEffect, useState } from 'react'
import { FolderPlus, MessageSquare, Sparkles } from 'lucide-react'
import { useApp } from '@/store/app'
import { call } from '@/lib/rpc'
import { Composer } from '@/views/Composer'
import { relativeTime } from '@/components/ui'
import * as A from '@/lib/actions'

const SUGGESTIONS = [
  'Explain the structure of this codebase',
  'Find and fix a bug in the most recently changed file',
  'Write tests for an untested module',
  'Review my uncommitted changes',
]

/** Starter prompts per project for this session (empty list = use the static ones). */
const starterCache = new Map<string, string[]>()
const starterInflight = new Map<string, Promise<string[]>>()
const STARTER_TIMEOUT_MS = 30_000

function fetchStarters(projectId: string): Promise<string[]> {
  const cached = starterCache.get(projectId)
  if (cached) return Promise.resolve(cached)
  let p = starterInflight.get(projectId)
  if (!p) {
    const timeout = new Promise<string[]>((resolve) => setTimeout(() => resolve([]), STARTER_TIMEOUT_MS))
    p = Promise.race([call('project/suggestPrompts', { id: projectId }).then((r) => r.prompts, () => [] as string[]), timeout]).then((list) => {
      starterCache.set(projectId, list)
      starterInflight.delete(projectId)
      return list
    })
    starterInflight.set(projectId, p)
  }
  return p
}

/** Context-aware starter prompts from the utility model when a project is selected, else the static list. */
function useStarterPrompts(projectId: string | null, ready: boolean): { prompts: string[]; generated: boolean } {
  const [state, setState] = useState<{ id: string; list: string[] } | null>(null)
  useEffect(() => {
    if (!projectId || !ready) return
    let off = false
    void fetchStarters(projectId).then((list) => !off && setState({ id: projectId, list }))
    return () => {
      off = true
    }
  }, [projectId, ready])
  const list = projectId ? (state?.id === projectId ? state.list : starterCache.get(projectId)) : undefined
  return list?.length ? { prompts: list, generated: true } : { prompts: SUGGESTIONS, generated: false }
}

export function HomeView() {
  const projects = useApp((s) => s.projects)
  const projectId = useApp((s) => s.ui.newThreadProjectId)
  const threads = useApp((s) => s.threads)
  const order = useApp((s) => s.threadOrder)
  const models = useApp((s) => s.models)
  const engineReady = useApp((s) => s.engine.state === 'ready')
  const project = projects.find((p) => p.id === projectId)
  const starters = useStarterPrompts(project ? project.id : null, engineReady && models.length > 0)
  const recent = order
    .map((id) => threads[id]?.thread)
    .filter((t) => t && !t.archived && (!projectId || t.projectId === projectId))
    .slice(0, 5)

  return (
    <div className="home">
      <h1>{project ? `What should we build in ${project.name}?` : 'What should we work on?'}</h1>
      {models.length === 0 && (
        <div className="banner info" style={{ borderRadius: 'var(--radius)', width: 'min(760px, 100%)' }}>
          <span className="grow">No model endpoint is set up yet.</span>
          <button className="btn btn-sm btn-primary" onClick={() => useApp.getState().setUi({ onboardingOpen: true })}>
            Set up
          </button>
        </div>
      )}
      <div className="composer-wrap">
        <Composer threadId={null} />
      </div>
      <div className="row starter-prompts" style={{ flexWrap: 'wrap', justifyContent: 'center', maxWidth: 760 }} aria-label="Starter prompts" data-generated={starters.generated || undefined}>
        {starters.prompts.map((s) => (
          <button key={s} className="chip" title={s} onClick={() => window.dispatchEvent(new CustomEvent('odex:attach', { detail: { type: 'text', text: s } }))}>
            {starters.generated ? (
              <>
                <Sparkles size={12} />
                <span>{s}</span>
              </>
            ) : (
              s
            )}
          </button>
        ))}
      </div>
      {projects.length === 0 && (
        <button className="btn" onClick={() => void A.addProjectFromDialog()}>
          <FolderPlus size={14} /> Add a project folder
        </button>
      )}
      {recent.length > 0 && (
        <div style={{ width: 'min(760px, 100%)', marginTop: 8 }}>
          <div className="section-title" style={{ marginBottom: 6 }}>
            Recent
          </div>
          {recent.map((t) => (
            <button key={t!.id} className="nav-item" style={{ width: '100%' }} onClick={() => void useApp.getState().selectThread(t!.id)}>
              <MessageSquare size={13} />
              <span className="ellipsis grow" style={{ textAlign: 'left' }}>
                {t!.name || t!.preview || 'New thread'}
              </span>
              <span className="xs subtle">{relativeTime(t!.updatedAt)}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
