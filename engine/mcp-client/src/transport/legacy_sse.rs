//! Legacy HTTP+SSE transport (MCP 2024-11-05): a long-lived GET SSE stream
//! whose first `endpoint` event names the URL to POST messages to.
//! Used as a fallback when a server rejects Streamable HTTP POSTs.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{ACCEPT, WWW_AUTHENTICATE};
use serde_json::Value;
use tokio::sync::oneshot;
use url::Url;

use super::http::{pump_sse, HttpShared};
use super::{truncate_body, AbortOnDrop, Inbound, Transport};
use crate::error::McpError;

pub(crate) struct LegacySse {
    shared: Arc<HttpShared>,
    endpoint: Url,
    reader: Mutex<Option<AbortOnDrop>>,
}

fn unauthorized(resp: &reqwest::Response) -> McpError {
    McpError::Unauthorized {
        www_authenticate: resp.headers().get(WWW_AUTHENTICATE).and_then(|v| v.to_str().ok()).map(str::to_string),
    }
}

impl LegacySse {
    pub(crate) async fn connect(shared: HttpShared, timeout: Duration) -> Result<Self, McpError> {
        let shared = Arc::new(shared);
        let req = shared.http.get(shared.url.clone()).header(ACCEPT, "text/event-stream");
        let (req, _) = shared.authed(req).await;
        let resp = req.send().await?;
        let status = resp.status();
        if status.as_u16() == 401 {
            return Err(unauthorized(&resp));
        }
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|c| c.to_ascii_lowercase().starts_with("text/event-stream"));
        if !status.is_success() || !is_sse {
            let body = resp.text().await.unwrap_or_default();
            return Err(McpError::Http { status: status.as_u16(), body: truncate_body(&body, 300) });
        }

        let (ep_tx, ep_rx) = oneshot::channel::<String>();
        let mut ep_tx = Some(ep_tx);
        let s2 = shared.clone();
        let task = tokio::spawn(async move {
            pump_sse(resp, s2.inbound.clone(), s2.log.clone(), None, move |ev| {
                if ev.event.as_deref() == Some("endpoint") {
                    if let Some(tx) = ep_tx.take() {
                        let _ = tx.send(ev.data.clone());
                    }
                    return false;
                }
                true
            })
            .await;
            let _ = s2.inbound.send(Inbound::Closed("SSE stream closed by the server".into()));
        });
        let reader = AbortOnDrop(task);
        let endpoint = tokio::time::timeout(timeout, ep_rx)
            .await
            .map_err(|_| McpError::Transport("no `endpoint` event on the SSE stream".into()))?
            .map_err(|_| McpError::Transport("SSE stream closed before the `endpoint` event".into()))?;
        let endpoint = shared
            .url
            .join(endpoint.trim())
            .map_err(|e| McpError::Transport(format!("invalid endpoint `{endpoint}`: {e}")))?;
        if endpoint.origin() != shared.url.origin() {
            return Err(McpError::Transport(format!("endpoint `{endpoint}` is on a different origin")));
        }
        shared.log.info(format!("legacy SSE transport; posting to {endpoint}"));
        Ok(LegacySse { shared, endpoint, reader: Mutex::new(Some(reader)) })
    }
}

#[async_trait]
impl Transport for LegacySse {
    async fn send(&self, msg: Value) -> Result<(), McpError> {
        let mut retried_auth = false;
        loop {
            let req = self.shared.http.post(self.endpoint.clone()).json(&msg);
            let (req, bearer) = self.shared.authed(req).await;
            let resp = req.send().await?;
            let status = resp.status();
            if status.as_u16() == 401 {
                if !retried_auth {
                    if let (Some(ts), Some(used)) = (&self.shared.tokens, &bearer) {
                        if ts.refresh_after_unauthorized(used).await {
                            retried_auth = true;
                            continue;
                        }
                    }
                }
                return Err(unauthorized(&resp));
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(McpError::Http { status: status.as_u16(), body: truncate_body(&body, 500) });
            }
            // Responses arrive on the SSE stream.
            return Ok(());
        }
    }

    async fn close(&self) {
        self.reader.lock().unwrap().take();
    }
}
