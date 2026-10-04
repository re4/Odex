//! Parsing of the `*** Begin Patch` format.
//!
//! The parser is deliberately lenient about the envelope (missing begin/end
//! markers, heredoc or code-fence wrappers, CRLF, surrounding blank lines)
//! but strict about anything that would change the meaning of an edit.

use crate::error::PatchError;
use crate::extract::extract_patch_from_command;

pub const BEGIN_PATCH: &str = "*** Begin Patch";
pub const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File:";
const DELETE_FILE: &str = "*** Delete File:";
const UPDATE_FILE: &str = "*** Update File:";
const MOVE_TO: &str = "*** Move to:";
const END_OF_FILE: &str = "*** End of File";

/// A parsed patch: an ordered list of file operations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Patch {
    pub ops: Vec<FileOp>,
}

/// One file-level operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOp {
    /// Create (or overwrite) a file. `contents` is LF-separated and ends with a
    /// newline unless the file is empty.
    Add { path: String, contents: String },
    /// Remove a file.
    Delete { path: String },
    /// Edit a file in place, optionally renaming it.
    Update { path: String, move_to: Option<String>, chunks: Vec<UpdateChunk> },
}

impl FileOp {
    /// The path the operation reads from / targets (as written in the patch).
    pub fn path(&self) -> &str {
        match self {
            FileOp::Add { path, .. } | FileOp::Delete { path } | FileOp::Update { path, .. } => path,
        }
    }
}

/// One `@@` chunk of an update.
///
/// `old_lines` are the context and `-` lines in order; `new_lines` are the
/// context and `+` lines in order. Applying the chunk replaces the first
/// occurrence of `old_lines` (searching forward from the previous chunk) with
/// `new_lines`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdateChunk {
    /// Text after `@@` (usually zero or one entry; consecutive `@@` lines
    /// stack, e.g. a class header then a method header). Each header is sought
    /// forward before the chunk's lines are matched.
    pub headers: Vec<String>,
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    /// `*** End of File` followed the chunk: it must match at the end of the file.
    pub is_eof: bool,
}

impl UpdateChunk {
    fn has_lines(&self) -> bool {
        !self.old_lines.is_empty() || !self.new_lines.is_empty()
    }
}

/// Parse patch text.
///
/// Leniency: a missing `*** Begin Patch` (when the text starts with a file
/// operation) or `*** End Patch`, `apply_patch <<'EOF'` heredoc / PowerShell
/// here-string wrappers, Markdown code fences, CRLF line endings, a BOM and
/// leading/trailing blank lines are all tolerated. Unprefixed lines inside an
/// update section are treated as context lines; blank lines between change
/// lines are treated as empty context lines.
pub fn parse_patch(text: &str) -> Result<Patch, PatchError> {
    let body = normalize_patch_text(text, 0);
    let lines: Vec<&str> = body.split('\n').collect();
    Parser { lines, idx: 0 }.parse()
}

/// Strip wrappers (BOM, CRLF, heredoc, code fences, surrounding whitespace).
fn normalize_patch_text(text: &str, depth: u8) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text).replace("\r\n", "\n");
    let mut trimmed = text.trim();

    // Markdown code fences: ```diff ... ```
    if trimmed.starts_with("```") {
        if let Some(first_nl) = trimmed.find('\n') {
            let inner = &trimmed[first_nl + 1..];
            let inner = inner.trim_end();
            if let Some(stripped) = inner.strip_suffix("```") {
                trimmed = stripped.trim();
            }
        }
    }

    // Shell wrappers: `apply_patch <<'EOF'`, `bash -lc "apply_patch ..."`, ...
    let first_line = trimmed.lines().next().unwrap_or("").trim_start();
    if depth < 2 && !first_line.starts_with("***") {
        if let Some(extracted) = extract_patch_from_command(trimmed) {
            return normalize_patch_text(&extracted.patch, depth + 1);
        }
    }
    trimmed.to_string()
}

fn is_file_op_header(trimmed: &str) -> bool {
    trimmed.starts_with(ADD_FILE) || trimmed.starts_with(DELETE_FILE) || trimmed.starts_with(UPDATE_FILE)
}

/// Lines that end the body of an Add/Update/Delete section. Only lines
/// without leading whitespace qualify, so a context line can never be
/// mistaken for a marker.
fn is_section_end(line: &str) -> bool {
    let line = line.trim_end();
    line == END_PATCH || line == BEGIN_PATCH || is_file_op_header(line)
}

