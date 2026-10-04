//! Turning a parsed [`Patch`] into file changes: preview (pure) and apply.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

use crate::error::{ContextMismatch, PatchError};
use crate::parser::{FileOp, Patch, UpdateChunk};
use crate::seek::{closest_match, seek_header, seek_sequence};
use crate::text::{Replacement, TextFile};

/// What a patch operation does to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Add,
    Delete,
    /// In-place edit; a rename when `move_to` is set.
    Update,
}

/// The computed effect of one patch operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangePreview {
    /// Absolute, lexically normalised path the operation targets.
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Absolute destination of a move.
    pub move_to: Option<PathBuf>,
    /// Exact previous contents (including BOM / CRLF), `None` for a new file.
    pub old_contents: Option<String>,
    /// Exact contents that will be written, `None` for a deletion.
    pub new_contents: Option<String>,
    /// Unified diff (LF-normalised, no BOM) with `a/` and `b/` headers
    /// relative to the working directory. Empty when nothing changes.
    pub unified_diff: String,
    pub additions: u32,
    pub deletions: u32,
}

/// Compute every change the patch would make, without touching the disk.
///
/// Paths are resolved against `cwd` unless absolute. Operations are applied
/// in order against an in-memory view, so later operations see the effects
/// of earlier ones on the same file.
pub fn preview(patch: &Patch, cwd: &Path) -> Result<Vec<FileChangePreview>, PatchError> {
    Ok(plan(patch, cwd)?.previews)
}

/// Apply the patch. Everything is computed first; if any operation fails
/// nothing is written. New contents are staged in temporary sibling files
/// and then renamed into place, parent directories are created as needed,
/// and a move writes the destination before removing the source.
pub fn apply(patch: &Patch, cwd: &Path) -> Result<Vec<FileChangePreview>, PatchError> {
    let plan = plan(patch, cwd)?;
    commit(&plan.changes)?;
    Ok(plan.previews)
}

/// Parse and apply patch text in one step.
pub fn apply_patch_text(text: &str, cwd: &Path) -> Result<Vec<FileChangePreview>, PatchError> {
    apply(&crate::parse_patch(text)?, cwd)
}

/// A short, model-facing summary of applied changes, e.g.
/// `Success. Updated the following files:\nA new.txt\nM src/app.py`.
pub fn summarize(changes: &[FileChangePreview], cwd: &Path) -> String {
    let cwd = base_dir(cwd);
    let mut out = String::from("Success. Updated the following files:");
    for change in changes {
        let shown = display_path(&change.path, &cwd);
        let line = match (change.kind, &change.move_to) {
            (ChangeKind::Add, _) => format!("A {shown}"),
            (ChangeKind::Delete, _) => format!("D {shown}"),
            (ChangeKind::Update, Some(dest)) => format!("R {shown} -> {}", display_path(dest, &cwd)),
            (ChangeKind::Update, None) => format!("M {shown}"),
        };
        out.push('\n');
        out.push_str(&line);
    }
    out
}

struct Plan {
    previews: Vec<FileChangePreview>,
    changes: Vec<PendingChange>,
}

/// Final state of one path after the whole patch.
struct PendingChange {
    path: PathBuf,
    shown: String,
    /// `None` = delete.
    content: Option<String>,
}

struct Entry {
    path: PathBuf,
    shown: String,
    original: Option<String>,
    current: Option<String>,
}

/// In-memory view of the files a patch touches.
#[derive(Default)]
struct Overlay {
    entries: Vec<Entry>,
}

impl Overlay {
    fn read(&mut self, path: &Path, shown: &str) -> Result<Option<String>, PatchError> {
        if let Some(entry) = self.entries.iter().find(|e| e.path == path) {
            return Ok(entry.current.clone());
        }
        let disk = read_disk(path, shown)?;
        self.entries.push(Entry {
            path: path.to_path_buf(),
            shown: shown.to_string(),
            original: disk.clone(),
            current: disk.clone(),
        });
        Ok(disk)
    }

