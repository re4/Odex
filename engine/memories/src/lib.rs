//! Odex memories: opt-in, user-reviewed facts stored as local JSON files.
//!
//! Layout under the store directory:
//! - `global.json` + `MEMORIES.md`
//! - `projects/<slug>-<hash>.json` + `projects/<slug>-<hash>.MEMORIES.md`
//!
//! Each JSON file is `{ "project_path": <string|null>, "memories": [Memory...] }`.
//! The Markdown files are regenerated on every write for humans to read; they
//! are never parsed back.

mod proposals;
mod redact;
mod store;

pub use proposals::{parse_proposals, proposal_request, ProposalRequest, MAX_PROPOSALS};
pub use redact::redact_secrets;
pub use store::{normalize_project_path, project_file_stem, MemoryStore, INJECTION_HEADING};

/// Allowed `Memory::category` values.
pub const CATEGORIES: &[&str] = &["preference", "convention", "stack", "other"];

/// Map free-form category text onto one of [`CATEGORIES`].
pub fn normalize_category(raw: &str) -> &'static str {
    let c = raw.trim().to_ascii_lowercase();
    let c = c.trim_end_matches('s');
    match c {
        "preference" | "pref" | "style" | "taste" | "workflow" | "user" => "preference",
        "convention" | "rule" | "guideline" | "pattern" | "practice" | "standard" => "convention",
        "stack" | "tech" | "technology" | "tool" | "tooling" | "framework" | "language" | "dependency"
        | "dependencie" | "infra" | "environment" => "stack",
        _ => "other",
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default()
}

/// Collapse whitespace (memories are single-line facts).
pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
