//! ripgrep-style content search.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::overrides::OverrideBuilder;
use ignore::types::TypesBuilder;
use ignore::WalkState;

use crate::walk::{is_file, walker};
use crate::Error;

/// Longest line (in chars) echoed back in content mode.
const MAX_LINE_CHARS: usize = 1_000;

/// What [`grep`] reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GrepMode {
    /// `path:line:content` for each matching line (plus context lines).
    #[default]
    Content,
    /// One path per matching file.
    FilesWithMatches,
    /// `path:count` per matching file.
    Count,
}

/// Options for [`grep`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepOptions {
    /// Regular expression (Rust regex syntax), or a literal with `fixed_strings`.
    pub pattern: String,
    /// File or directory to search.
    pub path: PathBuf,
    /// Glob filter, e.g. `*.rs` or `src/**/*.{ts,tsx}`; prefix with `!` to exclude.
    pub glob: Option<String>,
    /// File type from ripgrep's default type list, e.g. `rust`, `ts`, `py`.
    pub file_type: Option<String>,
    pub case_insensitive: bool,
    pub fixed_strings: bool,
    /// Let matches span lines (`.` also matches newlines).
    pub multiline: bool,
    pub context_before: usize,
    pub context_after: usize,
    /// Cap on reported results (matching lines in content mode, files otherwise). `0` = no cap.
    pub max_results: usize,
    pub mode: GrepMode,
    /// Search hidden files and directories too (`.git` is always skipped).
    pub include_hidden: bool,
}

impl Default for GrepOptions {
    fn default() -> Self {
        GrepOptions {
            pattern: String::new(),
            path: PathBuf::from("."),
            glob: None,
            file_type: None,
            case_insensitive: false,
            fixed_strings: false,
            multiline: false,
            context_before: 0,
            context_after: 0,
            max_results: 0,
            mode: GrepMode::Content,
            include_hidden: false,
        }
    }
}

impl GrepOptions {
    /// Options for `pattern` under `path` with defaults for everything else.
    pub fn new(pattern: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        GrepOptions { pattern: pattern.into(), path: path.into(), ..Default::default() }
    }
}

/// Output of [`grep`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrepResult {
    /// ripgrep-like text output (see [`GrepMode`]). Files are sorted by path.
    pub text: String,
    /// Matching lines (content mode), total count (count mode) or files (files mode).
    pub matches: usize,
    /// Files with at least one reported match.
    pub files: usize,
    /// Whether `max_results` cut the output short.
    pub truncated: bool,
}

#[derive(Debug)]
enum Out {
    /// First line number and the line(s) of one match (several in multiline mode).
    Match(u64, Vec<String>),
    Context(u64, String),
    Break,
}

#[derive(Debug)]
struct FileHits {
    display: String,
    lines: Vec<Out>,
    /// Matches in this file (1 in files mode).
    count: usize,
}

/// Shared state for the parallel search.
struct Shared {
    total: AtomicUsize,
    limit: usize,
    stop: AtomicBool,
    truncated: AtomicBool,
}

impl Shared {
    /// Reserve a result slot; false (and stop everything) once the cap is exceeded.
    fn reserve(&self) -> bool {
        if self.limit == 0 {
            return true;
        }
        let n = self.total.fetch_add(1, Ordering::Relaxed) + 1;
        if n > self.limit {
            self.truncated.store(true, Ordering::Relaxed);
            self.stop.store(true, Ordering::Relaxed);
            false
        } else {
            true
        }
    }
}

struct HitSink<'a> {
    mode: GrepMode,
    shared: &'a Shared,
    lines: Vec<Out>,
    count: usize,
}

fn clean_line(bytes: &[u8]) -> String {
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r') {
        end -= 1;
    }
    let text = String::from_utf8_lossy(&bytes[..end]);
    let total = text.chars().count();
    if total > MAX_LINE_CHARS {
        let kept: String = text.chars().take(MAX_LINE_CHARS).collect();
        format!("{kept}… [{} chars]", total - MAX_LINE_CHARS)
    } else {
        text.into_owned()
    }
}

