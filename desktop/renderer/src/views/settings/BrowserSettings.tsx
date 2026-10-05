import { useEffect, useMemo, useState } from 'react'
import { ExternalLink, Plus, X } from 'lucide-react'
import type { BrowserToml, ConfigReadResponse, JsonValue } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Toggle, relativeTime } from '@/components/ui'
import { Row } from '@/views/settings/GeneralSettings'
import { dedupeHistory, type HistoryEntry } from '@/panels/BrowserPanel'
import '@/styles/browser.css'

const HOUR = 3_600_000
const DAY = 24 * HOUR

// The engine rewrites config.toml per request; overlapping writes can collide,
// so writes from these pages go one at a time.
let writeQueue: Promise<unknown> = Promise.resolve()

/** Read the effective config and write single keys back, keeping the latest view. */
export function useConfig(): [ConfigReadResponse | null, (keyPath: string, value: JsonValue) => Promise<void>, string | null] {
  const [cfg, setCfg] = useState<ConfigReadResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    call('config/read', {})
      .then(setCfg)
      .catch((e: Error) => setError(e.message))
  }, [])
  const write = (keyPath: string, value: JsonValue): Promise<void> => {
    const run = writeQueue.then(async () => {
      try {
        setCfg(await call('config/write', { edits: [{ keyPath, value }] }))
      } catch (e) {
        toast(`Could not save ${keyPath}: ${(e as Error).message}`, 'error')
      }
    })
    writeQueue = run
    return run
  }
  return [cfg, write, error]
}

/** Editable list of host patterns or app names, shown as removable chips. */
export function ListEditor(props: { items: string[]; onChange: (items: string[]) => void; placeholder: string; label: string; danger?: boolean; empty: string; normalize?: (v: string) => string }) {
  const [v, setV] = useState('')
  const add = () => {
    const parts = v
      .split(/[\s,]+/)
      .map((x) => (props.normalize ? props.normalize(x) : x.trim()))
      .filter(Boolean)
    if (!parts.length) return
    const next = [...props.items]
    for (const p of parts) if (!next.includes(p)) next.push(p)
    props.onChange(next)
    setV('')
  }
  return (
    <div>
      <div className="site-list" aria-label={props.label}>
        {props.items.length === 0 && <span className="xs subtle">{props.empty}</span>}
        {props.items.map((s) => (
          <span key={s} className={`site-chip ${props.danger ? 'blocked' : ''}`}>
            {s}
            <button className="icon-btn sm" style={{ width: 18, height: 18 }} aria-label={`Remove ${s}`} onClick={() => props.onChange(props.items.filter((x) => x !== s))}>
              <X size={11} />
            </button>
          </span>
        ))}
      </div>
      <form
        className="row"
        style={{ gap: 6 }}
        onSubmit={(e) => {
          e.preventDefault()
          add()
        }}
      >
        <input className="input mono" style={{ maxWidth: 320 }} value={v} onChange={(e) => setV(e.target.value)} placeholder={props.placeholder} aria-label={`Add to ${props.label}`} />
        <button className="btn btn-sm" type="submit" disabled={!v.trim()}>
          <Plus size={13} /> Add
        </button>
      </form>
    </div>
  )
}

