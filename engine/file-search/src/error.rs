use std::path::PathBuf;

/// Errors produced by the search and read helpers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("path not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid pattern: {0}")]
    InvalidPattern(String),
    #[error("{0}")]
    InvalidArgument(String),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        let path = path.into();
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound(path)
        } else {
            Error::Io { path, source }
        }
    }
}
