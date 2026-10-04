//! Streamable HTTP transport (MCP 2025-03-26+): every message is a POST; the
//! response is JSON, an SSE stream, or `202 Accepted`. A GET SSE stream is
//! opened after initialization for server-initiated messages when offered.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{HeaderMap, ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;
use url::Url;

use super::{truncate_body, AbortOnDrop, Inbound, Transport};
use crate::error::McpError;
use crate::logbuf::LogSink;
use crate::oauth::TokenSource;
use crate::sse::{SseEvent, SseParser};

pub(crate) const SESSION_HEADER: &str = "mcp-session-id";
pub(crate) const PROTOCOL_HEADER: &str = "mcp-protocol-version";

pub(crate) struct HttpShared {
    pub http: reqwest::Client,
    pub url: Url,
    /// Static headers (custom headers and a static bearer token).
    pub headers: HeaderMap,
    pub tokens: Option<Arc<TokenSource>>,
    pub inbound: UnboundedSender<Inbound>,
    pub log: Arc<LogSink>,
}

impl HttpShared {
    /// Build a request with static headers + the OAuth bearer (if any).
    /// Returns the bearer that was used (for refresh-on-401).
    pub(crate) async fn authed(&self, req: reqwest::RequestBuilder) -> (reqwest::RequestBuilder, Option<String>) {
        let req = req.headers(self.headers.clone());
        match &self.tokens {
            Some(ts) => {
                let tok = ts.access_token().await;
                (req.bearer_auth(&tok), Some(tok))
            }
            None => (req, None),
        }
    }
}

struct State {
    session: Mutex<Option<String>>,
    protocol_version: Mutex<Option<String>>,
}

pub(crate) struct StreamableHttp {
    shared: Arc<HttpShared>,
    state: Arc<State>,
    streams: Mutex<Vec<AbortOnDrop>>,
    listener: Mutex<Option<AbortOnDrop>>,
}

impl StreamableHttp {
    pub(crate) fn new(shared: HttpShared) -> Self {
        StreamableHttp {
            shared: Arc::new(shared),
            state: Arc::new(State { session: Mutex::new(None), protocol_version: Mutex::new(None) }),
            streams: Mutex::new(Vec::new()),
            listener: Mutex::new(None),
        }
    }

    fn session(&self) -> Option<String> {
        self.state.session.lock().unwrap().clone()
    }

    fn with_session_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(s) = self.session() {
            req = req.header(SESSION_HEADER, s);
        }
        if let Some(v) = self.state.protocol_version.lock().unwrap().clone() {
            req = req.header(PROTOCOL_HEADER, v);
        }
        req
    }
}

/// The id of `msg` when it is a request (used to detect streams that end early).
fn request_id(msg: &Value) -> Option<Value> {
    if msg.get("method").is_some() {
        msg.get("id").filter(|i| !i.is_null()).cloned()
    } else {
        None
    }
}

pub(crate) fn forward_message(inbound: &UnboundedSender<Inbound>, log: &LogSink, data: &str) -> Vec<Value> {
    let data = data.trim();
    if data.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(data) {
        Ok(v) => {
            let ids: Vec<Value> = match &v {
                Value::Array(items) => items.iter().filter_map(|i| i.get("id").cloned()).collect(),
                other => other.get("id").cloned().into_iter().collect(),
            };
            let _ = inbound.send(Inbound::Message(v));
            ids
        }
        Err(e) => {
            log.error(format!("invalid JSON in server message: {e}"));
            Vec::new()
        }
    }
}

/// Read an SSE response body, forwarding `message` events. If `expect` is a
/// request id and the stream ends without its response, synthesize an error
/// response so the caller doesn't wait for the timeout.
pub(crate) async fn pump_sse(
    resp: reqwest::Response,
    inbound: UnboundedSender<Inbound>,
    log: Arc<LogSink>,
    expect: Option<Value>,
    mut on_event: impl FnMut(&SseEvent) -> bool + Send,
) {
    let mut parser = SseParser::new();
    let mut stream = resp.bytes_stream();
    let mut answered = expect.is_none();
    let mut handle = |ev: SseEvent, answered: &mut bool| {
        if !on_event(&ev) {
            return;
        }
        if matches!(ev.event.as_deref(), None | Some("message")) {
            let ids = forward_message(&inbound, &log, &ev.data);
            if let Some(want) = &expect {
                if ids.iter().any(|i| i == want) {
                    *answered = true;
                }
            }
        }
    };
    let mut error = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                for ev in parser.feed(&bytes) {
                    handle(ev, &mut answered);
                }
            }
            Err(e) => {
                error = Some(McpError::from(e).to_string());
                break;
            }
        }
    }
    if let Some(ev) = parser.finish() {
        handle(ev, &mut answered);
    }
    if !answered {
        let reason = error.unwrap_or_else(|| "stream ended".into());
        log.error(format!("SSE response ended before the result arrived: {reason}"));
        if let Some(id) = expect {
            let _ = inbound.send(Inbound::Message(json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32000, "message": format!("SSE stream ended before the response ({reason})")}
            })));
        }
    }
}

