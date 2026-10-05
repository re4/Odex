import { useEffect, useMemo, useState } from 'react'
import { PanelLeft, PanelBottom, PanelRight } from 'lucide-react'
import { afterReady, useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { useShortcuts } from '@/lib/shortcuts'
import * as A from '@/lib/actions'
import { Modal, ResizeHandle, Toasts } from '@/components/ui'
import { Sidebar } from '@/views/Sidebar'
import { ThreadView } from '@/views/ThreadView'
import { HomeView } from '@/views/HomeView'
import { ServerRequests } from '@/views/ServerRequests'
import { Onboarding } from '@/views/Onboarding'
import { SettingsView } from '@/views/settings/SettingsView'
import { SidePanel } from '@/panels/SidePanel'
import { BottomPanel } from '@/panels/BottomPanel'
import { CommandPalette } from '@/views/CommandPalette'
import { ContextView } from '@/views/ContextView'
import { ActivityView } from '@/views/ActivityView'
import { AutomationsView } from '@/views/AutomationsView'
import { SearchView } from '@/views/SearchView'
import { handleDeepLink } from '@/lib/deeplinks'

function applyTheme(): void {
  const s = useApp.getState().settings
  const root = document.documentElement
  if (!s) return
  const dark = s.theme === 'dark' || (s.theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)
  root.dataset.theme = dark ? 'dark' : 'light'
  root.dataset.density = s.density
  root.dataset.motion = s.reducedMotion === 'system' ? '' : s.reducedMotion
  root.style.setProperty('--accent', s.accent)
  root.style.setProperty('--font-ui', s.uiFont)
  root.style.setProperty('--font-code', s.codeFont)
  root.style.setProperty('--font-size', `${s.fontSize}px`)
}

function PromptHost() {
  const [p, setP] = useState<{ title: string; initial: string; resolve: (v: string | null) => void } | null>(null)
  const [c, setC] = useState<{ title: string; body: string; confirmLabel: string; danger: boolean; resolve: (v: boolean) => void } | null>(null)
  const [val, setVal] = useState('')
  useEffect(() => {
    const onPrompt = (e: Event) => {
      const d = (e as CustomEvent).detail
      setVal(d.initial)
      setP(d)
    }
    const onConfirm = (e: Event) => setC((e as CustomEvent).detail)
    window.addEventListener('odex:prompt', onPrompt)
    window.addEventListener('odex:confirm', onConfirm)
    return () => {
      window.removeEventListener('odex:prompt', onPrompt)
      window.removeEventListener('odex:confirm', onConfirm)
    }
  }, [])
  return (
    <>
      {p && (
        <Modal
          title={p.title}
          onClose={() => {
            p.resolve(null)
            setP(null)
          }}
          footer={
            <>
              <button className="btn" onClick={() => (p.resolve(null), setP(null))}>
                Cancel
              </button>
              <button className="btn btn-primary" onClick={() => (p.resolve(val), setP(null))}>
                OK
              </button>
            </>
          }
        >
          <input
            className="input"
            autoFocus
            value={val}
            onChange={(e) => setVal(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                p.resolve(val)
                setP(null)
              }
            }}
          />
        </Modal>
      )}
      {c && (
        <Modal
          title={c.title}
          onClose={() => (c.resolve(false), setC(null))}
          footer={
            <>
              <button className="btn" onClick={() => (c.resolve(false), setC(null))}>
                Cancel
              </button>
              <button className={`btn ${c.danger ? 'btn-danger' : 'btn-primary'}`} autoFocus onClick={() => (c.resolve(true), setC(null))}>
                {c.confirmLabel}
              </button>
            </>
          }
        >
          <p className="selectable" style={{ margin: 0, whiteSpace: 'pre-wrap' }}>
            {c.body}
          </p>
        </Modal>
      )}
    </>
  )
}