/** Host pattern: drop paths and whitespace, keep an explicit scheme or port. */
function sitePattern(raw: string): string {
  const t = raw.trim().toLowerCase()
  if (!t) return ''
  const m = /^([a-z*]+:\/\/)?([^/?#]+)/.exec(t)
  return m ? `${m[1] ?? ''}${m[2]}` : t
}

function TextSetting(props: { value: string; onSave: (v: string) => void; label: string; placeholder?: string; width?: number; mono?: boolean }) {
  const [v, setV] = useState(props.value)
  useEffect(() => setV(props.value), [props.value])
  const save = () => {
    if (v !== props.value) props.onSave(v)
  }
  return (
    <input
      className={`input ${props.mono ? 'mono' : ''}`}
      style={{ width: props.width ?? 280 }}
      value={v}
      placeholder={props.placeholder}
      aria-label={props.label}
      onChange={(e) => setV(e.target.value)}
      onBlur={save}
      onKeyDown={(e) => {
        if (e.key === 'Enter') save()
        if (e.key === 'Escape') setV(props.value)
      }}
    />
  )
}

function openInBrowser(url: string): void {
  const s = useApp.getState()
  s.setUi({ view: s.selectedThreadId ? 'thread' : 'home', sidePanelOpen: true, sidePanelTab: 'browser' })
  void window.odex.browser.newTab(url)
}

export function BrowserSettings() {
  const [cfg, write, error] = useConfig()
  const home = useApp((s) => s.settings?.browserHome ?? 'about:blank')
  const [history, setHistory] = useState<HistoryEntry[] | null>(null)
  const [q, setQ] = useState('')
  const reloadHistory = () =>
    void window.odex.browser
      .history()
      .then((h: HistoryEntry[]) => setHistory(h))
      .catch(() => setHistory([]))
  useEffect(reloadHistory, [])
  const recent = useMemo(() => dedupeHistory(history ?? [], q, 100), [history, q])

  // empty lists are omitted from the config JSON
  const raw: Partial<BrowserToml> = cfg?.effective.browser ?? {}
  const br: BrowserToml = { ...raw, allowed_sites: raw.allowed_sites ?? [], blocked_sites: raw.blocked_sites ?? [] }
  const enabled = br.enabled ?? true
  const developer = br.developer_mode ?? false

  const clearHistory = async (ms?: number) => {
    if (!ms && !(await A.confirmDialog('Clear all browsing history', 'Remove every page from the in-app browser history? This cannot be undone.', 'Clear history', true))) return
    await window.odex.browser.clearHistory(ms ? Date.now() - ms : undefined)
    reloadHistory()
    toast(ms ? `Cleared history from the last ${ms === HOUR ? 'hour' : 'day'}` : 'Browsing history cleared', 'success')
  }

  const clearData = async () => {
    if (!(await A.confirmDialog('Clear cookies and site data', 'Sign out of every site in the in-app browser and delete its cookies, storage and cache? Your own browser is not affected.', 'Clear data', true))) return
    try {
      await window.odex.browser.clearData()
      toast('Cookies and site data cleared', 'success')
    } catch (e) {
      toast(`Could not clear site data: ${(e as Error).message}`, 'error')
    }
  }

  return (
    <div>
      {error && (
        <div className="banner danger" role="alert" style={{ marginBottom: 12, borderRadius: 'var(--radius)' }}>
          Could not read config: {error}
        </div>
      )}
      <h3 className="section-title">Browser use</h3>
      <Row label="Let the agent use the browser" hint="Adds tools to open pages, read them (accessibility snapshot), click, type, scroll, take screenshots and read console and network logs.">
        <Toggle checked={enabled} disabled={!cfg} onChange={(v) => void write('browser.enabled', v)} label="Let the agent use the browser" />
      </Row>
      <Row
        label="Developer mode"
        hint={
          <>
            Also allows <code>browser_eval</code> (run JavaScript in pages) and raw Chrome DevTools Protocol commands. Only for sites you trust.
          </>
        }
      >
        <Toggle checked={developer} disabled={!cfg} onChange={(v) => void write('browser.developer_mode', v)} label="Developer mode" />
      </Row>
      <Row label="Headless CDP endpoint" hint="Used when no desktop window is attached (odex exec, automations). Leave empty to launch a private headless browser.">
        <TextSetting value={br.cdp_url ?? ''} onSave={(v) => void write('browser.cdp_url', v.trim() || null)} label="Headless CDP endpoint" placeholder="http://127.0.0.1:9222" mono />
      </Row>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Site permissions
      </h3>
      <p className="small muted" style={{ margin: '6px 0 4px' }}>
        Local addresses (<code>localhost</code>, <code>127.0.0.1</code>, <code>*.localhost</code>) and files are always allowed. The agent asks before visiting any other site unless it is listed here; blocked sites win. Patterns: <code>example.com</code>, <code>*.example.com</code>, <code>localhost:3000</code>, <code>https://docs.rs</code>.
      </p>
      <div style={{ padding: '10px 0', borderBottom: '1px solid var(--border)' }}>
        <div>Allowed sites</div>
        <div className="xs subtle">The agent may open these without asking.</div>
        <ListEditor label="Allowed sites" items={br.allowed_sites} onChange={(v) => void write('browser.allowed_sites', v)} placeholder="docs.rs, *.mdn.dev" empty="No sites yet." normalize={sitePattern} />
      </div>
      <div style={{ padding: '10px 0', borderBottom: '1px solid var(--border)' }}>
        <div>Blocked sites</div>
        <div className="xs subtle">The agent can never open these, even if you approve.</div>
        <ListEditor label="Blocked sites" danger items={br.blocked_sites} onChange={(v) => void write('browser.blocked_sites', v)} placeholder="bank.example.com" empty="Nothing blocked." normalize={sitePattern} />
      </div>
      <Row label="Downloads" hint="Every download from the in-app browser asks for approval and a save location first.">
        <span className="badge success">Always ask</span>
      </Row>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Browsing
      </h3>
      <Row label="New tab page" hint="Opened by New tab (Ctrl+T). Use about:blank for the built-in start page with local servers and recent pages.">
        <TextSetting value={home} onSave={(v) => void useApp.getState().setSettings({ browserHome: v.trim() || 'about:blank' })} label="New tab page" placeholder="about:blank" mono />
      </Row>
      <Row label="Cookies and site data" hint="The in-app browser has its own profile, separate from Odex and from your browser.">
        <button className="btn btn-sm" onClick={() => void clearData()}>
          Clear cookies and site data…
        </button>
      </Row>
      <Row label="Clear browsing history" hint={history ? `${history.length} visit${history.length === 1 ? '' : 's'} stored on this computer` : ' '}>
        <div className="row" style={{ gap: 6 }}>
          <button className="btn btn-sm" onClick={() => void clearHistory(HOUR)}>
            Last hour
          </button>
          <button className="btn btn-sm" onClick={() => void clearHistory(DAY)}>
            Last day
          </button>
          <button className="btn btn-sm btn-danger" onClick={() => void clearHistory()}>
            All time
          </button>
        </div>
      </Row>

      <div style={{ marginTop: 14 }}>
        <input className="input" placeholder="Search history" aria-label="Search browsing history" value={q} onChange={(e) => setQ(e.target.value)} style={{ maxWidth: 360 }} />
        <div className="col" style={{ marginTop: 8, gap: 0 }}>
          {history !== null && recent.length === 0 && <div className="small subtle">{q ? 'No matching pages.' : 'No history yet.'}</div>}
          {recent.map((e) => (
            <div key={e.url} className="row" style={{ padding: '5px 0', borderBottom: '1px solid var(--border)', gap: 10 }}>
              <div className="grow" style={{ minWidth: 0 }}>
                <div className="ellipsis small">{e.title || e.url}</div>
                <div className="ellipsis xs subtle mono">{e.url}</div>
              </div>
              <span className="xs subtle" style={{ flex: 'none' }}>
                {relativeTime(e.at)}
              </span>
              <button className="icon-btn sm" aria-label={`Open ${e.url} in the browser panel`} title="Open in the browser panel" onClick={() => openInBrowser(e.url)}>
                <ExternalLink size={12} />
              </button>
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
