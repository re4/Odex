import { useEffect } from 'react'
import { useApp } from '@/store/app'

/**
 * Keyboard shortcuts. Defaults follow the upstream app's Windows/Linux table
 * (Ctrl ↔ Cmd on macOS). Every entry is rebindable in Settings → Keyboard
 * Shortcuts; overrides live in desktop settings (`shortcuts`).
 */
export interface ShortcutDef {
  id: string
  label: string
  group: string
  keys: string // e.g. "Mod+K", "Mod+Shift+P", "Ctrl+`"
}

export const SHORTCUTS: ShortcutDef[] = [
  { id: 'palette', label: 'Command palette', group: 'General', keys: 'Mod+K' },
  { id: 'palette2', label: 'Command palette (alt)', group: 'General', keys: 'Mod+Shift+P' },
  { id: 'settings', label: 'Settings', group: 'General', keys: 'Mod+,' },
  { id: 'shortcuts', label: 'Keyboard shortcuts', group: 'General', keys: 'Mod+/' },
  { id: 'fileSearch', label: 'Search files', group: 'General', keys: 'Mod+P' },
  { id: 'openFolder', label: 'Open folder', group: 'General', keys: 'Mod+O' },
  { id: 'back', label: 'Back', group: 'Navigation', keys: 'Mod+[' },
  { id: 'forward', label: 'Forward', group: 'Navigation', keys: 'Mod+]' },
  { id: 'toggleSidebar', label: 'Toggle sidebar', group: 'View', keys: 'Mod+B' },
  { id: 'toggleBottom', label: 'Toggle bottom panel', group: 'View', keys: 'Mod+J' },
  { id: 'toggleTerminal', label: 'Toggle terminal', group: 'View', keys: 'Ctrl+`' },
  { id: 'toggleFileTree', label: 'Toggle file tree', group: 'View', keys: 'Mod+Shift+E' },
  { id: 'openReview', label: 'Open review tab', group: 'View', keys: 'Ctrl+Shift+G' },
  { id: 'cycleLayout', label: 'Cycle side panel layout', group: 'View', keys: 'Mod+Shift+B' },
  { id: 'newBrowserTab', label: 'New browser tab', group: 'View', keys: 'Mod+T' },
  { id: 'zoomIn', label: 'Zoom in', group: 'View', keys: 'Mod+=' },
  { id: 'zoomOut', label: 'Zoom out', group: 'View', keys: 'Mod+-' },
  { id: 'zoomReset', label: 'Reset zoom', group: 'View', keys: 'Mod+0' },
  { id: 'fullscreen', label: 'Full screen', group: 'View', keys: 'F11' },
  { id: 'newThread', label: 'New thread', group: 'Threads', keys: 'Mod+N' },
  { id: 'newThread2', label: 'New thread (alt)', group: 'Threads', keys: 'Mod+Shift+O' },
  { id: 'newStandalone', label: 'New thread without project', group: 'Threads', keys: 'Mod+Alt+O' },
  { id: 'quickChat', label: 'Quick chat', group: 'Threads', keys: 'Mod+Alt+N' },
  { id: 'archive', label: 'Archive thread', group: 'Threads', keys: 'Mod+Shift+A' },
  { id: 'markUnread', label: 'Mark unread', group: 'Threads', keys: 'Mod+Shift+U' },
  { id: 'pin', label: 'Pin/unpin thread', group: 'Threads', keys: 'Mod+Alt+P' },
  { id: 'rename', label: 'Rename thread', group: 'Threads', keys: 'Mod+Alt+R' },
  { id: 'sideChat', label: 'Side chat', group: 'Threads', keys: 'Mod+Alt+S' },
  { id: 'find', label: 'Find in thread', group: 'Threads', keys: 'Mod+F' },
  { id: 'prevThread', label: 'Previous thread', group: 'Threads', keys: 'Ctrl+Shift+Tab' },
  { id: 'nextThread', label: 'Next thread', group: 'Threads', keys: 'Ctrl+Tab' },
  { id: 'nextAttention', label: 'Next thread needing attention', group: 'Threads', keys: 'Mod+Alt+A' },
  { id: 'clearUnread', label: 'Clear all unread', group: 'Threads', keys: 'Shift+Escape' },
  { id: 'activity', label: 'Activity view', group: 'Threads', keys: 'Mod+Alt+U' },
  { id: 'modelPicker', label: 'Model picker', group: 'Composer', keys: 'Ctrl+Shift+M' },
  { id: 'projectPicker', label: 'Project picker', group: 'Composer', keys: 'Mod+Alt+Shift+O' },
  { id: 'interrupt', label: 'Stop the running turn', group: 'Composer', keys: 'Escape' },
  { id: 'runAction1', label: 'Run environment action 1', group: 'Project', keys: 'Mod+Shift+D' },
  { id: 'copyDeepLink', label: 'Copy thread deep link', group: 'Threads', keys: 'Mod+Alt+L' },
  { id: 'copyThreadId', label: 'Copy thread id', group: 'Threads', keys: 'Mod+Alt+C' },
  { id: 'copyCwd', label: 'Copy working directory', group: 'Threads', keys: 'Mod+Shift+C' },
  { id: 'undo', label: 'Undo last action (archive, pin, rename)', group: 'General', keys: 'Mod+Z' },
  { id: 'quit', label: 'Quit', group: 'General', keys: 'Mod+Q' },
  ...Array.from({ length: 9 }, (_, i) => ({ id: `goto${i + 1}`, label: `Go to thread ${i + 1}`, group: 'Navigation', keys: `Mod+${i + 1}` })),
]

