//! Model presets (`presets/models.toml`, embedded at build time, plus an
//! optional user override file `~/.odex/presets.toml`).

use std::collections::BTreeMap;
use std::path::Path;

use odex_protocol::config_types::ModelToml;
use odex_protocol::PresetInfo;
use serde::Deserialize;

const BUILTIN: &str = include_str!("../../../presets/models.toml");

#[derive(Debug, Clone, Deserialize)]
struct PresetFile {
    #[serde(default)]
    presets: BTreeMap<String, PresetToml>,
}

#[derive(Debug, Clone, Deserialize)]
struct PresetToml {
    display_name: String,
    #[serde(default)]
    family: String,
    #[serde(default, rename = "match")]
    match_patterns: Vec<String>,
    #[serde(default)]
    serve: String,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    settings: ModelToml,
}

#[derive(Debug, Clone)]
pub struct Presets {
    list: Vec<PresetInfo>,
}

impl Presets {
    pub fn builtin() -> Self {
        Self::parse(BUILTIN).expect("built-in presets/models.toml must parse")
    }

    /// Built-ins with user overrides merged on top (same id replaces).
    pub fn load(user_file: &Path) -> Self {
        let mut p = Self::builtin();
        if let Ok(text) = std::fs::read_to_string(user_file) {
            match Self::parse(&text) {
                Ok(user) => {
                    for u in user.list {
                        if let Some(existing) = p.list.iter_mut().find(|x| x.id == u.id) {
                            *existing = u;
                        } else {
                            // user presets take precedence when matching
                            p.list.insert(0, u);
                        }
                    }
                }
                Err(e) => tracing::warn!("ignoring {}: {e}", user_file.display()),
            }
        }
        p
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let file: PresetFile = toml::from_str(text)?;
        // Keep the file order: toml BTreeMap sorts keys, so re-read order from the text.
        let mut order: Vec<String> = Vec::new();
        for line in text.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("[presets.") {
                let id = rest.trim_end_matches(']').split('.').next().unwrap_or("").to_string();
                if !id.is_empty() && !order.contains(&id) {
                    order.push(id);
                }
            }
        }
        let mut list = Vec::new();
        for id in order {
            if let Some(p) = file.presets.get(&id) {
                list.push(PresetInfo {
                    id: id.clone(),
                    display_name: p.display_name.clone(),
                    family: p.family.clone(),
                    match_patterns: p.match_patterns.clone(),
                    serve_command: p.serve.clone(),
                    notes: p.notes.clone(),
                    settings: p.settings.clone(),
                });
            }
        }
        Ok(Self { list })
    }

    pub fn all(&self) -> &[PresetInfo] {
        &self.list
    }

    pub fn get(&self, id: &str) -> Option<&PresetInfo> {
        self.list.iter().find(|p| p.id == id)
    }

    /// First preset whose glob matches the served model id (case-insensitive).
    pub fn match_model(&self, model_id: &str) -> Option<&PresetInfo> {
        let lower = model_id.to_lowercase();
        self.list.iter().find(|p| {
            p.match_patterns
                .iter()
                .any(|pat| wildmatch::WildMatch::new(&pat.to_lowercase()).matches(&lower))
        })
    }

    pub fn generic(&self) -> Option<&PresetInfo> {
        self.get("generic")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses_and_matches() {
        let p = Presets::builtin();
        assert!(p.all().len() >= 10);
        assert_eq!(p.match_model("Qwen/Qwen3-Coder-30B-A3B-Instruct").unwrap().id, "qwen3-coder");
        assert_eq!(p.match_model("openai/gpt-oss-120b").unwrap().id, "gpt-oss");
        assert_eq!(p.match_model("zai-org/GLM-4.5-Air").unwrap().id, "glm-4");
        assert_eq!(p.match_model("mistralai/Devstral-Small-2507").unwrap().id, "devstral");
        assert_eq!(p.match_model("Qwen/Qwen3-VL-8B-Instruct").unwrap().id, "qwen3-vl");
        assert!(p.match_model("totally-unknown").is_none());
        assert!(p.generic().is_some());
        // every preset has serve flags with prefix caching
        for preset in p.all() {
            assert!(preset.serve_command.contains("--enable-prefix-caching"), "{}", preset.id);
        }
    }

    #[test]
    fn effort_maps_present() {
        let p = Presets::builtin();
        let q = p.get("qwen3-thinking").unwrap();
        assert!(q.settings.reasoning_effort_map.contains_key("none"));
    }
}
