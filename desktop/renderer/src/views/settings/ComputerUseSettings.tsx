import { useCallback, useEffect, useState } from 'react'
import { Camera, OctagonX, RefreshCw, ShieldCheck } from 'lucide-react'
import type { ComputerUseStatus, ComputerUseToml, WindowInfo } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Toggle } from '@/components/ui'
import { Row, useSetting } from '@/views/settings/GeneralSettings'
import { ListEditor, useConfig } from '@/views/settings/BrowserSettings'
import '@/styles/browser.css'

/**
 * Put something into the composer: go back to the thread (or home) view if
 * needed, wait for the composer to mount, then send the `odex:attach` event.
 */
export async function attachToComposer(detail: Record<string, unknown>): Promise<void> {
  const s = useApp.getState()
  if (s.ui.view !== 'thread' && s.ui.view !== 'home') s.setUi({ view: s.selectedThreadId ? 'thread' : 'home' })
  const composer = () => document.querySelector('textarea[aria-label="Message"]')
  const wasThere = !!composer()
  for (let i = 0; i < 60 && !composer(); i++) await new Promise((r) => setTimeout(r, 50))
  // give a freshly mounted composer a moment to subscribe
  if (!wasThere) await new Promise((r) => setTimeout(r, 80))
  window.dispatchEvent(new CustomEvent('odex:attach', { detail }))
}

function HotkeyInput(props: { value: string; onSave: (v: string) => void; label: string }) {
  const [v, setV] = useState(props.value)
  useEffect(() => setV(props.value), [props.value])
  const save = () => {
    if (v.trim() !== props.value) props.onSave(v.trim())
  }
  return (
    <input
      className="input mono"
      style={{ width: 220 }}
      value={v}
      aria-label={props.label}
      placeholder="Control+Alt+Escape"
      onChange={(e) => setV(e.target.value)}
      onBlur={save}
      onKeyDown={(e) => {
        if (e.key === 'Enter') save()
      }}
    />
  )
}

