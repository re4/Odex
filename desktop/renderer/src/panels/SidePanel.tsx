import { ClipboardCheck, FileText, FolderTree, Globe, GitBranch, ListChecks, SquareTerminal, X } from 'lucide-react'
import { useApp, type SidePanelTab } from '@/store/app'
import { ReviewPanel } from '@/panels/ReviewPanel'
import { PlanPanel } from '@/panels/PlanPanel'
import { SourcesPanel } from '@/panels/SourcesPanel'
import { FilesPanel } from '@/panels/FilesPanel'
import { BrowserPanel } from '@/panels/BrowserPanel'
import { GitPanel } from '@/panels/GitPanel'
import { TerminalPanel } from '@/panels/TerminalPanel'

const TABS: Array<{ id: SidePanelTab; label: string; icon: React.ReactNode; needsThread?: boolean }> = [
  { id: 'review', label: 'Review', icon: <ClipboardCheck size={13} /> },
  { id: 'plan', label: 'Plan', icon: <ListChecks size={13} />, needsThread: true },
  { id: 'sources', label: 'Sources', icon: <FileText size={13} />, needsThread: true },
  { id: 'files', label: 'Files', icon: <FolderTree size={13} /> },
  { id: 'git', label: 'Git', icon: <GitBranch size={13} /> },
  { id: 'browser', label: 'Browser', icon: <Globe size={13} /> },
  { id: 'terminal', label: 'Terminal', icon: <SquareTerminal size={13} /> },
]

export function SidePanel({ width }: { width: number }) {
  const ui = useApp((s) => s.ui)
  const setUi = useApp((s) => s.setUi)
  const terminalRight = useApp((s) => s.settings?.terminalLocation === 'right')
  const tabs = TABS.filter((t) => t.id !== 'terminal' || terminalRight || ui.sidePanelTab === 'terminal')
  const tab = ui.sidePanelTab
  return (
    <aside className="side-panel" style={{ width }} aria-label="Side panel">
      <div className="tabs" role="tablist">
        {tabs.map((t) => (
          <button key={t.id} role="tab" aria-selected={tab === t.id} className="tab" onClick={() => setUi({ sidePanelTab: t.id })}>
            {t.icon}
            {t.label}
          </button>
        ))}
        <span className="spacer" />
        <button className="icon-btn sm" aria-label="Close side panel" onClick={() => setUi({ sidePanelOpen: false })}>
          <X size={13} />
        </button>
      </div>
      <div className="panel-body" style={{ position: 'relative', display: 'flex', flexDirection: 'column' }}>
        {tab === 'review' && <ReviewPanel />}
        {tab === 'plan' && <PlanPanel />}
        {tab === 'sources' && <SourcesPanel />}
        {tab === 'files' && <FilesPanel />}
        {tab === 'git' && <GitPanel />}
        {tab === 'browser' && <BrowserPanel />}
        {tab === 'terminal' && <TerminalPanel />}
      </div>
    </aside>
  )
}
