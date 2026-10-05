import { GitMerge, GitPullRequest, GitPullRequestClosed, GitPullRequestDraft } from 'lucide-react'
import type { ThreadPr } from '@shared/index'
import '@/styles/review.css'

const STATE_LABEL: Record<string, string> = { open: 'open', draft: 'draft', merged: 'merged', closed: 'closed' }

function Icon({ state, size }: { state: string; size: number }) {
  if (state === 'merged') return <GitMerge size={size} aria-hidden />
  if (state === 'closed') return <GitPullRequestClosed size={size} aria-hidden />
  if (state === 'draft') return <GitPullRequestDraft size={size} aria-hidden />
  return <GitPullRequest size={size} aria-hidden />
}

/** Describes a thread's PR for titles and screen readers. */
export function prLabel(pr: ThreadPr): string {
  const checks = pr.checks === 'failure' ? `, ${pr.failedChecks || 1} failing check${pr.failedChecks === 1 ? '' : 's'}` : pr.checks === 'pending' ? ', checks running' : pr.checks === 'success' ? ', checks passed' : ''
  return `Pull request #${pr.number} ${STATE_LABEL[pr.state] ?? pr.state}${checks}${pr.title ? `: ${pr.title}` : ''}`
}

/**
 * PR status of a thread: state colour (open / draft / merged / closed) and a red dot when checks
 * fail. `compact` is the sidebar-row form (icon + number); otherwise it also shows the state.
 */
export function PrBadge({ pr, compact, onClick }: { pr: ThreadPr; compact?: boolean; onClick?: () => void }) {
  const failing = pr.checks === 'failure'
  const label = prLabel(pr)
  const cls = `pr-badge s-${pr.state}${failing ? ' failing' : ''}${compact ? ' compact' : ''}${onClick ? ' clickable' : ''}`
  const body = (
    <>
      <Icon state={pr.state} size={compact ? 11 : 12} />
      <span className="pr-badge-num">#{pr.number}</span>
      {!compact && <span className="pr-badge-state">{STATE_LABEL[pr.state] ?? pr.state}</span>}
      {failing && <span className="pr-badge-fail" aria-hidden />}
      {!failing && pr.checks === 'pending' && !compact && <span className="pr-badge-pending" aria-hidden />}
    </>
  )
  if (onClick) {
    return (
      <button type="button" className={cls} title={label} aria-label={label} onClick={onClick}>
        {body}
      </button>
    )
  }
  return (
    <span className={cls} title={label} aria-label={label} role="img">
      {body}
    </span>
  )
}
