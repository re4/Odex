import DOMPurify from 'dompurify'
import hljs from 'highlight.js/lib/common'
import { Marked } from 'marked'
import { memo, useEffect, useMemo, useRef, useSyncExternalStore } from 'react'
import '@/styles/thread-nav.css'

const marked = new Marked({ gfm: true, breaks: false })

marked.use({
  renderer: {
    code({ text, lang }) {
      const language = (lang || '').trim().split(/\s+/)[0]
      if (language.toLowerCase() === 'mermaid') {
        // rendered to a diagram after mount (lazy-loaded); the code block is the fallback
        return `<div class="mermaid-block pending" data-mermaid-src="${encodeURIComponent(text)}"><div class="code-block-header"><span>mermaid</span><span class="spacer"></span><button class="icon-btn sm copy-code" title="Copy source" aria-label="Copy code">⧉</button></div><div class="mermaid-svg"></div><div class="code-block mermaid-fallback"><pre><code class="hljs">${escapeHtml(text)}</code></pre></div></div>`
      }
      let html: string
      try {
        html = language && hljs.getLanguage(language) ? hljs.highlight(text, { language }).value : hljs.highlightAuto(text).value
      } catch {
        html = escapeHtml(text)
      }
      return `<div class="code-block"><div class="code-block-header"><span>${escapeHtml(language || 'text')}</span><span class="spacer"></span><button class="icon-btn sm copy-code" title="Copy" aria-label="Copy code">⧉</button></div><pre><code class="hljs">${html}</code></pre></div>`
    },
    link({ href, title, tokens }) {
      const text = this.parser.parseInline(tokens)
      return `<a href="${escapeHtml(href)}" ${title ? `title="${escapeHtml(title)}"` : ''} data-external="1">${text}</a>`
    },
  },
})

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!)
}

/** File references like `src/app.ts:42` become links that open in the Files tab. */
const FILE_REF = /(?<![\w/.-])((?:[\w.-]+[\\/])*[\w.-]+\.[a-zA-Z0-9]{1,8})(?::(\d+))?(?![\w/])/g

function linkifyFiles(html: string): string {
  // only outside of tags and code
  return html.replace(/(<code>)([^<]*)(<\/code>)/g, (_m, a, inner: string, c) => {
    const linked = inner.replace(FILE_REF, (m, path: string, line?: string) => {
      if (!/[\\/]/.test(path) && !/\.(rs|ts|tsx|js|jsx|py|go|java|c|cpp|h|md|json|toml|yaml|yml|css|html|sh|ps1)$/.test(path)) return m
      return `<a href="#" data-file="${escapeHtml(path)}" data-line="${line ?? ''}">${m}</a>`
    })
    return a + linked + c
  })
}

// ------------------------------------------------------------ mermaid

type MermaidApi = typeof import('mermaid').default
let mermaidLoad: Promise<MermaidApi> | null = null
let mermaidTheme = ''
let mermaidSeq = 0
const svgCache = new Map<string, string>()

