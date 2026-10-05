//! Unified diff parsing, single-hunk patch building and synthesized diffs for untracked files.

use std::path::Path;

use odex_protocol::{DiffFile, DiffHunk, DiffLine, DiffLineKind, DiffStats};

use crate::error::{GitError, Result};

/// Untracked files larger than this are listed without content.
const MAX_UNTRACKED_BYTES: u64 = 4 * 1024 * 1024;
/// Bytes inspected for NUL when deciding whether a file is binary (same as git).
const BINARY_SNIFF: usize = 8000;

/// Parse `git diff` / `git show` style unified diff text into files and hunks.
///
/// Handles git extended headers (new/deleted files, renames, copies, mode changes), binary
/// files (`Binary files … differ` and `GIT binary patch`), quoted paths, multiple hunks,
/// `\ No newline at end of file` markers (kept as [`DiffLineKind::Meta`] lines) and plain
/// (non-git) `---`/`+++` diffs. Each file's `header` holds the exact header lines needed to
/// rebuild a patch for any single hunk (see [`build_hunk_patch`]).
pub fn parse_unified_diff(text: &str) -> Vec<DiffFile> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let mut files = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("diff --git ") {
            i = parse_file(&lines, i, true, &mut files);
        } else if line.starts_with("--- ") && lines.get(i + 1).is_some_and(|l| l.starts_with("+++ ")) {
            i = parse_file(&lines, i, false, &mut files);
        } else {
            i += 1;
        }
    }
    files
}

#[derive(Default)]
struct FileBuilder {
    header: Vec<String>,
    git_old: Option<String>,
    git_new: Option<String>,
    minus: Option<Option<String>>,
    plus: Option<Option<String>>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    copy: bool,
    new_file: bool,
    deleted_file: bool,
    binary: bool,
    hunks: Vec<DiffHunk>,
}

impl FileBuilder {
    fn finish(self) -> DiffFile {
        let old = self.rename_from.clone().or_else(|| self.minus.clone().flatten()).or_else(|| {
            if self.minus.is_some() {
                None
            } else {
                self.git_old.clone()
            }
        });
        let new = self.rename_to.clone().or_else(|| self.plus.clone().flatten()).or_else(|| {
            if self.plus.is_some() {
                None
            } else {
                self.git_new.clone()
            }
        });
        let added = self.new_file || matches!(self.minus, Some(None));
        let deleted = self.deleted_file || matches!(self.plus, Some(None));
        let renamed = self.rename_from.is_some() && !self.copy;
        let path = if deleted { old.clone().or(new.clone()) } else { new.clone().or(old.clone()) }.unwrap_or_default();
        let old_path = if (renamed || self.copy) && old.as_deref() != Some(path.as_str()) { old } else { None };
        let status = if added || self.copy {
            "added"
        } else if deleted {
            "deleted"
        } else if renamed {
            "renamed"
        } else if self.binary {
            "binary"
        } else {
            "modified"
        };
        let mut additions = 0;
        let mut deletions = 0;
        for h in &self.hunks {
            for l in &h.lines {
                match l.kind {
                    DiffLineKind::Add => additions += 1,
                    DiffLineKind::Del => deletions += 1,
                    _ => {}
                }
            }
        }
        let mut header = self.header.join("\n");
        if !header.is_empty() {
            header.push('\n');
        }
        DiffFile {
            path,
            old_path,
            status: status.to_string(),
            additions,
            deletions,
            binary: self.binary,
            hunks: self.hunks,
            header,
        }
    }
}

