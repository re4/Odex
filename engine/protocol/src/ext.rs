//! MCP, skills, plugins, hooks, automations, memories, computer use.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::common::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum McpServerState {
    Disabled,
    Starting,
    Ready,
    Failed,
    NeedsAuth,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    /// Model-facing name `mcp__<server>__<tool>`.
    pub qualified_name: String,
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub enabled: bool,
    pub read_only_hint: bool,
    pub auto_approve: bool,
    pub schema_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub name: String,
    /// `stdio` or `http`.
    pub transport: String,
    pub enabled: bool,
    pub state: McpServerState,
    pub error: Option<String>,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub tools: Vec<McpToolInfo>,
    pub resources: Vec<McpResourceInfo>,
    pub prompts: Vec<McpPromptInfo>,
    pub authenticated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceInfo {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpPromptInfo {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SkillScope {
    User,
    Project,
    Plugin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub path: String,
    pub scope: SkillScope,
    pub enabled: bool,
    pub plugin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    /// Relative paths of skill folders.
    pub skills: Vec<String>,
    pub mcp_servers: BTreeMap<String, crate::config_types::McpServerToml>,
    pub hooks: Option<crate::config_types::HooksToml>,
    pub actions: Vec<crate::workspace::ProjectAction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub manifest: PluginManifest,
    pub path: String,
    /// Folder path or git URL it was installed from.
    pub source: String,
    pub enabled: bool,
    pub trusted: bool,
    /// Content hash reviewed at trust time.
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
    Notification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HookInfo {
    /// Stable id: hash of event + source + command.
    pub id: String,
    pub event: HookEvent,
    pub name: Option<String>,
    pub command: String,
    pub matcher: Option<String>,
    /// `user`, `project:<path>`, `plugin:<id>`.
    pub source: String,
    pub hash: String,
    /// `trusted`, `untrusted` (new) or `changed` (trusted hash differs).
    pub trust: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub id: String,
    pub name: String,
    /// Cron expression (5 or 6 fields) or friendly shorthand like `@hourly`.
    pub schedule: String,
    /// `project` (new thread per run) or `thread` (wake an existing thread).
    pub target: String,
    pub project_id: Option<String>,
    pub thread_id: Option<String>,
    pub cwd: Option<String>,
    pub prompt: String,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub permission_mode: PermissionMode,
    pub run_mode: RunMode,
    pub enabled: bool,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number | null")]
    pub last_run_at: Option<i64>,
    #[ts(type = "number | null")]
    pub next_run_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRun {
    pub id: String,
    pub automation_id: String,
    pub automation_name: String,
    pub thread_id: Option<String>,
    /// `running`, `completed`, `failed`, `skipped`.
    pub status: String,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub finished_at: Option<i64>,
    pub summary: Option<String>,
    pub error: Option<String>,
    pub unread: bool,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    pub text: String,
    /// `global` or `project`.
    pub scope: String,
    pub project_path: Option<String>,
    /// `proposed` or `approved`.
    pub status: String,
    /// `preference`, `convention`, `stack`, `other`.
    pub category: String,
    pub source_thread_id: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    /// Native handle as a decimal string.
    pub handle: String,
    pub title: String,
    pub app: String,
    pub pid: u32,
    pub bounds: crate::items::Rect,
    pub minimized: bool,
    pub focused: bool,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Appshot {
    pub title: String,
    pub app: String,
    /// PNG data URL.
    pub image_url: String,
    pub width: u32,
    pub height: u32,
    /// Compact text rendering of the UI Automation tree.
    pub ui_tree: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUseStatus {
    pub supported: bool,
    pub enabled: bool,
    pub platform: String,
    pub allowed_apps: Vec<String>,
    pub active_thread_id: Option<String>,
    pub killed: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SandboxStatus {
    pub backend: String,
    pub available: bool,
    pub network_isolated: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub thread_id: String,
    pub title: String,
    /// `title`, `content` or `branch`.
    pub field: String,
    pub snippet: String,
    #[ts(type = "number")]
    pub updated_at: i64,
}
