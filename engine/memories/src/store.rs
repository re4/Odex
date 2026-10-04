//! JSON-file persistence for memories.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use odex_protocol::Memory;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{normalize_category, now_ms, one_line, redact_secrets};

/// Heading of the block injected into the system prompt.
pub const INJECTION_HEADING: &str = "## Memories (user-approved facts; may be outdated)";

const GLOBAL_JSON: &str = "global.json";
const GLOBAL_MD: &str = "MEMORIES.md";
const PROJECTS_DIR: &str = "projects";

#[derive(Debug, Default, Serialize)]
struct MemoryFile {
    project_path: Option<String>,
    memories: Vec<Memory>,
}

/// On-disk shape, read leniently: entries that fail to decode are dropped.
#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    project_path: Option<String>,
    #[serde(default)]
    memories: Vec<Value>,
}

/// Memories stored as JSON files under one directory.
pub struct MemoryStore {
    dir: PathBuf,
    write_lock: Mutex<()>,
}

impl MemoryStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, write_lock: Mutex::new(()) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of the JSON file holding a project's memories.
    pub fn project_file(&self, project_path: &Path) -> PathBuf {
        self.dir.join(PROJECTS_DIR).join(format!("{}.json", project_file_stem(project_path)))
    }

    /// Path of the global JSON file.
    pub fn global_file(&self) -> PathBuf {
        self.dir.join(GLOBAL_JSON)
    }

    /// The human-readable Markdown file regenerated next to a JSON file.
    pub fn markdown_file_for(&self, json_path: &Path) -> PathBuf {
        if json_path == self.global_file() {
            self.dir.join(GLOBAL_MD)
        } else {
            json_path.with_extension("MEMORIES.md")
        }
    }

    /// Global memories plus the given project's, newest first, optionally filtered by status.
    pub fn list(&self, project_path: Option<&Path>, status: Option<&str>) -> Vec<Memory> {
        let mut out = read_lenient(&self.global_file()).memories;
        if let Some(p) = project_path {
            out.extend(read_lenient(&self.project_file(p)).memories);
        }
        if let Some(status) = status {
            out.retain(|m| m.status.eq_ignore_ascii_case(status));
        }
        sort_newest_first(&mut out);
        out
    }

    /// Every memory in every file, newest first.
    pub fn all(&self) -> Vec<Memory> {
        let mut out = Vec::new();
        for path in self.files() {
            out.extend(read_lenient(&path).memories);
        }
        sort_newest_first(&mut out);
        out
    }

    /// Insert or update a memory. Assigns `id` and timestamps, normalizes
    /// scope/status/category and redacts secrets from the text. A memory whose
    /// scope or project changed is moved to the right file.
    pub fn upsert(&self, mut m: Memory) -> io::Result<Memory> {
        let _guard = self.lock();
        m.text = one_line(&redact_secrets(m.text.trim()));
        if m.text.is_empty() {
            return Err(invalid("memory text is empty"));
        }
        let project = m.project_path.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(PathBuf::from);
        let project = match (m.scope.trim().to_ascii_lowercase().as_str(), project) {
            ("global", _) => None,
            ("project", None) => return Err(invalid("a project-scoped memory needs project_path")),
            (_, project) => project,
        };
        m.scope = if project.is_some() { "project" } else { "global" }.to_string();
        m.project_path = project.as_deref().map(display_path);
        m.status = if m.status.trim().eq_ignore_ascii_case("approved") { "approved" } else { "proposed" }.to_string();
        m.category = normalize_category(&m.category).to_string();

        let now = now_ms();
        let existing = if m.id.is_empty() { None } else { self.find(&m.id) };
        if m.id.is_empty() {
            m.id = uuid::Uuid::new_v4().to_string();
        }
        m.created_at = match &existing {
            Some((_, old)) => old.created_at,
            None if m.created_at > 0 => m.created_at,
            None => now,
        };
        m.updated_at = now;

        let target = match &project {
            Some(p) => self.project_file(p),
            None => self.global_file(),
        };
        if let Some((old_path, _)) = &existing {
            if *old_path != target {
                let mut old = self.read_for_write(old_path)?;
                old.memories.retain(|x| x.id != m.id);
                self.write(old_path, &old)?;
            }
        }
        let mut file = self.read_for_write(&target)?;
        file.project_path = m.project_path.clone();
        match file.memories.iter_mut().find(|x| x.id == m.id) {
            Some(slot) => *slot = m.clone(),
            None => file.memories.push(m.clone()),
        }
        self.write(&target, &file)?;
        Ok(m)
    }

    /// Delete by id. Returns whether anything was removed.
    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let _guard = self.lock();
        let Some((path, _)) = self.find(id) else {
            return Ok(false);
        };
        let mut file = self.read_for_write(&path)?;
        let before = file.memories.len();
        file.memories.retain(|m| m.id != id);
        let removed = file.memories.len() != before;
        self.write(&path, &file)?;
        Ok(removed)
    }

    /// Mark a proposed memory approved.
    pub fn approve(&self, id: &str) -> io::Result<Option<Memory>> {
        let _guard = self.lock();
        let Some((path, _)) = self.find(id) else {
            return Ok(None);
        };
        let mut file = self.read_for_write(&path)?;
        let Some(m) = file.memories.iter_mut().find(|m| m.id == id) else {
            return Ok(None);
        };
        m.status = "approved".to_string();
        m.updated_at = now_ms();
        let approved = m.clone();
        self.write(&path, &file)?;
        Ok(Some(approved))
    }

    /// Prompt block of approved memories: project-specific first, then
    /// global, newest first within each group, cut off before `max_tokens`
    /// (as measured by `count_tokens`) would be exceeded. `None` when there is
    /// nothing to inject.
    pub fn injection_block(
        &self,
        project_path: Option<&Path>,
        max_tokens: usize,
        count_tokens: &dyn Fn(&str) -> usize,
    ) -> Option<String> {
        let approved = |path: &Path| {
            let mut v: Vec<Memory> =
                read_lenient(path).memories.into_iter().filter(|m| m.status == "approved").collect();
            v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.created_at.cmp(&a.created_at)));
            v
        };
        let mut ordered = Vec::new();
        if let Some(p) = project_path {
            ordered.extend(approved(&self.project_file(p)));
        }
        ordered.extend(approved(&self.global_file()));

        let mut block = INJECTION_HEADING.to_string();
        let mut used = count_tokens(&block);
        let mut added = 0usize;
        for m in ordered {
            let line = format!("\n- {}", one_line(&m.text));
            let cost = count_tokens(&line);
            if used + cost > max_tokens {
                break;
            }
            used += cost;
            block.push_str(&line);
            added += 1;
        }
        (added > 0).then_some(block)
    }

    // -- internals ----------------------------------------------------------

    fn lock(&self) -> MutexGuard<'_, ()> {
        self.write_lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut files = vec![self.global_file()];
        if let Ok(entries) = fs::read_dir(self.dir.join(PROJECTS_DIR)) {
            let mut projects: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect();
            projects.sort();
            files.extend(projects);
        }
        files
    }

    fn find(&self, id: &str) -> Option<(PathBuf, Memory)> {
        self.files().into_iter().find_map(|path| {
            let m = read_lenient(&path).memories.into_iter().find(|m| m.id == id)?;
            Some((path, m))
        })
    }

    /// Read a file we are about to rewrite. A corrupt file is moved aside
    /// (`*.corrupt-<ms>`) rather than silently overwritten.
    fn read_for_write(&self, path: &Path) -> io::Result<MemoryFile> {
        match read_file(path) {
            Ok(file) => Ok(file.unwrap_or_default()),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let backup = path.with_extension(format!("json.corrupt-{}", now_ms()));
                fs::rename(path, &backup)?;
                Ok(MemoryFile::default())
            }
            Err(e) => Err(e),
        }
    }

    fn write(&self, path: &Path, file: &MemoryFile) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(file).map_err(io::Error::other)?;
        write_atomic(path, json.as_bytes())?;
        let title = match &file.project_path {
            Some(p) if *path != self.global_file() => p.clone(),
            _ => "Global".to_string(),
        };
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        write_atomic(&self.markdown_file_for(path), render_markdown(&title, &file_name, &file.memories).as_bytes())
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

