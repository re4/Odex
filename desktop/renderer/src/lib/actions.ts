import type { PermissionMode, ReasoningEffort, RunMode, ThreadKind, TurnMode, UserInput } from '@shared/index'
import { call, toast } from '@/lib/rpc'
import { useApp } from '@/store/app'

/** Prompt the user for text (simple, keyboard-friendly). */
export function promptText(title: string, initial = ''): Promise<string | null> {
  return new Promise((resolve) => {
    window.dispatchEvent(new CustomEvent('odex:prompt', { detail: { title, initial, resolve } }))
  })
}

export function confirmDialog(title: string, body: string, confirmLabel = 'Confirm', danger = false): Promise<boolean> {
  return new Promise((resolve) => {
    window.dispatchEvent(new CustomEvent('odex:confirm', { detail: { title, body, confirmLabel, danger, resolve } }))
  })
}

export interface NewThreadOpts {
  projectId?: string | null
  cwd?: string
  kind?: ThreadKind
  runMode?: RunMode
  model?: string
  effort?: ReasoningEffort
  permissionMode?: PermissionMode
  baseBranch?: string
  ephemeral?: boolean
  parentThreadId?: string
  name?: string
}

export async function createThread(opts: NewThreadOpts = {}): Promise<string | null> {
  try {
    const r = await call('thread/start', {
      projectId: opts.projectId ?? undefined,
      cwd: opts.cwd,
      kind: opts.kind,
      runMode: opts.runMode,
      model: opts.model,
      effort: opts.effort,
      permissionMode: opts.permissionMode,
      baseBranch: opts.baseBranch,
      ephemeral: opts.ephemeral,
      parentThreadId: opts.parentThreadId,
      name: opts.name,
    })
    const s = useApp.getState()
    s.applyNotification('thread/started', { thread: r.thread })
    await s.selectThread(r.thread.id)
    return r.thread.id
  } catch (e) {
    toast(`Could not start a thread: ${(e as Error).message}`, 'error')
    return null
  }
}

/** Start a turn (or queue/steer if busy). */
export async function sendMessage(threadId: string, input: UserInput[], opts: { mode?: TurnMode; steer?: boolean } = {}): Promise<void> {
  try {
    const r = await call('turn/start', { threadId, input, mode: opts.mode, ifBusy: opts.steer ? 'steer' : 'queue' })
    if (r.queued) toast('Queued; it will run when the current turn finishes.')
  } catch (e) {
    toast(`Send failed: ${(e as Error).message}`, 'error')
  }
}

export async function interrupt(threadId: string): Promise<void> {
  await call('turn/interrupt', { threadId }).catch(() => {})
}

// ------------------------------------------------------------- undo stack
// Client-side undo/redo for app actions (archive, pin, rename): Ctrl+Z and
// Ctrl+Shift+Z / Ctrl+Y outside text fields.

interface UndoEntry {
  label: string
  undo: () => Promise<unknown>
  redo?: () => Promise<unknown>
}
const undoStack: UndoEntry[] = []
const redoStack: UndoEntry[] = []

/** Record an undoable action; `redo` re-applies it after an undo. A new action clears the redo stack. */
export function pushUndo(label: string, undo: () => Promise<unknown>, redo?: () => Promise<unknown>): void {
  undoStack.push({ label, undo, redo })
  if (undoStack.length > 50) undoStack.shift()
  redoStack.length = 0
}

export async function undoLast(): Promise<void> {
  const e = undoStack.pop()
  if (!e) {
    toast('Nothing to undo')
    return
  }
  try {
    await e.undo()
    if (e.redo) redoStack.push(e)
    toast(`Undone: ${e.label}${e.redo ? ' · Ctrl+Shift+Z to redo' : ''}`)
  } catch (err) {
    toast(`Could not undo ${e.label}: ${(err as Error).message}`, 'error')
  }
}

export async function redoLast(): Promise<void> {
  const e = redoStack.pop()
  if (!e?.redo) {
    toast('Nothing to redo')
    return
  }
  try {
    await e.redo()
    undoStack.push(e)
    toast(`Redone: ${e.label}`)
  } catch (err) {
    toast(`Could not redo ${e.label}: ${(err as Error).message}`, 'error')
  }
}

