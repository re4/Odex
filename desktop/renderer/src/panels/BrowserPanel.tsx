import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from 'react'
import { createPortal } from 'react-dom'
import { ArrowLeft, ArrowRight, Bot, Code2, Globe, History, Lock, MessageSquarePlus, MoreHorizontal, MousePointerClick, Plus, RotateCw, TriangleAlert, X } from 'lucide-react'
import { useApp } from '@/store/app'
import { toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { bindings, canon } from '@/lib/shortcuts'
import { Menu, relativeTime } from '@/components/ui'
import '@/styles/browser.css'

export interface BrowserTab {
  id: string
  url: string
  title: string
  active: boolean
  threadId: string | null
  agentActive: boolean
  loading: boolean
  crashed: boolean
  canGoBack: boolean
  canGoForward: boolean
}

export interface BrowserState {
  tabs: BrowserTab[]
  activeId: string | null
  visible: boolean
  bounds: { x: number; y: number; width: number; height: number } | null
}

export interface HistoryEntry {
  url: string
  title: string
  at: number
}

interface PickResult {
  url: string
  title?: string
  selector: string | null
  bounds: { x: number; y: number; width: number; height: number } | null
  text?: string
  comment?: string
  screenshotUrl: string | null
}

const browser = () => window.odex.browser

/** Anything that would sit on top of the native page view. */
const OVERLAY_SELECTOR = '.modal-backdrop, .menu, [data-overlay]'

const LOCAL_PORTS = [3000, 5173, 8000, 8080, 4200]

function isBlank(url: string | undefined): boolean {
  return !url || url === 'about:blank'
}

function tabTitle(t: BrowserTab): string {
  if (isBlank(t.url)) return 'New tab'
  return t.title || t.url
}

/** Latest visit per URL, newest first, optionally filtered. */
export function dedupeHistory(h: HistoryEntry[], q = '', limit = 50): HistoryEntry[] {
  const seen = new Set<string>()
  const out: HistoryEntry[] = []
  const needle = q.trim().toLowerCase()
  for (let i = h.length - 1; i >= 0 && out.length < limit; i--) {
    const e = h[i]
    if (seen.has(e.url)) continue
    seen.add(e.url)
    if (needle && !e.url.toLowerCase().includes(needle) && !(e.title || '').toLowerCase().includes(needle)) continue
    out.push(e)
  }
  return out
}

function useBrowserState(): BrowserState {
  const [st, setSt] = useState<BrowserState>({ tabs: [], activeId: null, visible: false, bounds: null })
  useEffect(() => {
    let live = true
    void browser()
      .state()
      .then((s: BrowserState) => live && setSt(s))
    const off = browser().onState((s: BrowserState) => setSt(s))
    return () => {
      live = false
      off()
    }
  }, [])
  return st
}

/** True while a palette, dialog, menu or takeover overlay is open. */
function useOverlayOpen(): boolean {
  const flag = useApp((s) => s.ui.paletteOpen || s.ui.contextViewOpen || s.ui.onboardingOpen || (s.computerUseActive.active && s.computerUseActive.takeover))
  const [dom, setDom] = useState(() => !!document.querySelector(OVERLAY_SELECTOR))
  useEffect(() => {
    const check = () => setDom(!!document.querySelector(OVERLAY_SELECTOR))
    check()
    const mo = new MutationObserver(check)
    mo.observe(document.body, { childList: true, subtree: true })
    return () => mo.disconnect()
  }, [])
  return flag || dom
}

/**
 * Keep the native page view glued to `host` (CSS px; main scales by zoom).
 * The view is detached whenever `show` is false and when this unmounts.
 * Toasts in the bottom-right corner clip the view so they stay readable.
 */
function useNativeView(host: RefObject<HTMLDivElement | null>, show: boolean): void {
  useEffect(() => {
    if (!show) {
      void browser().setBounds(null)
      return
    }
    let raf = 0
    let last = ''
    const measure = () => {
      raf = 0
      const el = host.current
      if (!el) return
      const r = el.getBoundingClientRect()
      let bottom = r.bottom
      document.querySelectorAll('.toasts').forEach((c) => {
        if (!c.childElementCount) return
        const t = c.getBoundingClientRect()
        if (t.left < r.right && t.right > r.left && t.top < bottom && t.bottom > r.top) bottom = Math.max(r.top, t.top - 8)
      })
      const b = { x: Math.round(r.left), y: Math.round(r.top), width: Math.round(r.width), height: Math.round(bottom - r.top) }
      const ok = b.width > 4 && b.height > 4
      const key = ok ? `${b.x},${b.y},${b.width},${b.height}` : 'none'
      if (key === last) return
      last = key
      void browser().setBounds(ok ? b : null)
    }
    const schedule = () => {
      if (!raf) raf = requestAnimationFrame(measure)
    }
    measure()
    const ro = new ResizeObserver(schedule)
    if (host.current) ro.observe(host.current)
    ro.observe(document.body)
    const mo = new MutationObserver(schedule)
    mo.observe(document.body, { childList: true, subtree: true })
    window.addEventListener('resize', schedule)
    // position-only changes (banners above, sidebar) don't resize the host
    const iv = setInterval(schedule, 400)
    return () => {
      cancelAnimationFrame(raf)
      ro.disconnect()
      mo.disconnect()
      clearInterval(iv)
      window.removeEventListener('resize', schedule)
      void browser().setBounds(null)
    }
  }, [show, host])
}

function HistoryPopover({ anchor, onClose, onOpen }: { anchor: HTMLElement; onClose: () => void; onOpen: (url: string) => void }) {
  const ref = useRef<HTMLDivElement>(null)
  const [items, setItems] = useState<HistoryEntry[] | null>(null)
  const [q, setQ] = useState('')
  const [pos, setPos] = useState({ left: -9999, top: -9999 })
  useEffect(() => {
    void browser()
      .history()
      .then((h: HistoryEntry[]) => setItems(h))
  }, [])
  useLayoutEffect(() => {
    const r = anchor.getBoundingClientRect()
    const w = ref.current?.offsetWidth ?? 380
    setPos({ left: Math.max(8, Math.min(r.right - w, window.innerWidth - w - 8)), top: r.bottom + 4 })
  }, [anchor])
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node) && !anchor.contains(e.target as Node)) onClose()
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
      }
    }
    window.addEventListener('mousedown', onDown)
    window.addEventListener('keydown', onKey, true)
    return () => {
      window.removeEventListener('mousedown', onDown)
      window.removeEventListener('keydown', onKey, true)
    }
  }, [anchor, onClose])
  const list = useMemo(() => dedupeHistory(items ?? [], q, 60), [items, q])
  return createPortal(
    <div ref={ref} className="menu browser-history" role="dialog" aria-label="Browsing history" style={{ left: pos.left, top: pos.top }}>
      <input
        className="input"
        autoFocus
        placeholder="Search history"
        aria-label="Search history"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && list[0]) onOpen(list[0].url)
        }}
      />
      <div className="browser-history-list">
        {items === null && <div className="xs subtle browser-history-empty">Loading…</div>}
        {items !== null && list.length === 0 && <div className="xs subtle browser-history-empty">{q ? 'No matches.' : 'No history yet.'}</div>}
        {list.map((e) => (
          <button key={e.url} className="menu-item" title={e.url} onClick={() => onOpen(e.url)}>
            <span className="browser-history-text">
              <span className="ellipsis">{e.title || e.url}</span>
              <span className="xs subtle ellipsis">{e.url}</span>
            </span>
            <span className="hint">{relativeTime(e.at)}</span>
          </button>
        ))}
      </div>
      <div className="menu-sep" />
      <button
        className="menu-item"
        onClick={() => {
          onClose()
          A.openSettings('browser')
        }}
      >
        Manage history and site data…
      </button>
    </div>,
    document.body,
  )
}