fn parse_path(raw: &str, line: usize) -> Result<String, PatchError> {
    let mut path = raw.trim();
    for quote in ['"', '\'', '`'] {
        if path.len() >= 2 && path.starts_with(quote) && path.ends_with(quote) {
            path = path[1..path.len() - 1].trim();
            break;
        }
    }
    if path.is_empty() {
        return Err(PatchError::parse(line, "missing file path after the section marker"));
    }
    Ok(path.to_string())
}

/// Interpret the text after `@@`. Unified-diff style ranges (`-1,3 +1,4 @@`)
/// are dropped; a trailing `@@` is removed.
fn parse_chunk_header(line: &str) -> Option<String> {
    let mut rest = line.trim_end().strip_prefix("@@")?.trim();
    if let Some(stripped) = rest.strip_suffix("@@") {
        rest = stripped.trim();
    }
    // `-12,3 +12,4 @@ fn foo()` -> `fn foo()`
    if rest.starts_with('-') {
        let mut parts = rest.splitn(3, ' ');
        let old = parts.next().unwrap_or("");
        let new = parts.next().unwrap_or("");
        let is_range = |s: &str, sign: char| {
            s.strip_prefix(sign).is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit() || c == ','))
        };
        if is_range(old, '-') && is_range(new, '+') {
            let tail = parts.next().unwrap_or("").trim();
            let tail = tail.strip_prefix("@@").unwrap_or(tail).trim();
            return if tail.is_empty() { None } else { Some(tail.to_string()) };
        }
    }
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

struct Parser<'a> {
    lines: Vec<&'a str>,
    idx: usize,
}