    /// Callers always `read` a path before writing it.
    fn write(&mut self, path: &Path, content: Option<String>) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.path == path) {
            entry.current = content;
        }
    }

    fn into_changes(self) -> Vec<PendingChange> {
        self.entries
            .into_iter()
            .filter(|e| e.current != e.original)
            .map(|e| PendingChange { path: e.path, shown: e.shown, content: e.current })
            .collect()
    }
}

fn read_disk(path: &Path, shown: &str) -> Result<Option<String>, PatchError> {
    match fs::metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(PatchError::Io { path: shown.to_string(), action: "read", source: e }),
        Ok(meta) if meta.is_dir() => Err(PatchError::IsDirectory { path: shown.to_string() }),
        Ok(_) => {
            let bytes =
                fs::read(path).map_err(|e| PatchError::Io { path: shown.to_string(), action: "read", source: e })?;
            String::from_utf8(bytes).map(Some).map_err(|_| PatchError::NotUtf8 { path: shown.to_string() })
        }
    }
}

fn plan(patch: &Patch, cwd: &Path) -> Result<Plan, PatchError> {
    let cwd = base_dir(cwd);
    let mut overlay = Overlay::default();
    let mut previews = Vec::with_capacity(patch.ops.len());

    for op in &patch.ops {
        let abs = resolve(&cwd, op.path());
        let shown = display_path(&abs, &cwd);
        match op {
            FileOp::Add { contents, .. } => {
                let old = overlay.read(&abs, &shown)?;
                let (new_text, old_norm, new_norm) = match &old {
                    Some(old_text) => {
                        let old_file = TextFile::parse(old_text);
                        let new_file = TextFile::with_style_of(contents, &old_file);
                        (new_file.render(), old_file.render_normalized(), new_file.render_normalized())
                    }
                    None => (contents.clone(), String::new(), contents.clone()),
                };
                let old_label = if old.is_some() { format!("a/{shown}") } else { "/dev/null".to_string() };
                let (unified_diff, additions, deletions) =
                    unified_diff(&old_norm, &new_norm, &old_label, &format!("b/{shown}"));
                overlay.write(&abs, Some(new_text.clone()));
                previews.push(FileChangePreview {
                    path: abs,
                    kind: ChangeKind::Add,
                    move_to: None,
                    old_contents: old,
                    new_contents: Some(new_text),
                    unified_diff,
                    additions,
                    deletions,
                });
            }
            FileOp::Delete { .. } => {
                let old = overlay
                    .read(&abs, &shown)?
                    .ok_or_else(|| PatchError::FileNotFound { path: shown.clone(), action: "delete" })?;
                let old_norm = TextFile::parse(&old).render_normalized();
                let (unified_diff, additions, deletions) =
                    unified_diff(&old_norm, "", &format!("a/{shown}"), "/dev/null");
                overlay.write(&abs, None);
                previews.push(FileChangePreview {
                    path: abs,
                    kind: ChangeKind::Delete,
                    move_to: None,
                    old_contents: Some(old),
                    new_contents: None,
                    unified_diff,
                    additions,
                    deletions,
                });
            }
            FileOp::Update { move_to, chunks, .. } => {
                let old = overlay
                    .read(&abs, &shown)?
                    .ok_or_else(|| PatchError::FileNotFound { path: shown.clone(), action: "update" })?;
                let old_file = TextFile::parse(&old);
                let replacements = compute_replacements(&shown, &old_file.lines, chunks)?;
                let new_file = old_file.apply(&replacements);
                let new_text = new_file.render();

                let dest = move_to.as_deref().map(|m| resolve(&cwd, m)).filter(|d| *d != abs);
                let new_shown = dest.as_deref().map(|d| display_path(d, &cwd)).unwrap_or_else(|| shown.clone());
                let (unified_diff, additions, deletions) = unified_diff(
                    &old_file.render_normalized(),
                    &new_file.render_normalized(),
                    &format!("a/{shown}"),
                    &format!("b/{new_shown}"),
                );
                match &dest {
                    Some(dest) => {
                        // Validates the destination (e.g. not a directory); overwriting is allowed.
                        overlay.read(dest, &new_shown)?;
                        overlay.write(dest, Some(new_text.clone()));
                        overlay.write(&abs, None);
                    }
                    None => overlay.write(&abs, Some(new_text.clone())),
                }
                previews.push(FileChangePreview {
                    path: abs,
                    kind: ChangeKind::Update,
                    move_to: dest,
                    old_contents: Some(old),
                    new_contents: Some(new_text),
                    unified_diff,
                    additions,
                    deletions,
                });
            }
        }
    }
    Ok(Plan { previews, changes: overlay.into_changes() })
}

