//! Model registry: configured + discovered models across endpoints, role
//! resolution, capability cache (`~/.odex/models_cache.json`).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};

use odex_config::{Presets, ResolvedModel, Settings};
use odex_protocol::*;

use crate::client::LlmClient;
use crate::discovery;
use crate::tokens::TokenEstimator;

/// Default window when nothing reports one.
pub const FALLBACK_CONTEXT_WINDOW: u32 = 32_768;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedModel {
    pub capabilities: Option<ModelCapabilities>,
    pub structured_output: Option<StructuredOutputMode>,
    pub max_model_len: Option<u32>,
    pub server_version: Option<String>,
    pub doctor_ran_at: Option<i64>,
    pub chars_per_token: Option<f64>,
    pub doctor: Option<DoctorReport>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelsCache {
    /// provider id → model id → cached facts
    #[serde(default)]
    pub providers: BTreeMap<String, BTreeMap<String, CachedModel>>,
}

#[derive(Debug, Clone, Default)]
struct ProviderState {
    health: Option<EndpointHealth>,
    version: Option<String>,
    models: Vec<DiscoveredModel>,
    error: Option<String>,
}

/// A resolved model ready to call.
#[derive(Clone)]
pub struct ModelHandle {
    pub model: ResolvedModel,
    pub client: Arc<LlmClient>,
    pub context_window: u32,
}

impl ModelHandle {
    pub fn key(&self) -> &str {
        &self.model.key
    }
}

pub struct ModelRegistry {
    presets: Presets,
    settings: RwLock<Settings>,
    clients: RwLock<BTreeMap<String, Arc<LlmClient>>>,
    state: RwLock<BTreeMap<String, ProviderState>>,
    cache: RwLock<ModelsCache>,
    cache_path: Option<PathBuf>,
    secrets: RwLock<BTreeMap<String, String>>,
    estimators: Mutex<HashMap<String, TokenEstimator>>,
}

pub fn secret_key_for_provider(id: &str) -> String {
    format!("provider:{id}:api_key")
}

impl ModelRegistry {
    pub fn new(settings: Settings, presets: Presets, cache_path: Option<PathBuf>) -> Self {
        let cache = cache_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let r = Self {
            presets,
            settings: RwLock::new(settings),
            clients: RwLock::new(BTreeMap::new()),
            state: RwLock::new(BTreeMap::new()),
            cache: RwLock::new(cache),
            cache_path,
            secrets: RwLock::new(BTreeMap::new()),
            estimators: Mutex::new(HashMap::new()),
        };
        r.rebuild_clients();
        r
    }

    pub fn presets(&self) -> &Presets {
        &self.presets
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    pub fn update_settings(&self, settings: Settings) {
        *self.settings.write().unwrap() = settings;
        self.rebuild_clients();
    }

    pub fn set_secrets(&self, secrets: BTreeMap<String, String>) {
        *self.secrets.write().unwrap() = secrets;
        self.apply_secrets();
    }

    pub fn set_secret(&self, key: &str, value: Option<String>) {
        {
            let mut s = self.secrets.write().unwrap();
            match value {
                Some(v) if !v.is_empty() => {
                    s.insert(key.to_string(), v);
                }
                _ => {
                    s.remove(key);
                }
            }
        }
        self.apply_secrets();
    }

    fn apply_secrets(&self) {
        let secrets = self.secrets.read().unwrap();
        for (id, c) in self.clients.read().unwrap().iter() {
            if let Some(k) = secrets.get(&secret_key_for_provider(id)) {
                c.set_api_key(Some(k.clone()));
            } else {
                c.set_api_key(c.provider.api_key.clone());
            }
        }
    }

    fn rebuild_clients(&self) {
        let settings = self.settings.read().unwrap();
        let mut clients = self.clients.write().unwrap();
        let mut next = BTreeMap::new();
        for (id, p) in &settings.providers {
            // keep the existing client (and its queue) when unchanged
            let keep = clients.get(id).filter(|c| &c.provider == p).cloned();
            let c = keep.unwrap_or_else(|| Arc::new(LlmClient::new(p.clone(), None)));
            next.insert(id.clone(), c);
        }
        *clients = next;
        drop(clients);
        drop(settings);
        self.apply_secrets();
    }

    pub fn client(&self, provider_id: &str) -> Option<Arc<LlmClient>> {
        self.clients.read().unwrap().get(provider_id).cloned()
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.clients.read().unwrap().keys().cloned().collect()
    }

    /// Probe every enabled endpoint in parallel (`/v1/models`, `/health`, `/version`).
    pub async fn refresh(&self) {
        let clients: Vec<(String, Arc<LlmClient>)> = self
            .clients
            .read()
            .unwrap()
            .iter()
            .filter(|(_, c)| c.provider.enabled)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let futs = clients.into_iter().map(|(id, c)| async move {
            let p = discovery::probe(&c).await;
            (id, p)
        });
        let results = futures::future::join_all(futs).await;
        let mut state = self.state.write().unwrap();
        let mut cache = self.cache.write().unwrap();
        for (id, p) in results {
            let health = if !p.reachable {
                EndpointHealth::Unreachable
            } else if p.error.is_some() || p.healthy == Some(false) {
                EndpointHealth::Degraded
            } else {
                EndpointHealth::Healthy
            };
            for m in &p.models {
                let entry = cache.providers.entry(id.clone()).or_default().entry(m.id.clone()).or_default();
                if m.max_model_len.is_some() {
                    entry.max_model_len = m.max_model_len;
                }
                if p.version.is_some() {
                    entry.server_version = p.version.clone();
                }
            }
            state.insert(
                id,
                ProviderState { health: Some(health), version: p.version, models: p.models, error: p.error },
            );
        }
        drop(state);
        drop(cache);
        self.save_cache();
    }

    pub fn save_cache(&self) {
        if let Some(path) = &self.cache_path {
            if let Ok(text) = serde_json::to_string_pretty(&*self.cache.read().unwrap()) {
                let _ = std::fs::write(path, text);
            }
        }
    }

    pub fn record_doctor(&self, report: &DoctorReport) {
        {
            let mut cache = self.cache.write().unwrap();
            let e = cache
                .providers
                .entry(report.provider_id.clone())
                .or_default()
                .entry(report.model_id.clone())
                .or_default();
            e.capabilities = Some(report.inferred_capabilities);
            e.doctor_ran_at = Some(report.ran_at);
            e.server_version = report.server_version.clone().or(e.server_version.clone());
            let structured_ok =
                report.checks.iter().any(|c| c.id == "structuredOutput" && c.status == CheckStatus::Pass);
            e.structured_output =
                Some(if structured_ok { StructuredOutputMode::JsonSchema } else { StructuredOutputMode::None });
            e.doctor = Some(report.clone());
        }
        self.save_cache();
    }

    pub fn cached(&self, provider: &str, model: &str) -> Option<CachedModel> {
        self.cache.read().unwrap().providers.get(provider).and_then(|m| m.get(model)).cloned()
    }

    fn discovered_len(&self, provider: &str, model_id: &str) -> Option<u32> {
        let st = self.state.read().unwrap();
        st.get(provider)
            .and_then(|s| s.models.iter().find(|m| m.id == model_id))
            .and_then(|m| m.max_model_len)
            .or_else(|| self.cached(provider, model_id).and_then(|c| c.max_model_len))
    }

    /// Models a reachable endpoint currently lists (`None` when it was never
    /// probed, is unreachable, or listed nothing).
    pub fn served_models(&self, provider: &str) -> Option<Vec<DiscoveredModel>> {
        let st = self.state.read().unwrap();
        let s = st.get(provider)?;
        if matches!(s.health, None | Some(EndpointHealth::Unreachable)) || s.models.is_empty() {
            return None;
        }
        Some(s.models.clone())
    }

    fn is_discovered(&self, provider: &str, model_id: &str) -> bool {
        self.state.read().unwrap().get(provider).map(|s| s.models.iter().any(|m| m.id == model_id)).unwrap_or(false)
    }

    /// Apply cached Doctor findings unless config pinned them explicitly.
    fn with_cache(&self, mut m: ResolvedModel) -> ResolvedModel {
        let settings = self.settings.read().unwrap();
        let raw = settings.raw.models.get(&m.key);
        if let Some(c) = self.cached(&m.provider_id, &m.model_id) {
            if raw.and_then(|r| r.capabilities).is_none() {
                if let Some(caps) = c.capabilities {
                    // Doctor can only *downgrade* tools/parallel or *detect* vision/reasoning
                    m.capabilities = ModelCapabilities {
                        tools: m.capabilities.tools && caps.tools,
                        parallel_tools: m.capabilities.parallel_tools && caps.parallel_tools,
                        vision: caps.vision,
                        reasoning: m.capabilities.reasoning || caps.reasoning,
                    };
                }
            }
            if raw.and_then(|r| r.structured_output).is_none() {
                if let Some(so) = c.structured_output {
                    m.structured_output = so;
                }
            }
        }
        m
    }

    /// Resolve a model key: `[models.<key>]`, `provider:model_id`, or a bare
    /// served model id on any endpoint.
    pub fn resolve(&self, key: &str) -> Option<ModelHandle> {
        let settings = self.settings.read().unwrap().clone();
        let m = if let Some(m) = settings.models.get(key) {
            m.clone()
        } else if let Some((prov, id)) = key.split_once(':').filter(|(p, _)| settings.providers.contains_key(*p)) {
            settings
                .models
                .values()
                .find(|m| m.provider_id == prov && m.model_id == id)
                .cloned()
                .unwrap_or_else(|| ResolvedModel::discovered(prov, id, &self.presets))
        } else if let Some(m) = settings.models.values().find(|m| m.model_id == key) {
            m.clone()
        } else {
            let st = self.state.read().unwrap();
            let prov = st.iter().find(|(_, s)| s.models.iter().any(|d| d.id == key)).map(|(p, _)| p.clone())?;
            ResolvedModel::discovered(&prov, key, &self.presets)
        };
        let m = self.with_cache(m);
        let client = self.client(&m.provider_id)?;
        let context_window = m
            .context_window
            .or_else(|| self.discovered_len(&m.provider_id, &m.model_id))
            .unwrap_or(FALLBACK_CONTEXT_WINDOW);
        Some(ModelHandle { model: m, client, context_window })
    }

    /// Model for a role, with an optional per-thread override for `main`.
    pub fn resolve_role(&self, role: ModelRole, thread_main: Option<&str>) -> Option<ModelHandle> {
        if role == ModelRole::Main {
            if let Some(k) = thread_main {
                if let Some(h) = self.resolve(k) {
                    return Some(h);
                }
            }
        }
        let settings = self.settings.read().unwrap().clone();
        if let Some(k) = settings.roles.get(&role) {
            if let Some(h) = self.resolve(k) {
                return Some(h);
            }
        }
        if role != ModelRole::Main {
            if let Some(k) = thread_main {
                if let Some(h) = self.resolve(k) {
                    return Some(h);
                }
            }
        }
        self.default_main().and_then(|k| self.resolve(&k))
    }

    /// Configured main model, else the first discovered chat model.
    pub fn default_main(&self) -> Option<String> {
        let settings = self.settings.read().unwrap();
        if let Some(k) = settings.roles.get(&ModelRole::Main) {
            return Some(k.clone());
        }
        if let Some((k, _)) = settings.models.iter().next() {
            return Some(k.clone());
        }
        drop(settings);
        let st = self.state.read().unwrap();
        for (p, s) in st.iter() {
            if let Some(m) = s.models.iter().find(|m| !m.id.to_lowercase().contains("embed")) {
                return Some(format!("{p}:{}", m.id));
            }
        }
        None
    }

    pub fn list(&self) -> Vec<ModelInfo> {
        let settings = self.settings.read().unwrap().clone();
        let mut out: Vec<ModelInfo> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let roles_for = |key: &str, model_id: &str, provider: &str| -> Vec<ModelRole> {
            settings
                .roles
                .iter()
                .filter(|(_, v)| v.as_str() == key || v.as_str() == model_id || **v == format!("{provider}:{model_id}"))
                .map(|(r, _)| *r)
                .collect()
        };
        let mk = |m: ResolvedModel, available: bool, this: &Self| -> ModelInfo {
            let m = this.with_cache(m);
            let window = m
                .context_window
                .or_else(|| this.discovered_len(&m.provider_id, &m.model_id))
                .unwrap_or(FALLBACK_CONTEXT_WINDOW);
            let mut efforts: Vec<ReasoningEffort> = m.reasoning_effort_map.keys().copied().collect();
            efforts.sort();
            ModelInfo {
                roles: roles_for(&m.key, &m.model_id, &m.provider_id),
                key: m.key.clone(),
                provider_id: m.provider_id.clone(),
                model_id: m.model_id.clone(),
                display_name: m.display_name.clone(),
                context_window: window,
                max_output_tokens: m.max_output_tokens,
                capabilities: m.capabilities,
                tool_profile: m.tool_profile,
                efforts,
                default_effort: m.default_effort,
                available,
                preset: m.preset.clone(),
            }
        };
        for m in settings.models.values() {
            seen.insert((m.provider_id.clone(), m.model_id.clone()));
            let avail = self.is_discovered(&m.provider_id, &m.model_id);
            out.push(mk(m.clone(), avail, self));
        }
        let st = self.state.read().unwrap().clone();
        for (p, s) in st.iter() {
            for d in &s.models {
                if seen.insert((p.clone(), d.id.clone())) {
                    out.push(mk(ResolvedModel::discovered(p, &d.id, &self.presets), true, self));
                }
            }
        }
        out
    }

    pub fn providers(&self) -> Vec<ProviderInfo> {
        let st = self.state.read().unwrap();
        self.clients
            .read()
            .unwrap()
            .iter()
            .map(|(id, c)| {
                let s = st.get(id).cloned().unwrap_or_default();
                ProviderInfo {
                    id: id.clone(),
                    name: c.provider.name.clone(),
                    base_url: c.provider.base_url.clone(),
                    wire_api: c.provider.wire_api,
                    enabled: c.provider.enabled,
                    has_api_key: c.has_api_key(),
                    health: s.health.unwrap_or(EndpointHealth::Unknown),
                    version: s.version,
                    models: s.models,
                    max_concurrent_requests: c.provider.max_concurrent_requests,
                    in_flight: c.stats.in_flight.load(std::sync::atomic::Ordering::Relaxed),
                    queued: c.stats.queued.load(std::sync::atomic::Ordering::Relaxed),
                    error: s.error,
                }
            })
            .collect()
    }

    /// Per-model estimator (calibrated from usage over time).
    pub fn estimator(&self, key: &str) -> TokenEstimator {
        let mut e = self.estimators.lock().unwrap();
        if let Some(x) = e.get(key) {
            return x.clone();
        }
        let ratio = self
            .resolve(key)
            .and_then(|h| self.cached(&h.model.provider_id, &h.model.model_id))
            .and_then(|c| c.chars_per_token);
        let est = ratio.map(TokenEstimator::with_ratio).unwrap_or_default();
        e.insert(key.to_string(), est.clone());
        est
    }

    pub fn calibrate(&self, key: &str, chars: usize, tokens: u32) {
        let ratio = {
            let mut e = self.estimators.lock().unwrap();
            let est = e.entry(key.to_string()).or_default();
            est.calibrate(chars, tokens);
            est.ratio()
        };
        if let Some(h) = self.resolve(key) {
            let mut cache = self.cache.write().unwrap();
            cache
                .providers
                .entry(h.model.provider_id.clone())
                .or_default()
                .entry(h.model.model_id.clone())
                .or_default()
                .chars_per_token = Some(ratio);
        }
    }
}
