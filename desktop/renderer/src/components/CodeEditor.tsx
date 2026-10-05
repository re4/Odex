import { useEffect, useLayoutEffect, useRef } from 'react'
import { Compartment, EditorState, StateEffect, StateField, type Extension } from '@codemirror/state'
import {
  Decoration,
  type DecorationSet,
  EditorView,
  type ViewUpdate,
  crosshairCursor,
  drawSelection,
  dropCursor,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from '@codemirror/view'
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
import { bracketMatching, defaultHighlightStyle, foldGutter, foldKeymap, indentOnInput, syntaxHighlighting } from '@codemirror/language'
import { highlightSelectionMatches, search, searchKeymap } from '@codemirror/search'
import { oneDarkHighlightStyle } from '@codemirror/theme-one-dark'
import { javascript } from '@codemirror/lang-javascript'
import { python } from '@codemirror/lang-python'
import { rust } from '@codemirror/lang-rust'
import { json } from '@codemirror/lang-json'
import { markdown } from '@codemirror/lang-markdown'
import { css } from '@codemirror/lang-css'
import { html } from '@codemirror/lang-html'

/*
 * CodeMirror 6 editor used by the Files panel. Editor states are created
 * outside the component (one per open file, kept across tab switches so
 * undo history, selection and scroll survive) and swapped into a single
 * EditorView.
 */

const themeSlot = new Compartment()
const wrapSlot = new Compartment()
const readOnlySlot = new Compartment()

/** Per-view callbacks (states are shared, handlers belong to the mounted component). */
const handlers = new WeakMap<EditorView, { save?: () => void; update?: (u: ViewUpdate) => void }>()

export function isDarkTheme(): boolean {
  return document.documentElement.dataset.theme === 'dark'
}

const EXT_LANG: Record<string, () => Extension> = {
  js: () => javascript(),
  mjs: () => javascript(),
  cjs: () => javascript(),
  jsx: () => javascript({ jsx: true }),
  ts: () => javascript({ typescript: true }),
  mts: () => javascript({ typescript: true }),
  cts: () => javascript({ typescript: true }),
  tsx: () => javascript({ typescript: true, jsx: true }),
  py: () => python(),
  pyw: () => python(),
  pyi: () => python(),
  rs: () => rust(),
  json: () => json(),
  jsonc: () => json(),
  json5: () => json(),
  webmanifest: () => json(),
  md: () => markdown(),
  markdown: () => markdown(),
  mdx: () => markdown(),
  css: () => css(),
  scss: () => css(),
  less: () => css(),
  html: () => html(),
  htm: () => html(),
  xhtml: () => html(),
  svg: () => html(),
  xml: () => html(),
  vue: () => html(),
  svelte: () => html(),
}

export function languageName(path: string): string {
  const ext = extOf(path)
  const names: Record<string, string> = {
    js: 'JavaScript', mjs: 'JavaScript', cjs: 'JavaScript', jsx: 'JavaScript JSX', ts: 'TypeScript', mts: 'TypeScript', cts: 'TypeScript',
    tsx: 'TypeScript JSX', py: 'Python', pyw: 'Python', pyi: 'Python', rs: 'Rust', json: 'JSON', jsonc: 'JSON', json5: 'JSON',
    md: 'Markdown', markdown: 'Markdown', mdx: 'Markdown', css: 'CSS', scss: 'SCSS', less: 'Less', html: 'HTML', htm: 'HTML', svg: 'SVG', xml: 'XML',
  }
  return names[ext] ?? 'Plain text'
}

export function extOf(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? ''
  const i = name.lastIndexOf('.')
  return i > 0 ? name.slice(i + 1).toLowerCase() : ''
}

function languageFor(path: string): Extension {
  const f = EXT_LANG[extOf(path)]
  return f ? f() : []
}

// built once per scheme: each EditorView.theme() call mounts a new style module
const themeCache = new Map<boolean, Extension>()
function themeFor(dark: boolean): Extension {
  let ext = themeCache.get(dark)
  if (!ext) {
    ext = buildTheme(dark)
    themeCache.set(dark, ext)
  }
  return ext
}

function buildTheme(dark: boolean): Extension {
  const sel = 'color-mix(in srgb, var(--accent) 26%, transparent)'
  return [
    EditorView.theme(
      {
        '&': { height: '100%', backgroundColor: 'var(--bg-elev)', color: 'var(--fg)', fontSize: 'var(--font-size-sm)' },
        '&.cm-focused': { outline: 'none' },
        '.cm-scroller': { fontFamily: 'var(--font-code)', lineHeight: '1.6' },
        '.cm-content': { caretColor: 'var(--fg)', padding: '6px 0' },
        '.cm-cursor, .cm-dropCursor': { borderLeftColor: 'var(--fg)' },
        '&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, .cm-content ::selection': { backgroundColor: sel },
        '.cm-gutters': { backgroundColor: 'var(--bg-elev)', color: 'var(--fg-subtle)', border: 'none' },
        '.cm-lineNumbers .cm-gutterElement': { padding: '0 6px 0 10px', minWidth: '32px' },
        '.cm-foldGutter .cm-gutterElement': { padding: '0 4px 0 0', color: 'var(--fg-subtle)' },
        '.cm-activeLine': { backgroundColor: 'var(--bg-hover)' },
        '.cm-activeLineGutter': { backgroundColor: 'var(--bg-hover)', color: 'var(--fg)' },
        '.cm-panels': { backgroundColor: 'var(--bg-sunken)', color: 'var(--fg)' },
        '.cm-panels.cm-panels-top': { borderBottom: '1px solid var(--border)' },
        '.cm-panels.cm-panels-bottom': { borderTop: '1px solid var(--border)' },
        '.cm-searchMatch': {
          backgroundColor: 'color-mix(in srgb, var(--warning) 28%, transparent)',
          outline: '1px solid color-mix(in srgb, var(--warning) 55%, transparent)',
        },
        '.cm-searchMatch.cm-searchMatch-selected': { backgroundColor: 'color-mix(in srgb, var(--accent) 40%, transparent)' },
        '.cm-selectionMatch': { backgroundColor: 'color-mix(in srgb, var(--accent) 14%, transparent)' },
        '&.cm-focused .cm-matchingBracket': { backgroundColor: 'color-mix(in srgb, var(--success) 24%, transparent)', outline: 'none' },
        '&.cm-focused .cm-nonmatchingBracket': { backgroundColor: 'color-mix(in srgb, var(--danger) 24%, transparent)' },
        '.cm-foldPlaceholder': { backgroundColor: 'var(--bg-active)', border: 'none', color: 'var(--fg-muted)', padding: '0 4px' },
        '.cm-tooltip': { backgroundColor: 'var(--bg-elev)', border: '1px solid var(--border)', color: 'var(--fg)' },
        '.cm-specialChar': { color: 'var(--danger)' },
        '.cm-flash-line': { animation: 'odex-flash-line 1.8s ease-out forwards' },
      },
      { dark },
    ),
    syntaxHighlighting(dark ? oneDarkHighlightStyle : defaultHighlightStyle, { fallback: true }),
  ]
}

// ---- line flash (reveal a line opened from elsewhere)

const flashEffect = StateEffect.define<number | null>()
const flashMark = Decoration.line({ class: 'cm-flash-line' })
const flashField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const e of tr.effects) if (e.is(flashEffect)) return e.value == null ? Decoration.none : Decoration.set([flashMark.range(e.value)])
    return tr.docChanged ? Decoration.none : deco
  },
  provide: (f) => EditorView.decorations.from(f),
})

