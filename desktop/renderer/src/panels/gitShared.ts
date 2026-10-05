import type { DiffFile, DiffTarget, Project, Thread } from '@shared/index'
import { useApp } from '@/store/app'

/** Shared helpers for the review and git panels. */

export interface RepoContext {
  threadId: string | null
  thread?: Thread
  project?: Project
  /** Primary working folder (thread worktree/cwd, or the selected project's primary folder). */
  root: string | null
  /** All folders to diff (multi-repo projects). */
  cwds: string[]
}

export function normPath(p: string): string {
  return p.replace(/\\/g, '/').replace(/\/+$/, '')
}

export function samePath(a: string, b: string): boolean {
  const x = normPath(a)
  const y = normPath(b)
  return /^[a-zA-Z]:/.test(x) || /^[a-zA-Z]:/.test(y) ? x.toLowerCase() === y.toLowerCase() : x === y
}

export function isAbsolute(p: string): boolean {
  return /^([a-zA-Z]:[\\/]|[\\/])/.test(p)
}

export function joinPath(root: string, rel: string): string {
  if (isAbsolute(rel)) return rel
  return `${normPath(root)}/${rel.replace(/\\/g, '/').replace(/^\.\//, '')}`
}

/** `abs` relative to `root` when inside it, else null. */
export function relativeTo(root: string, abs: string): string | null {
  const r = normPath(root)
  const a = normPath(abs)
  const win = /^[a-zA-Z]:/.test(r)
  const ra = win ? r.toLowerCase() : r
  const aa = win ? a.toLowerCase() : a
  if (aa === ra) return ''
  if (aa.startsWith(`${ra}/`)) return a.slice(r.length + 1)
  return null
}

/** The repo the panels act on: the selected thread's folder, else the project chosen for new threads. */
export function useRepoContext(): RepoContext {
  const threadId = useApp((s) => (s.ui.view === 'thread' ? s.selectedThreadId : null))
  const thread = useApp((s) => (threadId ? s.threads[threadId]?.thread : undefined))
  const pid = useApp((s) => (thread ? (thread.projectId ?? null) : s.ui.newThreadProjectId))
  const project = useApp((s) => (pid ? s.projects.find((p) => p.id === pid) : undefined))
  if (thread) {
    const root = thread.worktree?.path ?? thread.cwd
    const cwds = [root]
    if (project) {
      const primary = project.folders[project.primary] ?? project.folders[0]
      for (const f of project.folders) if (!samePath(f, primary) && !cwds.some((c) => samePath(c, f))) cwds.push(f)
    }
    return { threadId, thread, project, root: root || null, cwds: root ? cwds : [] }
  }
  if (project) {
    const primary = project.folders[project.primary] ?? project.folders[0]
    const cwds = [primary, ...project.folders.filter((f) => !samePath(f, primary))]
    return { threadId: null, project, root: primary ?? null, cwds }
  }
  return { threadId: null, root: null, cwds: [] }
}

