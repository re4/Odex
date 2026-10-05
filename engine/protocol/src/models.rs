//! Model, endpoint, Doctor and context-status DTOs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::common::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// Key used everywhere else (`[models.<key>]` or `<provider>:<model id>`).
    pub key: String,
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub capabilities: ModelCapabilities,
    pub tool_profile: ToolProfile,
    /// Roles this model is assigned to.
    pub roles: Vec<ModelRole>,
    /// Reasoning efforts this model maps (empty = no effort control).
    pub efforts: Vec<ReasoningEffort>,
    pub default_effort: Option<ReasoningEffort>,
    /// Discovered on the endpoint (vs only configured).
    pub available: bool,
    pub preset: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum EndpointHealth {
    Unknown,
    Healthy,
    Degraded,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub wire_api: WireApi,
    pub enabled: bool,
    pub has_api_key: bool,
    pub health: EndpointHealth,
    pub version: Option<String>,
    pub models: Vec<DiscoveredModel>,
    pub max_concurrent_requests: u32,
    pub in_flight: u32,
    pub queued: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModel {
    pub id: String,
    pub max_model_len: Option<u32>,
    pub owned_by: Option<String>,
    pub root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestResult {
    pub ok: bool,
    #[ts(type = "number")]
    pub latency_ms: u64,
    pub version: Option<String>,
    pub models: Vec<DiscoveredModel>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    /// Stable id: `connect`, `models`, `streaming`, `toolCall`, `streamedToolCall`,
    /// `parallelToolCalls`, `reasoning`, `vision`, `tokenize`, `prefixCache`, `structuredOutput`.
    pub id: String,
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    #[ts(type = "number")]
    pub duration_ms: u64,
    /// `vllm serve` flags that would fix a failure.
    pub fix_flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub provider_id: String,
    pub model_id: String,
    pub base_url: String,
    pub server_version: Option<String>,
    pub checks: Vec<DoctorCheck>,
    /// Union of fix flags, plus a full suggested `vllm serve` command.
    pub suggested_flags: Vec<String>,
    pub suggested_command: Option<String>,
    #[ts(type = "number")]
    pub ran_at: i64,
    /// Capabilities inferred from the run (written back to the model cache).
    pub inferred_capabilities: ModelCapabilities,
}

/// Token breakdown of the current prompt, by category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContextBreakdown {
    pub system: u32,
    pub tools: u32,
    pub agents_md: u32,
    pub memories: u32,
    pub summary: u32,
    pub pinned: u32,
    pub history: u32,
    pub tool_outputs: u32,
    pub images: u32,
}

impl ContextBreakdown {
    pub fn total(&self) -> u32 {
        self.system
            + self.tools
            + self.agents_md
            + self.memories
            + self.summary
            + self.pinned
            + self.history
            + self.tool_outputs
            + self.images
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CompactionRecord {
    pub summary_number: u32,
    #[ts(type = "number")]
    pub at: i64,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub trigger: crate::items::CompactionTrigger,
    pub llm: bool,
    pub turn_id: Option<String>,
}

/// One decision recorded in a context summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SummaryDecision {
    pub decision: String,
    pub reason: String,
}

/// One changed file recorded in a context summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SummaryFile {
    pub path: String,
    pub purpose: String,
    pub state: String,
}

/// The latest compaction (handoff) summary of a thread, for the summary card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextSummary {
    /// Summary number (1 = first compaction).
    pub number: u32,
    /// Whether the LLM compactor wrote it (false = extractive fallback).
    pub llm: bool,
    /// When the compaction happened (ms), when known.
    #[ts(type = "number | null")]
    pub at: Option<i64>,
    pub goal_and_requirements: Vec<String>,
    pub decisions: Vec<SummaryDecision>,
    pub files_changed: Vec<SummaryFile>,
    pub open_errors: Vec<String>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextStatus {
    pub model: Option<String>,
    pub window: u32,
    /// window − reserved output − margin.
    pub budget: u32,
    /// Tokens the next request would use (exact after a response, estimated otherwise).
    pub used: u32,
    pub exact: bool,
    /// used / window in 0..1.
    pub percent: f64,
    pub prune_at: f64,
    pub compact_at: f64,
    pub breakdown: ContextBreakdown,
    pub compactions: Vec<CompactionRecord>,
    pub prunes: u32,
    pub lazy_tools: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    /// YYYY-MM-DD (local).
    pub date: String,
    pub model: String,
    pub requests: u32,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub rows: Vec<UsageRow>,
    pub totals: TokenUsage,
    pub by_model: BTreeMap<String, TokenUsage>,
}

/// Full effective-config view returned by `config/read`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadResponse {
    pub path: String,
    pub odex_home: String,
    /// The user config file as written (TOML → JSON).
    pub user: crate::config_types::ConfigToml,
    /// Effective config after profile/project layering.
    pub effective: crate::config_types::ConfigToml,
    pub active_profile: Option<String>,
    pub profiles: Vec<String>,
    /// Parse warnings and unknown keys.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConfigEdit {
    /// Dotted path, e.g. `models.qwen.temperature` or `roles.main`.
    pub key_path: String,
    /// `null` removes the key.
    pub value: Value,
}
