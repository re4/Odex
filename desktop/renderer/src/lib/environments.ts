import type { Environment, PerOs, Project, ProjectAction, Thread } from '@shared/index'

export type OsKey = 'windows' | 'macos' | 'linux'

export const OS_LABEL: Record<OsKey, string> = { windows: 'Windows', macos: 'macOS', linux: 'Linux' }

/** The OS this app runs on, as a `PerOs` key. */
export function currentOs(): OsKey {
  const p = window.odex.platform
  return p === 'win32' ? 'windows' : p === 'darwin' ? 'macos' : 'linux'
}

/** The non-blank per-OS value for this OS, else `fallback`. */
export function forOs(per: PerOs | null | undefined, fallback: string): string {
  const v = per?.[currentOs()]
  return v && v.trim() ? v : fallback
}

/** The command an action runs on this OS. */
export function actionCommand(a: ProjectAction): string {
  return forOs(a.commands, a.command)
}

/**
 * The environment a thread uses, as the engine resolves it: the thread's explicit choice
 * (`''` = none), else the project's default, else the project's first environment.
 */
export function threadEnvironment(thread: Pick<Thread, 'environmentId'> | null | undefined, project: Project | null | undefined): Environment | null {
  const envs = project?.trusted ? project.environments : []
  if (!envs.length) return null
  const wanted = thread?.environmentId
  if (wanted === '') return null
  const byId = (id: string | null | undefined) => (id ? envs.find((e) => e.id === id) : undefined)
  return byId(wanted) ?? byId(project?.defaultEnvironment) ?? envs[0] ?? null
}

/** Variables of a thread's environment (for integrated terminals and project actions). */
export function threadEnvVars(thread: Pick<Thread, 'environmentId'> | null | undefined, project: Project | null | undefined): Record<string, string> {
  const env = threadEnvironment(thread, project)?.env ?? {}
  return Object.fromEntries(Object.entries(env).filter((kv): kv is [string, string] => typeof kv[1] === 'string'))
}

/** Parse `KEY=value` lines (blank lines and lines without a key are ignored). */
export function parseEnvLines(text: string): Record<string, string> {
  const out: Record<string, string> = {}
  for (const line of text.split('\n')) {
    const i = line.indexOf('=')
    const k = (i < 0 ? line : line.slice(0, i)).trim()
    if (k) out[k] = i < 0 ? '' : line.slice(i + 1).replace(/\r$/, '')
  }
  return out
}

export function envLines(env: Environment['env']): string {
  return Object.entries(env)
    .map(([k, v]) => `${k}=${v ?? ''}`)
    .join('\n')
}
