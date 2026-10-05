import { useCallback, useEffect, useMemo, useState } from 'react'
import { Download, Eye, FolderOpen, Info, Pencil, Plus, RefreshCw, Search, Trash2 } from 'lucide-react'
import type { Project, SkillInfo, SkillScope } from '@shared/index'
import { call, toast } from '@/lib/rpc'
import * as A from '@/lib/actions'
import { Modal, Toggle } from '@/components/ui'
import { SectionHead, projectRoot, useTrustedProjects } from '@/views/settings/IntegrationsShared'

/** A listed skill plus the project it was found in (project scope). */
type Skill = SkillInfo & { projectId?: string }

const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9_-]*$/

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

function template(name: string): string {
  const title = name.replace(/[-_]+/g, ' ').replace(/^./, (c) => c.toUpperCase())
  return `---
name: ${name}
description: One sentence on what this skill does and when the agent should use it.
---

# ${title}

## When to use
- Situations where this skill applies.

## Steps
1. First step.
2. Second step.

## Notes
- Commands, files or conventions the agent should know about.
`
}

interface ParsedSkill {
  name: string
  description: string
  body: string
  /** Front-matter keys besides name and description. */
  extraKeys: string[]
  errors: string[]
  warnings: string[]
}

/** Parse and validate SKILL.md the way the engine reads it. */
export function parseSkill(text: string): ParsedSkill {
  const out: ParsedSkill = { name: '', description: '', body: '', extraKeys: [], errors: [], warnings: [] }
  const t = text.replace(/^\uFEFF/, '')
  if (!t.startsWith('---')) {
    out.errors.push('SKILL.md must start with a front matter block: a line with --- at the very top.')
    out.body = t
    return out
  }
  const rest = t.slice(3)
  const end = rest.indexOf('\n---')
  if (end < 0) {
    out.errors.push('The front matter block is not closed: add a line with --- after the description.')
    return out
  }
  out.body = rest.slice(end + 4).replace(/^[\r\n]+/, '')
  for (const line of rest.slice(0, end).split(/\r?\n/)) {
    const i = line.indexOf(':')
    if (i < 0 || /^\s/.test(line)) continue
    const key = line.slice(0, i).trim()
    const value = line
      .slice(i + 1)
      .trim()
      .replace(/^["']|["']$/g, '')
    if (key === 'name') out.name = value
    else if (key === 'description') out.description = value
    else if (key) out.extraKeys.push(key)
  }
  if (!out.name) out.errors.push('Front matter needs a name, e.g. “name: deploy”.')
  if (!out.description) out.errors.push('Front matter needs a description: one line telling the agent when to use the skill.')
  else if (/^[>|][+-]?$/.test(out.description)) out.errors.push('Write the description on a single line (block scalars like “>” are not supported).')
  if (out.name && !NAME_RE.test(out.name)) out.warnings.push('Use letters, digits, - and _ in the name so $name mentions work.')
  if (out.description.length > 400) out.warnings.push('Long descriptions cost prompt tokens on every request; keep it to a sentence or two.')
  if (!out.body.trim()) out.warnings.push('The body is empty: add the instructions the agent should follow.')
  return out
}

/** Full SKILL.md text for a listed skill (exact file contents when readable). */
async function readSkillText(s: Skill): Promise<string> {
  try {
    const r = (await window.odex.fs.read(s.path)) as { kind: string; text?: string }
    if (r.kind === 'text' && typeof r.text === 'string') return r.text
  } catch {
    /* fall back to the engine */
  }
  const r = await call('skills/read', { name: s.name })
  return `---\nname: ${r.skill.name}\ndescription: ${r.skill.description}\n---\n\n${r.body}`
}

// ------------------------------------------------------------------ editor

type EditorMode = { kind: 'new' } | { kind: 'edit'; skill: Skill } | { kind: 'view'; skill: Skill }

function SkillEditor(props: { mode: EditorMode; projects: Project[]; existing: Skill[]; onClose: () => void; onSaved: () => void }) {
  const { mode } = props
  const skill = mode.kind === 'new' ? null : mode.skill
  const readOnly = mode.kind === 'view'
  const [text, setText] = useState<string | null>(mode.kind === 'new' ? template('my-skill') : null)
  const [target, setTarget] = useState<string>('user')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!skill) return
    let alive = true
    readSkillText(skill)
      .then((t) => alive && setText(t))
      .catch((e) => alive && setError(errText(e)))
    return () => {
      alive = false
    }
  }, [skill])

  const parsed = useMemo(() => parseSkill(text ?? ''), [text])
  const renamed = !!skill && parsed.name !== skill.name
  const errors = [...parsed.errors]
  if (parsed.name && (mode.kind === 'new' || renamed)) {
    if (!NAME_RE.test(parsed.name)) errors.push('Name can only use letters, digits, - and _ (it becomes the folder name and the $mention).')
    const clash = props.existing.find((s) => s.name.toLowerCase() === parsed.name.toLowerCase() && s.path !== skill?.path)
    if (clash) errors.push(`A skill named “${parsed.name}” already exists (${clash.scope}).`)
  }
  const warnings = parsed.warnings.filter((w) => !(errors.length && w.startsWith('Use letters')))

  const save = async () => {
    if (text == null || errors.length) return
    setBusy(true)
    setError(null)
    try {
      if (skill && !renamed) {
        // same skill: write the file as typed (keeps any extra front matter)
        await window.odex.fs.write(skill.path, text)
      } else {
        const scope: SkillScope = skill ? skill.scope : target === 'user' ? 'user' : 'project'
        const project = skill?.projectId ? props.projects.find((p) => p.id === skill.projectId) : props.projects.find((p) => p.id === target)
        const r = await call('skills/write', {
          name: parsed.name,
          description: parsed.description,
          body: parsed.body,
          scope,
          projectPath: scope === 'project' && project ? projectRoot(project) : null,
        })
        if (parsed.extraKeys.length) await window.odex.fs.write(r.skill.path, text)
        if (skill && renamed) await call('skills/delete', { name: skill.name })
      }
      toast(`Saved $${parsed.name}`, 'success')
      props.onSaved()
      props.onClose()
    } catch (e) {
      setError(errText(e))
    } finally {
      setBusy(false)
    }
  }

  const title = mode.kind === 'new' ? 'New skill' : readOnly ? `$${skill!.name}` : `Edit $${skill!.name}`
  return (
    <Modal
      wide
      title={title}
      onClose={props.onClose}
      footer={
        readOnly ? (
          <button className="btn btn-primary" onClick={props.onClose}>
            Close
          </button>
        ) : (
          <>
            <button className="btn" onClick={props.onClose}>
              Cancel
            </button>
            <button className="btn btn-primary" disabled={busy || text == null || errors.length > 0} onClick={() => void save()}>
              {busy ? 'Saving…' : mode.kind === 'new' ? 'Create skill' : 'Save'}
            </button>
          </>
        )
      }
    >
      <div className="int-form">
        {skill && (
          <div className="xs subtle mono selectable ellipsis" title={skill.path}>
            {skill.path}
          </div>
        )}
        {mode.kind === 'new' && (
          <div className="field">
            <label htmlFor="skill-target">Save to</label>
            <select id="skill-target" className="select" value={target} onChange={(e) => setTarget(e.target.value)} style={{ maxWidth: 420 }}>
              <option value="user">User skills (~/.odex/skills) · every project</option>
              {props.projects.map((p) => (
                <option key={p.id} value={p.id}>
                  Project {p.name} (.odex/skills)
                </option>
              ))}
            </select>
          </div>
        )}
        {readOnly && skill?.scope === 'plugin' && <div className="xs subtle">Plugin skills are read-only. Edit them in the plugin’s source and reinstall it.</div>}
        {text == null && !error ? (
          <span className="spinner" />
        ) : (
          <textarea
            className="textarea int-editor"
            aria-label="SKILL.md"
            spellCheck={false}
            readOnly={readOnly}
            value={text ?? ''}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Tab' && !readOnly) {
                e.preventDefault()
                const el = e.currentTarget
                const s = el.selectionStart
                const next = `${el.value.slice(0, s)}  ${el.value.slice(el.selectionEnd)}`
                setText(next)
                requestAnimationFrame(() => el.setSelectionRange(s + 2, s + 2))
              } else if (e.key === 's' && (e.ctrlKey || e.metaKey) && !readOnly) {
                e.preventDefault()
                void save()
              }
            }}
          />
        )}
        {!readOnly && text != null && (
          <div className="int-validation" aria-live="polite" data-testid="skill-validation">
            {errors.map((e) => (
              <span key={e} className="err">
                ✕ {e}
              </span>
            ))}
            {warnings.map((w) => (
              <span key={w} className="warn">
                ! {w}
              </span>
            ))}
            {errors.length === 0 && <span className="ok">✓ Front matter is valid · mention it as ${parsed.name}</span>}
          </div>
        )}
        {error && (
          <div className="int-item-error" style={{ margin: 0 }} role="alert">
            {error}
          </div>
        )}
      </div>
    </Modal>
  )
}

