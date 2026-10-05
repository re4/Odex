import { useCallback, useEffect, useState } from 'react'
import { FilePlus2, FolderPlus, Lock, Plus, RefreshCw, RotateCcw, Save, ShieldAlert, ShieldCheck, Trash2, Zap } from 'lucide-react'
import type { ApprovalPolicy, PermissionMode, SandboxMode, SandboxStatus } from '@shared/index'
import { call, toast } from '@/lib/rpc'
import { confirmDialog, promptText } from '@/lib/actions'
import { Toggle } from '@/components/ui'
import { Row } from '@/views/settings/GeneralSettings'
import { Callout, SaveState, Section, joinPath, readText, useAutosave, useEngineConfig, useOdexHome } from '@/views/settings/ConfigSettings'

const MODES: Array<{ id: PermissionMode; title: string; desc: string; icon: typeof Lock }> = [
  { id: 'read-only', title: 'Read only', desc: 'Reads and searches freely. Every edit and every command that is not known to be read-only asks first.', icon: Lock },
  { id: 'auto', title: 'Auto', desc: 'Edits the workspace and runs commands in the sandbox. Asks before network access, escalation or anything outside the workspace.', icon: ShieldCheck },
  { id: 'full-access', title: 'Full access', desc: 'No sandbox and no approvals. Commands run with your full user rights.', icon: Zap },
]

const APPROVAL_POLICIES: ApprovalPolicy[] = ['untrusted', 'on-failure', 'on-request', 'never']
const SANDBOX_MODES: SandboxMode[] = ['read-only', 'workspace-write', 'danger-full-access']

const RULES_EXAMPLE = `# Rules are TOML files in the rules folder, loaded in name order.
[[rule]]
prefix = ["git", "push"]          # program, then the arguments that must follow
decision = "prompt"               # allow | prompt | forbid
justification = "Pushing publishes commits"

[[rule]]
prefix = ["npm", "test"]
decision = "allow"

[[rule]]
pattern = "Remove-Item .*-Recurse"  # regex over the whole command
decision = "forbid"`

function SandboxCard({ status, onRefresh }: { status: SandboxStatus | null; onRefresh: () => void }) {
  if (!status) return <div className="spinner" aria-label="Loading sandbox status" />
  const restricted = /restricted/i.test(status.backend)
  return (
    <div className="card" style={{ padding: 12 }} aria-label="Sandbox status">
      <div className="row" style={{ gap: 8, marginBottom: 8 }}>
        {status.available ? <ShieldCheck size={16} color="var(--success)" aria-hidden /> : <ShieldAlert size={16} color="var(--warning)" aria-hidden />}
        <b className="grow">Sandbox backend: {status.backend}</b>
        <span className={`badge ${status.available ? 'success' : 'warning'}`}>{status.available ? 'available' : 'unavailable'}</span>
        <span className={`badge ${status.networkIsolated ? 'success' : 'warning'}`}>{status.networkIsolated ? 'network isolated' : 'network not isolated'}</span>
        <button className="icon-btn sm" aria-label="Refresh sandbox status" title="Refresh" onClick={onRefresh}>
          <RefreshCw size={13} />
        </button>
      </div>
      {status.warning && <div className="small muted selectable" style={{ marginBottom: 6 }}>{status.warning}</div>}
      {!status.networkIsolated && (
        <div className="small muted">
          {restricted ? 'The Windows restricted-token sandbox limits writes to the workspace and the writable roots, but it cannot block network access. ' : 'This sandbox does not isolate the network. '}
          So in Auto mode, commands that look like they use the network (downloads, package installs, <code>git fetch</code>/<code>push</code>, <code>ssh</code>, ...) ask for approval unless network access is allowed below.
        </div>
      )}
    </div>
  )
}

