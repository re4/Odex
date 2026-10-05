import { ReactNode, useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'

/** Modal dialog with backdrop; Esc and backdrop click close it. */
export function Modal(props: { title?: ReactNode; onClose: () => void; children: ReactNode; footer?: ReactNode; wide?: boolean; className?: string; labelledBy?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const onClose = useRef(props.onClose)
  onClose.current = props.onClose
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null
    const first = ref.current?.querySelector<HTMLElement>('input, textarea, select, button:not([data-close])')
    first?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose.current()
      }
    }
    window.addEventListener('keydown', onKey, true)
    return () => {
      window.removeEventListener('keydown', onKey, true)
      prev?.focus?.()
    }
     
  }, [])
  return createPortal(
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div ref={ref} className={`modal ${props.wide ? 'wide' : ''} ${props.className ?? ''}`} role="dialog" aria-modal="true" aria-label={typeof props.title === 'string' ? props.title : undefined}>
        {props.title != null && <div className="modal-header">{props.title}</div>}
        <div className="modal-body">{props.children}</div>
        {props.footer && <div className="modal-footer">{props.footer}</div>}
      </div>
    </div>,
    document.body,
  )
}

export interface MenuItem {
  label: ReactNode
  icon?: ReactNode
  hint?: ReactNode
  onSelect?: () => void
  disabled?: boolean
  danger?: boolean
  separator?: boolean
  header?: boolean
  checked?: boolean
}

/** Positioned popup menu anchored to an element or point. Keyboard navigable. */
export function Menu(props: { anchor: HTMLElement | { x: number; y: number } | null; items: MenuItem[]; onClose: () => void; align?: 'left' | 'right'; above?: boolean; minWidth?: number }) {
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState<{ left: number; top: number }>({ left: -9999, top: -9999 })
  const [active, setActive] = useState(-1)
  const selectable = props.items.map((it, i) => (!it.separator && !it.header && !it.disabled ? i : -1)).filter((i) => i >= 0)

  useLayoutEffect(() => {
    const el = ref.current
    if (!el || !props.anchor) return
    const r = 'getBoundingClientRect' in props.anchor ? props.anchor.getBoundingClientRect() : { left: props.anchor.x, right: props.anchor.x, top: props.anchor.y, bottom: props.anchor.y, width: 0, height: 0 }
    const w = el.offsetWidth
    const h = el.offsetHeight
    let left = props.align === 'right' ? r.right - w : r.left
    let top = props.above ? r.top - h - 4 : r.bottom + 4
    if (top + h > window.innerHeight - 8) top = Math.max(8, r.top - h - 4)
    if (top < 8) top = 8
    left = Math.min(Math.max(8, left), window.innerWidth - w - 8)
    setPos({ left, top })
  }, [props.anchor, props.align, props.above, props.items.length])

  useEffect(() => {
    // a closed menu (anchor = null) must not capture keys or clicks
    if (!props.anchor) return
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) props.onClose()
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        props.onClose()
      } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault()
        const cur = selectable.indexOf(active)
        const next = e.key === 'ArrowDown' ? (cur + 1) % selectable.length : (cur - 1 + selectable.length) % selectable.length
        setActive(selectable[next] ?? -1)
      } else if (e.key === 'Enter' && active >= 0) {
        e.preventDefault()
        const it = props.items[active]
        props.onClose()
        it.onSelect?.()
      }
    }
    const t = setTimeout(() => window.addEventListener('mousedown', onDown), 0)
    window.addEventListener('keydown', onKey, true)
    return () => {
      clearTimeout(t)
      window.removeEventListener('mousedown', onDown)
      window.removeEventListener('keydown', onKey, true)
    }
  })

  if (!props.anchor) return null
  return createPortal(
    <div ref={ref} className="menu" role="menu" style={{ left: pos.left, top: pos.top, minWidth: props.minWidth }}>
      {props.items.map((it, i) =>
        it.separator ? (
          <div key={i} className="menu-sep" />
        ) : it.header ? (
          <div key={i} className="menu-label">
            {it.label}
          </div>
        ) : (
          <button
            key={i}
            role="menuitem"
            className="menu-item"
            data-active={i === active}
            disabled={it.disabled}
            style={it.danger ? { color: 'var(--danger)' } : undefined}
            onMouseEnter={() => setActive(i)}
            onClick={() => {
              props.onClose()
              it.onSelect?.()
            }}
          >
            {it.checked !== undefined && <span style={{ width: 14 }}>{it.checked ? '✓' : ''}</span>}
            {it.icon}
            <span className="grow ellipsis">{it.label}</span>
            {it.hint && <span className="hint">{it.hint}</span>}
          </button>
        ),
      )}
    </div>,
    document.body,
  )
}

/** Hook: open a menu anchored on a clicked element. */
export function useMenu(): [HTMLElement | null, (e: React.MouseEvent<HTMLElement>) => void, () => void] {
  const [anchor, setAnchor] = useState<HTMLElement | null>(null)
  const open = useCallback((e: React.MouseEvent<HTMLElement>) => {
    e.stopPropagation()
    // read the target now: the updater may run after the event is gone
    const el = e.currentTarget
    setAnchor((a) => (a ? null : el))
  }, [])
  const close = useCallback(() => setAnchor(null), [])
  return [anchor, open, close]
}