// ------------------------------------------------------------------ import

function ImportModal(props: { projects: Project[]; onClose: () => void; onDone: () => void }) {
  const [source, setSource] = useState('')
  const [target, setTarget] = useState('user')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const run = async () => {
    setBusy(true)
    setError(null)
    try {
      const project = props.projects.find((p) => p.id === target)
      await call('skills/import', { source: source.trim(), scope: project ? 'project' : 'user', projectPath: project ? projectRoot(project) : null })
      toast('Skills imported', 'success')
      props.onDone()
      props.onClose()
    } catch (e) {
      setError(errText(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      title="Import skills"
      onClose={props.onClose}
      footer={
        <>
          <button className="btn" onClick={props.onClose}>
            Cancel
          </button>
          <button className="btn btn-primary" disabled={busy || !source.trim()} onClick={() => void run()}>
            {busy ? 'Importing…' : 'Import'}
          </button>
        </>
      }
    >
      <div className="int-form">
        <div className="field">
          <label htmlFor="skill-source">Source</label>
          <div className="row" style={{ gap: 6 }}>
            <input id="skill-source" className="input mono" value={source} onChange={(e) => setSource(e.target.value)} placeholder="C:\\path\\to\\skill or https://github.com/…/skills.git" autoComplete="off" />
            <button
              type="button"
              className="btn btn-sm"
              onClick={async () => {
                const [dir] = await window.odex.dialog.openFolder()
                if (dir) setSource(dir)
              }}
            >
              <FolderOpen size={13} /> Browse
            </button>
          </div>
          <span className="hint">A folder with SKILL.md, a folder of skill folders, a SKILL.md file, or a git URL. Files are copied.</span>
        </div>
        <div className="field">
          <label htmlFor="skill-import-target">Import into</label>
          <select id="skill-import-target" className="select" value={target} onChange={(e) => setTarget(e.target.value)}>
            <option value="user">User skills (~/.odex/skills)</option>
            {props.projects.map((p) => (
              <option key={p.id} value={p.id}>
                Project {p.name} (.odex/skills)
              </option>
            ))}
          </select>
        </div>
        {error && (
          <div className="int-item-error" style={{ margin: 0 }} role="alert">
            {error}
          </div>
        )}
      </div>
    </Modal>
  )
}

// ------------------------------------------------------------------ panel

interface Group {
  key: string
  title: string
  /** Folder the skills live in (shown in code font). */
  path?: string
  hint: string
  skills: Skill[]
}

export function SkillsSettings() {
  const projects = useTrustedProjects()
  const [skills, setSkills] = useState<Skill[] | null>(null)
  const [filter, setFilter] = useState('')
  const [editor, setEditor] = useState<EditorMode | null>(null)
  const [importing, setImporting] = useState(false)
  const [busy, setBusy] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const lists = await Promise.all([
        call('skills/list', { cwd: null }).then((r) => r.skills as Skill[]),
        ...projects.map((p) =>
          call('skills/list', { cwd: projectRoot(p) })
            .then((r) => r.skills.filter((s) => s.scope === 'project').map((s) => ({ ...s, projectId: p.id })))
            .catch(() => [] as Skill[]),
        ),
      ])
      const seen = new Set<string>()
      const merged: Skill[] = []
      for (const list of lists)
        for (const s of list) {
          const k = s.path.replace(/\\/g, '/').toLowerCase()
          if (seen.has(k)) continue
          seen.add(k)
          merged.push(s)
        }
      setSkills(merged)
    } catch (e) {
      toast(`Skills: ${errText(e)}`, 'error')
      setSkills((s) => s ?? [])
    }
  }, [projects])

  useEffect(() => {
    void load()
  }, [load])

  const groups = useMemo<Group[]>(() => {
    const q = filter.trim().toLowerCase()
    const list = (skills ?? []).filter((s) => !q || s.name.toLowerCase().includes(q) || s.description.toLowerCase().includes(q))
    const out: Group[] = []
    const user = list.filter((s) => s.scope === 'user')
    if (user.length) out.push({ key: 'user', title: 'User', path: '~/.odex/skills', hint: 'available in every project', skills: user })
    for (const p of projects) {
      const ps = list.filter((s) => s.scope === 'project' && s.projectId === p.id)
      const root = projectRoot(p)
      const sep = root.includes('\\') ? '\\' : '/'
      if (ps.length) out.push({ key: `project:${p.id}`, title: `Project · ${p.name}`, path: `${root}${sep}.odex${sep}skills`, hint: 'this project only', skills: ps })
    }
    const plugins = [...new Set(list.filter((s) => s.scope === 'plugin').map((s) => s.plugin ?? 'plugin'))]
    for (const id of plugins) out.push({ key: `plugin:${id}`, title: `Plugin · ${id}`, hint: 'Managed by the plugin', skills: list.filter((s) => s.scope === 'plugin' && (s.plugin ?? 'plugin') === id) })
    const other = list.filter((s) => !['user', 'project', 'plugin'].includes(s.scope))
    if (other.length) out.push({ key: 'builtin', title: 'Built-in', hint: 'Ships with Odex', skills: other })
    return out
  }, [skills, filter, projects])

  const toggle = async (s: Skill, enabled: boolean) => {
    setBusy(s.path)
    try {
      await call('skills/setEnabled', { name: s.name, enabled })
      await load()
    } catch (e) {
      toast(errText(e), 'error')
    } finally {
      setBusy(null)
    }
  }

  const remove = async (s: Skill) => {
    if (!(await A.confirmDialog('Delete skill', `Delete $${s.name}? Its folder (${s.path.replace(/[\\/]SKILL\.md$/i, '')}) will be removed from disk.`, 'Delete', true))) return
    try {
      await call('skills/delete', { name: s.name })
      toast(`Deleted $${s.name}`)
      await load()
    } catch (e) {
      toast(errText(e), 'error')
    }
  }

  const total = skills?.length ?? 0
  return (
    <div className="int-panel">
      <p className="int-intro">
        Skills are reusable instructions in <code>SKILL.md</code> files. Only each enabled skill’s name and description go into the prompt; the agent reads the full file when a task matches.
      </p>
      <div className="int-note">
        <Info size={15} />
        <div>
          Type <b>$</b> in the composer to mention a skill, for example <code>$deploy</code>. A mention tells the agent to load and follow that skill for your message. Skills live in <code>~/.odex/skills/&lt;name&gt;/SKILL.md</code> (user) or <code>.odex/skills/</code> in a trusted project.
        </div>
      </div>
      <section>
        <SectionHead title={`Skills${total ? ` (${total})` : ''}`}>
          <button className="btn btn-sm" onClick={() => void load()}>
            <RefreshCw size={13} /> Refresh
          </button>
          <button className="btn btn-sm" onClick={() => setImporting(true)}>
            <Download size={13} /> Import
          </button>
          <button className="btn btn-sm btn-primary" onClick={() => setEditor({ kind: 'new' })}>
            <Plus size={13} /> New skill
          </button>
        </SectionHead>
        {total > 6 && (
          <div className="row" style={{ position: 'relative', marginBottom: 10 }}>
            <Search size={13} style={{ position: 'absolute', left: 9, color: 'var(--fg-subtle)' }} />
            <input className="input" style={{ paddingLeft: 28 }} placeholder="Filter skills" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter skills" />
          </div>
        )}
        {skills == null ? (
          <span className="spinner" />
        ) : total === 0 ? (
          <div className="int-empty">No skills yet. Create one, or import a folder that contains SKILL.md.</div>
        ) : groups.length === 0 ? (
          <div className="int-empty">No skills match “{filter}”.</div>
        ) : (
          groups.map((g) => (
            <div key={g.key} className="int-group" role="group" aria-label={g.title}>
              <div className="int-group-title">
                <span className="section-title">{g.title}</span>
                <span className="int-group-hint ellipsis" title={g.path ?? g.hint}>
                  {g.path && <span className="mono">{g.path} · </span>}
                  {g.hint}
                </span>
              </div>
              <div className="card">
                {g.skills.map((s) => {
                  const managed = s.scope === 'plugin' || !['user', 'project'].includes(s.scope)
                  return (
                    <div key={s.path} className="int-row" data-testid={`skill-${s.name}`}>
                      <div className="int-row-main">
                        <div className="int-row-title">
                          <span className="mono" style={{ fontWeight: 600 }}>
                            ${s.name}
                          </span>
                          {!s.enabled && <span className="badge">disabled</span>}
                        </div>
                        <div className="int-row-desc">{s.description || <i>No description</i>}</div>
                      </div>
                      <div className="int-item-actions">
                        {busy === s.path && <span className="spinner" />}
                        {managed ? (
                          <button className="icon-btn sm" title="View" aria-label={`View ${s.name}`} onClick={() => setEditor({ kind: 'view', skill: s })}>
                            <Eye size={13} />
                          </button>
                        ) : (
                          <button className="icon-btn sm" title="Edit" aria-label={`Edit ${s.name}`} onClick={() => setEditor({ kind: 'edit', skill: s })}>
                            <Pencil size={13} />
                          </button>
                        )}
                        <button className="icon-btn sm" title="Show in folder" aria-label={`Show ${s.name} in folder`} onClick={() => void window.odex.shell.showItem(s.path)}>
                          <FolderOpen size={13} />
                        </button>
                        {!managed && (
                          <button className="icon-btn sm" title="Delete" aria-label={`Delete ${s.name}`} onClick={() => void remove(s)}>
                            <Trash2 size={13} />
                          </button>
                        )}
                        <Toggle checked={s.enabled} disabled={busy === s.path} label={`Enable ${s.name}`} onChange={(v) => void toggle(s, v)} />
                      </div>
                    </div>
                  )
                })}
              </div>
            </div>
          ))
        )}
      </section>
      {editor && <SkillEditor mode={editor} projects={projects} existing={skills ?? []} onClose={() => setEditor(null)} onSaved={() => void load()} />}
      {importing && <ImportModal projects={projects} onClose={() => setImporting(false)} onDone={() => void load()} />}
    </div>
  )
}
