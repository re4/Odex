import { useCallback, useEffect, useMemo, useState } from 'react'
import { Keyboard, RotateCcw, Search, X } from 'lucide-react'
import { useApp } from '@/store/app'
import { confirmDialog } from '@/lib/actions'
import { SHORTCUTS, canon, displayKeys, eventToKeys } from '@/lib/shortcuts'
import { Section } from '@/views/settings/ConfigSettings'

const MODIFIERS = ['Mod', 'Ctrl', 'Meta', 'Alt', 'Shift']

/** True when the combo is only modifiers (still being pressed). */
function modifiersOnly(keys: string): boolean {
  return keys.split('+').every((p) => MODIFIERS.includes(p))
}

function Keys({ keys }: { keys: string }) {
  if (!keys) return <span className="subtle">Not set</span>
  const parts = displayKeys(keys).split(/(?<=.)\+(?=.)/)
  return (
    <span className="sx-keys">
      {parts.map((p, i) => (
        <kbd key={i}>{p}</kbd>
      ))}
    </span>
  )
}

/**
 * Capture the next key combo anywhere in the window (before the global
 * shortcut handler sees it). Esc cancels; Backspace/Delete clears.
 */
function useKeyRecorder(active: boolean, onDone: (r: { keys: string } | { cancel: true } | { clear: true }) => void) {
  useEffect(() => {
    if (!active) return
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault()
      e.stopPropagation()
      e.stopImmediatePropagation()
      const keys = eventToKeys(e)
      if (keys === 'Escape') return onDone({ cancel: true })
      if (keys === 'Backspace' || keys === 'Delete') return onDone({ clear: true })
      if (!keys || modifiersOnly(keys)) return
      onDone({ keys })
    }
    const swallow = (e: KeyboardEvent) => {
      e.preventDefault()
      e.stopPropagation()
    }
    // clicking anywhere but the recording control cancels
    const onDown = (e: MouseEvent) => {
      if (!(e.target as HTMLElement | null)?.closest?.('[data-recording="true"]')) onDone({ cancel: true })
    }
    window.addEventListener('keydown', onKey, true)
    window.addEventListener('keyup', swallow, true)
    window.addEventListener('mousedown', onDown, true)
    return () => {
      window.removeEventListener('keydown', onKey, true)
      window.removeEventListener('keyup', swallow, true)
      window.removeEventListener('mousedown', onDown, true)
    }
  }, [active, onDone])
}

/** Store a binding override; `null` (or the default combo) removes it, '' disables the shortcut. */
function saveBinding(id: string, keys: string | null): void {
  const def = SHORTCUTS.find((s) => s.id === id)!.keys
  const next = { ...(useApp.getState().settings?.shortcuts ?? {}) }
  if (keys == null || (keys && canon(keys) === canon(def))) delete next[id]
  else next[id] = keys
  void useApp.getState().setSettings({ shortcuts: next })
}

