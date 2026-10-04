//! Gitignore-aware glob search.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use globset::GlobBuilder;

use crate::walk::{is_file, relative_slash_path, walker};
use crate::Error;

/// Find files under `root` matching `pattern`, newest first (`limit` 0 = unlimited).
///
/// Patterns containing `/` match the path relative to `root` (`src/**/*.rs`); patterns without a
/// slash match the file name at any depth (`*.rs`), like gitignore/ripgrep globs. Hidden files are
/// included, `.git` and gitignored files are not. Returned paths are `root` joined with the
/// relative path.
pub fn glob(root: &Path, pattern: &str, limit: usize) -> Result<Vec<PathBuf>, Error> {
    if !root.exists() {
        return Err(Error::NotFound(root.to_path_buf()));
    }
    let normalized = pattern.trim().replace('\\', "/");
    let normalized = normalized.trim_start_matches("./").trim_start_matches('/');
    if normalized.is_empty() {
        return Err(Error::InvalidPattern("glob pattern is empty".into()));
    }
    let match_full_path = normalized.contains('/');
    let matcher = GlobBuilder::new(normalized)
        .literal_separator(true)
        .backslash_escape(false)
        .build()
        .map_err(|e| Error::InvalidPattern(e.to_string()))?
        .compile_matcher();

    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in walker(root, true).build().flatten() {
        if !is_file(&entry) {
            continue;
        }
        let rel = relative_slash_path(root, entry.path());
        let subject = if match_full_path { rel.as_str() } else { rel.rsplit('/').next().unwrap_or(&rel) };
        if matcher.is_match(subject) {
            let mtime = entry.metadata().ok().and_then(|m| m.modified().ok()).unwrap_or(SystemTime::UNIX_EPOCH);
            found.push((mtime, entry.into_path()));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    if limit > 0 {
        found.truncate(limit);
    }
    Ok(found.into_iter().map(|(_, p)| p).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn set_mtime(path: &Path, secs_ago: u64) {
        let f = fs::File::options().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(secs_ago)).unwrap();
    }

    #[test]
    fn matches_sorted_by_mtime_and_respects_ignores() {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("src/nested")).unwrap();
        fs::create_dir_all(r.join("build")).unwrap();
        for (p, age) in [("a.rs", 300), ("src/b.rs", 100), ("src/nested/c.rs", 200), ("build/gen.rs", 1)] {
            fs::write(r.join(p), "x").unwrap();
            set_mtime(&r.join(p), age);
        }
        fs::write(r.join("readme.md"), "x").unwrap();
        fs::write(r.join(".gitignore"), "build/\n").unwrap();

        let names = |v: Vec<PathBuf>| -> Vec<String> { v.iter().map(|p| relative_slash_path(r, p)).collect() };

        assert_eq!(names(glob(r, "*.rs", 0).unwrap()), ["src/b.rs", "src/nested/c.rs", "a.rs"]);
        assert_eq!(names(glob(r, "src/**/*.rs", 0).unwrap()), ["src/b.rs", "src/nested/c.rs"]);
        assert_eq!(names(glob(r, "src/*.rs", 0).unwrap()), ["src/b.rs"]);
        assert_eq!(names(glob(r, "**/*.rs", 2).unwrap()), ["src/b.rs", "src/nested/c.rs"]);
        assert_eq!(names(glob(r, "*.{md,rs}", 1).unwrap()), ["readme.md"]);
        assert!(glob(r, "*.py", 0).unwrap().is_empty());
        assert!(matches!(glob(r, "[", 0), Err(Error::InvalidPattern(_))));
        assert!(matches!(glob(&r.join("nope"), "*", 0), Err(Error::NotFound(_))));
    }
}