export async function archiveThread(threadId: string): Promise<void> {
  const t = useApp.getState().threads[threadId]?.thread
  let removeWorktree = false
  if (t?.worktree) {
    removeWorktree = await confirmDialog('Archive thread', `Also remove its worktree at ${t.worktree.path}? A snapshot is kept so it can be restored.`, 'Remove worktree')
  }
  await call('thread/archive', { threadId, removeWorktree })
  pushUndo(
    'archive',
    async () => {
      await call('thread/unarchive', { threadId })
      await useApp.getState().selectThread(threadId)
    },
    async () => {
      await call('thread/archive', { threadId, removeWorktree: false })
      const s = useApp.getState()
      if (s.selectedThreadId === threadId) await s.selectThread(null)
    },
  )
  toast('Thread archived · Ctrl+Z to undo')
  const s = useApp.getState()
  if (s.selectedThreadId === threadId) await s.selectThread(null)
}

export async function renameThread(threadId: string): Promise<void> {
  const t = useApp.getState().threads[threadId]?.thread
  const before = t?.name ?? null
  const name = await promptText('Rename thread', before ?? '')
  if (name == null || name === before) return
  await call('thread/update', { threadId, name })
  pushUndo('rename', () => call('thread/update', { threadId, name: before ?? '' }), () => call('thread/update', { threadId, name }))
}

export async function togglePin(threadId: string): Promise<void> {
  const t = useApp.getState().threads[threadId]?.thread
  const pinned = !t?.pinned
  await call('thread/update', { threadId, pinned })
  pushUndo(pinned ? 'pin' : 'unpin', () => call('thread/update', { threadId, pinned: !pinned }), () => call('thread/update', { threadId, pinned }))
}

export async function markUnread(threadId: string, unread = true): Promise<void> {
  await call('thread/update', { threadId, unread })
}

export async function forkThread(threadId: string, turnId?: string, runMode: RunMode = 'local', side = false): Promise<void> {
  try {
    const r = await call('thread/fork', { threadId, turnId, runMode, kind: side ? 'side' : 'normal', ephemeral: side })
    const s = useApp.getState()
    s.applyNotification('thread/started', { thread: r.thread })
    await s.selectThread(r.thread.id)
  } catch (e) {
    toast(`Fork failed: ${(e as Error).message}`, 'error')
  }
}

export async function addProjectFromDialog(): Promise<string | null> {
  const folders = await window.odex.dialog.openFolder({ multi: true })
  if (!folders.length) return null
  return addProject(folders)
}

export async function addProject(folders: string[]): Promise<string | null> {
  const trust = await call('trust/check', { path: folders[0] })
  if (trust.unknown) {
    const ok = await confirmDialog(
      'Trust this folder?',
      `Odex can load this folder's project configuration (.odex/: settings, hooks, actions, skills).${trust.hasOdexDir ? ' This folder has an .odex directory.' : ''} Only trust folders you control. Untrusted projects still work, but their .odex config is ignored.`,
      'Trust folder',
    )
    await call('trust/set', { path: folders[0], trusted: ok })
  }
  const r = await call('project/add', { folders, create: false })
  await useApp.getState().refreshProjects()
  useApp.getState().setUi({ newThreadProjectId: r.project.id })
  return r.project.id
}

/** A search match's absolute path (`fs/search` paths are relative to their root). */
export function matchPath(m: { path: string; root: string }): { abs: string; rel: string } {
  const isAbs = /^([a-zA-Z]:[\\/]|[\\/])/.test(m.path)
  if (isAbs) {
    const rel = m.path.toLowerCase().startsWith(m.root.toLowerCase()) ? m.path.slice(m.root.length).replace(/^[\\/]/, '') : m.path
    return { abs: m.path, rel }
  }
  const sep = m.root.includes('\\') ? '\\' : '/'
  return { abs: `${m.root.replace(/[\\/]+$/, '')}${sep}${m.path.replace(/\//g, sep)}`, rel: m.path }
}

