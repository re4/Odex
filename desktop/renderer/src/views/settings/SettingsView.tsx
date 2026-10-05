import { useState } from 'react'
import { ArrowLeft, Search } from 'lucide-react'
import { useApp } from '@/store/app'
import { SETTINGS_PANELS } from '@/views/settings/registry'

export function SettingsView() {
  const panelId = useApp((s) => s.ui.settingsPanel)
  const setUi = useApp((s) => s.setUi)
  const selected = useApp((s) => s.selectedThreadId)
  const [filter, setFilter] = useState('')
  const q = filter.toLowerCase()
  const visible = SETTINGS_PANELS.filter((p) => !q || p.label.toLowerCase().includes(q) || (p.keywords ?? '').includes(q))
  const panel = SETTINGS_PANELS.find((p) => p.id === panelId) ?? SETTINGS_PANELS[0]
  const groups = [...new Set(visible.map((p) => p.group))]
  const Panel = panel.component
  return (
    <div className="row" style={{ flex: 1, minHeight: 0, alignItems: 'stretch', gap: 0 }}>
      <nav style={{ width: 220, flex: 'none', borderRight: '1px solid var(--border)', padding: 8, overflowY: 'auto' }} aria-label="Settings sections">
        <button className="nav-item" style={{ width: '100%', marginBottom: 6 }} onClick={() => setUi({ view: selected ? 'thread' : 'home' })}>
          <ArrowLeft size={14} /> Back to app
        </button>
        <div className="row" style={{ position: 'relative', marginBottom: 8 }}>
          <Search size={13} style={{ position: 'absolute', left: 8, color: 'var(--fg-subtle)' }} />
          <input className="input" style={{ paddingLeft: 26, height: 28 }} placeholder="Search settings" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Search settings" />
        </div>
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
      </nav>
      <main style={{ flex: 1, minWidth: 0, overflowY: 'auto' }}>
        <div style={{ maxWidth: 920, margin: '0 auto', padding: '20px 28px 60px' }}>
          <h2 style={{ marginTop: 0 }}>{panel.label}</h2>
          <Panel />
        </div>
      </main>
    </div>
  )
}
