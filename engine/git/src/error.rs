use std::path::PathBuf;

/// Errors from git operations and the GitHub integration.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git executable not found on PATH")]
    GitNotFound,
    #[error("not a git repository: {}", .0.display())]
    NotARepo(PathBuf),
    /// A git (or gh) command exited unsuccessfully; `stderr` holds its diagnostics.
    #[error("`{program} {args}` failed (exit code {code:?}): {stderr}")]
    Command { program: String, args: String, code: Option<i32>, stderr: String, stdout: String },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// The request cannot be carried out (bad input or repository state).
    #[error("{0}")]
    Invalid(String),
    #[error("could not parse git output: {0}")]
    Parse(String),
    #[error("GitHub API error {status}: {message}")]
    Http { status: u16, message: String },
    #[error("network error: {0}")]
    Network(String),
}

impl GitError {
    /// The stderr of a failed command, if this is a command failure.
    pub fn stderr(&self) -> Option<&str> {
        match self {
            GitError::Command { stderr, .. } => Some(stderr),
            _ => None,
        }
    }
}

/// Result alias for this crate.
pub type Result<T, E = GitError> = std::result::Result<T, E>;
