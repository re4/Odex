//! Threads, turns and the items that make up a conversation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::common::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub base_branch: Option<String>,
    pub base_commit: Option<String>,
    /// The local checkout the worktree belongs to.
    pub repo_root: String,
    /// Environment setup script state: `running`, `ok`, `failed…` (none without a script).
    pub setup_status: Option<String>,
    /// Log file with the setup script's full output.
    #[serde(default)]
    pub setup_log: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub objective: String,
    /// `active`, `paused`, `done`, `blocked`, `budgetExhausted`, `cleared`.
    pub status: String,
    /// When the goal was paused (`thread/goal/pause`); paused time does not
    /// count against the time budget.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub paused_at: Option<i64>,
    #[ts(type = "number")]
    pub started_at: i64,
    pub time_budget_secs: Option<u32>,
    #[ts(type = "number | null")]
    pub token_budget: Option<u64>,
    #[ts(type = "number")]
    pub tokens_used: u64,
    pub turns: u32,
    pub last_update: Option<String>,
}

/// The model a thread's last turn ran on (for downgrade detection).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelUse {
    pub key: String,
    pub provider_id: String,
    pub model_id: String,
    pub context_window: u32,
}

/// The endpoint no longer serves what the thread was using: the model is
/// gone, the default model changed, or its context window shrank.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelWarning {
    /// `notServed`, `modelChanged`, `windowShrank`.
    pub code: String,
    pub message: String,
    /// What the endpoint serves instead (a same-family model when there is one).
    pub served_model: Option<String>,
    pub previous_window: Option<u32>,
    pub window: Option<u32>,
    #[ts(type = "number")]
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub name: Option<String>,
    pub kind: ThreadKind,
    pub project_id: Option<String>,
    pub cwd: String,
    pub run_mode: RunMode,
    pub worktree: Option<WorktreeInfo>,
    pub branch: Option<String>,
    /// Model key used for the `main` role in this thread.
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub permission_mode: PermissionMode,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub archived: bool,
    pub pinned: bool,
    pub unread: bool,
    pub status: ThreadStatus,
    /// First user message (or last agent message) snippet.
    pub preview: String,
    pub parent_thread_id: Option<String>,
    pub goal: Option<Goal>,
    pub memories_enabled: bool,
    pub ephemeral: bool,
    pub diff_stats: Option<DiffStats>,
    pub usage: TokenUsage,
    pub last_error: Option<String>,
    /// Model and context window the last turn ran with.
    #[serde(default)]
    pub last_model: Option<ModelUse>,
    /// Set when the served model or its window changed under this thread.
    #[serde(default)]
    pub model_warning: Option<ModelWarning>,
    /// The pull request opened from (or found for) this thread's branch.
    #[serde(default)]
    pub pr: Option<ThreadPr>,
    /// Environment (`.odex/environments.toml` id) whose variables and setup script this thread
    /// uses. `None` = the project's default environment (else its first one); `""` = none.
    #[serde(default)]
    pub environment_id: Option<String>,
}

/// Pull-request summary kept on a thread for status badges.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPr {
    pub number: u32,
    /// `open`, `draft`, `merged`, `closed`.
    pub state: String,
    pub url: String,
    pub title: Option<String>,
    /// Combined check state: `success`, `failure`, `pending`; none without checks.
    pub checks: Option<String>,
    #[serde(default)]
    pub failed_checks: u32,
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub message: String,
    /// `contextOverflow`, `endpointUnavailable`, `interrupted`, `internal`, ...
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub thread_id: String,
    pub status: TurnStatus,
    pub mode: TurnMode,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub completed_at: Option<i64>,
    pub error: Option<TurnError>,
    pub usage: TokenUsage,
    /// Present when a turn is returned as part of thread history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<ThreadItem>,
}

