import { useCallback, useEffect, useState } from 'react'
import { ExternalLink, FolderOpen, RotateCcw, Save } from 'lucide-react'
import { toast } from '@/lib/rpc'
import { openSettings } from '@/lib/actions'
import { Row } from '@/views/settings/GeneralSettings'
import { Callout, NumberField, SaveState, Section, joinPath, readText, useOdexHome, useAutosave, useEngineConfig } from '@/views/settings/ConfigSettings'

function CustomInstructions({ initial, save }: { initial: string; save: (v: string) => Promise<unknown> }) {
  const [text, setText] = useState(initial)
  const auto = useAutosave(save)
  return (
    <>
      <textarea
        className="textarea sx-prose"
        value={text}
        placeholder="e.g. Prefer small, focused commits. Explain trade-offs briefly. Use British spelling in docs."
        onChange={(e) => {
          setText(e.target.value)
          auto.schedule(e.target.value)
        }}
        onBlur={() => void auto.flush()}
        aria-label="Custom instructions"
        rows={6}
      />
      <div className="row xs subtle" style={{ marginTop: 4 }}>
        <span className="grow">{text.length.toLocaleString()} characters</span>
        <SaveState state={auto.state} />
      </div>
    </>
  )
}

function GlobalAgentsMd() {
  const home = useOdexHome()
  const path = home ? joinPath(home, 'AGENTS.md') : ''
  const [text, setText] = useState<string | null>(null)
  const [disk, setDisk] = useState('')
  const [exists, setExists] = useState(false)
  const [busy, setBusy] = useState(false)

  const load = useCallback(async () => {
    if (!path) return
    try {
      const t = await readText(path)
      setExists(t != null)
      setText(t ?? '')
      setDisk(t ?? '')
    } catch (e) {
      toast((e as Error).message, 'error')
    }
  }, [path])
  useEffect(() => {
    void load()
  }, [load])

  const dirty = text != null && text !== disk
  const save = async () => {
    if (text == null) return
    setBusy(true)
    try {
      await window.odex.fs.write(path, text)
      setDisk(text)
      setExists(true)
      toast('Global AGENTS.md saved', 'success')
    } catch (e) {
      toast(`Could not save: ${(e as Error).message}`, 'error')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Section
      title="Global AGENTS.md"
      desc={
        <>
          Instructions for every project, read from <code className="selectable">{path || '~/.odex/AGENTS.md'}</code> at the start of each turn.
        </>
      }
      actions={
        <>
          <SaveState state={dirty ? 'dirty' : 'idle'} />
          {exists && (
            <>
              <button className="icon-btn sm" title="Open in editor" aria-label="Open AGENTS.md in editor" onClick={() => void window.odex.shell.openInEditor(path)}>
                <ExternalLink size={13} />
              </button>
              <button className="icon-btn sm" title="Reveal in folder" aria-label="Reveal AGENTS.md" onClick={() => void window.odex.shell.showItem(path)}>
                <FolderOpen size={13} />
              </button>
            </>
          )}
          <button className="btn btn-sm" disabled={!dirty || busy} onClick={() => setText(disk)}>
            <RotateCcw size={13} /> Revert
          </button>
          <button className="btn btn-sm btn-primary" disabled={!dirty || busy} onClick={() => void save()}>
            <Save size={13} /> Save
          </button>
        </>
      }
    >
      <textarea
        className="textarea sx-editor"
        spellCheck={false}
        value={text ?? ''}
        placeholder={'# My conventions\n\n- Run the tests before saying a change is done.\n- Prefer the standard library over new dependencies.'}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') {
            e.preventDefault()
            if (dirty) void save()
          }
        }}
        aria-label="Global AGENTS.md"
      />
    </Section>
  )
}

export function PersonalizationSettings() {
  const { cfg, write } = useEngineConfig()
  const saveInstructions = useCallback((v: string) => write([{ keyPath: 'custom_instructions', value: v.trim() ? v : null }]), [write])
  if (!cfg) return <div className="spinner" aria-label="Loading" />
  const profile = cfg.activeProfile ? cfg.user.profiles[cfg.activeProfile] : undefined
  return (
    <div className="sx-panel">
      <Section title="Custom instructions" desc="Added to the system prompt of every thread. Use it for how you like to work; put project rules in the project's AGENTS.md instead.">
        {profile?.custom_instructions && (
          <div style={{ marginBottom: 8 }}>
            <Callout kind="warning">
              The active profile <b>{cfg.activeProfile}</b> sets its own custom instructions, which replace these while it is active.
            </Callout>
          </div>
        )}
        <CustomInstructions initial={cfg.user.custom_instructions ?? ''} save={saveInstructions} />
      </Section>

      <GlobalAgentsMd />

      <Section title="Project instructions">
        <div className="sx-help" style={{ marginBottom: 8 }}>
          <p style={{ margin: '0 0 6px' }}>For each turn Odex collects, in order:</p>
          <ol style={{ margin: '0 0 6px', paddingLeft: 20 }}>
            <li>the global AGENTS.md above;</li>
            <li>
              <code>AGENTS.md</code> in every folder from the project root down to the thread&apos;s working directory. An <code>AGENTS.override.md</code> in the same folder replaces it.
            </li>
          </ol>
          <p style={{ margin: 0 }}>
            Deeper files take precedence over shallower ones. Files inside an untrusted project are only read from its working directory. Use <code>/init</code> in a thread to generate one for a repository.
          </p>
        </div>
        <Row label="Maximum AGENTS.md size" hint="project_doc_max_bytes: the combined text is cut at this size (default 32 768 bytes) and never takes more than about 12% of the model's window.">
          <NumberField label="Maximum AGENTS.md size" value={cfg.user.project_doc_max_bytes} def={32768} min={1024} max={1_048_576} step={1024} unit="bytes" onCommit={(v) => void write([{ keyPath: 'project_doc_max_bytes', value: v }])} />
        </Row>
      </Section>

      <Section title="Memories" desc="Odex can remember your preferences, conventions and stack across threads once you approve them.">
        <button className="btn btn-sm" onClick={() => openSettings('memories')}>
          Manage memories
        </button>
      </Section>
    </div>
  )
}
