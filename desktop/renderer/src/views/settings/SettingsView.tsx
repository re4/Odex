import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { ArrowLeft, Search } from 'lucide-react'
import { useApp } from '@/store/app'
import { SETTINGS_PANELS } from '@/views/settings/registry'
import { panelKey, searchSettings, settingsIndex, type SettingHit } from '@/views/settings/settingsIndex'
import '@/styles/settings-search.css'

/** Elements in the panel that show a setting: `Row`s (data-setting-label) and headings/labels by text. */
function findSettingElements(root: HTMLElement, match: (text: string) => boolean): HTMLElement[] {
  const out: HTMLElement[] = []
  root.querySelectorAll<HTMLElement>('[data-setting-label]').forEach((el) => {
    if (match(`${el.dataset.settingLabel ?? ''} ${el.dataset.settingHint ?? ''}`)) out.push(el)
  })
  if (out.length) return out
  root.querySelectorAll<HTMLElement>('h3, h4, label, .section-title, .sx-head, .field-label').forEach((el) => {
    if (match(el.textContent ?? '')) out.push(el.closest<HTMLElement>('section, .field, .row') ?? el)
  })
  return out
}

export function SettingsView() {
  const panelId = useApp((s) => s.ui.settingsPanel)
  const setUi = useApp((s) => s.setUi)
  const selected = useApp((s) => s.selectedThreadId)
  const [filter, setFilter] = useState('')
  const [index, setIndex] = useState<SettingHit[] | null>(null)
  const [focus, setFocus] = useState<{ label: string; at: number } | null>(null)
  const mainRef = useRef<HTMLElement>(null)
  const q = filter.trim().toLowerCase()

  // load the deep index on the first search
  useEffect(() => {
    if (q && !index) void settingsIndex().then(setIndex)
  }, [q, index])

  const hits = useMemo(() => (q && index ? searchSettings(index, q) : []), [q, index])
  const hitPanels = useMemo(() => new Set(hits.map((h) => panelKey(h.panel))), [hits])
  const visible = SETTINGS_PANELS.filter((p) => !q || p.label.toLowerCase().includes(q) || (p.keywords ?? '').includes(q) || hitPanels.has(panelKey(p.id)))
  const panel = SETTINGS_PANELS.find((p) => p.id === panelId) ?? SETTINGS_PANELS[0]
  const groups = [...new Set(visible.map((p) => p.group))]
  const Panel = panel.component
  const panelFor = (h: SettingHit) => SETTINGS_PANELS.find((p) => panelKey(p.id) === panelKey(h.panel))
  const shownHits = hits.filter((h) => panelFor(h)).slice(0, 12)

  // highlight matching rows in the open panel while searching; scroll to a picked one
  useLayoutEffect(() => {
    const root = mainRef.current
    if (!root) return
    let raf = 0
    const apply = () => {
      root.querySelectorAll('.setting-match').forEach((el) => el.classList.remove('setting-match'))
      if (!q) return
      const words = q.split(/\s+/).filter(Boolean)
      for (const el of findSettingElements(root, (t) => words.every((w) => t.toLowerCase().includes(w)))) el.classList.add('setting-match')
    }
    apply()
    // panels render rows after loading their data: re-apply on DOM changes
    const mo = new MutationObserver(() => {
      cancelAnimationFrame(raf)
      raf = requestAnimationFrame(apply)
    })
    mo.observe(root, { childList: true, subtree: true })
    return () => {
      mo.disconnect()
      cancelAnimationFrame(raf)
    }
  }, [q, panel.id])

  useEffect(() => {
    const root = mainRef.current
    if (!root || !focus) return
    let tries = 0
    const timer = setInterval(() => {
      const label = focus.label.toLowerCase()
      const el = findSettingElements(root, (t) => t.toLowerCase().includes(label))[0]
      if (el || ++tries > 20) {
        clearInterval(timer)
        if (!el) return
        el.scrollIntoView({ block: 'center' })
        el.classList.remove('setting-flash')
        void el.offsetWidth
        el.classList.add('setting-flash')
      }
    }, 50)
    return () => clearInterval(timer)
  }, [focus, panel.id])

  return (
    <div className="row" style={{ flex: 1, minHeight: 0, alignItems: 'stretch', gap: 0 }}>
      <nav style={{ width: 220, flex: 'none', borderRight: '1px solid var(--border)', padding: 8, overflowY: 'auto' }} aria-label="Settings sections">
        <button className="nav-item" style={{ width: '100%', marginBottom: 6 }} onClick={() => setUi({ view: selected ? 'thread' : 'home' })}>
          <ArrowLeft size={14} /> Back to app
        </button>
        <div className="row" style={{ position: 'relative', marginBottom: 8 }}>
          <Search size={13} style={{ position: 'absolute', left: 8, color: 'var(--fg-subtle)' }} />
          <input
            className="input"
            style={{ paddingLeft: 26, height: 28 }}
            placeholder="Search settings"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && shownHits[0]) {
                setUi({ settingsPanel: panelFor(shownHits[0])!.id })
                setFocus({ label: shownHits[0].label, at: Date.now() })
              } else if (e.key === 'Escape' && filter) {
                e.stopPropagation()
                setFilter('')
              }
            }}
            aria-label="Search settings"
          />
        </div>
        {shownHits.length > 0 && (
          <div className="settings-hits" role="list" aria-label="Matching settings">
            <div className="section-title" style={{ padding: '4px 8px' }}>
              Settings
            </div>
            {shownHits.map((h) => {
              const p = panelFor(h)!
              return (
                <button
                  key={`${h.panel}:${h.label}`}
                  role="listitem"
                  className="nav-item settings-hit"
                  title={h.hint ?? h.label}
                  onClick={() => {
                    setUi({ settingsPanel: p.id })
                    setFocus({ label: h.label, at: Date.now() })
                  }}
                >
                  <span className="ellipsis grow">{h.label}</span>
                  <span className="xs subtle">{p.label}</span>
                </button>
              )
            })}
          </div>
        )}
        {groups.map((g) => (
          <div key={g} style={{ marginBottom: 8 }}>
            <div className="section-title" style={{ padding: '4px 8px' }}>
              {g}
            </div>
            {visible
              .filter((p) => p.group === g)
              .map((p) => (
                <button key={p.id} className={`nav-item ${p.id === panel.id ? 'active' : ''}`} aria-current={p.id === panel.id ? 'page' : undefined} style={{ width: '100%' }} onClick={() => setUi({ settingsPanel: p.id })}>
                  {p.label}
                </button>
              ))}
          </div>
        ))}
        {q && index && !visible.length && <div className="xs subtle" style={{ padding: '4px 8px' }}>No settings match “{filter.trim()}”.</div>}
      </nav>
      <main ref={mainRef} style={{ flex: 1, minWidth: 0, overflowY: 'auto' }}>
        <div style={{ maxWidth: 920, margin: '0 auto', padding: '20px 28px 60px' }}>
          <h2 style={{ marginTop: 0 }}>{panel.label}</h2>
          <Panel />
        </div>
      </main>
    </div>
  )
}
