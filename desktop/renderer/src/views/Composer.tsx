import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { ArrowUp, Brain, ChevronDown, Cpu, FileText, FolderGit2, GitBranch, ListChecks, Monitor, Paperclip, Shield, Sparkles, Square, X } from 'lucide-react'
import type { FileMatch, PermissionMode, ReasoningEffort, SkillInfo, UserInput } from '@shared/index'
import { isRunning, useApp, type Attachment } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Menu, basename, formatTokens, type MenuItem } from '@/components/ui'

const PERMISSION_LABEL: Record<PermissionMode, string> = {
  'read-only': 'Read only',
  auto: 'Auto',
  'full-access': 'Full access',
}
const PERMISSION_HINT: Record<PermissionMode, string> = {
  'read-only': 'Reads files; asks before any edit or command',
  auto: 'Edits and runs commands in the workspace sandbox; asks for network and outside access',
  'full-access': 'No sandbox and no approvals. Use with care.',
}
const EFFORT_LABEL: Record<ReasoningEffort, string> = { none: 'None', minimal: 'Minimal', low: 'Low', medium: 'Medium', high: 'High', xhigh: 'Extra high' }
const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp)$/i

let attachSeq = 0
const newId = () => `att_${Date.now().toString(36)}_${attachSeq++}`

function fileToDataUrl(f: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader()
    r.onload = () => resolve(String(r.result))
    r.onerror = () => reject(r.error)
    r.readAsDataURL(f)
  })
}

/** Ring showing how full the context window is. */
export function ContextRing({ threadId }: { threadId: string }) {
  const ctx = useApp((s) => s.threads[threadId]?.context)
  const setUi = useApp((s) => s.setUi)
  if (!ctx) return null
  const pct = Math.min(1, ctx.used / Math.max(1, ctx.window))
  const ofBudget = ctx.used / Math.max(1, ctx.budget)
  const color = ofBudget >= ctx.compactAt ? 'var(--danger)' : ofBudget >= ctx.pruneAt ? 'var(--warning)' : 'var(--accent)'
  const r = 8
  const c = 2 * Math.PI * r
  const b = ctx.breakdown
  const rows: Array<[string, number]> = [
    ['System', b.system],
    ['Tools', b.tools],
    ['AGENTS.md', b.agentsMd],
    ['Memories', b.memories],
    ['Summary', b.summary],
    ['Pinned', b.pinned],
    ['History', b.history],
    ['Tool output', b.toolOutputs],
    ['Images', b.images],
  ]
  return (
    <button className="ctx-ring" aria-label={`Context ${Math.round(pct * 100)}% used`} onClick={() => setUi({ contextViewOpen: true })} style={{ background: 'none', border: 'none', padding: 0 }}>
      <svg width="22" height="22" viewBox="0 0 22 22">
        <circle cx="11" cy="11" r={r} fill="none" stroke="var(--border-strong)" strokeWidth="2.5" />
        <circle cx="11" cy="11" r={r} fill="none" stroke={color} strokeWidth="2.5" strokeDasharray={`${c * pct} ${c}`} strokeLinecap="round" />
      </svg>
      <div className="ctx-tip" role="tooltip">
        <div style={{ fontWeight: 600, marginBottom: 4 }}>
          Context {Math.round(pct * 100)}% · {formatTokens(ctx.used)} / {formatTokens(ctx.window)}
          {ctx.exact ? '' : ' (est.)'}
        </div>
        {rows
          .filter(([, v]) => v > 0)
          .map(([k, v]) => (
            <div key={k} className="row" style={{ justifyContent: 'space-between' }}>
              <span className="muted">{k}</span>
              <span>{formatTokens(v)}</span>
            </div>
          ))}
        <div className="subtle" style={{ marginTop: 4 }}>
          {ctx.compactions.length} compaction(s) · {ctx.prunes} prune(s) · {ctx.model ?? ''}
        </div>
        <div className="subtle">Click for details</div>
      </div>
    </button>
  )
}

interface MentionState {
  kind: '@' | '$' | '/'
  start: number
  query: string
}

