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
  /** Check GitHub releases for new versions in the background and download them (installing always asks). */
  autoUpdate: boolean
}

/** App updates (main/updater.ts), pushed to the renderer on `odex:update-state`. */
export interface UpdateState {
  status: 'unsupported' | 'idle' | 'checking' | 'available' | 'not-available' | 'downloading' | 'downloaded' | 'error'
  currentVersion: string
  /** Why this build can't update itself (status 'unsupported'). */
  reason?: string
  /** Newest released version (when newer than the running one). */
  version?: string
  releaseDate?: string
  /** Release page for `version`, or the releases list. */
  releaseUrl: string
  progress?: { percent: number; transferred: number; total: number; bytesPerSecond: number }
  error?: string
  /** When the last check finished (ms since epoch). */
  checkedAt?: number
}
