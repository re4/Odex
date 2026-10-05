import { useCallback, useEffect, useRef, useState } from 'react'
import { Activity, Plus, Square, X } from 'lucide-react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import '@xterm/xterm/css/xterm.css'
import type { ExecSessionInfo } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { basename } from '@/components/ui'
import { threadEnvVars } from '@/lib/environments'
import '@/styles/environments.css'

interface TermInfo {
  id: string
  threadId: string | null
  title: string
  cwd: string
  shell: string
  running: boolean
  exitCode: number | null
}

function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim()
}

/** One xterm bound to a main-process PTY. */
function XTerm({ id, active }: { id: string; active: boolean }) {
  const host = useRef<HTMLDivElement>(null)
  const term = useRef<Terminal | null>(null)
  const fit = useRef<FitAddon | null>(null)
  const settings = useApp((s) => s.settings)

  useEffect(() => {
    const t = new Terminal({
      fontFamily: cssVar('--font-code') || 'Consolas, monospace',
      fontSize: Math.max(11, (settings?.fontSize ?? 13) - 1),
      cursorBlink: true,
      allowProposedApi: true,
      scrollback: 5000,
      theme: { background: cssVar('--bg-elev'), foreground: cssVar('--fg'), cursor: cssVar('--fg'), selectionBackground: cssVar('--bg-selected') },
    })
    const f = new FitAddon()
    t.loadAddon(f)
    t.loadAddon(new WebLinksAddon((_e, uri) => void window.odex.shell.openExternal(uri)))
    t.open(host.current!)
    term.current = t
    fit.current = f
    void window.odex.terminals.buffer(id).then((b) => b && t.write(b))
    const offData = window.odex.terminals.onData((d) => d.id === id && t.write(d.data))
    const offExit = window.odex.terminals.onExit((d) => d.id === id && t.write(`\r\n\x1b[2m[process exited with code ${d.exitCode}]\x1b[0m\r\n`))
    const sub = t.onData((data) => void window.odex.terminals.write(id, data))
    t.attachCustomKeyEventHandler((e) => {
      // let app shortcuts through; copy when there's a selection
      if (e.type === 'keydown' && e.ctrlKey && e.key.toLowerCase() === 'c' && t.hasSelection()) {
        void navigator.clipboard.writeText(t.getSelection())
        return false
      }
      if (e.type === 'keydown' && e.ctrlKey && e.key.toLowerCase() === 'v') {
        void navigator.clipboard.readText().then((x) => window.odex.terminals.write(id, x))
        return false
      }
      // Ctrl+L clears the terminal (scrollback included); the prompt line stays
      if (e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && e.key.toLowerCase() === 'l') {
        if (e.type === 'keydown') {
          e.preventDefault()
          t.clear()
        }
        return false
      }
      // app shortcuts pass through (Caps Lock may report upper case)
      if (e.ctrlKey && ['j', '`', 'k', 'b', 'p', ','].includes(e.key.toLowerCase())) return false
      return true
    })
    const ro = new ResizeObserver(() => {
      try {
        f.fit()
        void window.odex.terminals.resize(id, t.cols, t.rows)
      } catch {}
    })
    ro.observe(host.current!)
    return () => {
      ro.disconnect()
      sub.dispose()
      offData()
      offExit()
      t.dispose()
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id])

  useEffect(() => {
    if (active) {
      requestAnimationFrame(() => {
        try {
          fit.current?.fit()
        } catch {}
        term.current?.focus()
      })
    }
  }, [active])

  return <div ref={host} style={{ position: 'absolute', inset: '4px 0 0 8px', display: active ? 'block' : 'none' }} />
}

function duration(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m ${s % 60}s`
  const h = Math.floor(m / 60)
  return `${h}h ${m % 60}m`
}

/** Agent PTY sessions (`exec_command`) in the background, with Kill. */
function useExecSessions(intervalMs: number): [ExecSessionInfo[], () => Promise<void>] {
  const [sessions, setSessions] = useState<ExecSessionInfo[]>([])
  const load = useCallback(async () => {
    try {
      setSessions((await call('exec/sessions', {})).sessions)
    } catch {
      /* engine restarting */
    }
  }, [])
  useEffect(() => {
    void load()
    const t = setInterval(() => void load(), intervalMs)
    return () => clearInterval(t)
  }, [load, intervalMs])
  return [sessions, load]
}

function BackgroundSessions({ sessions, reload, threadId }: { sessions: ExecSessionInfo[]; reload: () => Promise<void>; threadId: string | null }) {
  const threads = useApp((s) => s.threads)
  const [all, setAll] = useState(!threadId)
  const [, tick] = useState(0)
  useEffect(() => {
    const t = setInterval(() => tick((n) => n + 1), 1000)
    return () => clearInterval(t)
  }, [])
  const shown = sessions
    .filter((s) => all || !threadId || s.threadId === threadId)
    .sort((a, b) => Number(b.running) - Number(a.running) || b.startedAt - a.startedAt)
  const kill = async (s: ExecSessionInfo) => {
    try {
      await call('exec/kill', { id: s.id })
      toast(`Stopped ${s.command}`)
    } catch (e) {
      toast(`Could not stop the process: ${(e as Error).message}`, 'error')
    } finally {
      void reload()
    }
  }
  return (
    <div className="bg-sessions" role="region" aria-label="Background processes">
      <div className="row" style={{ marginBottom: 6 }}>
        <span className="xs muted grow">Long-running processes the agent started (exec_command). They keep running between turns.</span>
        {threadId && (
          <label className="checkbox xs">
            <input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} /> All threads
          </label>
        )}
      </div>
      {shown.length === 0 && <div className="empty small">No background processes{threadId && !all ? ' for this thread' : ''}.</div>}
      {shown.map((s) => {
        const owner = s.threadId ? threads[s.threadId]?.thread : undefined
        return (
          <div key={s.id} className={`bg-session ${s.running ? '' : 'exited'}`} data-session={s.id}>
            <span className={`dot ${s.running ? 'success' : ''}`} aria-hidden />
            <div className="grow" style={{ minWidth: 0 }}>
              <div className="cmd ellipsis selectable" title={s.command}>
                {s.command}
              </div>
              <div className="xs subtle row" style={{ gap: 10 }}>
                <span>{s.running ? `running · ${duration(Date.now() - s.startedAt)}` : `exited${s.exitCode != null ? ` with code ${s.exitCode}` : ''}`}</span>
                {s.pid != null && <span>pid {s.pid}</span>}
                <span className="ellipsis" title={s.cwd}>
                  {basename(s.cwd)}
                </span>
                {(all || !threadId) && owner && <span className="ellipsis">{owner.name || owner.preview || 'Thread'}</span>}
              </div>
            </div>
            {s.running && (
              <button className="btn btn-sm btn-danger" aria-label={`Kill ${s.command}`} title="Stop this process" onClick={() => void kill(s)}>
                <Square size={11} /> Kill
              </button>
            )}
          </div>
        )
      })}
    </div>
  )
}

/** Terminal tabs scoped to the selected thread (or global when no thread), plus the agent's background processes. */
export function TerminalPanel() {
  const threadId = useApp((s) => s.selectedThreadId)
  const thread = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread : undefined))
  const projects = useApp((s) => s.projects)
  const newProject = useApp((s) => s.ui.newThreadProjectId)
  const [terms, setTerms] = useState<TermInfo[]>([])
  const [active, setActive] = useState<string | null>(null)
  const [showBg, setShowBg] = useState(false)
  const [sessions, reloadSessions] = useExecSessions(showBg ? 2000 : 5000)
  const running = sessions.filter((s) => s.running && (!threadId || s.threadId === threadId)).length

  const project = projects.find((x) => x.id === (thread ? thread.projectId : newProject))
  const cwd = thread ? (thread.worktree?.path ?? thread.cwd) : project ? (project.folders[project.primary] ?? project.folders[0]) : undefined

  const refresh = async (select?: string) => {
    const list = (await window.odex.terminals.list(threadId ?? null)) as TermInfo[]
    setTerms(list)
    const has = (id: string | null | undefined) => !!id && list.some((t) => t.id === id)
    setActive((cur) => (has(select) ? select! : has(cur) ? cur : (list[list.length - 1]?.id ?? null)))
    return list
  }

  const create = async () => {
    // the thread's environment variables (Settings → Local environments) apply to its terminals
    const t = (await window.odex.terminals.create({ threadId: threadId ?? null, cwd, env: threadEnvVars(thread, project) })) as TermInfo
    setShowBg(false)
    await refresh(t.id)
  }

  useEffect(() => {
    void refresh().then((list) => {
      if (!list.length) void create()
    })
    const off = window.odex.terminals.onExit(() => void refresh())
    // terminals opened elsewhere (project actions) become the active tab
    const onCreated = (e: Event) => {
      setShowBg(false)
      void refresh((e as CustomEvent<string>).detail)
    }
    window.addEventListener('odex:terminal-created', onCreated)
    return () => {
      off()
      window.removeEventListener('odex:terminal-created', onCreated)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [threadId])

  return (
    <div className="col" style={{ height: '100%', gap: 0 }}>
      <div className="tabs" role="tablist" aria-label="Terminals">
        {terms.map((t) => (
          <button
            key={t.id}
            role="tab"
            aria-selected={!showBg && t.id === active}
            className="tab"
            onClick={() => {
              setShowBg(false)
              setActive(t.id)
            }}
          >
            <span className="ellipsis" style={{ maxWidth: 140 }}>
              {t.title}
            </span>
            {!t.running && <span className="xs subtle">exited</span>}
            <span
              role="button"
              aria-label={`Close ${t.title}`}
              className="icon-btn sm"
              onClick={async (e) => {
                e.stopPropagation()
                await window.odex.terminals.kill(t.id)
                await refresh()
              }}
            >
              <X size={11} />
            </span>
          </button>
        ))}
        <button className="icon-btn sm" aria-label="New terminal" title="New terminal" onClick={() => void create()}>
          <Plus size={13} />
        </button>
        <button
          aria-pressed={showBg}
          className="tab term-bg-tab"
          title="Processes the agent left running in the background"
          onClick={() => {
            setShowBg(!showBg)
            void reloadSessions()
          }}
        >
          <Activity size={12} /> Background
          {running > 0 && <span className="badge accent count">{running}</span>}
        </button>
      </div>
      <div style={{ position: 'relative', flex: 1, minHeight: 0 }}>
        {terms.map((t) => (
          <XTerm key={t.id} id={t.id} active={!showBg && t.id === active} />
        ))}
        {!terms.length && !showBg && <div className="empty">No terminal</div>}
        {showBg && <BackgroundSessions sessions={sessions} reload={reloadSessions} threadId={threadId ?? null} />}
      </div>
    </div>
  )
}
