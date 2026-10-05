//! JSON-RPC connection on top of a [`Transport`]: request/response matching,
//! timeouts with `notifications/cancelled`, server→client requests (ping,
//! elicitation, roots) and the MCP initialize handshake.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::AbortHandle;

use crate::error::McpError;
use crate::logbuf::LogSink;
use crate::transport::{AbortOnDrop, Inbound, Transport};
use crate::{CLIENT_NAME, MCP_PROTOCOL_VERSION};

type Pending = oneshot::Sender<Result<Value, McpError>>;

/// Events surfaced to the server supervisor.
pub(crate) enum ConnEvent {
    Notification {
        method: String,
        params: Value,
    },
    /// `elicitation/create`; answer with the JSON-RPC `result` value.
    Elicitation {
        params: Value,
        respond: oneshot::Sender<Value>,
    },
    Closed(String),
}

/// Parsed `initialize` result.
#[derive(Debug, Clone, Default)]
pub(crate) struct InitInfo {
    pub protocol_version: String,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub instructions: Option<String>,
    pub capabilities: Value,
}

impl InitInfo {
    fn parse(v: &Value) -> Self {
        let info = v.get("serverInfo");
        InitInfo {
            protocol_version: v
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(MCP_PROTOCOL_VERSION)
                .to_string(),
            server_name: info.and_then(|i| i.get("name")).and_then(Value::as_str).map(str::to_string),
            server_version: info.and_then(|i| i.get("version")).and_then(Value::as_str).map(str::to_string),
            instructions: v
                .get("instructions")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty()),
            capabilities: v.get("capabilities").cloned().unwrap_or_else(|| json!({})),
        }
    }

    pub(crate) fn has_capability(&self, name: &str) -> bool {
        self.capabilities.get(name).is_some_and(|c| !c.is_null())
    }
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

struct Shared {
    transport: Arc<dyn Transport>,
    log: Arc<LogSink>,
    pending: Mutex<HashMap<String, Pending>>,
    next_id: AtomicU64,
    elicitations: AtomicUsize,
    closed: Mutex<Option<String>>,
    events: UnboundedSender<ConnEvent>,
    server_requests: Mutex<Vec<AbortHandle>>,
}

pub(crate) struct Connection {
    shared: Arc<Shared>,
    reinit: tokio::sync::Mutex<()>,
    _reader: AbortOnDrop,
}

impl Drop for Connection {
    fn drop(&mut self) {
        for h in self.shared.server_requests.lock().unwrap().drain(..) {
            h.abort();
        }
    }
}

impl Connection {
    pub(crate) fn new(
        transport: Arc<dyn Transport>,
        inbound: UnboundedReceiver<Inbound>,
        log: Arc<LogSink>,
    ) -> (Connection, UnboundedReceiver<ConnEvent>) {
        let (events, events_rx) = unbounded_channel();
        let shared = Arc::new(Shared {
            transport,
            log,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            elicitations: AtomicUsize::new(0),
            closed: Mutex::new(None),
            events,
            server_requests: Mutex::new(Vec::new()),
        });
        let reader = tokio::spawn(reader_loop(shared.clone(), inbound));
        (Connection { shared, reinit: tokio::sync::Mutex::new(()), _reader: AbortOnDrop(reader) }, events_rx)
    }

    /// `initialize` + `notifications/initialized`.
    pub(crate) async fn initialize(&self, timeout: Duration) -> Result<InitInfo, McpError> {
        let params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"elicitation": {}},
            "clientInfo": {"name": CLIENT_NAME, "title": "Odex", "version": env!("CARGO_PKG_VERSION")},
        });
        let result = self.shared.request_once("initialize", Some(params), timeout).await?;
        let info = InitInfo::parse(&result);
        if !crate::KNOWN_PROTOCOL_VERSIONS.contains(&info.protocol_version.as_str()) {
            self.shared
                .log
                .info(format!("server speaks unknown protocol version {}; continuing", info.protocol_version));
        }
        self.shared.transport.on_initialized(&info.protocol_version);
        self.notify("notifications/initialized", None).await?;
        Ok(info)
    }

    /// Send a request; on an expired HTTP session, re-initialize once and retry.
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        match self.shared.request_once(method, params.clone(), timeout).await {
            Err(McpError::SessionExpired) => {
                self.shared.log.info("HTTP session expired; re-initializing");
                {
                    let _g = self.reinit.lock().await;
                    if !self.shared.transport.has_session() {
                        self.initialize(timeout).await?;
                    }
                }
                self.shared.request_once(method, params, timeout).await
            }
            other => other,
        }
    }

    pub(crate) async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let mut msg = json!({"jsonrpc": "2.0", "method": method});
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.shared.transport.send(msg).await
    }

    /// Paginated `*/list` call collecting `key` from every page.
    pub(crate) async fn list_all(&self, method: &str, key: &str, timeout: Duration) -> Result<Vec<Value>, McpError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..100 {
            let params = cursor.as_ref().map(|c| json!({"cursor": c}));
            let page = self.request(method, params, timeout).await?;
            if let Some(Value::Array(items)) = page.get(key) {
                out.extend(items.iter().cloned());
            }
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(c) if !c.is_empty() && cursor.as_deref() != Some(c) => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    pub(crate) async fn close(&self) {
        self.shared.transport.close().await;
    }
}

async fn reader_loop(shared: Arc<Shared>, mut inbound: UnboundedReceiver<Inbound>) {
    let reason = loop {
        match inbound.recv().await {
            Some(Inbound::Message(v)) => shared.dispatch(v),
            Some(Inbound::Closed(reason)) => break reason,
            None => break "transport closed".to_string(),
        }
    };
    shared.mark_closed(reason);
}