/// Locate every chunk of an update in `lines`.
pub(crate) fn compute_replacements(
    path: &str,
    lines: &[String],
    chunks: &[UpdateChunk],
) -> Result<Vec<Replacement>, PatchError> {
    let chunk_count = chunks.len();
    let mut cursor = 0usize;
    let mut replacements: Vec<Replacement> = Vec::with_capacity(chunk_count);

    for (index, chunk) in chunks.iter().enumerate() {
        let number = index + 1;

        // Seek the `@@` headers forward from the previous chunk.
        let mut header_line: Option<usize> = None;
        let mut missing_header: Option<&str> = None;
        let mut from = cursor;
        for header in &chunk.headers {
            match seek_header(lines, header, from) {
                Some(found) => {
                    header_line = Some(found);
                    from = found + 1;
                }
                None => {
                    missing_header = Some(header);
                    break;
                }
            }
        }

        if chunk.old_lines.is_empty() {
            // Pure insertion: after the header line if given, else at EOF.
            if let Some(header) = missing_header {
                return Err(PatchError::HeaderNotFound {
                    path: path.to_string(),
                    chunk: number,
                    chunk_count,
                    header: header.to_string(),
                });
            }
            let at = match header_line {
                Some(line) if !chunk.is_eof => line + 1,
                _ => lines.len(),
            };
            replacements.push(Replacement { start: at, old_len: 0, new_lines: chunk.new_lines.clone(), chunk: number });
            cursor = at;
            continue;
        }

        let mut starts: Vec<usize> = Vec::with_capacity(4);
        if missing_header.is_none() {
            if let Some(line) = header_line {
                starts.push(line + 1);
                // The header line may double as the first context line.
                starts.push(line);
            }
        }
        starts.push(cursor);
        let locate = |old: &[String]| starts.iter().find_map(|&s| seek_sequence(lines, old, s, chunk.is_eof));

        let mut old_lines = chunk.old_lines.clone();
        let mut new_lines = chunk.new_lines.clone();
        let mut found = locate(&old_lines);
        if found.is_none() {
            // Models often add blank context lines that are not in the file.
            let (old_trimmed, new_trimmed) = trim_blank_context(&old_lines, &new_lines);
            if old_trimmed.len() != old_lines.len() && !old_trimmed.is_empty() {
                found = locate(&old_trimmed);
                if found.is_some() {
                    old_lines = old_trimmed;
                    new_lines = new_trimmed;
                }
            }
        }

        let Some(start) = found else {
            return Err(PatchError::ContextNotFound(Box::new(ContextMismatch {
                path: path.to_string(),
                chunk: number,
                chunk_count,
                headers: chunk.headers.clone(),
                expected: chunk.old_lines.clone(),
                is_eof: chunk.is_eof,
                searched_from: cursor + 1,
                closest: closest_match(lines, &chunk.old_lines),
            })));
        };
        keep_file_text_for_context(&mut new_lines, &old_lines, &lines[start..start + old_lines.len()]);
        replacements.push(Replacement { start, old_len: old_lines.len(), new_lines, chunk: number });
        cursor = start + old_lines.len();
    }

    // Every search starts at or after the end of the previous chunk, so the
    // replacements are already ordered and disjoint.
    debug_assert!(replacements.windows(2).all(|p| p[0].start + p[0].old_len <= p[1].start));
    Ok(replacements)
}

