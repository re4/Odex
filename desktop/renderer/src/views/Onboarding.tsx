import { useEffect, useState } from 'react'
import { CheckCircle2, CircleAlert, FolderPlus, Loader2 } from 'lucide-react'
import type { DoctorReport, PermissionMode, ProviderTestResult } from '@shared/index'
import { useApp } from '@/store/app'
import { call } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Modal } from '@/components/ui'
import { DoctorReportView } from '@/views/settings/ModelsSettings'

type Step = 'endpoint' | 'roles' | 'doctor' | 'permissions' | 'project'
const STEPS: Step[] = ['endpoint', 'roles', 'doctor', 'permissions', 'project']
const DEFAULT_URL = 'http://localhost:8000/v1'

export function Onboarding() {
  const setUi = useApp((s) => s.setUi)
  const models = useApp((s) => s.models)
  const projects = useApp((s) => s.projects)
  const [step, setStep] = useState<Step>('endpoint')
  const [baseUrl, setBaseUrl] = useState(DEFAULT_URL)
  const [apiKey, setApiKey] = useState('')
  const [providerId, setProviderId] = useState('local')
  const [testing, setTesting] = useState(false)
  const [test, setTest] = useState<ProviderTestResult | null>(null)
  const [mainModel, setMainModel] = useState('')
  const [utility, setUtility] = useState('')
  const [reports, setReports] = useState<DoctorReport[] | null>(null)
  const [doctorRunning, setDoctorRunning] = useState(false)
  const [perm, setPerm] = useState<PermissionMode>('auto')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const runTest = async (raw = baseUrl, key = apiKey) => {
    const url = A.normalizeBaseUrl(raw)
    if (url !== raw) setBaseUrl(url)
    setTesting(true)
    setError(null)
    try {
      const r = await call('provider/test', { provider: { base_url: url, headers: {}, query_params: {} }, apiKey: key || null })
      setTest(r)
      return r
    } catch (e) {
      setTest({ ok: false, latencyMs: 0, models: [], error: (e as Error).message })
      return null
    } finally {
      setTesting(false)
    }
  }

  // detect a local vLLM on first open
  useEffect(() => {
    void runTest(DEFAULT_URL, '')
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const close = () => {
    void useApp.getState().setSettings({ onboarded: true })
    setUi({ onboardingOpen: false })
  }

  const saveEndpoint = async () => {
    setBusy(true)
    setError(null)
    try {
      await call('provider/upsert', {
        id: providerId,
        provider: { name: providerId === 'local' ? 'Local vLLM' : providerId, base_url: A.normalizeBaseUrl(baseUrl), headers: {}, query_params: {}, enabled: true },
        apiKey: apiKey || null,
      })
      await useApp.getState().refreshModels(true)
      const first = test?.models[0]?.id
      if (first) {
        setMainModel(`${providerId}:${first}`)
        setUtility(`${providerId}:${first}`)
      }
      setStep('roles')
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  const saveRoles = async () => {
    setBusy(true)
    try {
      const edits = [{ keyPath: 'roles.main', value: mainModel }]
      if (utility && utility !== mainModel) edits.push({ keyPath: 'roles.utility', value: utility })
      await call('config/write', { edits })
      await useApp.getState().refreshModels()
      setStep('doctor')
      setDoctorRunning(true)
      const r = await call('doctor/run', { providerId, model: mainModel.split(':').slice(1).join(':') || null, quick: true }).catch((e: Error) => {
        setError(e.message)
        return null
      })
      setReports(r?.reports ?? [])
    } finally {
      setDoctorRunning(false)
      setBusy(false)
    }
  }

  const savePerm = async () => {
    await call('config/write', { edits: [{ keyPath: 'permission_mode', value: perm }] })
    setStep('project')
  }

  const idx = STEPS.indexOf(step)
  const discovered = test?.models ?? []
  const modelOptions = models.length ? models.map((m) => ({ key: m.key, label: `${m.displayName} (${m.contextWindow.toLocaleString()} ctx)` })) : discovered.map((m) => ({ key: `${providerId}:${m.id}`, label: m.id }))

  return (
    <Modal title="Set up Odex" onClose={close} wide>
      <div className="row xs subtle" style={{ marginBottom: 12, gap: 4 }}>
        {STEPS.map((s, i) => (
          <span key={s} className={`badge ${i === idx ? 'accent' : ''}`} style={{ opacity: i > idx ? 0.5 : 1 }}>
            {i + 1}. {s === 'endpoint' ? 'Endpoint' : s === 'roles' ? 'Models' : s === 'doctor' ? 'Doctor' : s === 'permissions' ? 'Permissions' : 'Project'}
          </span>
        ))}
      </div>

      {step === 'endpoint' && (
        <div className="col" style={{ gap: 12 }}>
          <p style={{ margin: 0 }}>Odex talks to an OpenAI-compatible server. Point it at your vLLM endpoint (the URL ends in <code>/v1</code>).</p>
          <div className="field">
            <label htmlFor="ob-url">Base URL</label>
            <input id="ob-url" className="input mono" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} onBlur={() => setBaseUrl(A.normalizeBaseUrl(baseUrl))} placeholder={DEFAULT_URL} />
          </div>
          <div className="field">
            <label htmlFor="ob-key">API key (optional)</label>
            <input id="ob-key" className="input" type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder="Only if vLLM runs with --api-key" autoComplete="off" />
            <span className="hint">Stored encrypted on this machine. Never written to config.toml.</span>
          </div>
          <div className="field">
            <label htmlFor="ob-id">Name</label>
            <input id="ob-id" className="input" value={providerId} onChange={(e) => setProviderId(e.target.value.replace(/[^\w-]/g, ''))} style={{ maxWidth: 200 }} />
          </div>
          <div className="row">
            <button className="btn" onClick={() => void runTest()} disabled={testing}>
              {testing ? <Loader2 size={14} className="spin" /> : null} Test connection
            </button>
            {test && (test.ok ? (
              <span className="row small" style={{ color: 'var(--success)' }}>
                <CheckCircle2 size={14} /> Connected{test.version ? ` · vLLM ${test.version}` : ''} · {test.models.length} model(s) · {test.latencyMs} ms
              </span>
            ) : (
              <span className="row small selectable" style={{ color: 'var(--danger)' }}>
                <CircleAlert size={14} /> {test.error ?? 'Not reachable'}
              </span>
            ))}
          </div>
          {test?.ok && test.models.length > 0 && (
            <div className="xs muted">
              {test.models.map((m) => (
                <div key={m.id} className="mono">
                  {m.id}
                  {m.maxModelLen ? ` · ${m.maxModelLen.toLocaleString()} tokens` : ''}
                </div>
              ))}
            </div>
          )}
          {!test?.ok && (
            <div className="xs subtle">
              No server yet? See docs/vllm-setup.md. A typical start: <code className="selectable">vllm serve Qwen/Qwen3-Coder-30B-A3B-Instruct --enable-auto-tool-choice --tool-call-parser qwen3_coder --enable-prefix-caching</code>
            </div>
          )}
        </div>
      )}

      {step === 'roles' && (
        <div className="col" style={{ gap: 12 }}>
          <p style={{ margin: 0 }}>Choose the model for each role. You can change these any time in Settings → Models.</p>
          <div className="field">
            <label htmlFor="ob-main">Main model (coding agent)</label>
            <select id="ob-main" className="select" value={mainModel} onChange={(e) => setMainModel(e.target.value)}>
              {modelOptions.map((m) => (
                <option key={m.key} value={m.key}>
                  {m.label}
                </option>
              ))}
            </select>
          </div>
          <div className="field">
            <label htmlFor="ob-util">Utility model (titles, commit messages, follow-ups)</label>
            <select id="ob-util" className="select" value={utility} onChange={(e) => setUtility(e.target.value)}>
              {modelOptions.map((m) => (
                <option key={m.key} value={m.key}>
                  {m.label}
                </option>
              ))}
            </select>
            <span className="hint">Compactor, reviewer and vision roles fall back to the main model.</span>
          </div>
        </div>
      )}

      {step === 'doctor' && (
        <div className="col" style={{ gap: 10 }}>
          <p style={{ margin: 0 }}>Doctor checks that your server handles streaming, tool calls and reasoning the way Odex expects.</p>
          {doctorRunning && (
            <div className="row">
              <span className="spinner" /> Running checks…
            </div>
          )}
          {reports?.map((r) => <DoctorReportView key={`${r.providerId}:${r.modelId}`} report={r} />)}
          {reports && reports.length === 0 && !doctorRunning && <div className="muted small">No report. You can run Doctor later from Settings → Models.</div>}
        </div>
      )}

      {step === 'permissions' && (
        <div className="col" style={{ gap: 8 }}>
          <p style={{ margin: 0 }}>How much should the agent do without asking?</p>
          {(
            [
              ['read-only', 'Read only', 'Reads and searches. Asks before every edit or command.'],
              ['auto', 'Auto (recommended)', 'Edits files and runs commands inside a sandbox limited to the workspace. Asks for network access and anything outside.'],
              ['full-access', 'Full access', 'No sandbox, no approvals. Only for trusted, disposable environments.'],
            ] as Array<[PermissionMode, string, string]>
          ).map(([id, label, hint]) => (
            <label key={id} className="card" style={{ padding: 10, display: 'flex', gap: 10, cursor: 'pointer', borderColor: perm === id ? 'var(--accent)' : undefined }}>
              <input type="radio" name="perm" checked={perm === id} onChange={() => setPerm(id)} />
              <div>
                <div style={{ fontWeight: 600 }}>{label}</div>
                <div className="small muted">{hint}</div>
              </div>
            </label>
          ))}
          {useApp.getState().engine.init?.sandbox && (
            <div className="xs subtle">
              Sandbox: {useApp.getState().engine.init!.sandbox.backend}
              {useApp.getState().engine.init!.sandbox.available ? '' : ' (unavailable)'}
              {useApp.getState().engine.init!.sandbox.warning ? ` · ${useApp.getState().engine.init!.sandbox.warning}` : ''}
            </div>
          )}
        </div>
      )}

      {step === 'project' && (
        <div className="col" style={{ gap: 12 }}>
          <p style={{ margin: 0 }}>Add a project folder to start working on code. You can also chat without a project.</p>
          <div>
            <button className="btn btn-primary" onClick={() => void A.addProjectFromDialog()}>
              <FolderPlus size={14} /> Add project folder…
            </button>
          </div>
          {projects.map((p) => (
            <div key={p.id} className="small">
              ✓ {p.name} <span className="subtle mono xs">{p.folders[p.primary] ?? p.folders[0]}</span>
            </div>
          ))}
        </div>
      )}

      {error && (
        <div className="error-box" style={{ marginTop: 10 }}>
          {error}
        </div>
      )}

      <div className="row" style={{ marginTop: 16 }}>
        {idx > 0 && (
          <button className="btn btn-ghost" onClick={() => setStep(STEPS[idx - 1])}>
            Back
          </button>
        )}
        <span className="spacer" />
        <button className="btn btn-ghost" onClick={close}>
          {step === 'project' ? 'Done' : 'Skip setup'}
        </button>
        {step === 'endpoint' && (
          <button className="btn btn-primary" disabled={busy || !baseUrl} onClick={() => void saveEndpoint()}>
            {test?.ok ? 'Continue' : 'Save anyway'}
          </button>
        )}
        {step === 'roles' && (
          <button className="btn btn-primary" disabled={busy || !mainModel} onClick={() => void saveRoles()}>
            Continue
          </button>
        )}
        {step === 'doctor' && (
          <button className="btn btn-primary" disabled={doctorRunning} onClick={() => setStep('permissions')}>
            Continue
          </button>
        )}
        {step === 'permissions' && (
          <button className="btn btn-primary" onClick={() => void savePerm()}>
            Continue
          </button>
        )}
      </div>
    </Modal>
  )
}
