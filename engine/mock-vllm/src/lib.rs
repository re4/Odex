//! A mock vLLM server for tests.
//!
//! Serves `/v1/models`, `/health`, `/version`, `/tokenize` and
//! `/v1/chat/completions` (streaming and not). Replies come from a FIFO queue
//! of [`MockReply`]s, then from a policy (Rust closure or JSON rules). It
//! enforces `max_model_len` with vLLM's real error text, can inject faults
//! (429/503, disconnects, stalls) and records every request.
//!
//! Control endpoints for out-of-process tests (Playwright): `POST /__mock/push`,
//! `POST /__mock/rules`, `GET /__mock/requests`, `POST /__mock/reset`.

pub mod rules;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub use rules::{Rule, RulePolicy};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockModel {
    pub id: String,
    pub max_model_len: u32,
}

#[derive(Debug, Clone)]
pub struct MockConfig {
    pub models: Vec<MockModel>,
    pub version: String,
    /// Mock tokenizer: characters per token.
    pub chars_per_token: f64,
    /// Delay between streamed chunks.
    pub chunk_delay: Duration,
    /// Characters per content delta.
    pub chunk_chars: usize,
    /// Require this bearer token on /v1 routes.
    pub api_key: Option<String>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            models: vec![MockModel { id: "mock-coder".into(), max_model_len: 32768 }],
            version: "0.30.0-mock".into(),
            chars_per_token: 4.0,
            chunk_delay: Duration::from_millis(0),
            chunk_chars: 8,
            api_key: None,
        }
    }
}

/// A scripted reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MockReply {
    Text {
        text: String,
    },
    /// Reasoning in the `reasoning` field, then content.
    Reasoning {
        reasoning: String,
        text: String,
    },
    /// Reasoning inline as `<think>...</think>` in content (no server parser).
    ThinkInContent {
        reasoning: String,
        text: String,
    },
    /// Native tool calls (optionally with preamble text).
    ToolCalls {
        calls: Vec<MockCall>,
        text: Option<String>,
    },
    /// Tool call markup inside `content` (for fallback-parser tests).
    ContentToolCall {
        content: String,
    },
    /// HTTP error.
    Error {
        status: u16,
        body: String,
        retry_after: Option<u64>,
    },
    /// Stream `partial` for `after_chunks` chunks then drop the connection.
    Disconnect {
        after_chunks: usize,
        partial: Box<MockReply>,
    },
    /// Send headers then stall (no data) for `ms`.
    Stall {
        ms: u64,
    },
    /// JSON content (structured outputs).
    Json {
        value: Value,
    },
    /// Raw SSE `data:` payloads, sent verbatim.
    RawSse {
        events: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MockCall {
    pub name: String,
    pub arguments: Value,
    /// Send arguments as this raw string instead (for malformed-args tests).
    #[serde(default)]
    pub raw_arguments: Option<String>,
}

impl MockReply {
    pub fn text(s: impl Into<String>) -> Self {
        MockReply::Text { text: s.into() }
    }
    pub fn tool(name: &str, args: Value) -> Self {
        MockReply::ToolCalls {
            calls: vec![MockCall { name: name.into(), arguments: args, raw_arguments: None }],
            text: None,
        }
    }
    pub fn tools(calls: Vec<(&str, Value)>) -> Self {
        MockReply::ToolCalls {
            calls: calls
                .into_iter()
                .map(|(n, a)| MockCall { name: n.into(), arguments: a, raw_arguments: None })
                .collect(),
            text: None,
        }
    }
    pub fn error(status: u16, body: &str) -> Self {
        MockReply::Error { status, body: body.into(), retry_after: None }
    }
}

/// A request as received, with helpers for policies.
#[derive(Debug, Clone, Serialize)]
pub struct RecordedRequest {
    pub index: usize,
    pub body: Value,
    pub prompt_tokens: u32,
}

impl RecordedRequest {
    pub fn messages(&self) -> &[Value] {
        self.body.get("messages").and_then(|m| m.as_array()).map(|a| a.as_slice()).unwrap_or(&[])
    }
    pub fn max_tokens(&self) -> Option<u32> {
        self.body
            .get("max_tokens")
            .or_else(|| self.body.get("max_completion_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
    }
    pub fn tool_names(&self) -> Vec<String> {
        self.body
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|t| t["function"]["name"].as_str().map(String::from)).collect())
            .unwrap_or_default()
    }
    pub fn last_role(&self) -> Option<String> {
        self.messages().last().and_then(|m| m.get("role")).and_then(|r| r.as_str()).map(String::from)
    }
    pub fn message_text(m: &Value) -> String {
        match m.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => {
                parts.iter().filter_map(|p| p.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n")
            }
            _ => String::new(),
        }
    }
    pub fn last_user_text(&self) -> String {
        self.messages()
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
            .map(Self::message_text)
            .unwrap_or_default()
    }
    pub fn last_text(&self) -> String {
        self.messages().last().map(Self::message_text).unwrap_or_default()
    }
    pub fn system_text(&self) -> String {
        self.messages()
            .iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            .map(Self::message_text)
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// All message text joined (for "does the context still contain X").
    pub fn all_text(&self) -> String {
        let mut s = String::new();
        for m in self.messages() {
            s.push_str(&Self::message_text(m));
            s.push('\n');
            if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    s.push_str(tc["function"]["arguments"].as_str().unwrap_or(""));
                    s.push('\n');
                }
            }
        }
        s
    }
    /// Name of the requested JSON schema, if structured output was asked for.
    pub fn structured_name(&self) -> Option<String> {
        if let Some(rf) = self.body.get("response_format") {
            return rf
                .get("json_schema")
                .and_then(|j| j.get("name"))
                .and_then(|n| n.as_str())
                .map(String::from)
                .or(Some("json".into()));
        }
        if self.body.get("guided_json").is_some() || self.body.get("structured_outputs").is_some() {
            return Some("json".into());
        }
        None
    }
    pub fn structured_schema(&self) -> Option<Value> {
        self.body
            .get("response_format")
            .and_then(|rf| rf.get("json_schema"))
            .and_then(|j| j.get("schema"))
            .cloned()
            .or_else(|| self.body.get("guided_json").cloned())
    }
    pub fn stream(&self) -> bool {
        self.body.get("stream").and_then(|s| s.as_bool()).unwrap_or(false)
    }
    /// Count of tool-result messages after the last user message.
    pub fn tool_results_since_user(&self) -> usize {
        self.messages()
            .iter()
            .rev()
            .take_while(|m| m.get("role").and_then(|r| r.as_str()) != Some("user"))
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("tool"))
            .count()
    }
}