/// Context lines may have matched fuzzily (whitespace, punctuation). Keep the
/// file's own text for them instead of the model's approximation, so only
/// the `-`/`+` lines actually change.
fn keep_file_text_for_context(new_lines: &mut [String], old_lines: &[String], file_lines: &[String]) {
    if old_lines == file_lines {
        return;
    }
    for op in similar::capture_diff_slices(similar::Algorithm::Myers, old_lines, new_lines) {
        if let similar::DiffOp::Equal { old_index, new_index, len } = op {
            for k in 0..len {
                new_lines[new_index + k].clone_from(&file_lines[old_index + k]);
            }
        }
    }
}

/// Drop leading/trailing blank lines that are context (present in both old and new).
fn trim_blank_context(old: &[String], new: &[String]) -> (Vec<String>, Vec<String>) {
    let blank = |s: &String| s.trim().is_empty();
    let mut old = old.to_vec();
    let mut new = new.to_vec();
    while old.len() > 1 && old.last().is_some_and(blank) && new.last().is_some_and(blank) {
        old.pop();
        new.pop();
    }
    while old.len() > 1 && old.first().is_some_and(blank) && new.first().is_some_and(blank) {
        old.remove(0);
        new.remove(0);
    }
    (old, new)
}

fn unified_diff(old: &str, new: &str, old_label: &str, new_label: &str) -> (String, u32, u32) {
    let diff = TextDiff::from_lines(old, new);
    let (mut additions, mut deletions) = (0u32, 0u32);
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    if additions == 0 && deletions == 0 {
        return (String::new(), 0, 0);
    }
    let text = diff.unified_diff().context_radius(3).header(old_label, new_label).to_string();
    (text, additions, deletions)
}

fn base_dir(cwd: &Path) -> PathBuf {
    normalize_lexically(&std::path::absolute(cwd).unwrap_or_else(|_| cwd.to_path_buf()))
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        normalize_lexically(path)
    } else {
        normalize_lexically(&cwd.join(path))
    }
}

/// Remove `.` components and resolve `..` without touching the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Path relative to `cwd` (forward slashes) when inside it, else absolute.
fn display_path(path: &Path, cwd: &Path) -> String {
    let shown = match path.strip_prefix(cwd) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel,
        _ => path,
    };
    shown.to_string_lossy().replace('\\', "/")
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_sibling(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(".{name}.odex-patch.{}.{n}.tmp", std::process::id()))
}

fn io_error(change: &PendingChange, action: &'static str, source: io::Error) -> PatchError {
    PatchError::Io { path: change.shown.clone(), action, source }
}

