import { useCallback } from 'react'
import { ShieldCheck } from 'lucide-react'
import { useApp } from '@/store/app'
import { call, toast } from '@/lib/rpc'
import { openSettings } from '@/lib/actions'
import { Row, useSetting } from '@/views/settings/GeneralSettings'
import { Section, useEngineConfig } from '@/views/settings/ConfigSettings'
import { PromptField } from '@/views/settings/GitSettings'

/** Settings → Code review: standing review guidelines, the reviewer model, where the review pane opens. */
export function CodeReviewSettings() {
  const { cfg, error, write } = useEngineConfig()
  const models = useApp((s) => s.models)
  const roles = useApp((s) => s.roles)
  const [delivery, setDelivery] = useSetting('reviewDelivery')
  const saveInstructions = useCallback((v: string) => write([{ keyPath: 'review_instructions', value: v.trim() ? v : null }]), [write])
  if (error && !cfg) return <div className="gp-note error">{error}</div>
  if (!cfg) return <div className="spinner" aria-label="Loading" />

  const reviewer = cfg.user.roles?.reviewer ?? ''
  const fallback = roles.main ? (models.find((m) => m.key === roles.main)?.displayName ?? roles.main) : 'the main model'
  const setReviewer = async (key: string) => {
    try {
      await call('config/write', { edits: [{ keyPath: 'roles.reviewer', value: key || null }] })
      await useApp.getState().refreshModels()
      toast(key ? `Reviews now use ${models.find((m) => m.key === key)?.displayName ?? key}` : 'Reviews use the main model')
    } catch (e) {
      toast((e as Error).message, 'error')
    }
  }

  return (
    <div className="sx-panel">
      <Section title="Review guidelines" desc="Added to every review the agent runs (/review and “Ask agent to review” in the review pane), in this and every project. A trusted project can set its own in .odex/config.toml.">
        <PromptField
          initial={cfg.user.review_instructions ?? ''}
          label="Review instructions"
          placeholder={'e.g. Flag any SQL built with string formatting.\nCheck that new endpoints have tests.\nIgnore generated files under src/gen/.'}
          save={saveInstructions}
        />
      </Section>

      <Section title="Reviewer model">
        <Row label="Model for reviews" hint={`The reviewer role. When unset, reviews use ${fallback}.`}>
          <select className="select" style={{ minWidth: 220 }} aria-label="Reviewer model" value={reviewer} onChange={(e) => void setReviewer(e.target.value)}>
            <option value="">Same as the main model</option>
            {reviewer && !models.some((m) => m.key === reviewer) && <option value={reviewer}>{reviewer} (not available)</option>}
            {models.map((m) => (
              <option key={m.key} value={m.key}>
                {m.displayName}
                {m.available ? '' : ' (offline)'}
              </option>
            ))}
          </select>
        </Row>
      </Section>

      <Section title="Review pane">
        <Row label="Open review in" hint="Where Open review (Ctrl+Shift+G) shows a thread's changes: the side panel, or a separate window with only the review pane.">
          <select className="select" style={{ minWidth: 160 }} aria-label="Review delivery" value={delivery ?? 'inline'} onChange={(e) => setDelivery(e.target.value as 'inline' | 'detached')}>
            <option value="inline">Side panel</option>
            <option value="detached">Separate window</option>
          </select>
        </Row>
      </Section>

      <Section title="Automatic review" desc="The reviewer model can also judge actions that need approval while the agent works.">
        <button className="btn btn-sm" onClick={() => openSettings('permissions')}>
          <ShieldCheck size={13} /> Automatic review settings
        </button>
      </Section>
    </div>
  )
}
