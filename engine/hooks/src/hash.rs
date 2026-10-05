//! Hook ids and definition hashes.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{event_name, HookEvent};

/// Length (hex chars) of a hook id.
const ID_LEN: usize = 16;

fn update_field(hasher: &mut Sha256, field: &[u8]) {
    hasher.update((field.len() as u64).to_le_bytes());
    hasher.update(field);
}

fn update_optional(hasher: &mut Sha256, field: Option<&str>) {
    match field {
        Some(value) => {
            hasher.update([1u8]);
            update_field(hasher, value.as_bytes());
        }
        None => hasher.update([0u8]),
    }
}

/// Short, stable id of a hook: sha256 over (event, source, command, matcher).
pub fn hook_id(event: HookEvent, source: &str, command: &str, matcher: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    update_field(&mut hasher, b"odex-hook-id/v1");
    update_field(&mut hasher, event_name(event).as_bytes());
    update_field(&mut hasher, source.as_bytes());
    update_field(&mut hasher, command.as_bytes());
    update_optional(&mut hasher, matcher);
    let mut id = hex::encode(hasher.finalize());
    id.truncate(ID_LEN);
    id
}

/// Split a command line into tokens: whitespace-separated, with `'...'` and
/// `"..."` grouping (no escape processing, so Windows paths survive).
fn tokens(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                in_token = true;
            }
            None if c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')') => {
                if in_token {
                    out.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            None => {
                current.push(c);
                in_token = true;
            }
        }
    }
    if in_token {
        out.push(current);
    }
    out
}

/// Existing files that tokens of `command` refer to (absolute, or relative
/// to `cwd`), in order of appearance, without duplicates.
pub fn referenced_files(command: &str, cwd: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    for token in tokens(command) {
        if token.is_empty() || token.starts_with('-') {
            continue;
        }
        let path = Path::new(&token);
        let candidate = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
        if candidate.is_file() && !files.contains(&candidate) {
            files.push(candidate);
        }
    }
    files
}

/// Definition hash: sha256 over (event, command, matcher) and the contents
/// of every file the command refers to. Unreadable files contribute a marker
/// so the hash still changes when a file appears or disappears.
pub fn definition_hash(event: HookEvent, command: &str, matcher: Option<&str>, cwd: &Path) -> String {
    let mut hasher = Sha256::new();
    update_field(&mut hasher, b"odex-hook-hash/v1");
    update_field(&mut hasher, event_name(event).as_bytes());
    update_field(&mut hasher, command.as_bytes());
    update_optional(&mut hasher, matcher);
    for file in referenced_files(command, cwd) {
        let shown = file.strip_prefix(cwd).unwrap_or(&file).to_string_lossy().replace('\\', "/");
        update_field(&mut hasher, shown.as_bytes());
        match hash_file(&file) {
            Some(digest) => {
                hasher.update([1u8]);
                hasher.update(digest);
            }
            None => hasher.update([0u8]),
        }
    }
    hex::encode(hasher.finalize())
}

fn hash_file(path: &Path) -> Option<[u8; 32]> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = reader.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_handles_quotes_and_operators() {
        assert_eq!(
            tokens("node \"scripts/my hook.js\" --x 'a b'; & .\\h.ps1|x"),
            vec!["node", "scripts/my hook.js", "--x", "a b", ".\\h.ps1", "x"]
        );
        assert_eq!(tokens(""), Vec::<String>::new());
        assert_eq!(tokens("a ''"), vec!["a", ""]);
    }

    #[test]
    fn ids_depend_on_identity_fields_only() {
        let a = hook_id(HookEvent::PreToolUse, "user", "x", None);
        assert_eq!(a.len(), ID_LEN);
        assert_eq!(a, hook_id(HookEvent::PreToolUse, "user", "x", None));
        assert_ne!(a, hook_id(HookEvent::PostToolUse, "user", "x", None));
        assert_ne!(a, hook_id(HookEvent::PreToolUse, "project:/r", "x", None));
        assert_ne!(a, hook_id(HookEvent::PreToolUse, "user", "y", None));
        assert_ne!(a, hook_id(HookEvent::PreToolUse, "user", "x", Some("")));
    }

    #[test]
    fn hash_covers_script_contents() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let command = "sh hooks/check.sh --strict";
        let without = definition_hash(HookEvent::Stop, command, None, cwd);
        std::fs::create_dir_all(cwd.join("hooks")).unwrap();
        std::fs::write(cwd.join("hooks/check.sh"), "echo one").unwrap();
        let first = definition_hash(HookEvent::Stop, command, None, cwd);
        assert_ne!(without, first);
        assert_eq!(first, definition_hash(HookEvent::Stop, command, None, cwd));
        std::fs::write(cwd.join("hooks/check.sh"), "echo two").unwrap();
        let second = definition_hash(HookEvent::Stop, command, None, cwd);
        assert_ne!(first, second);
        assert_ne!(second, definition_hash(HookEvent::Stop, command, Some("x"), cwd));
        assert_eq!(referenced_files(command, cwd), vec![cwd.join("hooks/check.sh")]);
        // Absolute paths are followed too.
        let abs = format!("bash \"{}\"", cwd.join("hooks/check.sh").display());
        assert_eq!(referenced_files(&abs, Path::new("/nonexistent")).len(), 1);
    }
}
