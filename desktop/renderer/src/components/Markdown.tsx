import DOMPurify from 'dompurify'
import hljs from 'highlight.js/lib/common'
import { Marked } from 'marked'
import { memo, useEffect, useMemo, useRef } from 'react'

const marked = new Marked({ gfm: true, breaks: false })

marked.use({
  renderer: {
    code({ text, lang }) {
      const language = (lang || '').trim().split(/\s+/)[0]
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

export const Markdown = memo(function Markdown(props: { text: string; onOpenFile?: (path: string, line?: number) => void; className?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const html = useMemo(() => {
    const raw = marked.parse(props.text || '', { async: false }) as string
    return linkifyFiles(DOMPurify.sanitize(raw, { ADD_ATTR: ['data-file', 'data-line', 'data-external', 'target'] }))
  }, [props.text])
  useEffect(() => {
    const el = ref.current
    if (!el) return
    const onClick = (e: MouseEvent) => {
      const t = e.target as HTMLElement
      const copy = t.closest('.copy-code')
      if (copy) {
        const code = copy.closest('.code-block')?.querySelector('code')?.textContent ?? ''
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
