import { BrowserWindow } from 'electron'
import fs from 'node:fs'
import path from 'node:path'
import { odexHome } from './paths'
import type { DesktopSettings } from '@shared/desktop'

export type { DesktopSettings }

export const defaults: DesktopSettings = {
  theme: 'system',
  accent: '#4f6bed',
  uiFont: 'Inter, "Segoe UI Variable", "Segoe UI", system-ui, sans-serif',
  codeFont: '"Cascadia Code", "JetBrains Mono", Consolas, monospace',
  fontSize: 13,
  density: 'comfortable',
  enterSends: true,
  followUpBehavior: 'queue',
  terminalLocation: 'bottom',
  defaultTerminalShell: process.platform === 'win32' ? 'powershell' : process.env.SHELL || 'bash',
  notifyTurnComplete: 'background',
  notifyApprovals: true,
  keepAwake: false,
  keepRunningInTray: true,
  shortcuts: {},
  editor: 'code',
  reviewDelivery: 'inline',
  killSwitchHotkey: 'Control+Alt+Escape',
  appshotHotkey: 'Control+Alt+Shift+A',
  onboarded: false,
  reducedMotion: 'system',
  showReasoning: true,
  browserHome: 'about:blank',
  zoom: 1,
  bgColor: '',
  fgColor: '',
  quickChatHotkey: '',
  quickChatOnTop: false,
  openDevServerUrls: true,
  projectEditors: {},
}

let cache: DesktopSettings | null = null

function file(): string {
  return path.join(odexHome(), 'desktop.json')
}

export function getSettings(): DesktopSettings {
  if (cache) return cache
  try {
    cache = { ...defaults, ...(JSON.parse(fs.readFileSync(file(), 'utf8')) as Partial<DesktopSettings>) }
  } catch {
    cache = { ...defaults }
  }
  return cache
}

export function setSettings(patch: Partial<DesktopSettings>): DesktopSettings {
  const next = { ...getSettings(), ...patch }
  cache = next
  fs.mkdirSync(path.dirname(file()), { recursive: true })
  fs.writeFileSync(file(), JSON.stringify(next, null, 2))
  for (const w of BrowserWindow.getAllWindows()) w.webContents.send('odex:settings', next)
  listeners.forEach((l) => l(next))
  return next
}

const listeners: Array<(s: DesktopSettings) => void> = []
export function onSettings(l: (s: DesktopSettings) => void): void {
  listeners.push(l)
}

/** Per-window UI state (panel sizes, open thread) persisted between runs. */
export function windowStateFile(): string {
  return path.join(odexHome(), 'window-state.json')
}
