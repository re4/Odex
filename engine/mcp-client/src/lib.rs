//! Odex MCP client.
//!
//! A self-contained Model Context Protocol client used by the Odex engine:
//!
//! * transports: stdio (newline-delimited JSON-RPC to a child process) and
//!   Streamable HTTP (JSON or SSE responses, `Mcp-Session-Id`), with a
//!   fallback to the legacy HTTP+SSE transport;
//! * OAuth 2.1 for HTTP servers (RFC 9728 / RFC 8414 discovery, RFC 7591
//!   dynamic registration, PKCE S256, loopback redirect, refresh);
//! * [`sanitize_schema`] to turn arbitrary JSON Schema into the flat shape
//!   vLLM chat templates can render, while the original schema is kept for
//!   argument validation ([`validate_arguments`]);
//! * [`McpManager`], which owns every configured server, starts them in
//!   parallel and reports live status, logs and events to the host.

mod client;
mod error;
mod logbuf;
mod manager;
mod naming;
pub mod oauth;
mod process;
mod schema;
mod sse;
mod transport;
mod types;
mod validate;

pub use error::McpError;
pub use manager::McpManager;
pub use naming::{qualify_tool_name, sanitize_name_part, MAX_TOOL_NAME_LEN};
pub use schema::sanitize_schema;
pub use types::{CallToolResult, McpEvent, McpTool};
pub use validate::validate_arguments;

/// Result type of the public API. Errors raised by this crate are [`McpError`]s
/// (use `err.downcast_ref::<McpError>()` to classify them).
pub type Result<T, E = anyhow::Error> = std::result::Result<T, E>;

/// Protocol version sent in `initialize`.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Protocol versions we know how to talk to. Servers answering with another
/// version are still accepted (best effort).
pub const KNOWN_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// `clientInfo.name` sent in `initialize`.
pub const CLIENT_NAME: &str = "odex";

/// Default `startup_timeout_ms`.
pub const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 30_000;

/// Default `tool_timeout_ms`.
pub const DEFAULT_TOOL_TIMEOUT_MS: u64 = 120_000;

/// Lines kept per server in the log ring buffer.
pub const LOG_CAPACITY: usize = 500;
