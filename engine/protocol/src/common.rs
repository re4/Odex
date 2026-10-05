//! Enums and small value types shared by the wire protocol and the config schema.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// User-facing permission mode. Maps onto an approval policy + sandbox mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// No writes; approval required for anything not known-safe.
    ReadOnly,
    /// Workspace-write sandbox; asks before escalating or using the network.
    #[default]
    Auto,
    /// No sandbox, no approvals (shows a warning in the UI).
    FullAccess,
}

impl PermissionMode {
    pub fn approval_policy(self) -> ApprovalPolicy {
        match self {
            PermissionMode::ReadOnly => ApprovalPolicy::OnRequest,
            PermissionMode::Auto => ApprovalPolicy::OnRequest,
            PermissionMode::FullAccess => ApprovalPolicy::Never,
        }
    }
    pub fn sandbox_mode(self) -> SandboxMode {
        match self {
            PermissionMode::ReadOnly => SandboxMode::ReadOnly,
            PermissionMode::Auto => SandboxMode::WorkspaceWrite,
            PermissionMode::FullAccess => SandboxMode::DangerFullAccess,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::ReadOnly => "read-only",
            PermissionMode::Auto => "auto",
            PermissionMode::FullAccess => "full-access",
        }
    }
}

/// When the agent must ask the user (mirrors the upstream policy names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy {
    /// Only known-safe read commands run without asking.
    Untrusted,
    /// Run in the sandbox; ask only when a sandboxed command fails.
    OnFailure,
    /// The model decides when to request escalation; network / outside-workspace asks.
    #[default]
    OnRequest,
    /// Never ask.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub enum RunMode {
    #[default]
    Local,
    Worktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningEffort::None => "none",
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Xhigh => "xhigh",
        }
    }
    pub fn all() -> &'static [ReasoningEffort] {
        &[
            ReasoningEffort::None,
            ReasoningEffort::Minimal,
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Xhigh,
        ]
    }
}

/// Which wire API an endpoint speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "lowercase")]
pub enum WireApi {
    #[default]
    Chat,
    Responses,
}

/// Model roles; every role falls back to `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ModelRole {
    Main,
    Compactor,
    Reviewer,
    Vision,
    Utility,
    Embedding,
}

impl ModelRole {
    pub fn all() -> &'static [ModelRole] {
        &[
            ModelRole::Main,
            ModelRole::Compactor,
            ModelRole::Reviewer,
            ModelRole::Vision,
            ModelRole::Utility,
            ModelRole::Embedding,
        ]
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ModelRole::Main => "main",
            ModelRole::Compactor => "compactor",
            ModelRole::Reviewer => "reviewer",
            ModelRole::Vision => "vision",
            ModelRole::Utility => "utility",
            ModelRole::Embedding => "embedding",
        }
    }
}

/// Which tool set a model gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolProfile {
    /// Core only: shell, exec sessions, apply_patch, update_plan, view_image.
    Codex,
    /// Core + read/list/grep/glob/edit/write/read_output/recall (default).
    #[default]
    Extended,
    /// shell, edit, read and plan, for small models.
    Minimal,
}

/// How much prior reasoning text is replayed to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningHistory {
    Drop,
    #[default]
    CurrentTurn,
    All,
}

/// Coordinate convention a vision model uses when it points at things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    #[default]
    Pixels,
    #[serde(rename = "normalized_1000")]
    Normalized1000,
    #[serde(rename = "normalized_1")]
    Normalized1,
}

/// How to request structured output from the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputMode {
    /// Pick automatically from `/version` (json_schema response_format on modern vLLM).
    #[default]
    Auto,
    /// OpenAI-style `response_format: {type: json_schema}`.
    JsonSchema,
    /// Legacy vLLM `guided_json` extra-body parameter.
    GuidedJson,
    /// No server-side constraint; parse and repair client-side.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub enum ThreadKind {
    #[default]
    Normal,
    QuickChat,
    Side,
    Subagent,
    Automation,
    PrChat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub enum ThreadStatus {
    #[default]
    Idle,
    Running,
    WaitingApproval,
    Reconnecting,
    Compacting,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    #[default]
    InProgress,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    #[default]
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub enum TurnMode {
    /// Normal agent turn.
    #[default]
    Default,
    /// Read-only exploration ending in a structured plan.
    Plan,
    /// A dedicated review turn using the reviewer model.
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub enum PlanStepStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub step: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub enum NoticeLevel {
    #[default]
    Info,
    Warning,
    Error,
}

/// Token usage reported by the server for one or more requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub cached_input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    #[ts(type = "number")]
    pub reasoning_tokens: u64,
    #[ts(type = "number")]
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn add(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub struct DiffStats {
    pub files_changed: u32,
    pub additions: u32,
    pub deletions: u32,
}

/// Capability flags for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(default)]
pub struct ModelCapabilities {
    pub tools: bool,
    pub vision: bool,
    pub parallel_tools: bool,
    pub reasoning: bool,
}
