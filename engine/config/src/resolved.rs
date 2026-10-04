//! Concrete settings with defaults filled in.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use odex_protocol::config_types::*;
use odex_protocol::*;
use serde_json::Value;

use crate::presets::Presets;
use crate::{OdexHome, DEFAULT_BASE_URL, DEFAULT_PROVIDER_ID};

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// Config-provided key (plain or from env). In-memory secrets from the
    /// desktop take precedence at request time.
    pub api_key: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub query_params: BTreeMap<String, String>,
    pub wire_api: WireApi,
    pub max_concurrent_requests: u32,
    pub request_max_retries: u32,
    pub stream_max_retries: u32,
    pub stream_idle_timeout: Duration,
    pub connect_timeout: Duration,
    pub request_timeout: Option<Duration>,
    pub enabled: bool,
}

impl ResolvedProvider {
    pub fn from_toml(id: &str, p: &ModelProviderToml) -> Self {
        let api_key = p
            .api_key_env
            .as_ref()
            .and_then(|v| std::env::var(v).ok())
            .or_else(|| p.api_key.clone())
            .filter(|k| !k.is_empty());
        let mut base_url = p.base_url.clone().unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        while base_url.ends_with('/') {
            base_url.pop();
        }
        Self {
            id: id.to_string(),
            name: p.name.clone().unwrap_or_else(|| id.to_string()),
            base_url,
            api_key,
            headers: p.headers.clone(),
            query_params: p.query_params.clone(),
            wire_api: p.wire_api.unwrap_or_default(),
            max_concurrent_requests: p.max_concurrent_requests.unwrap_or(8).max(1),
            request_max_retries: p.request_max_retries.unwrap_or(4),
            stream_max_retries: p.stream_max_retries.unwrap_or(5),
            stream_idle_timeout: Duration::from_millis(p.stream_idle_timeout_ms.unwrap_or(300_000) as u64),
            connect_timeout: Duration::from_millis(p.connect_timeout_ms.unwrap_or(10_000) as u64),
            request_timeout: p.request_timeout_ms.map(|ms| Duration::from_millis(ms as u64)),
            enabled: p.enabled.unwrap_or(true),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sampling {
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i32>,
    pub min_p: Option<f64>,
    pub repetition_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModel {
    pub key: String,
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    /// Explicit override; `None` means use the discovered `max_model_len`.
    pub context_window: Option<u32>,
    pub max_output_tokens: u32,
    pub sampling: Sampling,
    pub capabilities: ModelCapabilities,
    pub chat_template_kwargs: Option<Value>,
    pub extra_body: Option<Value>,
    pub reasoning_effort_map: BTreeMap<ReasoningEffort, Value>,
    pub default_effort: Option<ReasoningEffort>,
    pub tool_profile: ToolProfile,
    pub reasoning_history: ReasoningHistory,
    pub coordinate_space: CoordinateSpace,
    pub structured_output: StructuredOutputMode,
    pub tool_call_format: Option<String>,
    pub max_image_px: u32,
    pub tokenizer_path: Option<PathBuf>,
    pub preset: Option<String>,
    /// Defined in `[models]` (vs discovered only).
    pub configured: bool,
}

impl ResolvedModel {
    /// Build from a `[models.<key>]` entry layered over its preset.
    pub fn from_toml(key: &str, m: &ModelToml, presets: &Presets, default_provider: &str) -> Self {
        let model_id = m.model.clone().unwrap_or_else(|| key.to_string());
        let preset = m
            .preset
            .as_deref()
            .and_then(|p| presets.get(p))
            .or_else(|| presets.match_model(&model_id))
            .or_else(|| presets.generic());
        let base = preset.map(|p| p.settings.clone()).unwrap_or_default();
        let merged = merge_model(&base, m);
        Self::from_merged(
            key,
            m.provider.clone().unwrap_or_else(|| default_provider.to_string()),
            model_id,
            &merged,
            preset.map(|p| p.id.clone()),
            true,
        )
    }

    /// Build for a model discovered on an endpoint with no `[models]` entry.
    pub fn discovered(provider_id: &str, model_id: &str, presets: &Presets) -> Self {
        let preset = presets.match_model(model_id).or_else(|| presets.generic());
        let base = preset.map(|p| p.settings.clone()).unwrap_or_default();
        Self::from_merged(
            &format!("{provider_id}:{model_id}"),
            provider_id.to_string(),
            model_id.to_string(),
            &base,
            preset.map(|p| p.id.clone()),
            false,
        )
    }

    fn from_merged(
        key: &str,
        provider_id: String,
        model_id: String,
        m: &ModelToml,
        preset: Option<String>,
        configured: bool,
    ) -> Self {
        let caps = m.capabilities.unwrap_or_default();
        let mut effort_map = BTreeMap::new();
        for (k, v) in &m.reasoning_effort_map {
            if let Ok(e) = serde_json::from_value::<ReasoningEffort>(Value::String(k.clone())) {
                effort_map.insert(e, v.clone());
            }
        }
        let display_name = m.display_name.clone().unwrap_or_else(|| {
            model_id.rsplit('/').next().unwrap_or(&model_id).to_string()
        });
        Self {
            key: key.to_string(),
            provider_id,
            model_id,
            display_name,
            context_window: m.context_window,
            max_output_tokens: m.max_output_tokens.unwrap_or(8192),
            sampling: Sampling {
                temperature: m.temperature,
                top_p: m.top_p,
                top_k: m.top_k,
                min_p: m.min_p,
                repetition_penalty: m.repetition_penalty,
                presence_penalty: m.presence_penalty,
                frequency_penalty: m.frequency_penalty,
            },
            capabilities: ModelCapabilities {
                tools: caps.tools.unwrap_or(true),
                vision: caps.vision.unwrap_or(false),
                parallel_tools: caps.parallel_tools.unwrap_or(false),
                reasoning: caps.reasoning.unwrap_or(false),
            },
            chat_template_kwargs: m.chat_template_kwargs.clone(),
            extra_body: m.extra_body.clone(),
            reasoning_effort_map: effort_map,
            default_effort: m.default_reasoning_effort,
            tool_profile: m.tool_profile.unwrap_or_default(),
            reasoning_history: m.reasoning_history.unwrap_or_default(),
            coordinate_space: m.coordinate_space.unwrap_or_default(),
            structured_output: m.structured_output.unwrap_or_default(),
            tool_call_format: m.tool_call_format.clone(),
            max_image_px: m.max_image_px.unwrap_or(1568),
            tokenizer_path: m.tokenizer_path.as_ref().map(PathBuf::from),
            preset,
            configured,
        }
    }
}

/// Overlay `over` on `base` field-by-field (Some wins; maps merge).
pub fn merge_model(base: &ModelToml, over: &ModelToml) -> ModelToml {
    let mut b = serde_json::to_value(base).unwrap_or_default();
    let o = serde_json::to_value(over).unwrap_or_default();
    crate::merge_json(&mut b, &o);
    serde_json::from_value(b).unwrap_or_else(|_| over.clone())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextSettings {
    pub prune_at: f64,
    pub compact_at: f64,
    pub keep_recent_ratio: f64,
    pub target_after_compact: f64,
    pub reserve_output_ratio: f64,
    pub margin_ratio: f64,
    /// 0 = auto-scale to the window.
    pub tool_output_max_tokens: u32,
    pub stub_after_turns: u32,
    pub max_images: u32,
    pub mcp_tool_budget_ratio: f64,
    pub notes_max_bytes: u32,
    pub memories_max_tokens: u32,
    pub compactor_model: Option<String>,
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self::from_toml(&ContextToml::default())
    }
}

impl ContextSettings {
    pub fn from_toml(c: &ContextToml) -> Self {
        let clamp = |v: Option<f64>, d: f64, lo: f64, hi: f64| v.unwrap_or(d).clamp(lo, hi);
        let prune_at = clamp(c.prune_at, 0.70, 0.2, 0.98);
        let compact_at = clamp(c.compact_at, 0.85, prune_at, 0.99);
        Self {
            prune_at,
            compact_at,
            keep_recent_ratio: clamp(c.keep_recent_ratio, 0.20, 0.02, 0.45),
            target_after_compact: clamp(c.target_after_compact, 0.50, 0.2, 0.8),
            reserve_output_ratio: clamp(c.reserve_output_ratio, 0.25, 0.05, 0.5),
            margin_ratio: clamp(c.margin_ratio, 0.03, 0.0, 0.2),
            tool_output_max_tokens: c.tool_output_max_tokens.unwrap_or(0),
            stub_after_turns: c.stub_after_turns.unwrap_or(3),
            max_images: c.max_images.unwrap_or(2),
            mcp_tool_budget_ratio: clamp(c.mcp_tool_budget_ratio, 0.15, 0.01, 0.9),
            notes_max_bytes: c.notes_max_bytes.unwrap_or(8192),
            memories_max_tokens: c.memories_max_tokens.unwrap_or(1500),
            compactor_model: c.compactor_model.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxSettings {
    pub windows_backend: String,
    pub network_access: bool,
    pub writable_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputerUseSettings {
    pub enabled: bool,
    pub allowed_apps: Vec<String>,
    pub require_approval: bool,
    pub kill_switch: String,
    pub prefer_background: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrowserSettings {
    pub enabled: bool,
    pub allowed_sites: Vec<String>,
    pub blocked_sites: Vec<String>,
    pub developer_mode: bool,
    pub cdp_url: Option<String>,
}

/// Fully-resolved settings for one scope (user or a project).
#[derive(Debug, Clone)]
pub struct Settings {
    pub raw: ConfigToml,
    pub providers: BTreeMap<String, ResolvedProvider>,
    /// Configured models by key (discovered models are added by the registry).
    pub models: BTreeMap<String, ResolvedModel>,
    /// Role → model key, as configured (fallback to `main` happens at lookup).
    pub roles: BTreeMap<ModelRole, String>,
    pub permission_mode: PermissionMode,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub project_doc_max_bytes: usize,
    pub custom_instructions: Option<String>,
    pub default_shell: String,
    pub worktrees_dir: PathBuf,
    pub context: ContextSettings,
    pub sandbox: SandboxSettings,
    pub mcp_servers: BTreeMap<String, McpServerToml>,
    pub mcp_lazy_tools: String,
    pub hooks: HooksToml,
    pub computer_use: ComputerUseSettings,
    pub browser: BrowserSettings,
    pub memories_enabled: bool,
    pub memories_generate: bool,
    pub memories_max_tokens: u32,
    pub auto_review: bool,
    pub auto_review_rubric: Option<String>,
    pub disabled_skills: Vec<String>,
    pub follow_up_suggestions: bool,
    pub auto_title: bool,
    pub undo_snapshots: bool,
    /// True when no provider is configured (onboarding needed).
    pub unconfigured: bool,
}

impl Settings {
    pub fn resolve(cfg: &ConfigToml, presets: &Presets, home: &OdexHome) -> Self {
        let mut providers = BTreeMap::new();
        for (id, p) in &cfg.model_providers {
            providers.insert(id.clone(), ResolvedProvider::from_toml(id, p));
        }
        let unconfigured = providers.is_empty();
        if unconfigured {
            // Env override for tests/smoke runs, else localhost:8000.
            let base = std::env::var("ODEX_BASE_URL")
                .ok()
                .or_else(|| std::env::var("ODEX_E2E_BASE_URL").ok())
                .filter(|s| !s.is_empty());
            let p = ModelProviderToml { base_url: base, ..Default::default() };
            providers.insert(DEFAULT_PROVIDER_ID.into(), ResolvedProvider::from_toml(DEFAULT_PROVIDER_ID, &p));
        }
        let default_provider = providers.keys().next().cloned().unwrap_or_else(|| DEFAULT_PROVIDER_ID.into());

        let mut models = BTreeMap::new();
        for (key, m) in &cfg.models {
            models.insert(key.clone(), ResolvedModel::from_toml(key, m, presets, &default_provider));
        }

        let mut roles = BTreeMap::new();
        for (k, v) in &cfg.roles {
            if let Ok(role) = serde_json::from_value::<ModelRole>(Value::String(k.clone())) {
                roles.insert(role, v.clone());
            }
        }
        if let Some(m) = &cfg.model {
            roles.entry(ModelRole::Main).or_insert_with(|| m.clone());
            // `model = ...` at top level always wins for main
            roles.insert(ModelRole::Main, m.clone());
        }

        let ctx = ContextSettings::from_toml(&cfg.context.clone().unwrap_or_default());
        let sb = cfg.sandbox.clone().unwrap_or_default();
        let cu = cfg.computer_use.clone().unwrap_or_default();
        let br = cfg.browser.clone().unwrap_or_default();
        let mem = cfg.memories.clone().unwrap_or_default();
        let ar = cfg.automatic_review.clone().unwrap_or_default();
        let feats = cfg.features.clone().unwrap_or_default();

        let permission_mode = cfg.permission_mode.unwrap_or_else(|| match cfg.sandbox_mode {
            Some(SandboxMode::ReadOnly) => PermissionMode::ReadOnly,
            Some(SandboxMode::DangerFullAccess) => PermissionMode::FullAccess,
            _ => PermissionMode::Auto,
        });

        Self {
            raw: cfg.clone(),
            providers,
            models,
            roles,
            permission_mode,
            reasoning_effort: cfg.reasoning_effort,
            project_doc_max_bytes: cfg.project_doc_max_bytes.unwrap_or(32 * 1024) as usize,
            custom_instructions: cfg.custom_instructions.clone().filter(|s| !s.trim().is_empty()),
            default_shell: cfg.default_shell.clone().unwrap_or_else(default_shell),
            worktrees_dir: cfg
                .worktrees_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| home.default_worktrees_dir()),
            context: ctx,
            sandbox: SandboxSettings {
                windows_backend: sb.windows_backend.unwrap_or_else(|| "restricted-token".into()),
                network_access: sb.network_access.unwrap_or(false),
                writable_roots: sb.writable_roots.iter().map(PathBuf::from).collect(),
            },
            mcp_servers: cfg.mcp_servers.clone(),
            mcp_lazy_tools: cfg
                .mcp
                .as_ref()
                .and_then(|m| m.lazy_tools.clone())
                .unwrap_or_else(|| "auto".into()),
            hooks: cfg.hooks.clone().unwrap_or_default(),
            computer_use: ComputerUseSettings {
                enabled: cu.enabled.unwrap_or(false),
                allowed_apps: cu.allowed_apps,
                require_approval: cu.require_approval.unwrap_or(true),
                kill_switch: cu.kill_switch.unwrap_or_else(|| "Ctrl+Alt+Escape".into()),
                prefer_background: cu.prefer_background.unwrap_or(true),
            },
            browser: BrowserSettings {
                enabled: br.enabled.unwrap_or(true),
                allowed_sites: br.allowed_sites,
                blocked_sites: br.blocked_sites,
                developer_mode: br.developer_mode.unwrap_or(false),
                cdp_url: br.cdp_url,
            },
            memories_enabled: mem.enabled.unwrap_or(false),
            memories_generate: mem.generate.unwrap_or(mem.enabled.unwrap_or(false)),
            memories_max_tokens: mem.max_tokens.unwrap_or(1500),
            auto_review: ar.enabled.unwrap_or(false),
            auto_review_rubric: ar.rubric,
            disabled_skills: cfg.skills.clone().unwrap_or_default().disabled,
            follow_up_suggestions: feats.follow_up_suggestions.unwrap_or(true),
            auto_title: feats.auto_title.unwrap_or(true),
            undo_snapshots: feats.undo_snapshots.unwrap_or(true),
            unconfigured,
        }
    }

    /// Model key configured for a role, falling back to `main`.
    pub fn role_model(&self, role: ModelRole) -> Option<&str> {
        self.roles
            .get(&role)
            .or_else(|| self.roles.get(&ModelRole::Main))
            .map(|s| s.as_str())
    }
}

pub fn default_shell() -> String {
    if cfg!(windows) {
        "powershell".into()
    } else if std::path::Path::new("/bin/zsh").exists() && cfg!(target_os = "macos") {
        "zsh".into()
    } else {
        "bash".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_inherits_preset_and_overrides() {
        let presets = Presets::builtin();
        let m = ModelToml {
            model: Some("Qwen/Qwen3-Coder-30B-A3B-Instruct".into()),
            temperature: Some(0.2),
            ..Default::default()
        };
        let r = ResolvedModel::from_toml("coder", &m, &presets, "local");
        assert_eq!(r.preset.as_deref(), Some("qwen3-coder"));
        assert_eq!(r.sampling.temperature, Some(0.2));
        assert_eq!(r.sampling.top_k, Some(20));
        assert!(r.capabilities.tools);
        assert_eq!(r.max_output_tokens, 16384);
        assert_eq!(r.provider_id, "local");
    }

    #[test]
    fn discovered_model_key() {
        let presets = Presets::builtin();
        let r = ResolvedModel::discovered("gpu", "openai/gpt-oss-20b", &presets);
        assert_eq!(r.key, "gpu:openai/gpt-oss-20b");
        assert!(r.capabilities.reasoning);
        assert!(r.reasoning_effort_map.contains_key(&ReasoningEffort::High));
    }

    #[test]
    fn unconfigured_has_local_default() {
        let home = OdexHome::at(std::env::temp_dir().join("odex-test-home-x"));
        let s = Settings::resolve(&ConfigToml::default(), &Presets::builtin(), &home);
        assert!(s.unconfigured);
        assert!(s.providers.contains_key("local"));
        assert_eq!(s.context.compact_at, 0.85);
        assert_eq!(s.permission_mode, PermissionMode::Auto);
    }

    #[test]
    fn role_fallback() {
        let home = OdexHome::at(std::env::temp_dir().join("odex-test-home-y"));
        let mut cfg = ConfigToml::default();
        cfg.model = Some("main-model".into());
        cfg.roles.insert("compactor".into(), "small".into());
        let s = Settings::resolve(&cfg, &Presets::builtin(), &home);
        assert_eq!(s.role_model(ModelRole::Compactor), Some("small"));
        assert_eq!(s.role_model(ModelRole::Reviewer), Some("main-model"));
    }

    #[test]
    fn context_clamps() {
        let c = ContextSettings::from_toml(&ContextToml {
            prune_at: Some(0.9),
            compact_at: Some(0.5),
            ..Default::default()
        });
        assert!(c.compact_at >= c.prune_at);
    }
}