fn commit(changes: &[PendingChange]) -> Result<(), PatchError> {
    struct Staged<'a> {
        tmp: PathBuf,
        change: &'a PendingChange,
        content: &'a str,
    }
    fn cleanup(staged: &[Staged<'_>]) {
        for s in staged {
            let _ = fs::remove_file(&s.tmp);
        }
    }

    // Stage: nothing visible changes yet (apart from new parent directories).
    let mut staged: Vec<Staged<'_>> = Vec::new();
    for change in changes {
        let Some(content) = change.content.as_deref() else { continue };
        if let Some(parent) = change.path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                cleanup(&staged);
                return Err(io_error(change, "create parent directories", e));
            }
        }
        let tmp = temp_sibling(&change.path);
        if let Err(e) = fs::write(&tmp, content) {
            let _ = fs::remove_file(&tmp);
            cleanup(&staged);
            return Err(io_error(change, "write", e));
        }
        staged.push(Staged { tmp, change, content });
    }

    // Commit writes.
    for (i, s) in staged.iter().enumerate() {
        #[cfg(unix)]
        if let Ok(meta) = fs::metadata(&s.change.path) {
            let _ = fs::set_permissions(&s.tmp, meta.permissions());
        }
        if fs::rename(&s.tmp, &s.change.path).is_err() {
            // e.g. on Windows the target may be open without FILE_SHARE_DELETE;
            // fall back to writing in place.
            let result = fs::write(&s.change.path, s.content);
            let _ = fs::remove_file(&s.tmp);
            if let Err(e) = result {
                cleanup(&staged[i + 1..]);
                return Err(io_error(s.change, "write", e));
            }
        }
    }

    // Deletions last, so a move writes the destination before removing the source.
    for change in changes.iter().filter(|c| c.content.is_none()) {
        match fs::remove_file(&change.path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(change, "delete", e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    fn chunk(headers: &[&str], old: &[&str], new: &[&str], eof: bool) -> UpdateChunk {
        UpdateChunk { headers: v(headers), old_lines: v(old), new_lines: v(new), is_eof: eof }
    }

    #[test]
    fn lexical_normalisation() {
        let base = if cfg!(windows) { PathBuf::from("C:\\work\\repo") } else { PathBuf::from("/work/repo") };
        let resolved = resolve(&base, "./src/../lib/./a.rs");
        assert_eq!(resolved, base.join("lib").join("a.rs"));
        assert_eq!(display_path(&resolved, &base), "lib/a.rs");
        let up = resolve(&base, "../other/x");
        assert_eq!(display_path(&up, &base), up.to_string_lossy().replace('\\', "/"));
        assert_eq!(normalize_lexically(Path::new("a/../../b")), PathBuf::from("../b"));
    }

    #[test]
    fn header_line_can_be_first_context_line() {
        let lines = v(&["fn a() {", "    1", "}", "fn b() {", "    2", "}"]);
        let reps = compute_replacements(
            "f",
            &lines,
            &[chunk(&["fn b() {"], &["fn b() {", "    2"], &["fn b() {", "    3"], false)],
        )
        .unwrap();
        assert_eq!(reps[0].start, 3);
    }

    #[test]
    fn chunks_must_appear_in_file_order() {
        let lines = v(&["a", "b", "c", "d"]);
        let err =
            compute_replacements("f", &lines, &[chunk(&[], &["c"], &["C"], false), chunk(&[], &["a"], &["A"], false)])
                .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("(searching from line 4, after chunk 1)"), "{message}");
        assert!(message.contains("same order"), "{message}");
        let PatchError::ContextNotFound(details) = err else { panic!("{message}") };
        assert_eq!(details.chunk, 2);
        // The earlier occurrence is still pointed out to the model.
        assert_eq!(details.closest.unwrap().line, 1);
    }

    #[test]
    fn chunks_cannot_reuse_lines_of_the_previous_chunk() {
        let lines = v(&["a", "b", "c"]);
        let err = compute_replacements(
            "f",
            &lines,
            &[chunk(&[], &["b", "c"], &["x"], false), chunk(&[], &["a", "b"], &["y"], false)],
        )
        .unwrap_err();
        assert!(matches!(err, PatchError::ContextNotFound(ref d) if d.chunk == 2), "{err}");
    }

    #[test]
    fn trailing_blank_context_is_retried_without_it() {
        let lines = v(&["a", "b"]);
        let reps = compute_replacements("f", &lines, &[chunk(&[], &["b", ""], &["B", ""], false)]).unwrap();
        assert_eq!(reps[0], Replacement { start: 1, old_len: 1, new_lines: v(&["B"]), chunk: 1 });
    }

    #[test]
    fn missing_header_for_insertion_is_an_error() {
        let err = compute_replacements("f", &v(&["a"]), &[chunk(&["nope"], &[], &["x"], false)]).unwrap_err();
        assert!(err.to_string().contains("`@@ nope`"), "{err}");
    }

    #[test]
    fn missing_header_falls_back_to_plain_search() {
        let reps = compute_replacements("f", &v(&["a", "b"]), &[chunk(&["nope"], &["b"], &["c"], false)]).unwrap();
        assert_eq!(reps[0].start, 1);
    }
}
