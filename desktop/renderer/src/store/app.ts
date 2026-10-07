import { create } from 'zustand'
import type {
  AutomationRun,
  ContextStatus,
  InitializeResponse,
  ItemDelta,
  McpServerStatus,
  Memory,
  ModelInfo,
  PlanStep,
  Project,
  ProviderInfo,
  SourceEntry,
  Thread,
  ThreadItem,
  Turn,
  UserInput,
  HookInfo,
  DiffStats,
  DesktopSettings,
  UpdateState,
} from '@shared/index'
import { call, toast } from '@/lib/rpc'

export type { DesktopSettings }

/** A composer attachment (files, images, appshots, comments). */
export interface Attachment {
  id: string
  label: string
  input: UserInput
  preview?: string
  /** What the chip represents when `input.type` alone doesn't say (folder, thread, computer, ...). */
  kind?: 'folder' | 'thread' | 'computer' | 'mcpResource'
}

export interface ThreadState {
  thread: Thread
  loaded: boolean
  loading: boolean
  turns: Turn[]
  context?: ContextStatus
  plan: PlanStep[]
  planExplanation?: string | null
  sources: SourceEntry[]
  followups: string[]
  queued: UserInput[][]
  draft: string
  attachments: Attachment[]
  scrollTop?: number
  /** Review-pane comments waiting to be sent. */
  pendingComments: import('@shared/index').ReviewComment[]
  diffStats?: DiffStats | null
}

export interface ServerRequest {
  id: number
  method: string
  params: any
}

export type SidePanelTab = 'review' | 'plan' | 'sources' | 'files' | 'browser' | 'git' | 'terminal'
export type MainView = 'thread' | 'home' | 'activity' | 'automations' | 'search' | 'settings'

export interface UiState {
  view: MainView
  sidebarOpen: boolean
  sidebarWidth: number
  sidePanelOpen: boolean
  sidePanelTab: SidePanelTab
  sidePanelWidth: number
  bottomOpen: boolean
  bottomHeight: number
  paletteOpen: boolean
  paletteMode: 'commands' | 'files' | 'threads'
  settingsPanel: string
  contextViewOpen: boolean
  onboardingOpen: boolean
  newThreadProjectId: string | null
  newThreadRunMode: 'local' | 'worktree'
  popout: boolean
  findOpen: boolean
  /** Side-panel tab order (drag to reorder); missing tabs keep their default place. */
  sidePanelOrder: SidePanelTab[]
  /** `full`: the side panel takes the whole center and the chat is hidden. */
  sidePanelLayout: 'split' | 'full'
  /** Chat ↔ tabs swap: the side panel sits left of the chat. */
  sidePanelSwap: boolean
}

interface AppState {
  engine: { state: string; error?: string | null; init?: InitializeResponse | null }
  settings: DesktopSettings | null
  threads: Record<string, ThreadState>
  threadOrder: string[]
  selectedThreadId: string | null
  projects: Project[]
  models: ModelInfo[]
  roles: Record<string, string | undefined>
  /** Discovered models removed from the list (`hidden_models`). */
  hiddenModels: string[]
  providers: ProviderInfo[]
  mcp: McpServerStatus[]
  serverRequests: ServerRequest[]
  hooksNeedingReview: HookInfo[]
  proposedMemories: Memory[]
  automationRuns: AutomationRun[]
  automationUnread: number
  computerUseActive: { active: boolean; threadId?: string | null; app?: string | null; takeover: boolean }
  killSwitch: boolean
  /** App updates (Settings → About, the update prompt). */
  update: UpdateState | null
  ui: UiState
  /** File the files panel should open next (set before the panel mounts). */
  fileToOpen: { path: string; line?: number; at: number } | null
  history: string[]
  historyIndex: number

  // actions
  setUi: (patch: Partial<UiState>) => void
  setSettings: (patch: Partial<DesktopSettings>) => Promise<void>
  bootstrap: () => Promise<void>
  refreshThreads: () => Promise<void>
  refreshProjects: () => Promise<void>
  refreshModels: (refresh?: boolean) => Promise<void>
  selectThread: (id: string | null) => Promise<void>
  loadThread: (id: string) => Promise<void>
  patchThread: (id: string, patch: Partial<ThreadState>) => void
  applyNotification: (method: string, params: any) => void
  addServerRequest: (r: ServerRequest) => void
  resolveServerRequest: (id: number, result?: unknown, error?: string) => Promise<void>
  dropServerRequest: (id: number) => void
}