function TitleBar() {
  const ui = useApp((s) => s.ui)
  const setUi = useApp((s) => s.setUi)
  const thread = useApp((s) => (s.selectedThreadId ? s.threads[s.selectedThreadId]?.thread : undefined))
  const engine = useApp((s) => s.engine)
  const mac = window.odex.platform === 'darwin'
  return (
    <div className={`titlebar ${mac ? 'mac' : ''}`}>
      <button className="icon-btn" aria-label="Toggle sidebar" title="Toggle sidebar (Ctrl+B)" onClick={() => setUi({ sidebarOpen: !ui.sidebarOpen })}>
        <PanelLeft size={16} />
      </button>
      <div className="brand">
        <img src="./odex-mark.svg" width={16} height={16} alt="" onError={(e) => ((e.target as HTMLImageElement).style.display = 'none')} />
        Odex
      </div>
      <div className="title ellipsis">{ui.view === 'thread' && thread ? thread.name || thread.preview || 'New thread' : ''}</div>
      <div className="spacer" />
      {engine.state !== 'ready' && (
        <span className={`badge ${engine.state === 'failed' ? 'danger' : 'warning'} no-drag`} title={engine.error ?? ''}>
          engine {engine.state}
        </span>
      )}
      <button className={`icon-btn ${ui.bottomOpen ? 'active' : ''}`} aria-label="Toggle terminal panel" title="Toggle bottom panel (Ctrl+J)" onClick={() => setUi({ bottomOpen: !ui.bottomOpen })}>
        <PanelBottom size={16} />
      </button>
      <button className={`icon-btn ${ui.sidePanelOpen ? 'active' : ''}`} aria-label="Toggle side panel" title="Toggle side panel" onClick={() => setUi({ sidePanelOpen: !ui.sidePanelOpen })}>
        <PanelRight size={16} />
      </button>
    </div>
  )
}

