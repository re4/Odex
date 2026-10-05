//! Client → server requests (method name, params, result).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::common::*;
use crate::config_types::*;
use crate::ext::*;
use crate::items::*;
use crate::models::*;
use crate::workspace::*;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct EmptyParams {}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct EmptyResponse {}

// ---------------------------------------------------------------- initialize

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ClientCapabilities {
    /// Client renders approval requests.
    pub approvals: bool,
    /// Client can execute `browser/execute` requests (in-app browser).
    pub browser: bool,
    /// Client handles MCP elicitation requests.
    pub elicitation: bool,
    /// Client stores secrets (`secrets/store`).
    pub secrets: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    #[serde(default)]
    pub capabilities: ClientCapabilities,
    /// Decrypted secrets (API keys, tokens) held only in memory by the engine.
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: String,
    pub server_name: String,
    pub server_version: String,
    pub odex_home: String,
    pub platform: String,
    pub sandbox: SandboxStatus,
    /// True when no endpoint/model is configured yet (show onboarding).
    pub needs_onboarding: bool,
}

// ------------------------------------------------------------------- threads

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadStartParams {
    pub project_id: Option<String>,
    /// Working directory; defaults to the project's primary folder.
    pub cwd: Option<String>,
    pub kind: Option<ThreadKind>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub permission_mode: Option<PermissionMode>,
    pub run_mode: Option<RunMode>,
    /// Base branch for worktree mode (default: current branch).
    pub base_branch: Option<String>,
    pub environment_id: Option<String>,
    pub parent_thread_id: Option<String>,
    /// Not persisted to the sidebar (side chats).
    pub ephemeral: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResponse {
    pub thread: Thread,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIdParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadResponse {
    pub thread: Thread,
    pub turns: Vec<Turn>,
    pub pending_approvals: Vec<crate::server_requests::ApprovalRequestParams>,
    pub context: Option<ContextStatus>,
    pub plan: Vec<PlanStep>,
    pub sources: Vec<SourceEntry>,
    pub followups: Vec<String>,
    pub queued: Vec<Vec<UserInput>>,
    /// The thread's rollout file (JSONL event log) on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SourceEntry {
    pub path: String,
    pub read: bool,
    pub edited: bool,
    #[ts(type = "number")]
    pub last_touched: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadForkParams {
    pub thread_id: String,
    /// Fork after this turn (inclusive). Default: the latest turn.
    pub turn_id: Option<String>,
    pub run_mode: Option<RunMode>,
    pub kind: Option<ThreadKind>,
    pub ephemeral: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadListParams {
    pub project_id: Option<String>,
    pub archived: Option<bool>,
    pub kinds: Option<Vec<ThreadKind>>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResponse {
    pub threads: Vec<Thread>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSearchParams {
    pub query: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSearchResponse {
    pub hits: Vec<SearchHit>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadArchiveParams {
    pub thread_id: String,
    /// Also remove the thread's worktree (after the UI confirmed).
    pub remove_worktree: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRollbackParams {
    pub thread_id: String,
    /// Roll back to just before this turn (it and later turns are dropped).
    pub turn_id: String,
    /// Also restore files from the undo snapshot taken before that turn.
    #[serde(default)]
    pub restore_files: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadUpdateParams {
    pub thread_id: String,
    pub name: Option<String>,
    pub pinned: Option<bool>,
    pub unread: Option<bool>,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub permission_mode: Option<PermissionMode>,
    pub memories_enabled: Option<bool>,
    pub project_id: Option<String>,
    pub cwd: Option<String>,
    pub run_mode: Option<RunMode>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ThreadCompactParams {
    pub thread_id: String,
    /// Optional focus for the summary (`/compact <focus>`).
    pub focus: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct GoalSetParams {
    pub thread_id: String,
    pub objective: String,
    pub time_budget_secs: Option<u32>,
    #[ts(type = "number | null")]
    pub token_budget: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueSetParams {
    pub thread_id: String,
    /// Full replacement of the queued follow-ups (edit, reorder, delete).
    pub queued: Vec<Vec<UserInput>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ShellCommandParams {
    pub thread_id: String,
    /// User-initiated command (`!cmd`), run unsandboxed in the thread's cwd.
    pub command: String,
    pub timeout_ms: Option<u32>,
}

// --------------------------------------------------------------------- turns

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    pub mode: Option<TurnMode>,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub permission_mode: Option<PermissionMode>,
    /// If a turn is running: queue (default) or steer.
    pub if_busy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    /// Absent when the input was queued or used to steer.
    pub turn: Option<Turn>,
    pub queued: bool,
    pub steered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStartParams {
    pub thread_id: String,
    pub target: DiffTarget,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApproveOverrideParams {
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanDecisionParams {
    pub thread_id: String,
    pub item_id: String,
    /// `approve`, `edit` (with markdown) or `reject`.
    pub decision: String,
    pub markdown: Option<String>,
}

// ---------------------------------------------------------- models/providers

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    pub models: Vec<ModelInfo>,
    pub roles: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderListParams {
    /// Re-probe endpoints (/v1/models, /health, /version).
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderListResponse {
    pub providers: Vec<ProviderInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUpsertParams {
    pub id: String,
    pub provider: ModelProviderToml,
    /// New API key (stored by the client; held in memory by the engine).
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderIdParams {
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderTestParams {
    pub id: Option<String>,
    /// Test an unsaved definition.
    pub provider: Option<ModelProviderToml>,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct DoctorRunParams {
    pub provider_id: Option<String>,
    /// Model key or served model id; default: every discovered model's first.
    pub model: Option<String>,
    /// Skip slow checks (prefix cache timing).
    pub quick: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DoctorRunResponse {
    pub reports: Vec<DoctorReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PresetInfo {
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub match_patterns: Vec<String>,
    pub serve_command: String,
    pub notes: Option<String>,
    pub settings: ModelToml,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PresetListResponse {
    pub presets: Vec<PresetInfo>,
}

// -------------------------------------------------------------------- config

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConfigWriteParams {
    pub edits: Vec<ConfigEdit>,
    /// Write to this project's `.odex/config.toml` instead of the user config.
    pub project_path: Option<String>,
}

// ------------------------------------------------------------------ projects

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectListResponse {
    pub projects: Vec<Project>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectAddParams {
    pub folders: Vec<String>,
    pub name: Option<String>,
    pub primary: Option<u32>,
    /// Create the primary folder (and `git init`) if missing.
    pub create: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectResponse {
    pub project: Project,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectUpdateParams {
    pub id: String,
    pub name: Option<String>,
    pub folders: Option<Vec<String>>,
    pub primary: Option<u32>,
    pub actions: Option<Vec<ProjectAction>>,
    pub environments: Option<Vec<Environment>>,
    pub default_environment: Option<String>,
    pub collapsed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIdParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TrustParams {
    pub path: String,
    pub trusted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TrustCheckResponse {
    pub path: String,
    pub trusted: bool,
    /// The folder has never been seen (UI should ask).
    pub unknown: bool,
    pub has_odex_dir: bool,
    pub has_agents_md: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PathParams {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchParams {
    pub roots: Vec<String>,
    pub query: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchResponse {
    pub files: Vec<FileMatch>,
}

// ----------------------------------------------------------------------- git

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CwdParams {
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffParams {
    /// One or more repos (multi-repo review).
    pub cwds: Vec<String>,
    pub target: DiffTarget,
    #[serde(default)]
    pub ignore_whitespace: bool,
    #[serde(default)]
    pub context_lines: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffResponse {
    pub repos: Vec<RepoDiff>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitPathOpParams {
    pub cwd: String,
    /// Files to act on (empty = all).
    pub paths: Vec<String>,
    /// Hunk-level op: the file and hunk index from the diff the UI showed.
    pub hunk: Option<HunkRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HunkRef {
    pub path: String,
    pub hunk_index: u32,
    /// Which diff the hunk came from (`unstaged` for stage/revert, `staged` for unstage).
    pub target: DiffTarget,
    #[serde(default)]
    pub ignore_whitespace: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitParams {
    pub cwd: String,
    pub message: String,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub amend: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitResponse {
    pub sha: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommitMessageParams {
    pub cwd: String,
    pub thread_id: Option<String>,
    /// Only staged changes (default: staged if any, else all).
    pub staged_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommitMessageResponse {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitPushParams {
    pub cwd: String,
    pub remote: Option<String>,
    pub branch: Option<String>,
    #[serde(default)]
    pub set_upstream: bool,
    #[serde(default)]
    pub force_with_lease: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommandOutputResponse {
    pub ok: bool,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchesResponse {
    pub branches: Vec<GitBranch>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitLogParams {
    pub cwd: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitLogResponse {
    pub commits: Vec<GitCommitInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HandoffParams {
    pub thread_id: String,
    pub strategy: HandoffStrategy,
    /// Target local branch (default: the worktree's base branch).
    pub target_branch: Option<String>,
    /// Commit pending worktree changes first with this message.
    pub commit_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrCreateParams {
    pub cwd: String,
    pub title: String,
    pub body: String,
    pub base: Option<String>,
    #[serde(default)]
    pub draft: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrCreateResponse {
    pub url: String,
    pub number: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrViewParams {
    pub cwd: String,
    /// PR number; default: the PR for the current branch.
    pub number: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrViewResponse {
    pub pr: Option<PullRequest>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrCommentParams {
    pub cwd: String,
    pub number: u32,
    pub comments: Vec<ReviewComment>,
    pub body: Option<String>,
    /// Must be true; the UI sets it only after explicit confirmation.
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrDraftResponse {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ExecSessionsResponse {
    pub sessions: Vec<ExecSessionInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct IdParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct IdsParams {
    pub ids: Vec<String>,
}

// ----------------------------------------------------------------------- mcp

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpListResponse {
    pub servers: Vec<McpServerStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpUpsertParams {
    pub name: String,
    pub server: McpServerToml,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NameParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpLogsResponse {
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpLoginResponse {
    /// URL the client should open in the system browser.
    pub authorization_url: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpReadResourceParams {
    pub server: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpReadResourceResponse {
    pub contents: Value,
}

// -------------------------------------------------------- skills and plugins

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct SkillsListParams {
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListResponse {
    pub skills: Vec<SkillInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillWriteParams {
    pub name: String,
    pub description: String,
    pub body: String,
    pub scope: SkillScope,
    /// Project folder for project-scoped skills.
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillReadResponse {
    pub skill: SkillInfo,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillImportParams {
    /// Folder containing SKILL.md, a SKILL.md file, or a git URL.
    pub source: String,
    pub scope: SkillScope,
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SetEnabledParams {
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginsListResponse {
    pub plugins: Vec<PluginInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallParams {
    /// Folder path or git URL.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginResponse {
    pub plugin: PluginInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HooksListParams {
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HooksListResponse {
    pub hooks: Vec<HookInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HookTrustParams {
    pub id: String,
    pub hash: String,
    pub trusted: bool,
}

// --------------------------------------------------- automations & memories

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AutomationListResponse {
    pub automations: Vec<Automation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AutomationUpsertParams {
    pub automation: Automation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AutomationResponse {
    pub automation: Automation,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct AutomationRunsParams {
    pub automation_id: Option<String>,
    pub unread_only: bool,
    pub include_archived: bool,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunsResponse {
    pub runs: Vec<AutomationRun>,
    pub unread_count: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleValidateParams {
    pub schedule: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleValidateResponse {
    pub valid: bool,
    pub error: Option<String>,
    pub description: Option<String>,
    #[ts(type = "number[]")]
    pub next_runs: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct MemoryListParams {
    pub project_path: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MemoryListResponse {
    pub memories: Vec<Memory>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MemoryUpsertParams {
    pub memory: Memory,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MemoryResponse {
    pub memory: Memory,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageStatsParams {
    /// YYYY-MM-DD inclusive.
    pub since: Option<String>,
}

// ------------------------------------------------------------- computer use

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WindowListResponse {
    pub windows: Vec<WindowInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
pub struct AppshotParams {
    /// Window handle; default: the foreground window (excluding Odex).
    pub handle: Option<String>,
    pub include_ui_tree: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AppshotResponse {
    pub appshot: Appshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct KillSwitchParams {
    /// true = engage (stop everything), false = release.
    pub engaged: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextGetResponse {
    pub context: ContextStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeListResponse {
    pub worktrees: Vec<WorktreeInfo>,
    /// The owning thread of each worktree (same order as `worktrees`).
    #[serde(default)]
    pub thread_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitAgentsMdParams {
    pub thread_id: String,
}
