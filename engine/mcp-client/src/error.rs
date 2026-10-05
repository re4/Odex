use serde_json::Value;

/// Errors produced by the MCP client.
#[derive(Debug, Clone, thiserror::Error)]
pub enum McpError {
    #[error("unknown MCP server `{0}`")]
    UnknownServer(String),
    #[error("unknown MCP tool `{0}`")]
    UnknownTool(String),
    #[error("MCP tool `{0}` is disabled")]
    ToolDisabled(String),
    #[error("MCP server `{server}` is not ready ({state})")]
    NotReady { server: String, state: String },
    #[error("MCP server requires authentication (HTTP 401)")]
    Unauthorized { www_authenticate: Option<String> },
    #[error("MCP session expired")]
    SessionExpired,
    #[error("`{method}` timed out after {ms} ms")]
    Timeout { method: String, ms: u64 },
    #[error("MCP error {code}: {message}")]
    Rpc { code: i64, message: String, data: Option<Value> },
    #[error("invalid arguments for `{tool}`: {message}")]
    InvalidArguments { tool: String, message: String },
    #[error("connection closed: {0}")]
    Closed(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("OAuth: {0}")]
    OAuth(String),
    #[error("invalid MCP server config: {0}")]
    Config(String),
    #[error("{0}")]
    Other(String),
}

impl McpError {
    pub(crate) fn transport(e: impl std::fmt::Display) -> Self {
        McpError::Transport(e.to_string())
    }

    /// True for errors that mean "the server wants credentials".
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, McpError::Unauthorized { .. })
    }
}

impl From<reqwest::Error> for McpError {
    fn from(e: reqwest::Error) -> Self {
        // `{:#}`-style chain: reqwest's Display hides the root cause.
        let mut msg = e.to_string();
        let mut src = std::error::Error::source(&e);
        while let Some(s) = src {
            let s_msg = s.to_string();
            if !msg.contains(&s_msg) {
                msg.push_str(": ");
                msg.push_str(&s_msg);
            }
            src = s.source();
        }
        McpError::Transport(msg)
    }
}