pub type PolicyFn = Arc<dyn Fn(&RecordedRequest) -> MockReply + Send + Sync>;

struct Inner {
    queue: VecDeque<MockReply>,
    policy: Option<PolicyFn>,
    requests: Vec<RecordedRequest>,
    max_len_override: Option<u32>,
}

#[derive(Clone)]
pub struct MockState {
    cfg: Arc<MockConfig>,
    inner: Arc<Mutex<Inner>>,
}

pub struct MockServer {
    /// Base URL including `/v1`.
    pub url: String,
    /// Server root (for /health, /tokenize).
    pub root: String,
    pub addr: SocketAddr,
    state: MockState,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockServer {
    pub async fn start(cfg: MockConfig) -> Self {
        Self::start_on(cfg, "127.0.0.1:0").await
    }

    pub async fn start_on(cfg: MockConfig, bind: &str) -> Self {
        let state = MockState {
            cfg: Arc::new(cfg),
            inner: Arc::new(Mutex::new(Inner {
                queue: VecDeque::new(),
                policy: None,
                requests: vec![],
                max_len_override: None,
            })),
        };
        let app = router(state.clone());
        let listener = tokio::net::TcpListener::bind(bind).await.expect("bind mock server");
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });
        let root = format!("http://{addr}");
        Self { url: format!("{root}/v1"), root, addr, state, shutdown: Some(tx) }
    }

    pub fn push(&self, r: MockReply) {
        self.state.inner.lock().unwrap().queue.push_back(r);
    }

    pub fn push_all(&self, rs: impl IntoIterator<Item = MockReply>) {
        let mut i = self.state.inner.lock().unwrap();
        i.queue.extend(rs);
    }

    pub fn set_policy(&self, f: impl Fn(&RecordedRequest) -> MockReply + Send + Sync + 'static) {
        self.state.inner.lock().unwrap().policy = Some(Arc::new(f));
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.inner.lock().unwrap().requests.clone()
    }

    pub fn request_count(&self) -> usize {
        self.state.inner.lock().unwrap().requests.len()
    }

    pub fn last_request(&self) -> Option<RecordedRequest> {
        self.state.inner.lock().unwrap().requests.last().cloned()
    }

    pub fn queued(&self) -> usize {
        self.state.inner.lock().unwrap().queue.len()
    }

    /// Change the enforced context length at runtime (model-switch tests).
    pub fn set_max_model_len(&self, n: u32) {
        self.state.inner.lock().unwrap().max_len_override = Some(n);
    }

    /// Prompt tokens as the mock tokenizer counts them.
    pub fn count_tokens(&self, body: &Value) -> u32 {
        count_prompt_tokens(body, self.state.cfg.chars_per_token)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

pub fn router(state: MockState) -> Router {
    Router::new()
        .route("/v1/models", get(models))
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/version", get(version))
        .route("/tokenize", post(tokenize))
        .route("/v1/chat/completions", post(chat))
        .route("/__mock/push", post(ctl_push))
        .route("/__mock/rules", post(ctl_rules))
        .route("/__mock/requests", get(ctl_requests))
        .route("/__mock/reset", post(ctl_reset))
        .with_state(state)
}

/// Build a `MockState` for embedding the router elsewhere.
pub fn state(cfg: MockConfig) -> MockState {
    MockState {
        cfg: Arc::new(cfg),
        inner: Arc::new(Mutex::new(Inner {
            queue: VecDeque::new(),
            policy: None,
            requests: vec![],
            max_len_override: None,
        })),
    }
}

impl MockState {
    pub fn set_policy(&self, f: impl Fn(&RecordedRequest) -> MockReply + Send + Sync + 'static) {
        self.inner.lock().unwrap().policy = Some(Arc::new(f));
    }
}

fn max_len(st: &MockState) -> u32 {
    st.inner
        .lock()
        .unwrap()
        .max_len_override
        .unwrap_or_else(|| st.cfg.models.first().map(|m| m.max_model_len).unwrap_or(32768))
}

async fn models(State(st): State<MockState>) -> Json<Value> {
    let ml = max_len(&st);
    let data: Vec<Value> = st
        .cfg
        .models
        .iter()
        .enumerate()
        .map(|(i, m)| {
            json!({"id": m.id, "object": "model", "created": 1759600000, "owned_by": "vllm", "root": m.id, "parent": null,
                   "max_model_len": if i == 0 { ml } else { m.max_model_len }})
        })
        .collect();
    Json(json!({"object": "list", "data": data}))
}

async fn version(State(st): State<MockState>) -> Json<Value> {
    Json(json!({"version": st.cfg.version}))
}

async fn tokenize(State(st): State<MockState>, Json(body): Json<Value>) -> Json<Value> {
    let n = if body.get("messages").is_some() {
        count_prompt_tokens(&body, st.cfg.chars_per_token)
    } else {
        let p = body.get("prompt").and_then(|p| p.as_str()).unwrap_or("");
        (p.chars().count() as f64 / st.cfg.chars_per_token).ceil() as u32
    };
    Json(json!({"count": n, "max_model_len": max_len(&st), "tokens": []}))
}

/// Deterministic mock tokenizer: chars/ratio per message + overhead; images 1000.
pub fn count_prompt_tokens(body: &Value, cpt: f64) -> u32 {
    let mut chars = 0usize;
    let mut extra = 0u32;
    if let Some(ms) = body.get("messages").and_then(|m| m.as_array()) {
        for m in ms {
            extra += 4;
            match m.get("content") {
                Some(Value::String(s)) => chars += s.chars().count(),
                Some(Value::Array(parts)) => {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                            chars += t.chars().count();
                        } else if p.get("image_url").is_some() {
                            extra += 1000;
                        }
                    }
                }
                _ => {}
            }
            if let Some(tcs) = m.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    chars += tc["function"]["name"].as_str().map(|s| s.len()).unwrap_or(0);
                    chars += tc["function"]["arguments"].as_str().map(|s| s.chars().count()).unwrap_or(0);
                    extra += 6;
                }
            }
            for k in ["reasoning_content", "reasoning"] {
                if let Some(r) = m.get(k).and_then(|r| r.as_str()) {
                    chars += r.chars().count();
                    break;
                }
            }
        }
    }
    if let Some(tools) = body.get("tools") {
        chars += tools.to_string().chars().count();
    }
    (chars as f64 / cpt).ceil() as u32 + extra
}

