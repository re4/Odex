import { useCallback, useEffect, useMemo, useState } from 'react'
import { Archive, CheckCircle2, ChevronDown, ChevronRight, CircleAlert, CircleMinus, Clock, FileText, MessageSquare, Pencil, Play, Plus, Trash2 } from 'lucide-react'
import type { Automation, AutomationRun, PermissionMode, Project, ReasoningEffort, RunMode, ScheduleValidateResponse, Thread } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Menu, Modal, Toggle, relativeTime, type MenuItem } from '@/components/ui'
import '@/styles/automations.css'

// ---------------------------------------------------------------- shared helpers (also used by ActivityView)

export const PERMISSION_LABEL: Record<PermissionMode, string> = { 'read-only': 'Read only', auto: 'Auto', 'full-access': 'Full access' }
const EFFORT_LABEL: Record<ReasoningEffort, string> = { none: 'None', minimal: 'Minimal', low: 'Low', medium: 'Medium', high: 'High', xhigh: 'Extra high' }
const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday']

export const BACKGROUND_NOTE = 'Automations run on this machine while Odex is open or running in the tray.'

/** "5m ago" / "just now" / a date for older times. */
export function ago(ms: number): string {
  const r = relativeTime(ms)
  if (r === 'now') return 'just now'
  return /^\d+[mhd]$/.test(r) ? `${r} ago` : r
}

/** "in 5m" / "in 2h 10m" / "in 3d" / a date for later times. */
export function until(ms: number): string {
  const d = ms - Date.now()
  if (d < 60_000) return 'in <1m'
  if (d < 3_600_000) return `in ${Math.round(d / 60_000)}m`
  if (d < 86_400_000) {
    const h = Math.floor(d / 3_600_000)
    const m = Math.round((d % 3_600_000) / 60_000)
    return m ? `in ${h}h ${m}m` : `in ${h}h`
  }
  if (d < 2 * 86_400_000) return `in ${Math.round(d / 3_600_000)}h`
  if (d < 45 * 86_400_000) return `in ${Math.round(d / 86_400_000)}d`
  return `on ${new Date(ms).toLocaleDateString()}`
}

