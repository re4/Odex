import { memo, useEffect, useState } from 'react'
import {
  AlertTriangle,
  Bot,
  Brain,
  Check,
  ChevronDown,
  ChevronRight,
  CircleX,
  Copy,
  FileText,
  FolderGit2,
  GitFork,
  Globe,
  Info,
  ListChecks,
  Monitor,
  Pencil,
  Plug,
  RotateCcw,
  Undo2,
  Search,
  Terminal,
  Wrench,
} from 'lucide-react'
import type { FileChange, ThreadItem, Turn, UserInput } from '@shared/index'
import { Markdown } from '@/components/Markdown'
import { Identicon, Menu, Modal, basename, formatTokens } from '@/components/ui'
import { isRunning, useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import '@/styles/thread-nav.css'

export function openFileInPanel(path: string, line?: number): void {
  useApp.setState({ fileToOpen: { path, line, at: Date.now() } })
  window.dispatchEvent(new CustomEvent('odex:open-file', { detail: { path, line } }))
  useApp.getState().setUi({ sidePanelOpen: true, sidePanelTab: 'files' })
}

function Status({ status }: { status: string }) {
  if (status === 'inProgress') return <span className="spinner" aria-label="running" />
  if (status === 'completed') return <Check size={14} color="var(--success)" aria-label="completed" />
  if (status === 'declined') return <CircleX size={14} color="var(--warning)" aria-label="declined" />
  return <CircleX size={14} color="var(--danger)" aria-label="failed" />
}

export function MiniDiff({ diff, maxLines = 400 }: { diff: string; maxLines?: number }) {
  const lines = diff.split('\n').filter((l) => !l.startsWith('---') && !l.startsWith('+++') && !l.startsWith('diff ') && !l.startsWith('index '))
  return (
    <div className="mini-diff">
      {lines.slice(0, maxLines).map((l, i) => (
        <div key={i} className={`ln ${l.startsWith('+') ? 'diff-add text-add' : l.startsWith('-') ? 'diff-del text-del' : l.startsWith('@@') ? 'hunk' : ''}`}>
          {l || ' '}
        </div>
      ))}
      {lines.length > maxLines && <div className="ln hunk">… {lines.length - maxLines} more lines</div>}
    </div>
  )
}

function FileChanges({ changes, status, error }: { changes: FileChange[]; status: string; error?: string | null }) {
  const [open, setOpen] = useState<Record<string, boolean>>({})
  return (
    <div className="cell">
      {changes.length === 0 && (
        <div className="cell-head">
          <Status status={status} /> <span>Edit</span>
          {error && <span className="muted ellipsis">{error}</span>}
        </div>
      )}
      {changes.map((c) => (
        <div key={c.path}>
          <div className="cell-head" onClick={() => setOpen({ ...open, [c.path]: !open[c.path] })}>
            {open[c.path] ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
            <Status status={status} />
            <Pencil size={13} className="subtle" />
            <span className="ellipsis" title={c.path}>
              {c.kind === 'add' ? 'Added' : c.kind === 'delete' ? 'Deleted' : c.kind === 'move' ? 'Moved' : 'Edited'}{' '}
              <a
                href="#"
                onClick={(e) => {
                  e.preventDefault()
                  e.stopPropagation()
                  openFileInPanel(c.movePath || c.path)
                }}
              >
                {c.movePath || c.path}
              </a>
            </span>
            <span className="spacer" />
            <span className="xs text-add">+{c.additions}</span>
            <span className="xs text-del">-{c.deletions}</span>
          </div>
          {open[c.path] && (
            <div className="cell-body">
              <MiniDiff diff={c.diff} />
            </div>
          )}
        </div>
      ))}
      {error && changes.length > 0 && <div className="cell-body"><pre>{error}</pre></div>}
    </div>
  )
}

function Command({ item }: { item: Extract<ThreadItem, { type: 'commandExecution' }> }) {
  const [open, setOpen] = useState(item.status === 'inProgress' || item.status === 'failed')
  useEffect(() => {
    if (item.status === 'failed') setOpen(true)
  }, [item.status])
  return (
    <div className="cell">
      <div className="cell-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        <Status status={item.status} />
        <Terminal size={13} className="subtle" />
        <span className="cmd ellipsis grow" title={item.command}>
          {item.command}
        </span>
        {item.sandboxed && <span className="badge" title="Ran inside the sandbox">sandbox</span>}
        {item.sessionId && <span className="badge accent">session {item.sessionId}</span>}
        {item.exitCode != null && <span className={`badge ${item.exitCode === 0 ? 'success' : 'danger'}`}>exit {item.exitCode}</span>}
        {item.durationMs != null && <span className="xs subtle">{(item.durationMs / 1000).toFixed(1)}s</span>}
      </div>
      {open && (
        <div className="cell-body">
          <pre>{item.output || (item.status === 'inProgress' ? '…' : '(no output)')}</pre>
          {item.outputRef && <div className="xs subtle" style={{ padding: '0 10px 6px' }}>full output: {item.outputRef}</div>}
        </div>
      )}
    </div>
  )
}

function Collapsible({ icon, title, children, defaultOpen = false, right }: { icon: React.ReactNode; title: React.ReactNode; children?: React.ReactNode; defaultOpen?: boolean; right?: React.ReactNode }) {
  const [open, setOpen] = useState(defaultOpen)
  return (
    <div>
      <div className="tool-line" onClick={() => children && setOpen(!open)} aria-expanded={open}>
        {children ? open ? <ChevronDown size={13} /> : <ChevronRight size={13} /> : <span style={{ width: 13 }} />}
        {icon}
        <span className="ellipsis grow">{title}</span>
        {right}
      </div>
      {open && children && <div className="cell" style={{ marginTop: 4 }}>{children}</div>}
    </div>
  )
}

function UserMessage({ item, turn, threadId }: { item: Extract<ThreadItem, { type: 'userMessage' }>; turn: Turn; threadId: string }) {
  const text = item.content.map((c) => (c.type === 'text' ? c.text : '')).join('\n')
  const extras = item.content.filter((c) => c.type !== 'text')
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState(text)
  const [restoreFiles, setRestoreFiles] = useState(false)
  const [forkMenu, setForkMenu] = useState<HTMLElement | null>(null)
  const [rollback, setRollback] = useState(false)
  const running = useApp((s) => isRunning(s.threads[threadId]?.thread))
  const resend = async () => {
    if (!draft.trim() && !extras.length) return
    try {
      if (running) await A.interrupt(threadId)
      await call('thread/rollback', { threadId, turnId: turn.id, restoreFiles })
      await useApp.getState().loadThread(threadId)
      setEditing(false)
      await A.sendMessage(threadId, [...(draft.trim() ? [{ type: 'text' as const, text: draft }] : []), ...extras])
    } catch (e) {
      toast(`Could not resend: ${(e as Error).message}`, 'error')
    }
  }
  if (editing) {
    return (
      <div className="item-user">
        <div className="col" style={{ width: '85%', gap: 6 }}>
          <textarea
            className="textarea"
            style={{ minHeight: 70 }}
            value={draft}
            autoFocus
            aria-label="Edit message"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') setEditing(false)
              if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) void resend()
            }}
          />
          <div className="row xs">
            <label className="checkbox">
              <input type="checkbox" checked={restoreFiles} onChange={(e) => setRestoreFiles(e.target.checked)} /> Also undo file changes made since this message
            </label>
            <span className="spacer" />
            <button className="btn btn-sm btn-ghost" onClick={() => setEditing(false)}>
              Cancel
            </button>
            <button className="btn btn-sm btn-primary" onClick={() => void resend()} title="Ctrl+Enter">
              Resend
            </button>
          </div>
          <div className="xs subtle">This message and everything after it will be replaced.</div>
        </div>
      </div>
    )
  }
  return (
    <div className="item-user">
      <div style={{ position: 'relative', maxWidth: '85%' }}>
        <div className={`user-bubble ${item.steer ? 'steer' : ''}`} style={{ maxWidth: '100%' }}>
          {text}
          {extras.length > 0 && (
            <div className="row" style={{ flexWrap: 'wrap', marginTop: text ? 6 : 0 }}>
              {extras.map((c, i) => (
                <span
                  key={i}
                  className={`attachment ${imageOf(c) ? 'image-attachment' : ''}`}
                  title={imageOf(c) ? 'View image' : undefined}
                  role={imageOf(c) ? 'button' : undefined}
                  tabIndex={imageOf(c) ? 0 : undefined}
                  onClick={() => openAttachment(c)}
                  onKeyDown={(e) => e.key === 'Enter' && openAttachment(c)}
                >
                  {c.type === 'image' ? <img className="zoomable" src={c.url} alt="" /> : c.type === 'appshot' ? <img className="zoomable" src={c.image_url} alt="" /> : <FileText size={12} />}
                  <span className="ellipsis">
                    {c.type === 'file' || c.type === 'mention' || c.type === 'localImage'
                      ? basename(c.path)
                      : c.type === 'skill'
                        ? `$${c.name}`
                        : c.type === 'appshot'
                          ? c.title
                          : c.type === 'reviewComments'
                            ? `${c.comments.length} review comments`
                            : c.type === 'browserComment'
                              ? 'page comment'
                              : c.type === 'mcpResource'
                                ? c.uri
                                : c.type}
                  </span>
                </span>
              ))}
            </div>
          )}
        </div>
        <div className="item-actions" style={{ top: -14, right: 4 }}>
          <button className="icon-btn sm" title="Copy" aria-label="Copy message" onClick={() => A.copy(text)}>
            <Copy size={12} />
          </button>
          <button
            className="icon-btn sm"
            title="Edit and resend"
            aria-label="Edit and resend"
            onClick={() => {
              setDraft(text)
              setEditing(true)
            }}
          >
            <Pencil size={12} />
          </button>
          <button className="icon-btn sm" title="Fork from here" aria-label="Fork from here" aria-haspopup="menu" onClick={(e) => setForkMenu(e.currentTarget)}>
            <GitFork size={12} />
          </button>
          <button className="icon-btn sm" title="Roll back to here" aria-label="Roll back to here" onClick={() => setRollback(true)}>
            <Undo2 size={12} />
          </button>
        </div>
      </div>
      {forkMenu && (
        <Menu
          anchor={forkMenu}
          align="right"
          onClose={() => setForkMenu(null)}
          items={[
            { label: 'Fork from here', header: true },
            { label: 'To a new thread', icon: <GitFork size={13} />, hint: 'same folder', onSelect: () => void A.forkThread(threadId, turn.id, 'local') },
            { label: 'To a new worktree', icon: <FolderGit2 size={13} />, hint: 'isolated branch', onSelect: () => void A.forkThread(threadId, turn.id, 'worktree') },
          ]}
        />
      )}
      {rollback && <RollbackDialog threadId={threadId} turn={turn} text={text} onClose={() => setRollback(false)} />}
    </div>
  )
}

