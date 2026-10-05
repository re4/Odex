import { useCallback, useEffect, useMemo, useState } from 'react'
import { Copy, Info, RefreshCw, ShieldAlert, ShieldCheck } from 'lucide-react'
import type { HookEvent, HookInfo } from '@shared/index'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { SectionHead, projectRoot, sourceLabel, useTrustedProjects } from '@/views/settings/IntegrationsShared'

export const HOOK_EVENTS: Array<{ event: HookEvent; label: string; toml: string; help: string }> = [
  { event: 'sessionStart', label: 'Session start', toml: 'session_start', help: 'When a thread starts. Output is added to the agent’s context.' },
  { event: 'userPromptSubmit', label: 'User prompt submit', toml: 'user_prompt_submit', help: 'Before your message reaches the model. Exit code 2 blocks it; output adds context.' },
  { event: 'preToolUse', label: 'Before tool use', toml: 'pre_tool_use', help: 'Before a tool runs. The matcher (a regex on the tool name) picks the tools; exit code 2 blocks the call and JSON output can rewrite its input.' },
  { event: 'postToolUse', label: 'After tool use', toml: 'post_tool_use', help: 'After a tool finishes. Output is added to the tool result the model sees.' },
  { event: 'stop', label: 'Stop', toml: 'stop', help: 'When the agent finishes a turn. Exit code 2 with a reason asks it to keep going.' },
  { event: 'notification', label: 'Notification', toml: 'notification', help: 'When Odex sends a notification, such as an approval request.' },
]

const EXAMPLE = `# ~/.odex/config.toml (every project) or <project>/.odex/config.toml
[[hooks.pre_tool_use]]
name = "Guard shell commands"
matcher = "shell|exec_command"
command = "node .odex/hooks/guard.js"
timeout_ms = 10000`

const KEPT_KEY = 'odex.hooks.keptDisabled'

/** Hooks the user chose to keep disabled: `{ id: hash }` (re-asks when the hook changes). */
function loadKept(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(KEPT_KEY) || '{}') as Record<string, string>
  } catch {
    return {}
  }
}

