//! Odex configuration: `~/.odex/config.toml`, profiles, project layers,
//! model presets and resolution into concrete settings.

pub mod edit;
pub mod home;
pub mod presets;
pub mod resolved;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use odex_protocol::config_types::*;

pub use home::{normalize_path, project_dir, OdexHome, PROJECT_DIR};
pub use presets::Presets;
pub use resolved::*;

/// Default endpoint when nothing is configured.
pub const DEFAULT_BASE_URL: &str = "http://localhost:8000/v1";
pub const DEFAULT_PROVIDER_ID: &str = "local";

/// Load and parse a config file; a missing file is an empty config.
pub fn load_file(path: &Path) -> anyhow::Result<(ConfigToml, Vec<String>)> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((ConfigToml::default(), vec![])),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    parse_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Parse config text, collecting unknown top-level keys as warnings.
pub fn parse_str(text: &str) -> anyhow::Result<(ConfigToml, Vec<String>)> {
    let cfg: ConfigToml = toml::from_str(text)?;
    let mut warnings = Vec::new();
    if let Ok(raw) = text.parse::<toml::Table>() {
        let known = serde_json::to_value(ConfigToml::default()).ok();
        let _ = known;
        const KNOWN: &[&str] = &[
            "model",
            "profile",
            "permission_mode",
            "approval_policy",
            "sandbox_mode",
            "reasoning_effort",
            "project_doc_max_bytes",
            "custom_instructions",
            "default_shell",
            "worktrees_dir",
            "worktrees",
            "review_instructions",
            "git",
            "roles",
            "model_providers",
            "models",
            "context",
            "sandbox",
            "mcp_servers",
            "mcp",
            "hooks",
            "computer_use",
            "browser",
            "memories",
            "notifications",
            "automatic_review",
            "skills",
            "features",
            "profiles",
            "projects",
        ];
        for k in raw.keys() {
            if !KNOWN.contains(&k.as_str()) {
                warnings.push(format!("unknown config key `{k}` (ignored)"));
            }
        }
    }
    Ok((cfg, warnings))
}

/// Deep-merge `overlay` onto `base` (overlay wins; maps merge recursively).
pub fn merge(base: &ConfigToml, overlay: &ConfigToml) -> ConfigToml {
    let mut b = serde_json::to_value(base).unwrap_or_default();
    let o = serde_json::to_value(overlay).unwrap_or_default();
    merge_json(&mut b, &o);
    serde_json::from_value(b).unwrap_or_else(|_| base.clone())
}