impl<'a> Parser<'a> {
    fn current(&self) -> Option<&'a str> {
        self.lines.get(self.idx).copied()
    }

    /// 1-based line number of the current position.
    fn line_no(&self) -> usize {
        self.idx + 1
    }

    fn skip_blank(&mut self) {
        while self.current().is_some_and(|l| l.trim().is_empty()) {
            self.idx += 1;
        }
    }

    fn parse(mut self) -> Result<Patch, PatchError> {
        self.skip_blank();
        let Some(first) = self.current() else {
            return Err(PatchError::parse(1, "patch is empty"));
        };
        let first = first.trim();
        if first == BEGIN_PATCH {
            self.idx += 1;
        } else if !is_file_op_header(first) {
            return Err(PatchError::parse(
                self.line_no(),
                format!("expected '{BEGIN_PATCH}' as the first line, found '{first}'"),
            ));
        }

        let mut ops = Vec::new();
        loop {
            self.skip_blank();
            let Some(line) = self.current() else { break };
            let trimmed = line.trim();
            let line_no = self.line_no();
            if trimmed == END_PATCH {
                break;
            } else if trimmed == BEGIN_PATCH {
                // Duplicated begin marker: harmless.
                self.idx += 1;
            } else if let Some(rest) = trimmed.strip_prefix(ADD_FILE) {
                let path = parse_path(rest, line_no)?;
                self.idx += 1;
                ops.push(self.parse_add(path)?);
            } else if let Some(rest) = trimmed.strip_prefix(DELETE_FILE) {
                let path = parse_path(rest, line_no)?;
                self.idx += 1;
                self.skip_delete_body()?;
                ops.push(FileOp::Delete { path });
            } else if let Some(rest) = trimmed.strip_prefix(UPDATE_FILE) {
                let path = parse_path(rest, line_no)?;
                self.idx += 1;
                ops.push(self.parse_update(path)?);
            } else {
                return Err(PatchError::parse(
                    line_no,
                    format!(
                        "unexpected line '{trimmed}': expected '{ADD_FILE} <path>', '{UPDATE_FILE} <path>', \
                         '{DELETE_FILE} <path>' or '{END_PATCH}'"
                    ),
                ));
            }
        }

        if ops.is_empty() {
            return Err(PatchError::parse(self.line_no().min(self.lines.len()), "patch contains no file operations"));
        }
        Ok(Patch { ops })
    }

    fn parse_add(&mut self, path: String) -> Result<FileOp, PatchError> {
        let mut content: Vec<&str> = Vec::new();
        let mut pending_blank = 0usize;
        while let Some(line) = self.current() {
            if is_section_end(line) {
                break;
            }
            if let Some(rest) = line.strip_prefix('+') {
                content.resize(content.len() + pending_blank, "");
                pending_blank = 0;
                content.push(rest);
            } else if line.trim().is_empty() {
                pending_blank += 1;
            } else {
                return Err(PatchError::parse(
                    self.line_no(),
                    format!("every line of an '{ADD_FILE} {path}' section must start with '+', found '{line}'"),
                ));
            }
            self.idx += 1;
        }
        let contents = if content.is_empty() { String::new() } else { content.join("\n") + "\n" };
        Ok(FileOp::Add { path, contents })
    }

    /// Models sometimes echo the removed contents after a delete marker;
    /// `-` lines and blank lines are skipped.
    fn skip_delete_body(&mut self) -> Result<(), PatchError> {
        while let Some(line) = self.current() {
            if is_section_end(line) {
                break;
            }
            if !(line.starts_with('-') || line.trim().is_empty()) {
                return Err(PatchError::parse(
                    self.line_no(),
                    format!("unexpected line after '{DELETE_FILE}': '{line}'"),
                ));
            }
            self.idx += 1;
        }
        Ok(())
    }

    fn parse_update(&mut self, path: String) -> Result<FileOp, PatchError> {
        let section_line = self.line_no() - 1;
        let mut move_to = None;
        if let Some(line) = self.current() {
            if let Some(rest) = line.trim().strip_prefix(MOVE_TO) {
                move_to = Some(parse_path(rest, self.line_no())?);
                self.idx += 1;
            }
        }

        let mut chunks: Vec<UpdateChunk> = Vec::new();
        let mut current = UpdateChunk::default();
        let mut current_start = self.line_no();
        let mut pending_blank = 0usize;

        while let Some(line) = self.current() {
            if is_section_end(line) {
                break;
            }
            let line_no = self.line_no();
            let trimmed_end = line.trim_end();
            if trimmed_end == END_OF_FILE {
                pending_blank = 0;
                if current.has_lines() {
                    current.is_eof = true;
                    finish_chunk(&mut chunks, &mut current, current_start, &path)?;
                } else if current.headers.is_empty() && !chunks.is_empty() {
                    if let Some(last) = chunks.last_mut() {
                        last.is_eof = true;
                    }
                } else {
                    return Err(PatchError::parse(line_no, format!("'{END_OF_FILE}' must follow a chunk's lines")));
                }
                current_start = line_no + 1;
            } else if line.starts_with("@@") {
                pending_blank = 0;
                if current.has_lines() {
                    finish_chunk(&mut chunks, &mut current, current_start, &path)?;
                }
                if !current.has_lines() && current.headers.is_empty() {
                    current_start = line_no;
                }
                if let Some(header) = parse_chunk_header(line) {
                    current.headers.push(header);
                }
            } else if trimmed_end.starts_with("***") {
                return Err(PatchError::parse(line_no, format!("unknown marker '{trimmed_end}'")));
            } else if line.starts_with("\\ No newline at end of file") {
                // Unified-diff artifact; meaningless here.
            } else if line.trim().is_empty() && !line.starts_with(' ') {
                pending_blank += 1;
            } else {
                for _ in 0..pending_blank {
                    current.old_lines.push(String::new());
                    current.new_lines.push(String::new());
                }
                pending_blank = 0;
                if let Some(rest) = line.strip_prefix('-') {
                    current.old_lines.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix('+') {
                    current.new_lines.push(rest.to_string());
                } else {
                    // ' ' prefixed context, or (leniently) an unprefixed context line.
                    let rest = line.strip_prefix(' ').unwrap_or(line);
                    current.old_lines.push(rest.to_string());
                    current.new_lines.push(rest.to_string());
                }
            }
            self.idx += 1;
        }

        if current.has_lines() {
            finish_chunk(&mut chunks, &mut current, current_start, &path)?;
        } else if !current.headers.is_empty() {
            return Err(PatchError::parse(
                current_start,
                format!("chunk in '{UPDATE_FILE} {path}' has an '@@' header but no lines"),
            ));
        }

        if chunks.is_empty() && move_to.is_none() {
            return Err(PatchError::parse(
                section_line,
                format!(
                    "'{UPDATE_FILE} {path}' contains no changes (expected '@@' chunks with ' ', '-' and '+' lines)"
                ),
            ));
        }
        Ok(FileOp::Update { path, move_to, chunks })
    }
}

