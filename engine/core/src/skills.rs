//! Skills (`~/.odex/skills/<name>/SKILL.md`, `.odex/skills/`, plugins) and
//! the memories block injected into the system prompt.

use std::path::{Path, PathBuf};

use odex_config::Settings;
use odex_protocol::*;

use crate::engine::Engine;

/// Parse `---\nname: x\ndescription: y\n---\nbody`.
pub fn parse_skill_md(text: &str) -> (Option<String>, Option<String>, String) {
    let t = text.trim_start_matches('\u{feff}');
    if let Some(rest) = t.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let fm = &rest[..end];
            let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();
            let mut name = None;
            let mut desc = None;
            for line in fm.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                    match k.trim() {
                        "name" => name = Some(v),
                        "description" => desc = Some(v),
                        _ => {}
                    }
                }
            }
            return (name, desc, body);
        }
    }
    (None, None, t.to_string())
}

fn scan(dir: &Path, scope: SkillScope, plugin: Option<&str>, disabled: &[String], out: &mut Vec<SkillInfo>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let md = p.join("SKILL.md");
        if !md.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&md) else { continue };
        let (name, desc, _) = parse_skill_md(&text);
        let name = name.unwrap_or_else(|| e.file_name().to_string_lossy().to_string());
        if out.iter().any(|s| s.name == name) {
            continue; // earlier scope wins (project over user)
        }
        out.push(SkillInfo {
            enabled: !disabled.contains(&name),
            name,
            description: desc.unwrap_or_default(),
            path: md.to_string_lossy().to_string(),
            scope,
            plugin: plugin.map(String::from),
        });
    }
}

pub fn user_skills_dir(engine: &Engine) -> PathBuf {
    engine.home.skills_dir()
}

/// Skills visible from a project root: project (trusted only), user, plugins.
pub fn list(engine: &Engine, root: Option<&Path>, s: &Settings) -> Vec<SkillInfo> {
    let mut out = Vec::new();
    if let Some(r) = root {
        if engine.is_trusted(r) {
            scan(&odex_config::project_dir(r).join("skills"), SkillScope::Project, None, &s.disabled_skills, &mut out);
        }
    }
    scan(&engine.home.skills_dir(), SkillScope::User, None, &s.disabled_skills, &mut out);
    for (plugin_id, dir) in crate::plugins::skill_dirs(engine) {
        scan(&dir, SkillScope::Plugin, Some(&plugin_id), &s.disabled_skills, &mut out);
    }
    out
}

pub fn find(engine: &Engine, root: Option<&Path>, s: &Settings, name: &str) -> Option<(SkillInfo, String)> {
    let name = name.trim_start_matches(['$', '@']);
    let info = list(engine, root, s).into_iter().find(|k| k.name.eq_ignore_ascii_case(name))?;
    let text = std::fs::read_to_string(&info.path).ok()?;
    let (_, _, body) = parse_skill_md(&text);
    Some((info, body))
}

pub fn write_skill(dir: &Path, name: &str, description: &str, body: &str) -> std::io::Result<PathBuf> {
    let safe: String =
        name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let d = dir.join(&safe);
    std::fs::create_dir_all(&d)?;
    let p = d.join("SKILL.md");
    std::fs::write(
        &p,
        format!("---\nname: {name}\ndescription: {}\n---\n\n{}\n", description.replace('\n', " "), body.trim()),
    )?;
    Ok(p)
}

/// Approved memories within the token budget (empty unless enabled for the thread).
pub fn memories_block(engine: &Engine, t: &Thread, s: &Settings) -> String {
    if !(s.memories_enabled && t.memories_enabled) {
        return String::new();
    }
    let store = odex_memories::MemoryStore::new(engine.home.memories_dir());
    let root = engine.thread_root(t);
    let est = odex_llm::tokens::TokenEstimator::default();
    store
        .injection_block(Some(&root), s.memories_max_tokens.min(s.context.memories_max_tokens) as usize, &|x: &str| {
            est.text(x) as usize
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter() {
        let (n, d, b) = parse_skill_md("---\nname: deploy\ndescription: \"Ship it\"\n---\n\nRun the script.\n");
        assert_eq!(n.as_deref(), Some("deploy"));
        assert_eq!(d.as_deref(), Some("Ship it"));
        assert_eq!(b.trim(), "Run the script.");
        let (n, _, b) = parse_skill_md("just text");
        assert!(n.is_none());
        assert_eq!(b, "just text");
    }
}
