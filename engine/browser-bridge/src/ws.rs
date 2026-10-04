//! Direct CDP over WebSocket to a Chromium-based browser.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::{CdpEvent, CdpTransport, TabInfo};

type Sink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type Reply = std::result::Result<Value, String>;

const MAX_BUFFERED_EVENTS: usize = 10_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct Attached {
    target_id: String,
    session_id: String,
}

struct Inner {
    sink: tokio::sync::Mutex<Sink>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    events: Mutex<VecDeque<CdpEvent>>,
    current: Mutex<Option<Attached>>,
    /// target id -> flattened session id
    sessions: Mutex<HashMap<String, String>>,
    closed: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// CDP over a browser-level WebSocket, using flattened target sessions.
pub struct WsTransport {
    inner: Arc<Inner>,
    reader: tokio::task::JoinHandle<()>,
    http_base: Option<String>,
}

impl Drop for WsTransport {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl WsTransport {
    /// Connect to `http://127.0.0.1:9222` (resolved through `/json/version`),
    /// a `ws://.../devtools/browser/...` URL, or a page WebSocket URL (whose
    /// browser endpoint is looked up). Attaches to the first page (creating
    /// one if needed) and enables the domains the session needs.
    pub async fn connect(endpoint: &str) -> Result<Self> {
        let (ws_url, http_base) = resolve_browser_ws(endpoint).await?;
        let config = WebSocketConfig {
            max_message_size: Some(512 << 20),
            max_frame_size: Some(256 << 20),
            ..Default::default()
        };
        let (stream, _) = tokio_tungstenite::connect_async_with_config(ws_url.as_str(), Some(config), true)
            .await
            .with_context(|| format!("connecting to DevTools at {ws_url}"))?;
        let (sink, mut source) = stream.split();
        let inner = Arc::new(Inner {
            sink: tokio::sync::Mutex::new(sink),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            events: Mutex::new(VecDeque::new()),
            current: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        let reader_inner = inner.clone();
        let reader = tokio::spawn(async move {
            while let Some(msg) = source.next().await {
                match msg {
                    Ok(Message::Text(text)) => handle_message(&reader_inner, &text),
                    Ok(Message::Binary(bytes)) => {
                        if let Ok(text) = std::str::from_utf8(&bytes) {
                            handle_message(&reader_inner, text);
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            reader_inner.closed.store(true, Ordering::SeqCst);
            for (_, tx) in lock(&reader_inner.pending).drain() {
                let _ = tx.send(Err("DevTools connection closed".to_string()));
            }
        });
        let transport = Self { inner, reader, http_base };

        let pages = transport.page_targets().await?;
        let target = match pages.first() {
            Some(t) => t.id.clone(),
            None => transport.create_target("about:blank").await?,
        };
        transport.attach(&target).await?;
        Ok(transport)
    }

    /// The `http://host:port` DevTools endpoint, when known.
    pub fn http_endpoint(&self) -> Option<&str> {
        self.http_base.as_deref()
    }

    /// Send a browser-level command (no target session), e.g. `Browser.getVersion`.
    pub async fn browser_command(&self, method: &str, params: Value) -> Result<Value> {
        self.call(method, params, None, COMMAND_TIMEOUT).await
    }

    /// Ask the browser to exit (`Browser.close`). Errors are ignored: the
    /// connection usually drops before a reply arrives.
    pub async fn close_browser(&self) {
        let _ = self.call("Browser.close", json!({}), None, Duration::from_secs(3)).await;
    }

    async fn call(&self, method: &str, params: Value, session: Option<&str>, timeout: Duration) -> Result<Value> {
        if self.inner.closed.load(Ordering::SeqCst) {
            bail!("DevTools connection is closed");
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        let (tx, rx) = oneshot::channel();
        lock(&self.inner.pending).insert(id, tx);
        let sent = self.inner.sink.lock().await.send(Message::Text(msg.to_string())).await;
        if let Err(e) = sent {
            lock(&self.inner.pending).remove(&id);
            return Err(anyhow!("sending {method}: {e}"));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(anyhow!("{method}: {e}")),
            Ok(Err(_)) => Err(anyhow!("{method}: DevTools connection closed")),
            Err(_) => {
                lock(&self.inner.pending).remove(&id);
                Err(anyhow!("{method}: timed out after {}s", timeout.as_secs()))
            }
        }
    }

    fn current(&self) -> Option<Attached> {
        lock(&self.inner.current).clone()
    }

    async fn page_targets(&self) -> Result<Vec<TabInfo>> {
        let current = self.current().map(|c| c.target_id);
        let infos = match self.browser_command("Target.getTargets", json!({})).await {
            Ok(v) => v["targetInfos"].as_array().cloned().unwrap_or_default(),
            Err(e) => match &self.http_base {
                // Fall back to the HTTP listing (same fields, different names).
                Some(base) => http_json(&format!("{base}/json/list"))
                    .await
                    .with_context(|| format!("listing targets ({e})"))?
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|t| json!({"targetId": t["id"], "type": t["type"], "url": t["url"], "title": t["title"]}))
                    .collect(),
                None => return Err(e),
            },
        };
        Ok(infos
            .into_iter()
            .filter(|t| t["type"] == "page")
            .filter(|t| !t["url"].as_str().unwrap_or_default().starts_with("devtools://"))
            .map(|t| {
                let id = t["targetId"].as_str().unwrap_or_default().to_string();
                TabInfo {
                    active: current.as_deref() == Some(id.as_str()),
                    url: t["url"].as_str().unwrap_or_default().to_string(),
                    title: t["title"].as_str().unwrap_or_default().to_string(),
                    id,
                }
            })
            .collect())
    }

    async fn create_target(&self, url: &str) -> Result<String> {
        let v = self.browser_command("Target.createTarget", json!({ "url": url })).await?;
        v["targetId"].as_str().map(str::to_string).ok_or_else(|| anyhow!("Target.createTarget returned no targetId"))
    }

    /// Attach (or re-use the session for) a page target and make it current.
    async fn attach(&self, target_id: &str) -> Result<()> {
        let existing = lock(&self.inner.sessions).get(target_id).cloned();
        if let Some(session_id) = existing {
            *lock(&self.inner.current) = Some(Attached { target_id: target_id.to_string(), session_id });
            lock(&self.inner.events).clear();
            return Ok(());
        }
        let v =
            self.browser_command("Target.attachToTarget", json!({ "targetId": target_id, "flatten": true })).await?;
        let session_id =
            v["sessionId"].as_str().ok_or_else(|| anyhow!("Target.attachToTarget returned no sessionId"))?.to_string();
        lock(&self.inner.sessions).insert(target_id.to_string(), session_id.clone());
        *lock(&self.inner.current) =
            Some(Attached { target_id: target_id.to_string(), session_id: session_id.clone() });
        lock(&self.inner.events).clear();

        let s = Some(session_id.as_str());
        let t = Duration::from_secs(15);
        for required in ["Page.enable", "Runtime.enable"] {
            self.call(required, json!({}), s, t).await?;
        }
        for optional in ["Network.enable", "DOM.enable", "Accessibility.enable", "Log.enable"] {
            let _ = self.call(optional, json!({}), s, t).await;
        }
        let _ = self.call("Page.setLifecycleEventsEnabled", json!({ "enabled": true }), s, t).await;
        // Lets focus/typing work even when the window is not focused.
        let _ = self.call("Emulation.setFocusEmulationEnabled", json!({ "enabled": true }), s, t).await;
        Ok(())
    }
}

#[async_trait]
impl CdpTransport for WsTransport {
    async fn send(&self, method: &str, params: Value) -> Result<Value> {
        let current = self.current().ok_or_else(|| anyhow!("no page is attached (the tab was closed or crashed)"))?;
        self.call(method, params, Some(&current.session_id), COMMAND_TIMEOUT).await
    }

    async fn drain_events(&self) -> Vec<CdpEvent> {
        lock(&self.inner.events).drain(..).collect()
    }

    async fn tabs(&self) -> Result<Vec<TabInfo>> {
        self.page_targets().await
    }

    async fn new_tab(&self, url: &str) -> Result<TabInfo> {
        let url = if url.trim().is_empty() { "about:blank" } else { url };
        let id = self.create_target(url).await?;
        self.attach(&id).await?;
        let _ = self.browser_command("Target.activateTarget", json!({ "targetId": id })).await;
        Ok(TabInfo { id, url: url.to_string(), title: String::new(), active: true })
    }

    async fn select_tab(&self, id: &str) -> Result<()> {
        let pages = self.page_targets().await?;
        if !pages.iter().any(|p| p.id == id) {
            bail!("no tab with id {id}");
        }
        self.attach(id).await?;
        let _ = self.browser_command("Target.activateTarget", json!({ "targetId": id })).await;
        Ok(())
    }

    async fn close_tab(&self, id: &str) -> Result<()> {
        self.browser_command("Target.closeTarget", json!({ "targetId": id })).await?;
        lock(&self.inner.sessions).remove(id);
        let was_current = self.current().is_some_and(|c| c.target_id == id);
        if was_current {
            *lock(&self.inner.current) = None;
            // The target list may still contain the closing tab for a moment.
            let mut next = None;
            for _ in 0..20 {
                let pages = self.page_targets().await?;
                if !pages.iter().any(|p| p.id == id) {
                    next = pages.into_iter().next().map(|p| p.id);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let next = match next {
                Some(n) => n,
                None => self.create_target("about:blank").await?,
            };
            self.attach(&next).await?;
        }
        Ok(())
    }
}

fn handle_message(inner: &Inner, text: &str) {
    let Ok(msg) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        if let Some(tx) = lock(&inner.pending).remove(&id) {
            let reply = match msg.get("error") {
                Some(err) => {
                    let mut text = err["message"].as_str().unwrap_or("CDP error").to_string();
                    if let Some(data) = err.get("data").and_then(Value::as_str) {
                        text.push_str(&format!(" ({data})"));
                    }
                    Err(text)
                }
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(reply);
        }
        return;
    }
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return;
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "Target.detachedFromTarget" | "Target.targetDestroyed" | "Target.targetCrashed" => {
            let target = params["targetId"].as_str().map(str::to_string);
            let session = params["sessionId"].as_str().map(str::to_string);
            let mut sessions = lock(&inner.sessions);
            sessions.retain(|t, s| Some(t) != target.as_ref() && Some(&*s) != session.as_ref());
            let mut current = lock(&inner.current);
            let gone = current
                .as_ref()
                .is_some_and(|c| Some(&c.target_id) == target.as_ref() || Some(&c.session_id) == session.as_ref());
            if gone {
                *current = None;
            }
            return;
        }
        _ => {}
    }
    let Some(session) = msg.get("sessionId").and_then(Value::as_str) else {
        return;
    };
    let is_current = lock(&inner.current).as_ref().is_some_and(|c| c.session_id == session);
    if !is_current {
        return;
    }
    let mut events = lock(&inner.events);
    if events.len() >= MAX_BUFFERED_EVENTS {
        events.pop_front();
    }
    events.push_back(CdpEvent { method: method.to_string(), params });
}

async fn http_json(url: &str) -> Result<Value> {
    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build()?;
    let resp = client.get(url).send().await?.error_for_status()?;
    Ok(resp.json().await?)
}

/// Returns (browser WebSocket URL, http base).
async fn resolve_browser_ws(endpoint: &str) -> Result<(String, Option<String>)> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    let lower = endpoint.to_ascii_lowercase();
    if lower.starts_with("ws://") || lower.starts_with("wss://") {
        let url = url::Url::parse(endpoint).context("invalid DevTools WebSocket URL")?;
        let http_base = url.host_str().map(|h| {
            let scheme = if url.scheme() == "wss" { "https" } else { "http" };
            match url.port() {
                Some(p) => format!("{scheme}://{h}:{p}"),
                None => format!("{scheme}://{h}"),
            }
        });
        if url.path().starts_with("/devtools/browser/") {
            return Ok((endpoint.to_string(), http_base));
        }
        let base = http_base.ok_or_else(|| anyhow!("cannot derive the DevTools HTTP endpoint from {endpoint}"))?;
        let ws = browser_ws_from_http(&base).await?;
        return Ok((ws, Some(base)));
    }
    let base = if lower.starts_with("http://") || lower.starts_with("https://") {
        endpoint.to_string()
    } else {
        format!("http://{endpoint}")
    };
    // Strip any path such as /json/version that the caller included.
    let base = match url::Url::parse(&base) {
        Ok(u) => match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{}://{h}:{p}", u.scheme()),
            (Some(h), None) => format!("{}://{h}", u.scheme()),
            _ => base,
        },
        Err(_) => base,
    };
    let ws = browser_ws_from_http(&base).await?;
    Ok((ws, Some(base)))
}

async fn browser_ws_from_http(base: &str) -> Result<String> {
    let mut last_err = None;
    // A just-launched browser may need a moment before it answers.
    for _ in 0..25 {
        match http_json(&format!("{base}/json/version")).await {
            Ok(v) => {
                if let Some(ws) = v["webSocketDebuggerUrl"].as_str() {
                    return Ok(ws.to_string());
                }
                last_err = Some(anyhow!("{base}/json/version has no webSocketDebuggerUrl"));
            }
            Err(e) => last_err = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(last_err.unwrap_or_else(|| anyhow!("DevTools endpoint {base} did not respond")))
        .with_context(|| format!("resolving the browser WebSocket from {base}/json/version"))
}
