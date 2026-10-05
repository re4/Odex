import { useEffect, useRef, useState } from 'react'
import { Plus, X } from 'lucide-react'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import { WebLinksAddon } from '@xterm/addon-web-links'
import '@xterm/xterm/css/xterm.css'
import { useApp } from '@/store/app'

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
      if (e.type === 'keydown' && e.ctrlKey && e.key === 'c' && t.hasSelection()) {
        void navigator.clipboard.writeText(t.getSelection())
        return false
      }
      if (e.type === 'keydown' && e.ctrlKey && e.key === 'v') {
        void navigator.clipboard.readText().then((x) => window.odex.terminals.write(id, x))
        return false
      }
      if (e.ctrlKey && (e.key === 'j' || e.key === '`' || e.key === 'k' || e.key === 'b')) return false
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

/** Terminal tabs scoped to the selected thread (or global when no thread). */
export function TerminalPanel() {
  const threadId = useApp((s) => s.selectedThreadId)
  const thread = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread : undefined))
  const projects = useApp((s) => s.projects)
  const newProject = useApp((s) => s.ui.newThreadProjectId)
  const [terms, setTerms] = useState<TermInfo[]>([])
  const [active, setActive] = useState<string | null>(null)

  const cwd = thread ? (thread.worktree?.path ?? thread.cwd) : (() => {
    const p = projects.find((x) => x.id === newProject)
    return p ? (p.folders[p.primary] ?? p.folders[0]) : undefined
  })()

  const refresh = async (select?: string) => {
    const list = (await window.odex.terminals.list(threadId ?? null)) as TermInfo[]
    setTerms(list)
    setActive((cur) => select ?? (cur && list.some((t) => t.id === cur) ? cur : (list[list.length - 1]?.id ?? null)))
    return list
  }

  const create = async () => {
    const t = (await window.odex.terminals.create({ threadId: threadId ?? null, cwd })) as TermInfo
    await refresh(t.id)
  }

  useEffect(() => {
    void refresh().then((list) => {
      if (!list.length) void create()
    })
    const off = window.odex.terminals.onExit(() => void refresh())
    return off
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [threadId])

  return (
    <div className="col" style={{ height: '100%', gap: 0 }}>
      <div className="tabs" role="tablist" aria-label="Terminals">
        {terms.map((t) => (
          <button key={t.id} role="tab" aria-selected={t.id === active} className="tab" onClick={() => setActive(t.id)}>
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
      </div>
      <div style={{ position: 'relative', flex: 1, minHeight: 0 }}>
        {terms.map((t) => (
          <XTerm key={t.id} id={t.id} active={t.id === active} />
        ))}
        {!terms.length && <div className="empty">No terminal</div>}
      </div>
    </div>
  )
}
