import { X } from 'lucide-react'
import { useApp } from '@/store/app'
import { TerminalPanel } from '@/panels/TerminalPanel'

export function BottomPanel({ height }: { height: number }) {
  const setUi = useApp((s) => s.setUi)
  return (
    <div className="bottom-panel" style={{ height, position: 'relative' }} aria-label="Terminal panel">
      <button className="icon-btn sm" style={{ position: 'absolute', right: 6, top: 6, zIndex: 2 }} aria-label="Close panel" title="Close (Ctrl+J)" onClick={() => setUi({ bottomOpen: false })}>
        <X size={13} />
      </button>
      <TerminalPanel />
    </div>
  )
}