/** Confirm "Roll back to here": drop this message and everything after it, optionally restoring files. */
function RollbackDialog({ threadId, turn, text, onClose }: { threadId: string; turn: Turn; text: string; onClose: () => void }) {
  const [restoreFiles, setRestoreFiles] = useState(false)
  const [busy, setBusy] = useState(false)
  const later = useApp((s) => {
    const turns = s.threads[threadId]?.turns ?? []
    const i = turns.findIndex((t) => t.id === turn.id)
    return i < 0 ? 0 : turns.length - i - 1
  })
  const go = async () => {
    setBusy(true)
    const ok = await A.rollbackTo(threadId, turn.id, restoreFiles)
    setBusy(false)
    if (!ok) return
    // the message goes back into the composer so it can be edited (nothing is sent)
    const st = useApp.getState()
    if (!st.threads[threadId]?.draft?.trim()) st.patchThread(threadId, { draft: text })
    onClose()
  }
  return (
    <Modal
      title="Roll back to here?"
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-danger" disabled={busy} onClick={() => void go()} autoFocus>
            {restoreFiles ? 'Roll back and restore files' : 'Roll back'}
          </button>
        </>
      }
    >
      <div className="small">
        This message{later > 0 ? ` and the ${later} ${later === 1 ? 'turn' : 'turns'} after it` : ''} will be removed from the thread. Nothing is sent; the message goes back into the composer.
      </div>
      {text.trim() && <div className="rollback-preview selectable">{text.length > 600 ? `${text.slice(0, 600)}…` : text}</div>}
      <label className="checkbox small">
        <input type="checkbox" checked={restoreFiles} onChange={(e) => setRestoreFiles(e.target.checked)} aria-label="Also restore files" /> Also restore files to how they were before this message (from the undo snapshot)
      </label>
    </Modal>
  )
}

