//! HTTP client for one OpenAI-compatible endpoint (vLLM).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use futures::StreamExt;
use rand::Rng;
use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use odex_config::{ResolvedModel, ResolvedProvider};
use odex_protocol::StructuredOutputMode;

use crate::error::{extract_message, hint_for, parse_overflow, LlmError};
use crate::stream::{Assembler, SseDecoder};
use crate::types::{ChatEvent, ChatRequest, ChatResponse};

#[derive(Debug, Default)]
pub struct EndpointStats {
    pub in_flight: AtomicU32,
    pub queued: AtomicU32,
    pub requests: AtomicU32,
    pub failures: AtomicU32,
}

pub struct LlmClient {
    pub provider: ResolvedProvider,
    http: reqwest::Client,
    sem: Arc<Semaphore>,
    pub stats: Arc<EndpointStats>,
    api_key: RwLock<Option<String>>,
}

enum Attempt {
    Ok(ChatResponse),
    Retry { reason: String, retry_after: Option<Duration>, stream: bool },
    Fail(LlmError),
}

impl LlmClient {
    pub fn new(provider: ResolvedProvider, api_key: Option<String>) -> Self {
        let mut b = reqwest::Client::builder()
            .connect_timeout(provider.connect_timeout)
            .pool_idle_timeout(Duration::from_secs(90));
        if let Some(t) = provider.request_timeout {
            b = b.timeout(t);
        }
        let http = b.build().unwrap_or_else(|_| reqwest::Client::new());
        let key = api_key.or_else(|| provider.api_key.clone());
        Self {
            sem: Arc::new(Semaphore::new(provider.max_concurrent_requests as usize)),
            provider,
            http,
            stats: Arc::new(EndpointStats::default()),
            api_key: RwLock::new(key),
        }
    }

    pub fn set_api_key(&self, key: Option<String>) {
        *self.api_key.write().unwrap() = key.filter(|k| !k.is_empty());
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.read().unwrap().is_some()
    }

    pub fn base_url(&self) -> &str {
        &self.provider.base_url
    }

    /// Server root (base URL without a trailing `/v1`), where vLLM serves
    /// `/health`, `/version` and `/tokenize`.
    pub fn root_url(&self) -> String {
        let b = self.provider.base_url.trim_end_matches('/');
        b.strip_suffix("/v1").unwrap_or(b).to_string()
    }

    pub fn url(&self, path: &str) -> String {
        let mut u = format!("{}/{}", self.provider.base_url.trim_end_matches('/'), path.trim_start_matches('/'));
        self.append_query(&mut u);
        u
    }

    pub fn root(&self, path: &str) -> String {
        let mut u = format!("{}/{}", self.root_url(), path.trim_start_matches('/'));
        self.append_query(&mut u);
        u
    }

