//! Server → client requests: approvals, elicitations, browser actions, secrets.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::items::{FileChange, Rect};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AutoReviewVerdict {
    /// `allow`, `deny` or `askUser`.
    pub decision: String,
    pub risk: String,
    pub reason: String,
}

/// What is being approved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ApprovalKind {
    #[serde(rename_all = "camelCase")]
    Exec {
        command: String,
        cwd: String,
        justification: Option<String>,
        /// Why approval is needed (`escalation`, `sandboxFailure`, `policy`, `network`, `untrusted`).
        reason: String,
        /// Prefix that "don't ask again" would remember.
        prefix: Vec<String>,
        sandbox_output: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Patch {
        changes: Vec<FileChange>,
        reason: String,
        /// Files outside the writable roots.
        outside_workspace: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Mcp { server: String, tool: String, arguments: Value, description: Option<String>, read_only: bool },
    #[serde(rename_all = "camelCase")]
    ComputerUse {
        app: String,
        action: String,
        arguments: Value,
        /// PNG data URL of the current window.
        screenshot: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Browser { site: String, action: String, arguments: Value },
    #[serde(rename_all = "camelCase")]
    Download { url: String, filename: String },
    #[serde(rename_all = "camelCase")]
    Hook { hook_id: String, command: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequestParams {
    pub approval_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: Option<String>,
    pub approval: ApprovalKind,
    /// Present when automatic review escalated to the user.
    pub auto_review: Option<AutoReviewVerdict>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ApprovalDecision {
    Approve,
    /// Approve and don't ask again this session for this prefix/tool/app/site.
    ApproveForSession,
    Deny {
        feedback: Option<String>,
    },
    /// Approve with edits/instructions (Ctrl+Enter).
    Custom {
        /// Replacement command (exec only).
        command: Option<String>,
        /// Extra instructions passed to the agent with the result.
        feedback: Option<String>,
    },
    /// Stop the whole turn.
    Abort,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResponse {
    pub decision: ApprovalDecision,
    /// With `approveForSession` on an exec approval: also persist an allow
    /// rule for the command prefix in `~/.odex/rules/default.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationRequestParams {
    pub thread_id: Option<String>,
    pub server: String,
    pub message: String,
    pub requested_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationResponse {
    /// `accept`, `decline` or `cancel`.
    pub action: String,
    pub content: Option<Value>,
}

/// A browser-use action executed by the desktop's in-app browser over CDP.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BrowserExecuteParams {
    pub thread_id: String,
    /// `navigate`, `snapshot`, `click`, `type`, `select`, `scroll`, `screenshot`,
    /// `eval`, `console`, `network`, `back`, `forward`, `tabs`, `cdp`.
    pub action: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BrowserExecuteResponse {
    pub ok: bool,
    pub url: Option<String>,
    pub title: Option<String>,
    /// Text result (snapshot, eval result, console lines, ...).
    pub text: Option<String>,
    /// PNG data URL.
    pub image: Option<String>,
    pub error: Option<String>,
    pub bounds: Option<Rect>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SecretsStoreParams {
    pub key: String,
    /// `null` deletes.
    pub value: Option<String>,
}

/// Read the integrated terminal(s) of a thread (the agent's `read_terminal` tool).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReadParams {
    pub thread_id: String,
    /// Specific terminal tab; default: the most recently active one.
    pub terminal_id: Option<String>,
    /// Trailing lines to return (default 200).
    pub lines: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReadResponse {
    pub terminals: Vec<TerminalInfo>,
    pub terminal_id: Option<String>,
    pub text: String,
}