pub fn merge_json(base: &mut serde_json::Value, overlay: &serde_json::Value) {
    match (base, overlay) {
        (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
            for (k, v) in o {
                if v.is_null() {
                    continue;
                }
                match b.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge_json(existing, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => {
            if !o.is_null() {
                *b = o.clone();
            }
        }
    }
}

/// Apply the active profile (if any) on top of the base config.
pub fn apply_profile(cfg: &ConfigToml, profile: Option<&str>) -> ConfigToml {
    let name = profile.map(|s| s.to_string()).or_else(|| cfg.profile.clone());
    let Some(name) = name else { return cfg.clone() };
    let Some(p) = cfg.profiles.get(&name) else { return cfg.clone() };
    let overlay = ConfigToml {
        model: p.model.clone(),
        permission_mode: p.permission_mode,
        approval_policy: p.approval_policy,
        sandbox_mode: p.sandbox_mode,
        reasoning_effort: p.reasoning_effort,
        custom_instructions: p.custom_instructions.clone(),
        roles: p.roles.clone(),
        context: p.context.clone(),
        ..Default::default()
    };
    merge(cfg, &overlay)
}

/// Everything loaded from disk for one engine instance.
#[derive(Debug, Clone)]
pub struct ConfigStack {
    pub home: OdexHome,
    /// User config as written.
    pub user: ConfigToml,
    /// User config with the active profile applied.
    pub effective: ConfigToml,
    pub active_profile: Option<String>,
    pub warnings: Vec<String>,
    pub presets: Presets,
}

impl ConfigStack {
    pub fn load(home: &OdexHome, profile_override: Option<&str>) -> anyhow::Result<Self> {
        let (user, warnings) = load_file(&home.config_path())?;
        let active_profile = profile_override.map(|s| s.to_string()).or_else(|| user.profile.clone());
        let effective = apply_profile(&user, active_profile.as_deref());
        let presets = Presets::load(&home.user_presets_path());
        Ok(Self { home: home.clone(), user, effective, active_profile, warnings, presets })
    }

    /// Effective config for a specific project folder: adds the project's
    /// `.odex/config.toml` when the folder is trusted. Projects may not
    /// override providers, sandbox or trust (security-sensitive keys).
    pub fn for_project(&self, project_root: Option<&Path>) -> ConfigToml {
        let Some(root) = project_root else { return self.effective.clone() };
        if !self.is_trusted(root) {
            return self.effective.clone();
        }
        let path = project_dir(root).join("config.toml");
        match load_file(&path) {
            Ok((mut proj, _)) => {
                proj.model_providers.clear();
                proj.sandbox = None;
                proj.projects.clear();
                proj.profiles.clear();
                proj.profile = None;
                if proj.permission_mode == Some(odex_protocol::PermissionMode::FullAccess) {
                    proj.permission_mode = None;
                }
                proj.approval_policy = None;
                proj.sandbox_mode = None;
                merge(&self.effective, &proj)
            }
            Err(e) => {
                tracing::warn!("ignoring project config {}: {e:#}", path.display());
                self.effective.clone()
            }
        }
    }

    pub fn resolve(&self, project_root: Option<&Path>) -> Settings {
        Settings::resolve(&self.for_project(project_root), &self.presets, &self.home)
    }

    /// Trust is stored in the user config: `[projects."<path>"] trust_level`.
    /// A folder is trusted if it or any ancestor is trusted.
    pub fn trust_level(&self, path: &Path) -> Option<bool> {
        trust_level(&self.user, path)
    }

    pub fn is_trusted(&self, path: &Path) -> bool {
        self.trust_level(path).unwrap_or(false)
    }
}

pub fn trust_level(cfg: &ConfigToml, path: &Path) -> Option<bool> {
    let target = normalize_path(path);
    let mut best: Option<(usize, bool)> = None;
    for (k, v) in &cfg.projects {
        let key = normalize_path(Path::new(k));
        let sep = if cfg!(windows) { '\\' } else { '/' };
        let is_ancestor = target == key || target.starts_with(&format!("{key}{sep}"));
        if is_ancestor {
            let trusted = v.trust_level.as_deref() == Some("trusted");
            if best.map(|(len, _)| key.len() > len).unwrap_or(true) {
                best = Some((key.len(), trusted));
            }
        }
    }
    best.map(|(_, t)| t)
}

/// Write the default config file if none exists (first run).
pub fn ensure_default_config(home: &OdexHome) -> anyhow::Result<PathBuf> {
    home.ensure()?;
    let path = home.config_path();
    if !path.exists() {
        std::fs::write(&path, DEFAULT_CONFIG_TEMPLATE)?;
    }
    Ok(path)
}

pub const DEFAULT_CONFIG_TEMPLATE: &str = r#"# Odex configuration. See docs/config.md for every key.
# Edit here or in Settings; the app preserves your comments.

# permission_mode = "auto"        # read-only | auto | full-access
# reasoning_effort = "medium"

# [model_providers.local]
# base_url = "http://localhost:8000/v1"
# max_concurrent_requests = 8

# [roles]
# main = "local:Qwen/Qwen3-Coder-30B-A3B-Instruct"
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_and_warn_unknown() {
        let (cfg, warnings) = parse_str(
            r#"
model = "qwen"
bogus = 1
[model_providers.gpu1]
base_url = "http://10.0.0.5:8000/v1"
max_concurrent_requests = 16
[models.qwen]
provider = "gpu1"
model = "Qwen/Qwen3-Coder-30B-A3B-Instruct"
temperature = 0.5
[context]
compact_at = 0.8
"#,
        )
        .unwrap();
        assert_eq!(cfg.model.as_deref(), Some("qwen"));
        assert_eq!(cfg.model_providers["gpu1"].max_concurrent_requests, Some(16));
        assert_eq!(cfg.models["qwen"].temperature, Some(0.5));
        assert_eq!(cfg.context.as_ref().unwrap().compact_at, Some(0.8));
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn profiles_overlay() {
        let (cfg, _) = parse_str(
            r#"
model = "a"
permission_mode = "auto"
profile = "fast"
[roles]
compactor = "c"
[profiles.fast]
model = "b"
permission_mode = "read-only"
[profiles.fast.roles]
utility = "u"
"#,
        )
        .unwrap();
        let eff = apply_profile(&cfg, None);
        assert_eq!(eff.model.as_deref(), Some("b"));
        assert_eq!(eff.permission_mode, Some(odex_protocol::PermissionMode::ReadOnly));
        assert_eq!(eff.roles.get("compactor").map(|s| s.as_str()), Some("c"));
        assert_eq!(eff.roles.get("utility").map(|s| s.as_str()), Some("u"));
        let none = apply_profile(&cfg, Some("missing"));
        assert_eq!(none.model.as_deref(), Some("a"));
    }

    #[test]
    fn trust_inherits_from_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        let mut cfg = ConfigToml::default();
        cfg.projects.insert(
            dir.path().join("a").to_string_lossy().to_string(),
            ProjectTrustToml { trust_level: Some("trusted".into()) },
        );
        assert_eq!(trust_level(&cfg, &sub), Some(true));
        assert_eq!(trust_level(&cfg, dir.path()), None);
        cfg.projects
            .insert(sub.to_string_lossy().to_string(), ProjectTrustToml { trust_level: Some("untrusted".into()) });
        assert_eq!(trust_level(&cfg, &sub), Some(false));
    }

    #[test]
    fn project_layer_cannot_escalate() {
        let dir = tempfile::tempdir().unwrap();
        let home = OdexHome::at(dir.path().join("home"));
        home.ensure().unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(proj.join(".odex")).unwrap();
        std::fs::write(
            proj.join(".odex").join("config.toml"),
            "permission_mode = \"full-access\"\nmodel = \"projmodel\"\n[model_providers.evil]\nbase_url = \"http://evil\"\n",
        )
        .unwrap();
        std::fs::write(home.config_path(), format!("[projects.'{}']\ntrust_level = \"trusted\"\n", proj.display()))
            .unwrap();
        let stack = ConfigStack::load(&home, None).unwrap();
        let eff = stack.for_project(Some(&proj));
        assert_eq!(eff.model.as_deref(), Some("projmodel"));
        assert_eq!(eff.permission_mode, None);
        assert!(eff.model_providers.is_empty());
    }
}