fn parse_file(lines: &[&str], start: usize, git_style: bool, files: &mut Vec<DiffFile>) -> usize {
    let mut f = FileBuilder::default();
    let mut i = start;
    if git_style {
        let line = lines[i];
        f.header.push(line.to_string());
        if let Some((a, b)) = parse_diff_git_line(&line["diff --git ".len()..]) {
            f.git_old = Some(a);
            f.git_new = Some(b);
        }
        i += 1;
    }
    // Extended headers and the ---/+++ pair.
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.starts_with("diff --git ") || line.starts_with("@@") {
            break;
        }
        if line.starts_with("--- ") && lines.get(i + 1).is_some_and(|l| l.starts_with("+++ ")) {
            let plus = lines[i + 1].strip_suffix('\r').unwrap_or(lines[i + 1]);
            f.header.push(line.to_string());
            f.header.push(plus.to_string());
            f.minus = Some(parse_header_name(&line[4..], "a/"));
            f.plus = Some(parse_header_name(&plus[4..], "b/"));
            i += 2;
            break;
        }
        if !git_style {
            break;
        }
        if line.starts_with("Binary files ") && line.ends_with(" differ") {
            f.binary = true;
            i += 1;
            continue;
        }
        if line == "GIT binary patch" {
            f.binary = true;
            i += 1;
            while i < lines.len() && !lines[i].starts_with("diff --git ") {
                i += 1;
            }
            break;
        }
        if line.starts_with("new file mode ") {
            f.new_file = true;
        } else if line.starts_with("deleted file mode ") {
            f.deleted_file = true;
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            f.rename_from = Some(unquote(rest));
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            f.rename_to = Some(unquote(rest));
        } else if let Some(rest) = line.strip_prefix("copy from ") {
            f.rename_from = Some(unquote(rest));
            f.copy = true;
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            f.rename_to = Some(unquote(rest));
            f.copy = true;
        } else if !(line.starts_with("index ")
            || line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || line.starts_with("similarity index ")
            || line.starts_with("dissimilarity index "))
        {
            // Not an extended header we know: stop before it.
            break;
        }
        f.header.push(line.to_string());
        i += 1;
    }
    // Hunks.
    while i < lines.len() && lines[i].starts_with("@@") {
        let Some((header, old_start, old_lines, new_start, new_lines)) = parse_hunk_header(lines[i]) else {
            break;
        };
        i += 1;
        let mut hunk = DiffHunk {
            index: f.hunks.len() as u32,
            header,
            old_start,
            old_lines,
            new_start,
            new_lines,
            lines: Vec::new(),
        };
        let (mut old_rem, mut new_rem) = (old_lines, new_lines);
        let (mut old_no, mut new_no) = (old_start, new_start);
        while i < lines.len() {
            let l = lines[i];
            if old_rem == 0 && new_rem == 0 && !l.starts_with('\\') {
                break;
            }
            let (kind, text) = match l.as_bytes().first() {
                Some(b' ') => (DiffLineKind::Context, &l[1..]),
                // Some tools strip the space of empty context lines.
                None => (DiffLineKind::Context, ""),
                Some(b'+') => (DiffLineKind::Add, &l[1..]),
                Some(b'-') => (DiffLineKind::Del, &l[1..]),
                Some(b'\\') => (DiffLineKind::Meta, l),
                _ => break,
            };
            let (o, n) = match kind {
                DiffLineKind::Context => {
                    let r = (Some(old_no), Some(new_no));
                    old_no += 1;
                    new_no += 1;
                    old_rem = old_rem.saturating_sub(1);
                    new_rem = new_rem.saturating_sub(1);
                    r
                }
                DiffLineKind::Add => {
                    let r = (None, Some(new_no));
                    new_no += 1;
                    new_rem = new_rem.saturating_sub(1);
                    r
                }
                DiffLineKind::Del => {
                    let r = (Some(old_no), None);
                    old_no += 1;
                    old_rem = old_rem.saturating_sub(1);
                    r
                }
                DiffLineKind::Meta => (None, None),
            };
            hunk.lines.push(DiffLine { kind, old_no: o, new_no: n, text: text.to_string() });
            i += 1;
        }
        f.hunks.push(hunk);
    }
    files.push(f.finish());
    i.max(start + 1)
}

/// `@@ -a,b +c,d @@ section` → (full header, a, b, c, d). Counts default to 1.
fn parse_hunk_header(line: &str) -> Option<(String, u32, u32, u32, u32)> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let rest = line.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let mut parts = rest[..end].split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let range = |s: &str| -> Option<(u32, u32)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (os, ol) = range(old)?;
    let (ns, nl) = range(new)?;
    Some((line.to_string(), os, ol, ns, nl))
}