const defaultUi: UiState = {
  view: 'home',
  sidebarOpen: true,
  sidebarWidth: 272,
  sidePanelOpen: false,
  sidePanelTab: 'review',
  sidePanelWidth: 460,
  bottomOpen: false,
  bottomHeight: 260,
  paletteOpen: false,
  paletteMode: 'commands',
  settingsPanel: 'general',
  contextViewOpen: false,
  onboardingOpen: false,
  newThreadProjectId: null,
  newThreadRunMode: 'local',
  popout: false,
  findOpen: false,
  sidePanelOrder: [],
  sidePanelLayout: 'split',
  sidePanelSwap: false,
}

/** The Quick Chat window (`?quickchat=1`) never writes UI state to the shared localStorage either. */
const IS_QUICKCHAT = (() => {
  try {
    return new URLSearchParams(location.search).has('quickchat')
  } catch {
    return false
  }
})()

/** Pop-out windows (`?popout=1`) share localStorage with the main window but never write UI state to it. */
const IS_POPOUT = (() => {
  try {
    return new URLSearchParams(location.search).has('popout')
  } catch {
    return false
  }
})()

function loadUi(): UiState {
  const windowUi = IS_POPOUT ? { popout: true, sidebarOpen: false } : { popout: false }
  try {
    const saved = JSON.parse(localStorage.getItem('odex.ui') || '{}') as Partial<UiState>
    // older builds leaked a pop-out's `popout`/`sidebarOpen: false` into the shared state
    if (saved.popout) saved.sidebarOpen = true
    return { ...defaultUi, ...saved, paletteOpen: false, contextViewOpen: false, onboardingOpen: false, findOpen: false, ...windowUi }
  } catch {
    return { ...defaultUi, ...windowUi }
  }
}

function emptyThreadState(thread: Thread): ThreadState {
  return {
    thread,
    loaded: false,
    loading: false,
    turns: [],
    plan: [],
    sources: [],
    followups: [],
    queued: [],
    draft: '',
    attachments: [],
    pendingComments: [],
    diffStats: thread.diffStats,
  }
}

function upsertItem(turns: Turn[], turnId: string, item: ThreadItem, threadId: string): Turn[] {
  let found = false
  const next = turns.map((t) => {
    if (t.id !== turnId) return t
    found = true
    const idx = t.items.findIndex((i) => i.id === item.id)
    const items = idx >= 0 ? t.items.map((i, k) => (k === idx ? item : i)) : [...t.items, item]
    return { ...t, items }
  })
  if (!found) {
    next.push({
      id: turnId,
      threadId,
      status: 'inProgress',
      mode: 'default',
      startedAt: Date.now(),
      usage: { inputTokens: 0, cachedInputTokens: 0, outputTokens: 0, reasoningTokens: 0, totalTokens: 0 },
      items: [item],
    })
  }
  return next
}

function applyDelta(item: ThreadItem, delta: ItemDelta): ThreadItem {
  switch (delta.type) {
    case 'agentMessage':
      return item.type === 'agentMessage' ? { ...item, text: item.text + delta.text } : item
    case 'reasoning':
      return item.type === 'reasoning' ? { ...item, text: item.text + delta.text } : item
    case 'commandOutput':
      return item.type === 'commandExecution' ? { ...item, output: (item.output + delta.chunk).slice(-200_000) } : item
    default:
      return item
  }
}

