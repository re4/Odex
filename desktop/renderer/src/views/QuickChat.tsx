import { useEffect, useRef, type ReactNode } from 'react'
import { ExternalLink, Pin, PinOff, Plus, Zap } from 'lucide-react'
import { useApp } from '@/store/app'
import * as A from '@/lib/actions'
import { ThreadView } from '@/views/ThreadView'
import { ServerRequests } from '@/views/ServerRequests'
import '@/styles/quickchat.css'

/**
 * The Quick Chat window (`?quickchat=1`): a small window with only the thread
 * view and composer of a new projectless `quickChat` thread.
 */
export function QuickChat({ children }: { children?: ReactNode }) {
  const engine = useApp((s) => s.engine.state)
  const tid = useApp((s) => s.selectedThreadId)
  const onTop = useApp((s) => !!s.settings?.quickChatOnTop)
  const mac = window.odex.platform === 'darwin'
  const started = useRef(false)

  // one fresh quick chat per window
  useEffect(() => {
    if (engine !== 'ready' || started.current) return
    started.current = true
    if (!useApp.getState().selectedThreadId) void A.createThread({ kind: 'quickChat' })
  }, [engine])

  const togglePin = () => {
    void window.odex.win.alwaysOnTop(!onTop)
    void useApp.getState().setSettings({ quickChatOnTop: !onTop })
  }

  return (
    <div className="app quick-chat">
      <div className={`titlebar ${mac ? 'mac' : ''}`}>
        <div className="brand">
          <Zap size={14} color="var(--accent)" />
          Quick chat
        </div>
        <span className="spacer" />
        <button className="icon-btn" aria-label="New quick chat" title="New quick chat" onClick={() => void A.createThread({ kind: 'quickChat' })}>
          <Plus size={15} />
        </button>
        <button
          className={`icon-btn ${onTop ? 'active' : ''}`}
          aria-label="Keep on top"
          aria-pressed={onTop}
          title={onTop ? 'Stop keeping this window on top' : 'Keep this window on top'}
          onClick={togglePin}
        >
          {onTop ? <Pin size={15} /> : <PinOff size={15} />}
        </button>
        <button className="icon-btn" aria-label="Open in main window" title="Open this chat in the main window" disabled={!tid} onClick={() => tid && void window.odex.win.openInMain(tid)}>
          <ExternalLink size={15} />
        </button>
      </div>
      <div className="main">
        <div className="center">
          <div className="center-main">
            {tid ? (
              <ThreadView />
            ) : (
              <div className="empty">
                <span className="spinner" /> {engine === 'ready' ? 'Starting a chat…' : 'Waiting for the engine…'}
              </div>
            )}
          </div>
        </div>
      </div>
      <ServerRequests />
      {children}
    </div>
  )
}