/// Name from a `---`/`+++` line: `None` for `/dev/null`; strips the `a/`/`b/` prefix, quotes and
/// anything after a tab (git appends a tab to names containing spaces; other tools add dates).
fn parse_header_name(raw: &str, prefix: &str) -> Option<String> {
    let raw = if raw.starts_with('"') { raw } else { raw.split('\t').next().unwrap_or(raw) };
    let name = unquote(raw.trim_end_matches('\t'));
    if name == "/dev/null" {
        return None;
    }
    Some(name.strip_prefix(prefix).map(str::to_string).unwrap_or(name))
}

/// Paths from the `diff --git a/X b/Y` line (used when there are no `---`/`+++` lines).
fn parse_diff_git_line(rest: &str) -> Option<(String, String)> {
    let rest = rest.strip_suffix('\r').unwrap_or(rest);
    if rest.starts_with('"') {
        let (a, tail) = split_quoted(rest)?;
        let tail = tail.trim_start();
        let b = if tail.starts_with('"') { split_quoted(tail)?.0 } else { tail.to_string() };
        return Some((strip(&a, "a/"), strip(&b, "b/")));
    }
    if let Some(idx) = rest.find(" \"") {
        // a/plain "b/quoted"
        let a = &rest[..idx];
        let (b, _) = split_quoted(&rest[idx + 1..])?;
        return Some((strip(a, "a/"), strip(&b, "b/")));
    }
    // Unquoted: prefer the split where both names are equal (the common, non-rename case).
    let candidates: Vec<usize> = rest.match_indices(" b/").map(|(i, _)| i).collect();
    for &i in &candidates {
        let (a, b) = (&rest[..i], &rest[i + 1..]);
        if strip(a, "a/") == strip(b, "b/") {
            return Some((strip(a, "a/"), strip(b, "b/")));
        }
    }
    let i = *candidates.first()?;
    Some((strip(&rest[..i], "a/"), strip(&rest[i + 1..], "b/")))
}

fn strip(s: &str, prefix: &str) -> String {
    s.strip_prefix(prefix).unwrap_or(s).to_string()
}

/// Split a leading C-quoted string off `s`: returns (unquoted, rest).
fn split_quoted(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some((unquote(&s[..=i]), &s[i + 1..])),
            _ => i += 1,
        }
    }
    None
}

