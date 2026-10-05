import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { CircleAlert, CircleCheck, ExternalLink, FolderOpen, RefreshCw, RotateCcw, Save, TriangleAlert } from 'lucide-react'
import type { ConfigEdit, ConfigReadResponse, ProfileToml } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { Row } from '@/views/settings/GeneralSettings'
import '@/styles/settings-extra.css'

// ------------------------------------------------------------------ helpers
// Shared by the settings panels in this folder (Personalization, Permissions,
// Context, Memories, ...).

/** Reads the engine config once and writes edits through `config/write`. */
export function useEngineConfig(): {
  cfg: ConfigReadResponse | null
  setCfg: (c: ConfigReadResponse) => void
  error: string | null
  reload: () => Promise<void>
  write: (edits: ConfigEdit[]) => Promise<boolean>
} {
  const [cfg, setCfg] = useState<ConfigReadResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const reload = useCallback(async () => {
    try {
      setCfg(await call('config/read', {}))
      setError(null)
    } catch (e) {
      setError((e as Error).message)
    }
  }, [])
  useEffect(() => {
    void reload()
  }, [reload])
  const write = useCallback(async (edits: ConfigEdit[]) => {
    try {
      setCfg(await call('config/write', { edits }))
      return true
    } catch (e) {
      toast(`Could not save settings: ${(e as Error).message}`, 'error')
      return false
    }
  }, [])
  return { cfg, setCfg, error, reload, write }
}

/** Join path segments using the separator style of `dir`. */
export function joinPath(dir: string, ...parts: string[]): string {
  const sep = dir.includes('\\') ? '\\' : '/'
  return [dir.replace(/[\\/]+$/, ''), ...parts].join(sep)
}

/** The Odex home folder (`~/.odex`), from the engine's initialize response. */
export function useOdexHome(): string {
  return useApp((s) => s.engine.init?.odexHome) ?? ''
}

/** Text of a file, or null when it does not exist. Throws for binary or huge files. */
export async function readText(p: string): Promise<string | null> {
  if (!(await window.odex.fs.exists(p))) return null
  const r = (await window.odex.fs.read(p)) as { kind: string; text?: string }
  if (r.kind !== 'text') throw new Error(`${p} is not a text file (${r.kind})`)
  return r.text ?? ''
}

/** Debounced autosave for text fields; flushes on blur and unmount. */
export function useAutosave(save: (v: string) => Promise<unknown>, delay = 700): { schedule: (v: string) => void; flush: () => Promise<void>; state: 'idle' | 'pending' | 'saving' | 'saved' } {
  const timer = useRef<number | null>(null)
  const pending = useRef<string | null>(null)
  const saveRef = useRef(save)
  const [state, setState] = useState<'idle' | 'pending' | 'saving' | 'saved'>('idle')
  useEffect(() => {
    saveRef.current = save
  }, [save])
  const flush = useCallback(async () => {
    if (timer.current != null) {
      window.clearTimeout(timer.current)
      timer.current = null
    }
    const v = pending.current
    if (v == null) return
    pending.current = null
    setState('saving')
    await saveRef.current(v)
    setState('saved')
  }, [])
  const schedule = useCallback(
    (v: string) => {
      pending.current = v
      setState('pending')
      if (timer.current != null) window.clearTimeout(timer.current)
      timer.current = window.setTimeout(() => void flush(), delay)
    },
    [delay, flush],
  )
  useEffect(() => () => void flush(), [flush])
  return { schedule, flush, state }
}

export function SaveState({ state }: { state: 'idle' | 'pending' | 'saving' | 'saved' | 'dirty' }) {
  if (state === 'idle') return null
  const text = state === 'pending' || state === 'dirty' ? 'Unsaved changes' : state === 'saving' ? 'Saving…' : 'Saved'
  return (
    <span className={`sx-saved ${state === 'pending' || state === 'dirty' ? 'dirty' : ''}`} role="status">
      {text}
    </span>
  )
}

export function Section({ title, desc, actions, children }: { title?: string; desc?: ReactNode; actions?: ReactNode; children?: ReactNode }) {
  return (
    <section className="sx-section" aria-label={title}>
      {(title || actions) && (
        <div className="sx-head">
          {title ? <h3 className="section-title">{title}</h3> : <span className="spacer" />}
          {actions}
        </div>
      )}
      {desc && <p className="sx-desc">{desc}</p>}
      {children}
    </section>
  )
}

