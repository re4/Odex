//! `odex-apply-patch`: the agent's `apply_patch` tool.
//!
//! Patch format:
//!
//! ```text
//! *** Begin Patch
//! *** Add File: path/new.txt
//! +line one
//! *** Delete File: path/old.txt
//! *** Update File: src/app.py
//! *** Move to: src/main.py
//! @@ def greet():
//!  context line
//! -old line
//! +new line
//! *** End of File
//! *** End Patch
//! ```
//!
//! * [`parse_patch`] turns text into a [`Patch`] (leniently: heredoc
//!   wrappers, missing markers, CRLF, ...).
//! * [`extract_patch_from_command`] / [`extract_patch_from_argv`] recognise
//!   `apply_patch` invocations sent through a shell.
//! * [`preview`] computes every change (contents, unified diff, counts)
//!   without writing; [`apply`] does the same and then writes atomically
//!   with respect to chunk failures.
//!
//! Context matching tries, in order: exact, ignoring trailing whitespace,
//! ignoring surrounding whitespace, and Unicode punctuation normalisation.
//! Line endings, a UTF-8 BOM and the presence/absence of a final newline are
//! preserved.

mod apply;
mod error;
mod extract;
mod parser;
mod seek;
mod text;

pub use apply::{apply, apply_patch_text, preview, summarize, ChangeKind, FileChangePreview};
pub use error::{ClosestMatch, ContextMismatch, PatchError};
pub use extract::{extract_patch_from_argv, extract_patch_from_command, ExtractedPatch};
pub use parser::{parse_patch, FileOp, Patch, UpdateChunk, BEGIN_PATCH, END_PATCH};
