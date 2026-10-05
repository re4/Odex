import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { RefreshCw } from 'lucide-react'
import type { UsageStats } from '@shared/index'
import { call } from '@/lib/rpc'
import { formatTokens } from '@/components/ui'
import { Callout, Section } from '@/views/settings/ConfigSettings'

const RANGES: Array<{ id: string; label: string; days: number | null }> = [
  { id: '7', label: '7 days', days: 7 },
  { id: '30', label: '30 days', days: 30 },
  { id: '90', label: '90 days', days: 90 },
  { id: 'all', label: 'All time', days: null },
]

const SERIES = [
  { key: 'input', label: 'Input', color: 'var(--series-1)' },
  { key: 'cached', label: 'Cached input', color: 'var(--series-2)' },
  { key: 'output', label: 'Output', color: 'var(--series-3)' },
] as const

interface Day {
  date: string
  input: number
  cached: number
  output: number
  requests: number
}

function ymd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

function parseYmd(s: string): Date {
  const [y, m, d] = s.split('-').map(Number)
  return new Date(y, (m || 1) - 1, d || 1, 12)
}

function shortDate(s: string): string {
  return parseYmd(s).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

function sinceFor(days: number | null): string | null {
  if (days == null) return null
  const d = new Date()
  d.setHours(12, 0, 0, 0)
  d.setDate(d.getDate() - (days - 1))
  return ymd(d)
}

function niceMax(v: number): { max: number; step: number } {
  if (v <= 0) return { max: 1, step: 1 }
  const raw = v / 4
  const mag = 10 ** Math.floor(Math.log10(raw))
  const step = [1, 2, 2.5, 5, 10].map((k) => k * mag).find((s) => s >= raw) ?? 10 * mag
  return { max: Math.ceil(v / step) * step, step }
}

function topRoundedRect(x: number, y: number, w: number, h: number, r: number): string {
  const rr = Math.max(0, Math.min(r, w / 2, h))
  return `M${x},${y + h}V${y + rr}Q${x},${y} ${x + rr},${y}H${x + w - rr}Q${x + w},${y} ${x + w},${y + rr}V${y + h}Z`
}

function UsageChart({ days }: { days: Day[] }) {
  const ref = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(640)
  const [active, setActive] = useState<number | null>(null)
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const ro = new ResizeObserver(() => setWidth(Math.max(240, el.clientWidth - 28)))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  const H = 190
  const pad = { l: 44, r: 6, t: 8, b: 22 }
  const plotW = width - pad.l - pad.r
  const plotH = H - pad.t - pad.b
  const totals = days.map((d) => d.input + d.cached + d.output)
  const { max, step } = niceMax(Math.max(0, ...totals))
  const y = (v: number) => pad.t + plotH - (v / max) * plotH
  const slot = plotW / Math.max(1, days.length)
  const barW = Math.max(2, Math.min(24, slot * 0.72))
  const labelEvery = Math.max(1, Math.ceil(days.length / Math.max(2, Math.floor(plotW / 70))))
  const ticks: number[] = []
  for (let v = 0; v <= max + step / 2; v += step) ticks.push(v)
  const a = active != null ? days[active] : null
  // the card has 14px padding; center the ~180px tooltip on the bar
  const tipLeft = active != null ? Math.min(Math.max(14 + pad.l + active * slot + slot / 2 - 90, 0), width - 160) : 0

  return (
    <div ref={ref} className="card sx-viz" onMouseLeave={() => setActive(null)}>
      <div className="sx-legend" style={{ marginBottom: 8 }}>
        {SERIES.map((s) => (
          <span key={s.key}>
            <i className="sx-swatch" style={{ background: s.color }} />
            {s.label}
          </span>
        ))}
      </div>
      <svg height={H} viewBox={`0 0 ${width} ${H}`} role="img" aria-label={`Tokens per day, ${days.length} days`}>
        {ticks.map((t) => (
          <g key={t}>
            <line className="grid" x1={pad.l} x2={width - pad.r} y1={y(t)} y2={y(t)} />
            <text className="tick" x={pad.l - 6} y={y(t) + 3} textAnchor="end">
              {formatTokens(t)}
            </text>
          </g>
        ))}
        {days.map((d, i) => {
          const x = pad.l + i * slot + (slot - barW) / 2
          // stacked from the baseline with a 2px surface gap; the top segment gets the rounded end
          const segs: Array<{ k: number; y1: number; h: number }> = []
          let cum = 0
          ;[d.input, d.cached, d.output].forEach((v, k) => {
            if (v <= 0) return
            const y0 = y(cum) - (segs.length ? 2 : 0)
            cum += v
            const h = y0 - y(cum)
            if (h > 0.5) segs.push({ k, y1: y(cum), h })
          })
          return (
            <g key={d.date}>
              {segs.map((sg, j) =>
                j === segs.length - 1 ? <path key={sg.k} d={topRoundedRect(x, sg.y1, barW, sg.h, 4)} fill={SERIES[sg.k].color} /> : <rect key={sg.k} x={x} y={sg.y1} width={barW} height={sg.h} fill={SERIES[sg.k].color} />,
              )}
            </g>
          )
        })}
        {days.map((d, i) => {
          // every n-th day counting back from today, so today is always labeled
          const show = (days.length - 1 - i) % labelEvery === 0
          return show ? (
            <text key={d.date} className="tick" x={pad.l + i * slot + slot / 2} y={H - 6} textAnchor="middle">
              {shortDate(d.date)}
            </text>
          ) : null
        })}
        {days.map((d, i) => (
          <rect key={d.date} className={`hit ${active === i ? 'active' : ''}`} x={pad.l + i * slot} y={pad.t} width={slot} height={plotH} onMouseEnter={() => setActive(i)} />
        ))}
      </svg>
      {a && (
        <div className="sx-tooltip" style={{ left: tipLeft, top: 34 }} role="tooltip">
          <div style={{ fontWeight: 600, marginBottom: 4 }}>
            {parseYmd(a.date).toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' })}
          </div>
          {SERIES.map((s) => (
            <div key={s.key} className="row">
              <i className="sx-swatch" style={{ background: s.color }} />
              <span className="muted">{s.label}</span>
              <span className="num">{a[s.key].toLocaleString()}</span>
            </div>
          ))}
          <div className="row" style={{ marginTop: 4 }}>
            <span className="muted">Requests</span>
            <span className="num">{a.requests.toLocaleString()}</span>
          </div>
        </div>
      )}
    </div>
  )
}

export function UsageSettings() {
  const [range, setRange] = useState('30')
  const [stats, setStats] = useState<UsageStats | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [showTable, setShowTable] = useState(false)
  const [tick, setTick] = useState(0)
  const r = RANGES.find((x) => x.id === range) ?? RANGES[1]
  const since = sinceFor(r.days)

  useEffect(() => {
    let live = true
    call('usage/stats', { since })
      .then((s) => {
        if (!live) return
        setStats(s)
        setError(null)
      })
      .catch((e: Error) => live && setError(e.message))
    return () => {
      live = false
    }
  }, [since, tick])

  const days = useMemo<Day[]>(() => {
    if (!stats) return []
    const by = new Map<string, Day>()
    for (const row of stats.rows) {
      const d = by.get(row.date) ?? { date: row.date, input: 0, cached: 0, output: 0, requests: 0 }
      d.cached += row.usage.cachedInputTokens
      d.input += Math.max(0, row.usage.inputTokens - row.usage.cachedInputTokens)
      d.output += row.usage.outputTokens
      d.requests += row.requests
      by.set(row.date, d)
    }
    const today = new Date()
    today.setHours(12, 0, 0, 0)
    const first = since ?? [...by.keys()].sort()[0] ?? ymd(today)
    const out: Day[] = []
    for (let d = parseYmd(first); ymd(d) <= ymd(today) && out.length < 3660; d.setDate(d.getDate() + 1)) {
      const k = ymd(d)
      out.push(by.get(k) ?? { date: k, input: 0, cached: 0, output: 0, requests: 0 })
    }
    return out
  }, [stats, since])

  const perModel = useMemo(() => {
    if (!stats) return []
    const req = new Map<string, number>()
    for (const row of stats.rows) req.set(row.model, (req.get(row.model) ?? 0) + row.requests)
    return Object.entries(stats.byModel)
      .map(([model, u]) => ({ model, u: u!, requests: req.get(model) ?? 0 }))
      .sort((a, b) => b.u.totalTokens - a.u.totalTokens)
  }, [stats])

  const t = stats?.totals
  const requests = stats?.rows.reduce((s, x) => s + x.requests, 0) ?? 0
  const empty = !!stats && stats.rows.length === 0

  return (
    <div className="sx-panel">
      <Section
        title="Token usage"
        desc="Counted locally from the usage your endpoints report on every response. Nothing leaves this computer."
        actions={
          <>
            <div className="sx-seg" role="group" aria-label="Date range">
              {RANGES.map((x) => (
                <button key={x.id} aria-pressed={range === x.id} onClick={() => setRange(x.id)}>
                  {x.label}
                </button>
              ))}
            </div>
            <button className="icon-btn sm" aria-label="Refresh usage" title="Refresh" onClick={() => setTick((n) => n + 1)}>
              <RefreshCw size={13} />
            </button>
          </>
        }
      >
        {error && <Callout kind="danger">Could not load usage: {error}</Callout>}
        {!stats && !error && <div className="spinner" aria-label="Loading" />}
        {t && (
          <div className="sx-stats" aria-label="Usage totals">
            <div className="card sx-stat">
              <div className="sx-stat-label">Requests</div>
              <div className="sx-stat-value">{requests.toLocaleString()}</div>
            </div>
            <div className="card sx-stat">
              <div className="sx-stat-label">Input tokens</div>
              <div className="sx-stat-value" title={t.inputTokens.toLocaleString()}>
                {formatTokens(t.inputTokens)}
              </div>
            </div>
            <div className="card sx-stat">
              <div className="sx-stat-label">Cached input</div>
              <div className="sx-stat-value" title={t.cachedInputTokens.toLocaleString()}>
                {formatTokens(t.cachedInputTokens)}
                {t.inputTokens > 0 && <span className="xs muted" style={{ fontWeight: 400, marginLeft: 6 }}>{Math.round((t.cachedInputTokens / t.inputTokens) * 100)}%</span>}
              </div>
            </div>
            <div className="card sx-stat">
              <div className="sx-stat-label">Output tokens</div>
              <div className="sx-stat-value" title={t.outputTokens.toLocaleString()}>
                {formatTokens(t.outputTokens)}
              </div>
            </div>
            {t.reasoningTokens > 0 && (
              <div className="card sx-stat">
                <div className="sx-stat-label">Reasoning tokens</div>
                <div className="sx-stat-value" title={t.reasoningTokens.toLocaleString()}>
                  {formatTokens(t.reasoningTokens)}
                </div>
              </div>
            )}
            <div className="card sx-stat">
              <div className="sx-stat-label">Total tokens</div>
              <div className="sx-stat-value" title={t.totalTokens.toLocaleString()}>
                {formatTokens(t.totalTokens)}
              </div>
            </div>
          </div>
        )}
      </Section>

      {stats && (
        <Section
          title="Tokens per day"
          actions={
            <button className="btn btn-sm btn-ghost" aria-pressed={showTable} onClick={() => setShowTable((v) => !v)}>
              {showTable ? 'Show chart' : 'Show table'}
            </button>
          }
        >
          {empty ? (
            <div className="sx-list">
              <div className="sx-list-empty">No usage recorded {r.days ? `in the last ${r.days} days` : 'yet'}.</div>
            </div>
          ) : showTable ? (
            <div className="card" style={{ padding: '4px 8px', maxHeight: 360, overflowY: 'auto' }}>
              <table className="sx-table" aria-label="Tokens per day">
                <thead>
                  <tr>
                    <th>Date</th>
                    <th className="num">Requests</th>
                    <th className="num">Input</th>
                    <th className="num">Cached input</th>
                    <th className="num">Output</th>
                  </tr>
                </thead>
                <tbody>
                  {[...days].reverse().map((d) => (
                    <tr key={d.date}>
                      <td>{d.date}</td>
                      <td className="num">{d.requests.toLocaleString()}</td>
                      <td className="num">{d.input.toLocaleString()}</td>
                      <td className="num">{d.cached.toLocaleString()}</td>
                      <td className="num">{d.output.toLocaleString()}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <UsageChart days={days} />
          )}
        </Section>
      )}

      {stats && !empty && (
        <Section title="By model">
          <div className="card" style={{ padding: '4px 8px' }}>
            <table className="sx-table" aria-label="Usage by model">
              <thead>
                <tr>
                  <th>Model</th>
                  <th className="num">Requests</th>
                  <th className="num">Input</th>
                  <th className="num">Cached</th>
                  <th className="num">Output</th>
                  <th className="num">Total</th>
                </tr>
              </thead>
              <tbody>
                {perModel.map((m) => (
                  <tr key={m.model}>
                    <td className="mono small ellipsis" style={{ maxWidth: 280 }} title={m.model}>
                      {m.model}
                    </td>
                    <td className="num">{m.requests.toLocaleString()}</td>
                    <td className="num">{formatTokens(m.u.inputTokens)}</td>
                    <td className="num">{formatTokens(m.u.cachedInputTokens)}</td>
                    <td className="num">{formatTokens(m.u.outputTokens)}</td>
                    <td className="num">{formatTokens(m.u.totalTokens)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Section>
      )}
    </div>
  )
}
