import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { Download, Maximize, X, ZoomIn, ZoomOut } from 'lucide-react'
import { toast } from '@/lib/rpc'
import '@/styles/thread-nav.css'

/*
 * Full-window image viewer for images in a thread (attachments, appshots,
 * computer-use and browser screenshots, viewed images). Opened with
 * `openImage(src, name)` (an `odex:lightbox` event). Fit / 100% / wheel zoom,
 * drag to pan, Save.
 */

const MIN = 0.05
const MAX = 8

function extFor(src: string): string {
  const m = /^data:image\/([a-z0-9+.-]+)/i.exec(src)
  if (m) return m[1].toLowerCase() === 'jpeg' ? 'jpg' : m[1].toLowerCase().replace('svg+xml', 'svg')
  const e = /\.([a-z0-9]{2,5})(?:[?#].*)?$/i.exec(src)
  return e ? e[1].toLowerCase() : 'png'
}

async function toDataUrl(src: string): Promise<string> {
  if (src.startsWith('data:')) return src
  const blob = await (await fetch(src)).blob()
  return new Promise((resolve, reject) => {
    const r = new FileReader()
    r.onload = () => resolve(String(r.result))
    r.onerror = () => reject(r.error)
    r.readAsDataURL(blob)
  })
}

export async function saveImage(src: string, name: string): Promise<void> {
  const base = name.replace(/[\\/:*?"<>|]+/g, '_').replace(/\.[a-z0-9]{2,5}$/i, '') || 'image'
  const fileName = `${base}.${extFor(src)}`
  try {
    const dataUrl = await toDataUrl(src)
    if (window.odex.dialog.saveFile) {
      const path = await window.odex.dialog.saveFile({ defaultName: fileName, dataUrl })
      if (path) toast(`Saved ${path}`)
      return
    }
    const a = document.createElement('a')
    a.href = dataUrl
    a.download = fileName
    a.click()
  } catch (e) {
    toast(`Could not save the image: ${(e as Error).message}`, 'error')
  }
}

export function ImageLightbox() {
  const [img, setImg] = useState<{ src: string; name: string } | null>(null)
  useEffect(() => {
    const onOpen = (e: Event) => setImg((e as CustomEvent<{ src: string; name: string }>).detail)
    window.addEventListener('odex:lightbox', onOpen)
    return () => window.removeEventListener('odex:lightbox', onOpen)
  }, [])
  if (!img) return null
  return <Viewer key={img.src} src={img.src} name={img.name} onClose={() => setImg(null)} />
}

function Viewer({ src, name, onClose }: { src: string; name: string; onClose: () => void }) {
  const stage = useRef<HTMLDivElement>(null)
  const imgRef = useRef<HTMLImageElement>(null)
  const [nat, setNat] = useState<{ w: number; h: number } | null>(null)
  // null = fit to the window
  const [scale, setScale] = useState<number | null>(null)
  const [fitScale, setFitScale] = useState(1)
  const drag = useRef<{ x: number; y: number; left: number; top: number } | null>(null)
  const pendingScroll = useRef<{ fx: number; fy: number; cx: number; cy: number } | null>(null)

  // fit scale follows the window size
  useLayoutEffect(() => {
    const el = stage.current
    if (!el || !nat) return
    const update = () => setFitScale(Math.min(1, (el.clientWidth - 32) / nat.w, (el.clientHeight - 32) / nat.h))
    update()
    const ro = new ResizeObserver(update)
    ro.observe(el)
    return () => ro.disconnect()
  }, [nat])

  const effective = scale ?? fitScale
  const zoomBy = (f: number, at?: { x: number; y: number }) => {
    const el = stage.current
    const next = Math.min(MAX, Math.max(MIN, effective * f))
    if (el && nat) {
      // keep the point under the cursor (or the center) in place
      const r = el.getBoundingClientRect()
      const cx = at ? at.x - r.left : el.clientWidth / 2
      const cy = at ? at.y - r.top : el.clientHeight / 2
      const im = imgRef.current?.getBoundingClientRect()
      const fx = im ? (cx + r.left - im.left) / im.width : 0.5
      const fy = im ? (cy + r.top - im.top) / im.height : 0.5
      pendingScroll.current = { fx, fy, cx, cy }
    }
    setScale(next)
  }

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
      } else if (e.ctrlKey || e.metaKey || e.altKey) return
      else if (e.key === '+' || e.key === '=') zoomBy(1.25)
      else if (e.key === '-') zoomBy(0.8)
      else if (e.key === '0') setScale(null)
      else if (e.key === '1') setScale(1)
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  })

  useLayoutEffect(() => {
    const el = stage.current
    const p = pendingScroll.current
    const im = imgRef.current
    if (!el || !p || !im) return
    pendingScroll.current = null
    el.scrollLeft = im.offsetLeft + p.fx * im.offsetWidth - p.cx
    el.scrollTop = im.offsetTop + p.fy * im.offsetHeight - p.cy
  }, [scale])

  const pct = Math.round(effective * 100)
  const w = nat ? Math.round(nat.w * effective) : undefined
  const h = nat ? Math.round(nat.h * effective) : undefined
  return createPortal(
    <div className="lightbox" role="dialog" aria-modal="true" aria-label={`Image: ${name}`}>
      <div className="lightbox-bar">
        <span className="ellipsis grow" title={name}>
          {name}
        </span>
        {nat && (
          <span className="xs lightbox-dim">
            {nat.w}×{nat.h}
          </span>
        )}
        <button className="icon-btn sm" aria-label="Zoom out" title="Zoom out (-)" onClick={() => zoomBy(0.8)}>
          <ZoomOut size={14} />
        </button>
        <span className="xs lightbox-pct" aria-label="Zoom level">
          {pct}%
        </span>
        <button className="icon-btn sm" aria-label="Zoom in" title="Zoom in (+)" onClick={() => zoomBy(1.25)}>
          <ZoomIn size={14} />
        </button>
        <button className={`btn btn-sm ${scale === null ? 'active' : ''}`} aria-pressed={scale === null} title="Fit to window (0)" onClick={() => setScale(null)}>
          <Maximize size={12} /> Fit
        </button>
        <button className={`btn btn-sm ${scale === 1 ? 'active' : ''}`} aria-pressed={scale === 1} aria-label="Actual size" title="Actual size, 100% (1)" onClick={() => setScale(1)}>
          1:1
        </button>
        <button className="btn btn-sm" onClick={() => void saveImage(src, name)}>
          <Download size={12} /> Save
        </button>
        <button className="icon-btn sm" aria-label="Close image" title="Close (Esc)" onClick={onClose}>
          <X size={14} />
        </button>
      </div>
      <div
        ref={stage}
        className={`lightbox-stage ${scale !== null && scale > fitScale ? 'pannable' : ''}`}
        onMouseDown={(e) => {
          if (e.target === e.currentTarget && (scale === null || scale <= fitScale)) return onClose()
          const el = stage.current
          if (!el || e.button !== 0) return
          drag.current = { x: e.clientX, y: e.clientY, left: el.scrollLeft, top: el.scrollTop }
          e.preventDefault()
        }}
        onMouseMove={(e) => {
          const d = drag.current
          const el = stage.current
          if (!d || !el) return
          el.scrollLeft = d.left - (e.clientX - d.x)
          el.scrollTop = d.top - (e.clientY - d.y)
        }}
        onMouseUp={() => (drag.current = null)}
        onMouseLeave={() => (drag.current = null)}
        onWheel={(e) => zoomBy(e.deltaY < 0 ? 1.15 : 1 / 1.15, { x: e.clientX, y: e.clientY })}
        onDoubleClick={(e) => (scale === null ? zoomBy(1 / effective, { x: e.clientX, y: e.clientY }) : setScale(null))}
      >
        <div className="lightbox-canvas">
          <img
            ref={imgRef}
            src={src}
            alt={name}
            draggable={false}
            style={w && h ? { width: w, height: h } : { maxWidth: '100%', maxHeight: '100%' }}
            onLoad={(e) => setNat({ w: e.currentTarget.naturalWidth || 1, h: e.currentTarget.naturalHeight || 1 })}
          />
        </div>
      </div>
    </div>,
    document.body,
  )
}