export function ShortcutsSettings() {
  const overrides = useApp((s) => s.settings?.shortcuts)
  const [recording, setRecording] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [keyQuery, setKeyQuery] = useState<string | null>(null)
  const [searchByKeys, setSearchByKeys] = useState(false)

  const current = useMemo(() => {
    const o = overrides ?? {}
    return Object.fromEntries(SHORTCUTS.map((s) => [s.id, o[s.id] ?? s.keys])) as Record<string, string>
  }, [overrides])

  const conflicts = useMemo(() => {
    const by = new Map<string, string[]>()
    for (const s of SHORTCUTS) {
      const k = current[s.id]
      if (!k) continue
      const c = canon(k)
      by.set(c, [...(by.get(c) ?? []), s.id])
    }
    const out: Record<string, string[]> = {}
    for (const ids of by.values()) if (ids.length > 1) for (const id of ids) out[id] = ids.filter((x) => x !== id)
    return out
  }, [current])

  const onRecorded = useCallback(
    (r: { keys: string } | { cancel: true } | { clear: true }) => {
      const id = recording
      setRecording(null)
      if (!id) return
      if (id === '__search__') {
        if ('keys' in r) setKeyQuery(r.keys)
        else if (!('cancel' in r)) setKeyQuery(null)
        return
      }
      if ('keys' in r) saveBinding(id, r.keys)
      else if ('clear' in r) saveBinding(id, '')
    },
    [recording],
  )
  useKeyRecorder(recording != null, onRecorded)

  const q = query.trim().toLowerCase()
  const visible = SHORTCUTS.filter((s) => {
    if (keyQuery) return !!current[s.id] && canon(current[s.id]) === canon(keyQuery)
    return !q || s.label.toLowerCase().includes(q) || s.group.toLowerCase().includes(q) || displayKeys(current[s.id]).toLowerCase().includes(q)
  })
  const groups = [...new Set(visible.map((s) => s.group))]
  const customCount = Object.keys(overrides ?? {}).length
  const label = (id: string) => SHORTCUTS.find((s) => s.id === id)?.label ?? id

  return (
    <div className="sx-panel">
      <Section desc="Click a shortcut and press the new keys. Esc cancels, Backspace removes the shortcut.">
        <div className="sx-filters">
          <div className="sx-search">
            <Search size={13} aria-hidden />
            {searchByKeys ? (
              <button data-recording={recording === '__search__'} className={`input row ${recording === '__search__' ? 'sx-bind recording' : ''}`} style={{ textAlign: 'left', cursor: 'pointer', gap: 6 }} onClick={() => setRecording('__search__')} aria-label="Keys to search for">
                {recording === '__search__' ? 'Press keys…' : keyQuery ? <Keys keys={keyQuery} /> : <span className="subtle">Click, then press keys</span>}
              </button>
            ) : (
              <input className="input" placeholder="Search shortcuts" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search shortcuts" />
            )}
          </div>
          <button
            className={`btn btn-sm ${searchByKeys ? 'btn-primary' : ''}`}
            aria-pressed={searchByKeys}
            onClick={() => {
              const on = !searchByKeys
              setSearchByKeys(on)
              setKeyQuery(null)
              setRecording(on ? '__search__' : null)
            }}
            title="Find a shortcut by pressing its keys"
          >
            <Keyboard size={13} /> Search by keys
          </button>
          {keyQuery && (
            <button className="icon-btn sm" aria-label="Clear key search" onClick={() => setKeyQuery(null)}>
              <X size={13} />
            </button>
          )}
          <button
            className="btn btn-sm"
            disabled={customCount === 0}
            title={customCount ? `${customCount} changed` : 'All shortcuts use their defaults'}
            onClick={async () => {
              if (await confirmDialog('Reset all shortcuts', `Restore the default for ${customCount} changed shortcut(s)?`, 'Reset all')) void useApp.getState().setSettings({ shortcuts: {} })
            }}
          >
            <RotateCcw size={13} /> Reset all
          </button>
        </div>
      </Section>

      {visible.length === 0 && (
        <div className="sx-list">
          <div className="sx-list-empty">{keyQuery ? `Nothing is bound to ${displayKeys(keyQuery)}.` : 'No shortcuts match.'}</div>
        </div>
      )}

      {groups.map((g) => (
        <Section key={g} title={g}>
          <div className="sx-list" role="list" aria-label={`${g} shortcuts`}>
            {visible
              .filter((s) => s.group === g)
              .map((s) => {
                const keys = current[s.id]
                const custom = overrides?.[s.id] != null
                const isRec = recording === s.id
                return (
                  <div key={s.id} className="sx-list-row" role="listitem" aria-label={s.label}>
                    <div className="grow" style={{ minWidth: 0 }}>
                      <div className="small">{s.label}</div>
                      {conflicts[s.id] && <div className="sx-conflict">Also used by {conflicts[s.id].map(label).join(', ')}</div>}
                      {custom && <div className="xs subtle">Default: {displayKeys(s.keys)}</div>}
                    </div>
                    <button data-recording={isRec} className={`btn btn-sm sx-bind ${isRec ? 'recording' : ''} ${custom ? 'custom' : ''}`} aria-label={`Change shortcut for ${s.label}`} onClick={() => setRecording(isRec ? null : s.id)}>
                      {isRec ? 'Press keys…' : <Keys keys={keys} />}
                    </button>
                    <button className="icon-btn sm" aria-label={`Reset shortcut for ${s.label}`} title="Reset to default" style={{ visibility: custom ? 'visible' : 'hidden' }} onClick={() => saveBinding(s.id, null)}>
                      <RotateCcw size={12} />
                    </button>
                  </div>
                )
              })}
          </div>
        </Section>
      ))}
    </div>
  )
}