/** `http://host:8000` → `http://host:8000/v1` (vLLM serves the OpenAI API under /v1). */
export function normalizeBaseUrl(raw: string): string {
  const t = raw.trim().replace(/\/+$/, '')
  if (!t) return t
  try {
    const u = new URL(/^\w+:\/\//.test(t) ? t : `http://${t}`)
    if (u.pathname === '/' || u.pathname === '') u.pathname = '/v1'
    return u.toString().replace(/\/+$/, '')
  } catch {
    return t
  }
}

export function copy(text: string, what = 'Copied'): void {
  void navigator.clipboard.writeText(text)
  toast(what)
}

export function openSettings(panel = 'general'): void {
  useApp.getState().setUi({ view: 'settings', settingsPanel: panel })
}

export function openSidePanel(tab: import('@/store/app').SidePanelTab): void {
  useApp.getState().setUi({ sidePanelOpen: true, sidePanelTab: tab })
}

/** `30m`, `1h30m`, `90s`, `2h`, `45` (minutes) → seconds; null if unparsable. */
export function parseDuration(raw: string): number | null {
  const t = raw.trim().toLowerCase().replace(/\s+/g, '')
  if (!t) return null
  if (/^\d+(\.\d+)?$/.test(t)) return Math.round(Number(t) * 60)
  if (!/^(\d+(\.\d+)?[dhms])+$/.test(t)) return null
  let total = 0
  for (const m of t.matchAll(/(\d+(?:\.\d+)?)([dhms])/g)) total += Number(m[1]) * { d: 86400, h: 3600, m: 60, s: 1 }[m[2] as 'd' | 'h' | 'm' | 's']
  return total > 0 ? Math.round(total) : null
}

/** `200k`, `1.5m`, `50000` → tokens; null if unparsable. */
export function parseTokenCount(raw: string): number | null {
  const m = /^(\d+(?:\.\d+)?)\s*([km]?)$/i.exec(raw.trim().replace(/[,_]/g, ''))
  if (!m) return null
  const unit = m[2].toLowerCase()
  const n = Number(m[1]) * (unit === 'k' ? 1_000 : unit === 'm' ? 1_000_000 : 1)
  return n > 0 ? Math.round(n) : null
}

/** 5400 → `1h30m`, 90 → `1m30s`. */
export function formatDuration(secs: number): string {
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  const s = Math.floor(secs % 60)
  return [h ? `${h}h` : '', m ? `${m}m` : '', s && !h ? `${s}s` : ''].filter(Boolean).join('') || '0m'
}

/** 200000 → `200k`, 1500000 → `1.5m`. */
export function formatTokenCount(n: number): string {
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(2)}m`
  if (n >= 1_000) return `${+(n / 1_000).toFixed(1)}k`
  return String(n)
}

export interface GoalArgs {
  objective: string
  /** undefined: not given; null: removed (`--time none`). */
  timeBudgetSecs?: number | null
  tokenBudget?: number | null
  error?: string
}

/** Parse `/goal` arguments: `--time 30m --tokens 200k objective…` (flags anywhere, `--time=30m` too, `none` removes a budget). */
export function parseGoalArgs(arg: string): GoalArgs {
  const words = arg.trim().split(/\s+/).filter(Boolean)
  const rest: string[] = []
  const out: GoalArgs = { objective: '' }
  for (let i = 0; i < words.length; i++) {
    const m = /^--(time|tokens)(?:=(.*))?$/.exec(words[i])
    if (!m) {
      rest.push(words[i])
      continue
    }
    const v = m[2] ?? words[++i] ?? ''
    const none = /^(none|off|0)$/i.test(v)
    if (m[1] === 'time') {
      const secs = none ? null : parseDuration(v)
      if (secs === null && !none) return { objective: '', error: `Bad --time value "${v}" (try 30m, 1h30m, 90s)` }
      out.timeBudgetSecs = secs
    } else {
      const n = none ? null : parseTokenCount(v)
      if (n === null && !none) return { objective: '', error: `Bad --tokens value "${v}" (try 200k, 1.5m)` }
      out.tokenBudget = n
    }
  }
  out.objective = rest.join(' ')
  return out
}

/** Roll the thread back to just before `turnId` (drops it and later turns) without resending. */
export async function rollbackTo(threadId: string, turnId: string, restoreFiles: boolean): Promise<boolean> {
  try {
    const t = useApp.getState().threads[threadId]?.thread
    if (t && (t.status === 'running' || t.status === 'waitingApproval' || t.status === 'compacting' || t.status === 'reconnecting')) await interrupt(threadId)
    await call('thread/rollback', { threadId, turnId, restoreFiles })
    await useApp.getState().loadThread(threadId)
    toast(restoreFiles ? 'Rolled back; files restored' : 'Rolled back')
    return true
  } catch (e) {
    toast(`Could not roll back: ${(e as Error).message}`, 'error')
    return false
  }
}

/** Show an image in the lightbox (zoom, save). */
export function openImage(src: string, name = 'image'): void {
  window.dispatchEvent(new CustomEvent('odex:lightbox', { detail: { src, name } }))
}

/** Slash commands available in the composer. */
export interface SlashCommand {
  name: string
  description: string
  args?: string
  run: (threadId: string | null, arg: string) => Promise<void> | void
}

export const SLASH_COMMANDS: SlashCommand[] = [
  {
    name: 'plan',
    description: 'Plan first: explore read-only and propose a plan for approval',
    args: '<task>',
    run: async (tid, arg) => {
      if (!arg.trim()) {
        // no task: turn plan mode on for the next message (the composer listens)
        if (!tid && useApp.getState().ui.view !== 'home') useApp.getState().setUi({ view: 'home' })
        window.dispatchEvent(new CustomEvent('odex:plan-mode', { detail: { on: true } }))
        return
      }
      const id = tid ?? (await createThread({ projectId: useApp.getState().ui.newThreadProjectId }))
      if (id) await sendMessage(id, [{ type: 'text', text: arg }], { mode: 'plan' })
    },
  },
  {
    name: 'goal',
    description: 'Set a persistent goal the agent keeps pursuing. Budgets: --time 30m (1h30m, 90s) and --tokens 200k (1.5m); options alone edit the current goal. Also /goal pause, resume, clear',
    args: '<objective> [--time 30m] [--tokens 200k]',
    run: async (tid, arg) => {
      const id = tid ?? (await createThread({ projectId: useApp.getState().ui.newThreadProjectId }))
      if (!id) return
      const a = arg.trim()
      const goal = useApp.getState().threads[id]?.thread.goal
      const live = goal && goal.status !== 'cleared' ? goal : null
      try {
        if (!a || a === 'clear') {
          await call('thread/goal/clear', { threadId: id })
          toast('Goal cleared')
          return
        }
        if (a === 'pause') return void (await call('thread/goal/pause', { threadId: id }))
        if (a === 'resume') return void (await call('thread/goal/resume', { threadId: id }))
        const g = parseGoalArgs(a)
        if (g.error) return toast(g.error, 'error')
        if (!g.objective && !live) return toast('Give the goal an objective: /goal [--time 30m] [--tokens 200k] <objective>', 'error')
        if (!g.objective && live) {
          // options alone: edit the current goal's budgets
          await call('thread/goal/set', {
            threadId: id,
            objective: live.objective,
            timeBudgetSecs: g.timeBudgetSecs !== undefined ? g.timeBudgetSecs : (live.timeBudgetSecs ?? null),
            tokenBudget: g.tokenBudget !== undefined ? g.tokenBudget : (live.tokenBudget ?? null),
            edit: true,
          })
          toast('Goal budget updated')
          return
        }
        await call('thread/goal/set', { threadId: id, objective: g.objective, timeBudgetSecs: g.timeBudgetSecs ?? null, tokenBudget: g.tokenBudget ?? null })
      } catch (e) {
        toast((e as Error).message, 'error')
      }
    },
  },
  {
    name: 'compact',
    description: 'Summarize the conversation to free context',
    args: '[focus]',
    run: async (tid, arg) => {
      if (!tid) return
      await call('thread/compact', { threadId: tid, focus: arg || undefined })
    },
  },
  {
    name: 'review',
    description: 'Review changes with the reviewer model',
    args: '[base <branch> | commit <sha> | instructions]',
    run: async (tid, arg) => {
      if (!tid) return toast('Open a thread first')
      const a = arg.trim()
      let target: import('@shared/index').DiffTarget = { type: 'uncommitted' }
      let instructions: string | undefined
      if (a.startsWith('base ')) target = { type: 'base', branch: a.slice(5).trim() }
      else if (a.startsWith('commit ')) target = { type: 'commit', sha: a.slice(7).trim() }
      else if (a) instructions = a
      try {
        await call('review/start', { threadId: tid, target, instructions })
      } catch (e) {
        toast((e as Error).message, 'error')
      }
    },
  },
  {
    name: 'init',
    description: 'Generate or improve AGENTS.md for this project',
    run: async (tid) => {
      const id = tid ?? (await createThread({ projectId: useApp.getState().ui.newThreadProjectId }))
      if (id) await call('thread/initAgentsMd', { threadId: id })
    },
  },
  {
    name: 'status',
    description: 'Show thread id, model, endpoint, token and context usage',
    run: (tid) => {
      if (tid) useApp.getState().setUi({ contextViewOpen: true })
      else toast('Open a thread to see its status')
    },
  },
  { name: 'context', description: 'Open the context view', run: () => useApp.getState().setUi({ contextViewOpen: true }) },
  { name: 'mcp', description: 'Show MCP server status', run: () => openSettings('mcp') },
  { name: 'model', description: 'Choose the model for this thread', run: () => window.dispatchEvent(new CustomEvent('odex:open-picker', { detail: 'model' })) },
  { name: 'reasoning', description: 'Choose the reasoning effort', run: () => window.dispatchEvent(new CustomEvent('odex:open-picker', { detail: 'effort' })) },
  { name: 'permissions', description: 'Choose the permission mode', run: () => window.dispatchEvent(new CustomEvent('odex:open-picker', { detail: 'permission' })) },
  {
    name: 'memories',
    description: 'Memories for this thread: on, off, or generate (propose memories from this thread now)',
    args: '[on | off | generate]',
    run: async (tid, arg) => {
      if (!tid) return openSettings('memories')
      const a = arg.trim().toLowerCase()
      const t = useApp.getState().threads[tid]?.thread
      try {
        if (a === 'generate' || a === 'gen') {
          toast('Looking for memories in this thread…')
          const r = await call('memory/propose', { threadId: tid })
          toast(r.memories.length ? `Proposed ${r.memories.length} ${r.memories.length === 1 ? 'memory' : 'memories'} · review in Settings → Memories` : 'No new memories found in this thread')
          return
        }
        const on = a === 'on' ? true : a === 'off' ? false : a ? null : !t?.memoriesEnabled
        if (on === null) return toast('Usage: /memories [on | off | generate]', 'error')
        await call('thread/update', { threadId: tid, memoriesEnabled: on })
        toast(`Memories ${on ? 'on' : 'off'} for this thread`)
      } catch (e) {
        toast((e as Error).message, 'error')
      }
    },
  },
  {
    name: 'approve',
    description: 'Allow the action automatic review denied (once)',
    run: async (tid) => {
      if (!tid) return
      try {
        await call('thread/approveOverride', { threadId: tid })
      } catch (e) {
        toast((e as Error).message, 'error')
      }
    },
  },
  { name: 'skills', description: 'Manage skills', run: () => openSettings('skills') },
  {
    name: 'local',
    description: 'Run new threads in the project folder',
    run: () => {
      useApp.getState().setUi({ newThreadRunMode: 'local' })
      toast('New threads run locally')
    },
  },
  {
    name: 'worktree',
    description: 'Run new threads in a git worktree',
    run: () => {
      useApp.getState().setUi({ newThreadRunMode: 'worktree' })
      toast('New threads run in a worktree')
    },
  },
  { name: 'project', description: 'Choose the project for new threads', run: () => window.dispatchEvent(new CustomEvent('odex:open-picker', { detail: 'project' })) },
  {
    name: 'fork',
    description: 'Fork this thread (to a new thread, or `worktree`)',
    args: '[worktree]',
    run: async (tid, arg) => {
      if (tid) await forkThread(tid, undefined, arg.trim() === 'worktree' ? 'worktree' : 'local')
    },
  },
  {
    name: 'side',
    description: 'Open a temporary side chat',
    run: async (tid) => {
      if (tid) await forkThread(tid, undefined, 'local', true)
    },
  },
  {
    name: 'task',
    description: 'Start a thread without a project',
    run: async () => {
      useApp.getState().setUi({ newThreadProjectId: null })
      await useApp.getState().selectThread(null)
    },
  },
  {
    name: 'new',
    description: 'Start a new thread in the same project',
    run: async (tid) => {
      const t = tid ? useApp.getState().threads[tid]?.thread : undefined
      useApp.getState().setUi({ newThreadProjectId: t?.projectId ?? useApp.getState().ui.newThreadProjectId })
      await useApp.getState().selectThread(null)
    },
  },
  {
    name: 'doctor',
    description: "Run Doctor (quick checks) for this thread's model",
    run: (tid) => {
      const st = useApp.getState()
      const model = (tid ? st.threads[tid]?.thread.model : null) ?? st.roles.main ?? undefined
      window.dispatchEvent(new CustomEvent('odex:doctor', { detail: { model } }))
    },
  },
  {
    name: 'clear',
    description: 'Start fresh in this project',
    run: async (tid) => {
      const t = tid ? useApp.getState().threads[tid]?.thread : undefined
      useApp.getState().setUi({ newThreadProjectId: t?.projectId ?? null })
      await useApp.getState().selectThread(null)
    },
  },
]
