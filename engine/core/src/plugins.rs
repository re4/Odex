//! Local plugins: bundles of skills, MCP servers, hooks and project actions,
//! installed from a folder or a git URL after a trust review. No marketplace.
//!
//! Layout: `~/.odex/plugins/<id>/odex-plugin.toml` + files;
//! registry: `~/.odex/plugins/plugins.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use odex_protocol::config_types::{HooksToml, McpServerToml};
use odex_protocol::*;

use crate::engine::Engine;

pub const MANIFEST: &str = "odex-plugin.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Entry {
    source: String,
    enabled: bool,
    trusted_hash: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    plugins: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ManifestToml {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    mcp_servers: BTreeMap<String, McpServerToml>,
    #[serde(default)]
    hooks: Option<HooksToml>,
    #[serde(default)]
    actions: Vec<ProjectAction>,
}

fn dir(engine: &Engine) -> PathBuf {
    engine.home.plugins_dir()
}

fn load_registry(engine: &Engine) -> Registry {
    std::fs::read_to_string(dir(engine).join("plugins.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_registry(engine: &Engine, r: &Registry) -> std::io::Result<()> {
    std::fs::create_dir_all(dir(engine))?;
    std::fs::write(dir(engine).join("plugins.json"), serde_json::to_string_pretty(r)?)
}

fn read_manifest(p: &Path) -> anyhow::Result<PluginManifest> {
    let text =
        std::fs::read_to_string(p.join(MANIFEST)).map_err(|_| anyhow::anyhow!("{} has no {MANIFEST}", p.display()))?;
    let m: ManifestToml = toml::from_str(&text)?;
    if m.name.trim().is_empty() {
        anyhow::bail!("plugin manifest has no name");
    }
    Ok(PluginManifest {
        name: m.name,
        version: m.version,
        description: m.description,
        author: m.author,
        skills: m.skills,
        mcp_servers: m.mcp_servers,
        hooks: m.hooks,
        actions: m.actions,
    })
}

/// Content hash over every file (sorted relative paths + bytes).
pub fn content_hash(root: &Path) -> String {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().map(|n| n == ".git").unwrap_or(false) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut h = Sha256::new();
    for f in files {
        h.update(f.strip_prefix(root).unwrap_or(&f).to_string_lossy().replace('\\', "/").as_bytes());
        h.update([0]);
        if let Ok(b) = std::fs::read(&f) {
            h.update(&b);
        }
        h.update([0]);
    }
    hex::encode(&h.finalize()[..16])
}

fn info(engine: &Engine, id: &str, e: &Entry) -> Option<PluginInfo> {
    let path = dir(engine).join(id);
    let manifest = read_manifest(&path).ok()?;
    let hash = content_hash(&path);
    Some(PluginInfo {
        id: id.to_string(),
        manifest,
        path: path.to_string_lossy().to_string(),
        source: e.source.clone(),
        enabled: e.enabled,
        trusted: e.trusted_hash.as_deref() == Some(hash.as_str()),
        hash,
    })
}

pub fn list(engine: &Engine) -> Vec<PluginInfo> {
    let r = load_registry(engine);
    r.plugins.iter().filter_map(|(id, e)| info(engine, id, e)).collect()
}

fn active(engine: &Engine) -> Vec<PluginInfo> {
    list(engine).into_iter().filter(|p| p.enabled && p.trusted).collect()
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let p = e.path();
        if p.file_name().map(|n| n == ".git" || n == "node_modules").unwrap_or(false) {
            continue;
        }
        let dest = to.join(e.file_name());
        if p.is_dir() {
            copy_dir(&p, &dest)?;
        } else {
            std::fs::copy(&p, &dest)?;
        }
    }
    Ok(())
}

/// Install (copy or clone) a plugin. It starts untrusted and disabled until
/// the user reviews it (`plugins/trust`).
pub async fn install(engine: &Engine, source: &str) -> anyhow::Result<PluginInfo> {
    let staging = engine.home.tmp_dir().join(format!("plugin-{}", uuid::Uuid::new_v4().simple()));
    let is_git = source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("git@")
        || source.ends_with(".git");
    if is_git {
        let out =
            tokio::process::Command::new("git").args(["clone", "--depth", "1", source]).arg(&staging).output().await?;
        if !out.status.success() {
            anyhow::bail!("git clone failed: {}", String::from_utf8_lossy(&out.stderr));
        }
    } else {
        let src = PathBuf::from(source);
        if !src.join(MANIFEST).is_file() {
            anyhow::bail!("{source} has no {MANIFEST}");
        }
        copy_dir(&src, &staging)?;
    }
    let manifest = read_manifest(&staging)?;
    let id: String = manifest
        .name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let dest = dir(engine).join(&id);
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(dir(engine))?;
    if std::fs::rename(&staging, &dest).is_err() {
        copy_dir(&staging, &dest)?;
        let _ = std::fs::remove_dir_all(&staging);
    }
    let mut r = load_registry(engine);
    r.plugins.insert(id.clone(), Entry { source: source.to_string(), enabled: false, trusted_hash: None });
    save_registry(engine, &r)?;
    info(engine, &id, &r.plugins[&id]).ok_or_else(|| anyhow::anyhow!("installed plugin is unreadable"))
}

/// Trust (and enable) a plugin at a reviewed hash.
pub fn trust(engine: &Engine, id: &str, hash: &str, trusted: bool) -> anyhow::Result<PluginInfo> {
    let mut r = load_registry(engine);
    let e = r.plugins.get_mut(id).ok_or_else(|| anyhow::anyhow!("plugin {id} not installed"))?;
    let current = content_hash(&dir(engine).join(id));
    if trusted {
        if current != hash {
            anyhow::bail!("plugin files changed since review; review again");
        }
        e.trusted_hash = Some(current);
        e.enabled = true;
    } else {
        e.trusted_hash = None;
        e.enabled = false;
    }
    let e = e.clone();
    save_registry(engine, &r)?;
    info(engine, id, &e).ok_or_else(|| anyhow::anyhow!("plugin unreadable"))
}

pub fn set_enabled(engine: &Engine, id: &str, enabled: bool) -> anyhow::Result<()> {
    let mut r = load_registry(engine);
    let e = r.plugins.get_mut(id).ok_or_else(|| anyhow::anyhow!("plugin {id} not installed"))?;
    e.enabled = enabled;
    save_registry(engine, &r)?;
    Ok(())
}

pub fn remove(engine: &Engine, id: &str) -> anyhow::Result<()> {
    let mut r = load_registry(engine);
    r.plugins.remove(id);
    save_registry(engine, &r)?;
    let p = dir(engine).join(id);
    if p.exists() {
        std::fs::remove_dir_all(p)?;
    }
    Ok(())
}

pub fn skill_dirs(engine: &Engine) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for p in active(engine) {
        let root = PathBuf::from(&p.path);
        let dirs = if p.manifest.skills.is_empty() { vec!["skills".to_string()] } else { p.manifest.skills.clone() };
        for d in dirs {
            out.push((p.id.clone(), root.join(d)));
        }
    }
    out
}

pub fn mcp_servers(engine: &Engine) -> Vec<(String, McpServerToml)> {
    let mut out = Vec::new();
    for p in active(engine) {
        for (name, mut cfg) in p.manifest.mcp_servers.clone() {
            if cfg.cwd.is_none() {
                cfg.cwd = Some(p.path.clone());
            }
            out.push((format!("{}-{}", p.id, name), cfg));
        }
    }
    out
}

pub fn hooks(engine: &Engine) -> Vec<(String, PathBuf, HooksToml)> {
    active(engine)
        .into_iter()
        .filter_map(|p| p.manifest.hooks.clone().map(|h| (p.id.clone(), PathBuf::from(&p.path), h)))
        .collect()
}

pub fn actions(engine: &Engine) -> Vec<ProjectAction> {
    active(engine).into_iter().flat_map(|p| p.manifest.actions.clone()).collect()
}