impl Sink for HitSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if self.shared.stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        match self.mode {
            GrepMode::Content => {
                // In content mode the cap applies to matches, so reserve each one.
                if !self.shared.reserve() {
                    return Ok(false);
                }
                self.count += 1;
                let first = mat.line_number().unwrap_or(0);
                self.lines.push(Out::Match(first, mat.lines().map(clean_line).collect()));
                Ok(true)
            }
            GrepMode::Count => {
                self.count += 1;
                Ok(true)
            }
            GrepMode::FilesWithMatches => {
                self.count = 1;
                Ok(false)
            }
        }
    }

    fn context(&mut self, _searcher: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, Self::Error> {
        if self.mode == GrepMode::Content {
            self.lines.push(Out::Context(ctx.line_number().unwrap_or(0), clean_line(ctx.bytes())));
        }
        Ok(true)
    }

    fn context_break(&mut self, _searcher: &Searcher) -> Result<bool, Self::Error> {
        if self.mode == GrepMode::Content {
            self.lines.push(Out::Break);
        }
        Ok(true)
    }
}

fn build_matcher(opts: &GrepOptions) -> Result<RegexMatcher, Error> {
    if opts.pattern.is_empty() {
        return Err(Error::InvalidPattern("pattern is empty".into()));
    }
    let mut b = RegexMatcherBuilder::new();
    // `crlf` makes `$` match before `\r\n`; it also sets a CRLF line terminator, which is then
    // replaced: none in multiline mode, plain `\n` (the searcher's terminator) otherwise.
    b.case_insensitive(opts.case_insensitive).fixed_strings(opts.fixed_strings).crlf(true);
    if opts.multiline {
        b.multi_line(true).dot_matches_new_line(true).line_terminator(None);
    } else {
        b.line_terminator(Some(b'\n'));
    }
    b.build(&opts.pattern).map_err(|e| Error::InvalidPattern(e.to_string()))
}

fn display_path(root: &Path, path: &Path) -> String {
    if root == Path::new(".") {
        if let Ok(rel) = path.strip_prefix(".") {
            return rel.to_string_lossy().into_owned();
        }
    }
    path.to_string_lossy().into_owned()
}

/// Search file contents under `opts.path`.
pub fn grep(opts: &GrepOptions) -> Result<GrepResult, Error> {
    if !opts.path.exists() {
        return Err(Error::NotFound(opts.path.clone()));
    }
    let matcher = build_matcher(opts)?;

    let mut wb = walker(&opts.path, opts.include_hidden);
    if let Some(ty) = opts.file_type.as_deref().filter(|t| !t.trim().is_empty()) {
        let mut tb = TypesBuilder::new();
        tb.add_defaults();
        for name in ty.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            tb.select(name);
        }
        let types = tb.build().map_err(|e| Error::InvalidArgument(format!("unknown file type {ty:?}: {e}")))?;
        wb.types(types);
    }
    if let Some(glob) = opts.glob.as_deref().filter(|g| !g.trim().is_empty()) {
        let base = if opts.path.is_dir() { opts.path.as_path() } else { opts.path.parent().unwrap_or(Path::new(".")) };
        let mut ob = OverrideBuilder::new(base);
        for g in glob.split_whitespace() {
            ob.add(g).map_err(|e| Error::InvalidPattern(format!("glob {g:?}: {e}")))?;
        }
        let overrides = ob.build().map_err(|e| Error::InvalidPattern(e.to_string()))?;
        wb.overrides(overrides);
    }

    let mut sb = SearcherBuilder::new();
    sb.line_number(true).multi_line(opts.multiline).binary_detection(BinaryDetection::quit(b'\x00')).bom_sniffing(true);
    if opts.mode == GrepMode::Content {
        sb.before_context(opts.context_before).after_context(opts.context_after);
    }

    let shared = Shared {
        total: AtomicUsize::new(0),
        limit: opts.max_results,
        stop: AtomicBool::new(false),
        truncated: AtomicBool::new(false),
    };
    let results: Mutex<Vec<FileHits>> = Mutex::new(Vec::new());

    wb.build_parallel().run(|| {
        let matcher = matcher.clone();
        let mut searcher = sb.build();
        let shared = &shared;
        let results = &results;
        let root = opts.path.clone();
        let mode = opts.mode;
        Box::new(move |entry| {
            if shared.stop.load(Ordering::Relaxed) {
                return WalkState::Quit;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return WalkState::Continue,
            };
            if !is_file(&entry) {
                return WalkState::Continue;
            }
            let mut sink = HitSink { mode, shared, lines: Vec::new(), count: 0 };
            if searcher.search_path(&matcher, entry.path(), &mut sink).is_err() {
                return WalkState::Continue;
            }
            // Outside content mode the cap applies to files.
            if sink.count > 0 && (mode == GrepMode::Content || shared.reserve()) {
                let hits =
                    FileHits { display: display_path(&root, entry.path()), lines: sink.lines, count: sink.count };
                if let Ok(mut r) = results.lock() {
                    r.push(hits);
                }
            }
            if shared.stop.load(Ordering::Relaxed) {
                WalkState::Quit
            } else {
                WalkState::Continue
            }
        })
    });

    let mut files = results.into_inner().unwrap_or_default();
    files.sort_by(|a, b| a.display.cmp(&b.display));
    Ok(render(files, opts, shared.truncated.load(Ordering::Relaxed)))
}