export function Toggle(props: { checked: boolean; onChange: (v: boolean) => void; label?: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.checked}
      aria-label={props.label}
      className="toggle"
      disabled={props.disabled}
      onClick={() => props.onChange(!props.checked)}
    />
  )
}

/** Drag handle that resizes a neighbor. `axis` x → width, y → height. */
export function ResizeHandle(props: { axis: 'x' | 'y'; value: number; onChange: (v: number) => void; min: number; max: number; invert?: boolean; label?: string }) {
  const [dragging, setDragging] = useState(false)
  const onDown = (e: React.PointerEvent) => {
    e.preventDefault()
    const start = props.axis === 'x' ? e.clientX : e.clientY
    const startVal = props.value
    setDragging(true)
    const move = (ev: PointerEvent) => {
      const d = (props.axis === 'x' ? ev.clientX : ev.clientY) - start
      const v = startVal + (props.invert ? -d : d)
      props.onChange(Math.min(props.max, Math.max(props.min, v)))
    }
    const up = () => {
      setDragging(false)
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
  }
  return (
    <div
      className={`${props.axis === 'x' ? 'resize-h' : 'resize-v'} ${dragging ? 'dragging' : ''}`}
      role="separator"
      aria-orientation={props.axis === 'x' ? 'vertical' : 'horizontal'}
      aria-label={props.label ?? 'Resize'}
      tabIndex={0}
      onPointerDown={onDown}
      onKeyDown={(e) => {
        const step = e.shiftKey ? 40 : 10
        const dir = props.axis === 'x' ? (e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0) : e.key === 'ArrowDown' ? 1 : e.key === 'ArrowUp' ? -1 : 0
        if (dir) props.onChange(Math.min(props.max, Math.max(props.min, props.value + (props.invert ? -dir : dir) * step)))
      }}
    />
  )
}

/** Deterministic 5x5 symmetric identicon (subagents, threads). */
export function Identicon(props: { seed: string; size?: number }) {
  const size = props.size ?? 20
  let h = 2166136261
  for (let i = 0; i < props.seed.length; i++) {
    h ^= props.seed.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  const hue = Math.abs(h) % 360
  const cells: boolean[] = []
  let x = Math.abs(h)
  for (let i = 0; i < 15; i++) {
    cells.push((x & 1) === 1)
    x = (x >>> 1) | ((x & 1) << 30)
    x = Math.imul(x ^ (x >>> 15), 2246822507) >>> 0
  }
  const c = size / 5
  const rects = []
  for (let row = 0; row < 5; row++) {
    for (let col = 0; col < 3; col++) {
      if (!cells[row * 3 + col]) continue
      rects.push(<rect key={`${row}-${col}`} x={col * c} y={row * c} width={c} height={c} />)
      if (col < 2) rects.push(<rect key={`${row}-${col}m`} x={(4 - col) * c} y={row * c} width={c} height={c} />)
    }
  }
  return (
    <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} style={{ borderRadius: 4, background: `hsl(${hue} 60% 92%)`, flex: 'none' }} aria-hidden>
      <g fill={`hsl(${hue} 55% 45%)`}>{rects}</g>
    </svg>
  )
}

interface ToastMsg {
  id: number
  kind: 'info' | 'error' | 'success'
  text: string
}

/** Renders toasts dispatched via `toast()` (lib/rpc). */
export function Toasts() {
  const [list, setList] = useState<ToastMsg[]>([])
  useEffect(() => {
    let n = 0
    const h = (e: Event) => {
      const d = (e as CustomEvent).detail as { kind: ToastMsg['kind']; text: string }
      const id = ++n
      setList((l) => [...l.slice(-4), { id, ...d }])
      setTimeout(() => setList((l) => l.filter((t) => t.id !== id)), d.kind === 'error' ? 8000 : 4000)
    }
    window.addEventListener('odex:toast', h)
    return () => window.removeEventListener('odex:toast', h)
  }, [])
  return (
    <div className="toasts" role="status" aria-live="polite">
      {list.map((t) => (
        <div key={t.id} className={`toast ${t.kind}`} onClick={() => setList((l) => l.filter((x) => x.id !== t.id))}>
          {t.text}
        </div>
      ))}
    </div>
  )
}

export function relativeTime(ms: number): string {
  const d = Date.now() - ms
  if (d < 60_000) return 'now'
  if (d < 3_600_000) return `${Math.floor(d / 60_000)}m`
  if (d < 86_400_000) return `${Math.floor(d / 3_600_000)}h`
  if (d < 7 * 86_400_000) return `${Math.floor(d / 86_400_000)}d`
  return new Date(ms).toLocaleDateString()
}

export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 10_000) return `${Math.round(n / 1000)}k`
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`
  return String(n)
}

export function basename(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() ?? p
}
