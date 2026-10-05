import { memo, useMemo, useState } from 'react'
import {
  Archive,
  Bell,
  ChevronDown,
  ChevronRight,
  CircleAlert,
  Clock,
  FolderPlus,
  GitBranch,
  MessageSquarePlus,
  MoreHorizontal,
  Pin,
  Plus,
  Search,
  Settings,
  Zap,
} from 'lucide-react'
import type { Project, Thread } from '@shared/index'
import { useApp } from '@/store/app'
import * as A from '@/lib/actions'
import { call } from '@/lib/rpc'
import { Menu, MenuItem, relativeTime } from '@/components/ui'

function StateIcon({ t }: { t: Thread }) {
  if (t.status === 'waitingApproval') return <CircleAlert size={13} color="var(--warning)" aria-label="Needs approval" />
  if (t.status === 'running' || t.status === 'compacting') return <span className="spinner" style={{ width: 11, height: 11 }} aria-label="Running" />
  if (t.status === 'reconnecting') return <span className="spinner" style={{ width: 11, height: 11, borderTopColor: 'var(--warning)' }} aria-label="Reconnecting" />
  if (t.status === 'error') return <span className="dot" style={{ background: 'var(--danger)' }} aria-label="Error" />
  if (t.unread) return <span className="dot accent" aria-label="Unread" />
  return null
}

export function threadMenu(t: Thread): MenuItem[] {
  return [
    { label: 'Rename', onSelect: () => void A.renameThread(t.id), hint: 'Ctrl+Alt+R' },
    { label: t.pinned ? 'Unpin' : 'Pin', onSelect: () => void A.togglePin(t.id), hint: 'Ctrl+Alt+P' },
    { label: t.unread ? 'Mark as read' : 'Mark as unread', onSelect: () => void A.markUnread(t.id, !t.unread), hint: 'Ctrl+Shift+U' },
    { separator: true, label: '' },
    { label: 'Fork to new thread', onSelect: () => void A.forkThread(t.id) },
    { label: 'Fork to worktree', onSelect: () => void A.forkThread(t.id, undefined, 'worktree') },
    { label: 'Open in new window', onSelect: () => void window.odex.win.newWindow(t.id) },
    { separator: true, label: '' },
    { label: 'Copy deep link', onSelect: () => A.copy(`odex://threads/${t.id}`, 'Deep link copied') },
    { label: 'Copy thread id', onSelect: () => A.copy(t.id, 'Thread id copied') },
    { label: 'Copy working directory', onSelect: () => A.copy(t.cwd, 'Path copied') },
    { label: 'Show working directory', onSelect: () => void window.odex.shell.openPath(t.cwd) },
    { separator: true, label: '' },
    { label: 'Archive', onSelect: () => void A.archiveThread(t.id), danger: true, hint: 'Ctrl+Shift+A' },
  ]
}

const ThreadRow = memo(function ThreadRow({ t, active }: { t: Thread; active: boolean }) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null)
  const select = useApp((s) => s.selectThread)
  const title = t.name || t.preview || 'New thread'
  return (
    <>
      <button
        className={`thread-row ${active ? 'active' : ''} ${t.unread ? 'unread' : ''}`}
        onClick={() => void select(t.id)}
        onContextMenu={(e) => {
          e.preventDefault()
          setAnchor(e.currentTarget)
        }}
        title={title}
        aria-current={active ? 'page' : undefined}
      >
        <span className="state">
          <StateIcon t={t} />
        </span>
        <span className="name grow ellipsis">{title}</span>
        {t.worktree && <GitBranch size={12} className="subtle" aria-label="worktree" />}
        {t.kind === 'automation' && <Clock size={12} className="subtle" aria-label="automation" />}
        <span className="when">{relativeTime(t.updatedAt)}</span>
        <span
          className="icon-btn sm more"
          role="button"
          tabIndex={-1}
          aria-label="Thread actions"
          onClick={(e) => {
            e.stopPropagation()
            setAnchor(e.currentTarget as HTMLElement)
          }}
        >
          <MoreHorizontal size={14} />
        </span>
      </button>
      {anchor && <Menu anchor={anchor} items={threadMenu(t)} onClose={() => setAnchor(null)} />}
    </>
  )
})

