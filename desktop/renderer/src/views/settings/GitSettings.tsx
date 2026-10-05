import { useCallback, useEffect, useState } from 'react'
import { KeyRound, RotateCcw, Trash2 } from 'lucide-react'
import { toast } from '@/lib/rpc'
import { Toggle } from '@/components/ui'
import { Row } from '@/views/settings/GeneralSettings'
import { Callout, SaveState, Section, useAutosave, useEngineConfig } from '@/views/settings/ConfigSettings'
import '@/styles/review.css'

const DEFAULT_PREFIX = 'odex/'
const TOKEN_KEY = 'github:token'

/** A config-backed multi-line prompt field with autosave. */
export function PromptField({ initial, label, placeholder, save }: { initial: string; label: string; placeholder: string; save: (v: string) => Promise<unknown> }) {
  const [text, setText] = useState(initial)
  const auto = useAutosave(save)
  return (
    <>
      <textarea
        className="textarea sx-prose"
        value={text}
        rows={4}
        placeholder={placeholder}
        aria-label={label}
        onChange={(e) => {
          setText(e.target.value)
          auto.schedule(e.target.value)
        }}
        onBlur={() => void auto.flush()}
      />
      <div className="row xs subtle" style={{ marginTop: 4, minHeight: 16 }}>
        <span className="grow" />
        <SaveState state={auto.state} />
      </div>
    </>
  )
}

function BranchPrefix({ value, onSave }: { value: string | null | undefined; onSave: (v: string | null) => Promise<boolean> }) {
  const [text, setText] = useState(value ?? DEFAULT_PREFIX)
  const [shown, setShown] = useState(value)
  if (shown !== value) {
    setShown(value)
    setText(value ?? DEFAULT_PREFIX)
  }
  const commit = async () => {
    const v = text.trim()
    if ((value ?? DEFAULT_PREFIX) === v) return
    // the default is stored as "unset" so it follows future defaults
    if (await onSave(v === DEFAULT_PREFIX ? null : v)) toast(v ? `New worktree branches start with ${v}` : 'New worktree branches have no prefix')
  }
  return (
    <div className="row" style={{ gap: 6 }}>
      <input
        className="input mono"
        style={{ width: 180 }}
        aria-label="Branch prefix"
        value={text}
        placeholder="(no prefix)"
        onChange={(e) => setText(e.target.value)}
        onBlur={() => void commit()}
        onKeyDown={(e) => {
          if (e.key === 'Enter') (e.target as HTMLInputElement).blur()
        }}
      />
      {value != null && (
        <button className="icon-btn sm" title={`Reset to ${DEFAULT_PREFIX}`} aria-label="Reset branch prefix" onClick={() => void onSave(null)}>
          <RotateCcw size={13} />
        </button>
      )}
    </div>
  )
}

/** GitHub token kept in the OS-encrypted secret store (never in config.toml). */
function GithubToken() {
  const [stored, setStored] = useState<boolean | null>(null)
  const [value, setValue] = useState('')
  const [busy, setBusy] = useState(false)
  const refresh = useCallback(async () => setStored(await window.odex.secrets.has(TOKEN_KEY)), [])
  useEffect(() => {
    void refresh()
  }, [refresh])

  const save = async (v: string | null) => {
    setBusy(true)
    try {
      await window.odex.secrets.set(TOKEN_KEY, v)
      setValue('')
      toast(v ? 'GitHub token saved' : 'GitHub token removed', 'success')
    } catch (e) {
      toast(`Could not store the token: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(false)
      void refresh()
    }
  }

  return (
    <>
      <Row
        label="Personal access token"
        hint={stored ? 'A token is stored, encrypted by the operating system. Enter a new one to replace it.' : 'Not set. Odex falls back to the GITHUB_TOKEN / GH_TOKEN environment variables.'}
      >
        <form
          className="row"
          style={{ gap: 6 }}
          onSubmit={(e) => {
            e.preventDefault()
            if (value.trim()) void save(value.trim())
          }}
        >
          <input
            className="input mono"
            type="password"
            autoComplete="off"
            spellCheck={false}
            style={{ width: 240 }}
            aria-label="GitHub token"
            placeholder={stored ? '••••••••••••' : 'github_pat_… or ghp_…'}
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
          <button type="submit" className="btn btn-sm btn-primary" disabled={busy || !value.trim()}>
            <KeyRound size={12} /> Save
          </button>
          {stored && (
            <button type="button" className="btn btn-sm btn-ghost" disabled={busy} onClick={() => void save(null)} aria-label="Remove GitHub token">
              <Trash2 size={12} /> Remove
            </button>
          )}
        </form>
      </Row>
      <p className="xs subtle" style={{ margin: '6px 0 0' }}>
        Used to create, view and review pull requests and to read check logs. A fine-grained token needs read/write access to Pull requests and read access to Contents, Checks and Actions on the repositories you use (a classic token needs the <code>repo</code> scope).
        When the GitHub CLI (<code>gh</code>) is installed, Odex runs it with this token. Changes apply immediately.
      </p>
    </>
  )
}

/** Settings → Git: worktree branch prefix, force pushes, commit / PR prompt additions, GitHub token. */
export function GitSettings() {
  const { cfg, error, write } = useEngineConfig()
  const saveCommit = useCallback((v: string) => write([{ keyPath: 'git.commit_prompt', value: v.trim() ? v : null }]), [write])
  const savePr = useCallback((v: string) => write([{ keyPath: 'git.pr_prompt', value: v.trim() ? v : null }]), [write])
  if (error && !cfg) return <div className="gp-note error">{error}</div>
  if (!cfg) return <div className="spinner" aria-label="Loading" />
  const g = cfg.user.git ?? {}
  const effectivePrefix = cfg.effective.git?.branch_prefix
  return (
    <div className="sx-panel">
      <Section title="Branches">
        <Row
          label="Branch prefix"
          hint={
            <>
              Threads started in worktree mode get a branch named <span className="mono">{effectivePrefix ?? DEFAULT_PREFIX}&lt;thread&gt;</span>. Clear the field for no prefix.
            </>
          }
        >
          <BranchPrefix value={g.branch_prefix} onSave={(v) => write([{ keyPath: 'git.branch_prefix', value: v }])} />
        </Row>
        {effectivePrefix != null && effectivePrefix !== (g.branch_prefix ?? null) && (
          <Callout kind="warning">
            The active profile or project sets the prefix to <span className="mono">{effectivePrefix || '(none)'}</span>.
          </Callout>
        )}
      </Section>

      <Section title="Pushing">
        <Row label="Allow force push" hint="Offers “Force with lease” in the push dialog. While it is off, Odex refuses force pushes. Only your user settings can turn it on, never a project's config.">
          <Toggle checked={!!g.allow_force_push} onChange={(v) => void write([{ keyPath: 'git.allow_force_push', value: v ? true : null }])} label="Allow force push" />
        </Row>
      </Section>

      <Section title="Commit messages" desc="Extra instructions for the utility model when it writes a commit message (Generate message in the Git panel).">
        <PromptField initial={g.commit_prompt ?? ''} label="Commit message instructions" placeholder="e.g. Use Conventional Commits (feat:, fix:, chore:). Mention the ticket id from the branch name." save={saveCommit} />
      </Section>

      <Section title="Pull requests" desc="Extra instructions for the utility model when it drafts a pull request title and description.">
        <PromptField initial={g.pr_prompt ?? ''} label="Pull request instructions" placeholder="e.g. Add a Risks section. Keep the summary under five bullet points." save={savePr} />
      </Section>

      <Section title="GitHub">
        <GithubToken />
      </Section>
    </div>
  )
}
