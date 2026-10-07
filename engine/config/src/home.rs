//! Layout of the user config directory (`~/.odex/`).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OdexHome {
    root: PathBuf,
}

impl OdexHome {
    /// `$ODEX_HOME`, else `~/.odex`.
    pub fn resolve() -> Self {
        if let Ok(p) = std::env::var("ODEX_HOME") {
            if !p.trim().is_empty() {
                return Self::at(PathBuf::from(p));
            }
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self::at(home.join(".odex"))
    }

    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Create the directory skeleton (idempotent).
    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [
            self.root.clone(),
            self.sessions_dir(),
            self.outputs_dir(),
            self.media_dir(),
            self.skills_dir(),
            self.plugins_dir(),
            self.rules_dir(),
            self.memories_dir(),
            self.logs_dir(),
            self.tmp_dir(),
        ] {
            std::fs::create_dir_all(&d)?;
        }
        Ok(())
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
    /// Full tool outputs, addressed by `ref:` ids.
    pub fn outputs_dir(&self) -> PathBuf {
        self.root.join("outputs")
    }
    /// Screenshots and attachments.
    pub fn media_dir(&self) -> PathBuf {
        self.root.join("media")
    }
    pub fn db_path(&self) -> PathBuf {
        self.root.join("odex.sqlite")
    }
    pub fn default_worktrees_dir(&self) -> PathBuf {
        self.root.join("worktrees")
    }
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }
    pub fn plugins_dir(&self) -> PathBuf {
        self.root.join("plugins")
    }
    pub fn rules_dir(&self) -> PathBuf {
        self.root.join("rules")
    }
    pub fn memories_dir(&self) -> PathBuf {
        self.root.join("memories")
    }
    /// Default home of ComfyUI workflow files.
    pub fn comfyui_dir(&self) -> PathBuf {
        self.root.join("comfyui")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }
    pub fn models_cache_path(&self) -> PathBuf {
        self.root.join("models_cache.json")
    }
    pub fn trusted_hooks_path(&self) -> PathBuf {
        self.root.join("trusted_hooks.json")
    }
    pub fn global_agents_md(&self) -> PathBuf {
        self.root.join("AGENTS.md")
    }
    pub fn user_presets_path(&self) -> PathBuf {
        self.root.join("presets.toml")
    }
    pub fn mcp_tokens_path(&self) -> PathBuf {
        self.root.join("mcp_tokens.json")
    }
}

/// Per-project directory name.
pub const PROJECT_DIR: &str = ".odex";

pub fn project_dir(root: &Path) -> PathBuf {
    root.join(PROJECT_DIR)
}

/// Normalize a path for comparisons and map keys: absolute, no `\\?\`
/// prefix, forward slashes collapsed, case-folded on Windows.
pub fn normalize_path(p: &Path) -> String {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|c| c.join(p)).unwrap_or_else(|_| p.to_path_buf())
    };
    let canon = dunce_like(&abs);
    let mut s = canon.to_string_lossy().to_string();
    if cfg!(windows) {
        s = s.replace('/', "\\");
        while s.len() > 3 && s.ends_with('\\') {
            s.pop();
        }
        s = s.to_lowercase();
    } else {
        while s.len() > 1 && s.ends_with('/') {
            s.pop();
        }
    }
    s
}

/// Canonicalize if possible and strip the Windows verbatim prefix.
pub fn dunce_like(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_string());
    }
    c
}