export const useApp = create<AppState>((set, get) => ({
  engine: { state: 'starting' },
  settings: null,
  threads: {},
  threadOrder: [],
  selectedThreadId: null,
  projects: [],
  models: [],
  roles: {},
  hiddenModels: [],
  providers: [],
  mcp: [],
  serverRequests: [],
  hooksNeedingReview: [],
  proposedMemories: [],
  automationRuns: [],
  automationUnread: 0,
  computerUseActive: { active: false, takeover: false },
  killSwitch: false,
  update: null,
  ui: loadUi(),
  fileToOpen: null,
  history: [],
  historyIndex: -1,

  setUi: (patch) => {
    const ui = { ...get().ui, ...patch }
    set({ ui })
    if (IS_POPOUT || IS_QUICKCHAT || ui.popout) return
    try {
      const { view: _v, paletteOpen: _p, contextViewOpen: _c, onboardingOpen: _o, popout: _w, ...persist } = ui
      localStorage.setItem('odex.ui', JSON.stringify(persist))
    } catch {}
  },

  setSettings: async (patch) => {
    const s = (await window.odex.settings.set(patch)) as DesktopSettings
    set({ settings: s })
  },

  bootstrap: async () => {
    const [settings, info] = await Promise.all([window.odex.settings.get() as Promise<DesktopSettings>, window.odex.engineInfo()])
    set({ settings, engine: { state: info.state, error: info.error, init: info.init } })
    if (info.state === 'ready') await afterReady()
  },

  refreshThreads: async () => {
    const r = await call('thread/list', { limit: 2000 })
    const threads = { ...get().threads }
    for (const t of r.threads) {
      threads[t.id] = threads[t.id] ? { ...threads[t.id], thread: t } : emptyThreadState(t)
    }
    set({ threads, threadOrder: r.threads.map((t) => t.id) })
  },

  refreshProjects: async () => {
    const r = await call('project/list', {})
    set({ projects: r.projects })
  },

  refreshModels: async (refresh = false) => {
    const [m, p] = await Promise.all([call('model/list', {}), call('provider/list', { refresh })])
    set({ models: m.models, roles: m.roles, hiddenModels: m.hidden, providers: p.providers })
  },

  selectThread: async (id) => {
    const { history, historyIndex } = get()
    if (id && history[historyIndex] !== id) {
      const h = [...history.slice(0, historyIndex + 1), id].slice(-100)
      set({ history: h, historyIndex: h.length - 1 })
    }
    set({ selectedThreadId: id })
    get().setUi({ view: id ? 'thread' : 'home' })
    if (!id) return
    const ts = get().threads[id]
    if (!ts?.loaded) await get().loadThread(id)
    const t = get().threads[id]?.thread
    if (t?.unread) {
      void call('thread/update', { threadId: id, unread: false }).catch(() => {})
    }
  },

  loadThread: async (id) => {
    const cur = get().threads[id]
    if (cur?.loading) return
    if (cur) get().patchThread(id, { loading: true })
    try {
      const r = await call('thread/resume', { threadId: id })
      const base = get().threads[id] ?? emptyThreadState(r.thread)
      set({
        threads: {
          ...get().threads,
          [id]: {
            ...base,
            thread: r.thread,
            loaded: true,
            loading: false,
            turns: r.turns,
            context: r.context ?? undefined,
            plan: r.plan,
            sources: r.sources,
            followups: r.followups,
            queued: r.queued,
          },
        },
      })
      // pending approvals surface as server requests already; nothing to do
    } catch (e) {
      get().patchThread(id, { loading: false })
      toast(`Could not open thread: ${(e as Error).message}`, 'error')
    }
  },

  patchThread: (id, patch) => {
    const cur = get().threads[id]
    if (!cur) return
    set({ threads: { ...get().threads, [id]: { ...cur, ...patch } } })
  },

  applyNotification: (method, p) => {
    const s = get()
    const th = (id: string) => s.threads[id]
    switch (method) {
      case 'thread/started':
      case 'thread/updated': {
        const t = p.thread as Thread
        const cur = th(t.id)
        const threads = { ...s.threads, [t.id]: cur ? { ...cur, thread: t, diffStats: t.diffStats ?? cur.diffStats } : emptyThreadState(t) }
        const order = s.threadOrder.includes(t.id) ? s.threadOrder : [t.id, ...s.threadOrder]
        set({ threads, threadOrder: order })
        // a thread the user is looking at never becomes unread
        if (t.unread && s.selectedThreadId === t.id && s.ui.view === 'thread' && document.visibilityState === 'visible') {
          void call('thread/update', { threadId: t.id, unread: false }).catch(() => {})
        }
        break
      }
      case 'thread/deleted': {
        const { [p.threadId]: _gone, ...rest } = s.threads
        set({ threads: rest, threadOrder: s.threadOrder.filter((x) => x !== p.threadId), selectedThreadId: s.selectedThreadId === p.threadId ? null : s.selectedThreadId })
        break
      }
      case 'turn/started': {
        const cur = th(p.threadId)
        if (!cur) break
        const turns = cur.turns.some((t) => t.id === p.turn.id) ? cur.turns : [...cur.turns, { ...p.turn, items: [] }]
        s.patchThread(p.threadId, { turns, followups: [] })
        break
      }
      case 'turn/completed': {
        const cur = th(p.threadId)
        if (!cur) break
        const turns = cur.turns.map((t) => (t.id === p.turn.id ? { ...p.turn, items: t.items } : t))
        s.patchThread(p.threadId, { turns })
        break
      }
      case 'item/started':
      case 'item/completed': {
        const cur = th(p.threadId)
        if (!cur || !cur.loaded) break
        s.patchThread(p.threadId, { turns: upsertItem(cur.turns, p.turnId, p.item, p.threadId) })
        break
      }
      case 'item/delta': {
        const cur = th(p.threadId)
        if (!cur || !cur.loaded) break
        const turns = cur.turns.map((t) => {
          if (t.id !== p.turnId) return t
          return { ...t, items: t.items.map((i) => (i.id === p.itemId ? applyDelta(i, p.delta) : i)) }
        })
        s.patchThread(p.threadId, { turns })
        break
      }
      case 'turn/plan/updated':
        s.patchThread(p.threadId, { plan: p.plan, planExplanation: p.explanation })
        break
      case 'turn/diff/updated':
        s.patchThread(p.threadId, { diffStats: p.stats })
        break
      case 'thread/context/updated':
        s.patchThread(p.threadId, { context: p.context })
        break
      case 'thread/tokenUsage/updated': {
        const cur = th(p.threadId)
        if (cur) s.patchThread(p.threadId, { thread: { ...cur.thread, usage: p.total } })
        break
      }
      case 'thread/followups':
        s.patchThread(p.threadId, { followups: p.suggestions })
        break
      case 'thread/sources/updated':
        s.patchThread(p.threadId, { sources: p.sources })
        break
      case 'thread/queue/updated':
        s.patchThread(p.threadId, { queued: p.queued })
        break
      case 'approval/resolved':
        set({ serverRequests: s.serverRequests.filter((r) => !(r.method === 'approval/request' && r.params.approvalId === p.approvalId)) })
        break
      case 'mcp/status/updated': {
        const others = s.mcp.filter((m) => m.name !== p.server.name)
        set({ mcp: [...others, p.server].sort((a, b) => a.name.localeCompare(b.name)) })
        break
      }
      case 'automation/run/updated': {
        const runs = [p.run, ...s.automationRuns.filter((r) => r.id !== p.run.id)]
        set({ automationRuns: runs, automationUnread: runs.filter((r) => r.unread && !r.archived).length })
        break
      }
      case 'memory/proposed':
        set({ proposedMemories: [...p.memories, ...s.proposedMemories] })
        toast(`${p.memories.length} new memory suggestion(s) to review in Settings → Memories`)
        break
      case 'hooks/reviewRequired':
        set({ hooksNeedingReview: p.hooks })
        break
      case 'computerUse/active':
        set({ computerUseActive: p })
        break
      case 'providers/updated':
        set({ providers: p.providers })
        void call('model/list', {}).then((m) => set({ models: m.models, roles: m.roles, hiddenModels: m.hidden })).catch(() => {})
        break
      case 'projects/changed':
        set({ projects: p.projects })
        break
      case 'log':
        if (p.level === 'error') toast(p.message, 'error')
        break
      default:
        break
    }
  },

  addServerRequest: (r) => set({ serverRequests: [...get().serverRequests.filter((x) => x.id !== r.id), r] }),
  dropServerRequest: (id) => set({ serverRequests: get().serverRequests.filter((x) => x.id !== id) }),
  resolveServerRequest: async (id, result, error) => {
    get().dropServerRequest(id)
    await window.odex.respond(id, result, error)
  },
}))