function WritableRoots({ roots, onChange }: { roots: string[]; onChange: (r: string[]) => void }) {
  const [draft, setDraft] = useState('')
  const add = (p: string) => {
    const v = p.trim()
    if (!v || roots.includes(v)) return
    onChange([...roots, v])
    setDraft('')
  }
  return (
    <div>
      <div className="sx-list" role="list" aria-label="Writable roots">
        {roots.length === 0 && <div className="sx-list-empty">Only the thread&apos;s workspace is writable.</div>}
        {roots.map((r) => (
          <div key={r} className="sx-list-row" role="listitem">
            <code className="small grow ellipsis selectable" title={r}>
              {r}
            </code>
            <button className="icon-btn sm" aria-label={`Remove ${r}`} onClick={() => onChange(roots.filter((x) => x !== r))}>
              <Trash2 size={13} />
            </button>
          </div>
        ))}
      </div>
      <div className="row" style={{ gap: 6, marginTop: 8 }}>
        <input className="input mono" value={draft} placeholder={window.odex.platform === 'win32' ? 'C:\\path\\to\\folder' : '/path/to/folder'}onChange={(e) => setDraft(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && add(draft)} aria-label="New writable root" />
        <button className="btn btn-sm" disabled={!draft.trim()} onClick={() => add(draft)}>
          <Plus size={13} /> Add
        </button>
        <button
          className="btn btn-sm"
          onClick={async () => {
            const picked = await window.odex.dialog.openFolder({ multi: true })
            const next = [...roots, ...picked.filter((p) => !roots.includes(p))]
            if (picked.length) onChange(next)
          }}
        >
          <FolderPlus size={13} /> Browse…
        </button>
      </div>
    </div>
  )
}

function Rubric({ initial, save }: { initial: string; save: (v: string) => Promise<unknown> }) {
  const [text, setText] = useState(initial)
  const auto = useAutosave(save)
  return (
    <div className="field" style={{ marginTop: 8 }}>
      <div className="row">
        <label htmlFor="auto-review-rubric" className="grow">
          Review rubric
        </label>
        <SaveState state={auto.state} />
      </div>
      <textarea
        id="auto-review-rubric"
        className="textarea"
        rows={3}
        value={text}
        placeholder="e.g. Never allow commands that touch production credentials or delete outside the repo."
        onChange={(e) => {
          setText(e.target.value)
          auto.schedule(e.target.value)
        }}
        onBlur={() => void auto.flush()}
      />
      <span className="hint">Appended to the reviewer prompt.</span>
    </div>
  )
}

function CommandRules() {
  const home = useOdexHome()
  const dir = home ? joinPath(home, 'rules') : ''
  const [files, setFiles] = useState<string[]>([])
  const [selected, setSelected] = useState<string | null>(null)
  const [text, setText] = useState('')
  const [disk, setDisk] = useState('')
  const [changedSinceStart, setChangedSinceStart] = useState(false)

  const list = useCallback(async () => {
    if (!dir) return
    try {
      const r = (await window.odex.fs.read(dir)) as { kind: string; entries?: Array<{ name: string; dir: boolean }> }
      const names = (r.entries ?? []).filter((e) => !e.dir && /\.toml$/i.test(e.name)).map((e) => e.name).sort()
      setFiles(names)
      setSelected((cur) => (cur && names.includes(cur) ? cur : (names[0] ?? null)))
    } catch {
      setFiles([])
    }
  }, [dir])
  useEffect(() => {
    void list()
  }, [list])
  useEffect(() => {
    if (!selected) return
    void readText(joinPath(dir, selected))
      .then((t) => {
        setText(t ?? '')
        setDisk(t ?? '')
      })
      .catch((e: Error) => toast(e.message, 'error'))
  }, [selected, dir])

  const dirty = text !== disk
  const save = async () => {
    if (!selected) return
    try {
      await window.odex.fs.write(joinPath(dir, selected), text)
      setDisk(text)
      setChangedSinceStart(true)
      toast(`${selected} saved. Restart the engine to apply it.`, 'success')
    } catch (e) {
      toast(`Could not save: ${(e as Error).message}`, 'error')
    }
  }
  const create = async () => {
    const raw = await promptText('New rules file name', 'my-rules')
    if (!raw) return
    const name = raw.trim().replace(/\.toml$/i, '').replace(/[^\w.-]/g, '-') + '.toml'
    if (files.includes(name)) {
      setSelected(name)
      return
    }
    await window.odex.fs.write(joinPath(dir, name), '# Command rules: see the syntax help below.\n\n')
    await list()
    setSelected(name)
  }
  const restart = async () => {
    if (!(await confirmDialog('Restart the engine', 'Running turns are stopped. Threads, settings and history are kept.', 'Restart'))) return
    await window.odex.restartEngine()
    setChangedSinceStart(false)
    toast('Engine restarted with the new rules', 'success')
  }

  return (
    <Section
      title="Command rules"
      desc={
        <>
          Rules decide whether a command is allowed, needs approval or is forbidden, on top of the permission mode. They live in <code className="selectable">{dir || '~/.odex/rules'}</code> as <code>*.toml</code> files and load when the engine starts.
        </>
      }
      actions={
        <>
          <button className="btn btn-sm" onClick={() => void create()}>
            <FilePlus2 size={13} /> New file
          </button>
          <button className={`btn btn-sm ${changedSinceStart ? 'btn-primary' : ''}`} onClick={() => void restart()}>
            <RefreshCw size={13} /> Restart engine
          </button>
        </>
      }
    >
      {files.length === 0 ? (
        <div className="sx-list">
          <div className="sx-list-empty">No rules files yet. Only the built-in rules apply: they ask before, or refuse, a few dangerous commands.</div>
        </div>
      ) : (
        <>
          <div className="row" style={{ gap: 8, marginBottom: 8 }}>
            <select className="select" style={{ maxWidth: 260 }} value={selected ?? ''} onChange={(e) => setSelected(e.target.value)} aria-label="Rules file">
              {files.map((f) => (
                <option key={f} value={f}>
                  {f}
                </option>
              ))}
            </select>
            <span className="spacer" />
            <SaveState state={dirty ? 'dirty' : 'idle'} />
            <button className="btn btn-sm" disabled={!dirty} onClick={() => setText(disk)}>
              <RotateCcw size={13} /> Revert
            </button>
            <button className="btn btn-sm btn-primary" disabled={!dirty} onClick={() => void save()}>
              <Save size={13} /> Save
            </button>
          </div>
          <textarea
            className="textarea sx-editor"
            spellCheck={false}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
                e.preventDefault()
                if (dirty) void save()
              }
            }}
            aria-label="Rules file contents"
          />
        </>
      )}
      <details className="sx-help" style={{ marginTop: 8 }}>
        <summary>Rule syntax</summary>
        <pre>{RULES_EXAMPLE}</pre>
        <p style={{ margin: '4px 0' }}>
          <code>prefix[0]</code> is the program name (<code>git.exe</code> and <code>git</code> match the same rule); the other entries must equal the arguments that follow. Wrappers such as <code>bash -c</code>, <code>powershell -Command</code> and <code>sudo</code> are looked through.
        </p>
        <p style={{ margin: '4px 0' }}>
          A matching <code>forbid</code> always wins. Otherwise the most specific rule wins (a longer prefix, then a pattern), and a later file overrides an earlier one. For a script, the strictest decision of its commands applies. Invalid rules are skipped and logged.
        </p>
      </details>
    </Section>
  )
}