    fn append_query(&self, u: &mut String) {
        if self.provider.query_params.is_empty() {
            return;
        }
        let q: Vec<String> =
            self.provider.query_params.iter().map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v))).collect();
        u.push(if u.contains('?') { '&' } else { '?' });
        u.push_str(&q.join("&"));
    }

    pub fn request(&self, method: reqwest::Method, url: String) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, url);
        for (k, v) in &self.provider.headers {
            r = r.header(k, v);
        }
        if let Some(k) = self.api_key.read().unwrap().as_ref() {
            r = r.bearer_auth(k);
        }
        r
    }

    /// Build the `/chat/completions` body.
    pub fn build_body(&self, model: &ResolvedModel, req: &ChatRequest, stream: bool) -> Value {
        let mut body = Map::new();
        body.insert("model".into(), json!(model.model_id));
        let msgs: Vec<Value> = req.messages.iter().map(|m| m.to_wire(req.include_reasoning)).collect();
        body.insert("messages".into(), Value::Array(msgs));
        body.insert("stream".into(), json!(stream));
        if stream {
            body.insert("stream_options".into(), json!({"include_usage": true}));
        }
        if !req.tools.is_empty() && model.capabilities.tools {
            body.insert("tools".into(), Value::Array(req.tools.iter().map(|t| t.to_wire()).collect()));
            body.insert("tool_choice".into(), json!(if req.tool_choice_none { "none" } else { "auto" }));
            if model.capabilities.parallel_tools {
                body.insert("parallel_tool_calls".into(), json!(true));
            }
        }
        if let Some(mt) = req.max_tokens {
            body.insert("max_tokens".into(), json!(mt));
        }
        let s = &model.sampling;
        if let Some(t) = req.temperature_override.or(s.temperature) {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(v) = s.top_p {
            body.insert("top_p".into(), json!(v));
        }
        if let Some(v) = s.top_k {
            body.insert("top_k".into(), json!(v));
        }
        if let Some(v) = s.min_p {
            body.insert("min_p".into(), json!(v));
        }
        if let Some(v) = s.repetition_penalty {
            body.insert("repetition_penalty".into(), json!(v));
        }
        if let Some(v) = s.presence_penalty {
            body.insert("presence_penalty".into(), json!(v));
        }
        if let Some(v) = s.frequency_penalty {
            body.insert("frequency_penalty".into(), json!(v));
        }
        if let Some(k) = &model.chat_template_kwargs {
            body.insert("chat_template_kwargs".into(), k.clone());
        }
        if let Some(so) = &req.structured {
            match model.structured_output {
                StructuredOutputMode::None => {}
                StructuredOutputMode::GuidedJson => {
                    body.insert("guided_json".into(), so.schema.clone());
                }
                StructuredOutputMode::JsonSchema | StructuredOutputMode::Auto => {
                    body.insert(
                        "response_format".into(),
                        json!({"type": "json_schema", "json_schema": {"name": so.name, "schema": so.schema, "strict": true}}),
                    );
                }
            }
        }
        let mut v = Value::Object(body);
        // effort mapping → extra body → request extra (in that order)
        let effort = req.effort.or(model.default_effort);
        if let Some(e) = effort {
            if let Some(extra) = model.reasoning_effort_map.get(&e) {
                merge_deep(&mut v, extra);
            }
        }
        if let Some(extra) = &model.extra_body {
            merge_deep(&mut v, extra);
        }
        if let Some(extra) = &req.extra {
            merge_deep(&mut v, extra);
        }
        v
    }

    /// Stream a chat completion, forwarding events. Retries transient
    /// failures (429/5xx/resets/idle/mid-stream drops) with backoff + jitter.
    pub async fn stream_chat(
        &self,
        model: &ResolvedModel,
        req: &ChatRequest,
        cancel: &CancellationToken,
        on_event: &mut (dyn FnMut(ChatEvent) + Send),
    ) -> Result<ChatResponse, LlmError> {
        let body = self.build_body(model, req, true);
        // Any model may leak `<think>` tags; only reasoning models with thinking on
        // get early content held back to detect a prefilled thought.
        let strip_think = true;
        let effort = req.effort.or(model.default_effort);
        let hold = model.capabilities.reasoning
            && !matches!(
                effort,
                Some(odex_protocol::ReasoningEffort::None) | Some(odex_protocol::ReasoningEffort::Minimal)
            );
        let mut req_attempts = 0u32;
        let mut stream_attempts = 0u32;
        let started = Instant::now();
        loop {
            if cancel.is_cancelled() {
                return Err(LlmError::Cancelled);
            }
            self.stats.queued.fetch_add(1, Ordering::Relaxed);
            let permit = tokio::select! {
                p = self.sem.clone().acquire_owned() => p,
                _ = cancel.cancelled() => {
                    self.stats.queued.fetch_sub(1, Ordering::Relaxed);
                    return Err(LlmError::Cancelled);
                }
            };
            self.stats.queued.fetch_sub(1, Ordering::Relaxed);
            let _permit = permit.map_err(|_| LlmError::Other("client closed".into()))?;
            self.stats.in_flight.fetch_add(1, Ordering::Relaxed);
            self.stats.requests.fetch_add(1, Ordering::Relaxed);
            let res = self.attempt(&body, req, (strip_think, hold), cancel, on_event).await;
            self.stats.in_flight.fetch_sub(1, Ordering::Relaxed);
            drop(_permit);
            match res {
                Attempt::Ok(mut r) => {
                    r.total_ms = started.elapsed().as_millis() as u64;
                    r.retries = req_attempts + stream_attempts;
                    return Ok(r);
                }
                Attempt::Fail(e) => {
                    self.stats.failures.fetch_add(1, Ordering::Relaxed);
                    return Err(e);
                }
                Attempt::Retry { reason, retry_after, stream } => {
                    self.stats.failures.fetch_add(1, Ordering::Relaxed);
                    let (n, max) = if stream {
                        stream_attempts += 1;
                        (stream_attempts, self.provider.stream_max_retries)
                    } else {
                        req_attempts += 1;
                        (req_attempts, self.provider.request_max_retries)
                    };
                    if n > max {
                        return Err(LlmError::Exhausted { attempts: req_attempts + stream_attempts, last: reason });
                    }
                    let delay = backoff(n, retry_after);
                    on_event(ChatEvent::Retrying {
                        attempt: req_attempts + stream_attempts,
                        reason: reason.clone(),
                        delay_ms: delay.as_millis() as u64,
                    });
                    tracing::warn!(endpoint = %self.provider.id, "retrying in {delay:?}: {reason}");
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel.cancelled() => return Err(LlmError::Cancelled),
                    }
                }
            }
        }
    }

    /// Non-streaming convenience wrapper (still streams internally).
    pub async fn chat(
        &self,
        model: &ResolvedModel,
        req: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, LlmError> {
        let mut sink = |_e: ChatEvent| {};
        self.stream_chat(model, req, cancel, &mut sink).await
    }

    async fn attempt(
        &self,
        body: &Value,
        req: &ChatRequest,
        (strip_think, hold): (bool, bool),
        cancel: &CancellationToken,
        on_event: &mut (dyn FnMut(ChatEvent) + Send),
    ) -> Attempt {
        let t0 = Instant::now();
        let send = self.request(reqwest::Method::POST, self.url("chat/completions")).json(body).send();
        let resp = tokio::select! {
            r = send => r,
            _ = cancel.cancelled() => return Attempt::Fail(LlmError::Cancelled),
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                let msg = describe_reqwest(&e);
                return if e.is_connect() || e.is_timeout() || e.is_request() {
                    Attempt::Retry { reason: msg, retry_after: None, stream: false }
                } else {
                    Attempt::Retry { reason: msg, retry_after: None, stream: true }
                };
            }
        };
        let status = resp.status();
        if !status.is_success() {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<f64>().ok())
                .map(|s| Duration::from_millis((s * 1000.0) as u64));
            let text = resp.text().await.unwrap_or_default();
            let code = status.as_u16();
            return match code {
                400 | 413 | 422 => {
                    if let Some(o) = parse_overflow(&text) {
                        Attempt::Fail(LlmError::ContextOverflow(o))
                    } else {
                        let mut m = extract_message(&text);
                        if let Some(h) = hint_for(&text) {
                            m = format!("{m} — {h}");
                        }
                        Attempt::Fail(LlmError::BadRequest(m))
                    }
                }
                401 | 403 => Attempt::Fail(LlmError::Unauthorized(extract_message(&text))),
                404 => Attempt::Fail(LlmError::ModelNotFound(extract_message(&text))),
                408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 520..=529 => Attempt::Retry {
                    reason: format!("HTTP {code}: {}", extract_message(&text)),
                    retry_after,
                    stream: false,
                },
                _ => Attempt::Fail(LlmError::Http { status: code, body: extract_message(&text) }),
            };
        }

        let mut asm = Assembler::with_options(strip_think, hold, !req.tools.is_empty());
        let mut sse = SseDecoder::default();
        let mut stream = resp.bytes_stream();
        let mut ttft: Option<u64> = None;
        let idle = self.provider.stream_idle_timeout;
        let mut utf8_buf: Vec<u8> = Vec::new();
        loop {
            let next = tokio::select! {
                n = tokio::time::timeout(idle, stream.next()) => n,
                _ = cancel.cancelled() => return Attempt::Fail(LlmError::Cancelled),
            };
            match next {
                Err(_) => {
                    return Attempt::Retry {
                        reason: format!("no data for {}s (idle stream timeout)", idle.as_secs()),
                        retry_after: None,
                        stream: true,
                    }
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    return Attempt::Retry {
                        reason: format!("stream dropped: {}", describe_reqwest(&e)),
                        retry_after: None,
                        stream: true,
                    }
                }
                Ok(Some(Ok(bytes))) => {
                    utf8_buf.extend_from_slice(&bytes);
                    // decode only complete UTF-8 sequences
                    let valid_up_to = match std::str::from_utf8(&utf8_buf) {
                        Ok(_) => utf8_buf.len(),
                        Err(e) => e.valid_up_to(),
                    };
                    let text = String::from_utf8_lossy(&utf8_buf[..valid_up_to]).to_string();
                    utf8_buf.drain(..valid_up_to);
                    for payload in sse.feed(&text) {
                        for ev in asm.push(&payload) {
                            if ttft.is_none()
                                && matches!(
                                    ev,
                                    ChatEvent::ContentDelta(_)
                                        | ChatEvent::ReasoningDelta(_)
                                        | ChatEvent::ToolCallStart { .. }
                                )
                            {
                                ttft = Some(t0.elapsed().as_millis() as u64);
                            }
                            if !matches!(&ev, ChatEvent::ContentDelta(s) if s.is_empty()) {
                                on_event(ev);
                            }
                        }
                    }
                    if asm.saw_done {
                        break;
                    }
                }
            }
        }
        if let Some(rest) = sse.finish() {
            for ev in asm.push(&rest) {
                on_event(ev);
            }
        }
        if !asm.saw_done && !asm.has_output() {
            return Attempt::Retry { reason: "stream ended without data".into(), retry_after: None, stream: true };
        }
        if !asm.saw_done {
            // vLLM always terminates with [DONE]; a missing terminator means a drop.
            return Attempt::Retry {
                reason: "stream ended early (connection reset)".into(),
                retry_after: None,
                stream: true,
            };
        }
        let (mut resp, tail) = asm.finish(&req.tools);
        for ev in tail {
            on_event(ev);
        }
        resp.ttft_ms = ttft;
        on_event(ChatEvent::Done(resp.clone()));
        Attempt::Ok(resp)
    }
}