function ProjectSection({ p, threads, selected }: { p: Project; threads: Thread[]; selected: string | null }) {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null)
  const [showAll, setShowAll] = useState(false)
  const setUi = useApp((s) => s.setUi)
  const selectThread = useApp((s) => s.selectThread)
  const collapsed = p.collapsed
  const visible = showAll ? threads : threads.slice(0, 12)
  const newThread = () => {
    setUi({ newThreadProjectId: p.id })
    void selectThread(null)
  }
  const items: MenuItem[] = [
    { label: 'New thread', onSelect: newThread },
    { label: 'New thread in worktree', onSelect: () => (setUi({ newThreadProjectId: p.id, newThreadRunMode: 'worktree' }), void selectThread(null)) },
    { separator: true, label: '' },
    { label: 'Edit project…', onSelect: () => A.openSettings(`project:${p.id}`) },
    { label: 'Open in file manager', onSelect: () => void window.odex.shell.openPath(p.folders[p.primary] ?? p.folders[0]) },
    { label: p.trusted ? 'Untrust folder' : 'Trust folder', onSelect: () => void call('trust/set', { path: p.folders[p.primary] ?? p.folders[0], trusted: !p.trusted }) },
    { separator: true, label: '' },
    {
      label: 'Archive all threads',
      onSelect: async () => {
        if (await A.confirmDialog('Archive all threads', `Archive ${threads.length} thread(s) in ${p.name}?`, 'Archive all')) {
          for (const t of threads) await call('thread/archive', { threadId: t.id, removeWorktree: false })
        }
      },
    },
    { label: 'Remove from sidebar', danger: true, onSelect: () => void call('project/remove', { id: p.id }) },
  ]
  return (
    <div className="sidebar-section">
      <div
        className="sidebar-section-header"
        onClick={() => void call('project/update', { id: p.id, collapsed: !collapsed })}
        role="button"
        aria-expanded={!collapsed}
        tabIndex={0}
      >
        {collapsed ? <ChevronRight size={13} className="subtle" /> : <ChevronDown size={13} className="subtle" />}
        <span className="ellipsis small" style={{ fontWeight: 600 }} title={p.folders.join('\n')}>
          {p.name}
        </span>
        {!p.trusted && (
          <span className="badge" title="Untrusted: .odex config is ignored">
            untrusted
          </span>
        )}
        <span className="actions">
          <button className="icon-btn sm" aria-label={`New thread in ${p.name}`} title="New thread" onClick={(e) => (e.stopPropagation(), newThread())}>
            <Plus size={13} />
          </button>
          <button className="icon-btn sm" aria-label="Project actions" onClick={(e) => (e.stopPropagation(), setAnchor(e.currentTarget))}>
            <MoreHorizontal size={13} />
          </button>
        </span>
      </div>
      {!collapsed && (
        <div>
          {visible.map((t) => (
            <ThreadRow key={t.id} t={t} active={t.id === selected} />
          ))}
          {threads.length > 12 && (
            <button className="nav-item xs" onClick={() => setShowAll(!showAll)}>
              {showAll ? 'Show less' : `Show ${threads.length - 12} more`}
            </button>
          )}
          {threads.length === 0 && <div className="xs subtle" style={{ padding: '2px 10px 4px 28px' }}>No threads yet</div>}
        </div>
      )}
      {anchor && <Menu anchor={anchor} items={items} onClose={() => setAnchor(null)} />}
    </div>
  )
}