/// Rectangle in CSS/screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One inline review comment on a diff line (review pane or PR chat).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    pub path: String,
    pub line: Option<u32>,
    pub end_line: Option<u32>,
    /// `old` or `new` side of the diff.
    pub side: Option<String>,
    pub body: String,
    /// The diff lines the comment is anchored to, for context.
    pub snippet: Option<String>,
}

/// User input parts for a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput {
    Text {
        text: String,
    },
    /// Image as a data URL or http(s) URL.
    Image {
        url: String,
        name: Option<String>,
    },
    /// Image file on disk.
    LocalImage {
        path: String,
    },
    /// File attachment; small text files are inlined, others referenced.
    File {
        path: String,
        name: Option<String>,
    },
    /// `@skill` mention; the skill body is loaded into context.
    Skill {
        name: String,
    },
    /// `@file` mention.
    Mention {
        path: String,
    },
    /// `@app` mention of an MCP server/resource.
    McpResource {
        server: String,
        uri: String,
    },
    /// Window capture with UI tree.
    Appshot {
        title: String,
        app: Option<String>,
        image_url: String,
        ui_tree: Option<String>,
    },
    /// Comment on a page element in the in-app browser.
    BrowserComment {
        url: String,
        selector: Option<String>,
        bounds: Option<Rect>,
        comment: String,
        screenshot_url: Option<String>,
    },
    /// Batched review comments from the review pane.
    ReviewComments {
        comments: Vec<ReviewComment>,
    },
}