impl Shared {
    fn closed_reason(&self) -> Option<String> {
        self.closed.lock().unwrap().clone()
    }

    fn mark_closed(&self, reason: String) {
        {
            let mut c = self.closed.lock().unwrap();
            if c.is_some() {
                return;
            }
            *c = Some(reason.clone());
        }
        let pending: Vec<Pending> = self.pending.lock().unwrap().drain().map(|(_, tx)| tx).collect();
        for tx in pending {
            let _ = tx.send(Err(McpError::Closed(reason.clone())));
        }
        let _ = self.events.send(ConnEvent::Closed(reason));
    }

    fn dispatch(self: &Arc<Self>, v: Value) {
        match v {
            Value::Array(items) => {
                for item in items {
                    self.dispatch(item);
                }
            }
            Value::Object(m) => {
                let method = m.get("method").and_then(Value::as_str).map(str::to_string);
                let id = m.get("id").filter(|i| !i.is_null()).cloned();
                match (method, id) {
                    (Some(method), Some(id)) => self.handle_server_request(id, method, m.get("params").cloned()),
                    (Some(method), None) => {
                        let params = m.get("params").cloned().unwrap_or(Value::Null);
                        let _ = self.events.send(ConnEvent::Notification { method, params });
                    }
                    (None, Some(id)) => self.handle_response(&id, &m),
                    (None, None) => match m.get("error") {
                        Some(err) => self.log.error(format!("server reported an error: {err}")),
                        None => self.log.error("ignoring a message without method or id"),
                    },
                }
            }
            other => self.log.error(format!("ignoring a non-object message: {other}")),
        }
    }

    fn handle_response(&self, id: &Value, m: &Map<String, Value>) {
        let Some(tx) = self.pending.lock().unwrap().remove(&id_key(id)) else {
            return; // late response after a timeout
        };
        let result = match m.get("error") {
            Some(err) => Err(McpError::Rpc {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                message: err.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_string(),
                data: err.get("data").cloned(),
            }),
            None => Ok(m.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    }

    fn handle_server_request(self: &Arc<Self>, id: Value, method: String, params: Option<Value>) {
        let shared = self.clone();
        let task = tokio::spawn(async move {
            let reply = match method.as_str() {
                "ping" => Ok(json!({})),
                "roots/list" => Ok(json!({"roots": []})),
                "elicitation/create" => {
                    shared.elicitations.fetch_add(1, Ordering::SeqCst);
                    let (tx, rx) = oneshot::channel();
                    let sent = shared
                        .events
                        .send(ConnEvent::Elicitation { params: params.unwrap_or(Value::Null), respond: tx })
                        .is_ok();
                    let answer = if sent { rx.await.ok() } else { None };
                    shared.elicitations.fetch_sub(1, Ordering::SeqCst);
                    Ok(answer.unwrap_or_else(|| json!({"action": "cancel"})))
                }
                _ => Err((-32601, format!("method not found: {method}"))),
            };
            let msg = match reply {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, message)) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                }
            };
            if let Err(e) = shared.transport.send(msg).await {
                shared.log.error(format!("failed to answer `{method}`: {e}"));
            }
        });
        let mut reqs = self.server_requests.lock().unwrap();
        reqs.retain(|h| !h.is_finished());
        reqs.push(task.abort_handle());
    }

    async fn request_once(&self, method: &str, params: Option<Value>, timeout: Duration) -> Result<Value, McpError> {
        if let Some(reason) = self.closed_reason() {
            return Err(McpError::Closed(reason));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.to_string(), tx);
        // Removes the pending entry on every exit path; when the request was
        // still outstanding (timeout, or the caller dropped us) the server is
        // told with `notifications/cancelled`.
        let mut guard = InFlight { shared: self, id, method, reason: "cancelled by the client" };
        let mut msg = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if let Some(p) = params {
            msg["params"] = p;
        }

        let send = self.transport.send(msg);
        tokio::pin!(send);
        tokio::pin!(rx);
        let mut sent = false;
        let mut deadline = tokio::time::Instant::now() + timeout;
        loop {
            tokio::select! {
                r = &mut send, if !sent => {
                    sent = true;
                    if let Err(e) = r {
                        self.pending.lock().unwrap().remove(&id.to_string());
                        return Err(e);
                    }
                }
                r = &mut rx => {
                    return match r {
                        Ok(res) => res,
                        Err(_) => Err(McpError::Closed(self.closed_reason().unwrap_or_else(|| "connection dropped".into()))),
                    };
                }
                _ = tokio::time::sleep_until(deadline) => {
                    // Don't time out while the user is answering an elicitation.
                    if self.elicitations.load(Ordering::SeqCst) > 0 {
                        deadline = tokio::time::Instant::now() + timeout;
                        continue;
                    }
                    guard.reason = "timeout";
                    return Err(McpError::Timeout { method: method.to_string(), ms: timeout.as_millis() as u64 });
                }
            }
        }
    }
}

struct InFlight<'a> {
    shared: &'a Shared,
    id: u64,
    method: &'a str,
    reason: &'static str,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let outstanding = self.shared.pending.lock().unwrap().remove(&self.id.to_string()).is_some();
        if !outstanding || self.method == "initialize" {
            return;
        }
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let transport = self.shared.transport.clone();
            let note = json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                              "params": {"requestId": self.id, "reason": self.reason}});
            rt.spawn(async move {
                let _ = transport.send(note).await;
            });
        }
    }
}
