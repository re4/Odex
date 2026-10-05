//! `edit_file` / `write_file` with line-ending and BOM preservation and diffs.

use std::path::{Path, PathBuf};

use odex_protocol::{FileChange, FileChangeKind};

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedWrite {
    pub path: PathBuf,
    pub old: Option<String>,
    pub new: String,
    pub change: FileChange,
}

const BOM: &str = "\u{feff}";

fn detect_crlf(s: &str) -> bool {
    let crlf = s.matches("\r\n").count();
    let lf = s.matches('\n').count();
    crlf > 0 && crlf * 2 >= lf
}

pub fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

pub fn display_path(cwd: &Path, p: &Path) -> String {
    p.strip_prefix(cwd)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| p.to_string_lossy().to_string())
}

/// Unified diff with additions/deletions counts.
pub fn unified_diff(old: &str, new: &str, path: &str) -> (String, u32, u32) {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut adds = 0;
    let mut dels = 0;
    for c in diff.iter_all_changes() {
        match c.tag() {
            similar::ChangeTag::Insert => adds += 1,
            similar::ChangeTag::Delete => dels += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    let text = diff.unified_diff().context_radius(3).header(&format!("a/{path}"), &format!("b/{path}")).to_string();
    (text, adds, dels)
}

fn change(cwd: &Path, path: &Path, old: Option<&str>, new: &str) -> FileChange {
    let disp = display_path(cwd, path);
    let (diff, additions, deletions) = unified_diff(old.unwrap_or(""), new, &disp);
    FileChange {
        path: disp,
        kind: if old.is_some() { FileChangeKind::Update } else { FileChangeKind::Add },
        move_path: None,
        diff,
        additions,
        deletions,
    }
}

/// Normalize for fuzzy matching: trailing whitespace off each line, CRLF→LF.
fn loose(s: &str) -> String {
    s.replace("\r\n", "\n").lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n")
}

/// Plan an `edit_file` without writing.
pub fn plan_edit(
    cwd: &Path,
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<PlannedWrite, String> {
    let full = resolve(cwd, path);
    if old_string.is_empty() {
        if full.exists() {
            let cur = std::fs::read_to_string(&full).unwrap_or_default();
            if !cur.trim().is_empty() {
                return Err(format!(
                    "{path} already exists; old_string must not be empty when editing an existing file"
                ));
            }
        }
        return Ok(PlannedWrite {
            change: change(cwd, &full, None, new_string),
            path: full,
            old: None,
            new: new_string.to_string(),
        });
    }
    let raw = std::fs::read_to_string(&full).map_err(|e| format!("cannot read {path}: {e}"))?;
    let has_bom = raw.starts_with(BOM);
    let body = raw.strip_prefix(BOM).unwrap_or(&raw);
    let crlf = detect_crlf(body);
    let text = body.replace("\r\n", "\n");
    let old_n = old_string.replace("\r\n", "\n");
    let new_n = new_string.replace("\r\n", "\n");
    if old_n == new_n {
        return Err("old_string and new_string are identical; nothing to change".into());
    }
    let count = text.matches(&old_n).count();
    let updated = if count == 1 || (count > 1 && replace_all) {
        if replace_all {
            text.replace(&old_n, &new_n)
        } else {
            text.replacen(&old_n, &new_n, 1)
        }
    } else if count > 1 {
        return Err(format!(
            "old_string matches {count} times in {path}; add more surrounding context to make it unique, or set replace_all=true"
        ));
    } else {
        // fuzzy: ignore trailing whitespace differences, line by line
        let lt = loose(&text);
        let lo = loose(&old_n);
        let fuzzy_count = lt.matches(&lo).count();
        if fuzzy_count == 1 || (fuzzy_count > 1 && replace_all) {
            // map back: operate on the loose text (trailing whitespace is dropped in touched file)
            if replace_all {
                lt.replace(&lo, &new_n)
            } else {
                lt.replacen(&lo, &new_n, 1)
            }
        } else {
            return Err(not_found_message(path, &text, &old_n));
        }
    };
    let mut out = if crlf { updated.replace('\n', "\r\n") } else { updated };
    if has_bom {
        out = format!("{BOM}{out}");
    }
    Ok(PlannedWrite { change: change(cwd, &full, Some(&raw), &out), path: full, old: Some(raw), new: out })
}

fn not_found_message(path: &str, text: &str, old: &str) -> String {
    // point at the most similar line to help the model retry
    let first = old.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut best: Option<(usize, usize, &str)> = None;
    if !first.is_empty() {
        for (i, l) in text.lines().enumerate() {
            let score = common_prefix(l.trim(), first);
            if score > 3 && best.map(|b| score > b.1).unwrap_or(true) {
                best = Some((i + 1, score, l));
            }
        }
    }
    match best {
        Some((line, _, l)) => format!(
            "old_string was not found in {path}. The closest line is {line}: `{}`. Re-read the file and copy the exact text (whitespace included).",
            l.trim()
        ),
        None => format!("old_string was not found in {path}. Re-read the file and copy the exact text to replace."),
    }
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Plan a `write_file`. Keeps the existing file's BOM and line endings.
pub fn plan_write(cwd: &Path, path: &str, content: &str) -> PlannedWrite {
    let full = resolve(cwd, path);
    let old = std::fs::read_to_string(&full).ok();
    let mut new = content.to_string();
    if let Some(o) = &old {
        let body = o.strip_prefix(BOM).unwrap_or(o);
        if detect_crlf(body) && !new.contains("\r\n") {
            new = new.replace('\n', "\r\n");
        }
        if o.starts_with(BOM) && !new.starts_with(BOM) {
            new = format!("{BOM}{new}");
        }
    }
    PlannedWrite { change: change(cwd, &full, old.as_deref(), &new), path: full, old, new }
}

pub fn commit(w: &PlannedWrite) -> std::io::Result<()> {
    if let Some(parent) = w.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&w.path, &w.new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_unique_and_crlf_bom() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "\u{feff}one\r\ntwo\r\nthree\r\n").unwrap();
        let w = plan_edit(d.path(), "a.txt", "two", "TWO", false).unwrap();
        commit(&w).unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join("a.txt")).unwrap(), "\u{feff}one\r\nTWO\r\nthree\r\n");
        assert_eq!(w.change.additions, 1);
        assert_eq!(w.change.deletions, 1);
        assert!(w.change.diff.contains("-two"));
    }

    #[test]
    fn ambiguous_and_missing() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.txt"), "x = 1\nx = 1\nfn foo() {}\n").unwrap();
        let e = plan_edit(d.path(), "a.txt", "x = 1", "x = 2", false).unwrap_err();
        assert!(e.contains("matches 2 times"));
        let w = plan_edit(d.path(), "a.txt", "x = 1", "x = 2", true).unwrap();
        assert_eq!(w.new, "x = 2\nx = 2\nfn foo() {}\n");
        let e = plan_edit(d.path(), "a.txt", "fn foo(bar) {}", "z", false).unwrap_err();
        assert!(e.contains("closest line is 3"), "{e}");
    }

    #[test]
    fn fuzzy_trailing_whitespace() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.py"), "def f():   \n    return 1\n").unwrap();
        let w = plan_edit(d.path(), "a.py", "def f():\n    return 1", "def f():\n    return 2", false).unwrap();
        assert!(w.new.contains("return 2"));
    }

    #[test]
    fn create_with_empty_old() {
        let d = tempfile::tempdir().unwrap();
        let w = plan_edit(d.path(), "new/b.txt", "", "hello\n", false).unwrap();
        commit(&w).unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join("new/b.txt")).unwrap(), "hello\n");
        assert_eq!(w.change.kind, FileChangeKind::Add);
    }

    #[test]
    fn write_keeps_crlf() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("w.txt"), "a\r\nb\r\n").unwrap();
        let w = plan_write(d.path(), "w.txt", "c\nd\n");
        assert_eq!(w.new, "c\r\nd\r\n");
    }
}
