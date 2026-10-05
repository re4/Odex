//! Odex browser bridge: agent browser use over the Chrome DevTools Protocol.
//!
//! All browser-use logic lives in [`BrowserSession`]. It speaks CDP through
//! the [`CdpTransport`] trait, which has two implementations:
//!
//! - [`WsTransport`]: a direct WebSocket connection to a Chromium-based
//!   browser started with `--remote-debugging-port` (headless `exec` mode and
//!   tests; see [`launch_headless_browser`]).
//! - A transport implemented by the engine that forwards each call to the
//!   desktop app (`webContents.debugger.sendCommand`) and relays the debugger's
//!   `message` events back as [`CdpEvent`]s.
//!
//! Transport contract (what [`BrowserSession`] relies on):
//! - `send` targets the currently selected page (flattened session or
//!   `webContents`), and returns the CDP `result` object or an error.
//! - The `Page`, `Runtime`, `Network`, `DOM` and `Accessibility` domains are
//!   enabled on the selected page (`Log` and `Page.setLifecycleEventsEnabled`
//!   are optional extras), and their events are buffered until
//!   `drain_events` is called.
//! - Tab operations act on top-level pages only.

mod events;
mod launch;
mod session;
mod site;
mod snapshot;
mod ws;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use launch::{find_browser, launch_headless_browser};
pub use session::{BrowserOptions, BrowserSession};
pub use site::{site_decision, SiteDecision};
pub use ws::WsTransport;

/// A CDP event (`method` + `params`) from the selected page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
}

/// A top-level browser tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    /// The tab `send` currently targets.
    pub active: bool,
}

/// How [`BrowserSession`] reaches a browser page.
#[async_trait]
pub trait CdpTransport: Send + Sync {
    /// Send a CDP command to the current page target and return its `result`.
    async fn send(&self, method: &str, params: Value) -> Result<Value>;
    /// Events buffered since the last call (oldest first).
    async fn drain_events(&self) -> Vec<CdpEvent>;
    async fn tabs(&self) -> Result<Vec<TabInfo>>;
    /// Open a tab and make it current.
    async fn new_tab(&self, url: &str) -> Result<TabInfo>;
    /// Make a tab current.
    async fn select_tab(&self, id: &str) -> Result<()>;
    /// Close a tab; if it was current, another tab becomes current.
    async fn close_tab(&self, id: &str) -> Result<()>;
}

#[async_trait]
impl<T: CdpTransport + ?Sized> CdpTransport for Arc<T> {
    async fn send(&self, method: &str, params: Value) -> Result<Value> {
        (**self).send(method, params).await
    }
    async fn drain_events(&self) -> Vec<CdpEvent> {
        (**self).drain_events().await
    }
    async fn tabs(&self) -> Result<Vec<TabInfo>> {
        (**self).tabs().await
    }
    async fn new_tab(&self, url: &str) -> Result<TabInfo> {
        (**self).new_tab(url).await
    }
    async fn select_tab(&self, id: &str) -> Result<()> {
        (**self).select_tab(id).await
    }
    async fn close_tab(&self, id: &str) -> Result<()> {
        (**self).close_tab(id).await
    }
}

#[async_trait]
impl<T: CdpTransport + ?Sized> CdpTransport for Box<T> {
    async fn send(&self, method: &str, params: Value) -> Result<Value> {
        (**self).send(method, params).await
    }
    async fn drain_events(&self) -> Vec<CdpEvent> {
        (**self).drain_events().await
    }
    async fn tabs(&self) -> Result<Vec<TabInfo>> {
        (**self).tabs().await
    }
    async fn new_tab(&self, url: &str) -> Result<TabInfo> {
        (**self).new_tab(url).await
    }
    async fn select_tab(&self, id: &str) -> Result<()> {
        (**self).select_tab(id).await
    }
    async fn close_tab(&self, id: &str) -> Result<()> {
        (**self).close_tab(id).await
    }
}