export function ComputerUseSettings() {
  const [cfg, write] = useConfig()
  const [status, setStatus] = useState<ComputerUseStatus | null>(null)
  const [windows, setWindows] = useState<WindowInfo[] | null>(null)
  const [winError, setWinError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const killSwitch = useApp((s) => s.killSwitch)
  const activeUse = useApp((s) => s.computerUseActive)
  const activeThread = useApp((s) => (s.computerUseActive.threadId ? s.threads[s.computerUseActive.threadId]?.thread : undefined))
  const [killHotkey, setKillHotkey] = useSetting('killSwitchHotkey')
  const [shotHotkey, setShotHotkey] = useSetting('appshotHotkey')

  const refreshStatus = useCallback(() => {
    call('computerUse/status', {})
      .then(setStatus)
      .catch(() => {})
  }, [])
  const refreshWindows = useCallback(() => {
    setWinError(null)
    call('computerUse/windows', {})
      .then((r) => setWindows(r.windows.filter((w) => w.title.trim())))
      .catch((e: Error) => {
        setWindows([])
        setWinError(e.message)
      })
  }, [])

  useEffect(() => {
    refreshStatus()
    refreshWindows()
    void window.odex.app
      .killSwitchState()
      .then((on) => useApp.setState({ killSwitch: on }))
      .catch(() => {})
  }, [refreshStatus, refreshWindows])
  useEffect(refreshStatus, [killSwitch, refreshStatus])

  // empty lists are omitted from the config JSON
  const rawCu: Partial<ComputerUseToml> = cfg?.effective.computer_use ?? {}
  const cu: ComputerUseToml = { ...rawCu, allowed_apps: rawCu.allowed_apps ?? [] }
  const enabled = cu.enabled ?? false
  const requireApproval = cu.require_approval ?? true
  const preferBackground = cu.prefer_background ?? true
  const killed = killSwitch || !!status?.killed

  const setConfig = async (key: keyof ComputerUseToml, value: boolean | string[]) => {
    await write(`computer_use.${key}`, value)
    refreshStatus()
    if (key === 'allowed_apps') refreshWindows()
  }

  const toggleApp = (app: string, allow: boolean) => {
    const next = allow ? [...new Set([...cu.allowed_apps, app])] : cu.allowed_apps.filter((a) => a.toLowerCase() !== app.toLowerCase())
    void setConfig('allowed_apps', next)
  }

  const appshot = async (w?: WindowInfo) => {
    setBusy(w?.handle ?? 'front')
    try {
      const r = await call('appshot/capture', { handle: w?.handle ?? null, includeUiTree: true })
      const shot = r.appshot
      await attachToComposer({ type: 'appshot', title: shot.title, app: shot.app, imageUrl: shot.imageUrl, uiTree: shot.uiTree ?? null })
      toast(`Appshot of ${shot.title || shot.app} added to your message`)
    } catch (e) {
      toast(`Appshot failed: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(null)
    }
  }

  return (
    <div className="col" style={{ gap: 0 }}>
      <div className={`card cu-status ${killed ? 'killed' : ''}`} role="status" aria-label="Computer use status">
        <div className="row" style={{ gap: 8 }}>
          <span className={`dot ${killed ? 'danger' : !status?.supported ? 'warning' : enabled ? 'success' : ''}`} />
          <b className="grow">
            {killed ? 'Kill switch engaged: computer and browser actions are stopped' : !status ? 'Checking…' : !status.supported ? 'Not available on this platform yet' : enabled ? 'Computer use is on' : 'Computer use is off'}
          </b>
          {status && <span className="badge">{status.platform}</span>}
          {status && <span className={`badge ${status.supported ? 'success' : 'warning'}`}>{status.supported ? 'supported' : 'unsupported'}</span>}
        </div>
        {activeUse.active && (
          <div className="small">
            Controlling <b>{activeUse.app || 'the computer'}</b>
            {activeThread ? ` for “${activeThread.name || activeThread.preview || 'a thread'}”` : ''}
            {activeUse.takeover ? ' with real mouse and keyboard input' : ' in the background'}.
          </div>
        )}
        {status && status.notes.length > 0 && (
          <ul className="cu-notes small muted">
            {status.notes.map((n, i) => (
              <li key={i}>{n}</li>
            ))}
          </ul>
        )}
        <div className="row" style={{ gap: 8 }}>
          {killed ? (
            <button className="btn btn-sm" onClick={() => void window.odex.app.killSwitch(false)}>
              Release kill switch
            </button>
          ) : (
            <button className="btn btn-sm btn-danger" onClick={() => void window.odex.app.killSwitch(true)}>
              <OctagonX size={13} /> Engage kill switch
            </button>
          )}
          <span className="xs subtle">
            Hotkey: <span className="kbd">{killHotkey || 'none'}</span>
          </span>
        </div>
      </div>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Access
      </h3>
      <Row label="Let the agent control apps on this computer" hint="Adds screenshot, UI tree, UI actions, mouse, keyboard, window and clipboard tools to new turns.">
        <Toggle checked={enabled} disabled={!cfg} onChange={(v) => void setConfig('enabled', v)} label="Enable computer use" />
      </Row>
      <Row label="Ask before each action" hint="Recommended. When off, the agent acts on allowed apps without asking (the kill switch still works).">
        <Toggle
          checked={requireApproval}
          disabled={!cfg}
          onChange={async (v) => {
            if (!v && !(await A.confirmDialog('Stop asking before actions?', 'The agent will click, type and control allowed apps without asking first. You can stop it at any time with the kill switch.', 'Stop asking', true))) return
            void setConfig('require_approval', v)
          }}
          label="Ask before each action"
        />
      </Row>
      <Row label="Work in the background when possible" hint="Prefer UI Automation actions that don't move your mouse or steal focus, so you can keep working.">
        <Toggle checked={preferBackground} disabled={!cfg} onChange={(v) => void setConfig('prefer_background', v)} label="Work in the background" />
      </Row>
      <div style={{ padding: '10px 0', borderBottom: '1px solid var(--border)' }}>
        <div>Allowed apps</div>
        <div className="xs subtle">Executable names the agent may control, e.g. notepad.exe. Every other app is off limits.</div>
        <ListEditor label="Allowed apps" items={cu.allowed_apps} onChange={(v) => void setConfig('allowed_apps', v)} placeholder="notepad.exe" empty="No apps allowed yet." normalize={(x) => x.trim()} />
      </div>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Hotkeys
      </h3>
      <Row label="Kill switch" hint="Global hotkey that stops all computer and browser actions at once">
        <HotkeyInput value={killHotkey ?? ''} onSave={(v) => setKillHotkey(v)} label="Kill switch hotkey" />
      </Row>
      <Row label="Appshot" hint="Global hotkey that captures the frontmost window into the composer">
        <HotkeyInput value={shotHotkey ?? ''} onSave={(v) => setShotHotkey(v)} label="Appshot hotkey" />
      </Row>

      <div className="row" style={{ marginTop: 20, gap: 8 }}>
        <h3 className="section-title grow" style={{ margin: 0 }}>
          Windows and appshots
        </h3>
        <button className="btn btn-sm" disabled={!!busy} onClick={() => void appshot()}>
          <Camera size={13} /> {busy === 'front' ? 'Capturing…' : 'Appshot frontmost window'}
        </button>
        <button className="btn btn-sm btn-ghost" onClick={refreshWindows} aria-label="Refresh windows">
          <RefreshCw size={13} /> Refresh
        </button>
      </div>
      <p className="xs subtle" style={{ margin: '6px 0' }}>
        An appshot attaches a window's screenshot and accessible text to your message, so the agent can see what you see. Appshots work for any window; control is limited to allowed apps.
      </p>
      {winError && <div className="small" style={{ color: 'var(--danger)' }}>{winError}</div>}
      {windows === null && <div className="small subtle">Loading windows…</div>}
      {windows && windows.length === 0 && !winError && <div className="small subtle">No windows found.</div>}
      {windows && windows.length > 0 && (
        <table className="cu-windows small" aria-label="Open windows">
          <tbody>
            {windows.map((w) => (
              <tr key={w.handle}>
                <td className="cu-win-title">
                  <div className="ellipsis" title={w.title}>
                    {w.title}
                  </div>
                  <div className="xs subtle mono ellipsis">
                    {w.app}
                    {w.minimized ? ' · minimized' : ''}
                    {w.focused ? ' · focused' : ''}
                  </div>
                </td>
                <td style={{ whiteSpace: 'nowrap' }}>
                  {w.allowed ? (
                    <button className="btn btn-sm btn-ghost" title="Remove from allowed apps" onClick={() => toggleApp(w.app, false)}>
                      <span className="badge success">allowed</span>
                    </button>
                  ) : (
                    <button className="btn btn-sm btn-ghost" disabled={!cfg || !w.app} onClick={() => toggleApp(w.app, true)}>
                      Allow app
                    </button>
                  )}
                </td>
                <td style={{ whiteSpace: 'nowrap', textAlign: 'right' }}>
                  <button className="btn btn-sm" disabled={!!busy} onClick={() => void appshot(w)} aria-label={`Appshot ${w.title}`}>
                    <Camera size={13} /> {busy === w.handle ? 'Capturing…' : 'Appshot'}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <div className="card cu-safety small" style={{ marginTop: 20 }}>
        <div className="row" style={{ gap: 6 }}>
          <ShieldCheck size={14} color="var(--success)" />
          <b>How computer use stays safe</b>
        </div>
        <ul className="muted">
          <li>It is off until you turn it on, and the agent can only control the apps you allow.</li>
          <li>Each action asks for your approval unless you turn that off.</li>
          <li>It prefers background UI Automation; when it must use the real mouse or keyboard, a red border shows on screen.</li>
          <li>The kill switch ({killHotkey || 'no hotkey'}) or the tray menu stops every computer and browser action immediately.</li>
          <li>It never types into password fields or touches UAC or sign-in prompts; it asks you to do those steps.</li>
          <li>Every action is logged in the thread with screenshots.</li>
        </ul>
      </div>
    </div>
  )
}
