//! Schema of `~/.odex/config.toml` (and `.odex/config.toml` in trusted projects).
//!
//! Every field is optional so that layers (defaults → user → profile → project →
//! per-thread overrides) can be merged. Keys are snake_case, as in TOML.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::common::*;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ConfigToml {
    /// Default model key for the `main` role (shorthand for `roles.main`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Active profile name from `[profiles]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<SandboxMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Max bytes of AGENTS.md content injected into the prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_doc_max_bytes: Option<u32>,
    /// Extra instructions appended to every system prompt ("Personalization").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// `powershell` (default on Windows), `pwsh`, `cmd`, `bash`, `zsh`, `sh`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_shell: Option<String>,
    /// Where worktrees live; default `~/.odex/worktrees`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktrees_dir: Option<String>,

    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub model_providers: BTreeMap<String, ModelProviderToml>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, ModelToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxToml>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, McpServerToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HooksToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computer_use: Option<ComputerUseToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memories: Option<MemoriesToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notifications: Option<NotificationsToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_review: Option<AutomaticReviewToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillsToml>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub features: Option<FeaturesToml>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, ProfileToml>,
    /// Per-folder trust: `[projects."C:\\code\\app"] trust_level = "trusted"`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub projects: BTreeMap<String, ProjectTrustToml>,
}

/// A profile overrides a subset of top-level settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ProfileToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ApprovalPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<SandboxMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextToml>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ProjectTrustToml {
    /// `trusted` or `untrusted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_level: Option<String>,
}

/// `[model_providers.<id>]`: one OpenAI-compatible endpoint (usually vLLM).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ModelProviderToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Defaults to `http://localhost:8000/v1`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Plain API key. Prefer `api_key_env` or the desktop's encrypted store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Environment variable holding the API key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub query_params: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_api: Option<WireApi>,
    /// Concurrent requests allowed against this endpoint (vLLM batches them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_max_retries: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_max_retries: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_timeout_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// `[models.<key>]`: per-model settings layered on top of a preset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ModelToml {
    /// Provider id from `[model_providers]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id as served (`/v1/models`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Preset id from `presets/models.toml` to inherit from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Overrides the discovered `max_model_len`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilitiesToml>,
    /// Passed verbatim as `chat_template_kwargs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<Value>,
    /// Merged verbatim into the request body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<Value>,
    /// Effort level → JSON merged into the request body for that level.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_effort_map: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<ToolProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_history: Option<ReasoningHistory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinate_space: Option<CoordinateSpace>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<StructuredOutputMode>,
    /// Hint for the client-side fallback tool-call parser (`auto` by default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_format: Option<String>,
    /// Max image edge in pixels sent to this model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_image_px: Option<u32>,
    /// HF tokenizer.json path for local token estimates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenizer_path: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ModelCapabilitiesToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tools: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
}

/// `[context]`: smart context engine thresholds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ContextToml {
    /// Tier 1 pruning threshold as a fraction of the budget (default 0.70).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune_at: Option<f64>,
    /// Tier 2 compaction threshold (default 0.85).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_at: Option<f64>,
    /// Fraction of the window kept verbatim on compaction (default 0.20).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_recent_ratio: Option<f64>,
    /// Target fraction of the window after compaction (default 0.50).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_after_compact: Option<f64>,
    /// Fraction of the window reserved for output, capped by max_output_tokens (default 0.25).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_output_ratio: Option<f64>,
    /// Safety margin fraction (default 0.03).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_ratio: Option<f64>,
    /// Max tokens for one tool output kept inline; 0 = auto (scaled to window).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output_max_tokens: Option<u32>,
    /// Tool outputs older than this many turns become stubs during pruning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stub_after_turns: Option<u32>,
    /// Images kept as images (older become text stubs). Default 2.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_images: Option<u32>,
    /// MCP tool schemas above this fraction of the window switch to lazy loading (default 0.15).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_tool_budget_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes_max_bytes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memories_max_tokens: Option<u32>,
    /// Model key used for compaction (otherwise the `compactor` role).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compactor_model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct SandboxToml {
    /// Windows backend: `restricted-token` (default), `appcontainer`, or `none`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows_backend: Option<String>,
    /// Allow network inside workspace-write (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_access: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub writable_roots: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct McpServerToml {
    // stdio
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    // http
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bearer_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bearer_token_env_var: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Use OAuth for this HTTP server (tokens are stored by the engine).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_timeout_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_timeout_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    /// Tools that never need approval for this server.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub auto_approve_tools: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct McpToml {
    /// `auto` (default), `always` or `never` lazy tool loading via `search_tools`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lazy_tools: Option<String>,
}

/// `[[hooks.<event>]]` command hooks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct HooksToml {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub session_start: Vec<HookToml>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub user_prompt_submit: Vec<HookToml>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub pre_tool_use: Vec<HookToml>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_tool_use: Vec<HookToml>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<HookToml>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notification: Vec<HookToml>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct HookToml {
    /// Shell command to run. Receives the event JSON on stdin.
    pub command: String,
    /// Regex on tool name (tool hooks only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct ComputerUseToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Executable names (e.g. `notepad.exe`) the agent may control.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_apps: Vec<String>,
    /// Per-action approval (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_approval: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kill_switch: Option<String>,
    /// Prefer background (non-intrusive) automation; default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefer_background: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct BrowserToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Sites the agent may use without asking (host globs).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_sites: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocked_sites: Vec<String>,
    /// Enables `browser_eval` and raw CDP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_mode: Option<bool>,
    /// CDP websocket/HTTP endpoint used by headless `exec` (e.g. http://127.0.0.1:9222).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cdp_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct MemoriesToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Propose memories after threads end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct NotificationsToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_needed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_awake: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct AutomaticReviewToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Extra rubric text appended to the reviewer prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rubric: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct SkillsToml {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct FeaturesToml {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_suggestions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_title: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_snapshots: Option<bool>,
}