/** Turn git / gh / GitHub API failures into an actionable sentence. */
export function friendlyGitError(raw: string): string {
  const m = raw || 'Unknown error'
  const l = m.toLowerCase()
  if (l.includes('git executable not found')) return 'Git is not installed or not on PATH. Install Git and restart Odex.'
  if (l.includes('gh auth login') || l.includes('not logged into') || l.includes('authentication required') || l.includes('bad credentials') || l.includes('http 401') || l.includes('error 401'))
    return 'GitHub rejected the credentials. Check the token in Settings → Git, or run `gh auth login` in a terminal, then retry.'
  if (l.includes('github token is required'))
    return 'GitHub access is not set up. Add a token in Settings → Git, or install the GitHub CLI (gh) and run `gh auth login`.'
  if (l.includes('rate limit')) return 'GitHub API rate limit reached. Add a token in Settings → Git (or sign in with `gh auth login`) to raise the limit.'
  if (l.includes('github api error 404') || l.includes('github api error 403'))
    return 'GitHub denied access: the repository is private, missing, or the token lacks permission. Check the token in Settings → Git, or sign in with `gh auth login`.'
  if (l.includes('not a github remote') || l.includes('no git remotes') || l.includes("no such remote 'origin'") || l.includes('none of the git remotes') || (l.includes('remote get-url') && l.includes('origin')))
    return 'This repository has no GitHub remote named origin. Add one (git remote add origin …) to work with pull requests.'
  if (l.includes('must first push') || l.includes('no commits between') || l.includes('could not find any commits'))
    return 'Push this branch (with commits ahead of the base) before creating a pull request.'
  if (l.includes('already exists') && l.includes('pull request')) return 'A pull request for this branch already exists.'
  if (l.includes('not a git repository')) return 'This folder is not a git repository.'
  if (l.includes('could not resolve host') || l.includes('network error') || l.includes('unable to access')) return 'Network error: GitHub could not be reached.'
  if (l.includes('rejected') && (l.includes('non-fast-forward') || l.includes('fetch first'))) return 'The remote has commits you do not have. Pull (or rebase) first, or push with force-with-lease.'
  if (l.includes('no upstream') || l.includes('has no upstream branch')) return 'This branch has no upstream yet. Push with “Set upstream” checked.'
  return m
}

export function isGhSetupError(raw: string): boolean {
  return friendlyGitError(raw) !== raw
}

// ------------------------------------------------- cross-panel review requests

let pendingReview: { target: DiffTarget; path?: string; at: number } | null = null

/** Open the review tab on a given target (and scroll to `path`). */
export function showInReview(target: DiffTarget, path?: string): void {
  pendingReview = { target, path, at: Date.now() }
  window.dispatchEvent(new CustomEvent('odex:review-target', { detail: pendingReview }))
  useApp.getState().setUi({ sidePanelOpen: true, sidePanelTab: 'review' })
}

export function takeReviewRequest(): { target: DiffTarget; path?: string } | null {
  const r = pendingReview
  pendingReview = null
  return r && Date.now() - r.at < 10_000 ? r : null
}

/** This window is a detached review pop-out (`?panel=review`). */
export function isDetachedReviewWindow(): boolean {
  return new URLSearchParams(location.search).get('panel') === 'review'
}

/**
 * "Open review" (Ctrl+Shift+G): the review tab of the side panel, or a pop-out window with only
 * the review panel when Settings → Code review → delivery is "detached".
 */
export function openReview(): void {
  const s = useApp.getState()
  const threadId = s.ui.view === 'thread' ? s.selectedThreadId : null
  if (s.settings?.reviewDelivery === 'detached' && threadId && !isDetachedReviewWindow()) {
    void window.odex.win.newWindow(threadId, 'review')
    return
  }
  s.setUi({ sidePanelOpen: true, sidePanelTab: 'review' })
}

/** First changed line of a file diff on the new side (for "open at change"). */
export function firstChangedLine(f: DiffFile): number | undefined {
  for (const h of f.hunks) {
    for (const l of h.lines) if (l.kind === 'add' && l.newNo != null) return l.newNo
    for (const l of h.lines) if (l.kind === 'del') return Math.max(1, h.newStart)
  }
  return f.hunks[0]?.newStart || undefined
}

// ------------------------------------------------------------------ pull requests

/** Badge style for a PR state. */
export const PR_STATE_CLASS: Record<string, string> = { open: 'success', draft: '', merged: 'accent', closed: 'danger' }

/** The message that asks the agent to fix a failing check, with its (capped) log. */
export function fixCheckMessage(check: string, pr: { number: number; title?: string | null; head?: string | null } | null, log: string, truncated: boolean): string {
  const where = pr ? ` on pull request #${pr.number}${pr.title ? ` (${pr.title})` : ''}${pr.head ? `, branch ${pr.head}` : ''}` : ''
  return [
    `The CI check "${check}" failed${where}. Find the cause and fix it, then tell me what you changed.`,
    '',
    `Check output${truncated ? ' (trimmed to the most relevant part)' : ''}:`,
    '```text',
    log.replace(/```/g, '`​``'),
    '```',
  ].join('\n')
}