fn render(files: Vec<FileHits>, opts: &GrepOptions, mut truncated: bool) -> GrepResult {
    let limit = if opts.max_results == 0 { usize::MAX } else { opts.max_results };
    let with_context = opts.context_before > 0 || opts.context_after > 0;
    let mut text = String::new();
    // `reported` is what the cap applies to: matches in content mode, files otherwise.
    let mut reported = 0usize;
    let mut matches = 0usize;
    let mut file_count = 0usize;
    for file in files {
        if reported >= limit {
            truncated = true;
            break;
        }
        match opts.mode {
            GrepMode::FilesWithMatches => {
                text.push_str(&file.display);
                text.push('\n');
                reported += 1;
                matches += 1;
            }
            GrepMode::Count => {
                text.push_str(&format!("{}:{}\n", file.display, file.count));
                reported += 1;
                matches += file.count;
            }
            GrepMode::Content => {
                if with_context && file_count > 0 {
                    text.push_str("--\n");
                }
                for line in file.lines {
                    match line {
                        Out::Match(first, lines) => {
                            if reported >= limit {
                                truncated = true;
                                break;
                            }
                            reported += 1;
                            matches += 1;
                            for (i, s) in lines.iter().enumerate() {
                                text.push_str(&format!("{}:{}:{s}\n", file.display, first + i as u64));
                            }
                        }
                        Out::Context(n, s) => text.push_str(&format!("{}-{n}-{s}\n", file.display)),
                        Out::Break => text.push_str("--\n"),
                    }
                }
            }
        }
        file_count += 1;
    }
    // Drop trailing separators left after a truncation point.
    while text.ends_with("--\n") {
        text.truncate(text.len() - 3);
    }
    GrepResult { text, matches, files: file_count, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        fs::create_dir_all(r.join("src")).unwrap();
        fs::create_dir_all(r.join("web")).unwrap();
        fs::create_dir_all(r.join("ignored")).unwrap();
        fs::create_dir_all(r.join(".hidden")).unwrap();
        fs::write(r.join("src/main.rs"), "fn main() {\n    let x = 1;\n    println!(\"Hello\");\n    let y = 2;\n}\n")
            .unwrap();
        fs::write(r.join("src/lib.rs"), "pub fn hello() {}\n// TODO: more\n").unwrap();
        fs::write(r.join("web/app.ts"), "export const hello = 1;\r\nconsole.log(hello);\r\n").unwrap();
        fs::write(r.join("ignored/skip.rs"), "fn hello() {}\n").unwrap();
        fs::write(r.join(".hidden/secret.rs"), "fn hello() {}\n").unwrap();
        fs::write(r.join("bin.dat"), b"hello\x00world").unwrap();
        fs::write(r.join(".gitignore"), "ignored/\n").unwrap();
        dir
    }

    fn opts(dir: &tempfile::TempDir, pattern: &str) -> GrepOptions {
        GrepOptions::new(pattern, dir.path())
    }

    fn rel(dir: &tempfile::TempDir, text: &str) -> String {
        let prefix = format!("{}{}", dir.path().display(), std::path::MAIN_SEPARATOR);
        text.replace(&prefix, "").replace('\\', "/")
    }

    #[test]
    fn content_mode_case_and_ignores() {
        let dir = fixture();
        let res = grep(&opts(&dir, "hello")).unwrap();
        assert_eq!(
            rel(&dir, &res.text),
            "src/lib.rs:1:pub fn hello() {}\nweb/app.ts:1:export const hello = 1;\nweb/app.ts:2:console.log(hello);\n"
        );
        assert_eq!(res.matches, 3);
        assert_eq!(res.files, 2);
        assert!(!res.truncated);

        let mut o = opts(&dir, "hello");
        o.case_insensitive = true;
        o.include_hidden = true;
        let res = grep(&o).unwrap();
        let text = rel(&dir, &res.text);
        assert!(text.contains("src/main.rs:3:    println!(\"Hello\");"));
        assert!(text.contains(".hidden/secret.rs:1:"));
        assert!(!text.contains("ignored/"), "gitignored files are skipped");
        assert!(!text.contains("bin.dat"), "binary files are skipped");
    }

    #[test]
    fn files_and_count_modes() {
        let dir = fixture();
        let mut o = opts(&dir, "hello");
        o.mode = GrepMode::FilesWithMatches;
        let res = grep(&o).unwrap();
        assert_eq!(rel(&dir, &res.text), "src/lib.rs\nweb/app.ts\n");
        assert_eq!(res.files, 2);

        o.mode = GrepMode::Count;
        let res = grep(&o).unwrap();
        assert_eq!(rel(&dir, &res.text), "src/lib.rs:1\nweb/app.ts:2\n");
        assert_eq!(res.matches, 3);
    }

    #[test]
    fn context_lines_and_separators() {
        let dir = fixture();
        let mut o = opts(&dir, "let");
        o.path = dir.path().join("src/main.rs");
        o.context_before = 1;
        let res = grep(&o).unwrap();
        let text = rel(&dir, &res.text);
        assert_eq!(
            text,
            "src/main.rs-1-fn main() {\nsrc/main.rs:2:    let x = 1;\nsrc/main.rs-3-    println!(\"Hello\");\nsrc/main.rs:4:    let y = 2;\n"
        );

        let mut o = opts(&dir, "x = 1|y = 2");
        o.path = dir.path().join("src");
        let res = grep(&o).unwrap();
        assert_eq!(res.matches, 2);

        // Non-adjacent groups get a `--` break.
        let mut o = opts(&dir, "fn main|y = 2");
        o.path = dir.path().join("src/main.rs");
        let res = grep(&o).unwrap();
        assert!(!res.text.contains("--"));
        o.context_after = 1;
        let text = rel(&dir, &grep(&o).unwrap().text);
        assert_eq!(text, "src/main.rs:1:fn main() {\nsrc/main.rs-2-    let x = 1;\n--\nsrc/main.rs:4:    let y = 2;\nsrc/main.rs-5-}\n");
    }

    #[test]
    fn glob_type_fixed_strings_and_multiline() {
        let dir = fixture();
        let mut o = opts(&dir, "hello");
        o.glob = Some("*.ts".into());
        assert_eq!(grep(&o).unwrap().files, 1);
        o.glob = Some("!*.ts".into());
        assert_eq!(rel(&dir, &grep(&o).unwrap().text), "src/lib.rs:1:pub fn hello() {}\n");
        o.glob = None;
        o.file_type = Some("rust".into());
        assert_eq!(grep(&o).unwrap().files, 1);
        o.file_type = Some("no-such-type".into());
        assert!(matches!(grep(&o), Err(Error::InvalidArgument(_))));

        let mut o = opts(&dir, "println!(");
        assert!(matches!(grep(&o), Err(Error::InvalidPattern(_))));
        o.fixed_strings = true;
        assert_eq!(grep(&o).unwrap().matches, 1);

        let mut o = opts(&dir, r"x = 1;\s+println");
        assert_eq!(grep(&o).unwrap().matches, 0);
        o.multiline = true;
        let res = grep(&o).unwrap();
        assert_eq!(res.matches, 1);
        assert_eq!(rel(&dir, &res.text), "src/main.rs:2:    let x = 1;\nsrc/main.rs:3:    println!(\"Hello\");\n");
    }

    #[test]
    fn max_results_truncates() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fs::write(dir.path().join(format!("f{i}.txt")), "needle\nneedle\n").unwrap();
        }
        let mut o = GrepOptions::new("needle", dir.path());
        o.max_results = 3;
        let res = grep(&o).unwrap();
        assert_eq!(res.matches, 3);
        assert!(res.truncated);
        assert_eq!(res.text.lines().count(), 3);

        o.mode = GrepMode::FilesWithMatches;
        o.max_results = 2;
        let res = grep(&o).unwrap();
        assert_eq!(res.files, 2);
        assert!(res.truncated);

        o.max_results = 10;
        let res = grep(&o).unwrap();
        assert_eq!(res.files, 5);
        assert!(!res.truncated);
        assert!(matches!(grep(&GrepOptions::new("x", dir.path().join("missing"))), Err(Error::NotFound(_))));
    }
}