const isMac = typeof navigator !== 'undefined' && /Mac/.test(navigator.platform)

/** Normalize a KeyboardEvent to "Mod+Shift+K" form. */
export function eventToKeys(e: KeyboardEvent): string {
  const parts: string[] = []
  const mod = isMac ? e.metaKey : e.ctrlKey
  if (mod) parts.push('Mod')
  if (isMac && e.ctrlKey) parts.push('Ctrl')
  if (!isMac && e.metaKey) parts.push('Meta')
  if (e.altKey) parts.push('Alt')
  if (e.shiftKey) parts.push('Shift')
  let k = e.key
  if (k === ' ') k = 'Space'
  else if (k.length === 1) k = k.toUpperCase()
  if (e.code === 'Backquote') k = '`'
  if (e.code === 'Comma') k = ','
  if (e.code === 'Slash') k = '/'
  if (e.code === 'BracketLeft') k = '['
  if (e.code === 'BracketRight') k = ']'
  if (e.code === 'Equal') k = '='
  if (e.code === 'Minus') k = '-'
  if (/^Digit\d$/.test(e.code)) k = e.code.slice(5)
  if (['Control', 'Shift', 'Alt', 'Meta'].includes(e.key)) return parts.join('+')
  parts.push(k)
  return parts.join('+')
}

/** Canonicalize a binding string ("Ctrl+`" means Mod on Win/Linux). */
export function canon(keys: string): string {
  return keys
    .split('+')
    .map((p) => (p === 'Ctrl' && !isMac ? 'Mod' : p === 'Cmd' ? 'Mod' : p))
    .map((p) => (p.length === 1 ? p.toUpperCase() : p))
    .join('+')
}

export function displayKeys(keys: string): string {
  return keys
    .split('+')
    .map((p) => (p === 'Mod' ? (isMac ? '⌘' : 'Ctrl') : p === 'Alt' && isMac ? '⌥' : p === 'Shift' && isMac ? '⇧' : p))
    .join(isMac ? '' : '+')
}

export function bindings(): Record<string, string> {
  const overrides = useApp.getState().settings?.shortcuts ?? {}
  const out: Record<string, string> = {}
  for (const s of SHORTCUTS) out[s.id] = overrides[s.id] ?? s.keys
  return out
}

/** Commands that never fire while typing in a text field. */
const TEXT_KEYS = new Set(['undo'])

/** Global key handler: maps key presses to command ids via `handlers`. */
export function useShortcuts(handlers: Record<string, () => void>): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const pressed = eventToKeys(e)
      if (!pressed) return
      const b = bindings()
      for (const [id, keys] of Object.entries(b)) {
        if (canon(keys) === canon(pressed) && handlers[id]) {
          // let text fields keep plain keys
          const t = e.target as HTMLElement
          const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)
          if (typing && !/Mod|Alt|Ctrl/.test(pressed) && pressed !== 'Escape' && pressed !== 'F11') continue
          // text editing keeps its own undo
          if (typing && TEXT_KEYS.has(id)) continue
          e.preventDefault()
          handlers[id]()
          return
        }
      }
    }
    // the command palette runs shortcut commands by id
    const onCmd = (e: Event) => handlers[(e as CustomEvent<string>).detail]?.()
    window.addEventListener('keydown', onKey)
    window.addEventListener('odex:shortcut', onCmd)
    return () => {
      window.removeEventListener('keydown', onKey)
      window.removeEventListener('odex:shortcut', onCmd)
    }
  }, [handlers])
}
