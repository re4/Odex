//! Server → client notifications.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::common::*;
use crate::ext::*;
use crate::items::*;
use crate::methods::SourceEntry;
use crate::models::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNotification {
    pub thread: Thread,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIdNotification {
    pub thread_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnNotification {
    pub thread_id: String,
    pub turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ThreadItem,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ItemDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: ItemDelta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanUpdatedNotification {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub explanation: Option<String>,
    pub plan: Vec<PlanStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DiffUpdatedNotification {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub stats: DiffStats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextUpdatedNotification {
    pub thread_id: String,
    pub context: ContextStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageNotification {
    pub thread_id: String,
    pub turn_id: String,
    /// Usage of the request that just finished.
    pub last: TokenUsage,
    /// Thread total.
    pub total: TokenUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FollowupsNotification {
    pub thread_id: String,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SourcesNotification {
    pub thread_id: String,
    pub sources: Vec<SourceEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueueNotification {
    pub thread_id: String,
    pub queued: Vec<Vec<UserInput>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResolvedNotification {
    pub thread_id: String,
    pub approval_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpStatusNotification {
    pub server: McpServerStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRunNotification {
    pub run: AutomationRun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MemoriesProposedNotification {
    pub memories: Vec<Memory>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HooksReviewNotification {
    pub hooks: Vec<HookInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUseActiveNotification {
    pub active: bool,
    pub thread_id: Option<String>,
    pub app: Option<String>,
    /// True while real mouse/keyboard input is being sent (show takeover overlay).
    pub takeover: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProvidersNotification {
    pub providers: Vec<ProviderInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct OpenUrlNotification {
    pub url: String,
    /// `external` (system browser) or `inApp`.
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LogNotification {
    pub level: String,
    pub message: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectsChangedNotification {
    pub projects: Vec<crate::workspace::Project>,
}