export function Composer({ threadId, autoFocus = true, placeholder }: { threadId: string | null; autoFocus?: boolean; placeholder?: string }) {
  const ts = useApp((s) => (threadId ? s.threads[threadId] : undefined))
  const settings = useApp((s) => s.settings)
  const models = useApp((s) => s.models)
  const roles = useApp((s) => s.roles)
  const projects = useApp((s) => s.projects)
  const ui = useApp((s) => s.ui)
  const setUi = useApp((s) => s.setUi)

  // Home composer state (no thread yet)
  const [homeDraft, setHomeDraft] = useState('')
  const [homeAtt, setHomeAtt] = useState<Attachment[]>([])
  const [homeModel, setHomeModel] = useState<string | null>(null)
  const [homeEffort, setHomeEffort] = useState<ReasoningEffort | null>(null)
  const [homePerm, setHomePerm] = useState<PermissionMode>('auto')
  const [planNext, setPlanNext] = useState(false)

  const draft = threadId ? (ts?.draft ?? '') : homeDraft
  const attachments = threadId ? (ts?.attachments ?? []) : homeAtt
  const setDraft = useCallback(
    (v: string) => {
      if (threadId) useApp.getState().patchThread(threadId, { draft: v })
      else setHomeDraft(v)
    },
    [threadId],
  )
  const setAttachments = useCallback(
    (f: (a: Attachment[]) => Attachment[]) => {
      if (threadId) {
        const cur = useApp.getState().threads[threadId]?.attachments ?? []
        useApp.getState().patchThread(threadId, { attachments: f(cur) })
      } else setHomeAtt(f)
    },
    [threadId],
  )

  const thread = ts?.thread
  const running = isRunning(thread)
  const modelKey = thread?.model ?? homeModel ?? roles.main ?? models[0]?.key ?? null
  const model = models.find((m) => m.key === modelKey)
  const effort = (thread ? thread.effort : homeEffort) ?? model?.defaultEffort ?? null
  const perm: PermissionMode = thread?.permissionMode ?? homePerm
  const project = projects.find((p) => p.id === (thread ? thread.projectId : ui.newThreadProjectId))
  const ta = useRef<HTMLTextAreaElement>(null)
  const [mention, setMention] = useState<MentionState | null>(null)
  const [mentionItems, setMentionItems] = useState<MenuItem[]>([])
  const [mentionActive, setMentionActive] = useState(0)
  const [picker, setPicker] = useState<{ kind: string; anchor: HTMLElement } | null>(null)
  const [skills, setSkills] = useState<SkillInfo[]>([])
  const [dragOver, setDragOver] = useState(false)
  const historyPos = useRef(-1)
  const chipRefs = useRef<Record<string, HTMLButtonElement | null>>({})

  useEffect(() => {
    if (autoFocus) ta.current?.focus()
  }, [threadId, autoFocus])

  // auto-grow
  useEffect(() => {
    const el = ta.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, window.innerHeight * 0.4)}px`
  }, [draft])

  // external attachments (appshots, browser comments, review comments, files from panels)
  useEffect(() => {
    const onAttach = (e: Event) => {
      const d = (e as CustomEvent).detail
      if (useApp.getState().selectedThreadId !== threadId) return
      if (d.type === 'appshot') {
        setAttachments((a) => [...a, { id: newId(), label: d.title || 'Appshot', preview: d.imageUrl, input: { type: 'appshot', title: d.title, app: d.app ?? null, image_url: d.imageUrl, ui_tree: d.uiTree ?? null } }])
      } else if (d.type === 'browserComment') {
        setAttachments((a) => [...a, { id: newId(), label: `Comment: ${d.comment.slice(0, 40)}`, preview: d.screenshotUrl, input: { type: 'browserComment', url: d.url, selector: d.selector, bounds: d.bounds, comment: d.comment, screenshot_url: d.screenshotUrl } }])
      } else if (d.type === 'file') {
        setAttachments((a) => [...a, { id: newId(), label: basename(d.path), input: IMAGE_EXT.test(d.path) ? { type: 'localImage', path: d.path } : { type: 'mention', path: d.path } }])
      } else if (d.type === 'text') {
        setDraft((useApp.getState().threads[threadId ?? '']?.draft ?? homeDraft) + d.text)
      }
      ta.current?.focus()
    }
    const onPicker = (e: Event) => {
      const kind = (e as CustomEvent).detail as string
      const anchor = chipRefs.current[kind] ?? ta.current
      if (anchor) setPicker({ kind, anchor })
    }
    window.addEventListener('odex:attach', onAttach)
    window.addEventListener('odex:open-picker', onPicker)
    return () => {
      window.removeEventListener('odex:attach', onAttach)
      window.removeEventListener('odex:open-picker', onPicker)
    }
  }, [threadId, setAttachments, setDraft, homeDraft])

  const roots = useMemo(() => {
    if (thread) return [thread.worktree?.path ?? thread.cwd]
    return project?.folders ?? []
  }, [thread, project])

  // mention/slash/skill completion
  useEffect(() => {
    if (!mention) return
    let cancelled = false
    const q = mention.query.toLowerCase()
    const pick = (insert: string, input?: UserInput, label?: string) => {
      const el = ta.current
      if (!el) return
      const before = draft.slice(0, mention.start)
      const after = draft.slice(el.selectionStart)
      const next = `${before}${insert} ${after}`
      setDraft(next)
      if (input) setAttachments((a) => [...a, { id: newId(), label: label ?? insert, input }])
      setMention(null)
      requestAnimationFrame(() => {
        el.focus()
        const pos = before.length + insert.length + 1
        el.setSelectionRange(pos, pos)
      })
    }
    if (mention.kind === '/') {
      const items = A.SLASH_COMMANDS.filter((c) => c.name.startsWith(q)).map<MenuItem>((c) => ({
        label: `/${c.name}${c.args ? ` ${c.args}` : ''}`,
        hint: c.description,
        onSelect: () => {
          if (c.args) pick(`/${c.name}`)
          else {
            setDraft('')
            setMention(null)
            void c.run(threadId, '')
          }
        },
      }))
      setMentionItems(items)
      setMentionActive(0)
      return
    }
    if (mention.kind === '$') {
      const run = async () => {
        let list = skills
        if (!list.length) {
          list = (await call('skills/list', { cwd: roots[0] ?? null }).catch(() => ({ skills: [] as SkillInfo[] }))).skills
          if (!cancelled) setSkills(list)
        }
        if (cancelled) return
        setMentionItems(
          list
            .filter((s) => s.enabled && s.name.toLowerCase().includes(q))
            .slice(0, 30)
            .map((s) => ({ label: `$${s.name}`, hint: s.description, icon: <Sparkles size={13} />, onSelect: () => pick(`$${s.name}`, { type: 'skill', name: s.name }, s.name) })),
        )
        setMentionActive(0)
      }
      void run()
      return () => {
        cancelled = true
      }
    }
    // @ files
    if (!roots.length) {
      setMentionItems([{ label: 'Choose a project to mention files', disabled: true }])
      return
    }
    const t = setTimeout(async () => {
      const r = await call('fs/search', { roots, query: mention.query, limit: 40 }).catch(() => ({ files: [] as FileMatch[] }))
      if (cancelled) return
      setMentionItems(
        r.files.map((m) => {
          const rel = m.path.startsWith(m.root) ? m.path.slice(m.root.length).replace(/^[\\/]/, '') : m.path
          return {
            label: rel,
            icon: <FileText size={13} />,
            onSelect: () => pick(`@${rel}`, IMAGE_EXT.test(rel) ? { type: 'localImage', path: m.path } : { type: 'mention', path: m.path }, rel),
          }
        }),
      )
      setMentionActive(0)
    }, 60)
    return () => {
      cancelled = true
      clearTimeout(t)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mention?.kind, mention?.query, mention?.start, roots])

  function detectMention(value: string, caret: number) {
    const upto = value.slice(0, caret)
    if (/^\/[\w-]*$/.test(upto)) {
      setMention({ kind: '/', start: 0, query: upto.slice(1) })
      return
    }
    const m = /(^|\s)([@$])([^\s@$]*)$/.exec(upto)
    if (m) setMention({ kind: m[2] as '@' | '$', start: upto.length - m[3].length - 1, query: m[3] })
    else setMention(null)
  }

  async function addFiles(files: File[] | string[]) {
    for (const f of files) {
      if (typeof f === 'string') {
        setAttachments((a) => [...a, { id: newId(), label: basename(f), input: IMAGE_EXT.test(f) ? { type: 'localImage', path: f } : { type: 'file', path: f, name: basename(f) } }])
        continue
      }
      const p = (f as File & { path?: string }).path
      if (f.type.startsWith('image/')) {
        const url = await fileToDataUrl(f)
        setAttachments((a) => [...a, { id: newId(), label: f.name || 'image', preview: url, input: p ? { type: 'localImage', path: p } : { type: 'image', url, name: f.name } }])
      } else if (p) {
        setAttachments((a) => [...a, { id: newId(), label: f.name, input: { type: 'file', path: p, name: f.name } }])
      } else {
        toast(`Can't attach ${f.name}: no file path`, 'error')
      }
    }
  }

  async function submit(steer = false) {
    const text = draft.trim()
    if (!text && !attachments.length) return
    // slash command with args
    const slash = /^\/([\w-]+)(?:\s+([\s\S]*))?$/.exec(text)
    if (slash && !attachments.length) {
      const cmd = A.SLASH_COMMANDS.find((c) => c.name === slash[1])
      if (cmd) {
        setDraft('')
        setMention(null)
        await cmd.run(threadId, slash[2] ?? '')
        return
      }
    }
    const input: UserInput[] = []
    const inlineMentions = new Set<string>()
    for (const a of attachments) {
      if (a.input.type === 'mention' || a.input.type === 'skill') inlineMentions.add(a.label)
      input.push(a.input)
    }
    if (text) input.unshift({ type: 'text', text })
    setDraft('')
    setAttachments(() => [])
    setMention(null)
    historyPos.current = -1
    const mode = planNext ? 'plan' : undefined
    setPlanNext(false)
    if (threadId) {
      const behavior = steer ? 'steer' : (settings?.followUpBehavior ?? 'queue')
      await A.sendMessage(threadId, input, { mode, steer: running && behavior === 'steer' })
      return
    }
    // home composer: start a thread first
    const id = await A.createThread({
      projectId: ui.newThreadProjectId,
      runMode: project?.isGit ? ui.newThreadRunMode : 'local',
      model: homeModel ?? undefined,
      effort: homeEffort ?? undefined,
      permissionMode: homePerm,
    })
    if (id) await A.sendMessage(id, input, { mode })
  }

  function recallHistory(dir: 1 | -1): boolean {
    if (!threadId || !ts) return false
    const msgs: string[] = []
    for (const t of ts.turns) for (const i of t.items) if (i.type === 'userMessage') msgs.push(i.content.map((c) => (c.type === 'text' ? c.text : '')).join(''))
    if (!msgs.length) return false
    let pos = historyPos.current
    pos = dir === -1 ? (pos < 0 ? msgs.length - 1 : Math.max(0, pos - 1)) : pos < 0 ? -1 : pos + 1
    if (pos >= msgs.length) pos = -1
    historyPos.current = pos
    setDraft(pos < 0 ? '' : msgs[pos])
    return true
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (mention && mentionItems.length) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault()
        const n = mentionItems.length
        setMentionActive((i) => (e.key === 'ArrowDown' ? (i + 1) % n : (i - 1 + n) % n))
        return
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        const it = mentionItems[mentionActive]
        if (it && !it.disabled) {
          e.preventDefault()
          it.onSelect?.()
          return
        }
      }
      if (e.key === 'Escape') {
        e.preventDefault()
        setMention(null)
        return
      }
    }
    if (e.key === 'Tab' && e.shiftKey) {
      e.preventDefault()
      setPlanNext((p) => !p)
      return
    }
    if (e.key === 'Escape' && running && threadId) {
      e.preventDefault()
      void A.interrupt(threadId)
      return
    }
    const enterSends = settings?.enterSends ?? true
    if (e.key === 'Enter' && !e.nativeEvent.isComposing) {
      const mod = e.ctrlKey || e.metaKey
      if (mod && e.shiftKey) {
        e.preventDefault()
        void submit(true)
        return
      }
      if ((enterSends && !e.shiftKey && !mod) || (!enterSends && mod)) {
        e.preventDefault()
        void submit()
        return
      }
    }
    if (e.key === 'ArrowUp' && !draft && recallHistory(-1)) {
      e.preventDefault()
      return
    }
    if (e.key === 'ArrowDown' && historyPos.current >= 0 && recallHistory(1)) {
      e.preventDefault()
    }
  }

  async function setModel(key: string) {
    if (thread) await call('thread/update', { threadId: thread.id, model: key })
    else setHomeModel(key)
  }
  async function setEffort(e: ReasoningEffort) {
    if (thread) await call('thread/update', { threadId: thread.id, effort: e })
    else setHomeEffort(e)
  }
  async function setPerm(p: PermissionMode) {
    if (p === 'full-access') {
      const ok = await A.confirmDialog('Full access', 'The agent will run commands without a sandbox and without asking. It can change or delete anything your account can. Continue?', 'Allow full access', true)
      if (!ok) return
    }
    if (thread) await call('thread/update', { threadId: thread.id, permissionMode: p })
    else setHomePerm(p)
  }

  const pickerItems: MenuItem[] = useMemo(() => {
    if (!picker) return []
    switch (picker.kind) {
      case 'model': {
        const byProvider = new Map<string, typeof models>()
        for (const m of models) byProvider.set(m.providerId, [...(byProvider.get(m.providerId) ?? []), m])
        const items: MenuItem[] = []
        for (const [prov, ms] of byProvider) {
          items.push({ label: prov, header: true })
          for (const m of ms)
            items.push({
              label: m.displayName,
              hint: `${formatTokens(m.contextWindow)}${m.capabilities.vision ? ' · vision' : ''}${m.available ? '' : ' · offline'}`,
              checked: m.key === modelKey,
              onSelect: () => void setModel(m.key),
            })
        }
        if (!items.length) items.push({ label: 'No models: add an endpoint', onSelect: () => A.openSettings('models') })
        items.push({ separator: true, label: '' }, { label: 'Models & endpoints…', onSelect: () => A.openSettings('models') })
        return items
      }
      case 'effort':
        return (model?.efforts.length ? model.efforts : (['low', 'medium', 'high'] as ReasoningEffort[])).map((e) => ({
          label: EFFORT_LABEL[e],
          checked: e === effort,
          onSelect: () => void setEffort(e),
        }))
      case 'permission':
        return (Object.keys(PERMISSION_LABEL) as PermissionMode[]).map((p) => ({
          label: PERMISSION_LABEL[p],
          hint: PERMISSION_HINT[p],
          checked: p === perm,
          danger: p === 'full-access',
          onSelect: () => void setPerm(p),
        }))
      case 'project':
        return [
          { label: 'No project (chat)', checked: !ui.newThreadProjectId, onSelect: () => setUi({ newThreadProjectId: null }) },
          ...projects.map((p) => ({ label: p.name, hint: p.folders[p.primary] ?? p.folders[0], checked: p.id === ui.newThreadProjectId, onSelect: () => setUi({ newThreadProjectId: p.id }) })),
          { separator: true, label: '' },
          { label: 'Add project folder…', onSelect: () => void A.addProjectFromDialog() },
        ]
      case 'runMode':
        return [
          { label: 'Local', hint: 'Work in the project folder', checked: ui.newThreadRunMode === 'local', onSelect: () => setUi({ newThreadRunMode: 'local' }) },
          { label: 'Worktree', hint: 'Isolated git worktree on a new branch', checked: ui.newThreadRunMode === 'worktree', disabled: !project?.isGit, onSelect: () => setUi({ newThreadRunMode: 'worktree' }) },
        ]
      default:
        return []
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [picker, models, modelKey, effort, perm, projects, ui.newThreadProjectId, ui.newThreadRunMode, model])

  const canSend = !!draft.trim() || attachments.length > 0
  const chip = (kind: string, icon: React.ReactNode, label: React.ReactNode, title: string) => (
    <button
      ref={(el) => {
        chipRefs.current[kind] = el
      }}
      className="chip"
      title={title}
      onClick={(e) => setPicker({ kind, anchor: e.currentTarget })}
    >
      {icon}
      <span className="ellipsis" style={{ maxWidth: 180 }}>
        {label}
      </span>
      <ChevronDown size={11} />
    </button>
  )

  return (
    <div style={{ position: 'relative' }}>
      {mention && mentionItems.length > 0 && (
        <div className="mention-menu" role="listbox" aria-label="Suggestions">
          {mentionItems.map((it, i) => (
            <button
              key={i}
              role="option"
              aria-selected={i === mentionActive}
              className="menu-item"
              data-active={i === mentionActive}
              disabled={it.disabled}
              onMouseEnter={() => setMentionActive(i)}
              onMouseDown={(e) => {
                e.preventDefault()
                it.onSelect?.()
              }}
            >
              {it.icon}
              <span className="ellipsis">{it.label}</span>
              {it.hint && <span className="hint ellipsis" style={{ maxWidth: '55%' }}>{it.hint}</span>}
            </button>
          ))}
        </div>
      )}
      <div
        className="composer"
        style={dragOver ? { borderColor: 'var(--accent)', borderStyle: 'dashed' } : undefined}
        onDragOver={(e) => {
          e.preventDefault()
          setDragOver(true)
        }}
        onDragLeave={() => setDragOver(false)}
        onDrop={(e) => {
          e.preventDefault()
          setDragOver(false)
          void addFiles(Array.from(e.dataTransfer.files))
        }}
      >
        {attachments.length > 0 && (
          <div className="composer-attachments">
            {attachments.map((a) => (
              <span key={a.id} className="attachment" title={a.label}>
                {a.preview ? <img src={a.preview} alt="" /> : a.input.type === 'skill' ? <Sparkles size={12} /> : a.input.type === 'appshot' ? <Monitor size={12} /> : <FileText size={12} />}
                <span className="ellipsis">{a.label}</span>
                <button className="icon-btn sm" aria-label={`Remove ${a.label}`} onClick={() => setAttachments((x) => x.filter((y) => y.id !== a.id))}>
                  <X size={11} />
                </button>
              </span>
            ))}
          </div>
        )}
        <textarea
          ref={ta}
          rows={1}
          value={draft}
          aria-label="Message"
          placeholder={placeholder ?? (running ? (settings?.followUpBehavior === 'steer' ? 'Steer the agent…' : 'Queue a follow-up…') : threadId ? 'Ask for follow-up changes' : 'Ask Odex anything. @ to mention files, $ for skills, / for commands')}
          onChange={(e) => {
            setDraft(e.target.value)
            detectMention(e.target.value, e.target.selectionStart)
          }}
          onKeyDown={onKeyDown}
          onClick={(e) => detectMention(draft, e.currentTarget.selectionStart)}
          onBlur={() => setTimeout(() => setMention(null), 150)}
          onPaste={(e) => {
            const files = Array.from(e.clipboardData.files)
            if (files.length) {
              e.preventDefault()
              void addFiles(files)
            }
          }}
        />
        <div className="composer-bar">
          <button
            className="icon-btn"
            aria-label="Attach files"
            title="Attach files or images"
            onClick={async () => {
              const files = await window.odex.dialog.openFiles()
              void addFiles(files)
            }}
          >
            <Paperclip size={15} />
          </button>
          {!threadId && chip('project', <FolderGit2 size={13} />, project?.name ?? 'No project', 'Project for the new thread')}
          {!threadId && project?.isGit && chip('runMode', <GitBranch size={13} />, ui.newThreadRunMode === 'worktree' ? 'Worktree' : 'Local', 'Where the thread runs')}
          {chip('model', <Cpu size={13} />, model?.displayName ?? modelKey ?? 'No model', 'Model (Ctrl+Shift+M)')}
          {model && model.efforts.length > 0 && chip('effort', <Brain size={13} />, EFFORT_LABEL[effort ?? 'medium'], 'Reasoning effort')}
          {chip('permission', <Shield size={13} />, PERMISSION_LABEL[perm], PERMISSION_HINT[perm])}
          <button className={`chip ${planNext ? 'active' : ''}`} title="Plan first (Shift+Tab)" aria-pressed={planNext} onClick={() => setPlanNext((p) => !p)}>
            <ListChecks size={13} /> Plan
          </button>
          <span className="spacer" />
          {threadId && <ContextRing threadId={threadId} />}
          {running && !canSend ? (
            <button className="send-btn stop" aria-label="Stop" title="Stop (Esc)" onClick={() => threadId && void A.interrupt(threadId)}>
              <Square size={12} fill="currentColor" />
            </button>
          ) : (
            <button className="send-btn" aria-label="Send" title={running ? 'Queue (Enter) · Steer (Ctrl+Shift+Enter)' : 'Send (Enter)'} disabled={!canSend} onClick={() => void submit()}>
              <ArrowUp size={16} />
            </button>
          )}
        </div>
      </div>
      {picker && pickerItems.length > 0 && <Menu anchor={picker.anchor} items={pickerItems} above onClose={() => setPicker(null)} minWidth={240} />}
      {ts && ts.thread.lastError && ts.thread.status === 'error' && (
        <div className="xs" style={{ color: 'var(--danger)', marginTop: 4 }}>
          {ts.thread.lastError}
        </div>
      )}
    </div>
  )
}