fn vllm_error(status: StatusCode, msg: &str) -> Response {
    let body = json!({"error": {"message": msg, "type": "BadRequestError", "param": null, "code": status.as_u16()}});
    (status, Json(body)).into_response()
}

async fn chat(State(st): State<MockState>, headers: axum::http::HeaderMap, Json(body): Json<Value>) -> Response {
    if let Some(key) = &st.cfg.api_key {
        let ok = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|v| v == format!("Bearer {key}"))
            .unwrap_or(false);
        if !ok {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Unauthorized"}))).into_response();
        }
    }
    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    if !st.cfg.models.iter().any(|m| m.id == model) {
        return vllm_error(StatusCode::NOT_FOUND, &format!("The model `{model}` does not exist."));
    }
    let prompt_tokens = count_prompt_tokens(&body, st.cfg.chars_per_token);
    let (reply, req) = {
        let mut inner = st.inner.lock().unwrap();
        let req = RecordedRequest { index: inner.requests.len(), body: body.clone(), prompt_tokens };
        inner.requests.push(req.clone());
        let reply = if let Some(r) = inner.queue.pop_front() {
            r
        } else if let Some(p) = inner.policy.clone() {
            drop(inner);
            p(&req)
        } else {
            default_reply(&req)
        };
        (reply, req)
    };
    // context enforcement (vLLM v0.18+ wording)
    let ml = max_len(&st);
    let max_tokens = req.max_tokens();
    if let Some(mt) = max_tokens {
        if mt > ml {
            return vllm_error(
                StatusCode::BAD_REQUEST,
                &format!("max_tokens={mt} cannot be greater than max_model_len=max_total_tokens={ml}. Please request fewer output tokens. (parameter=max_tokens, value={mt})"),
            );
        }
    }
    let out = max_tokens.unwrap_or(0);
    if prompt_tokens + out > ml {
        return vllm_error(
            StatusCode::BAD_REQUEST,
            &format!(
                "This model's maximum context length is {ml} tokens. However, you requested {out} output tokens and your prompt contains {prompt_tokens} input tokens, for a total of {} tokens. Please reduce the length of the input prompt or the number of requested output tokens. (parameter=input_tokens, value={prompt_tokens})",
                prompt_tokens + out
            ),
        );
    }
    respond(&st, req, reply).await
}

