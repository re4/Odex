import type { ComponentType } from 'react'
import { GeneralSettings } from '@/views/settings/GeneralSettings'
import { ModelsSettings } from '@/views/settings/ModelsSettings'

export interface SettingsPanelDef {
  id: string
  label: string
  group: 'App' | 'Agent' | 'Integrations' | 'Advanced'
  component: ComponentType
  /** Extra search terms for the settings filter. */
  keywords?: string
}

/** Settings panels in sidebar order. Add new panels here. */
export const SETTINGS_PANELS: SettingsPanelDef[] = [
  { id: 'general', label: 'General', group: 'App', component: GeneralSettings, keywords: 'theme font density enter notifications tray terminal editor' },
  { id: 'models', label: 'Models & Endpoints', group: 'Agent', component: ModelsSettings, keywords: 'vllm provider doctor roles presets api key' },
]
