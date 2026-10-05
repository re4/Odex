import { useEffect, useState } from 'react'
import { FolderOpen, LayoutPanelLeft, RefreshCw, Wand2 } from 'lucide-react'
import { PROTOCOL_VERSION } from '@shared/index'
import { useApp } from '@/store/app'
import { toast } from '@/lib/rpc'
import { confirmDialog } from '@/lib/actions'
import { Row } from '@/views/settings/GeneralSettings'
import { Callout, Section } from '@/views/settings/ConfigSettings'
import pkg from '../../../../package.json'

interface AppInfo {
  version: string
  platform: string
  odexHome: string
  isPackaged: boolean
  logs: string
}

export function AboutSettings() {
  const engine = useApp((s) => s.engine)
  const providers = useApp((s) => s.providers)
  const [info, setInfo] = useState<AppInfo | null>(null)
  useEffect(() => {
    void window.odex.app
      .info()
      .then((i: AppInfo) => setInfo(i))
      .catch(() => {})
  }, [])

  const init = engine.init
  const home = init?.odexHome ?? info?.odexHome ?? ''
  const protocolMismatch = !!init && init.protocolVersion.split('.')[0] !== PROTOCOL_VERSION.split('.')[0]

  return (
    <div className="sx-panel">
      <Section title="Version">
        <dl className="sx-kv">
          <dt>Odex</dt>
          <dd>
            {info?.isPackaged ? info.version : `${pkg.version} (development build)`} · {info?.platform ?? window.odex.platform}
          </dd>
          <dt>Engine</dt>
          <dd>{init ? `${init.serverName} ${init.serverVersion}` : `not running (${engine.state})`}</dd>
          <dt>Protocol</dt>
          <dd>
            {init ? init.protocolVersion : '…'} <span className="subtle">(app expects {PROTOCOL_VERSION})</span>
          </dd>
          <dt>Engine state</dt>
          <dd>
            <span className={`badge ${engine.state === 'ready' ? 'success' : engine.state === 'failed' ? 'danger' : 'warning'}`}>{engine.state}</span>
            {engine.error && <span className="small muted selectable"> {engine.error}</span>}
          </dd>
        </dl>
        {protocolMismatch && (
          <div style={{ marginTop: 8 }}>
            <Callout kind="warning">The engine speaks a different protocol version than this app. Rebuild or reinstall so both match.</Callout>
          </div>
        )}
      </Section>

      <Section title="Data">
        <dl className="sx-kv">
          <dt>Odex home</dt>
          <dd className="row" style={{ gap: 6 }}>
            <code className="selectable small ellipsis grow" title={home}>
              {home || '…'}
            </code>
            {home && (
              <button className="btn btn-sm" onClick={() => void window.odex.shell.openPath(home)}>
                <FolderOpen size={13} /> Open
              </button>
            )}
          </dd>
          <dt>Logs</dt>
          <dd className="row" style={{ gap: 6 }}>
            <code className="selectable small ellipsis grow" title={info?.logs}>
              {info?.logs ?? '…'}
            </code>
            {info && (
              <button className="btn btn-sm" onClick={() => void window.odex.shell.openPath(info.logs)}>
                <FolderOpen size={13} /> Open logs folder
              </button>
            )}
          </dd>
          <dt>Sandbox</dt>
          <dd>
            {init ? (
              <>
                {init.sandbox.backend} · {init.sandbox.available ? 'available' : 'unavailable'} · {init.sandbox.networkIsolated ? 'network isolated' : 'network not isolated'}
              </>
            ) : (
              '…'
            )}
          </dd>
        </dl>
      </Section>

      <Section title="Privacy">
        <Callout kind="success">
          <div style={{ marginBottom: 4 }}>
            <b>No telemetry.</b>
          </div>
          Odex sends nothing about you or your usage anywhere. It only talks to the model endpoints you configure
          {providers.length ? ` (${providers.map((p) => p.name).join(', ')})` : ''}, the MCP servers you add, and the websites you or the agent open in the built-in browser. Threads, memories, usage stats and settings stay in the Odex home folder on this computer.
        </Callout>
      </Section>

      <Section title="Maintenance">
        <Row label="Run setup again" hint="Walk through endpoint, model and permission setup.">
          <button className="btn btn-sm" onClick={() => useApp.getState().setUi({ onboardingOpen: true })}>
            <Wand2 size={13} /> Open setup
          </button>
        </Row>
        <Row label="Restart the engine" hint="Stops running turns; threads and settings are kept.">
          <button
            className="btn btn-sm"
            onClick={async () => {
              if (!(await confirmDialog('Restart the engine', 'Running turns are stopped. Threads, settings and history are kept.', 'Restart'))) return
              await window.odex.restartEngine()
              toast('Engine restarted', 'success')
            }}
          >
            <RefreshCw size={13} /> Restart engine
          </button>
        </Row>
        <Row label="Reset UI layout" hint="Restore panel sizes, open panels and the sidebar to their defaults. Settings are not touched.">
          <button
            className="btn btn-sm"
            onClick={async () => {
              if (!(await confirmDialog('Reset UI layout', 'Restore the default window layout? The window reloads.', 'Reset layout'))) return
              try {
                localStorage.removeItem('odex.ui')
              } catch {}
              location.reload()
            }}
          >
            <LayoutPanelLeft size={13} /> Reset layout
          </button>
        </Row>
      </Section>
    </div>
  )
}