fn is_event_stream(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.to_ascii_lowercase().starts_with("text/event-stream"))
}

/// GET stream for server-initiated messages. Servers that don't offer one answer 405.
async fn listen(shared: Arc<HttpShared>, state: Arc<State>) {
    let mut short_lived = 0;
    loop {
        let mut req = shared.http.get(shared.url.clone()).header(ACCEPT, "text/event-stream");
        if let Some(s) = state.session.lock().unwrap().clone() {
            req = req.header(SESSION_HEADER, s);
        }
        if let Some(v) = state.protocol_version.lock().unwrap().clone() {
            req = req.header(PROTOCOL_HEADER, v);
        }
        let (req, _) = shared.authed(req).await;
        let resp = match req.send().await {
            Ok(r) => r,
            Err(_) => return,
        };
        if !resp.status().is_success() || !is_event_stream(&resp) {
            return;
        }
        let started = Instant::now();
        pump_sse(resp, shared.inbound.clone(), shared.log.clone(), None, |_| true).await;
        if started.elapsed() < Duration::from_secs(5) {
            short_lived += 1;
            if short_lived >= 3 {
                return;
            }
        } else {
            short_lived = 0;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[async_trait]
impl Transport for StreamableHttp {
    async fn send(&self, msg: Value) -> Result<(), McpError> {
        let expect = request_id(&msg);
        let mut retried_auth = false;
        loop {
            let session = self.session();
            let req = self
                .shared
                .http
                .post(self.shared.url.clone())
                .header(ACCEPT, "application/json, text/event-stream")
                .json(&msg);
            let (req, bearer) = self.shared.authed(self.with_session_headers(req)).await;
            let resp = req.send().await?;
            let status = resp.status();
            if let Some(sid) = resp.headers().get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
                *self.state.session.lock().unwrap() = Some(sid.to_string());
            }
            if status.as_u16() == 401 {
                let challenge = resp.headers().get(WWW_AUTHENTICATE).and_then(|v| v.to_str().ok()).map(str::to_string);
                if !retried_auth {
                    if let (Some(ts), Some(used)) = (&self.shared.tokens, &bearer) {
                        if ts.refresh_after_unauthorized(used).await {
                            retried_auth = true;
                            continue;
                        }
                    }
                }
                return Err(McpError::Unauthorized { www_authenticate: challenge });
            }
            if status.as_u16() == 404 && session.is_some() {
                *self.state.session.lock().unwrap() = None;
                return Err(McpError::SessionExpired);
            }
            if status.as_u16() == 202 || status.as_u16() == 204 {
                return Ok(());
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(McpError::Http { status: status.as_u16(), body: truncate_body(&body, 500) });
            }
            if is_event_stream(&resp) {
                let shared = self.shared.clone();
                let task = tokio::spawn(async move {
                    pump_sse(resp, shared.inbound.clone(), shared.log.clone(), expect, |_| true).await;
                });
                let mut streams = self.streams.lock().unwrap();
                streams.retain(|t| !t.is_finished());
                streams.push(AbortOnDrop(task));
                return Ok(());
            }
            let body = resp.bytes().await?;
            if body.iter().all(u8::is_ascii_whitespace) {
                return Ok(());
            }
            let text = String::from_utf8_lossy(&body);
            if forward_message(&self.shared.inbound, &self.shared.log, &text).is_empty() && expect.is_some() {
                return Err(McpError::Transport(format!("unexpected response body: {}", truncate_body(&text, 200))));
            }
            return Ok(());
        }
    }

    fn on_initialized(&self, protocol_version: &str) {
        *self.state.protocol_version.lock().unwrap() = Some(protocol_version.to_string());
        let task = tokio::spawn(listen(self.shared.clone(), self.state.clone()));
        *self.listener.lock().unwrap() = Some(AbortOnDrop(task));
    }

    fn has_session(&self) -> bool {
        self.session().is_some()
    }

    async fn close(&self) {
        self.listener.lock().unwrap().take();
        self.streams.lock().unwrap().clear();
        if let Some(session) = self.session() {
            let req = self.shared.http.delete(self.shared.url.clone()).header(SESSION_HEADER, session);
            let req = match self.state.protocol_version.lock().unwrap().clone() {
                Some(v) => req.header(PROTOCOL_HEADER, v),
                None => req,
            };
            let (req, _) = self.shared.authed(req).await;
            let _ = req.timeout(Duration::from_secs(2)).send().await;
            *self.state.session.lock().unwrap() = None;
        }
    }
}
