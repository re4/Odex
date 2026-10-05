// Shared between the Electron main process and the renderer.

/** Desktop-only preferences (engine settings live in ~/.odex/config.toml). */
export interface DesktopSettings {
  theme: 'system' | 'light' | 'dark'
  accent: string
  uiFont: string
  codeFont: string
  fontSize: number
  density: 'compact' | 'comfortable'
  enterSends: boolean
  followUpBehavior: 'queue' | 'steer'
  terminalLocation: 'bottom' | 'right'
  defaultTerminalShell: string
  notifyTurnComplete: 'never' | 'background' | 'always'
  notifyApprovals: boolean
  keepAwake: boolean
  keepRunningInTray: boolean
  shortcuts: Record<string, string>
  editor: string
  reviewDelivery: 'inline' | 'detached'
  killSwitchHotkey: string
  appshotHotkey: string
  onboarded: boolean
  reducedMotion: 'system' | 'on' | 'off'
  showReasoning: boolean
  browserHome: string
  zoom: number
}
