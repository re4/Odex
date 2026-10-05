import { useEffect, useMemo } from 'react'
import { Hammer, Play, Server, Sparkles, TestTube2 } from 'lucide-react'
import type { ProjectAction } from '@shared/index'
import { useApp } from '@/store/app'
import { toast } from '@/lib/rpc'

const ICON: Record<string, React.ReactNode> = {
  play: <Play size={13} />,
  test: <TestTube2 size={13} />,
  build: <Hammer size={13} />,
  lint: <Sparkles size={13} />,
  server: <Server size={13} />,
}

function join(base: string, rel: string): string {
  if (/^([a-zA-Z]:[\\/]|\/)/.test(rel)) return rel
  return `${base.replace(/[\\/]+$/, '')}/${rel}`
}

/** Run a project action in the integrated terminal (and open its URL in the in-app browser). */
export async function runProjectAction(a: ProjectAction, threadId: string | null, baseCwd: string): Promise<void> {
  const st = useApp.getState()
  const cwd = a.cwd ? join(baseCwd, a.cwd) : baseCwd
  try {
    await window.odex.terminals.run({ threadId, cwd, command: a.command, title: a.name })
  } catch (e) {
    toast(`Could not run ${a.name}: ${(e as Error).message}`, 'error')
    return
  }
  if (st.settings?.terminalLocation === 'right') st.setUi({ sidePanelOpen: true, sidePanelTab: 'terminal' })
  else st.setUi({ bottomOpen: true })
  if (a.openUrl) {
    const url = a.openUrl
    // give dev servers a moment to bind before loading the page
    setTimeout(() => {
      void window.odex.browser.newTab(url)
      useApp.getState().setUi({ sidePanelOpen: true, sidePanelTab: 'browser' })
    }, 2500)
  }
}

export function ProjectActions({ projectId, threadId, cwd }: { projectId: string | null | undefined; threadId: string | null; cwd: string }) {
  const project = useApp((s) => s.projects.find((p) => p.id === projectId))
  const actions = useMemo(() => project?.actions ?? [], [project])

  useEffect(() => {
    const onRun = (e: Event) => {
      const a = actions[(e as CustomEvent<number>).detail ?? 0]
      if (a) void runProjectAction(a, threadId, cwd)
    }
    window.addEventListener('odex:run-action', onRun)
    return () => window.removeEventListener('odex:run-action', onRun)
  }, [actions, threadId, cwd])

  if (!actions.length) return null
  return (
    <div className="row" style={{ gap: 2 }} aria-label="Project actions">
      {actions.slice(0, 4).map((a, i) => (
        <button key={a.id} className="btn btn-sm btn-ghost" title={`${a.command}${i === 0 ? ' (Ctrl+Shift+D)' : ''}`} onClick={() => void runProjectAction(a, threadId, cwd)}>
          {ICON[a.icon ?? 'play'] ?? ICON.play}
          <span className="ellipsis" style={{ maxWidth: 110 }}>
            {a.name}
          </span>
        </button>
      ))}
    </div>
  )
}