impl UserInput {
    pub fn text(s: impl Into<String>) -> Self {
        UserInput::Text { text: s.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum FileChangeKind {
    Add,
    Delete,
    Update,
    Move,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub move_path: Option<String>,
    /// Unified diff of this file's change.
    pub diff: String,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewFinding {
    pub title: String,
    pub body: String,
    /// 0 = highest.
    pub priority: u32,
    pub confidence: Option<f64>,
    pub path: Option<String>,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum CompactionTrigger {
    Auto,
    Manual,
    Emergency,
    ModelSwitch,
}

/// One entry in the conversation, as rendered by the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    #[serde(rename_all = "camelCase")]
    UserMessage {
        id: String,
        content: Vec<UserInput>,
        /// Sent while a turn was running and injected as steering.
        #[serde(default)]
        steer: bool,
    },
    #[serde(rename_all = "camelCase")]
    AgentMessage { id: String, text: String },
    #[serde(rename_all = "camelCase")]
    Reasoning { id: String, text: String },
    #[serde(rename_all = "camelCase")]
    CommandExecution {
        id: String,
        command: String,
        cwd: String,
        status: ItemStatus,
        exit_code: Option<i32>,
        #[ts(type = "number | null")]
        duration_ms: Option<u64>,
        /// Aggregated output (capped for display).
        output: String,
        /// Ref for `read_output` when the output was truncated.
        output_ref: Option<String>,
        sandboxed: bool,
        /// PTY session id when started via exec_command.
        session_id: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    FileChange { id: String, changes: Vec<FileChange>, status: ItemStatus, error: Option<String> },
    /// Generic built-in tool (read_file, grep, list_dir, ...).
    #[serde(rename_all = "camelCase")]
    ToolCall {
        id: String,
        tool: String,
        arguments: Value,
        status: ItemStatus,
        output: Option<String>,
        /// One-line human summary, e.g. "Read src/main.rs (lines 1-200)".
        summary: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        arguments: Value,
        status: ItemStatus,
        result: Option<Value>,
        error: Option<String>,
        #[ts(type = "number | null")]
        duration_ms: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Plan { id: String, explanation: Option<String>, steps: Vec<PlanStep> },
    /// A structured plan proposed in plan mode, awaiting approval.
    #[serde(rename_all = "camelCase")]
    ProposedPlan { id: String, markdown: String, approved: bool },
    #[serde(rename_all = "camelCase")]
    Subagent {
        id: String,
        agent_thread_id: String,
        task: String,
        mode: String,
        status: ItemStatus,
        summary: Option<String>,
        diff_stats: Option<DiffStats>,
        /// Stable seed for the identicon.
        identicon_seed: String,
        nickname: String,
    },
    #[serde(rename_all = "camelCase")]
    ContextCompaction {
        id: String,
        summary_number: u32,
        tokens_before: u32,
        tokens_after: u32,
        trigger: CompactionTrigger,
        /// Whether the LLM compactor succeeded (false = extractive fallback).
        llm: bool,
        status: ItemStatus,
    },
    #[serde(rename_all = "camelCase")]
    ComputerUse {
        id: String,
        action: String,
        arguments: Value,
        status: ItemStatus,
        app: Option<String>,
        output: Option<String>,
        /// Thumbnails stored under ~/.odex/media (paths) or data URLs.
        before_image: Option<String>,
        after_image: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Browser {
        id: String,
        action: String,
        arguments: Value,
        status: ItemStatus,
        url: Option<String>,
        output: Option<String>,
        image: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ImageView {
        id: String,
        path: String,
        /// Set when `generate_image` created the file.
        prompt: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Review { id: String, summary: String, findings: Vec<ReviewFinding>, overall_correctness: Option<String> },
    #[serde(rename_all = "camelCase")]
    Notice { id: String, level: NoticeLevel, message: String, code: Option<String> },
    #[serde(rename_all = "camelCase")]
    Error { id: String, message: String },
}

impl ThreadItem {
    pub fn id(&self) -> &str {
        match self {
            ThreadItem::UserMessage { id, .. }
            | ThreadItem::AgentMessage { id, .. }
            | ThreadItem::Reasoning { id, .. }
            | ThreadItem::CommandExecution { id, .. }
            | ThreadItem::FileChange { id, .. }
            | ThreadItem::ToolCall { id, .. }
            | ThreadItem::McpToolCall { id, .. }
            | ThreadItem::Plan { id, .. }
            | ThreadItem::ProposedPlan { id, .. }
            | ThreadItem::Subagent { id, .. }
            | ThreadItem::ContextCompaction { id, .. }
            | ThreadItem::ComputerUse { id, .. }
            | ThreadItem::Browser { id, .. }
            | ThreadItem::ImageView { id, .. }
            | ThreadItem::Review { id, .. }
            | ThreadItem::Notice { id, .. }
            | ThreadItem::Error { id, .. } => id,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            ThreadItem::UserMessage { .. } => "userMessage",
            ThreadItem::AgentMessage { .. } => "agentMessage",
            ThreadItem::Reasoning { .. } => "reasoning",
            ThreadItem::CommandExecution { .. } => "commandExecution",
            ThreadItem::FileChange { .. } => "fileChange",
            ThreadItem::ToolCall { .. } => "toolCall",
            ThreadItem::McpToolCall { .. } => "mcpToolCall",
            ThreadItem::Plan { .. } => "plan",
            ThreadItem::ProposedPlan { .. } => "proposedPlan",
            ThreadItem::Subagent { .. } => "subagent",
            ThreadItem::ContextCompaction { .. } => "contextCompaction",
            ThreadItem::ComputerUse { .. } => "computerUse",
            ThreadItem::Browser { .. } => "browser",
            ThreadItem::ImageView { .. } => "imageView",
            ThreadItem::Review { .. } => "review",
            ThreadItem::Notice { .. } => "notice",
            ThreadItem::Error { .. } => "error",
        }
    }
}

/// Streaming delta for an in-progress item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ItemDelta {
    AgentMessage {
        text: String,
    },
    Reasoning {
        text: String,
    },
    /// Command output chunk; `stream` is `stdout`, `stderr` or `pty`.
    CommandOutput {
        chunk: String,
        stream: String,
    },
    /// Partial tool arguments while the model is still writing them.
    ToolArguments {
        text: String,
    },
    /// Free-form progress text (MCP progress, subagent activity, ...).
    Progress {
        message: String,
    },
}
