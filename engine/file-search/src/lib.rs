//! File discovery and content search for Odex.
//!
//! * [`FileIndex`] / [`fuzzy_search`]: cached, gitignore-aware file list with fzf-style fuzzy
//!   scoring, used by the desktop's Ctrl+P and by the agent.
//! * [`grep`]: ripgrep-style content search (grep-searcher + grep-regex over a parallel
//!   `ignore` walk) producing compact text output for the agent.
//! * [`glob`], [`list_dir`], [`read_file_paged`]: the remaining read-only filesystem tools.

mod error;
mod fuzzy;
mod glob;
mod grep;
mod index;
mod list;
mod read;
mod walk;

pub use error::Error;
pub use fuzzy::fuzzy_match;
pub use glob::glob;
pub use grep::{grep, GrepMode, GrepOptions, GrepResult};
pub use index::{fuzzy_search, FileIndex};
pub use list::list_dir;
pub use read::{read_file_paged, FilePage};

/// Convenience alias for results in this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