export function Callout({ kind, children }: { kind?: 'warning' | 'danger' | 'success'; children: ReactNode }) {
  const Icon = kind === 'danger' ? CircleAlert : kind === 'success' ? CircleCheck : TriangleAlert
  const color = kind === 'danger' ? 'var(--danger)' : kind === 'success' ? 'var(--success)' : kind === 'warning' ? 'var(--warning)' : 'var(--fg-muted)'
  return (
    <div className={`sx-callout ${kind ?? ''}`}>
      <Icon size={14} color={color} aria-hidden />
      <div className="grow" style={{ minWidth: 0 }}>
        {children}
      </div>
    </div>
  )
}

/**
 * Numeric config field (optionally with a slider). `scale` converts the stored
 * value to the displayed one (100 for ratios shown as percent). Commits on
 * blur, Enter and slider release; `null` means "use the default".
 */
export function NumberField(props: {
  label: string
  value: number | null | undefined
  def: number
  min: number
  max: number
  step?: number
  scale?: number
  unit?: string
  slider?: boolean
  onCommit: (v: number | null) => void
}) {
  const scale = props.scale ?? 1
  const step = props.step ?? 1
  const [draft, setDraft] = useState<string | null>(null)
  const shown = draft ?? String(round((props.value ?? props.def) * scale))
  const commit = () => {
    if (draft == null) return
    setDraft(null)
    const n = Number(draft)
    if (draft.trim() === '' || !Number.isFinite(n)) return
    const clamped = Math.min(props.max, Math.max(props.min, n))
    const raw = round(clamped / scale)
    if (raw === (props.value ?? props.def)) return
    props.onCommit(raw)
  }
  return (
    <div className="sx-field">
      {props.slider && (
        <input
          type="range"
          className="sx-range"
          min={props.min}
          max={props.max}
          step={step}
          value={Number(shown) || 0}
          onChange={(e) => setDraft(e.target.value)}
          onPointerUp={commit}
          onKeyUp={commit}
          onBlur={commit}
          aria-label={`${props.label} slider`}
        />
      )}
      <input
        className="input sx-num"
        type="number"
        min={props.min}
        max={props.max}
        step={step}
        value={shown}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') commit()
          if (e.key === 'Escape') setDraft(null)
        }}
        aria-label={props.label}
      />
      <span className="sx-unit">{props.unit ?? ''}</span>
    </div>
  )
}

function round(n: number): number {
  return Number(n.toFixed(6))
}

// -------------------------------------------------------------- the panel

type Result = { kind: 'success' | 'danger' | 'warning'; message: string }

function profileSummary(p: ProfileToml | undefined): string {
  if (!p) return ''
  const keys: string[] = []
  for (const [k, v] of Object.entries(p)) {
    if (v == null) continue
    if (typeof v === 'object' && Object.keys(v as object).length === 0) continue
    keys.push(k)
  }
  return keys.length ? keys.join(', ') : 'no overrides'
}