export function formatWhen(ms: number): string {
  return new Date(ms).toLocaleString(undefined, { weekday: 'short', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

export function duration(run: AutomationRun): string | null {
  if (!run.finishedAt) return null
  const s = Math.max(0, Math.round((run.finishedAt - run.startedAt) / 1000))
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`
}

const STATUS_LABEL: Record<string, string> = { running: 'Running', completed: 'Completed', failed: 'Failed', skipped: 'Skipped' }

export function RunStatusIcon({ status, size = 14 }: { status: string; size?: number }) {
  const label = STATUS_LABEL[status] ?? status
  if (status === 'running') return <span className="spinner" style={{ width: size - 2, height: size - 2 }} role="img" aria-label={label} />
  if (status === 'completed') return <CheckCircle2 size={size} color="var(--success)" aria-label={label} />
  if (status === 'failed') return <CircleAlert size={size} color="var(--danger)" aria-label={label} />
  return <CircleMinus size={size} color="var(--fg-subtle)" aria-label={label} />
}

export function RunStatusBadge({ status }: { status: string }) {
  const kind = status === 'completed' ? 'success' : status === 'failed' ? 'danger' : status === 'running' ? 'accent' : ''
  return <span className={`badge ${kind}`}>{STATUS_LABEL[status] ?? status}</span>
}

/** Re-read the inbox runs and the unread count into the store (keeps the sidebar badge right). */
export async function refreshRunsStore(): Promise<void> {
  const r = await call('automation/runs', { unreadOnly: false, includeArchived: false, limit: 200 })
  useApp.setState({ automationRuns: r.runs, automationUnread: r.unreadCount })
}

/** Optimistically patch runs in the store, then confirm with the engine. */
export function patchRunsStore(ids: string[], patch: Partial<AutomationRun>): void {
  const s = useApp.getState()
  const set = new Set(ids)
  let runs = s.automationRuns.map((r) => (set.has(r.id) ? { ...r, ...patch } : r))
  if (patch.archived) runs = runs.filter((r) => !r.archived)
  const removed = s.automationRuns.filter((r) => set.has(r.id) && r.unread && !r.archived).length
  const after = runs.filter((r) => set.has(r.id) && r.unread && !r.archived).length
  useApp.setState({ automationRuns: runs, automationUnread: Math.max(0, s.automationUnread - removed + after) })
}

export async function markRunsRead(ids: string[]): Promise<void> {
  if (!ids.length) return
  patchRunsStore(ids, { unread: false })
  try {
    await call('automation/runs/markRead', { ids })
  } catch (e) {
    toast(`Could not mark read: ${(e as Error).message}`, 'error')
  }
  await refreshRunsStore().catch(() => {})
}

export async function archiveRuns(ids: string[]): Promise<void> {
  if (!ids.length) return
  patchRunsStore(ids, { archived: true, unread: false })
  try {
    await call('automation/runs/archive', { ids })
  } catch (e) {
    toast(`Could not archive: ${(e as Error).message}`, 'error')
  }
  await refreshRunsStore().catch(() => {})
}

/** Open the thread a run worked in and mark the run read. */
export async function openRun(run: AutomationRun): Promise<void> {
  if (run.unread) void markRunsRead([run.id])
  if (run.threadId) await useApp.getState().selectThread(run.threadId)
  else toast('This run has no thread (it failed before starting one).')
}

export function threadTitle(t: Thread | undefined): string {
  return t ? t.name || t.preview || 'Untitled thread' : 'Unknown thread'
}

/** Threads that can be woken by a thread automation. */
function wakeableThreads(threads: Record<string, { thread: Thread }>, order: string[]): Thread[] {
  return order
    .map((id) => threads[id]?.thread)
    .filter((t): t is Thread => !!t && !t.archived && !t.ephemeral && t.kind !== 'subagent' && t.kind !== 'side')
}

function useScheduleDescriptions(schedules: string[]): Record<string, ScheduleValidateResponse> {
  const [map, setMap] = useState<Record<string, ScheduleValidateResponse>>({})
  const key = [...new Set(schedules)].sort().join('\n')
  useEffect(() => {
    let cancelled = false
    const missing = key ? key.split('\n') : []
    void Promise.all(missing.map((s) => call('automation/validateSchedule', { schedule: s }).then((r) => [s, r] as const).catch(() => null))).then((rows) => {
      if (cancelled) return
      const next: Record<string, ScheduleValidateResponse> = {}
      for (const row of rows) if (row) next[row[0]] = row[1]
      setMap(next)
    })
    return () => {
      cancelled = true
    }
  }, [key])
  return map
}

/** Re-render periodically so relative times stay fresh. */
function useTick(ms: number): number {
  const [n, setN] = useState(0)
  useEffect(() => {
    const t = setInterval(() => setN((x) => x + 1), ms)
    return () => clearInterval(t)
  }, [ms])
  return n
}

// ---------------------------------------------------------------- schedule presets

type Preset = 'hourly' | 'daily' | 'weekdays' | 'weekly' | 'custom'
interface Sched {
  preset: Preset
  minute: number
  time: string
  day: number
  custom: string
}

const PRESETS: Array<{ id: Preset; label: string }> = [
  { id: 'hourly', label: 'Hourly' },
  { id: 'daily', label: 'Daily' },
  { id: 'weekdays', label: 'Weekdays' },
  { id: 'weekly', label: 'Weekly' },
  { id: 'custom', label: 'Custom' },
]

function buildSchedule(s: Sched): string {
  const [h, m] = s.time.split(':').map((x) => Number(x) || 0)
  switch (s.preset) {
    case 'hourly':
      return `${s.minute} * * * *`
    case 'daily':
      return `${m} ${h} * * *`
    case 'weekdays':
      return `${m} ${h} * * 1-5`
    case 'weekly':
      return `${m} ${h} * * ${s.day}`
    default:
      return s.custom.trim()
  }
}

/** Map a stored schedule back onto a preset when it has one of the preset shapes. */
function parseSched(src: string): Sched {
  const base: Sched = { preset: 'custom', minute: 0, time: '09:00', day: 1, custom: src }
  const f = src.trim().split(/\s+/)
  if (f.length !== 5 || f[2] !== '*' || f[3] !== '*' || !/^\d{1,2}$/.test(f[0])) return base
  const minute = Number(f[0])
  if (minute > 59) return base
  if (f[1] === '*' && f[4] === '*') return { ...base, preset: 'hourly', minute }
  if (!/^\d{1,2}$/.test(f[1]) || Number(f[1]) > 23) return base
  const time = `${f[1].padStart(2, '0')}:${f[0].padStart(2, '0')}`
  if (f[4] === '*') return { ...base, preset: 'daily', time, minute }
  if (f[4] === '1-5') return { ...base, preset: 'weekdays', time, minute }
  if (/^[0-6]$/.test(f[4])) return { ...base, preset: 'weekly', time, minute, day: Number(f[4]) }
  return base
}

function SchedulePreview({ schedule, onValid }: { schedule: string; onValid: (valid: boolean) => void }) {
  const [res, setRes] = useState<ScheduleValidateResponse | null>(null)
  useEffect(() => {
    onValid(false)
    if (!schedule.trim()) {
      setRes({ valid: false, error: 'Enter a schedule.', nextRuns: [] })
      return
    }
    let cancelled = false
    const t = setTimeout(() => {
      void call('automation/validateSchedule', { schedule })
        .then((r) => {
          if (cancelled) return
          setRes(r)
          onValid(r.valid)
        })
        .catch((e: Error) => !cancelled && setRes({ valid: false, error: e.message, nextRuns: [] }))
    }, 180)
    return () => {
      cancelled = true
      clearTimeout(t)
    }
  }, [schedule, onValid])
  if (!res) return <div className="sched-preview muted">Checking schedule…</div>
  if (!res.valid)
    return (
      <div className="sched-preview error" role="alert" aria-label="Schedule preview">
        <CircleAlert size={14} />
        <span className="selectable">{res.error}</span>
      </div>
    )
  return (
    <div className="sched-preview" aria-label="Schedule preview">
      <div className="row" style={{ gap: 6 }}>
        <Clock size={14} color="var(--accent)" />
        <b className="sched-desc">{res.description}</b>
      </div>
      <div className="xs subtle" style={{ marginTop: 6 }}>
        Next runs
      </div>
      <ol className="sched-next" aria-label="Next runs">
        {res.nextRuns.slice(0, 3).map((t) => (
          <li key={t}>
            {formatWhen(t)} {until(t).startsWith('in ') && <span className="subtle">({until(t)})</span>}
          </li>
        ))}
      </ol>
    </div>
  )
}

// ---------------------------------------------------------------- templates

export interface AutomationTemplate {
  id: string
  name: string
  prompt: string
  schedule: string
  permissionMode: PermissionMode
}

/** Ready-made automations ("Use template" in the editor). */
export const AUTOMATION_TEMPLATES: AutomationTemplate[] = [
  {
    id: 'commit-summary',
    name: 'Daily summary of commits',
    schedule: '0 9 * * *',
    permissionMode: 'read-only',
    prompt:
      'Summarize the commits from the last 24 hours (git log --since="24 hours ago" --stat). Group them by area, call out risky or large changes, and list anything that looks unfinished. Do not modify any files.',
  },
  {
    id: 'deps',
    name: 'Dependency update check',
    schedule: '0 9 * * 1',
    permissionMode: 'auto',
    prompt:
      "Check the project's dependencies for available updates with the package manager's own tool (npm outdated, pip list --outdated, cargo outdated, …). List outdated packages with current and latest versions, flag major-version bumps and known security advisories. Do not change any files.",
  },
  {
    id: 'flaky',
    name: 'Flaky test triage',
    schedule: '0 7 * * 1-5',
    permissionMode: 'auto',
    prompt:
      'Run the test suite twice. Report tests that fail in one run but pass in the other (flaky) separately from tests that fail every time, with the likely cause of each and a suggested fix. Do not modify any files.',
  },
  {
    id: 'todo',
    name: 'TODO sweep',
    schedule: '0 16 * * 5',
    permissionMode: 'read-only',
    prompt:
      'Find TODO, FIXME and HACK comments in the codebase. Group them by file, mark the ones added in the last week (git log -S or git blame), and suggest the three most valuable ones to tackle next.',
  },
  {
    id: 'changelog',
    name: 'Changelog draft',
    schedule: '0 17 * * 5',
    permissionMode: 'auto',
    prompt:
      'Draft a changelog entry for the changes since the last release tag (git describe --tags --abbrev=0). Group the entries under Added, Changed and Fixed, in user-facing language. Write the draft to CHANGELOG_DRAFT.md and do not commit.',
  },
  {
    id: 'nightly-tests',
    name: 'Nightly test run',
    schedule: '0 2 * * *',
    permissionMode: 'auto',
    prompt:
      'Run the full test suite and the linter. If everything passes, reply with a one-line summary. If something fails, list each failure with its file and line, the error, and a suggested fix. Do not modify any files.',
  },
]

// ---------------------------------------------------------------- editor

function Seg<T extends string>(props: { value: T; options: Array<{ id: T; label: string }>; onChange: (v: T) => void; label: string }) {
  return (
    <div className="seg" role="radiogroup" aria-label={props.label}>
      {props.options.map((o) => (
        <button key={o.id} type="button" role="radio" aria-checked={props.value === o.id} onClick={() => props.onChange(o.id)}>
          {o.label}
        </button>
      ))}
    </div>
  )
}

function AutomationEditor({ initial, onClose, onSaved }: { initial: Automation | null; onClose: () => void; onSaved: (a: Automation) => void }) {
  const projects = useApp((s) => s.projects)
  const models = useApp((s) => s.models)
  const roles = useApp((s) => s.roles)
  const threadsMap = useApp((s) => s.threads)
  const order = useApp((s) => s.threadOrder)
  const threads = useMemo(() => wakeableThreads(threadsMap, order), [threadsMap, order])
  const defaults = useApp.getState()

  const [name, setName] = useState(initial?.name ?? '')
  const [prompt, setPrompt] = useState(initial?.prompt ?? '')
  const [target, setTarget] = useState<'project' | 'thread'>((initial?.target as 'project' | 'thread') ?? 'project')
  const [projectId, setProjectId] = useState<string>(initial?.projectId ?? defaults.ui.newThreadProjectId ?? projects[0]?.id ?? '')
  const [threadId, setThreadId] = useState<string>(
    () => initial?.threadId ?? threads.find((t) => t.id === defaults.selectedThreadId)?.id ?? threads[0]?.id ?? '',
  )
  const [sched, setSched] = useState<Sched>(() => (initial ? parseSched(initial.schedule) : { preset: 'daily', minute: 0, time: '09:00', day: 1, custom: '0 9 * * 1-5' }))
  const [model, setModel] = useState<string>(initial?.model ?? '')
  const [effort, setEffort] = useState<string>(initial?.effort ?? '')
  const [perm, setPerm] = useState<PermissionMode>(initial?.permissionMode ?? 'auto')
  const [runMode, setRunMode] = useState<RunMode>(initial?.runMode ?? 'local')
  const [enabled, setEnabled] = useState(initial?.enabled ?? true)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  const [schedOk, setSchedOk] = useState(false)
  const [templateAnchor, setTemplateAnchor] = useState<HTMLElement | null>(null)

  const applyTemplate = (tpl: AutomationTemplate) => {
    setName(tpl.name)
    setPrompt(tpl.prompt)
    setSched(parseSched(tpl.schedule))
    setPerm(tpl.permissionMode)
    setTarget('project')
  }
  const templateItems: MenuItem[] = [
    { label: 'Templates', header: true },
    ...AUTOMATION_TEMPLATES.map((tpl) => ({ label: tpl.name, icon: <FileText size={13} />, onSelect: () => applyTemplate(tpl) })),
  ]

  const schedule = buildSchedule(sched)
  const project: Project | undefined = projects.find((p) => p.id === projectId)
  const effectiveModel = models.find((m) => m.key === (model || roles.main)) ?? models[0]
  const efforts = effectiveModel?.efforts ?? []
  const canWorktree = !!project?.isGit
  const targetOk = target === 'project' ? !!project : !!threadId
  const valid = !!prompt.trim() && !!schedule && schedOk && targetOk

  const patchSched = (p: Partial<Sched>) =>
    setSched((s) => {
      const next = { ...s, ...p }
      // switching to custom starts from the expression the picker produced
      if (p.preset === 'custom' && s.preset !== 'custom') next.custom = buildSchedule(s)
      return next
    })

  const save = async () => {
    setBusy(true)
    setErr(null)
    const thread = threads.find((t) => t.id === threadId)
    const a: Automation = {
      // the engine assigns an id to new automations
      id: initial?.id ?? '',
      name: name.trim() || prompt.trim().split('\n')[0].slice(0, 48),
      schedule,
      target,
      projectId: target === 'project' ? projectId : (thread?.projectId ?? null),
      threadId: target === 'thread' ? threadId : null,
      cwd: target === 'thread' ? (thread?.cwd ?? null) : (initial?.cwd ?? null),
      prompt: prompt.trim(),
      model: model || null,
      effort: (effort || null) as ReasoningEffort | null,
      permissionMode: perm,
      runMode: target === 'project' && canWorktree ? runMode : 'local',
      enabled,
      createdAt: initial?.createdAt ?? 0,
      lastRunAt: initial?.lastRunAt ?? null,
      nextRunAt: initial?.nextRunAt ?? null,
    }
    try {
      const r = await call('automation/upsert', { automation: a })
      onSaved(r.automation)
      onClose()
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      title={initial ? 'Edit automation' : 'New automation'}
      onClose={onClose}
      className="auto-editor"
      footer={
        <>
          <label className="checkbox small" style={{ marginRight: 'auto' }}>
            <Toggle checked={enabled} onChange={setEnabled} label="Enabled" />
            Enabled
          </label>
          <button className="btn" onClick={onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" disabled={busy || !valid} onClick={() => void save()}>
            {initial ? 'Save' : 'Create'}
          </button>
        </>
      }
    >
      <div className="col auto-form">
        <div className="field">
          <label htmlFor="ae-name">Name</label>
          <input id="ae-name" className="input" value={name} placeholder="Nightly dependency check" onChange={(e) => setName(e.target.value)} />
        </div>

        <div className="field">
          <div className="row" style={{ alignItems: 'flex-end' }}>
            <label htmlFor="ae-prompt" className="grow">
              Prompt
            </label>
            <button className="btn btn-sm btn-ghost" aria-haspopup="menu" aria-expanded={!!templateAnchor} onClick={(e) => setTemplateAnchor(templateAnchor ? null : e.currentTarget)}>
              <FileText size={13} /> Use template <ChevronDown size={12} />
            </button>
            {templateAnchor && <Menu anchor={templateAnchor} items={templateItems} onClose={() => setTemplateAnchor(null)} align="right" minWidth={230} />}
          </div>
          <textarea id="ae-prompt" className="textarea" rows={4} value={prompt} placeholder="What should Odex do on each run?" onChange={(e) => setPrompt(e.target.value)} />
        </div>

        <div className="field">
          <span className="field-label">Schedule</span>
          <Seg label="Schedule" value={sched.preset} options={PRESETS} onChange={(preset) => patchSched({ preset })} />
          <div className="row sched-inputs">
            {sched.preset === 'hourly' && (
              <>
                <label htmlFor="ae-minute" className="small muted">
                  At minute
                </label>
                <input
                  id="ae-minute"
                  className="input"
                  type="number"
                  min={0}
                  max={59}
                  style={{ width: 80 }}
                  value={sched.minute}
                  onChange={(e) => patchSched({ minute: Math.min(59, Math.max(0, Math.floor(Number(e.target.value) || 0))) })}
                />
                <span className="small muted">past every hour</span>
              </>
            )}
            {sched.preset === 'weekly' && (
              <>
                <label htmlFor="ae-day" className="small muted">
                  Every
                </label>
                <select id="ae-day" className="select" style={{ width: 140 }} value={sched.day} onChange={(e) => patchSched({ day: Number(e.target.value) })}>
                  {[1, 2, 3, 4, 5, 6, 0].map((d) => (
                    <option key={d} value={d}>
                      {DAYS[d]}
                    </option>
                  ))}
                </select>
              </>
            )}
            {(sched.preset === 'daily' || sched.preset === 'weekdays' || sched.preset === 'weekly') && (
              <>
                <label htmlFor="ae-time" className="small muted">
                  at
                </label>
                <input id="ae-time" className="input" type="time" style={{ width: 120 }} value={sched.time} onChange={(e) => patchSched({ time: e.target.value || '00:00' })} />
              </>
            )}
            {sched.preset === 'custom' && (
              <div className="field grow">
                <label htmlFor="ae-cron" className="sr-only">
                  Cron expression
                </label>
                <input
                  id="ae-cron"
                  className="input mono"
                  value={sched.custom}
                  placeholder="*/30 9-17 * * 1-5"
                  spellCheck={false}
                  onChange={(e) => patchSched({ custom: e.target.value })}
                />
                <span className="hint">Cron (minute hour day month weekday) or a phrase like “every 15m”, “weekdays 9am”, “monthly 1 09:00”.</span>
              </div>
            )}
          </div>
          <SchedulePreview schedule={schedule} onValid={setSchedOk} />
        </div>

        <div className="field">
          <span className="field-label">Runs in</span>
          <Seg
            label="Target"
            value={target}
            options={[
              { id: 'project', label: 'New thread in a project' },
              { id: 'thread', label: 'Wake an existing thread' },
            ]}
            onChange={setTarget}
          />
          {target === 'project' ? (
            projects.length ? (
              <div className="field" style={{ marginTop: 6 }}>
                <label htmlFor="ae-project" className="sr-only">
                  Project
                </label>
                <select id="ae-project" className="select" value={projectId} onChange={(e) => setProjectId(e.target.value)}>
                  {!project && <option value="">Choose a project…</option>}
                  {projects.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
                <span className="hint">Each run starts a fresh thread in this project.</span>
              </div>
            ) : (
              <div className="hint-box small">
                Add a project first.{' '}
                <button className="btn btn-sm" onClick={() => void A.addProjectFromDialog()}>
                  Add project…
                </button>
              </div>
            )
          ) : threads.length ? (
            <div className="field" style={{ marginTop: 6 }}>
              <label htmlFor="ae-thread" className="sr-only">
                Thread
              </label>
              <select id="ae-thread" className="select" value={threadId} onChange={(e) => setThreadId(e.target.value)}>
                {!threads.some((t) => t.id === threadId) && <option value="">Choose a thread…</option>}
                {threads.map((t) => (
                  <option key={t.id} value={t.id}>
                    {threadTitle(t).slice(0, 80)}
                    {t.projectId ? ` · ${projects.find((p) => p.id === t.projectId)?.name ?? ''}` : ''}
                  </option>
                ))}
              </select>
              <span className="hint">The prompt is sent into this thread, which keeps its context, model and permission mode. Busy threads skip the run.</span>
            </div>
          ) : (
            <div className="hint-box small">There are no threads to wake yet.</div>
          )}
        </div>

        {target === 'project' && (
          <div className="auto-grid">
            <div className="field">
              <label htmlFor="ae-model">Model</label>
              <select
                id="ae-model"
                className="select"
                value={model}
                onChange={(e) => {
                  setModel(e.target.value)
                  const m = models.find((x) => x.key === (e.target.value || roles.main))
                  if (effort && !m?.efforts.includes(effort as ReasoningEffort)) setEffort('')
                }}
              >
                <option value="">Default{roles.main ? ` (${models.find((m) => m.key === roles.main)?.displayName ?? roles.main})` : ''}</option>
                {models.map((m) => (
                  <option key={m.key} value={m.key}>
                    {m.displayName}
                  </option>
                ))}
              </select>
            </div>
            <div className="field">
              <label htmlFor="ae-effort">Reasoning effort</label>
              <select id="ae-effort" className="select" value={effort} disabled={!efforts.length} onChange={(e) => setEffort(e.target.value)}>
                <option value="">{efforts.length ? 'Default' : 'Not supported'}</option>
                {efforts.map((e) => (
                  <option key={e} value={e}>
                    {EFFORT_LABEL[e]}
                  </option>
                ))}
              </select>
            </div>
            <div className="field">
              <label htmlFor="ae-perm">Permissions</label>
              <select id="ae-perm" className="select" value={perm} onChange={(e) => setPerm(e.target.value as PermissionMode)}>
                {(Object.keys(PERMISSION_LABEL) as PermissionMode[]).map((p) => (
                  <option key={p} value={p}>
                    {PERMISSION_LABEL[p]}
                  </option>
                ))}
              </select>
              {perm === 'full-access' && <span className="hint" style={{ color: 'var(--danger)' }}>No sandbox and no approvals while unattended.</span>}
              {perm !== 'full-access' && <span className="hint">Actions that need approval wait for you.</span>}
            </div>
            <div className="field">
              <label htmlFor="ae-runmode">Run in</label>
              <select id="ae-runmode" className="select" value={canWorktree ? runMode : 'local'} onChange={(e) => setRunMode(e.target.value as RunMode)}>
                <option value="local">Project folder (local)</option>
                <option value="worktree" disabled={!canWorktree}>
                  New git worktree{canWorktree ? '' : ' (git projects only)'}
                </option>
              </select>
            </div>
          </div>
        )}
        {err && (
          <div className="sched-preview error selectable" role="alert">
            <CircleAlert size={14} />
            <span>{err}</span>
          </div>
        )}
      </div>
    </Modal>
  )
}

// ---------------------------------------------------------------- list

function RunHistory({ automation }: { automation: Automation }) {
  const live = useApp((s) => s.automationRuns)
  const [runs, setRuns] = useState<AutomationRun[] | null>(null)
  useEffect(() => {
    let cancelled = false
    void call('automation/runs', { automationId: automation.id, unreadOnly: false, includeArchived: true, limit: 50 })
      .then((r) => !cancelled && setRuns(r.runs))
      .catch(() => !cancelled && setRuns([]))
    return () => {
      cancelled = true
    }
  }, [automation.id, live])
  return (
    <div className="auto-detail">
      <div className="auto-detail-prompt">
        <div className="section-title">Prompt</div>
        <div className="selectable small">{automation.prompt}</div>
      </div>
      <div className="row" style={{ margin: '12px 0 4px', alignItems: 'center' }}>
        <div className="section-title grow">Run history</div>
        {runs && runs.some((r) => !r.archived && r.status !== 'running') && (
          <button
            className="btn btn-sm btn-ghost"
            title="Archive every finished run of this automation (they leave Activity)"
            onClick={async () => {
              const ids = runs.filter((r) => !r.archived && r.status !== 'running').map((r) => r.id)
              if (!(await A.confirmDialog('Archive all runs', `Archive ${ids.length} run${ids.length === 1 ? '' : 's'} of “${automation.name}”? They stay in the run history, marked archived.`, 'Archive'))) return
              await archiveRuns(ids)
              setRuns((cur) => (cur ?? []).map((r) => (ids.includes(r.id) ? { ...r, archived: true, unread: false } : r)))
            }}
          >
            <Archive size={13} /> Archive all runs
          </button>
        )}
      </div>
      {runs === null ? (
        <div className="row small muted" style={{ padding: 8 }}>
          <span className="spinner" /> Loading…
        </div>
      ) : runs.length === 0 ? (
        <div className="small subtle" style={{ padding: '6px 2px' }}>
          No runs yet. Use “Run now” to try it.
        </div>
      ) : (
        <ul className="run-list" aria-label="Run history">
          {runs.map((r) => (
            <li key={r.id} className={`run-row ${r.unread ? 'unread' : ''}`}>
              <span className="run-status">
                <RunStatusIcon status={r.status} />
              </span>
              <div className="grow">
                <div className="row small" style={{ gap: 6 }}>
                  <span title={new Date(r.startedAt).toLocaleString()}>{formatWhen(r.startedAt)}</span>
                  {duration(r) && <span className="subtle">· {duration(r)}</span>}
                  {r.unread && <span className="dot accent" aria-label="Unread" />}
                  {r.archived && <span className="badge">archived</span>}
                </div>
                {(r.error || r.summary) && <div className={`run-summary ${r.error ? 'error' : ''}`}>{r.error || r.summary}</div>}
              </div>
              {r.threadId && (
                <button className="btn btn-sm btn-ghost" onClick={() => void openRun(r)}>
                  <MessageSquare size={13} /> Open thread
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

function AutomationCard(props: {
  a: Automation
  desc?: ScheduleValidateResponse
  lastRun?: AutomationRun
  selected: boolean
  onSelect: () => void
  onEdit: () => void
  onChanged: () => void
}) {
  const { a, lastRun } = props
  const projects = useApp((s) => s.projects)
  const thread = useApp((s) => (a.threadId ? s.threads[a.threadId]?.thread : undefined))
  const targetLabel = a.target === 'thread' ? `Wakes “${threadTitle(thread)}”` : (projects.find((p) => p.id === a.projectId)?.name ?? 'No project')
  const running = lastRun?.status === 'running'

  const setEnabled = async (enabled: boolean) => {
    try {
      await call('automation/upsert', { automation: { ...a, enabled } })
    } catch (e) {
      toast(`Could not update: ${(e as Error).message}`, 'error')
    }
    props.onChanged()
  }
  const runNow = async () => {
    try {
      await call('automation/runNow', { id: a.id })
      toast(`Started “${a.name}”`)
    } catch (e) {
      toast(`Could not run: ${(e as Error).message}`, 'error')
    }
  }
  const remove = async () => {
    if (!(await A.confirmDialog('Delete automation', `Delete “${a.name}”? Past runs stay in Activity.`, 'Delete', true))) return
    try {
      await call('automation/delete', { id: a.id })
    } catch (e) {
      toast(`Could not delete: ${(e as Error).message}`, 'error')
    }
    props.onChanged()
  }

  return (
    <article className={`auto-card ${props.selected ? 'selected' : ''} ${a.enabled ? '' : 'paused'}`} aria-label={a.name}>
      <div className="auto-row">
        <button className="auto-row-main" onClick={props.onSelect} aria-expanded={props.selected} title={props.selected ? 'Hide run history' : 'Show run history'}>
          <span className="auto-chevron">{props.selected ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
          <span className={`auto-icon ${a.enabled ? '' : 'paused'}`}>
            <Clock size={15} />
          </span>
          <span className="grow" style={{ minWidth: 0 }}>
            <span className="auto-name ellipsis">{a.name}</span>
            <span className="auto-meta">
              <span className="ellipsis" title={a.schedule}>
                {props.desc?.description ?? (props.desc && !props.desc.valid ? 'Invalid schedule' : a.schedule)}
              </span>
              <span className="sep">·</span>
              <span className="ellipsis">{targetLabel}</span>
            </span>
          </span>
        </button>
        <div className="auto-side">
          <span className={a.enabled ? '' : 'subtle'} title={a.nextRunAt ? formatWhen(a.nextRunAt) : undefined}>
            {a.enabled ? (a.nextRunAt ? `Next ${until(a.nextRunAt)}` : 'Not scheduled') : 'Paused'}
          </span>
          {lastRun ? (
            <span className="row" style={{ gap: 4 }} title={lastRun.error ?? lastRun.summary ?? undefined}>
              <RunStatusIcon status={lastRun.status} size={12} />
              <span className="subtle">{running ? 'Running now' : `${STATUS_LABEL[lastRun.status] ?? lastRun.status} ${ago(lastRun.startedAt)}`}</span>
            </span>
          ) : (
            <span className="subtle">Never run</span>
          )}
        </div>
        <div className="auto-actions">
          <Toggle checked={a.enabled} onChange={(v) => void setEnabled(v)} label="Enabled" />
          <button className="btn btn-sm" onClick={() => void runNow()} title="Run now">
            <Play size={12} /> Run now
          </button>
          <button className="icon-btn sm" aria-label="Edit" title="Edit" onClick={props.onEdit}>
            <Pencil size={13} />
          </button>
          <button className="icon-btn sm danger" aria-label="Delete" title="Delete" onClick={() => void remove()}>
            <Trash2 size={13} />
          </button>
        </div>
      </div>
      {props.selected && <RunHistory automation={a} />}
    </article>
  )
}

/** Scheduled prompts: list, editor and per-automation run history. */
export function AutomationsView() {
  const live = useApp((s) => s.automationRuns)
  const [list, setList] = useState<Automation[] | null>(null)
  const [runs, setRuns] = useState<AutomationRun[]>([])
  const [selected, setSelected] = useState<string | null>(null)
  const [editing, setEditing] = useState<Automation | 'new' | null>(null)
  const tick = useTick(30_000)

  const load = useCallback(async () => {
    try {
      const [l, r] = await Promise.all([call('automation/list', {}), call('automation/runs', { unreadOnly: false, includeArchived: true, limit: 500 })])
      setList(l.automations)
      setRuns(r.runs)
    } catch (e) {
      setList((cur) => cur ?? [])
      toast(`Could not load automations: ${(e as Error).message}`, 'error')
    }
  }, [])

  // reload on mount, on run updates (last run status, next run time) and periodically
  useEffect(() => {
    void load()
  }, [load, live, tick])

  const descs = useScheduleDescriptions((list ?? []).map((a) => a.schedule))
  const lastRun = useMemo(() => {
    const m = new Map<string, AutomationRun>()
    for (const r of runs) if (!m.has(r.automationId)) m.set(r.automationId, r)
    return m
  }, [runs])

  return (
    <div className="auto-page">
      <div className="auto-inner">
        <header className="auto-header">
          <div className="grow">
            <h1>Automations</h1>
            <p>Run a prompt on a schedule, in a new thread or by waking an existing one. {BACKGROUND_NOTE}</p>
          </div>
          <button className="btn btn-primary" onClick={() => setEditing('new')}>
            <Plus size={14} /> New automation
          </button>
        </header>

        {list === null ? (
          <div className="empty">
            <span className="spinner" />
          </div>
        ) : list.length === 0 ? (
          <div className="auto-empty card">
            <span className="auto-empty-icon">
              <Clock size={22} />
            </span>
            <h2>No automations yet</h2>
            <p>
              Automations send a prompt on a schedule, like a nightly dependency check or a weekday summary of open issues. Results land in Activity for you to review.
              <br />
              {BACKGROUND_NOTE}
            </p>
            <button className="btn btn-primary" onClick={() => setEditing('new')}>
              <Plus size={14} /> New automation
            </button>
          </div>
        ) : (
          <div className="auto-list">
            {list.map((a) => (
              <AutomationCard
                key={a.id}
                a={a}
                desc={descs[a.schedule]}
                lastRun={lastRun.get(a.id)}
                selected={selected === a.id}
                onSelect={() => setSelected(selected === a.id ? null : a.id)}
                onEdit={() => setEditing(a)}
                onChanged={() => void load()}
              />
            ))}
          </div>
        )}
      </div>
      {editing && (
        <AutomationEditor
          initial={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={(a) => {
            setSelected(a.id)
            void load()
          }}
        />
      )}
    </div>
  )
}