function saveKept(k: Record<string, string>) {
  try {
    localStorage.setItem(KEPT_KEY, JSON.stringify(k))
  } catch {}
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

function eventLabel(e: HookEvent): string {
  return HOOK_EVENTS.find((x) => x.event === e)?.label ?? e
}

export function HooksSettings() {
  const projects = useTrustedProjects()
  const allProjects = useApp((s) => s.projects)
  const [hooks, setHooks] = useState<HookInfo[] | null>(null)
  const [kept, setKept] = useState<Record<string, string>>(loadKept)
  const [busy, setBusy] = useState<string | null>(null)

  const isKept = useCallback((h: HookInfo) => h.trust !== 'trusted' && kept[h.id] === h.hash, [kept])

  const load = useCallback(async () => {
    try {
      const lists = await Promise.all([
        call('hooks/list', { cwd: null }).then((r) => r.hooks),
        ...projects.map((p) =>
          call('hooks/list', { cwd: projectRoot(p) })
            .then((r) => r.hooks)
            .catch(() => [] as HookInfo[]),
        ),
      ])
      const byId = new Map<string, HookInfo>()
      for (const list of lists) for (const h of list) if (!byId.has(h.id)) byId.set(h.id, h)
      setHooks([...byId.values()])
    } catch (e) {
      toast(`Hooks: ${errText(e)}`, 'error')
      setHooks((h) => h ?? [])
    }
  }, [projects])

  useEffect(() => {
    void load()
  }, [load])

  // keep the app-wide review banner in sync with what this panel knows
  useEffect(() => {
    if (!hooks) return
    useApp.setState({ hooksNeedingReview: hooks.filter((h) => h.trust !== 'trusted' && !isKept(h)) })
  }, [hooks, isKept])

  const setTrust = async (h: HookInfo, trusted: boolean) => {
    setBusy(h.id)
    try {
      await call('hooks/trust', { id: h.id, hash: h.hash, trusted })
      const next = { ...kept }
      if (trusted) delete next[h.id]
      else next[h.id] = h.hash
      setKept(next)
      saveKept(next)
      await load()
      toast(trusted ? 'Hook trusted; it runs from now on' : 'Hook kept disabled', trusted ? 'success' : 'info')
    } catch (e) {
      toast(errText(e), 'error')
      await load()
    } finally {
      setBusy(null)
    }
  }

  const review = useMemo(() => (hooks ?? []).filter((h) => h.trust !== 'trusted' && !isKept(h)), [hooks, isKept])

  const badge = (h: HookInfo) =>
    h.trust === 'trusted' ? (
      <span className="badge success">Trusted</span>
    ) : isKept(h) ? (
      <span className="badge">Disabled</span>
    ) : h.trust === 'changed' ? (
      <span className="badge warning">Changed</span>
    ) : (
      <span className="badge warning">Untrusted</span>
    )

  return (
    <div className="int-panel">
      <p className="int-intro">
        Hooks run shell commands at points in the agent loop. Each hook gets the event as JSON on stdin and can block the action (exit code 2) or add context. New or changed hooks never run until you trust them here; project hooks only load from trusted projects.
      </p>

      {review.length > 0 && (
        <section aria-label="Hooks needing review">
          <SectionHead title={`Needs review (${review.length})`} />
          <div className="int-list">
            {review.map((h) => (
              <div key={h.id} className="int-review" data-testid={`hook-review-${h.id}`}>
                <div className="int-review-head">
                  <ShieldAlert size={16} color="var(--warning)" />
                  <b>{h.name || eventLabel(h.event)}</b>
                  <span className="badge warning">{h.trust === 'changed' ? 'Changed since trusted' : 'New hook'}</span>
                  <span className="badge">{eventLabel(h.event)}</span>
                  <span className="xs subtle">{sourceLabel(h.source, allProjects)}</span>
                </div>
                {h.matcher && (
                  <div className="xs muted">
                    Runs for tools matching <code>{h.matcher}</code>
                  </div>
                )}
                <pre className="int-code" aria-label="Hook command">
                  {h.command}
                </pre>
                <div className="xs muted">{h.trust === 'changed' ? 'The command, its matcher or a script it runs changed after you trusted it. Check the new version before trusting it again.' : 'Trusting lets Odex run this command automatically with your permissions whenever the event fires.'}</div>
                <div className="int-review-actions">
                  <button className="btn btn-sm" disabled={busy === h.id} onClick={() => void setTrust(h, false)}>
                    Keep disabled
                  </button>
                  <button className="btn btn-sm btn-primary" disabled={busy === h.id} onClick={() => void setTrust(h, true)}>
                    <ShieldCheck size={13} /> Trust
                  </button>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      <section>
        <SectionHead title={`Hooks${hooks?.length ? ` (${hooks.length})` : ''}`}>
          <button className="btn btn-sm" onClick={() => void load()}>
            <RefreshCw size={13} /> Refresh
          </button>
        </SectionHead>
        {hooks == null ? (
          <span className="spinner" />
        ) : (
          HOOK_EVENTS.map((ev) => {
            const list = hooks.filter((h) => h.event === ev.event)
            return (
              <div key={ev.event} className="int-group" role="group" aria-label={ev.label}>
                <div className="int-group-title">
                  <span className="section-title">{ev.label}</span>
                  <span className="int-group-hint">{ev.help}</span>
                </div>
                {list.length === 0 ? (
                  <div className="xs subtle" style={{ paddingLeft: 2 }}>
                    No hooks · <code>[[hooks.{ev.toml}]]</code>
                  </div>
                ) : (
                  <div className="card">
                    {list.map((h) => (
                      <div key={h.id} className="int-row" data-testid={`hook-${h.id}`}>
                        <div className="int-row-main">
                          <div className="int-row-title">
                            {h.name && <span style={{ fontWeight: 600, flex: 'none' }}>{h.name}</span>}
                            <code className="mono ellipsis selectable" title={h.command}>
                              {h.command}
                            </code>
                          </div>
                          <div className="row xs subtle" style={{ gap: 6, marginTop: 2 }}>
                            <span>{sourceLabel(h.source, allProjects)}</span>
                            {h.matcher && (
                              <span>
                                · matcher <code>{h.matcher}</code>
                              </span>
                            )}
                          </div>
                        </div>
                        {badge(h)}
                        {busy === h.id && <span className="spinner" />}
                        {h.trust === 'trusted' ? (
                          <button className="btn btn-sm btn-ghost" disabled={busy === h.id} onClick={() => void setTrust(h, false)} aria-label={`Revoke trust for ${h.name || h.command}`}>
                            Revoke
                          </button>
                        ) : (
                          <button className="btn btn-sm" disabled={busy === h.id} onClick={() => void setTrust(h, true)} aria-label={`Trust ${h.name || h.command}`}>
                            Trust
                          </button>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )
          })
        )}
      </section>

      <section>
        <SectionHead title="Adding hooks">
          <button className="btn btn-sm btn-ghost" onClick={() => A.copy(EXAMPLE, 'Example copied')}>
            <Copy size={13} /> Copy example
          </button>
        </SectionHead>
        <pre className="int-code">{EXAMPLE}</pre>
        <div className="int-note" style={{ marginTop: 10 }}>
          <Info size={15} />
          <div>
            stdin carries <code>hook_event</code>, <code>thread_id</code>, <code>cwd</code> and event fields such as <code>tool_name</code> and <code>tool_input</code>. Exit 0 to continue: plain stdout adds context, or print JSON like <code>{'{"decision": "block", "reason": "…"}'}</code> or <code>{'{"modified_input": {…}}'}</code>. Exit 2 blocks with stderr as the reason. Editing a hook or a script it references asks for review again.
          </div>
        </div>
      </section>
    </div>
  )
}