export function ConfigSettings() {
  const { cfg, setCfg, error, write } = useEngineConfig()
  const [text, setText] = useState<string | null>(null)
  const [disk, setDisk] = useState('')
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<Result | null>(null)
  const path = cfg?.path ?? ''

  const loadFile = useCallback(async (p: string) => {
    try {
      const t = (await readText(p)) ?? ''
      setText(t)
      setDisk(t)
    } catch (e) {
      setResult({ kind: 'danger', message: (e as Error).message })
    }
  }, [])

  useEffect(() => {
    if (path) void loadFile(path)
  }, [path, loadFile])

  const dirty = text != null && text !== disk

  const save = async () => {
    if (!path || text == null) return
    setBusy(true)
    setResult(null)
    try {
      const previous = (await readText(path)) ?? ''
      await window.odex.fs.write(`${path}.bak`, previous)
      await window.odex.fs.write(path, text)
      try {
        // an empty edit re-parses the file and reloads the engine config
        const r = await call('config/write', { edits: [] })
        setCfg(r)
        setDisk(text)
        setResult(r.warnings.length ? { kind: 'warning', message: `Saved with ${r.warnings.length} warning(s); see Warnings above.` } : { kind: 'success', message: 'Saved and reloaded. The previous version is in config.toml.bak.' })
        void useApp.getState().refreshModels().catch(() => {})
      } catch (e) {
        await window.odex.fs.write(path, previous)
        setResult({ kind: 'danger', message: `Not saved: ${(e as Error).message}. The previous file was kept; fix the error and save again.` })
      }
    } catch (e) {
      setResult({ kind: 'danger', message: `Could not write ${path}: ${(e as Error).message}` })
    } finally {
      setBusy(false)
    }
  }

  const reloadFromDisk = async () => {
    setResult(null)
    try {
      const r = await call('config/write', { edits: [] })
      setCfg(r)
      await loadFile(r.path)
      void useApp.getState().refreshModels().catch(() => {})
      toast('Config reloaded from disk', 'success')
    } catch (e) {
      await loadFile(path)
      setResult({ kind: 'danger', message: `The file on disk has an error: ${(e as Error).message}` })
    }
  }

  if (error) return <Callout kind="danger">Could not read the config: {error}</Callout>
  if (!cfg) return <div className="spinner" aria-label="Loading" />

  const active = cfg.activeProfile ?? ''
  return (
    <div className="sx-panel">
      <Section
        title="Config file"
        desc={
          <>
            Engine settings live in <code>config.toml</code> inside the Odex home folder. Settings panels edit it for you and keep comments and ordering intact. Projects can add a{' '}
            <code>.odex/config.toml</code> once trusted.
          </>
        }
      >
        <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
          <code className="selectable small grow ellipsis" title={cfg.path} style={{ minWidth: 200 }}>
            {cfg.path}
          </code>
          <button className="btn btn-sm" onClick={() => void window.odex.shell.openInEditor(cfg.path)}>
            <ExternalLink size={13} /> Open in editor
          </button>
          <button className="btn btn-sm" onClick={() => void window.odex.shell.showItem(cfg.path)}>
            <FolderOpen size={13} /> Reveal
          </button>
          <button className="btn btn-sm" onClick={() => void reloadFromDisk()} title="Re-read config.toml after editing it elsewhere">
            <RefreshCw size={13} /> Reload
          </button>
        </div>
      </Section>

      <Section title="Warnings">
        {cfg.warnings.length === 0 ? (
          <div className="small muted row" style={{ gap: 6 }}>
            <CircleCheck size={14} color="var(--success)" aria-hidden /> No warnings. The config parsed cleanly.
          </div>
        ) : (
          <div className="sx-list" role="list" aria-label="Config warnings">
            {cfg.warnings.map((w, i) => (
              <div key={i} className="sx-list-row small selectable" role="listitem">
                <TriangleAlert size={14} color="var(--warning)" aria-hidden />
                <span className="grow">{w}</span>
              </div>
            ))}
          </div>
        )}
      </Section>

      <Section
        title="Profiles"
        desc={
          <>
            A profile overrides a subset of settings (model, permission mode, reasoning effort, context, roles). Define them as <code>[profiles.&lt;name&gt;]</code> tables in the editor below.
          </>
        }
      >
        <Row label="Active profile" hint={active ? `Overrides: ${profileSummary(cfg.user.profiles[active])}` : 'No profile: the base settings apply'}>
          <select className="select" style={{ minWidth: 200 }} value={active} aria-label="Active profile" onChange={(e) => void write([{ keyPath: 'profile', value: e.target.value || null }]).then((ok) => ok && toast(e.target.value ? `Profile “${e.target.value}” is active` : 'Profile cleared', 'success'))}>
            <option value="">(none)</option>
            {cfg.profiles.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </Row>
        {cfg.profiles.length > 0 && (
          <div className="sx-list" style={{ marginTop: 10 }}>
            {cfg.profiles.map((p) => (
              <div key={p} className="sx-list-row small">
                <b>{p}</b>
                {p === active && <span className="badge accent">active</span>}
                <span className="muted grow ellipsis">{profileSummary(cfg.user.profiles[p])}</span>
              </div>
            ))}
          </div>
        )}
      </Section>

      <Section
        title="Raw config.toml"
        desc="Saving writes a backup (config.toml.bak), then reloads the engine. If the new file does not parse, the previous file is kept and the error is shown."
        actions={
          <>
            <SaveState state={dirty ? 'dirty' : 'idle'} />
            <button className="btn btn-sm" disabled={!dirty || busy} onClick={() => setText(disk)}>
              <RotateCcw size={13} /> Revert
            </button>
            <button className="btn btn-sm btn-primary" disabled={!dirty || busy} onClick={() => void save()}>
              <Save size={13} /> {busy ? 'Saving…' : 'Save'}
            </button>
          </>
        }
      >
        {result && (
          <div style={{ marginBottom: 8 }} role="alert">
            <Callout kind={result.kind}>
              <span className="selectable">{result.message}</span>
            </Callout>
          </div>
        )}
        <textarea
          className="textarea sx-editor tall"
          spellCheck={false}
          value={text ?? ''}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
              e.preventDefault()
              if (dirty) void save()
            }
          }}
          aria-label="config.toml contents"
        />
      </Section>
    </div>
  )
}