fn describe_reqwest(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    s
}

/// Exponential backoff with full jitter, honoring Retry-After.
pub fn backoff(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(r) = retry_after {
        return r.min(Duration::from_secs(60));
    }
    let base = 400u64 * 2u64.saturating_pow(attempt.saturating_sub(1).min(8));
    let cap = base.min(20_000);
    let jitter = rand::thread_rng().gen_range(cap / 2..=cap);
    Duration::from_millis(jitter)
}

pub fn merge_deep(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge_deep(existing, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, StructuredOutput, ToolSpec};
    use odex_config::Presets;
    use odex_protocol::config_types::ModelProviderToml;
    use odex_protocol::ReasoningEffort;

    fn client() -> LlmClient {
        LlmClient::new(ResolvedProvider::from_toml("p", &ModelProviderToml::default()), None)
    }

    #[test]
    fn urls() {
        let c = client();
        assert_eq!(c.url("chat/completions"), "http://localhost:8000/v1/chat/completions");
        assert_eq!(c.root("health"), "http://localhost:8000/health");
    }

    #[test]
    fn body_has_effort_mapping_and_tools() {
        let presets = Presets::builtin();
        let m = ResolvedModel::discovered("p", "Qwen/Qwen3-32B", &presets);
        let req = ChatRequest {
            messages: vec![ChatMessage::user("hi")],
            tools: vec![ToolSpec { name: "t".into(), description: "d".into(), parameters: json!({"type":"object"}) }],
            max_tokens: Some(100),
            effort: Some(ReasoningEffort::None),
            structured: Some(StructuredOutput { name: "s".into(), schema: json!({"type":"object"}) }),
            ..Default::default()
        };
        let b = client().build_body(&m, &req, true);
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], json!(false));
        assert_eq!(b["tool_choice"], json!("auto"));
        assert_eq!(b["max_tokens"], json!(100));
        assert_eq!(b["stream_options"]["include_usage"], json!(true));
        assert_eq!(b["response_format"]["type"], json!("json_schema"));
        assert_eq!(b["top_k"], json!(20));
    }

    #[test]
    fn backoff_bounds() {
        for a in 1..10 {
            let d = backoff(a, None);
            assert!(d <= Duration::from_secs(20));
        }
        assert_eq!(backoff(1, Some(Duration::from_secs(3))), Duration::from_secs(3));
    }
}
