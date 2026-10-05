import { useRef, useState } from 'react'
import {
  ArrowLeftRight,
  ClipboardCheck,
  FileText,
  FolderOpen,
  FolderTree,
  Globe,
  GitBranch,
  ListChecks,
  Maximize2,
  Minimize2,
  Plus,
  SquareTerminal,
  X,
} from 'lucide-react'
import { useApp, type SidePanelTab } from '@/store/app'
import { Menu, type MenuItem } from '@/components/ui'
import { openFileInPanel } from '@/views/items'
import { ReviewPanel } from '@/panels/ReviewPanel'
import { PlanPanel } from '@/panels/PlanPanel'
import { SourcesPanel } from '@/panels/SourcesPanel'
import { FilesPanel } from '@/panels/FilesPanel'
import { BrowserPanel } from '@/panels/BrowserPanel'
import { GitPanel } from '@/panels/GitPanel'
import { TerminalPanel } from '@/panels/TerminalPanel'
import '@/styles/sidepanel.css'

const TABS: Array<{ id: SidePanelTab; label: string; icon: React.ReactNode }> = [
  { id: 'review', label: 'Review', icon: <ClipboardCheck size={13} /> },
  { id: 'plan', label: 'Plan', icon: <ListChecks size={13} /> },
  { id: 'sources', label: 'Sources', icon: <FileText size={13} /> },
  { id: 'files', label: 'Files', icon: <FolderTree size={13} /> },
  { id: 'git', label: 'Git', icon: <GitBranch size={13} /> },
  { id: 'browser', label: 'Browser', icon: <Globe size={13} /> },
  { id: 'terminal', label: 'Terminal', icon: <SquareTerminal size={13} /> },
]

/** Tabs in the user's order (drag to reorder); tabs missing from the saved order keep their default place. */
export function orderedTabs(order: SidePanelTab[] | undefined): typeof TABS {
  const known = (order ?? []).filter((id, i, a) => TABS.some((t) => t.id === id) && a.indexOf(id) === i)
  const out = known.map((id) => TABS.find((t) => t.id === id)!)
  TABS.forEach((t, i) => {
    if (out.includes(t)) return
    // insert after the previous default tab that is already placed
    const prev = TABS.slice(0, i).reverse().find((p) => out.includes(p))
    out.splice(prev ? out.indexOf(prev) + 1 : 0, 0, t)
  })
  return out
}

/** Folder new terminals start in: the thread's worktree/cwd, else the project chosen for new threads. */
function terminalCwd(): string | undefined {
  const s = useApp.getState()
  const t = s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread : undefined
  if (t) return t.worktree?.path ?? t.cwd
  const p = s.projects.find((x) => x.id === s.ui.newThreadProjectId)
  return p ? (p.folders[p.primary] ?? p.folders[0]) : undefined
}