/** Called once the engine is ready (initially and after restarts). */
export async function afterReady(): Promise<void> {
  const s = useApp.getState()
  await Promise.allSettled([s.refreshThreads(), s.refreshProjects(), s.refreshModels()])
  void call('mcp/list', {}).then((r) => useApp.setState({ mcp: r.servers })).catch(() => {})
  void call('automation/runs', { unreadOnly: false, includeArchived: false, limit: 200 })
    .then((r) => useApp.setState({ automationRuns: r.runs, automationUnread: r.unreadCount }))
    .catch(() => {})
  // re-attach loaded threads after an engine restart
  const st = useApp.getState()
  for (const id of Object.keys(st.threads)) {
    if (st.threads[id].loaded) void st.loadThread(id)
  }
  const init = st.engine.init
  if (init?.needsOnboarding && !st.settings?.onboarded) st.setUi({ onboardingOpen: true })
}

/** Flatten a thread's turns into renderable rows. */
export function threadItems(ts: ThreadState | undefined): Array<{ turn: Turn; item: ThreadItem }> {
  if (!ts) return []
  const out: Array<{ turn: Turn; item: ThreadItem }> = []
  for (const turn of ts.turns) for (const item of turn.items) out.push({ turn, item })
  return out
}

export function selectedThread(): ThreadState | undefined {
  const s = useApp.getState()
  return s.selectedThreadId ? s.threads[s.selectedThreadId] : undefined
}

export function isRunning(t: Thread | undefined): boolean {
  return !!t && (t.status === 'running' || t.status === 'waitingApproval' || t.status === 'reconnecting' || t.status === 'compacting')
}

// Exposed for end-to-end tests and debugging from devtools.
;(window as unknown as { __odexStore?: typeof useApp }).__odexStore = useApp
