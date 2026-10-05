import { FolderPlus, MessageSquare } from 'lucide-react'
import { useApp } from '@/store/app'
import { Composer } from '@/views/Composer'
import { relativeTime } from '@/components/ui'
import * as A from '@/lib/actions'

const SUGGESTIONS = [
  'Explain the structure of this codebase',
  'Find and fix a bug in the most recently changed file',
  'Write tests for an untested module',
  'Review my uncommitted changes',
]

export function HomeView() {
  const projects = useApp((s) => s.projects)
  const projectId = useApp((s) => s.ui.newThreadProjectId)
  const threads = useApp((s) => s.threads)
  const order = useApp((s) => s.threadOrder)
  const models = useApp((s) => s.models)
  const project = projects.find((p) => p.id === projectId)
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
      <div className="row" style={{ flexWrap: 'wrap', justifyContent: 'center', maxWidth: 760 }}>
        {SUGGESTIONS.map((s) => (
          <button key={s} className="chip" onClick={() => window.dispatchEvent(new CustomEvent('odex:attach', { detail: { type: 'text', text: s } }))}>
            {s}
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