/// Default reply when no queue/policy: valid JSON for structured requests, else "OK".
pub fn default_reply(req: &RecordedRequest) -> MockReply {
    if let Some(schema) = req.structured_schema() {
        return MockReply::Json { value: rules::example_for_schema(&schema) };
    }
    if req.structured_name().is_some() {
        return MockReply::Json { value: json!({}) };
    }
    MockReply::text("OK")
}

fn completion_tokens(s: &str) -> u32 {
    (s.chars().count() as f64 / 4.0).ceil() as u32 + 1
}

async fn respond(st: &MockState, req: RecordedRequest, reply: MockReply) -> Response {
    match reply {
        MockReply::Error { status, body, retry_after } => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut resp =
                if body.trim_start().starts_with('{') { (code, body).into_response() } else { vllm_error(code, &body) };
            if let Some(ra) = retry_after {
                resp.headers_mut().insert("retry-after", HeaderValue::from_str(&ra.to_string()).unwrap());
            }
            resp.headers_mut().insert("content-type", HeaderValue::from_static("application/json"));
            resp
        }
        other => {
            let stream = req.stream();
            let events = events_for(&other, st.cfg.chunk_chars, &req);
            if !stream {
                return Json(non_stream_body(&other, &req)).into_response();
            }
            let (disconnect_after, stall) = match &other {
                MockReply::Disconnect { after_chunks, .. } => (Some(*after_chunks), None),
                MockReply::Stall { ms } => (None, Some(*ms)),
                _ => (None, None),
            };
            let delay = st.cfg.chunk_delay;
            let s = async_stream_events(events, disconnect_after, stall, delay);
            let mut resp = Response::new(Body::from_stream(s));
            resp.headers_mut().insert("content-type", HeaderValue::from_static("text/event-stream"));
            resp.headers_mut().insert("cache-control", HeaderValue::from_static("no-cache"));
            resp
        }
    }
}