export interface EditorOptions {
  readOnly?: boolean
  wrap?: boolean
}

/** Build the editor state for one file. */
export function createEditorState(doc: string, path: string, opts: EditorOptions = {}): EditorState {
  return EditorState.create({
    doc,
    extensions: [
      lineNumbers(),
      highlightActiveLineGutter(),
      highlightSpecialChars(),
      history(),
      foldGutter({ openText: '▾', closedText: '▸' }),
      drawSelection(),
      dropCursor(),
      EditorState.allowMultipleSelections.of(true),
      indentOnInput(),
      bracketMatching(),
      rectangularSelection(),
      crosshairCursor(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      search({ top: true }),
      keymap.of([
        {
          key: 'Mod-s',
          preventDefault: true,
          run: (v) => {
            handlers.get(v)?.save?.()
            return true
          },
        },
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        ...foldKeymap,
        indentWithTab,
      ]),
      EditorView.updateListener.of((u) => handlers.get(u.view)?.update?.(u)),
      flashField,
      languageFor(path),
      themeSlot.of(themeFor(isDarkTheme())),
      wrapSlot.of(opts.wrap ? EditorView.lineWrapping : []),
      readOnlySlot.of(EditorState.readOnly.of(!!opts.readOnly)),
    ],
  })
}

/** Reconfigure line wrapping / read-only on a view. */
export function setEditorOptions(view: EditorView, opts: EditorOptions): void {
  const effects: StateEffect<unknown>[] = []
  if (opts.wrap !== undefined) effects.push(wrapSlot.reconfigure(opts.wrap ? EditorView.lineWrapping : []))
  if (opts.readOnly !== undefined) effects.push(readOnlySlot.reconfigure(EditorState.readOnly.of(opts.readOnly)))
  if (effects.length) view.dispatch({ effects })
}

/** Same, for a state that is not mounted in a view. */
export function withEditorOptions(state: EditorState, opts: EditorOptions): EditorState {
  const effects: StateEffect<unknown>[] = []
  if (opts.wrap !== undefined) effects.push(wrapSlot.reconfigure(opts.wrap ? EditorView.lineWrapping : []))
  if (opts.readOnly !== undefined) effects.push(readOnlySlot.reconfigure(EditorState.readOnly.of(opts.readOnly)))
  return effects.length ? state.update({ effects }).state : state
}

const scrollPos = new Map<string, { top: number; left: number }>()

export interface CodeEditorProps {
  /** Identity of the document shown; changing it swaps in `getState()`. */
  docKey: string
  getState: () => EditorState
  onUpdate?: (u: ViewUpdate) => void
  onSave?: () => void
  /** Called with the view on mount and after every document swap, and with null on unmount. */
  onView?: (v: EditorView | null) => void
  /** Scroll to and flash a 1-based line (re-runs when `at` changes). */
  reveal?: { line: number; at: number } | null
  onRevealed?: () => void
  wrap?: boolean
  readOnly?: boolean
  label?: string
}

export function CodeEditor(props: CodeEditorProps) {
  const host = useRef<HTMLDivElement>(null)
  const viewRef = useRef<EditorView | null>(null)
  const keyRef = useRef<string>(props.docKey)
  const latest = useRef(props)
  latest.current = props

  // create the view once
  useLayoutEffect(() => {
    const el = host.current!
    const view = new EditorView({ state: latest.current.getState(), parent: el })
    viewRef.current = view
    keyRef.current = latest.current.docKey
    handlers.set(view, { save: () => latest.current.onSave?.(), update: (u) => latest.current.onUpdate?.(u) })
    view.dispatch({ effects: themeSlot.reconfigure(themeFor(isDarkTheme())) })
    view.contentDOM.setAttribute('aria-label', latest.current.label ?? 'Editor')
    if (!latest.current.reveal) restoreScroll(view, keyRef.current)
    latest.current.onView?.(view)

    // keys the editor handled (Ctrl+F, Ctrl+S, Ctrl+[ ...) must not reach global shortcuts
    const stop = (e: KeyboardEvent) => {
      if (e.defaultPrevented) e.stopPropagation()
    }
    el.addEventListener('keydown', stop)
    // theme / font changes on <html>
    const mo = new MutationObserver(() => {
      view.dispatch({ effects: themeSlot.reconfigure(themeFor(isDarkTheme())) })
      view.requestMeasure()
    })
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'style', 'data-density'] })
    const onScroll = () => scrollPos.set(keyRef.current, { top: view.scrollDOM.scrollTop, left: view.scrollDOM.scrollLeft })
    view.scrollDOM.addEventListener('scroll', onScroll, { passive: true })
    return () => {
      mo.disconnect()
      el.removeEventListener('keydown', stop)
      view.scrollDOM.removeEventListener('scroll', onScroll)
      latest.current.onView?.(null)
      handlers.delete(view)
      view.destroy()
      viewRef.current = null
    }
  }, [])

  // swap documents
  useLayoutEffect(() => {
    const view = viewRef.current
    if (!view || keyRef.current === props.docKey) return
    keyRef.current = props.docKey
    view.setState(latest.current.getState())
    latest.current.onView?.(view)
    view.dispatch({ effects: themeSlot.reconfigure(themeFor(isDarkTheme())) })
    view.contentDOM.setAttribute('aria-label', latest.current.label ?? 'Editor')
    if (!latest.current.reveal) restoreScroll(view, props.docKey)
  }, [props.docKey])

  // options
  useEffect(() => {
    const view = viewRef.current
    if (view) setEditorOptions(view, { wrap: !!props.wrap, readOnly: !!props.readOnly })
  }, [props.wrap, props.readOnly, props.docKey])

  // reveal a line
  const revealAt = props.reveal?.at
  useEffect(() => {
    const view = viewRef.current
    const r = latest.current.reveal
    if (!view || !r) return
    const n = Math.min(Math.max(1, Math.floor(r.line) || 1), view.state.doc.lines)
    const line = view.state.doc.line(n)
    view.dispatch({
      selection: { anchor: line.from },
      effects: [EditorView.scrollIntoView(line.from, { y: 'center' }), flashEffect.of(line.from)],
    })
    view.focus()
    latest.current.onRevealed?.()
  }, [revealAt, props.docKey])

  return <div ref={host} className="code-editor" />
}

function restoreScroll(view: EditorView, key: string): void {
  const pos = scrollPos.get(key)
  if (!pos) return
  view.requestMeasure({
    read: () => null,
    write: () => {
      view.scrollDOM.scrollTop = pos.top
      view.scrollDOM.scrollLeft = pos.left
    },
  })
}

/** Forget remembered scroll for a closed document. */
export function forgetEditorScroll(key: string): void {
  scrollPos.delete(key)
}
