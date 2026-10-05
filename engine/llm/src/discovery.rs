//! Endpoint discovery: `/v1/models`, `/health`, `/version`, `/tokenize`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use odex_protocol::DiscoveredModel;

use crate::client::LlmClient;
use crate::error::{extract_message, LlmError};
use crate::types::ChatMessage;

#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub reachable: bool,
    pub healthy: Option<bool>,
    pub version: Option<String>,
    pub models: Vec<DiscoveredModel>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

pub async fn list_models(c: &LlmClient) -> Result<Vec<DiscoveredModel>, LlmError> {
    let r = c
        .request(reqwest::Method::GET, c.url("models"))
        .timeout(PROBE_TIMEOUT)
        .send()
        .await
        .map_err(|e| LlmError::Connect(e.to_string()))?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(LlmError::Unauthorized(extract_message(&text)));
    }
    if !status.is_success() {
        return Err(LlmError::Http { status: status.as_u16(), body: extract_message(&text) });
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| LlmError::Other(format!("bad /models JSON: {e}")))?;
    let data = v.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    Ok(data
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_string();
            Some(DiscoveredModel {
                id,
                max_model_len: m
                    .get("max_model_len")
                    .or_else(|| m.get("context_length"))
                    .or_else(|| m.get("context_window"))
                    .and_then(|x| x.as_u64())
                    .map(|x| x as u32),
                owned_by: m.get("owned_by").and_then(|x| x.as_str()).map(String::from),
                root: m.get("root").and_then(|x| x.as_str()).map(String::from),
            })
        })
        .collect())
}

pub async fn health(c: &LlmClient) -> Option<bool> {
    let r = c.request(reqwest::Method::GET, c.root("health")).timeout(PROBE_TIMEOUT).send().await.ok()?;
    match r.status().as_u16() {
        200 => Some(true),
        404 => None, // not vLLM, or proxied without /health
        _ => Some(false),
    }
}

pub async fn version(c: &LlmClient) -> Option<String> {
    let r = c.request(reqwest::Method::GET, c.root("version")).timeout(PROBE_TIMEOUT).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let v: Value = r.json().await.ok()?;
    v.get("version").and_then(|x| x.as_str()).map(String::from)
}

pub async fn probe(c: &LlmClient) -> Probe {
    let t0 = Instant::now();
    let (models, healthy, version) = tokio::join!(list_models(c), health(c), version(c));
    let latency_ms = t0.elapsed().as_millis() as u64;
    match models {
        Ok(models) => Probe { reachable: true, healthy, version, models, latency_ms, error: None },
        Err(e) => Probe {
            reachable: !matches!(e, LlmError::Connect(_)),
            healthy,
            version,
            models: vec![],
            latency_ms,
            error: Some(e.to_string()),
        },
    }
}

/// Exact prompt token count via vLLM's `/tokenize` (chat form applies the template).
pub async fn tokenize_messages(
    c: &LlmClient,
    model: &str,
    messages: &[ChatMessage],
    tools: Option<&Value>,
) -> Result<u32, LlmError> {
    let msgs: Vec<Value> = messages.iter().map(|m| m.to_wire(true)).collect();
    let mut body = json!({"model": model, "messages": msgs, "add_generation_prompt": true});
    if let Some(t) = tools {
        body["tools"] = t.clone();
    }
    tokenize_body(c, body).await
}

pub async fn tokenize_text(c: &LlmClient, model: &str, text: &str) -> Result<u32, LlmError> {
    tokenize_body(c, json!({"model": model, "prompt": text, "add_special_tokens": false})).await
}

async fn tokenize_body(c: &LlmClient, body: Value) -> Result<u32, LlmError> {
    let r = c
        .request(reqwest::Method::POST, c.root("tokenize"))
        .timeout(Duration::from_secs(20))
        .json(&body)
        .send()
        .await
        .map_err(|e| LlmError::Connect(e.to_string()))?;
    let status = r.status();
    let text = r.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(LlmError::Http { status: status.as_u16(), body: extract_message(&text) });
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| LlmError::Other(e.to_string()))?;
    v.get("count")
        .and_then(|c| c.as_u64())
        .or_else(|| v.get("tokens").and_then(|t| t.as_array()).map(|a| a.len() as u64))
        .map(|n| n as u32)
        .ok_or_else(|| LlmError::Other("tokenize: no count".into()))
}
