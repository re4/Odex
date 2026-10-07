import { useState } from 'react'
import { Download, X } from 'lucide-react'
import { isRunning, useApp } from '@/store/app'
import { Modal } from '@/components/ui'
import { Callout } from '@/views/settings/ConfigSettings'

/**
 * Once an update has downloaded, ask to restart and install it. After "Later" a banner keeps the
 * offer open (Settings → About and the tray menu have it too). Main window only.
 */
export function UpdatePrompt() {
  const update = useApp((s) => s.update)
  const popout = useApp((s) => s.ui.popout)
  const running = useApp((s) => Object.values(s.threads).filter((t) => isRunning(t.thread)).length)
  // the version the dialog was answered for, and the version whose banner was closed
  const [asked, setAsked] = useState<string | null>(null)
  const [hidden, setHidden] = useState<string | null>(null)
  if (popout || update?.status !== 'downloaded' || !update.version) return null
  const version = update.version
  const install = () => void window.odex.updates.install()
  const notes = (e: React.MouseEvent) => {
    e.preventDefault()
    void window.odex.shell.openExternal(update.releaseUrl)
  }

  if (asked !== version) {
    return (
      <Modal
        title="Update ready"
        onClose={() => setAsked(version)}
        footer={
          <>
            <a href={update.releaseUrl} className="small" style={{ marginRight: 'auto' }} onClick={notes}>
              What’s new
            </a>
            <button className="btn" onClick={() => setAsked(version)}>
              Later
            </button>
            <button className="btn btn-primary" onClick={install}>
              <Download size={13} /> Restart and update
            </button>
          </>
        }
      >
        <p style={{ margin: 0 }}>
          Odex {version} has been downloaded. Restart Odex now to install it? You have {update.currentVersion}.
        </p>
        {running > 0 && (
          <div style={{ marginTop: 10 }}>
            <Callout kind="warning">
              {running === 1 ? 'A thread is' : `${running} threads are`} still running. Restarting stops {running === 1 ? 'it' : 'them'}; threads and history are kept.
            </Callout>
          </div>
        )}
      </Modal>
    )
  }
  if (hidden === version) return null
  return (
    <div className="banner info" role="status">
      <Download size={14} aria-hidden />
      <span className="grow">Odex {version} is ready to install.</span>
      <button className="btn btn-sm" onClick={() => void window.odex.shell.openExternal(update.releaseUrl)}>
        What’s new
      </button>
      <button className="btn btn-sm btn-primary" onClick={install}>
        Restart and update
      </button>
      <button className="icon-btn" aria-label="Hide update banner" title="Hide until next start" onClick={() => setHidden(version)}>
        <X size={14} />
      </button>
    </div>
  )
}