/** The image an attachment shows, if any (for the lightbox). */
function imageOf(c: UserInput): string | null {
  if (c.type === 'image') return c.url
  if (c.type === 'appshot') return c.image_url
  if (c.type === 'localImage') return c.path
  return null
}

function openAttachment(c: UserInput): void {
  if (c.type === 'image') A.openImage(c.url, 'image')
  else if (c.type === 'appshot') A.openImage(c.image_url, c.title || 'appshot')
  else if (c.type === 'localImage')
    void window.odex.fs
      .read(c.path)
      .then((r: { kind?: string; dataUrl?: string }) => (r.kind === 'media' && r.dataUrl ? A.openImage(r.dataUrl, basename(c.path)) : toast('Could not open the image', 'error')))
      .catch(() => toast('Could not open the image', 'error'))
}

function SubagentCard({ item }: { item: Extract<ThreadItem, { type: 'subagent' }> }) {
  const [open, setOpen] = useState(false)
  return (
    <div className="cell">
      <div className="cell-head" onClick={() => setOpen(!open)}>
        <Identicon seed={item.identiconSeed} size={18} />
        <span style={{ fontWeight: 600 }}>{item.nickname}</span>
        <span className="badge">{item.mode === 'read_only' ? 'read-only' : 'write'}</span>
        <span className="ellipsis grow muted">{item.task}</span>
        {item.diffStats && item.diffStats.filesChanged > 0 && (
          <span className="xs">
            <span className="text-add">+{item.diffStats.additions}</span> <span className="text-del">-{item.diffStats.deletions}</span>
          </span>
        )}
        <Status status={item.status} />
        <button
          className="btn btn-sm"
          onClick={(e) => {
            e.stopPropagation()
            void useApp.getState().selectThread(item.agentThreadId)
          }}
        >
          Open
        </button>
      </div>
      {open && (
        <div className="cell-body" style={{ padding: '8px 10px' }}>
          <div className="small muted" style={{ marginBottom: 4 }}>
            Task
          </div>
          <div className="small selectable" style={{ whiteSpace: 'pre-wrap' }}>
            {item.task}
          </div>
          {item.summary && (
            <>
              <div className="small muted" style={{ margin: '8px 0 4px' }}>
                Report
              </div>
              <Markdown text={item.summary} onOpenFile={openFileInPanel} />
            </>
          )}
        </div>
      )}
    </div>
  )
}

