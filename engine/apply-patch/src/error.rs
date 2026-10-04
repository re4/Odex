//! Error types. Messages are written to be read by the model, so they say
//! which file and chunk failed and what the file actually contains.

use std::fmt;
use std::io;

/// Maximum number of lines echoed back in an error message per block.
const MAX_ECHO_LINES: usize = 40;

/// The region of the file that most resembles the lines a chunk expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosestMatch {
    /// 1-based line number where the partial match starts.
    pub line: usize,
    /// How many of the expected (non-blank) lines matched there, ignoring
    /// whitespace and punctuation differences.
    pub matched: usize,
    /// The actual file lines at that position.
    pub snippet: Vec<String>,
}

/// Details of a chunk whose context / removed lines could not be located.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMismatch {
    /// Path as shown to the model (relative to the working directory when possible).
    pub path: String,
    /// 1-based chunk index within the `*** Update File` section.
    pub chunk: usize,
    pub chunk_count: usize,
    /// `@@` headers the chunk carried.
    pub headers: Vec<String>,
    /// Context and `-` lines that were searched for.
    pub expected: Vec<String>,
    /// Whether the chunk was anchored with `*** End of File`.
    pub is_eof: bool,
    /// 1-based line where the search started (just after the previous chunk).
    pub searched_from: usize,
    pub closest: Option<ClosestMatch>,
}

impl fmt::Display for ContextMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: chunk {} of {} failed to apply: could not find the expected lines",
            self.path, self.chunk, self.chunk_count
        )?;
        if !self.headers.is_empty() {
            let headers: Vec<String> = self.headers.iter().map(|h| format!("`@@ {h}`")).collect();
            write!(f, " after {}", headers.join(" then "))?;
        }
        if self.is_eof {
            write!(f, " at the end of the file")?;
        }
        if self.chunk > 1 {
            write!(f, " (searching from line {}, after chunk {})", self.searched_from, self.chunk - 1)?;
        }
        writeln!(f, ":")?;
        write_block(f, self.expected.iter().map(|l| format!("    {l}")))?;
        match &self.closest {
            Some(closest) => {
                let non_blank = self.expected.iter().filter(|l| !l.trim().is_empty()).count();
                writeln!(
                    f,
                    "Closest partial match starts at line {} ({} of {} lines similar). The file actually contains:",
                    closest.line, closest.matched, non_blank
                )?;
                write_block(
                    f,
                    closest.snippet.iter().enumerate().map(|(i, l)| format!("{:>6} | {l}", closest.line + i)),
                )?;
            }
            None => writeln!(f, "No similar region was found in the file.")?,
        }
        if self.closest.as_ref().is_some_and(|c| c.line < self.searched_from) {
            writeln!(f, "Chunks must appear in the same order as the code they change in the file.")?;
        }
        write!(f, "Context and '-' lines must match the current file contents. Re-read the file and resend the patch.")
    }
}

fn write_block(f: &mut fmt::Formatter<'_>, lines: impl ExactSizeIterator<Item = String>) -> fmt::Result {
    let total = lines.len();
    for line in lines.take(MAX_ECHO_LINES) {
        writeln!(f, "{line}")?;
    }
    if total > MAX_ECHO_LINES {
        writeln!(f, "    ... ({} more lines)", total - MAX_ECHO_LINES)?;
    }
    Ok(())
}

/// Everything that can go wrong while parsing or applying a patch.
///
/// Note that `*** Add File` on a path that already exists is **not** an error:
/// the file is overwritten (its BOM and line-ending style are kept).
#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    /// The patch text is malformed. `line` is 1-based within the patch body.
    #[error("invalid patch at line {line}: {message}")]
    Parse { line: usize, message: String },

    /// `*** Update File` / `*** Delete File` named a file that does not exist.
    #[error("{path}: file not found (cannot {action} a file that does not exist)")]
    FileNotFound { path: String, action: &'static str },

    /// The path points at a directory.
    #[error("{path}: is a directory, not a file")]
    IsDirectory { path: String },

    /// The file is not UTF-8; only text files can be patched.
    #[error("{path}: file is not valid UTF-8 text; apply_patch can only edit text files")]
    NotUtf8 { path: String },

    /// A chunk's context / removed lines were not found.
    #[error("{0}")]
    ContextNotFound(Box<ContextMismatch>),

    /// A pure-insertion chunk's `@@` header line was not found.
    #[error("{path}: chunk {chunk} of {chunk_count} failed to apply: could not find a line matching the `@@ {header}` header")]
    HeaderNotFound { path: String, chunk: usize, chunk_count: usize, header: String },

    /// A filesystem operation failed.
    #[error("{path}: failed to {action}: {source}")]
    Io {
        path: String,
        action: &'static str,
        #[source]
        source: io::Error,
    },
}

impl PatchError {
    pub(crate) fn parse(line: usize, message: impl Into<String>) -> Self {
        PatchError::Parse { line, message: message.into() }
    }
}
