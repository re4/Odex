import type { ComponentType } from 'react'
import { GeneralSettings } from '@/views/settings/GeneralSettings'
import { ModelsSettings } from '@/views/settings/ModelsSettings'
import { PersonalizationSettings } from '@/views/settings/PersonalizationSettings'
import { PermissionsSettings } from '@/views/settings/PermissionsSettings'
import { ContextSettings } from '@/views/settings/ContextSettings'
import { MemoriesSettings } from '@/views/settings/MemoriesSettings'
import { UsageSettings } from '@/views/settings/UsageSettings'
import { ShortcutsSettings } from '@/views/settings/ShortcutsSettings'
import { McpSettings } from '@/views/settings/McpSettings'
import { SkillsSettings } from '@/views/settings/SkillsSettings'
import { PluginsSettings } from '@/views/settings/PluginsSettings'
import { HooksSettings } from '@/views/settings/HooksSettings'
import { ComputerUseSettings } from '@/views/settings/ComputerUseSettings'
import { BrowserSettings } from '@/views/settings/BrowserSettings'
import { WorktreesSettings } from '@/views/settings/WorktreesSettings'
import { EnvironmentsSettings } from '@/views/settings/EnvironmentsSettings'
import { GitSettings } from '@/views/settings/GitSettings'
import { CodeReviewSettings } from '@/views/settings/CodeReviewSettings'
import { ArchivedSettings } from '@/views/settings/ArchivedSettings'
import { ConfigSettings } from '@/views/settings/ConfigSettings'
import { AboutSettings } from '@/views/settings/AboutSettings'

export interface SettingsPanelDef {
  id: string
  label: string
  group: 'App' | 'Agent' | 'Integrations' | 'Advanced'
  component: ComponentType
  /** Extra search terms for the settings filter. */
  keywords?: string
}

/** Settings panels in sidebar order. Each panel lives in its own file. */
export const SETTINGS_PANELS: SettingsPanelDef[] = [
  { id: 'general', label: 'General', group: 'App', component: GeneralSettings, keywords: 'theme font density enter notifications tray terminal editor' },
  { id: 'shortcuts', label: 'Keyboard shortcuts', group: 'App', component: ShortcutsSettings, keywords: 'keys bindings hotkeys' },
  { id: 'models', label: 'Models & Endpoints', group: 'Agent', component: ModelsSettings, keywords: 'vllm provider doctor roles presets api key remove hide comfyui image 3d generation workflow' },
  { id: 'personalization', label: 'Personalization', group: 'Agent', component: PersonalizationSettings, keywords: 'custom instructions agents.md' },
  { id: 'permissions', label: 'Permissions & sandbox', group: 'Agent', component: PermissionsSettings, keywords: 'approvals sandbox network rules execpolicy auto review' },
  { id: 'context', label: 'Context', group: 'Agent', component: ContextSettings, keywords: 'compaction prune budget tokens' },
  { id: 'memories', label: 'Memories', group: 'Agent', component: MemoriesSettings, keywords: 'remember preferences' },
  { id: 'usage', label: 'Usage', group: 'Agent', component: UsageSettings, keywords: 'tokens stats' },
  { id: 'mcp', label: 'MCP servers', group: 'Integrations', component: McpSettings, keywords: 'model context protocol tools resources oauth' },
  { id: 'skills', label: 'Skills', group: 'Integrations', component: SkillsSettings, keywords: 'skill.md' },
  { id: 'plugins', label: 'Plugins', group: 'Integrations', component: PluginsSettings, keywords: 'extensions marketplace' },
  { id: 'hooks', label: 'Hooks', group: 'Integrations', component: HooksSettings, keywords: 'pretooluse posttooluse stop trust' },
  { id: 'computer-use', label: 'Computer use', group: 'Integrations', component: ComputerUseSettings, keywords: 'desktop control apps kill switch appshot' },
  { id: 'browser', label: 'Browser', group: 'Integrations', component: BrowserSettings, keywords: 'web sites history cookies' },
  { id: 'git', label: 'Git', group: 'Advanced', component: GitSettings, keywords: 'branch prefix force push commit message pull request github token pr' },
  { id: 'code-review', label: 'Code review', group: 'Advanced', component: CodeReviewSettings, keywords: 'review instructions guidelines reviewer model detached pop out' },
  { id: 'worktrees', label: 'Worktrees', group: 'Advanced', component: WorktreesSettings, keywords: 'git branches cleanup retention keep auto cleanup' },
  { id: 'environments', label: 'Local environments', group: 'Advanced', component: EnvironmentsSettings, keywords: 'setup script worktree environment variables per-os environments.toml' },
  { id: 'archived', label: 'Archived threads', group: 'Advanced', component: ArchivedSettings, keywords: 'restore delete' },
  { id: 'config', label: 'Config & profiles', group: 'Advanced', component: ConfigSettings, keywords: 'config.toml profiles raw' },
  { id: 'about', label: 'About & data', group: 'Advanced', component: AboutSettings, keywords: 'version logs reset privacy' },
]