export function App() {
  const settings = useApp((s) => s.settings)
  const ui = useApp((s) => s.ui)
  const setUi = useApp((s) => s.setUi)
  const engine = useApp((s) => s.engine)
  const cu = useApp((s) => s.computerUseActive)
  const killSwitch = useApp((s) => s.killSwitch)
  const hooks = useApp((s) => s.hooksNeedingReview)

  // engine + IPC wiring
  useEffect(() => {
    const s = useApp.getState()
    const params = new URLSearchParams(location.search)
    if (params.get('popout')) setUi({ popout: true, sidebarOpen: false })
    void s.bootstrap().then(() => {
      const t = params.get('thread')
      if (t) void useApp.getState().selectThread(t)
    })
    const offs = [
      window.odex.onEngineState((st) => {
        const prev = useApp.getState().engine.state
        useApp.setState({ engine: { state: st.state, error: st.error, init: st.init } })
        if (st.state === 'ready' && prev !== 'ready') void afterReady()
        if (st.state === 'restarting') toast('The engine stopped unexpectedly and is restarting…', 'error')
      }),
      window.odex.onNotification(({ method, params }) => useApp.getState().applyNotification(method, params)),
      window.odex.onServerRequest((r) => useApp.getState().addServerRequest(r)),
      window.odex.onServerRequestResolved((id) => useApp.getState().dropServerRequest(id)),
      window.odex.settings.onChange((st) => {
        useApp.setState({ settings: st })
        applyTheme()
      }),
      window.odex.onNativeTheme(() => applyTheme()),
      window.odex.onKillSwitch((on) => useApp.setState({ killSwitch: on })),
      window.odex.onDeepLink((url) => void handleDeepLink(url)),
      window.odex.onCommand((c) => {
        if (c.command === 'newThread') void useApp.getState().selectThread(null)
        if (c.command === 'quickChat') void A.createThread({ kind: 'quickChat' })
        if (c.command === 'openThread' && c.threadId) void useApp.getState().selectThread(c.threadId)
        if (c.command === 'nextAttention') nextAttention()
      }),
      window.odex.onAppshot((shot) => window.dispatchEvent(new CustomEvent('odex:attach', { detail: { type: 'appshot', ...shot } }))),
    ]
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    mq.addEventListener('change', applyTheme)
    return () => {
      offs.forEach((o) => o())
      mq.removeEventListener('change', applyTheme)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(applyTheme, [settings])

  const handlers = useMemo(
    () => ({
      palette: () => setUi({ paletteOpen: true, paletteMode: 'commands' }),
      palette2: () => setUi({ paletteOpen: true, paletteMode: 'commands' }),
      fileSearch: () => setUi({ paletteOpen: true, paletteMode: 'files' }),
      settings: () => A.openSettings('general'),
      shortcuts: () => A.openSettings('shortcuts'),
      openFolder: () => void A.addProjectFromDialog(),
      toggleSidebar: () => setUi({ sidebarOpen: !useApp.getState().ui.sidebarOpen }),
      toggleBottom: () => setUi({ bottomOpen: !useApp.getState().ui.bottomOpen }),
      toggleTerminal: () => {
        const u = useApp.getState().ui
        if (useApp.getState().settings?.terminalLocation === 'right') setUi({ sidePanelOpen: !(u.sidePanelOpen && u.sidePanelTab === 'terminal'), sidePanelTab: 'terminal' })
        else setUi({ bottomOpen: !u.bottomOpen })
      },
      openReview: () => setUi({ sidePanelOpen: true, sidePanelTab: 'review' }),
      toggleFileTree: () => setUi({ sidePanelOpen: true, sidePanelTab: 'files' }),
      cycleLayout: () => {
        const u = useApp.getState().ui
        if (!u.sidePanelOpen) setUi({ sidePanelOpen: true })
        else if (u.sidePanelWidth < 700) setUi({ sidePanelWidth: Math.round(window.innerWidth * 0.62) })
        else setUi({ sidePanelOpen: false, sidePanelWidth: 460 })
      },
      newBrowserTab: () => {
        setUi({ sidePanelOpen: true, sidePanelTab: 'browser' })
        void window.odex.browser.newTab()
      },
      newThread: () => void useApp.getState().selectThread(null),
      newThread2: () => void useApp.getState().selectThread(null),
      newStandalone: () => {
        setUi({ newThreadProjectId: null })
        void useApp.getState().selectThread(null)
      },
      quickChat: () => void A.createThread({ kind: 'quickChat' }),
      archive: () => withThread((id) => A.archiveThread(id)),
      markUnread: () => withThread((id) => A.markUnread(id)),
      pin: () => withThread((id) => A.togglePin(id)),
      rename: () => withThread((id) => A.renameThread(id)),
      sideChat: () => withThread((id) => A.forkThread(id, undefined, 'local', true)),
      find: () => setUi({ findOpen: true }),
      back: () => navigateHistory(-1),
      forward: () => navigateHistory(1),
      prevThread: () => cycleThread(-1),
      nextThread: () => cycleThread(1),
      nextAttention,
      clearUnread: () => {
        for (const ts of Object.values(useApp.getState().threads)) if (ts.thread.unread) void call('thread/update', { threadId: ts.thread.id, unread: false })
      },
      activity: () => setUi({ view: useApp.getState().ui.view === 'activity' ? 'thread' : 'activity' }),
      modelPicker: () => window.dispatchEvent(new CustomEvent('odex:open-picker', { detail: 'model' })),
      projectPicker: () => window.dispatchEvent(new CustomEvent('odex:open-picker', { detail: 'project' })),
      zoomIn: () => void window.odex.win.zoom(Math.min(2, (useApp.getState().settings?.zoom ?? 1) + 0.1)),
      zoomOut: () => void window.odex.win.zoom(Math.max(0.6, (useApp.getState().settings?.zoom ?? 1) - 0.1)),
      zoomReset: () => void window.odex.win.zoom(1),
      fullscreen: () => void window.odex.win.fullscreen(),
      copyDeepLink: () => withThread((id) => A.copy(`odex://threads/${id}`, 'Deep link copied')),
      copyThreadId: () => withThread((id) => A.copy(id, 'Thread id copied')),
      copyCwd: () => withThread((id) => A.copy(useApp.getState().threads[id]?.thread.cwd ?? '', 'Working directory copied')),
      runAction1: () => window.dispatchEvent(new CustomEvent('odex:run-action', { detail: 0 })),
      quit: () => void window.odex.app.quit(),
      ...Object.fromEntries(Array.from({ length: 9 }, (_, i) => [`goto${i + 1}`, () => gotoThread(i)])),
    }),
    [setUi],
  )
  useShortcuts(handlers)

  if (!settings) return <div className="app" />
  const showSide = ui.sidePanelOpen && (ui.view === 'thread' || ui.view === 'home')
  return (
    <div className="app">
      <TitleBar />
      {engine.state === 'failed' && (
        <div className="banner danger" role="alert">
          <span className="grow selectable">The Odex engine is not running: {engine.error}</span>
          <button className="btn btn-sm" onClick={() => void window.odex.restartEngine()}>
            Restart engine
          </button>
        </div>
      )}
      {killSwitch && (
        <div className="banner danger" role="alert">
          <span className="grow">Kill switch engaged: computer and browser actions are stopped.</span>
          <button className="btn btn-sm" onClick={() => void window.odex.app.killSwitch(false)}>
            Release
          </button>
        </div>
      )}
      {hooks.length > 0 && (
        <div className="banner info">
          <span className="grow">{hooks.length} hook(s) need your review before they can run.</span>
          <button className="btn btn-sm" onClick={() => A.openSettings('hooks')}>
            Review hooks
          </button>
        </div>
      )}
      <div className="main">
        {ui.sidebarOpen && !ui.popout && (
          <>
            <Sidebar width={ui.sidebarWidth} />
            <ResizeHandle axis="x" value={ui.sidebarWidth} min={200} max={480} onChange={(v) => setUi({ sidebarWidth: v })} label="Resize sidebar" />
          </>
        )}
        <div className="center">
          <div className="center-split">
            <div className="center-main">
              {ui.view === 'settings' ? (
                <SettingsView />
              ) : ui.view === 'activity' ? (
                <ActivityView />
              ) : ui.view === 'automations' ? (
                <AutomationsView />
              ) : ui.view === 'search' ? (
                <SearchView />
              ) : ui.view === 'thread' ? (
                <ThreadView />
              ) : (
                <HomeView />
              )}
            </div>
            {showSide && (
              <>
                <ResizeHandle axis="x" invert value={ui.sidePanelWidth} min={300} max={Math.max(320, window.innerWidth - 420)} onChange={(v) => setUi({ sidePanelWidth: v })} label="Resize side panel" />
                <SidePanel width={ui.sidePanelWidth} />
              </>
            )}
          </div>
          {ui.bottomOpen && (
            <>
              <ResizeHandle axis="y" invert value={ui.bottomHeight} min={120} max={Math.max(140, window.innerHeight - 260)} onChange={(v) => setUi({ bottomHeight: v })} label="Resize bottom panel" />
              <BottomPanel height={ui.bottomHeight} />
            </>
          )}
        </div>
      </div>
      <ServerRequests />
      {ui.paletteOpen && <CommandPalette />}
      {ui.contextViewOpen && <ContextView />}
      {ui.onboardingOpen && <Onboarding />}
      <PromptHost />
      <Toasts />
      {cu.active && cu.takeover && (
        <>
          <div className="takeover" />
          <div className="takeover-label">
            Odex is controlling {cu.app || 'the computer'} — press {useApp.getState().settings?.killSwitchHotkey ?? 'Ctrl+Alt+Esc'} to stop
          </div>
        </>
      )}
    </div>
  )
}

function withThread(f: (id: string) => unknown): void {
  const id = useApp.getState().selectedThreadId
  if (id) void f(id)
}

function sortedThreadIds(): string[] {
  const s = useApp.getState()
  return s.threadOrder.filter((id) => s.threads[id] && !s.threads[id].thread.archived)
}

function cycleThread(dir: number): void {
  const ids = sortedThreadIds()
  if (!ids.length) return
  const cur = useApp.getState().selectedThreadId
  const i = cur ? ids.indexOf(cur) : -1
  void useApp.getState().selectThread(ids[(i + dir + ids.length) % ids.length])
}

function gotoThread(i: number): void {
  const ids = sortedThreadIds()
  if (ids[i]) void useApp.getState().selectThread(ids[i])
}

function navigateHistory(dir: number): void {
  const s = useApp.getState()
  const idx = s.historyIndex + dir
  const id = s.history[idx]
  if (!id) return
  useApp.setState({ historyIndex: idx, selectedThreadId: id })
  s.setUi({ view: 'thread' })
  if (!s.threads[id]?.loaded) void s.loadThread(id)
}

function nextAttention(): void {
  const s = useApp.getState()
  const ids = sortedThreadIds()
  const pick =
    ids.find((id) => s.threads[id].thread.status === 'waitingApproval') ??
    ids.find((id) => s.threads[id].thread.unread && id !== s.selectedThreadId) ??
    ids.find((id) => s.threads[id].thread.status === 'error')
  if (pick) void s.selectThread(pick)
}
