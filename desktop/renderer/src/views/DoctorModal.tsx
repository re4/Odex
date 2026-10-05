import { useEffect, useState } from 'react'
import { RefreshCw } from 'lucide-react'
import type { DoctorReport } from '@shared/index'
import { call } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Modal } from '@/components/ui'
import { DoctorReportView } from '@/views/settings/ModelsSettings'

/** `/doctor`: run Doctor's quick checks for the thread's model and show the report. */
export function DoctorHost() {
  const [req, setReq] = useState<{ model?: string; n: number } | null>(null)
  useEffect(() => {
    let n = 0
    const onRun = (e: Event) => setReq({ model: (e as CustomEvent<{ model?: string }>).detail?.model || undefined, n: ++n })
    window.addEventListener('odex:doctor', onRun)
    return () => window.removeEventListener('odex:doctor', onRun)
  }, [])
  if (!req) return null
  return <DoctorModal key={req.n} model={req.model} onClose={() => setReq(null)} />
}

function DoctorModal({ model, onClose }: { model?: string; onClose: () => void }) {
  const [reports, setReports] = useState<DoctorReport[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [run, setRun] = useState(0)
  useEffect(() => {
    let cancelled = false
    setReports(null)
    setError(null)
    call('doctor/run', { model: model ?? null, quick: true })
      .then((r) => !cancelled && setReports(r.reports))
      .catch((e: Error) => !cancelled && setError(e.message))
    return () => {
      cancelled = true
    }
  }, [model, run])
  return (
    <Modal
      title="Doctor"
      wide
      onClose={onClose}
      footer={
        <>
          <button className="btn btn-ghost" onClick={() => (onClose(), A.openSettings('models'))}>
            Models & Endpoints
          </button>
          <span className="spacer" />
          <button className="btn" disabled={!reports && !error} onClick={() => setRun((x) => x + 1)}>
            <RefreshCw size={13} /> Run again
          </button>
          <button className="btn btn-primary" onClick={onClose}>
            Close
          </button>
        </>
      }
    >
      <div className="col" style={{ gap: 10 }} aria-label="Doctor report">
        <div className="small muted">
          Quick checks for {model ? <b className="mono">{model}</b> : 'every endpoint'}: connectivity, streaming, tool calling and structured output. The full run (with prefix-cache timing) is in Settings → Models & Endpoints.
        </div>
        {error && (
          <div className="error-box" role="alert">
            {error}
          </div>
        )}
        {!reports && !error && (
          <div className="row small muted">
            <span className="spinner" /> Running checks…
          </div>
        )}
        {reports?.length === 0 && <div className="small muted">No endpoint to check. Add one in Settings → Models & Endpoints.</div>}
        {reports?.map((r) => <DoctorReportView key={`${r.providerId}:${r.modelId}`} report={r} />)}
      </div>
    </Modal>
  )
}
