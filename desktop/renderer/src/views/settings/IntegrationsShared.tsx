import { useMemo, type ReactNode } from 'react'
import { Plus, X } from 'lucide-react'
import type { Project } from '@shared/index'
import { useApp } from '@/store/app'
import { basename } from '@/components/ui'
import '@/styles/integrations.css'

/** Primary folder of a project. */
export function projectRoot(p: Project): string {
  return p.folders[p.primary] ?? p.folders[0] ?? ''
}

/** Trusted projects: only these load `.odex/` skills and hooks. */
export function useTrustedProjects(): Project[] {
  const projects = useApp((s) => s.projects)
  return useMemo(() => projects.filter((p) => p.trusted && p.folders.length > 0), [projects])
}

/** Same path, ignoring case and separators (Windows-friendly). */
export function samePath(a: string, b: string): boolean {
  const n = (s: string) => s.replace(/[\\/]+$/, '').replace(/\\/g, '/').toLowerCase()
  return n(a) === n(b)
}

/** Label for a `source` string such as `user`, `project:<path>`, `plugin:<id>`. */
export function sourceLabel(source: string, projects: Project[]): string {
  if (source === 'user') return 'User'
  if (source.startsWith('project:')) {
    const path = source.slice('project:'.length)
    const p = projects.find((x) => x.folders.some((f) => samePath(f, path)))
    return `Project · ${p?.name ?? basename(path)}`
  }
  if (source.startsWith('plugin:')) return `Plugin · ${source.slice('plugin:'.length)}`
  return source
}

export function SectionHead({ title, children }: { title: ReactNode; children?: ReactNode }) {
  return (
    <div className="int-section-head">
      <h3>{title}</h3>
      {children}
    </div>
  )
}

/** Editable list of strings (arguments, tool names). */
export function ListEditor(props: { label: string; values: string[]; onChange: (v: string[]) => void; placeholder?: string; addLabel?: string }) {
  const { label, values, onChange } = props
  return (
    <div className="int-list-editor" role="group" aria-label={label}>
      {values.map((v, i) => (
        <div key={i} className="row" style={{ gap: 6 }}>
          <input className="input mono" value={v} placeholder={props.placeholder} aria-label={`${label} ${i + 1}`} onChange={(e) => onChange(values.map((x, k) => (k === i ? e.target.value : x)))} />
          <button type="button" className="icon-btn sm" aria-label={`Remove ${label.toLowerCase()} ${i + 1}`} onClick={() => onChange(values.filter((_, k) => k !== i))}>
            <X size={13} />
          </button>
        </div>
      ))}
      <button type="button" className="btn btn-sm btn-ghost int-add" onClick={() => onChange([...values, ''])}>
        <Plus size={13} /> {props.addLabel ?? 'Add'}
      </button>
    </div>
  )
}

export type Pairs = Array<[string, string]>

export function recordToPairs(r: { [k: string]: string | undefined } | null | undefined): Pairs {
  return Object.entries(r ?? {}).map(([k, v]) => [k, v ?? ''])
}

export function pairsToRecord(p: Pairs): { [k: string]: string } {
  const out: { [k: string]: string } = {}
  for (const [k, v] of p) if (k.trim()) out[k.trim()] = v
  return out
}

/** Editable key/value pairs (environment variables, headers). */
export function KeyValueEditor(props: { label: string; pairs: Pairs; onChange: (p: Pairs) => void; keyPlaceholder?: string; valuePlaceholder?: string; addLabel?: string; secret?: boolean }) {
  const { label, pairs, onChange } = props
  const set = (i: number, k: 0 | 1, v: string) => onChange(pairs.map((p, j) => (j === i ? ((k === 0 ? [v, p[1]] : [p[0], v]) as [string, string]) : p)))
  return (
    <div className="int-list-editor" role="group" aria-label={label}>
      {pairs.map(([k, v], i) => (
        <div key={i} className="int-kv">
          <input className="input mono" value={k} placeholder={props.keyPlaceholder ?? 'KEY'} aria-label={`${label} name ${i + 1}`} onChange={(e) => set(i, 0, e.target.value)} />
          <input className="input mono" value={v} placeholder={props.valuePlaceholder ?? 'value'} aria-label={`${label} value ${i + 1}`} onChange={(e) => set(i, 1, e.target.value)} type={props.secret ? 'password' : 'text'} autoComplete="off" />
          <button type="button" className="icon-btn sm" aria-label={`Remove ${label.toLowerCase()} ${i + 1}`} onClick={() => onChange(pairs.filter((_, j) => j !== i))}>
            <X size={13} />
          </button>
        </div>
      ))}
      <button type="button" className="btn btn-sm btn-ghost int-add" onClick={() => onChange([...pairs, ['', '']])}>
        <Plus size={13} /> {props.addLabel ?? 'Add'}
      </button>
    </div>
  )
}

/** Two-or-more option segmented control. */
export function Segmented<T extends string>(props: { label: string; value: T; options: Array<[T, string]>; onChange: (v: T) => void }) {
  return (
    <div className="int-seg" role="radiogroup" aria-label={props.label}>
      {props.options.map(([v, l]) => (
        <button key={v} type="button" role="radio" aria-checked={props.value === v} className="int-seg-btn" onClick={() => props.onChange(v)}>
          {l}
        </button>
      ))}
    </div>
  )
}

/** Comma/newline separated list → trimmed non-empty entries. */
export function parseList(s: string): string[] {
  return s
    .split(/[,\n]/)
    .map((x) => x.trim())
    .filter(Boolean)
}
