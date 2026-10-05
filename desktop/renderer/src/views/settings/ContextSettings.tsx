import { RotateCcw } from 'lucide-react'
import type { ContextToml } from '@shared/index'
import { useApp } from '@/store/app'
import { confirmDialog } from '@/lib/actions'
import { formatTokens } from '@/components/ui'
import { Row } from '@/views/settings/GeneralSettings'
import { Callout, NumberField, Section, useEngineConfig } from '@/views/settings/ConfigSettings'

type NumKey = Exclude<keyof ContextToml, 'compactor_model'>

interface FieldDef {
  key: NumKey
  label: string
  hint: string
  def: number
  /** Display units (percent for ratios). */
  min: number
  max: number
  step?: number
  ratio?: boolean
  unit?: string
}

const F: Record<NumKey, FieldDef> = {
  prune_at: { key: 'prune_at', label: 'Prune at', hint: 'Old tool outputs and images become short stubs when the prompt reaches this share of the budget.', def: 0.7, min: 20, max: 98, ratio: true },
  compact_at: { key: 'compact_at', label: 'Compact at', hint: 'Older history is summarized by the compactor model at this share of the budget (never below Prune at).', def: 0.85, min: 20, max: 99, ratio: true },
  target_after_compact: { key: 'target_after_compact', label: 'Target after compaction', hint: 'Compaction aims to bring the prompt down to this share of the window.', def: 0.5, min: 20, max: 80, ratio: true },
  keep_recent_ratio: { key: 'keep_recent_ratio', label: 'Keep recent', hint: 'Share of the window kept word for word (the newest turns) when compacting.', def: 0.2, min: 2, max: 45, ratio: true },
  reserve_output_ratio: { key: 'reserve_output_ratio', label: 'Output reserve', hint: "Share of the window held back for the reply, capped by the model's max output tokens.", def: 0.25, min: 5, max: 50, ratio: true },
  margin_ratio: { key: 'margin_ratio', label: 'Safety margin', hint: 'Headroom for token-count estimation errors (at least 16 tokens).', def: 0.03, min: 0, max: 20, step: 0.5, ratio: true },
  mcp_tool_budget_ratio: { key: 'mcp_tool_budget_ratio', label: 'MCP tool budget', hint: 'When MCP tool schemas would take more than this share of the window, they load on demand instead.', def: 0.15, min: 1, max: 90, ratio: true },
  tool_output_max_tokens: { key: 'tool_output_max_tokens', label: 'Tool output cap', hint: 'Max tokens of one tool output kept inline; the full output is saved and can be re-read. 0 = auto (scaled to the window).', def: 0, min: 0, max: 200_000, step: 100, unit: 'tokens' },
  stub_after_turns: { key: 'stub_after_turns', label: 'Stub tool outputs after', hint: 'When pruning, tool outputs older than this many turns become stubs.', def: 3, min: 0, max: 100, unit: 'turns' },
  max_images: { key: 'max_images', label: 'Images kept', hint: 'The newest images stay as images; older ones become text stubs.', def: 2, min: 0, max: 50, unit: 'images' },
  notes_max_bytes: { key: 'notes_max_bytes', label: 'NOTES.md cap', hint: 'Max bytes of the project’s .odex/NOTES.md pinned into compaction summaries.', def: 8192, min: 0, max: 1_000_000, step: 256, unit: 'bytes' },
  memories_max_tokens: { key: 'memories_max_tokens', label: 'Memories budget', hint: 'Max tokens of approved memories added to the prompt.', def: 1500, min: 0, max: 50_000, step: 50, unit: 'tokens' },
}

const GROUPS: Array<{ title: string; desc?: string; keys: NumKey[] }> = [
  { title: 'Thresholds', desc: 'Two tiers keep long threads inside the window: pruning first (cheap, no model call), then compaction (a summary by the compactor model).', keys: ['prune_at', 'compact_at', 'target_after_compact', 'keep_recent_ratio'] },
  { title: 'Window budget', keys: ['reserve_output_ratio', 'margin_ratio'] },
  { title: 'Tool outputs and images', keys: ['tool_output_max_tokens', 'stub_after_turns', 'max_images', 'mcp_tool_budget_ratio'] },
  { title: 'Pinned content', keys: ['notes_max_bytes', 'memories_max_tokens'] },
]