function ProposedPlan({ item, threadId }: { item: Extract<ThreadItem, { type: 'proposedPlan' }>; threadId: string }) {
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState(item.markdown)
  const [busy, setBusy] = useState(false)
  const decide = async (decision: 'approve' | 'edit' | 'reject') => {
    setBusy(true)
    try {
      await call('thread/plan/decide', { threadId, itemId: item.id, decision, markdown: decision === 'edit' ? draft : undefined })
      setEditing(false)
    } catch (e) {
      toast((e as Error).message, 'error')
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className="cell" style={{ borderColor: 'color-mix(in srgb, var(--accent) 45%, var(--border))' }}>
      <div className="cell-head" style={{ cursor: 'default' }}>
        <ListChecks size={14} color="var(--accent)" />
        <span style={{ fontWeight: 600 }}>Proposed plan</span>
        {item.approved && <span className="badge success">approved</span>}
      </div>
      <div className="cell-body" style={{ padding: '10px 12px', maxHeight: 'none' }}>
        {editing ? (
          <textarea className="textarea mono" style={{ width: '100%', minHeight: 220 }} value={draft} onChange={(e) => setDraft(e.target.value)} aria-label="Edit plan" autoFocus />
        ) : (
          <Markdown text={item.markdown} onOpenFile={openFileInPanel} />
        )}
      </div>
      {!item.approved && (
        <div className="approval-actions">
          {editing ? (
            <>
              <button className="btn btn-primary btn-sm" disabled={busy || !draft.trim()} onClick={() => void decide('edit')}>
                Run edited plan
              </button>
              <button className="btn btn-ghost btn-sm" onClick={() => (setEditing(false), setDraft(item.markdown))}>
                Cancel
              </button>
            </>
          ) : (
            <>
              <button className="btn btn-primary btn-sm" disabled={busy} onClick={() => void decide('approve')}>
                Approve and run
              </button>
              <button className="btn btn-sm" onClick={() => setEditing(true)}>
                Edit plan
              </button>
              <button className="btn btn-ghost btn-sm" disabled={busy} onClick={() => void decide('reject')}>
                Dismiss
              </button>
            </>
          )}
        </div>
      )}
    </div>
  )
}

function ImageFromPath({ path, cwd }: { path: string; cwd: string }) {
  const [src, setSrc] = useState<string | null>(null)
  useEffect(() => {
    const full = /^([a-zA-Z]:[\\/]|\/)/.test(path) ? path : `${cwd}/${path}`
    void window.odex.fs.read(full).then((r: any) => r.kind === 'media' && setSrc(r.dataUrl)).catch(() => {})
  }, [path, cwd])
  return src ? <img className="viewed-image zoomable" src={src} alt={path} title="View image" onClick={() => A.openImage(src, basename(path))} /> : null
}

export const ItemView = memo(function ItemView({ item, turn, threadId }: { item: ThreadItem; turn: Turn; threadId: string }) {
  const showReasoning = useApp((s) => s.settings?.showReasoning ?? true)
  const cwd = useApp((s) => s.threads[threadId]?.thread.cwd ?? '')
  switch (item.type) {
    case 'userMessage':
      return <UserMessage item={item} turn={turn} threadId={threadId} />
    case 'agentMessage':
      if (!item.text.trim()) return null
      return (
        <div className="item">
          <Markdown text={item.text} onOpenFile={openFileInPanel} />
          <div className="item-actions">
            <button className="icon-btn sm" title="Copy" aria-label="Copy response" onClick={() => A.copy(item.text)}>
              <Copy size={12} />
            </button>
          </div>
        </div>
      )
    case 'reasoning':
      if (!showReasoning || !item.text.trim()) return null
      return (
        <div className="item reasoning">
          <Collapsible icon={<Brain size={13} />} title={<span>Thinking</span>} right={<span className="xs subtle">{formatTokens(Math.round(item.text.length / 4))} tok</span>}>
            <div className="body" style={{ padding: '8px 10px' }}>
              {item.text}
            </div>
          </Collapsible>
        </div>
      )
    case 'commandExecution':
      return (
        <div className="item">
          <Command item={item} />
        </div>
      )
    case 'fileChange':
      return (
        <div className="item">
          <FileChanges changes={item.changes} status={item.status} error={item.error} />
        </div>
      )
    case 'toolCall': {
      const icon = item.tool === 'grep' || item.tool === 'glob' || item.tool === 'recall' ? <Search size={13} /> : item.tool === 'read_file' ? <FileText size={13} /> : <Wrench size={13} />
      return (
        <div className="item">
          <Collapsible icon={icon} title={item.summary || `${item.tool}`} right={item.status === 'inProgress' ? <span className="spinner" /> : item.status === 'failed' ? <CircleX size={13} color="var(--danger)" /> : null}>
            {item.output && <div className="cell-body" style={{ borderTop: 'none' }}><pre>{item.output}</pre></div>}
          </Collapsible>
        </div>
      )
    }
    case 'mcpToolCall':
      return (
        <div className="item">
          <Collapsible icon={<Plug size={13} />} title={`${item.server} · ${item.tool}`} right={<Status status={item.status} />}>
            <div className="cell-body" style={{ borderTop: 'none' }}>
              <pre>{JSON.stringify(item.arguments, null, 2)}</pre>
              {item.error && <pre style={{ color: 'var(--danger)' }}>{item.error}</pre>}
              {item.result != null && <pre>{mcpText(item.result)}</pre>}
            </div>
          </Collapsible>
        </div>
      )
    case 'plan':
      return (
        <div className="item">
          <div className="cell" style={{ padding: '8px 12px' }}>
            <div className="row small" style={{ marginBottom: 4, fontWeight: 600 }}>
              <ListChecks size={14} /> Plan
            </div>
            {item.explanation && <div className="small muted" style={{ marginBottom: 4 }}>{item.explanation}</div>}
            <ul className="plan-list small">
              {item.steps.map((s, i) => (
                <li key={i} className={s.status}>
                  {s.status === 'completed' ? <Check size={14} color="var(--success)" /> : s.status === 'inProgress' ? <span className="spinner" style={{ marginTop: 3 }} /> : <span style={{ width: 14, height: 14, border: '1.5px solid var(--border-strong)', borderRadius: 4, flex: 'none', marginTop: 2 }} />}
                  <span>{s.step}</span>
                </li>
              ))}
            </ul>
          </div>
        </div>
      )
    case 'proposedPlan':
      return (
        <div className="item">
          <ProposedPlan item={item} threadId={threadId} />
        </div>
      )
    case 'subagent':
      return (
        <div className="item">
          <SubagentCard item={item} />
        </div>
      )
    case 'contextCompaction':
      return (
        <div className="item">
          <div className="compaction" role="note">
            {item.status === 'inProgress' ? (
              <>
                <span className="spinner" /> compacting context…
              </>
            ) : item.status === 'failed' ? (
              <>context compaction skipped</>
            ) : (
              <a href="#" onClick={(e) => (e.preventDefault(), useApp.getState().setUi({ contextViewOpen: true }))}>
                context compacted {formatTokens(item.tokensBefore)} → {formatTokens(item.tokensAfter)} (summary #{item.summaryNumber}
                {item.llm ? '' : ', extractive'}
                {item.trigger === 'emergency' ? ', emergency' : item.trigger === 'manual' ? ', manual' : ''})
              </a>
            )}
          </div>
        </div>
      )
    case 'computerUse':
      return (
        <div className="item">
          <Collapsible icon={<Monitor size={13} />} title={`${item.action}${item.app ? ` · ${item.app}` : ''}`} right={<Status status={item.status} />} defaultOpen={!!(item.beforeImage || item.afterImage)}>
            {(item.beforeImage || item.afterImage) && (
              <div className="thumbs">
                {item.beforeImage && <img className="zoomable" src={item.beforeImage} alt="before" title="before" onClick={() => A.openImage(item.beforeImage!, `${item.action} before`)} />}
                {item.afterImage && <img className="zoomable" src={item.afterImage} alt="after" title="after" onClick={() => A.openImage(item.afterImage!, `${item.action} after`)} />}
              </div>
            )}
            {item.output && <div className="cell-body"><pre>{item.output}</pre></div>}
          </Collapsible>
        </div>
      )
    case 'browser':
      return (
        <div className="item">
          <Collapsible icon={<Globe size={13} />} title={`${item.action}${item.url ? ` · ${item.url}` : ''}`} right={<Status status={item.status} />}>
            {item.image && (
              <div className="thumbs">
                <img className="zoomable" src={item.image} alt="page" title="View screenshot" onClick={() => A.openImage(item.image!, item.url ? `page ${item.url}` : 'page')} />
              </div>
            )}
            {item.output && <div className="cell-body"><pre>{item.output}</pre></div>}
          </Collapsible>
        </div>
      )
    case 'imageView':
      return (
        <div className="item">
          <div className="tool-line" title={item.prompt || undefined}>
            <FileText size={13} /> {item.prompt != null ? 'Generated image' : 'Viewed image'} {item.path}
          </div>
          <ImageFromPath path={item.path} cwd={cwd} />
        </div>
      )
    case 'review':
      return (
        <div className="item">
          <div className="cell">
            <div className="cell-head" style={{ cursor: 'default' }}>
              <Bot size={14} /> <span style={{ fontWeight: 600 }}>Review</span>
              {item.overallCorrectness && <span className={`badge ${item.overallCorrectness === 'correct' ? 'success' : 'warning'}`}>{item.overallCorrectness}</span>}
              <span className="spacer" />
              <span className="xs subtle">{item.findings.length} finding(s)</span>
            </div>
            <div className="cell-body" style={{ padding: '8px 12px', maxHeight: 'none' }}>
              {item.summary && <Markdown text={item.summary} />}
              {item.findings.map((f, i) => (
                <div key={i} style={{ marginTop: 10 }}>
                  <div className="row small">
                    <span className={`badge ${f.priority === 0 ? 'danger' : f.priority === 1 ? 'warning' : ''}`}>P{f.priority}</span>
                    <span style={{ fontWeight: 600 }}>{f.title}</span>
                    {f.path && (
                      <a href="#" className="xs" onClick={(e) => (e.preventDefault(), openFileInPanel(f.path!, f.lineStart ?? undefined))}>
                        {f.path}
                        {f.lineStart ? `:${f.lineStart}` : ''}
                      </a>
                    )}
                  </div>
                  <div className="small" style={{ marginTop: 2 }}>
                    <Markdown text={f.body} onOpenFile={openFileInPanel} />
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      )
    case 'notice':
      if (item.code === 'synthetic') {
        return (
          <div className="item">
            <div className="notice">
              <RotateCcw size={13} /> <span className="ellipsis">{item.message}</span>
            </div>
          </div>
        )
      }
      return (
        <div className="item">
          <div className={`notice ${item.level}`} role="status">
            {item.level === 'info' ? <Info size={13} /> : <AlertTriangle size={13} />}
            <span className="selectable">{item.message}</span>
          </div>
        </div>
      )
    case 'error':
      return (
        <div className="item">
          <div className="error-box" role="alert">
            <div className="row" style={{ fontWeight: 600, marginBottom: 2 }}>
              <CircleX size={14} color="var(--danger)" /> Error
            </div>
            {item.message}
            <div style={{ marginTop: 6 }}>
              <button className="btn btn-sm" onClick={() => void A.sendMessage(threadId, [{ type: 'text', text: 'Continue.' }])}>
                Retry
              </button>
              <button className="btn btn-sm btn-ghost" onClick={() => A.openSettings('models')}>
                Check endpoints
              </button>
            </div>
          </div>
        </div>
      )
    default:
      return null
  }
})

function mcpText(result: any): string {
  const content = result?.content
  if (Array.isArray(content)) return content.map((c: any) => (c.type === 'text' ? c.text : `[${c.type}]`)).join('\n')
  return JSON.stringify(result, null, 2)
}

export function copyThreadAsMarkdown(threadId: string): void {
  const ts = useApp.getState().threads[threadId]
  if (!ts) return
  const parts: string[] = []
  for (const t of ts.turns)
    for (const i of t.items) {
      if (i.type === 'userMessage') parts.push(`**You:** ${i.content.map((c) => (c.type === 'text' ? c.text : '')).join(' ')}`)
      if (i.type === 'agentMessage') parts.push(i.text)
    }
  void navigator.clipboard.writeText(parts.join('\n\n'))
  toast('Conversation copied')
}
