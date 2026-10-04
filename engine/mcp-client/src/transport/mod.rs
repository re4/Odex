//! Message transports. A transport only moves JSON-RPC messages; request /
//! response matching lives in [`crate::client::Connection`].

pub(crate) mod http;
pub(crate) mod legacy_sse;
pub(crate) mod stdio;

use async_trait::async_trait;
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::error::McpError;

/// What a transport hands to the connection's reader.
#[derive(Debug)]
pub(crate) enum Inbound {
    Message(Value),
    Closed(String),
}

#[async_trait]
pub(crate) trait Transport: Send + Sync + 'static {
    /// Deliver one outgoing message. Responses arrive through the inbound channel.
    async fn send(&self, msg: Value) -> Result<(), McpError>;

    /// Called once `initialize` succeeded with the negotiated protocol version.
    fn on_initialized(&self, _protocol_version: &str) {}

    /// Whether the transport currently holds a server session (HTTP).
    fn has_session(&self) -> bool {
        false
    }

    /// Graceful shutdown (close stdin / DELETE the session, then kill).
    async fn close(&self);
}

/// Aborts the task when dropped.
pub(crate) struct AbortOnDrop(pub JoinHandle<()>);

impl AbortOnDrop {
    pub(crate) fn is_finished(&self) -> bool {
        self.0.is_finished()
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) fn truncate_body(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &s[..cut])
}