export function PermissionsSettings() {
  const { cfg, write } = useEngineConfig()
  const [status, setStatus] = useState<SandboxStatus | null>(null)
  const refreshStatus = useCallback(() => {
    void call('sandbox/status', {})
      .then(setStatus)
      .catch((e: Error) => toast(`Sandbox status: ${e.message}`, 'error'))
  }, [])
  useEffect(refreshStatus, [refreshStatus])
  const saveRubric = useCallback((v: string) => write([{ keyPath: 'automatic_review.rubric', value: v.trim() ? v : null }]), [write])

  if (!cfg) return <div className="spinner" aria-label="Loading" />
  const u = cfg.user
  const mode: PermissionMode = u.permission_mode ?? (u.sandbox_mode === 'read-only' ? 'read-only' : u.sandbox_mode === 'danger-full-access' ? 'full-access' : 'auto')
  const sb = u.sandbox ?? { writable_roots: [] }
  const profileMode = cfg.activeProfile ? cfg.user.profiles[cfg.activeProfile]?.permission_mode : null

  const setMode = async (m: PermissionMode) => {
    if (m === mode) return
    if (m === 'full-access' && !(await confirmDialog('Use full access by default?', 'New threads will run commands without a sandbox and without asking. Only use this on a machine or VM you can afford to break.', 'Use full access', true))) return
    await write([{ keyPath: 'permission_mode', value: m }])
  }

  return (
    <div className="sx-panel">
      <Section title="Default permission mode" desc="Used by new threads. Change a running thread's mode from the composer.">
        <div className="sx-modes" role="radiogroup" aria-label="Default permission mode">
          {MODES.map((m) => (
            <button key={m.id} role="radio" aria-checked={mode === m.id} className="sx-mode" onClick={() => void setMode(m.id)}>
              <span className="sx-mode-title">
                <m.icon size={14} color={m.id === 'full-access' ? 'var(--danger)' : 'var(--accent)'} aria-hidden />
                {m.title}
              </span>
              <span className="sx-mode-desc">{m.desc}</span>
            </button>
          ))}
        </div>
        {mode === 'full-access' && (
          <div style={{ marginTop: 8 }}>
            <Callout kind="danger">Full access is on: the agent can change or delete anything your user account can, including outside the project, and use the network without asking.</Callout>
          </div>
        )}
        {profileMode && profileMode !== mode && (
          <div style={{ marginTop: 8 }}>
            <Callout kind="warning">
              The active profile <b>{cfg.activeProfile}</b> sets the mode to <b>{profileMode}</b>.
            </Callout>
          </div>
        )}
        <details className="sx-help" style={{ marginTop: 10 }}>
          <summary>Advanced: approval policy and sandbox mode</summary>
          <p style={{ margin: '4px 0 8px' }}>
            <code>approval_policy</code> refines the Auto mode: <b>untrusted</b> asks before anything that isn't a known-safe read; <b>on-failure</b> runs everything in the sandbox and asks only to retry a command the sandbox blocked; <b>on-request</b> (default) asks when the agent requests escalation or network access; <b>never</b> never asks and returns refusals to the agent. Read-only and Full access ignore it. <code>sandbox_mode</code> picks the permission mode only when <code>permission_mode</code> is not set.
          </p>
          <Row label="approval_policy">
            <select className="select" style={{ minWidth: 180 }} value={u.approval_policy ?? ''} aria-label="Approval policy" onChange={(e) => void write([{ keyPath: 'approval_policy', value: e.target.value || null }])}>
              <option value="">(not set)</option>
              {APPROVAL_POLICIES.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </Row>
          <Row label="sandbox_mode">
            <select className="select" style={{ minWidth: 180 }} value={u.sandbox_mode ?? ''} aria-label="Sandbox mode" onChange={(e) => void write([{ keyPath: 'sandbox_mode', value: e.target.value || null }])}>
              <option value="">(not set)</option>
              {SANDBOX_MODES.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </Row>
        </details>
      </Section>

      <Section title="Sandbox">
        <SandboxCard status={status} onRefresh={refreshStatus} />
        <Row label="Windows sandbox backend" hint="Restricted token works everywhere; AppContainer is stricter but some tools fail inside it; None asks before every command.">
          <select
            className="select"
            style={{ minWidth: 180 }}
            value={sb.windows_backend ?? ''}
            aria-label="Windows sandbox backend"
            onChange={async (e) => {
              await write([{ keyPath: 'sandbox.windows_backend', value: e.target.value || null }])
              refreshStatus()
            }}
          >
            <option value="">Default (restricted token)</option>
            <option value="restricted-token">Restricted token</option>
            <option value="appcontainer">AppContainer</option>
            <option value="none">None</option>
          </select>
        </Row>
        <Row label="Allow network access" hint="Lets sandboxed commands use the network without asking (Auto mode).">
          <Toggle checked={!!sb.network_access} onChange={(v) => void write([{ keyPath: 'sandbox.network_access', value: v }])} label="Allow network access" />
        </Row>
        <div style={{ padding: '10px 0 0' }}>
          <div style={{ marginBottom: 6 }}>Extra writable folders</div>
          <div className="xs subtle" style={{ marginBottom: 8 }}>
            Besides the workspace, sandboxed commands may write here (for example a shared build cache). <code>.git</code> folders stay protected.
          </div>
          <WritableRoots roots={sb.writable_roots ?? []} onChange={(r) => void write([{ keyPath: 'sandbox.writable_roots', value: r }])} />
        </div>
      </Section>

      <Section title="Automatic review" desc="The reviewer model looks at actions that would need your approval. It approves low-risk ones, denies clearly unsafe ones (use /approve to override) and passes the rest to you with its verdict.">
        <Row label="Review approvals automatically">
          <Toggle checked={!!u.automatic_review?.enabled} onChange={(v) => void write([{ keyPath: 'automatic_review.enabled', value: v }])} label="Automatic review" />
        </Row>
        <Rubric initial={u.automatic_review?.rubric ?? ''} save={saveRubric} />
      </Section>

      <CommandRules />
    </div>
  )
}