// Project type lacks the derived field; keep a helper for clarity.
export function Sidebar({ width }: { width: number }) {
  const threadsMap = useApp((s) => s.threads)
  const order = useApp((s) => s.threadOrder)
  const projects = useApp((s) => s.projects)
  const selected = useApp((s) => s.selectedThreadId)
  const ui = useApp((s) => s.ui)
  const setUi = useApp((s) => s.setUi)
  const selectThread = useApp((s) => s.selectThread)
  const automationUnread = useApp((s) => s.automationUnread)

  const { pinned, byProject, loose, unreadCount } = useMemo(() => {
    const list = order.map((id) => threadsMap[id]?.thread).filter((t): t is Thread => !!t && !t.archived && !t.ephemeral && t.kind !== 'subagent' && t.kind !== 'side')
    const pinned = list.filter((t) => t.pinned)
    const byProject = new Map<string, Thread[]>()
    const loose: Thread[] = []
    for (const t of list) {
      if (t.pinned) continue
      if (t.projectId) byProject.set(t.projectId, [...(byProject.get(t.projectId) ?? []), t])
      else loose.push(t)
    }
    for (const arr of byProject.values()) arr.sort((a, b) => b.updatedAt - a.updatedAt)
    loose.sort((a, b) => b.updatedAt - a.updatedAt)
    return { pinned, byProject, loose, unreadCount: list.filter((t) => t.unread || t.status === 'waitingApproval').length }
  }, [order, threadsMap])

  return (
    <nav className="sidebar" style={{ width }} aria-label="Threads">
      <div className="sidebar-top">
        <button
          className={`nav-item ${ui.view === 'home' ? 'active' : ''}`}
          onClick={() => void selectThread(null)}
          title="New thread (Ctrl+N)"
        >
          <MessageSquarePlus size={15} /> New thread
        </button>
        <button className="nav-item" onClick={() => void A.createThread({ kind: 'quickChat' })} title="Quick chat (Ctrl+Alt+N)">
          <Zap size={15} /> Quick chat
        </button>
        <button className={`nav-item ${ui.view === 'search' ? 'active' : ''}`} onClick={() => setUi({ view: 'search' })}>
          <Search size={15} /> Search
        </button>
        <button className={`nav-item ${ui.view === 'activity' ? 'active' : ''}`} onClick={() => setUi({ view: 'activity' })} title="Activity (Ctrl+Alt+U)">
          <Bell size={15} /> Activity
          {unreadCount > 0 && <span className="badge accent count">{unreadCount}</span>}
        </button>
        <button className={`nav-item ${ui.view === 'automations' ? 'active' : ''}`} onClick={() => setUi({ view: 'automations' })}>
          <Clock size={15} /> Automations
          {automationUnread > 0 && <span className="badge accent count">{automationUnread}</span>}
        </button>
      </div>
      <div className="sidebar-scroll">
        {pinned.length > 0 && (
          <div className="sidebar-section">
            <div className="sidebar-section-header" style={{ cursor: 'default' }}>
              <Pin size={12} className="subtle" />
              <span className="section-title">Pinned</span>
            </div>
            {pinned.map((t) => (
              <ThreadRow key={t.id} t={t} active={t.id === selected} />
            ))}
          </div>
        )}
        <div className="sidebar-section">
          <div className="sidebar-section-header" style={{ cursor: 'default' }}>
            <span className="section-title">Projects</span>
            <span className="actions" style={{ display: 'flex' }}>
              <button className="icon-btn sm" aria-label="Add project" title="Add project (Ctrl+O)" onClick={() => void A.addProjectFromDialog()}>
                <FolderPlus size={13} />
              </button>
            </span>
          </div>
          {projects.map((p) => (
            <ProjectSection key={p.id} p={p} threads={byProject.get(p.id) ?? []} selected={selected} />
          ))}
          {projects.length === 0 && (
            <button className="nav-item small" onClick={() => void A.addProjectFromDialog()}>
              <FolderPlus size={14} /> Add a project folder
            </button>
          )}
        </div>
        {loose.length > 0 && (
          <div className="sidebar-section">
            <div className="sidebar-section-header" style={{ cursor: 'default' }}>
              <span className="section-title">Chats</span>
            </div>
            {loose.slice(0, 40).map((t) => (
              <ThreadRow key={t.id} t={t} active={t.id === selected} />
            ))}
          </div>
        )}
      </div>
      <div className="sidebar-top" style={{ borderTop: '1px solid var(--border)', paddingTop: 6, paddingBottom: 8 }}>
        <button className="nav-item" onClick={() => A.openSettings('archived')}>
          <Archive size={15} /> Archived
        </button>
        <button className={`nav-item ${ui.view === 'settings' ? 'active' : ''}`} onClick={() => A.openSettings('general')} title="Settings (Ctrl+,)">
          <Settings size={15} /> Settings
        </button>
      </div>
    </nav>
  )
}