fn async_stream_events(
    events: Vec<String>,
    disconnect_after: Option<usize>,
    stall: Option<u64>,
    delay: Duration,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    tokio::spawn(async move {
        if let Some(ms) = stall {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            return; // close without data
        }
        for (i, e) in events.iter().enumerate() {
            if let Some(n) = disconnect_after {
                if i >= n {
                    let _ =
                        tx.send(Err(std::io::Error::new(std::io::ErrorKind::ConnectionReset, "mock disconnect"))).await;
                    return;
                }
            }
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if tx.send(Ok(Bytes::from(format!("data: {e}\n\n")))).await.is_err() {
                return;
            }
        }
        if disconnect_after.is_none() {
            let _ = tx.send(Ok(Bytes::from_static(b"data: [DONE]\n\n"))).await;
        }
    });
    tokio_stream::wrappers::ReceiverStream::new(rx)
}

fn chunk_str(s: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(n.max(1)).map(|c| c.iter().collect()).collect()
}

fn base_chunk(id: &str, model: &str, delta: Value, finish: Option<&str>) -> String {
    json!({
        "id": id, "object": "chat.completion.chunk", "created": 1759600000, "model": model,
        "choices": [{"index": 0, "delta": delta, "logprobs": null, "finish_reason": finish}]
    })
    .to_string()
}