fn finish_chunk(
    chunks: &mut Vec<UpdateChunk>,
    current: &mut UpdateChunk,
    start_line: usize,
    path: &str,
) -> Result<(), PatchError> {
    let chunk = std::mem::take(current);
    if !chunk.has_lines() {
        return Err(PatchError::parse(start_line, format!("empty chunk in '{UPDATE_FILE} {path}'")));
    }
    chunks.push(chunk);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn chunk(headers: &[&str], old: &[&str], new: &[&str], eof: bool) -> UpdateChunk {
        UpdateChunk {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            old_lines: old.iter().map(|s| s.to_string()).collect(),
            new_lines: new.iter().map(|s| s.to_string()).collect(),
            is_eof: eof,
        }
    }

    const FULL: &str = "*** Begin Patch
*** Add File: path/new.txt
+line one
+line two
*** Delete File: path/old.txt
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
 context line
-old line
+new line
@@
 more context
-x
+y
*** End of File
*** End Patch";

    #[test]
    fn parses_full_example() {
        let patch = parse_patch(FULL).unwrap();
        assert_eq!(
            patch.ops,
            vec![
                FileOp::Add { path: "path/new.txt".into(), contents: "line one\nline two\n".into() },
                FileOp::Delete { path: "path/old.txt".into() },
                FileOp::Update {
                    path: "src/app.py".into(),
                    move_to: Some("src/main.py".into()),
                    chunks: vec![
                        chunk(&["def greet():"], &["context line", "old line"], &["context line", "new line"], false),
                        chunk(&[], &["more context", "x"], &["more context", "y"], true),
                    ],
                },
            ]
        );
    }

    #[test]
    fn crlf_and_bom_and_surrounding_whitespace() {
        let text = format!("\u{feff}\r\n\r\n   {}\r\n\r\n  ", FULL.replace('\n', "\r\n"));
        assert_eq!(parse_patch(&text).unwrap(), parse_patch(FULL).unwrap());
    }

    #[test]
    fn missing_begin_and_end_markers() {
        let text = "*** Update File: a.txt\n@@\n-a\n+b\n";
        let patch = parse_patch(text).unwrap();
        assert_eq!(
            patch.ops,
            vec![FileOp::Update {
                path: "a.txt".into(),
                move_to: None,
                chunks: vec![chunk(&[], &["a"], &["b"], false)]
            }]
        );
        let text = "*** Begin Patch\n*** Delete File: x\n";
        assert_eq!(parse_patch(text).unwrap().ops, vec![FileOp::Delete { path: "x".into() }]);
    }

    #[test]
    fn heredoc_wrapper_is_stripped() {
        let text = format!("apply_patch <<'EOF'\n{FULL}\nEOF\n");
        assert_eq!(parse_patch(&text).unwrap(), parse_patch(FULL).unwrap());
        let text = format!("apply_patch @'\n{FULL}\n'@");
        assert_eq!(parse_patch(&text).unwrap(), parse_patch(FULL).unwrap());
    }

    #[test]
    fn code_fence_wrapper_is_stripped() {
        let text = format!("```diff\n{FULL}\n```");
        assert_eq!(parse_patch(&text).unwrap(), parse_patch(FULL).unwrap());
    }

    #[test]
    fn first_chunk_without_header_marker() {
        let text = "*** Begin Patch\n*** Update File: a.txt\n foo\n-bar\n+baz\n*** End Patch";
        let patch = parse_patch(text).unwrap();
        let FileOp::Update { chunks, .. } = &patch.ops[0] else { panic!() };
        assert_eq!(chunks, &vec![chunk(&[], &["foo", "bar"], &["foo", "baz"], false)]);
    }

    #[test]
    fn stacked_headers_and_unified_ranges() {
        let text = "*** Begin Patch\n*** Update File: a.py\n@@ class A:\n@@     def f(self):\n-  pass\n+  return 1\n@@ -10,2 +10,2 @@ def g():\n-a\n+b\n@@ -1 +1 @@\n-c\n+d\n*** End Patch";
        let patch = parse_patch(text).unwrap();
        let FileOp::Update { chunks, .. } = &patch.ops[0] else { panic!() };
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].headers, vec!["class A:".to_string(), "def f(self):".to_string()]);
        assert_eq!(chunks[1].headers, vec!["def g():".to_string()]);
        assert!(chunks[2].headers.is_empty());
    }

    #[test]
    fn blank_lines_inside_chunk_become_context_and_trailing_blanks_are_dropped() {
        let text = "*** Begin Patch\n*** Update File: a.txt\n@@\n a\n\n-b\n+c\n\n\n*** Update File: b.txt\n@@\n-x\n+y\n*** End Patch";
        let patch = parse_patch(text).unwrap();
        let FileOp::Update { chunks, .. } = &patch.ops[0] else { panic!() };
        assert_eq!(chunks, &vec![chunk(&[], &["a", "", "b"], &["a", "", "c"], false)]);
        assert_eq!(patch.ops.len(), 2);
    }

    #[test]
    fn unprefixed_lines_are_context() {
        let text = "*** Begin Patch\n*** Update File: a.txt\n@@\nfoo\n-bar\n+baz\n*** End Patch";
        let patch = parse_patch(text).unwrap();
        let FileOp::Update { chunks, .. } = &patch.ops[0] else { panic!() };
        assert_eq!(chunks, &vec![chunk(&[], &["foo", "bar"], &["foo", "baz"], false)]);
    }

    #[test]
    fn add_file_blank_lines_and_empty_file() {
        let text = "*** Begin Patch\n*** Add File: a.txt\n+one\n\n+three\n\n*** Add File: empty.txt\n*** End Patch";
        let patch = parse_patch(text).unwrap();
        assert_eq!(
            patch.ops,
            vec![
                FileOp::Add { path: "a.txt".into(), contents: "one\n\nthree\n".into() },
                FileOp::Add { path: "empty.txt".into(), contents: String::new() },
            ]
        );
    }

    #[test]
    fn add_file_rejects_unprefixed_lines() {
        let err = parse_patch("*** Begin Patch\n*** Add File: a.txt\n+ok\nnot ok\n*** End Patch").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("line 4"), "{msg}");
        assert!(msg.contains("must start with '+'"), "{msg}");
    }

    #[test]
    fn quoted_paths_are_unquoted() {
        let patch = parse_patch("*** Begin Patch\n*** Delete File: \"dir/my file.txt\"\n*** End Patch").unwrap();
        assert_eq!(patch.ops, vec![FileOp::Delete { path: "dir/my file.txt".into() }]);
    }

    #[test]
    fn pure_move_is_allowed() {
        let patch = parse_patch("*** Begin Patch\n*** Update File: a\n*** Move to: b\n*** End Patch").unwrap();
        assert_eq!(patch.ops, vec![FileOp::Update { path: "a".into(), move_to: Some("b".into()), chunks: vec![] }]);
    }

    #[test]
    fn delete_body_minus_lines_are_skipped() {
        let patch = parse_patch("*** Begin Patch\n*** Delete File: a\n-old\n-content\n*** End Patch").unwrap();
        assert_eq!(patch.ops, vec![FileOp::Delete { path: "a".into() }]);
    }

    #[test]
    fn error_cases() {
        let cases: &[(&str, &str)] = &[
            ("", "patch is empty"),
            ("hello world", "expected '*** Begin Patch'"),
            ("*** Begin Patch\n*** End Patch", "no file operations"),
            ("*** Begin Patch\n*** Frobnicate: x\n*** End Patch", "unexpected line"),
            ("*** Begin Patch\n*** Update File: a\n*** End Patch", "contains no changes"),
            ("*** Begin Patch\n*** Update File: a\n@@ foo\n*** End Patch", "header but no lines"),
            ("*** Begin Patch\n*** Update File: a\n*** End of File\n*** End Patch", "must follow"),
            ("*** Begin Patch\n*** Add File:   \n+x\n*** End Patch", "missing file path"),
            ("*** Begin Patch\n*** Update File: a\n@@\n-a\n*** Bogus\n*** End Patch", "unknown marker"),
            ("*** Begin Patch\n*** Delete File: a\nstray\n*** End Patch", "unexpected line after"),
        ];
        for (text, needle) in cases {
            let err = parse_patch(text).unwrap_err();
            assert!(matches!(err, PatchError::Parse { .. }), "{text:?}");
            assert!(err.to_string().contains(needle), "{text:?} -> {err}");
        }
    }

    #[test]
    fn header_parsing() {
        assert_eq!(parse_chunk_header("@@"), None);
        assert_eq!(parse_chunk_header("@@   "), None);
        assert_eq!(parse_chunk_header("@@ def f():"), Some("def f():".into()));
        assert_eq!(parse_chunk_header("@@ fn main() @@"), Some("fn main()".into()));
        assert_eq!(parse_chunk_header("@@ -1,2 +1,3 @@"), None);
        assert_eq!(parse_chunk_header("@@ -1,2 +1,3 @@ impl Foo"), Some("impl Foo".into()));
        assert_eq!(parse_chunk_header("@@ -x is a flag"), Some("-x is a flag".into()));
    }

    #[test]
    fn eof_marker_after_blank_line_applies_to_previous_chunk() {
        let text = "*** Begin Patch\n*** Update File: a\n@@\n-a\n+b\n\n*** End of File\n*** End Patch";
        let patch = parse_patch(text).unwrap();
        let FileOp::Update { chunks, .. } = &patch.ops[0] else { panic!() };
        assert!(chunks[0].is_eof);
    }

    #[test]
    fn content_after_end_patch_is_ignored() {
        let text = "*** Begin Patch\n*** Delete File: a\n*** End Patch\ngarbage here";
        assert_eq!(parse_patch(text).unwrap().ops.len(), 1);
    }
}