fn sort_newest_first(v: &mut [Memory]) {
    v.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
}

fn read_file(path: &Path) -> io::Result<Option<MemoryFile>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let raw: RawFile =
        serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let memories = raw.memories.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
    Ok(Some(MemoryFile { project_path: raw.project_path, memories }))
}

fn read_lenient(path: &Path) -> MemoryFile {
    read_file(path).ok().flatten().unwrap_or_default()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

fn render_markdown(title: &str, source_file: &str, memories: &[Memory]) -> String {
    let mut sorted: Vec<&Memory> = memories.iter().collect();
    sorted.sort_by_key(|m| std::cmp::Reverse(m.created_at));
    let mut md = format!(
        "# Memories: {title}\n\n<!-- Generated by Odex from {source_file}. Manage memories in Odex; edits to this file \
         are overwritten. -->\n"
    );
    if sorted.is_empty() {
        md.push_str("\n_No memories yet._\n");
        return md;
    }
    for (heading, status) in [("Approved", "approved"), ("Proposed (awaiting review)", "proposed")] {
        let group: Vec<&&Memory> = sorted.iter().filter(|m| m.status == status).collect();
        if group.is_empty() {
            continue;
        }
        md.push_str(&format!("\n## {heading}\n\n"));
        for m in group {
            md.push_str(&format!("- **{}**: {}\n", m.category, one_line(&m.text)));
        }
    }
    md
}

/// Absolute, lexically cleaned path (no `.`/`..`), without the Windows
/// verbatim prefix. Keeps the original case.
fn clean_absolute(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    let s = out.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => out,
    }
}

