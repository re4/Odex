//! Shared walker configuration.

use std::path::Path;

use ignore::WalkBuilder;

/// A gitignore-aware walker rooted at `root`.
///
/// `.gitignore`, `.ignore`, `.git/info/exclude` and the global excludes file are honoured even
/// outside git repositories (users expect a `.gitignore` to apply to plain folders too). The
/// `.git` directory itself is always skipped; other hidden entries are included unless
/// `include_hidden` is false.
pub(crate) fn walker(root: &Path, include_hidden: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!include_hidden)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    builder
}

/// Whether a walked entry is a regular file (symlinks are resolved to decide).
pub(crate) fn is_file(entry: &ignore::DirEntry) -> bool {
    match entry.file_type() {
        Some(ft) if ft.is_file() => true,
        Some(ft) if ft.is_symlink() => entry.path().metadata().map(|m| m.is_file()).unwrap_or(false),
        _ => false,
    }
}

/// `path` relative to `root`, with forward slashes. Falls back to the file name when `root` is
/// the file itself.
pub(crate) fn relative_slash_path(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let rel = if rel.as_os_str().is_empty() { path.file_name().map(Path::new).unwrap_or(rel) } else { rel };
    let mut out = String::new();
    for (i, comp) in rel.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&comp.as_os_str().to_string_lossy());
    }
    out
}