fn events_for(reply: &MockReply, n: usize, req: &RecordedRequest) -> Vec<String> {
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let model = req.body.get("model").and_then(|m| m.as_str()).unwrap_or("mock");
    let mut ev = vec![base_chunk(&id, model, json!({"role": "assistant", "content": ""}), None)];
    let mut completion = String::new();
    let mut finish = "stop";
    match reply {
        MockReply::Text { text } => {
            for c in chunk_str(text, n) {
                ev.push(base_chunk(&id, model, json!({"content": c}), None));
            }
            completion.push_str(text);
        }
        MockReply::Json { value } => {
            let text = value.to_string();
            for c in chunk_str(&text, n) {
                ev.push(base_chunk(&id, model, json!({"content": c}), None));
            }
            completion.push_str(&text);
        }
        MockReply::Reasoning { reasoning, text } => {
            for c in chunk_str(reasoning, n) {
                ev.push(base_chunk(&id, model, json!({"reasoning": c}), None));
            }
            for c in chunk_str(text, n) {
                ev.push(base_chunk(&id, model, json!({"content": c}), None));
            }
            completion.push_str(reasoning);
            completion.push_str(text);
        }
        MockReply::ThinkInContent { reasoning, text } => {
            let full = format!("<think>{reasoning}</think>\n\n{text}");
            for c in chunk_str(&full, n) {
                ev.push(base_chunk(&id, model, json!({"content": c}), None));
            }
            completion.push_str(&full);
        }
        MockReply::ContentToolCall { content } => {
            for c in chunk_str(content, n) {
                ev.push(base_chunk(&id, model, json!({"content": c}), None));
            }
            completion.push_str(content);
        }
        MockReply::ToolCalls { calls, text } => {
            if let Some(t) = text {
                for c in chunk_str(t, n) {
                    ev.push(base_chunk(&id, model, json!({"content": c}), None));
                }
                completion.push_str(t);
            }
            for (i, call) in calls.iter().enumerate() {
                let call_id = format!("chatcmpl-tool-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
                ev.push(base_chunk(
                    &id,
                    model,
                    json!({"tool_calls": [{"index": i, "id": call_id, "type": "function", "function": {"name": call.name}}]}),
                    None,
                ));
                let args = call.raw_arguments.clone().unwrap_or_else(|| call.arguments.to_string());
                for c in chunk_str(&args, n.max(4) * 2) {
                    ev.push(base_chunk(
                        &id,
                        model,
                        json!({"tool_calls": [{"index": i, "function": {"arguments": c}}]}),
                        None,
                    ));
                }
                completion.push_str(&call.name);
                completion.push_str(&args);
            }
            finish = "tool_calls";
        }
        MockReply::Disconnect { partial, .. } => {
            return events_for(partial, n, req);
        }
        MockReply::RawSse { events } => return events.clone(),
        MockReply::Stall { .. } | MockReply::Error { .. } => {}
    }
    ev.push(base_chunk(&id, model, json!({}), Some(finish)));
    let ct = completion_tokens(&completion);
    ev.push(
        json!({"id": id, "object": "chat.completion.chunk", "created": 1759600000, "model": model, "choices": [],
               "usage": {"prompt_tokens": req.prompt_tokens, "completion_tokens": ct, "total_tokens": req.prompt_tokens + ct}})
        .to_string(),
    );
    ev
}

fn non_stream_body(reply: &MockReply, req: &RecordedRequest) -> Value {
    let model = req.body.get("model").and_then(|m| m.as_str()).unwrap_or("mock");
    let mut message = json!({"role": "assistant", "content": null});
    let mut finish = "stop";
    let mut completion = String::new();
    match reply {
        MockReply::Text { text } | MockReply::ContentToolCall { content: text } => {
            message["content"] = json!(text);
            completion = text.clone();
        }
        MockReply::Json { value } => {
            message["content"] = json!(value.to_string());
            completion = value.to_string();
        }
        MockReply::Reasoning { reasoning, text } => {
            message["content"] = json!(text);
            message["reasoning"] = json!(reasoning);
            completion = format!("{reasoning}{text}");
        }
        MockReply::ThinkInContent { reasoning, text } => {
            message["content"] = json!(format!("<think>{reasoning}</think>\n\n{text}"));
        }
        MockReply::ToolCalls { calls, text } => {
            message["content"] = json!(text);
            message["tool_calls"] = Value::Array(
                calls
                    .iter()
                    .map(|c| {
                        json!({"id": format!("chatcmpl-tool-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]), "type": "function",
                               "function": {"name": c.name, "arguments": c.raw_arguments.clone().unwrap_or_else(|| c.arguments.to_string())}})
                    })
                    .collect(),
            );
            finish = "tool_calls";
        }
        _ => {}
    }
    let ct = completion_tokens(&completion);
    json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()), "object": "chat.completion", "created": 1759600000, "model": model,
        "choices": [{"index": 0, "message": message, "finish_reason": finish}],
        "usage": {"prompt_tokens": req.prompt_tokens, "completion_tokens": ct, "total_tokens": req.prompt_tokens + ct}
    })
}

async fn ctl_push(State(st): State<MockState>, Json(body): Json<Value>) -> Result<StatusCode, (StatusCode, String)> {
    let replies: Vec<MockReply> = if body.is_array() {
        serde_json::from_value(body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    } else {
        vec![serde_json::from_value(body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?]
    };
    st.inner.lock().unwrap().queue.extend(replies);
    Ok(StatusCode::NO_CONTENT)
}

async fn ctl_rules(State(st): State<MockState>, Json(body): Json<Value>) -> Result<StatusCode, (StatusCode, String)> {
    let policy = RulePolicy::from_json(&body).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let p = Arc::new(policy);
    st.inner.lock().unwrap().policy = Some(Arc::new(move |r: &RecordedRequest| p.reply(r)));
    Ok(StatusCode::NO_CONTENT)
}

async fn ctl_requests(State(st): State<MockState>) -> Json<Value> {
    let reqs = st.inner.lock().unwrap().requests.clone();
    Json(serde_json::to_value(reqs).unwrap_or_default())
}

async fn ctl_reset(State(st): State<MockState>) -> StatusCode {
    let mut i = st.inner.lock().unwrap();
    i.queue.clear();
    i.requests.clear();
    i.policy = None;
    i.max_len_override = None;
    StatusCode::NO_CONTENT
}