fn display_path(p: &Path) -> String {
    clean_absolute(p).to_string_lossy().into_owned()
}

/// Normalized identity of a project path: absolute, `/` separators, no
/// trailing slash, lowercased on case-insensitive platforms.
pub fn normalize_project_path(p: &Path) -> String {
    let mut s = clean_absolute(p).to_string_lossy().replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') && !s.ends_with(":/") {
        s.pop();
    }
    if cfg!(any(windows, target_os = "macos")) {
        s = s.to_lowercase();
    }
    s
}

/// `<slug>-<hash>`: slug of the folder name plus 8 hex chars of the SHA-256
/// of the normalized absolute path.
pub fn project_file_stem(p: &Path) -> String {
    let clean = clean_absolute(p);
    let name = clean.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut slug = String::new();
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let mut slug: String = slug.trim_matches('-').chars().take(40).collect();
    slug = slug.trim_end_matches('-').to_string();
    if slug.is_empty() {
        slug = "project".to_string();
    }
    let digest = Sha256::digest(normalize_project_path(p).as_bytes());
    format!("{slug}-{}", &hex::encode(digest)[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(text: &str, scope: &str, project: Option<&Path>, status: &str) -> Memory {
        Memory {
            id: String::new(),
            text: text.to_string(),
            scope: scope.to_string(),
            project_path: project.map(|p| p.to_string_lossy().into_owned()),
            status: status.to_string(),
            category: "preference".to_string(),
            source_thread_id: Some("th_1".into()),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn approx_tokens(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn crud_round_trip_and_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(tmp.path().to_path_buf());
        assert!(store.list(None, None).is_empty());

        let a = store.upsert(mem("Prefers concise answers", "global", None, "proposed")).unwrap();
        assert!(!a.id.is_empty());
        assert!(a.created_at > 0 && a.updated_at >= a.created_at);
        assert_eq!(a.project_path, None);
        assert_eq!(store.list(None, None), vec![a.clone()]);

        let mut edit = a.clone();
        edit.text = "Prefers concise answers with code first".into();
        edit.category = "Preferences".into();
        edit.created_at = 5;
        let edited = store.upsert(edit).unwrap();
        assert_eq!(edited.id, a.id);
        assert_eq!(edited.created_at, a.created_at);
        assert_eq!(edited.category, "preference");
        assert_eq!(store.all().len(), 1);

        let json = fs::read_to_string(tmp.path().join("global.json")).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert!(v["project_path"].is_null());
        assert_eq!(v["memories"][0]["text"], "Prefers concise answers with code first");
        let md = fs::read_to_string(tmp.path().join("MEMORIES.md")).unwrap();
        assert!(md.contains("# Memories: Global"));
        assert!(md.contains("## Proposed (awaiting review)"));
        assert!(md.contains("code first"));

        let approved = store.approve(&a.id).unwrap().unwrap();
        assert_eq!(approved.status, "approved");
        assert_eq!(store.list(None, Some("approved")).len(), 1);
        assert!(store.list(None, Some("proposed")).is_empty());
        assert!(fs::read_to_string(tmp.path().join("MEMORIES.md")).unwrap().contains("## Approved"));
        assert!(store.approve("missing").unwrap().is_none());

        assert!(store.delete(&a.id).unwrap());
        assert!(!store.delete(&a.id).unwrap());
        assert!(store.all().is_empty());
        assert!(fs::read_to_string(tmp.path().join("MEMORIES.md")).unwrap().contains("No memories yet"));
    }

    #[test]
    fn projects_are_separate_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(tmp.path().join("mem"));
        let p1 = tmp.path().join("work").join("My App");
        let p2 = tmp.path().join("work").join("other");

        store.upsert(mem("Global fact", "global", None, "approved")).unwrap();
        let m1 = store.upsert(mem("Uses pnpm", "project", Some(&p1), "approved")).unwrap();
        store.upsert(mem("Uses cargo nextest", "project", Some(&p2), "approved")).unwrap();
        assert_eq!(m1.scope, "project");
        assert!(Path::new(m1.project_path.as_deref().unwrap()).is_absolute());

        let in_p1: Vec<String> = store.list(Some(&p1), None).into_iter().map(|m| m.text).collect();
        assert_eq!(in_p1.len(), 2);
        assert!(in_p1.contains(&"Uses pnpm".to_string()) && in_p1.contains(&"Global fact".to_string()));
        assert_eq!(store.list(None, None).len(), 1);
        assert_eq!(store.all().len(), 3);

        let f1 = store.project_file(&p1);
        let name = f1.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("my-app-") && name.ends_with(".json"), "{name}");
        assert_ne!(f1, store.project_file(&p2));
        let raw: Value = serde_json::from_str(&fs::read_to_string(&f1).unwrap()).unwrap();
        assert_eq!(raw["memories"].as_array().unwrap().len(), 1);
        assert!(raw["project_path"].as_str().unwrap().ends_with("My App"));
        let md = fs::read_to_string(store.markdown_file_for(&f1)).unwrap();
        assert!(md.contains("Uses pnpm") && md.contains("My App"));

        // Equivalent spellings of the same path map to the same file.
        let alt = tmp.path().join("work").join(".").join("x").join("..").join("My App");
        assert_eq!(store.project_file(&alt), f1);

        // Moving a memory from project to global relocates it.
        let mut moved = m1.clone();
        moved.scope = "global".into();
        let moved = store.upsert(moved).unwrap();
        assert_eq!(moved.project_path, None);
        assert_eq!(store.list(Some(&p1), None).len(), 2);
        assert_eq!(store.list(None, None).len(), 2);
        let raw: Value = serde_json::from_str(&fs::read_to_string(&f1).unwrap()).unwrap();
        assert!(raw["memories"].as_array().unwrap().is_empty());

        // Project scope without a path is rejected.
        assert!(store.upsert(mem("orphan", "project", None, "proposed")).is_err());
        assert!(store.upsert(mem("   ", "global", None, "proposed")).is_err());
    }

    #[test]
    fn upsert_redacts_secrets() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(tmp.path().to_path_buf());
        let m = store.upsert(mem("Deploy token: abc123xyz for staging", "global", None, "proposed")).unwrap();
        assert_eq!(m.text, "Deploy token: [REDACTED] for staging");
        let on_disk = fs::read_to_string(tmp.path().join("global.json")).unwrap();
        assert!(!on_disk.contains("abc123xyz"));
    }

    #[test]
    fn injection_block_orders_and_budgets() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(tmp.path().to_path_buf());
        let proj = tmp.path().join("repo");

        let mut g_old = mem("global old fact", "global", None, "approved");
        g_old.created_at = 1;
        store.upsert(g_old).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.upsert(mem("global new fact", "global", None, "approved")).unwrap();
        store.upsert(mem("unapproved idea", "global", None, "proposed")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store.upsert(mem("project convention one", "project", Some(&proj), "approved")).unwrap();

        assert!(store.injection_block(None, 1000, &approx_tokens).unwrap().contains("global new fact"));
        let full = store.injection_block(Some(&proj), 1000, &approx_tokens).unwrap();
        assert!(full.starts_with(INJECTION_HEADING));
        let lines: Vec<&str> = full.lines().skip(1).collect();
        assert_eq!(lines, ["- project convention one", "- global new fact", "- global old fact"]);
        assert!(!full.contains("unapproved"));

        // Budget: heading (6 words) + two lines of 4 words each.
        let heading_cost = approx_tokens(INJECTION_HEADING);
        let cut = store.injection_block(Some(&proj), heading_cost + 8, &approx_tokens).unwrap();
        assert_eq!(cut.lines().count(), 3);
        assert!(!cut.contains("old fact"));
        assert!(store.injection_block(Some(&proj), heading_cost + 1, &approx_tokens).is_none());

        let empty = MemoryStore::new(tmp.path().join("none"));
        assert!(empty.injection_block(None, 1000, &approx_tokens).is_none());
    }

    #[test]
    fn corrupt_files_are_preserved() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("global.json"), "{not json").unwrap();
        let store = MemoryStore::new(tmp.path().to_path_buf());
        assert!(store.list(None, None).is_empty());
        store.upsert(mem("fresh", "global", None, "approved")).unwrap();
        assert_eq!(store.list(None, None).len(), 1);
        let backups = fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn stems_are_stable_and_slugged() {
        let stem = project_file_stem(Path::new("/srv/Weird Name!!/"));
        assert!(stem.starts_with("weird-name-"), "{stem}");
        assert_eq!(stem.len(), "weird-name-".len() + 8);
        assert_eq!(stem, project_file_stem(Path::new("/srv/Weird Name!!")));
        assert!(project_file_stem(Path::new("/")).starts_with("project-"));
    }
}