function BudgetDiagram({ window: win, maxOutput, ctx, modelName }: { window: number; maxOutput: number; ctx: { prune_at: number; compact_at: number; reserve_output_ratio: number; margin_ratio: number }; modelName: string | null }) {
  const reserve = Math.max(Math.min(maxOutput || Infinity, Math.floor(win * ctx.reserve_output_ratio)), Math.min(64, Math.floor(win / 4)))
  const margin = Math.max(Math.ceil(win * ctx.margin_ratio), 16)
  const budget = Math.max(0, win - reserve - margin)
  const pct = (n: number) => `${(n / win) * 100}%`
  const prune = budget * ctx.prune_at
  const compact = budget * Math.max(ctx.prune_at, ctx.compact_at)
  return (
    <div className="card sx-budget" aria-label="Context window budget">
      <div className="row small" style={{ gap: 6 }}>
        <b>{win.toLocaleString()} token window</b>
        <span className="muted">{modelName ? `· ${modelName}` : '· no main model yet, 32k example'}</span>
      </div>
      <div className="sx-budget-bar">
        <div className="sx-budget-seg budget" style={{ width: pct(budget) }} title={`Prompt budget: ${budget.toLocaleString()} tokens`} />
        <div className="sx-budget-seg reserve" style={{ width: pct(reserve) }} title={`Output reserve: ${reserve.toLocaleString()} tokens`} />
        <div className="sx-budget-seg margin" style={{ width: pct(margin) }} title={`Safety margin: ${margin.toLocaleString()} tokens`} />
        <div className="sx-marker" style={{ left: pct(prune) }} aria-hidden>
          <span className="sx-marker-label">prune</span>
        </div>
        <div className="sx-marker compact" style={{ left: pct(compact) }} aria-hidden>
          <span className="sx-marker-label">compact</span>
        </div>
      </div>
      <div className="sx-legend">
        <span>
          <i className="sx-swatch" style={{ background: 'color-mix(in srgb, var(--accent) 22%, var(--bg-elev))' }} />
          Prompt budget {formatTokens(budget)}
        </span>
        <span>
          <i className="sx-swatch" style={{ background: 'color-mix(in srgb, var(--accent) 55%, var(--bg-elev))' }} />
          Output reserve {formatTokens(reserve)}
        </span>
        <span>
          <i className="sx-swatch" style={{ background: 'var(--border-strong)' }} />
          Margin {formatTokens(margin)}
        </span>
        <span>
          <i className="sx-swatch line" style={{ background: 'var(--fg)' }} />
          Prune at {formatTokens(Math.round(prune))}
        </span>
        <span>
          <i className="sx-swatch line" style={{ background: 'var(--warning)' }} />
          Compact at {formatTokens(Math.round(compact))}
        </span>
      </div>
    </div>
  )
}

export function ContextSettings() {
  const { cfg, write } = useEngineConfig()
  const models = useApp((s) => s.models)
  const roles = useApp((s) => s.roles)
  if (!cfg) return <div className="spinner" aria-label="Loading" />

  const user = cfg.user.context ?? {}
  const eff = cfg.effective.context ?? {}
  const value = (k: NumKey) => (eff[k] ?? undefined) as number | undefined
  const set = (k: NumKey, v: number | null) => void write([{ keyPath: `context.${k}`, value: v }])
  const main = models.find((m) => m.key === roles.main) ?? null
  const profileCtx = cfg.activeProfile ? cfg.user.profiles[cfg.activeProfile]?.context : null
  const anyCustom = Object.values(user).some((v) => v != null)
  const ctxNow = {
    prune_at: value('prune_at') ?? F.prune_at.def,
    compact_at: value('compact_at') ?? F.compact_at.def,
    reserve_output_ratio: value('reserve_output_ratio') ?? F.reserve_output_ratio.def,
    margin_ratio: value('margin_ratio') ?? F.margin_ratio.def,
  }

  const field = (f: FieldDef) => {
    const custom = user[f.key] != null
    return (
      <Row
        key={f.key}
        label={f.label}
        hint={
          <>
            {f.hint} Default {f.ratio ? `${Math.round(f.def * 1000) / 10}%` : `${f.def.toLocaleString()}${f.unit ? ` ${f.unit}` : ''}`}.
          </>
        }
      >
        <div className="row" style={{ gap: 4 }}>
          <NumberField label={f.label} value={value(f.key)} def={f.def} min={f.min} max={f.max} step={f.step} scale={f.ratio ? 100 : 1} unit={f.ratio ? '%' : f.unit} slider={f.ratio} onCommit={(v) => set(f.key, v)} />
          <button className="icon-btn sm" aria-label={`Reset ${f.label}`} title="Reset to default" disabled={!custom} style={{ visibility: custom ? 'visible' : 'hidden' }} onClick={() => set(f.key, null)}>
            <RotateCcw size={12} />
          </button>
        </div>
      </Row>
    )
  }

  return (
    <div className="sx-panel">
      <Section
        title="Context window"
        desc="Odex measures every request and keeps the prompt inside the model's window. These settings apply to all threads; the defaults suit most models."
        actions={
          <button
            className="btn btn-sm"
            disabled={!anyCustom}
            onClick={async () => {
              if (await confirmDialog('Reset context settings', 'Remove every [context] setting from config.toml and use the defaults?', 'Reset')) await write([{ keyPath: 'context', value: null }])
            }}
          >
            <RotateCcw size={13} /> Reset to defaults
          </button>
        }
      >
        {profileCtx && (
          <div style={{ marginBottom: 8 }}>
            <Callout kind="warning">
              The active profile <b>{cfg.activeProfile}</b> overrides some of these values; the numbers below include its overrides.
            </Callout>
          </div>
        )}
        <BudgetDiagram window={main?.contextWindow || 32768} maxOutput={main?.maxOutputTokens ?? 0} ctx={ctxNow} modelName={main?.displayName ?? null} />
      </Section>

      {GROUPS.map((g) => (
        <Section key={g.title} title={g.title} desc={g.desc}>
          {g.keys.map((k) => field(F[k]))}
        </Section>
      ))}

      <Section title="Compaction model">
        <Row label="Model used to summarize history" hint="Defaults to the Compactor role from Models & Endpoints (which falls back to the main model).">
          <select className="select" style={{ minWidth: 240, maxWidth: 340 }} value={user.compactor_model ?? ''} aria-label="Compaction model" onChange={(e) => void write([{ keyPath: 'context.compactor_model', value: e.target.value || null }])}>
            <option value="">Compactor role</option>
            {models.map((m) => (
              <option key={m.key} value={m.key}>
                {m.displayName}
              </option>
            ))}
            {user.compactor_model && !models.some((m) => m.key === user.compactor_model) && <option value={user.compactor_model}>{user.compactor_model} (not found)</option>}
          </select>
        </Row>
      </Section>
    </div>
  )
}