/// Undo git's C-style path quoting (`"a\tb\303\251"`). Unquoted input is returned as is.
pub(crate) fn unquote(s: &str) -> String {
    if !(s.len() >= 2 && s.starts_with('"') && s.ends_with('"')) {
        return s.to_string();
    }
    let inner = &s.as_bytes()[1..s.len() - 1];
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b != b'\\' || i + 1 >= inner.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let e = inner[i + 1];
        i += 2;
        match e {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = u32::from(e - b'0');
                let mut n = 1;
                while n < 3 && i < inner.len() && (b'0'..=b'7').contains(&inner[i]) {
                    v = v * 8 + u32::from(inner[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push(v as u8);
            }
            other => out.push(other),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// C-quote a name the way git does when it contains special characters.
pub(crate) fn quote_if_needed(name: &str) -> String {
    let needs = name.bytes().any(|b| b == b'"' || b == b'\\' || b < 0x20 || b == 0x7f);
    if !needs {
        return name.to_string();
    }
    let mut out = String::from("\"");
    for c in name.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Rebuild a patch containing only hunk `hunk_index` of `file`.
pub fn build_hunk_patch(file: &DiffFile, hunk_index: u32) -> Result<String> {
    if file.binary {
        return Err(GitError::Invalid(format!("{} is binary; hunks cannot be applied individually", file.path)));
    }
    let hunk = file
        .hunks
        .iter()
        .find(|h| h.index == hunk_index)
        .ok_or_else(|| GitError::Invalid(format!("{} has no hunk {hunk_index}", file.path)))?;
    if !file.header.contains("\n+++ ") && !file.header.starts_with("+++ ") {
        return Err(GitError::Invalid(format!("diff header for {} is incomplete", file.path)));
    }
    let mut patch = file.header.clone();
    if !patch.ends_with('\n') {
        patch.push('\n');
    }
    patch.push_str(&hunk.header);
    patch.push('\n');
    for line in &hunk.lines {
        match line.kind {
            DiffLineKind::Context => patch.push(' '),
            DiffLineKind::Add => patch.push('+'),
            DiffLineKind::Del => patch.push('-'),
            DiffLineKind::Meta => {}
        }
        patch.push_str(&line.text);
        patch.push('\n');
    }
    Ok(patch)
}

/// Whether `hunk` has no context lines (needs `git apply --unidiff-zero`).
pub(crate) fn hunk_has_no_context(file: &DiffFile, hunk_index: u32) -> bool {
    file.hunks
        .iter()
        .find(|h| h.index == hunk_index)
        .is_some_and(|h| !h.lines.iter().any(|l| l.kind == DiffLineKind::Context))
}

pub(crate) fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0)
}

/// A diff entry for an untracked file (all lines added), as `git diff --no-index /dev/null f`
/// would print it. Returns `None` for directories (e.g. nested repositories) and vanished files.
pub(crate) fn untracked_file_diff(root: &Path, rel: &str) -> Option<DiffFile> {
    if rel.ends_with('/') {
        return None;
    }
    let full = root.join(rel);
    let meta = std::fs::symlink_metadata(&full).ok()?;
    let (mode, content): (&str, Option<Vec<u8>>) = if meta.file_type().is_symlink() {
        let target = std::fs::read_link(&full).ok()?;
        ("120000", Some(target.to_string_lossy().replace('\\', "/").into_bytes()))
    } else if meta.is_file() {
        let mode = if is_executable(&meta) { "100755" } else { "100644" };
        if meta.len() > MAX_UNTRACKED_BYTES {
            (mode, None)
        } else {
            (mode, Some(std::fs::read(&full).ok()?))
        }
    } else {
        return None;
    };

    let a = quote_if_needed(&format!("a/{rel}"));
    let b = quote_if_needed(&format!("b/{rel}"));
    let mut header = format!("diff --git {a} {b}\nnew file mode {mode}\n");
    let mut file = DiffFile {
        path: rel.to_string(),
        old_path: None,
        status: "untracked".into(),
        additions: 0,
        deletions: 0,
        binary: false,
        hunks: Vec::new(),
        header: String::new(),
    };
    let content = match content {
        Some(c) if !looks_binary(&c) => c,
        _ => {
            file.binary = true;
            file.header = header;
            return Some(file);
        }
    };
    if content.is_empty() {
        file.header = header;
        return Some(file);
    }
    let tab = if b.contains(' ') { "\t" } else { "" };
    header.push_str(&format!("--- /dev/null\n+++ {b}{tab}\n"));
    let text = String::from_utf8_lossy(&content);
    let ends_with_newline = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(&text);
    let mut lines: Vec<DiffLine> = body
        .split('\n')
        .enumerate()
        .map(|(i, l)| DiffLine {
            kind: DiffLineKind::Add,
            old_no: None,
            new_no: Some(i as u32 + 1),
            text: l.to_string(),
        })
        .collect();
    let count = lines.len() as u32;
    if !ends_with_newline {
        lines.push(DiffLine {
            kind: DiffLineKind::Meta,
            old_no: None,
            new_no: None,
            text: "\\ No newline at end of file".into(),
        });
    }
    file.additions = count;
    file.hunks.push(DiffHunk {
        index: 0,
        header: format!("@@ -0,0 +1{} @@", if count == 1 { String::new() } else { format!(",{count}") }),
        old_start: 0,
        old_lines: 0,
        new_start: 1,
        new_lines: count,
        lines,
    });
    file.header = header;
    Some(file)
}

/// Line count of an untracked file for stats (0 for binary/oversized/unreadable files).
pub(crate) fn untracked_line_count(root: &Path, rel: &str) -> u32 {
    let full = root.join(rel);
    let Ok(meta) = std::fs::metadata(&full) else { return 0 };
    if !meta.is_file() || meta.len() > MAX_UNTRACKED_BYTES {
        return 0;
    }
    let Ok(bytes) = std::fs::read(&full) else { return 0 };
    if bytes.is_empty() || looks_binary(&bytes) {
        return 0;
    }
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count() as u32;
    newlines + u32::from(bytes.last() != Some(&b'\n'))
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// Parse `git diff --numstat -z` output.
pub(crate) fn parse_numstat_z(out: &[u8]) -> DiffStats {
    let tokens: Vec<&[u8]> = out.split(|&b| b == 0).collect();
    let mut stats = DiffStats::default();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        i += 1;
        if t.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(t);
        let mut parts = s.splitn(3, '\t');
        let adds = parts.next().unwrap_or("");
        let dels = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        if path.is_empty() {
            // Rename/copy: source and destination follow as separate tokens.
            i += 2;
        }
        stats.files_changed += 1;
        stats.additions += adds.parse::<u32>().unwrap_or(0);
        stats.deletions += dels.parse::<u32>().unwrap_or(0);
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const MULTI: &str = "diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,4 @@ mod a;
 line1
-line2
+line2 changed
 line3
 line4
@@ -10,3 +10,4 @@ fn x()
 ten
 eleven
+inserted
 twelve
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
index 3333333..4444444 100644
--- a/old name.txt\t
+++ b/new name.txt\t
@@ -1 +1 @@
-a
+b
\\ No newline at end of file
diff --git a/img.png b/img.png
new file mode 100644
index 0000000..5555555
Binary files /dev/null and b/img.png differ
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 6666666..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-x
--- not a header
diff --git a/script.sh b/script.sh
old mode 100644
new mode 100755
diff --git a/moved.rs b/dir/moved.rs
similarity index 100%
rename from moved.rs
rename to dir/moved.rs
";

    #[test]
    fn parses_multi_file_diff() {
        let files = parse_unified_diff(MULTI);
        assert_eq!(files.len(), 6);

        let lib = &files[0];
        assert_eq!(
            (lib.path.as_str(), lib.status.as_str(), lib.additions, lib.deletions),
            ("src/lib.rs", "modified", 2, 1)
        );
        assert_eq!(lib.hunks.len(), 2);
        assert_eq!(
            lib.header,
            "diff --git a/src/lib.rs b/src/lib.rs\nindex 1111111..2222222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n"
        );
        let h1 = &lib.hunks[1];
        assert_eq!((h1.index, h1.old_start, h1.old_lines, h1.new_start, h1.new_lines), (1, 10, 3, 10, 4));
        assert_eq!(h1.header, "@@ -10,3 +10,4 @@ fn x()");
        assert_eq!(
            h1.lines[2],
            DiffLine { kind: DiffLineKind::Add, old_no: None, new_no: Some(12), text: "inserted".into() }
        );
        assert_eq!(h1.lines[3].old_no, Some(12));
        assert_eq!(h1.lines[3].new_no, Some(13));

        let ren = &files[1];
        assert_eq!(ren.path, "new name.txt");
        assert_eq!(ren.old_path.as_deref(), Some("old name.txt"));
        assert_eq!(ren.status, "renamed");
        assert_eq!(ren.hunks[0].lines.len(), 3);
        assert_eq!(ren.hunks[0].lines[2].kind, DiffLineKind::Meta);
        assert_eq!(ren.hunks[0].lines[2].text, "\\ No newline at end of file");

        let img = &files[2];
        assert!(img.binary);
        assert_eq!((img.path.as_str(), img.status.as_str()), ("img.png", "added"));
        assert!(img.hunks.is_empty());

        let gone = &files[3];
        assert_eq!((gone.path.as_str(), gone.status.as_str(), gone.deletions), ("gone.txt", "deleted", 2));
        assert_eq!(gone.hunks[0].lines[1].text, "-- not a header");

        let mode = &files[4];
        assert_eq!((mode.path.as_str(), mode.status.as_str()), ("script.sh", "modified"));
        assert!(mode.header.contains("new mode 100755"));

        let moved = &files[5];
        assert_eq!(
            (moved.path.as_str(), moved.old_path.as_deref(), moved.status.as_str()),
            ("dir/moved.rs", Some("moved.rs"), "renamed")
        );
    }

    #[test]
    fn single_hunk_patch_roundtrip() {
        let files = parse_unified_diff(MULTI);
        let patch = build_hunk_patch(&files[0], 1).unwrap();
        assert_eq!(
            patch,
            "diff --git a/src/lib.rs b/src/lib.rs\nindex 1111111..2222222 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -10,3 +10,4 @@ fn x()\n ten\n eleven\n+inserted\n twelve\n"
        );
        let patch = build_hunk_patch(&files[1], 0).unwrap();
        assert!(patch.ends_with("-a\n+b\n\\ No newline at end of file\n"));
        assert!(build_hunk_patch(&files[2], 0).is_err());
        assert!(build_hunk_patch(&files[0], 7).is_err());
        // Re-parsing a rebuilt patch yields the same hunk.
        let reparsed = parse_unified_diff(&build_hunk_patch(&files[0], 0).unwrap());
        assert_eq!(reparsed[0].hunks[0].lines, files[0].hunks[0].lines);
    }

    #[test]
    fn quoted_paths_and_plain_diffs() {
        let text = "diff --git \"a/t\\303\\251st\\tx.txt\" \"b/t\\303\\251st\\tx.txt\"\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ \"b/t\\303\\251st\\tx.txt\"\n@@ -0,0 +1 @@\n+hi\n";
        let files = parse_unified_diff(text);
        assert_eq!(files[0].path, "tést\tx.txt");
        assert_eq!(files[0].status, "added");

        let plain = "--- foo.c\t2024-01-01 00:00:00\n+++ foo.c\t2024-01-02 00:00:00\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n";
        let files = parse_unified_diff(plain);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "foo.c");
        assert_eq!((files[0].additions, files[0].deletions), (1, 1));

        // Mode-only change with spaces in the name and no ---/+++ lines.
        let text = "diff --git a/my file b/my file\nold mode 100644\nnew mode 100755\n";
        assert_eq!(parse_unified_diff(text)[0].path, "my file");
        assert!(parse_unified_diff("").is_empty());
        assert!(parse_unified_diff("not a diff\n").is_empty());
    }

    #[test]
    fn hunk_header_defaults_and_crlf_content() {
        assert_eq!(parse_hunk_header("@@ -3 +4,0 @@"), Some(("@@ -3 +4,0 @@".into(), 3, 1, 4, 0)));
        assert!(parse_hunk_header("@@@ -1,2 -1,2 +1,3 @@@").is_none());
        let text = "diff --git a/w.txt b/w.txt\n--- a/w.txt\n+++ b/w.txt\n@@ -1 +1 @@\n-a\r\n+b\r\n";
        let f = &parse_unified_diff(text)[0];
        assert_eq!(f.hunks[0].lines[1].text, "b\r");
        assert!(build_hunk_patch(f, 0).unwrap().ends_with("+b\r\n"));
    }

    #[test]
    fn quoting_helpers() {
        assert_eq!(unquote("\"a\\\\b\\\"c\""), "a\\b\"c");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(quote_if_needed("a\tb"), "\"a\\tb\"");
        assert_eq!(quote_if_needed("tést"), "tést");
        assert_eq!(unquote(&quote_if_needed("x\"y\\z\n")), "x\"y\\z\n");
    }

    #[test]
    fn numstat_parsing() {
        let out = ["3\t1\tsrc/a.rs", "-\t-\timg.png", "5\t0\t", "old.rs", "new.rs", ""].join("\0");
        let s = parse_numstat_z(out.as_bytes());
        assert_eq!(s, DiffStats { files_changed: 3, additions: 8, deletions: 1 });
    }
}
