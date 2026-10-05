import type { ReactNode } from 'react'
import type { DesktopSettings } from '@shared/index'
import { useApp } from '@/store/app'
import { Toggle } from '@/components/ui'

export function Row({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="row" style={{ padding: '10px 0', borderBottom: '1px solid var(--border)', alignItems: 'center', gap: 16 }}>
      <div className="grow" style={{ minWidth: 0 }}>
        <div>{label}</div>
        {hint && <div className="xs subtle">{hint}</div>}
      </div>
      <div style={{ flex: 'none' }}>{children}</div>
    </div>
  )
}

export function useSetting<K extends keyof DesktopSettings>(key: K): [DesktopSettings[K] | undefined, (v: DesktopSettings[K]) => void] {
  const value = useApp((s) => s.settings?.[key])
  const set = (v: DesktopSettings[K]) => void useApp.getState().setSettings({ [key]: v } as Partial<DesktopSettings>)
  return [value, set]
}

function Select<T extends string>({ value, options, onChange, label }: { value: T | undefined; options: Array<[T, string]>; onChange: (v: T) => void; label: string }) {
  return (
    <select className="select" value={value} onChange={(e) => onChange(e.target.value as T)} aria-label={label} style={{ minWidth: 160 }}>
      {options.map(([v, l]) => (
        <option key={v} value={v}>
          {l}
        </option>
      ))}
    </select>
  )
}

const ACCENTS = ['#2f6feb', '#7c3aed', '#0d9488', '#db2777', '#ea580c', '#16a34a', '#64748b']

export function GeneralSettings() {
  const s = useApp((st) => st.settings)
  const set = (patch: Partial<DesktopSettings>) => void useApp.getState().setSettings(patch)
  if (!s) return null
  return (
    <div>
      <h3 className="section-title">Appearance</h3>
      <Row label="Theme">
        <Select label="Theme" value={s.theme} options={[['system', 'System'], ['light', 'Light'], ['dark', 'Dark']]} onChange={(v) => set({ theme: v })} />
      </Row>
      <Row label="Accent color">
        <div className="row" style={{ gap: 6 }}>
          {ACCENTS.map((c) => (
            <button key={c} aria-label={`Accent ${c}`} aria-pressed={s.accent === c} onClick={() => set({ accent: c })} style={{ width: 20, height: 20, borderRadius: '50%', background: c, border: s.accent === c ? '2px solid var(--fg)' : '2px solid transparent', cursor: 'pointer' }} />
          ))}
          <input type="color" value={s.accent} onChange={(e) => set({ accent: e.target.value })} aria-label="Custom accent" style={{ width: 28, height: 22, border: 'none', background: 'none' }} />
        </div>
      </Row>
      <Row label="Density">
        <Select label="Density" value={s.density} options={[['comfortable', 'Comfortable'], ['compact', 'Compact']]} onChange={(v) => set({ density: v })} />
      </Row>
      <Row label="Font size">
        <input className="input" type="number" min={10} max={22} value={s.fontSize} onChange={(e) => set({ fontSize: Math.min(22, Math.max(10, Number(e.target.value) || 13)) })} style={{ width: 80 }} aria-label="Font size" />
      </Row>
      <Row label="UI font">
        <input className="input" value={s.uiFont} onChange={(e) => set({ uiFont: e.target.value })} style={{ width: 280 }} aria-label="UI font" />
      </Row>
      <Row label="Code font">
        <input className="input" value={s.codeFont} onChange={(e) => set({ codeFont: e.target.value })} style={{ width: 280 }} aria-label="Code font" />
      </Row>
      <Row label="Reduce motion">
        <Select label="Reduce motion" value={s.reducedMotion} options={[['system', 'System'], ['on', 'On'], ['off', 'Off']]} onChange={(v) => set({ reducedMotion: v })} />
      </Row>
      <Row label="Show reasoning" hint="Show the model's thinking in threads (collapsed)">
        <Toggle checked={s.showReasoning} onChange={(v) => set({ showReasoning: v })} label="Show reasoning" />
      </Row>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Composer
      </h3>
      <Row label="Enter sends" hint={s.enterSends ? 'Enter sends, Shift+Enter adds a line' : 'Ctrl+Enter sends, Enter adds a line'}>
        <Toggle checked={s.enterSends} onChange={(v) => set({ enterSends: v })} label="Enter sends" />
      </Row>
      <Row label="Follow-ups while the agent works" hint="Queue waits for the turn to finish; steer injects the message into the running turn">
        <Select label="Follow-up behavior" value={s.followUpBehavior} options={[['queue', 'Queue'], ['steer', 'Steer']]} onChange={(v) => set({ followUpBehavior: v })} />
      </Row>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Terminal & editor
      </h3>
      <Row label="Terminal location">
        <Select label="Terminal location" value={s.terminalLocation} options={[['bottom', 'Bottom panel'], ['right', 'Side panel']]} onChange={(v) => set({ terminalLocation: v })} />
      </Row>
      <Row label="Default shell" hint="Used for the integrated terminal">
        <Select
          label="Default shell"
          value={s.defaultTerminalShell || 'powershell'}
          options={
            window.odex.platform === 'win32'
              ? [['powershell', 'Windows PowerShell'], ['pwsh', 'PowerShell 7'], ['cmd', 'Command Prompt'], ['gitbash', 'Git Bash'], ['wsl', 'WSL']]
              : [['bash', 'bash'], ['zsh', 'zsh'], ['fish', 'fish']]
          }
          onChange={(v) => set({ defaultTerminalShell: v })}
        />
      </Row>
      <Row label="Open files in" hint="Editor for “Open in editor” (a command; {file} and {line} are replaced)">
        <input className="input" value={s.editor} onChange={(e) => set({ editor: e.target.value })} placeholder="code -g {file}:{line}" style={{ width: 280 }} aria-label="Editor command" />
      </Row>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Notifications & background
      </h3>
      <Row label="Notify when a turn completes">
        <Select label="Turn notifications" value={s.notifyTurnComplete} options={[['background', 'When in background'], ['always', 'Always'], ['never', 'Never']]} onChange={(v) => set({ notifyTurnComplete: v })} />
      </Row>
      <Row label="Notify when approval is needed">
        <Toggle checked={s.notifyApprovals} onChange={(v) => set({ notifyApprovals: v })} label="Approval notifications" />
      </Row>
      <Row label="Keep the computer awake while agents run">
        <Toggle checked={s.keepAwake} onChange={(v) => set({ keepAwake: v })} label="Keep awake" />
      </Row>
      <Row label="Keep running in the tray" hint="Closing the window keeps automations and agents running">
        <Toggle checked={s.keepRunningInTray} onChange={(v) => set({ keepRunningInTray: v })} label="Run in tray" />
      </Row>

      <h3 className="section-title" style={{ marginTop: 20 }}>
        Safety
      </h3>
      <Row label="Kill switch hotkey" hint="Stops all computer-use and browser actions immediately">
        <input className="input mono" value={s.killSwitchHotkey} onChange={(e) => set({ killSwitchHotkey: e.target.value })} style={{ width: 200 }} aria-label="Kill switch hotkey" />
      </Row>
      <Row label="Appshot hotkey" hint="Capture the frontmost window into the composer">
        <input className="input mono" value={s.appshotHotkey} onChange={(e) => set({ appshotHotkey: e.target.value })} style={{ width: 200 }} aria-label="Appshot hotkey" />
      </Row>
      <Row label="Run setup again">
        <button className="btn btn-sm" onClick={() => useApp.getState().setUi({ onboardingOpen: true })}>
          Open setup
        </button>
      </Row>
    </div>
  )
}