export function SidePanel({ width, full }: { width?: number; full?: boolean }) {
  const ui = useApp((s) => s.ui)
  const setUi = useApp((s) => s.setUi)
  const terminalRight = useApp((s) => s.settings?.terminalLocation === 'right')
  const [plusAnchor, setPlusAnchor] = useState<HTMLElement | null>(null)
  const [terminalKey, setTerminalKey] = useState(0)
  const [dragOver, setDragOver] = useState<SidePanelTab | null>(null)
  const dragging = useRef<SidePanelTab | null>(null)
  const ordered = orderedTabs(ui.sidePanelOrder)
  const tabs = ordered.filter((t) => t.id !== 'terminal' || terminalRight || ui.sidePanelTab === 'terminal')
  const tab = ui.sidePanelTab

  const move = (from: SidePanelTab, to: SidePanelTab) => {
    if (from === to) return
    // the dragged tab takes the target's place (after it when moving right, before it when moving left)
    const ids = ordered.map((t) => t.id)
    const target = ids.indexOf(to)
    ids.splice(ids.indexOf(from), 1)
    ids.splice(target, 0, from)
    setUi({ sidePanelOrder: ids })
  }

  const plusItems: MenuItem[] = [
    { label: 'New tab', header: true },
    {
      label: 'Terminal',
      icon: <SquareTerminal size={13} />,
      onSelect: async () => {
        const st = useApp.getState()
        await window.odex.terminals.create({ threadId: st.selectedThreadId ?? null, cwd: terminalCwd() })
        // remount so the terminal tab lists (and selects) the new terminal
        setTerminalKey((n) => n + 1)
        setUi({ sidePanelOpen: true, sidePanelTab: 'terminal' })
      },
    },
    {
      label: 'Browser tab',
      icon: <Globe size={13} />,
      hint: 'Ctrl+T',
      onSelect: () => {
        setUi({ sidePanelOpen: true, sidePanelTab: 'browser' })
        void window.odex.browser.newTab()
      },
    },
    {
      label: 'File…',
      icon: <FolderOpen size={13} />,
      onSelect: async () => {
        const files = await window.odex.dialog.openFiles()
        for (const f of files) openFileInPanel(f)
      },
    },
  ]

  return (
    <aside className={`side-panel ${full ? 'side-panel-full' : ''} ${ui.sidePanelSwap && !full ? 'side-panel-left' : ''}`} style={full ? undefined : { width }} aria-label="Side panel">
      <div className="sp-header">
        <div
          className="tabs sp-tabs"
          role="tablist"
          aria-label="Side panel tabs"
          onWheel={(e) => {
            if (Math.abs(e.deltaY) > Math.abs(e.deltaX)) e.currentTarget.scrollLeft += e.deltaY
          }}
        >
          {tabs.map((t) => (
            <button
              key={t.id}
              role="tab"
              aria-selected={tab === t.id}
              className={`tab ${dragOver === t.id ? 'sp-drop' : ''}`}
              onClick={() => setUi({ sidePanelTab: t.id })}
              draggable
              onDragStart={(e) => {
                dragging.current = t.id
                e.dataTransfer.effectAllowed = 'move'
                e.dataTransfer.setData('application/x-odex-tab', t.id)
              }}
              onDragOver={(e) => {
                if (!dragging.current) return
                e.preventDefault()
                e.dataTransfer.dropEffect = 'move'
                if (dragOver !== t.id) setDragOver(t.id)
              }}
              onDragLeave={() => setDragOver((d) => (d === t.id ? null : d))}
              onDrop={(e) => {
                e.preventDefault()
                const from = dragging.current
                dragging.current = null
                setDragOver(null)
                if (from) move(from, t.id)
              }}
              onDragEnd={() => {
                dragging.current = null
                setDragOver(null)
              }}
            >
              {t.icon}
              {t.label}
            </button>
          ))}
        </div>
        <div className="sp-actions">
          <button className={`icon-btn sm ${plusAnchor ? 'active' : ''}`} aria-label="New side panel tab" title="New tab: terminal, browser or file" onClick={(e) => setPlusAnchor(plusAnchor ? null : e.currentTarget)}>
            <Plus size={13} />
          </button>
          {!full && (
            <button
              className={`icon-btn sm ${ui.sidePanelSwap ? 'active' : ''}`}
              aria-label="Swap chat and panel"
              aria-pressed={ui.sidePanelSwap}
              title="Swap chat and panel sides"
              onClick={() => setUi({ sidePanelSwap: !ui.sidePanelSwap })}
            >
              <ArrowLeftRight size={13} />
            </button>
          )}
          <button
            className={`icon-btn sm ${full ? 'active' : ''}`}
            aria-label={full ? 'Show chat' : 'Full width'}
            aria-pressed={!!full}
            title={full ? 'Show the chat again (Ctrl+Shift+B cycles layouts)' : 'Full width: hide the chat (Ctrl+Shift+B cycles layouts)'}
            onClick={() => setUi({ sidePanelLayout: full ? 'split' : 'full' })}
          >
            {full ? <Minimize2 size={13} /> : <Maximize2 size={13} />}
          </button>
          <button className="icon-btn sm" aria-label="Close side panel" onClick={() => setUi({ sidePanelOpen: false, sidePanelLayout: 'split' })}>
            <X size={13} />
          </button>
        </div>
      </div>
      <div className="panel-body" style={{ position: 'relative', display: 'flex', flexDirection: 'column' }}>
        {tab === 'review' && <ReviewPanel />}
        {tab === 'plan' && <PlanPanel />}
        {tab === 'sources' && <SourcesPanel />}
        {tab === 'files' && <FilesPanel />}
        {tab === 'git' && <GitPanel />}
        {tab === 'browser' && <BrowserPanel />}
        {tab === 'terminal' && <TerminalPanel key={terminalKey} />}
      </div>
      {plusAnchor && <Menu anchor={plusAnchor} items={plusItems} onClose={() => setPlusAnchor(null)} minWidth={190} />}
    </aside>
  )
}
