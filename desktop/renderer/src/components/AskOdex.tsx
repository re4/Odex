import { useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { MessageSquareQuote } from 'lucide-react'
import { EditorView } from '@codemirror/view'
import { useApp } from '@/store/app'
import '@/styles/thread-nav.css'

/*
 * "Ask Odex": select text in the conversation (messages, command output),
 * the code editor or a diff, and a small button appears next to the selection.
 * Clicking it quotes the selection into the composer.
 */

/** Where a selection may be quoted from (closest match wins). */
const SOURCES = '.cm-editor, .dv-file, .mini-diff, .thread-view .cell-body, .thread-view .markdown, .thread-view .user-bubble, .files-md-preview .markdown'

const FENCE_LANG: Record<string, string> = { py: 'python', ts: 'ts', tsx: 'tsx', js: 'js', jsx: 'jsx', rs: 'rust', go: 'go', md: 'markdown', json: 'json', css: 'css', html: 'html', sh: 'sh', ps1: 'powershell', toml: 'toml', yaml: 'yaml', yml: 'yaml', java: 'java', c: 'c', cpp: 'cpp', h: 'c' }

function fence(code: string, file?: string): string {
  const ext = file?.split('.').pop()?.toLowerCase() ?? ''
  const ticks = code.includes('```') ? '````' : '```'
  return `${ticks}${FENCE_LANG[ext] ?? ''}\n${code.replace(/\n+$/, '')}\n${ticks}`
}

function blockquote(text: string): string {
  return text
    .replace(/\n{3,}/g, '\n\n')
    .trim()
    .split('\n')
    .map((l) => (l.trim() ? `> ${l}` : '>'))
    .join('\n')
}

interface Pending {
  x: number
  y: number
  quote: string
}

/** Build the quote for the current selection, or null if it isn't quotable. */
function quoteFor(sel: Selection): string | null {
  if (sel.rangeCount === 0 || sel.isCollapsed) return null
  const range = sel.getRangeAt(0)
  const startEl = (range.startContainer.nodeType === 1 ? range.startContainer : range.startContainer.parentElement) as Element | null
  const endEl = (range.endContainer.nodeType === 1 ? range.endContainer : range.endContainer.parentElement) as Element | null
  if (!startEl || !endEl) return null
  // never inside text fields (the composer itself, inputs)
  if (startEl.closest('textarea, input, .composer, [role="dialog"]')) return null
  const src = startEl.closest(SOURCES)
  if (!src || !src.contains(endEl)) return null

  if (src.classList.contains('cm-editor')) {
    const view = EditorView.findFromDOM(src as HTMLElement)
    if (!view) return null
    const { from, to } = view.state.selection.main
    const code = from < to ? view.state.sliceDoc(from, to) : sel.toString()
    if (!code.trim()) return null
    const label = view.contentDOM.getAttribute('aria-label') ?? ''
    const file = label.startsWith('Editor: ') ? label.slice(8) : undefined
    const l1 = view.state.doc.lineAt(from).number
    const l2 = view.state.doc.lineAt(Math.max(from, to - 1)).number
    const where = file ? `\`${file}\` ${l1 === l2 ? `line ${l1}` : `lines ${l1}-${l2}`}` : l1 === l2 ? `line ${l1}` : `lines ${l1}-${l2}`
    return `From ${where}:\n${fence(code, file)}\n\n`
  }
  if (src.classList.contains('dv-file') || src.classList.contains('mini-diff')) {
    // only the code columns, not line numbers or +/- signs
    const lines = Array.from(src.querySelectorAll('.dv-code, .ln')).filter((n) => range.intersectsNode(n))
    const code = lines.length > 1 ? lines.map((n) => n.textContent ?? '').join('\n') : sel.toString()
    if (!code.trim()) return null
    const file = (src as HTMLElement).dataset.file || src.querySelector('.dv-file-head [title]')?.getAttribute('title') || undefined
    return `${file ? `From the diff of \`${file}\`` : 'From the diff'}:\n${fence(code, file)}\n\n`
  }
  const text = sel.toString()
  if (!text.trim()) return null
  if (src.classList.contains('cell-body')) return `${fence(text)}\n\n`
  return `${blockquote(text)}\n\n`
}

export function AskOdex() {
  const [p, setP] = useState<Pending | null>(null)
  const raf = useRef(0)

  useEffect(() => {
    const update = () => {
      cancelAnimationFrame(raf.current)
      raf.current = requestAnimationFrame(() => {
        const sel = document.getSelection()
        const quote = sel ? quoteFor(sel) : null
        if (!sel || !quote) return setP(null)
        const rects = Array.from(sel.getRangeAt(0).getClientRects()).filter((r) => r.width > 0 || r.height > 0)
        const last = rects[rects.length - 1] ?? sel.getRangeAt(0).getBoundingClientRect()
        if (!last || (last.width === 0 && last.height === 0)) return setP(null)
        const x = Math.min(Math.max(8, last.right - 4), window.innerWidth - 120)
        const below = last.bottom + 36 < window.innerHeight
        setP({ x, y: below ? last.bottom + 6 : Math.max(8, last.top - 34), quote })
      })
    }
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setP(null)
    document.addEventListener('selectionchange', update)
    window.addEventListener('scroll', update, true)
    window.addEventListener('resize', update)
    window.addEventListener('keydown', onKey)
    return () => {
      cancelAnimationFrame(raf.current)
      document.removeEventListener('selectionchange', update)
      window.removeEventListener('scroll', update, true)
      window.removeEventListener('resize', update)
      window.removeEventListener('keydown', onKey)
    }
  }, [])

  if (!p) return null
  const ask = () => {
    const s = useApp.getState()
    const cur = s.selectedThreadId ? (s.threads[s.selectedThreadId]?.draft ?? '') : ''
    const text = cur && !cur.endsWith('\n') ? `\n\n${p.quote}` : p.quote
    if (s.ui.view !== 'thread' && s.ui.view !== 'home') s.setUi({ view: s.selectedThreadId ? 'thread' : 'home' })
    window.dispatchEvent(new CustomEvent('odex:attach', { detail: { type: 'text', text } }))
    document.getSelection()?.removeAllRanges()
    setP(null)
    // the composer focuses itself on attach; make sure the caret lands at the end
    requestAnimationFrame(() => {
      const ta = document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Message"]')
      if (ta) {
        ta.focus()
        ta.setSelectionRange(ta.value.length, ta.value.length)
      }
    })
  }
  return createPortal(
    <button
      className="ask-odex"
      style={{ left: p.x, top: p.y }}
      // keep the selection while clicking
      onMouseDown={(e) => e.preventDefault()}
      onClick={ask}
      title="Quote the selection in the composer"
    >
      <MessageSquareQuote size={13} /> Ask Odex
    </button>,
    document.body,
  )
}