/** Current light/dark theme (`data-theme` on <html>), as an external store. */
function subscribeTheme(cb: () => void): () => void {
  const mo = new MutationObserver(cb)
  mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
  return () => mo.disconnect()
}
const themeSnapshot = () => (document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light')

async function mermaidFor(theme: string): Promise<MermaidApi> {
  mermaidLoad ??= import('mermaid').then((m) => m.default)
  const m = await mermaidLoad
  if (mermaidTheme !== theme) {
    // strict: no click handlers or HTML labels; output is sanitized
    m.initialize({ startOnLoad: false, securityLevel: 'strict', theme: theme === 'dark' ? 'dark' : 'default' })
    mermaidTheme = theme
  }
  return m
}

/** A block's diagram source (URI-encoded: the sanitizer drops attribute values containing `-->`). */
function mermaidSource(b: HTMLElement): string {
  try {
    return decodeURIComponent(b.dataset.mermaidSrc ?? '')
  } catch {
    return ''
  }
}

/** Render every pending ```mermaid block under `root`; failures keep the code block. */
async function renderMermaid(root: HTMLElement, theme: string, alive: () => boolean): Promise<void> {
  const blocks = Array.from(root.querySelectorAll<HTMLElement>('.mermaid-block[data-mermaid-src]'))
  if (!blocks.length) return
  let m: MermaidApi
  try {
    m = await mermaidFor(theme)
  } catch {
    return
  }
  for (const b of blocks) {
    if (!alive()) return
    const src = mermaidSource(b)
    const key = `${theme}\n${src}`
    let svg = svgCache.get(key)
    if (svg === undefined) {
      try {
        svg = (await m.render(`odex-mermaid-${++mermaidSeq}`, src)).svg
      } catch {
        svg = ''
        // mermaid leaves its error element behind on failure
        document.getElementById(`dodex-mermaid-${mermaidSeq}`)?.remove()
      }
      svgCache.set(key, svg)
      if (svgCache.size > 200) svgCache.delete(svgCache.keys().next().value!)
    }
    if (!alive() || !b.isConnected) return
    b.classList.remove('pending')
    const out = b.querySelector<HTMLElement>('.mermaid-svg')
    const fallback = b.querySelector<HTMLElement>('.mermaid-fallback')
    if (svg && out) {
      out.innerHTML = svg
      if (fallback) fallback.style.display = 'none'
      b.classList.add('rendered')
    } else {
      b.classList.add('failed')
      if (out) out.style.display = 'none'
    }
  }
}

export const Markdown = memo(function Markdown(props: { text: string; onOpenFile?: (path: string, line?: number) => void; className?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const html = useMemo(() => {
    const raw = marked.parse(props.text || '', { async: false }) as string
    return linkifyFiles(DOMPurify.sanitize(raw, { ADD_ATTR: ['data-file', 'data-line', 'data-external', 'target', 'data-mermaid-src'] }))
  }, [props.text])
  const theme = useSyncExternalStore(subscribeTheme, themeSnapshot)
  const hasMermaid = html.includes('data-mermaid-src')
  useEffect(() => {
    const el = ref.current
    if (!el || !hasMermaid) return
    let alive = true
    // wait out streaming deltas before rendering diagrams
    const t = setTimeout(() => void renderMermaid(el, theme, () => alive), 120)
    return () => {
      alive = false
      clearTimeout(t)
    }
  }, [html, theme, hasMermaid])
  useEffect(() => {
    const el = ref.current
    if (!el) return
    const onClick = (e: MouseEvent) => {
      const t = e.target as HTMLElement
      if (t.tagName === 'IMG' && (t as HTMLImageElement).src && !t.closest('a')) {
        window.dispatchEvent(new CustomEvent('odex:lightbox', { detail: { src: (t as HTMLImageElement).src, name: (t as HTMLImageElement).alt || 'image' } }))
        return
      }
      const copy = t.closest('.copy-code')
      if (copy) {
        const block = copy.closest('.mermaid-block') as HTMLElement | null
        const code = block ? mermaidSource(block) : (copy.closest('.code-block')?.querySelector('code')?.textContent ?? '')
        void navigator.clipboard.writeText(code)
        copy.textContent = '✓'
        setTimeout(() => (copy.textContent = '⧉'), 1200)
        return
      }
      const a = t.closest('a') as HTMLAnchorElement | null
      if (!a) return
      e.preventDefault()
      if (a.dataset.file) {
        props.onOpenFile?.(a.dataset.file, a.dataset.line ? Number(a.dataset.line) : undefined)
      } else if (a.href && /^https?:/.test(a.href)) {
        void window.odex.shell.openExternal(a.href)
      }
    }
    el.addEventListener('click', onClick)
    return () => el.removeEventListener('click', onClick)
  }, [props])
  return <div ref={ref} className={`markdown selectable ${props.className ?? ''}`} dangerouslySetInnerHTML={{ __html: html }} />
})
