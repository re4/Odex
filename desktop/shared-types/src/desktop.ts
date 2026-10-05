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
  /** Background color override ('' = theme default). */
  bgColor: string
  /** Foreground (text) color override ('' = theme default). */
  fgColor: string
  /** OS-wide hotkey that opens the Quick Chat window ('' = off). */
  quickChatHotkey: string
  /** Keep the Quick Chat window above other windows. */
  quickChatOnTop: boolean
  /** Open local dev-server URLs printed by agent processes and project actions in the browser panel. */
  openDevServerUrls: boolean
  /** Per-project editor command overriding `editor`, keyed by project id. */
  projectEditors: Record<string, string>
}