function NewTabPage({ onOpen }: { onOpen: (url: string) => void }) {
  const [recent, setRecent] = useState<HistoryEntry[]>([])
  useEffect(() => {
    void browser()
      .history()
      .then((h: HistoryEntry[]) => setRecent(dedupeHistory(h, '', 6)))
  }, [])
  return (
    <div className="browser-ntp">
      <Globe size={26} className="subtle" />
      <div className="browser-ntp-title">New tab</div>
      <div className="small muted">Preview a local dev server or browse the web. Pages use their own profile, separate from Odex and your browser.</div>
      <div className="section-title">Local servers</div>
      <div className="browser-ntp-chips">
        {LOCAL_PORTS.map((p) => (
          <button key={p} className="chip mono" onClick={() => onOpen(`http://localhost:${p}`)}>
            localhost:{p}
          </button>
        ))}
      </div>
      {recent.length > 0 && (
        <>
          <div className="section-title">Recent</div>
          <div className="browser-ntp-recent">
            {recent.map((e) => (
              <button key={e.url} className="menu-item" title={e.url} onClick={() => onOpen(e.url)}>
                <Globe size={13} className="subtle" />
                <span className="ellipsis grow">{e.title || e.url}</span>
                <span className="hint">{relativeTime(e.at)}</span>
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  )
}

export function BrowserPanel() {
  const st = useBrowserState()
  const overlay = useOverlayOpen()
  const active = st.tabs.find((t) => t.id === st.activeId) ?? null
  const blank = !active || isBlank(active.url)
  const [picking, setPicking] = useState<string | null>(null)
  const [historyAnchor, setHistoryAnchor] = useState<HTMLElement | null>(null)
  const [moreAnchor, setMoreAnchor] = useState<HTMLElement | null>(null)
  const [input, setInput] = useState('')
  const [editing, setEditing] = useState(false)
  const urlRef = useRef<HTMLInputElement>(null)
  const host = useRef<HTMLDivElement>(null)
  const threadName = useApp((s) => {
    const id = active?.threadId
    if (!id) return ''
    const t = s.threads[id]?.thread
    return t ? t.name || t.preview || 'Untitled thread' : 'a thread'
  })

  const showNative = !!active && !overlay && !blank && !active.crashed
  useNativeView(host, showNative)

  // the address bar follows the page unless the user is typing
  useEffect(() => {
    if (!editing) setInput(active && !isBlank(active.url) ? active.url : '')
  }, [active?.id, active?.url, editing]) // eslint-disable-line react-hooks/exhaustive-deps

  // show a tab when this window has none (never steal one shown in another window)
  useEffect(() => {
    if (st.activeId || !st.tabs.length) return
    const free = [...st.tabs].reverse().find((t) => !t.active)
    if (free) void browser().show(free.id)
  }, [st])

  // a running pick is cancelled when the panel goes away
  const pickingRef = useRef<string | null>(null)
  useEffect(() => {
    pickingRef.current = picking
  }, [picking])
  useEffect(
    () => () => {
      if (pickingRef.current) void browser().pickCancel(pickingRef.current)
    },
    [],
  )

  const open = useCallback(
    (raw: string) => {
      const v = raw.trim()
      if (!v) return
      if (active) void browser().navigate(active.id, v)
      else void browser().newTab(v)
      setEditing(false)
      urlRef.current?.blur()
    },
    [active],
  )

  const closeTab = useCallback(
    (id: string) => {
      if (id === st.activeId) {
        const i = st.tabs.findIndex((t) => t.id === id)
        const next = st.tabs.filter((t) => t.id !== id && !t.active)[Math.max(0, i - 1)] ?? null
        void browser().show(next?.id ?? null)
      }
      void browser().close(id)
    },
    [st],
  )

  const comment = async () => {
    if (!active || picking || blank) return
    setPicking(active.id)
    let r: PickResult | null = null
    try {
      r = (await browser().pick(active.id)) as PickResult | null
    } catch (e) {
      toast(`Could not start comment mode: ${(e as Error).message}`, 'error')
    } finally {
      setPicking(null)
    }
    if (!r) return
    const what = r.selector ? (r.selector.length > 48 ? `…${r.selector.slice(-48)}` : r.selector) : 'the selected area'
    const text = r.comment ?? (await A.promptText(`Comment on ${what}`))
    if (!text?.trim()) return
    window.dispatchEvent(
      new CustomEvent('odex:attach', {
        detail: { type: 'browserComment', url: r.url, selector: r.selector ?? null, bounds: r.bounds ?? null, comment: text.trim(), screenshotUrl: r.screenshotUrl ?? null },
      }),
    )
    toast('Page comment added to your message')
  }

  // app shortcuts pressed while the page had keyboard focus
  useEffect(
    () =>
      browser().onKey(({ keys }) => {
        if (keys === 'Mod+L') {
          urlRef.current?.focus()
          urlRef.current?.select()
          return
        }
        if (keys === 'Mod+W') {
          if (st.activeId) closeTab(st.activeId)
          return
        }
        const b = bindings()
        const id = Object.keys(b).find((k) => canon(b[k]) === canon(keys))
        if (id) window.dispatchEvent(new CustomEvent('odex:shortcut', { detail: id }))
      }),
    [st.activeId, closeTab],
  )

  const secure = active?.url.startsWith('https://')
  const http = !!active && /^https?:/.test(active.url)

  return (
    <div className="browser-panel">
      <div className="browser-tabs" role="tablist" aria-label="Browser tabs">
        {st.tabs.map((t) => {
          const title = tabTitle(t)
          return (
            <div
              key={t.id}
              role="tab"
              tabIndex={0}
              aria-selected={t.id === st.activeId}
              className={`browser-tab ${t.threadId ? 'agent' : ''}`}
              title={isBlank(t.url) ? title : `${title}\n${t.url}`}
              onClick={() => void browser().show(t.id)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') void browser().show(t.id)
              }}
              onAuxClick={(e) => {
                if (e.button === 1) closeTab(t.id)
              }}
            >
              {t.loading ? <span className="spinner browser-tab-spinner" /> : t.threadId ? <Bot size={13} className="browser-tab-icon" /> : <Globe size={13} className="browser-tab-icon" />}
              <span className="ellipsis grow">{title}</span>
              {t.agentActive && <span className="dot accent pulse" aria-label="Agent active" title="The agent is using this tab" />}
              <button
                className="icon-btn sm browser-tab-close"
                aria-label={`Close tab ${title}`}
                onClick={(e) => {
                  e.stopPropagation()
                  closeTab(t.id)
                }}
              >
                <X size={12} />
              </button>
            </div>
          )
        })}
        <button className="icon-btn sm browser-newtab" aria-label="New tab" title="New tab (Ctrl+T)" onClick={() => void browser().newTab()}>
          <Plus size={14} />
        </button>
      </div>

      <div className="browser-toolbar">
        <button className="icon-btn sm" aria-label="Back" title="Back (Alt+Left)" disabled={!active?.canGoBack} onClick={() => active && void browser().command(active.id, 'back')}>
          <ArrowLeft size={14} />
        </button>
        <button className="icon-btn sm" aria-label="Forward" title="Forward (Alt+Right)" disabled={!active?.canGoForward} onClick={() => active && void browser().command(active.id, 'forward')}>
          <ArrowRight size={14} />
        </button>
        {active?.loading ? (
          <button className="icon-btn sm" aria-label="Stop" title="Stop loading" onClick={() => void browser().command(active.id, 'stop')}>
            <X size={14} />
          </button>
        ) : (
          <button className="icon-btn sm" aria-label="Reload" title="Reload (F5)" disabled={!active || blank} onClick={() => active && void browser().command(active.id, 'reload')}>
            <RotateCw size={13} />
          </button>
        )}
        <form
          className="browser-url"
          onSubmit={(e) => {
            e.preventDefault()
            open(input)
          }}
        >
          {secure && !editing ? <Lock size={12} className="subtle" aria-label="Secure connection" /> : <Globe size={12} className="subtle" />}
          <input
            ref={urlRef}
            value={input}
            placeholder="Search or enter address (try localhost:3000)"
            aria-label="Address"
            spellCheck={false}
            autoComplete="off"
            onFocus={(e) => {
              setEditing(true)
              e.target.select()
            }}
            onBlur={() => setEditing(false)}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.stopPropagation()
                setInput(active && !isBlank(active.url) ? active.url : '')
                e.currentTarget.blur()
              }
            }}
          />
        </form>
        <button
          className={`icon-btn sm ${historyAnchor ? 'active' : ''}`}
          aria-label="History"
          title="History"
          onClick={(e) => {
            const el = e.currentTarget
            setHistoryAnchor((a) => (a ? null : el))
          }}
        >
          <History size={14} />
        </button>
        <button className={`icon-btn sm ${picking ? 'active' : ''}`} aria-label="Comment on page" title="Comment on an element or area" disabled={!active || blank || !!active?.crashed} onClick={() => void comment()}>
          <MessageSquarePlus size={14} />
        </button>
        <button className="icon-btn sm" aria-label="Developer tools" title="Developer tools (F12)" disabled={!active} onClick={() => active && void browser().command(active.id, 'devtools')}>
          <Code2 size={14} />
        </button>
        <button
          className={`icon-btn sm ${moreAnchor ? 'active' : ''}`}
          aria-label="More browser actions"
          onClick={(e) => {
            const el = e.currentTarget
            setMoreAnchor((a) => (a ? null : el))
          }}
        >
          <MoreHorizontal size={14} />
        </button>
      </div>

      {active?.threadId && (
        <div className={`browser-bar agent ${active.agentActive ? 'live' : ''}`} role="status">
          <Bot size={13} />
          <span className="grow ellipsis">
            {active.agentActive ? 'The agent is using this tab' : 'Agent tab'} · {threadName}
          </span>
          <button className="btn btn-sm btn-ghost" onClick={() => void useApp.getState().selectThread(active.threadId)}>
            Open thread
          </button>
        </div>
      )}
      {picking && (
        <div className="browser-bar pick" role="status">
          <MousePointerClick size={13} />
          <span className="grow">Click an element or drag across an area to comment. Esc cancels.</span>
          <button className="btn btn-sm btn-ghost" onClick={() => void browser().pickCancel(picking)}>
            Cancel
          </button>
        </div>
      )}

      <div ref={host} className="browser-viewport" data-browser-viewport="">
        {active?.crashed ? (
          <div className="empty">
            <TriangleAlert size={22} color="var(--warning)" />
            <div>This page stopped responding.</div>
            <button className="btn btn-sm" onClick={() => void browser().command(active.id, 'reload')}>
              Reload
            </button>
          </div>
        ) : blank ? (
          <NewTabPage onOpen={open} />
        ) : null}
      </div>

      {historyAnchor && (
        <HistoryPopover
          anchor={historyAnchor}
          onClose={() => setHistoryAnchor(null)}
          onOpen={(url) => {
            setHistoryAnchor(null)
            open(url)
          }}
        />
      )}
      {/* Menu listens for Escape even without an anchor, so only mount it while open */}
      {moreAnchor && (
        <Menu
          anchor={moreAnchor}
          onClose={() => setMoreAnchor(null)}
          align="right"
          items={[
            { label: 'New tab', hint: 'Ctrl+T', onSelect: () => void browser().newTab() },
            { label: 'Hard reload', disabled: !active || blank, onSelect: () => active && void browser().command(active.id, 'hardReload') },
            { label: 'Copy address', disabled: !active || blank, onSelect: () => active && A.copy(active.url, 'Address copied') },
            { label: 'Open in system browser', disabled: !http, onSelect: () => active && void window.odex.shell.openExternal(active.url) },
            { separator: true, label: '' },
            { label: 'Close tab', hint: 'Ctrl+W', disabled: !active, onSelect: () => active && closeTab(active.id) },
            { label: 'Browser settings…', onSelect: () => A.openSettings('browser') },
          ]}
        />
      )}
    </div>
  )
}
