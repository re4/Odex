//! `BrowserSession`: the agent-facing browser actions.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use odex_protocol::{BrowserExecuteResponse, Rect};
use serde_json::{json, Value};

use crate::events::{truncate, EventLog};
use crate::site::{display_host, site_decision, SiteDecision};
use crate::snapshot::{budget_join, render_ax_tree, render_dom_walk, RefEntry, RefTarget, DOM_WALK_JS};
use crate::CdpTransport;

/// Session configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserOptions {
    /// Enables `eval` and raw `cdp`.
    pub developer_mode: bool,
    /// Host patterns the agent may visit without asking (see [`site_decision`]).
    pub allowed_sites: Vec<String>,
    /// Host patterns the agent may never visit.
    pub blocked_sites: Vec<String>,
    /// Size budget of `snapshot` (and `eval`/`cdp`) text.
    pub max_snapshot_chars: usize,
    /// Screenshots wider (full page) or larger (viewport) than this are downscaled.
    pub screenshot_max_edge: u32,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        Self {
            developer_mode: false,
            allowed_sites: Vec::new(),
            blocked_sites: Vec::new(),
            max_snapshot_chars: 24_000,
            screenshot_max_edge: 1_600,
        }
    }
}

#[derive(Default)]
struct State {
    log: EventLog,
    refs: HashMap<String, RefEntry>,
    /// Sites approved by the user during this session.
    session_allowed: Vec<String>,
}

#[derive(Default)]
struct Outcome {
    text: Option<String>,
    image: Option<String>,
    bounds: Option<Rect>,
}

impl Outcome {
    fn text(s: impl Into<String>) -> Self {
        Self { text: Some(s.into()), ..Default::default() }
    }
}

#[derive(Clone, Copy)]
struct Marks {
    load: u64,
    started: u64,
    committed: u64,
}

const NAV_TIMEOUT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);

/// Agent browser use on top of a [`CdpTransport`]. Actions are serialized.
pub struct BrowserSession<T: CdpTransport> {
    transport: T,
    opts: BrowserOptions,
    state: Mutex<State>,
    op: tokio::sync::Mutex<()>,
}

impl<T: CdpTransport> BrowserSession<T> {
    pub fn new(transport: T, opts: BrowserOptions) -> Self {
        Self { transport, opts, state: Mutex::new(State::default()), op: tokio::sync::Mutex::new(()) }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn options(&self) -> &BrowserOptions {
        &self.opts
    }

    /// Site decision including sites the user approved for this session.
    pub fn site_decision_for(&self, url: &str) -> SiteDecision {
        let mut allowed = self.opts.allowed_sites.clone();
        allowed.extend(self.state().session_allowed.iter().cloned());
        site_decision(url, &allowed, &self.opts.blocked_sites)
    }

    /// Allow a host pattern for the rest of the session (after the user
    /// approved an `Ask` decision). Blocked sites stay blocked.
    pub fn allow_site(&self, pattern: &str) {
        let pattern = pattern.trim().to_string();
        if pattern.is_empty() {
            return;
        }
        let mut st = self.state();
        if !st.session_allowed.contains(&pattern) {
            st.session_allowed.push(pattern);
        }
    }

    /// Run one action. Never panics; failures come back as `ok: false` with `error`.
    ///
    /// Actions: `navigate {url}`, `back`, `forward`, `reload`, `snapshot {mode?: "dom"}`,
    /// `click {ref|selector|x,y}`, `type {ref|selector, text, submit?, clear?}`,
    /// `select {ref|selector, value|label}`, `scroll {ref? | dx, dy}`,
    /// `hover {ref|selector}`, `press {key}`, `screenshot {full_page?, ref?}`,
    /// `eval {expression}` and `cdp {method, params}` (developer mode only),
    /// `console {limit?, clear?}`, `network {limit?, filter?, clear?}`,
    /// `tabs`, `new_tab {url?}`, `select_tab {id}`, `close_tab {id?}`,
    /// `wait {ms | selector | text, timeout_ms?}`.
    pub async fn execute(&self, action: &str, args: &Value) -> BrowserExecuteResponse {
        let _serial = self.op.lock().await;
        let args = if args.is_object() { args.clone() } else { json!({}) };
        let result = self.run(action.trim(), &args).await;
        let (url, title) = self.page_info().await;
        match result {
            Ok(o) => BrowserExecuteResponse {
                ok: true,
                url,
                title,
                text: o.text,
                image: o.image,
                error: None,
                bounds: o.bounds,
            },
            Err(e) => BrowserExecuteResponse {
                ok: false,
                url,
                title,
                text: None,
                image: None,
                error: Some(format!("{e:#}")),
                bounds: None,
            },
        }
    }

    async fn run(&self, action: &str, args: &Value) -> Result<Outcome> {
        self.pump().await;
        match action {
            "navigate" | "goto" | "open" => self.navigate(args).await,
            "back" => self.history(-1).await,
            "forward" => self.history(1).await,
            "reload" => self.reload().await,
            "snapshot" => self.snapshot(args).await,
            "click" => self.click(args).await,
            "type" | "fill" => self.type_text(args).await,
            "select" => self.select(args).await,
            "scroll" => self.scroll(args).await,
            "hover" => self.hover(args).await,
            "press" | "key" => self.press(args).await,
            "screenshot" => self.screenshot(args).await,
            "eval" => self.eval(args).await,
            "cdp" => self.raw_cdp(args).await,
            "console" => {
                self.pump().await;
                let mut st = self.state();
                let text = st.log.console_text(usize_arg(args, "limit", 100));
                if args["clear"].as_bool() == Some(true) {
                    st.log.clear_console();
                }
                Ok(Outcome::text(text))
            }
            "network" => {
                self.pump().await;
                let mut st = self.state();
                let text = st.log.network_text(usize_arg(args, "limit", 100), args["filter"].as_str());
                if args["clear"].as_bool() == Some(true) {
                    st.log.clear_network();
                }
                Ok(Outcome::text(text))
            }
            "tabs" => self.tabs().await,
            "new_tab" => self.new_tab(args).await,
            "select_tab" => self.select_tab(args).await,
            "close_tab" => self.close_tab(args).await,
            "wait" => self.wait(args).await,
            "" => bail!("missing browser action"),
            other => bail!(
                "unknown browser action `{other}` (expected navigate, back, forward, reload, snapshot, click, type, \
                 select, scroll, hover, press, screenshot, eval, cdp, console, network, tabs, new_tab, select_tab, \
                 close_tab or wait)"
            ),
        }
    }

    // -- plumbing -----------------------------------------------------------

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Send a command while keeping events flowing, so a JavaScript dialog
    /// that blocks the page gets dismissed instead of hanging the call.
    async fn send(&self, method: &str, params: Value) -> Result<Value> {
        let fut = self.transport.send(method, params);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return r,
                _ = tokio::time::sleep(Duration::from_millis(150)) => self.pump().await,
            }
        }
    }

    /// Fold buffered events into the log; dismiss dialogs; drop stale refs.
    async fn pump(&self) {
        let events = self.transport.drain_events().await;
        let dialog = {
            let mut st = self.state();
            for ev in &events {
                st.log.apply(ev);
            }
            if st.log.doc_changed {
                st.log.doc_changed = false;
                st.refs.clear();
            }
            st.log.pending_dialog.take()
        };
        if let Some((kind, _)) = dialog {
            // Alerts are acknowledged; confirm/prompt are cancelled so the
            // agent never confirms something on the user's behalf.
            let accept = matches!(kind.as_str(), "alert" | "beforeunload");
            let _ = self.transport.send("Page.handleJavaScriptDialog", json!({ "accept": accept })).await;
        }
    }

    fn marks(&self) -> Marks {
        let st = self.state();
        Marks { load: st.log.load_count, started: st.log.nav_started, committed: st.log.nav_committed }
    }

    async fn ensure_main_frame(&self) {
        if self.state().log.main_frame.is_some() {
            return;
        }
        if let Ok(tree) = self.send("Page.getFrameTree", json!({})).await {
            if let Some(id) = tree["frameTree"]["frame"]["id"].as_str() {
                let mut st = self.state();
                if st.log.main_frame.is_none() {
                    st.log.main_frame = Some(id.to_string());
                }
            }
        }
    }

    /// Wait for a load event after `since`, then briefly for network quiet.
    async fn wait_for_load(&self, since: Marks, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            self.pump().await;
            if self.state().log.load_count > since.load {
                break;
            }
            if start.elapsed() > timeout {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
        self.wait_network_idle(Duration::from_millis(400), Duration::from_secs(3)).await;
        true
    }

    async fn wait_network_idle(&self, quiet: Duration, max: Duration) -> bool {
        let start = Instant::now();
        loop {
            self.pump().await;
            if self.state().log.network_idle_for() >= quiet {
                return true;
            }
            if start.elapsed() > max {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// After an input event: notice navigations it caused and wait for them.
    async fn settle(&self, before: Marks) -> Option<String> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(350) {
            tokio::time::sleep(POLL).await;
            self.pump().await;
            let now = self.marks();
            if now.started > before.started || now.load > before.load {
                let loaded = self.wait_for_load(before, NAV_TIMEOUT).await;
                return Some(if loaded { " and the page navigated".into() } else { " (page still loading)".into() });
            }
            if now.committed > before.committed {
                self.wait_network_idle(Duration::from_millis(200), Duration::from_secs(2)).await;
                return Some(" (URL changed)".into());
            }
        }
        self.wait_network_idle(Duration::from_millis(150), Duration::from_secs(1)).await;
        None
    }

    async fn eval_value(&self, expression: &str) -> Result<Value> {
        let r = self
            .send("Runtime.evaluate", json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }))
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("{}", exception_text(ex));
        }
        Ok(r["result"]["value"].clone())
    }

    async fn page_info(&self) -> (Option<String>, Option<String>) {
        let probe = self.transport.send(
            "Runtime.evaluate",
            json!({ "expression": "[location.href, document.title]", "returnByValue": true }),
        );
        match tokio::time::timeout(Duration::from_secs(3), probe).await {
            Ok(Ok(v)) => {
                let arr = &v["result"]["value"];
                (arr[0].as_str().map(str::to_string), arr[1].as_str().map(str::to_string))
            }
            _ => (None, None),
        }
    }

    fn check_site(&self, url: &str) -> Result<()> {
        match self.site_decision_for(url) {
            SiteDecision::Allowed => Ok(()),
            SiteDecision::Blocked => {
                bail!("navigation blocked: {} is on the blocked sites list or is not a web page", display_host(url))
            }
            SiteDecision::Ask => bail!(
                "navigation blocked: {} is not in the allowed sites list; ask the user to approve this site first",
                display_host(url)
            ),
        }
    }

    /// If the page ended up somewhere not allowed (redirect, link, script),
    /// leave it and report why.
    async fn enforce_site(&self) -> Result<()> {
        let (Some(url), _) = self.page_info().await else {
            return Ok(());
        };
        let Err(e) = self.check_site(&url) else {
            return Ok(());
        };
        let marks = self.marks();
        let mut left = false;
        if let Ok(h) = self.send("Page.getNavigationHistory", json!({})).await {
            let idx = h["currentIndex"].as_i64().unwrap_or(0);
            let entries = h["entries"].as_array().cloned().unwrap_or_default();
            if let Some(prev) = usize::try_from(idx - 1).ok().and_then(|i| entries.get(i)) {
                let prev_ok = prev["url"].as_str().is_some_and(|u| self.check_site(u).is_ok());
                if prev_ok && self.send("Page.navigateToHistoryEntry", json!({ "entryId": prev["id"] })).await.is_ok() {
                    left = true;
                }
            }
        }
        if !left {
            let _ = self.send("Page.navigate", json!({ "url": "about:blank" })).await;
        }
        self.wait_for_load(marks, Duration::from_secs(5)).await;
        self.state().refs.clear();
        Err(e.context(format!("left {url}")))
    }

    // -- element resolution -------------------------------------------------

    /// `(backendNodeId, label)` for `ref`/`selector` args, `None` if neither was given.
    async fn resolve_node(&self, args: &Value) -> Result<Option<(i64, String)>> {
        if let Some(raw) = args["ref"].as_str().or_else(|| args["element"].as_str()) {
            let r = normalize_ref(raw);
            let entry = self.state().refs.get(&r).cloned().ok_or_else(|| {
                anyhow!("unknown ref `{raw}`; take a new snapshot (refs reset on navigation and on every snapshot)")
            })?;
            let label = format!("{} [{r}]", entry.label);
            return match entry.target {
                RefTarget::Backend(id) => Ok(Some((id, label))),
                RefTarget::Selector(sel) => Ok(Some((self.query_selector(&sel).await?, label))),
            };
        }
        if let Some(sel) = args["selector"].as_str() {
            return Ok(Some((self.query_selector(sel).await?, format!("`{sel}`"))));
        }
        Ok(None)
    }

    async fn query_selector(&self, selector: &str) -> Result<i64> {
        let expr = format!("document.querySelector({})", serde_json::to_string(selector)?);
        let r = self.send("Runtime.evaluate", json!({ "expression": expr })).await?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("invalid selector `{selector}`: {}", exception_text(ex));
        }
        let Some(object_id) = r["result"]["objectId"].as_str() else {
            bail!("no element matches `{selector}`");
        };
        let node = self.send("DOM.describeNode", json!({ "objectId": object_id })).await;
        let _ = self.transport.send("Runtime.releaseObject", json!({ "objectId": object_id })).await;
        node?["node"]["backendNodeId"].as_i64().ok_or_else(|| anyhow!("could not resolve `{selector}`"))
    }

    async fn object_for(&self, backend: i64) -> Result<String> {
        let r = self.send("DOM.resolveNode", json!({ "backendNodeId": backend })).await.map_err(stale)?;
        r["object"]["objectId"].as_str().map(str::to_string).ok_or_else(|| anyhow!("element is gone"))
    }

    async fn call_on(&self, object_id: &str, function: &str, args: Vec<Value>) -> Result<Value> {
        let arguments: Vec<Value> = args.into_iter().map(|v| json!({ "value": v })).collect();
        let r = self
            .send(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object_id,
                    "functionDeclaration": function,
                    "arguments": arguments,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("{}", exception_text(ex));
        }
        Ok(r["result"]["value"].clone())
    }

    async fn viewport(&self) -> (f64, f64, f64, f64) {
        match self.send("Page.getLayoutMetrics", json!({})).await {
            Ok(m) => {
                let vp =
                    if m["cssLayoutViewport"].is_object() { &m["cssLayoutViewport"] } else { &m["layoutViewport"] };
                let vis =
                    if m["cssVisualViewport"].is_object() { &m["cssVisualViewport"] } else { &m["visualViewport"] };
                (
                    vp["clientWidth"].as_f64().unwrap_or(1280.0),
                    vp["clientHeight"].as_f64().unwrap_or(800.0),
                    vis["pageX"].as_f64().unwrap_or(0.0),
                    vis["pageY"].as_f64().unwrap_or(0.0),
                )
            }
            Err(_) => (1280.0, 800.0, 0.0, 0.0),
        }
    }

    /// Scroll the element into view and return its visible box (viewport CSS px).
    async fn element_rect(&self, backend: i64) -> Result<Rect> {
        self.send("DOM.scrollIntoViewIfNeeded", json!({ "backendNodeId": backend })).await.map_err(stale)?;
        let mut quad: Option<Vec<f64>> = None;
        if let Ok(q) = self.send("DOM.getContentQuads", json!({ "backendNodeId": backend })).await {
            quad = q["quads"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|q| q.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<_>>()))
                .find(|q| q.len() == 8 && quad_area(q) > 0.5);
        }
        if quad.is_none() {
            let m = self.send("DOM.getBoxModel", json!({ "backendNodeId": backend })).await.map_err(stale)?;
            quad = m["model"]["border"].as_array().map(|a| a.iter().filter_map(Value::as_f64).collect());
        }
        let q = quad.filter(|q| q.len() == 8).ok_or_else(|| anyhow!("element has no visible box"))?;
        let xs = [q[0], q[2], q[4], q[6]];
        let ys = [q[1], q[3], q[5], q[7]];
        let (x0, x1) = (xs.iter().copied().fold(f64::MAX, f64::min), xs.iter().copied().fold(f64::MIN, f64::max));
        let (y0, y1) = (ys.iter().copied().fold(f64::MAX, f64::min), ys.iter().copied().fold(f64::MIN, f64::max));
        Ok(Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 })
    }

    /// Center of the part of `rect` inside the viewport.
    async fn click_point(&self, rect: &Rect) -> Result<(f64, f64)> {
        let (vw, vh, _, _) = self.viewport().await;
        let x0 = rect.x.max(0.0);
        let y0 = rect.y.max(0.0);
        let x1 = (rect.x + rect.width).min(vw);
        let y1 = (rect.y + rect.height).min(vh);
        if x1 <= x0 || y1 <= y0 {
            bail!("element is outside the visible viewport");
        }
        Ok(((x0 + x1) / 2.0, (y0 + y1) / 2.0))
    }

    async fn mouse(&self, kind: &str, x: f64, y: f64, button: &str, buttons: u32, clicks: u32) -> Result<()> {
        self.send(
            "Input.dispatchMouseEvent",
            json!({ "type": kind, "x": x, "y": y, "button": button, "buttons": buttons, "clickCount": clicks }),
        )
        .await
        .map(|_| ())
    }

    async fn key(&self, key: &str) -> Result<()> {
        let (code, vk, text) = key_info(key).ok_or_else(|| anyhow!("unsupported key `{key}`"))?;
        let mut down = json!({ "type": "keyDown", "key": key_name(key), "code": code, "windowsVirtualKeyCode": vk });
        if let Some(t) = text {
            down["text"] = json!(t);
            down["unmodifiedText"] = json!(t);
        } else {
            down["type"] = json!("rawKeyDown");
        }
        self.send("Input.dispatchKeyEvent", down).await?;
        self.send(
            "Input.dispatchKeyEvent",
            json!({ "type": "keyUp", "key": key_name(key), "code": code, "windowsVirtualKeyCode": vk }),
        )
        .await?;
        Ok(())
    }

    // -- actions ------------------------------------------------------------

    async fn navigate(&self, args: &Value) -> Result<Outcome> {
        let raw =
            args["url"].as_str().filter(|u| !u.trim().is_empty()).ok_or_else(|| anyhow!("navigate needs `url`"))?;
        let url = normalize_url(raw);
        self.check_site(&url)?;
        let timeout = Duration::from_millis(u64_arg(args, "timeout_ms", NAV_TIMEOUT.as_millis() as u64).min(120_000));
        self.pump().await;
        let before = self.marks();
        let r = self.send("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            bail!("could not load {url}: {err}");
        }
        let loaded = if r.get("loaderId").and_then(Value::as_str).is_some() {
            self.wait_for_load(before, timeout).await
        } else {
            self.wait_network_idle(Duration::from_millis(200), Duration::from_secs(2)).await;
            true
        };
        self.enforce_site().await?;
        self.state().refs.clear();
        let (u, t) = self.page_info().await;
        let mut text = format!("Navigated to {}", u.unwrap_or(url));
        if let Some(t) = t.filter(|t| !t.is_empty()) {
            text.push_str(&format!(" — \"{t}\""));
        }
        if !loaded {
            text.push_str(" (still loading after the timeout; use `wait` or `snapshot`)");
        }
        Ok(Outcome::text(text))
    }

    async fn history(&self, delta: i64) -> Result<Outcome> {
        let dir = if delta < 0 { "back" } else { "forward" };
        let h = self.send("Page.getNavigationHistory", json!({})).await?;
        let idx = h["currentIndex"].as_i64().unwrap_or(0) + delta;
        let entries = h["entries"].as_array().cloned().unwrap_or_default();
        let entry =
            usize::try_from(idx).ok().and_then(|i| entries.get(i)).ok_or_else(|| anyhow!("no page to go {dir} to"))?;
        if let Some(url) = entry["url"].as_str() {
            self.check_site(url)?;
        }
        let before = self.marks();
        self.send("Page.navigateToHistoryEntry", json!({ "entryId": entry["id"] })).await?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            self.pump().await;
            let m = self.marks();
            if m.load > before.load || m.committed > before.committed {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        self.wait_network_idle(Duration::from_millis(300), Duration::from_secs(3)).await;
        self.enforce_site().await?;
        self.state().refs.clear();
        let (u, _) = self.page_info().await;
        Ok(Outcome::text(format!("Went {dir} to {}", u.unwrap_or_default())))
    }

    async fn reload(&self) -> Result<Outcome> {
        let before = self.marks();
        self.send("Page.reload", json!({})).await?;
        let loaded = self.wait_for_load(before, NAV_TIMEOUT).await;
        self.state().refs.clear();
        Ok(Outcome::text(if loaded { "Reloaded" } else { "Reloaded (still loading)" }))
    }

    /// `mode: "dom"` skips the accessibility tree and walks the DOM instead.
    async fn snapshot(&self, args: &Value) -> Result<Outcome> {
        let mut outline = None;
        if args["mode"].as_str() != Some("dom") {
            let ax = self.send("Accessibility.getFullAXTree", json!({})).await;
            outline = ax
                .ok()
                .and_then(|v| v["nodes"].as_array().map(|n| render_ax_tree(n)))
                .filter(|o| !o.refs.is_empty() || o.lines.len() > 1);
        }
        if outline.is_none() {
            let json = self.eval_value(DOM_WALK_JS).await.context("building a DOM snapshot")?;
            let items: Vec<Value> = json.as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
            outline = Some(render_dom_walk(&items));
        }
        let mut outline = outline.unwrap_or_default();
        let (url, title) = self.page_info().await;
        let url = url.unwrap_or_default();

        // Resolve hrefs the accessibility tree did not report (bounded).
        let base = url::Url::parse(&url).ok();
        for (line, backend) in std::mem::take(&mut outline.missing_href).into_iter().take(150) {
            let Ok(d) = self.send("DOM.describeNode", json!({ "backendNodeId": backend })).await else {
                continue;
            };
            let attrs = d["node"]["attributes"].as_array().cloned().unwrap_or_default();
            let href =
                attrs.chunks(2).find(|kv| kv[0] == "href").and_then(|kv| kv.get(1)?.as_str().map(str::to_string));
            if let Some(href) = href {
                let abs = base.as_ref().and_then(|b| b.join(&href).ok()).map(|u| u.to_string()).unwrap_or(href);
                if let Some(l) = outline.lines.get_mut(line) {
                    l.1.push_str(&format!(" href={}", truncate(&abs, 200)));
                }
            }
        }

        let header = format!("Page: {}\nURL: {url}\n", title.unwrap_or_default());
        let budget = self.opts.max_snapshot_chars.saturating_sub(header.len() + 120).max(500);
        let (body, omitted) = budget_join(&outline.lines, budget);
        let mut text = header + &body;
        if body.is_empty() {
            text.push_str("(no visible text or interactive elements)\n");
        }
        if omitted > 0 {
            text.push_str(&format!(
                "… {omitted} more lines not shown (snapshot budget reached); scroll, or act with a CSS selector\n"
            ));
        }
        self.state().refs = outline.refs;
        Ok(Outcome::text(text.trim_end().to_string()))
    }

    async fn click(&self, args: &Value) -> Result<Outcome> {
        let button = match args["button"].as_str().unwrap_or("left") {
            b @ ("left" | "right" | "middle") => b,
            other => bail!("unknown mouse button `{other}`"),
        };
        let mask = match button {
            "left" => 1,
            "right" => 2,
            _ => 4,
        };
        let clicks = if args["double"].as_bool() == Some(true) { 2 } else { 1 };
        let (x, y, bounds, label) = match self.resolve_node(args).await? {
            Some((backend, label)) => {
                let rect = self.element_rect(backend).await?;
                let (x, y) = self.click_point(&rect).await?;
                (x, y, Some(rect), label)
            }
            None => match (args["x"].as_f64(), args["y"].as_f64()) {
                (Some(x), Some(y)) => (x, y, None, format!("at ({x}, {y})")),
                _ => bail!("click needs `ref`, `selector`, or `x` and `y`"),
            },
        };
        self.ensure_main_frame().await;
        let before = self.marks();
        self.mouse("mouseMoved", x, y, "none", 0, 0).await?;
        for n in 1..=clicks {
            self.mouse("mousePressed", x, y, button, mask, n).await?;
            self.mouse("mouseReleased", x, y, button, 0, n).await?;
        }
        let nav = self.settle(before).await;
        self.enforce_site().await?;
        Ok(Outcome { text: Some(format!("Clicked {label}{}", nav.unwrap_or_default())), bounds, ..Default::default() })
    }

    async fn hover(&self, args: &Value) -> Result<Outcome> {
        let (backend, label) =
            self.resolve_node(args).await?.ok_or_else(|| anyhow!("hover needs `ref` or `selector`"))?;
        let rect = self.element_rect(backend).await?;
        let (x, y) = self.click_point(&rect).await?;
        self.mouse("mouseMoved", x, y, "none", 0, 0).await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(Outcome { text: Some(format!("Hovering over {label}")), bounds: Some(rect), ..Default::default() })
    }

    async fn type_text(&self, args: &Value) -> Result<Outcome> {
        let text = args["text"].as_str().ok_or_else(|| anyhow!("type needs `text`"))?;
        let clear = args["clear"].as_bool().unwrap_or(true);
        let submit = args["submit"].as_bool().unwrap_or(false);
        let target = self.resolve_node(args).await?;
        let (object_id, label, bounds) = match &target {
            Some((backend, label)) => {
                let rect = self.element_rect(*backend).await.ok();
                (self.object_for(*backend).await?, label.clone(), rect)
            }
            None => {
                let r = self.send("Runtime.evaluate", json!({ "expression": "document.activeElement" })).await?;
                let id = r["result"]["objectId"]
                    .as_str()
                    .filter(|_| !matches!(r["result"]["description"].as_str(), Some("body") | None))
                    .ok_or_else(|| anyhow!("no field is focused; pass `ref` or `selector`"))?;
                (id.to_string(), "the focused field".to_string(), None)
            }
        };
        let result = async {
            let info = self.call_on(&object_id, FIELD_INFO_FN, vec![]).await?;
            if let Some(reason) = sensitive_field_reason(&info) {
                bail!(
                    "refusing to type into {label}: it looks like a {reason}. Passwords and payment details must be \
                     entered by the user."
                );
            }
            if info["editable"].as_bool() != Some(true) {
                bail!("{label} is not an editable text field ({})", info["tag"].as_str().unwrap_or("element"));
            }
            if info["disabled"].as_bool() == Some(true) || info["readOnly"].as_bool() == Some(true) {
                bail!("{label} is disabled or read-only");
            }
            self.call_on(&object_id, PREPARE_FN, vec![json!(clear)]).await?;
            if !text.is_empty() {
                self.send("Input.insertText", json!({ "text": text })).await?;
            } else if clear {
                self.key("Backspace").await?;
            }
            Ok(())
        }
        .await;
        let _ = self.transport.send("Runtime.releaseObject", json!({ "objectId": object_id })).await;
        result?;
        let mut msg = format!("Typed {} characters into {label}", text.chars().count());
        if submit {
            self.ensure_main_frame().await;
            let before = self.marks();
            self.key("Enter").await?;
            msg.push_str(" and pressed Enter");
            if let Some(nav) = self.settle(before).await {
                msg.push_str(&nav);
            }
            self.enforce_site().await?;
        }
        Ok(Outcome { text: Some(msg), bounds, ..Default::default() })
    }

    async fn select(&self, args: &Value) -> Result<Outcome> {
        let (backend, label) =
            self.resolve_node(args).await?.ok_or_else(|| anyhow!("select needs `ref` or `selector`"))?;
        let value = args["value"].as_str().map(str::to_string);
        let wanted = args["label"].as_str().or_else(|| args["option"].as_str()).map(str::to_string);
        if value.is_none() && wanted.is_none() {
            bail!("select needs `value` or `label`");
        }
        let object_id = self.object_for(backend).await?;
        let r = self.call_on(&object_id, SELECT_FN, vec![json!(value), json!(wanted)]).await;
        let _ = self.transport.send("Runtime.releaseObject", json!({ "objectId": object_id })).await;
        let r = r?;
        if let Some(err) = r["error"].as_str() {
            let options =
                r["options"].as_array().map(|o| o.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "));
            match options {
                Some(o) if !o.is_empty() => bail!("{label}: {err}; available options: {o}"),
                _ => bail!("{label}: {err}"),
            }
        }
        self.settle(self.marks()).await;
        Ok(Outcome::text(format!(
            "Selected \"{}\" (value \"{}\") in {label}",
            r["selected"].as_str().unwrap_or_default(),
            r["value"].as_str().unwrap_or_default()
        )))
    }

    async fn scroll(&self, args: &Value) -> Result<Outcome> {
        if let Some((backend, label)) = self.resolve_node(args).await? {
            let rect = self.element_rect(backend).await?;
            return Ok(Outcome {
                text: Some(format!("Scrolled {label} into view")),
                bounds: Some(rect),
                ..Default::default()
            });
        }
        let mut dx = args["dx"].as_f64().unwrap_or(0.0);
        let mut dy = args["dy"].as_f64().unwrap_or(0.0);
        if dx == 0.0 && dy == 0.0 {
            let amount = args["amount"].as_f64().unwrap_or(600.0);
            match args["direction"].as_str().unwrap_or("down") {
                "up" => dy = -amount,
                "left" => dx = -amount,
                "right" => dx = amount,
                _ => dy = amount,
            }
        }
        let (vw, vh, _, _) = self.viewport().await;
        let x = args["x"].as_f64().unwrap_or(vw / 2.0);
        let y = args["y"].as_f64().unwrap_or(vh / 2.0);
        self.send(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy }),
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let pos = self
            .eval_value(
                "[Math.round(scrollX), Math.round(scrollY), document.documentElement.scrollHeight, innerHeight]",
            )
            .await
            .unwrap_or(Value::Null);
        Ok(Outcome::text(format!(
            "Scrolled by ({dx}, {dy}); page offset now ({}, {}) of height {} (viewport {})",
            pos[0], pos[1], pos[2], pos[3]
        )))
    }

    async fn press(&self, args: &Value) -> Result<Outcome> {
        let key = args["key"].as_str().ok_or_else(|| anyhow!("press needs `key` (e.g. Enter, Escape, Tab)"))?;
        self.ensure_main_frame().await;
        let before = self.marks();
        self.key(key).await?;
        let nav = self.settle(before).await;
        self.enforce_site().await?;
        Ok(Outcome::text(format!("Pressed {}{}", key_name(key), nav.unwrap_or_default())))
    }

    async fn screenshot(&self, args: &Value) -> Result<Outcome> {
        let full = args["full_page"].as_bool().or_else(|| args["fullPage"].as_bool()).unwrap_or(false);
        let mut params = json!({ "format": "png" });
        let mut bounds = None;
        if let Some((backend, _)) = self.resolve_node(args).await? {
            let rect = self.element_rect(backend).await?;
            let (_, _, px, py) = self.viewport().await;
            params["clip"] = json!({ "x": rect.x + px, "y": rect.y + py, "width": rect.width.max(1.0), "height": rect.height.max(1.0), "scale": 1 });
            bounds = Some(rect);
        } else if full {
            let m = self.send("Page.getLayoutMetrics", json!({})).await?;
            let size = if m["cssContentSize"].is_object() { &m["cssContentSize"] } else { &m["contentSize"] };
            let w = size["width"].as_f64().unwrap_or(1280.0).clamp(1.0, 16_384.0);
            let h = size["height"].as_f64().unwrap_or(800.0).clamp(1.0, 16_384.0);
            params["clip"] = json!({ "x": 0, "y": 0, "width": w, "height": h, "scale": 1 });
            params["captureBeyondViewport"] = json!(true);
        }
        let r = self.send("Page.captureScreenshot", params).await?;
        let data = r["data"].as_str().ok_or_else(|| anyhow!("the browser returned no image"))?;
        let png = base64::engine::general_purpose::STANDARD.decode(data).context("decoding screenshot")?;
        let Scaled { png, size: (w, h), original: (ow, oh) } = downscale_png(png, self.opts.screenshot_max_edge, full)?;
        let mut text = format!("Screenshot {w}x{h}");
        if (w, h) != (ow, oh) {
            text.push_str(&format!(" (downscaled from {ow}x{oh})"));
        }
        let image = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png));
        Ok(Outcome { text: Some(text), image: Some(image), bounds })
    }

    async fn eval(&self, args: &Value) -> Result<Outcome> {
        if !self.opts.developer_mode {
            bail!("`eval` is only available in browser developer mode (enable it in settings)");
        }
        let expression = args["expression"]
            .as_str()
            .or_else(|| args["script"].as_str())
            .ok_or_else(|| anyhow!("eval needs `expression`"))?;
        let r = self
            .send(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true, "userGesture": true }),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            bail!("{}", exception_text(ex));
        }
        let res = &r["result"];
        let text = match res.get("value") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => serde_json::to_string_pretty(v)?,
            None => res["description"].as_str().or_else(|| res["type"].as_str()).unwrap_or("undefined").to_string(),
        };
        self.settle(self.marks()).await;
        self.enforce_site().await?;
        Ok(Outcome::text(truncate(&text, self.opts.max_snapshot_chars)))
    }

    async fn raw_cdp(&self, args: &Value) -> Result<Outcome> {
        if !self.opts.developer_mode {
            bail!("raw `cdp` is only available in browser developer mode (enable it in settings)");
        }
        let method = args["method"].as_str().ok_or_else(|| anyhow!("cdp needs `method`"))?;
        let params = if args["params"].is_object() { args["params"].clone() } else { json!({}) };
        if matches!(method, "Page.navigate" | "Target.createTarget") {
            self.check_site(params["url"].as_str().unwrap_or_default())?;
        }
        let r = self.send(method, params).await?;
        Ok(Outcome::text(truncate(&serde_json::to_string_pretty(&r)?, self.opts.max_snapshot_chars)))
    }

    async fn tabs(&self) -> Result<Outcome> {
        let tabs = self.transport.tabs().await?;
        if tabs.is_empty() {
            return Ok(Outcome::text("(no tabs)"));
        }
        let lines: Vec<String> = tabs
            .iter()
            .map(|t| {
                let title = if t.title.is_empty() { "(untitled)" } else { t.title.as_str() };
                format!("{} {} — {} — {}", if t.active { "*" } else { " " }, t.id, truncate(title, 80), t.url)
            })
            .collect();
        Ok(Outcome::text(lines.join("\n")))
    }

    fn reset_page_state(&self) {
        let mut st = self.state();
        st.log.reset_page();
        st.refs.clear();
    }

    async fn new_tab(&self, args: &Value) -> Result<Outcome> {
        let url = args["url"].as_str().map(str::trim).filter(|u| !u.is_empty() && *u != "about:blank");
        let url = url.map(normalize_url);
        if let Some(u) = &url {
            self.check_site(u)?;
        }
        let tab = self.transport.new_tab("about:blank").await?;
        self.reset_page_state();
        self.pump().await;
        let mut text = format!("Opened tab {}", tab.id);
        if let Some(u) = url {
            let nav = self.navigate(&json!({ "url": u })).await?;
            text.push_str(&format!(". {}", nav.text.unwrap_or_default()));
        }
        Ok(Outcome::text(text))
    }

    async fn select_tab(&self, args: &Value) -> Result<Outcome> {
        let id = args["id"].as_str().ok_or_else(|| anyhow!("select_tab needs `id` (see `tabs`)"))?;
        self.transport.select_tab(id).await?;
        self.reset_page_state();
        self.pump().await;
        Ok(Outcome::text(format!("Switched to tab {id}")))
    }

    async fn close_tab(&self, args: &Value) -> Result<Outcome> {
        let id = match args["id"].as_str() {
            Some(id) => id.to_string(),
            None => {
                let tabs = self.transport.tabs().await?;
                tabs.into_iter().find(|t| t.active).map(|t| t.id).ok_or_else(|| anyhow!("close_tab needs `id`"))?
            }
        };
        self.transport.close_tab(&id).await?;
        self.reset_page_state();
        self.pump().await;
        Ok(Outcome::text(format!("Closed tab {id}")))
    }

    async fn wait(&self, args: &Value) -> Result<Outcome> {
        let timeout = Duration::from_millis(u64_arg(args, "timeout_ms", 10_000).min(120_000));
        if let Some(ms) = args["ms"].as_u64().or_else(|| args["ms"].as_f64().map(|f| f.max(0.0) as u64)) {
            let end = Instant::now() + Duration::from_millis(ms.min(60_000));
            while Instant::now() < end {
                tokio::time::sleep(POLL.min(end.saturating_duration_since(Instant::now()))).await;
                self.pump().await;
            }
            return Ok(Outcome::text(format!("Waited {}ms", ms.min(60_000))));
        }
        let (what, expr) = if let Some(sel) = args["selector"].as_str() {
            (
                format!("selector `{sel}`"),
                format!(
                    "(e => !!e && !!(e.offsetWidth || e.offsetHeight || e.getClientRects().length))(document.querySelector({}))",
                    serde_json::to_string(sel)?
                ),
            )
        } else if let Some(text) = args["text"].as_str() {
            (
                format!("text \"{text}\""),
                format!("!!document.body && document.body.innerText.includes({})", serde_json::to_string(text)?),
            )
        } else {
            let idle = self.wait_network_idle(Duration::from_millis(500), timeout).await;
            return Ok(Outcome::text(if idle { "Network is idle" } else { "Timed out waiting for network idle" }));
        };
        let start = Instant::now();
        loop {
            match self.eval_value(&expr).await {
                Ok(Value::Bool(true)) => {
                    return Ok(Outcome::text(format!("Found {what} after {}ms", start.elapsed().as_millis())))
                }
                Err(e) if args["selector"].is_string() && e.to_string().contains("SyntaxError") => return Err(e),
                _ => {}
            }
            if start.elapsed() > timeout {
                bail!("timed out after {}ms waiting for {what}", timeout.as_millis());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

// -- helpers ------------------------------------------------------------------

const FIELD_INFO_FN: &str = r#"function() {
  const el = this;
  const attr = (n) => (el.getAttribute && el.getAttribute(n)) || '';
  const tag = (el.tagName || '').toLowerCase();
  let label = '';
  try { if (el.labels) label = Array.from(el.labels).map(l => l.innerText).join(' '); } catch (e) {}
  const type = tag === 'input' ? String(el.type || attr('type') || 'text').toLowerCase() : '';
  const nonText = ['checkbox','radio','button','submit','reset','image','file','range','color','hidden'];
  const editable = !!el.isContentEditable || tag === 'textarea' || (tag === 'input' && !nonText.includes(type));
  return { tag, type, autocomplete: attr('autocomplete').toLowerCase(), name: attr('name'), id: el.id || '',
    placeholder: attr('placeholder'), ariaLabel: attr('aria-label'), label, editable,
    disabled: !!el.disabled, readOnly: !!el.readOnly, inputmode: attr('inputmode') };
}"#;

const PREPARE_FN: &str = r#"function(clear) {
  this.focus();
  if (!clear) {
    if (typeof this.setSelectionRange === 'function' && typeof this.value === 'string') {
      try { this.setSelectionRange(this.value.length, this.value.length); } catch (e) {}
    }
    return true;
  }
  if (typeof this.select === 'function' && 'value' in this) { this.select(); return true; }
  if (this.isContentEditable) {
    const r = document.createRange(); r.selectNodeContents(this);
    const s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
  }
  return true;
}"#;

const SELECT_FN: &str = r#"function(value, label) {
  if (!this.tagName || this.tagName.toUpperCase() !== 'SELECT')
    return { error: 'not a native <select>; for custom dropdowns click to open them, then click the option' };
  const opts = Array.from(this.options);
  const norm = s => String(s || '').replace(/\s+/g, ' ').trim().toLowerCase();
  let opt = null;
  if (value != null) opt = opts.find(o => o.value === value) || opts.find(o => norm(o.text) === norm(value));
  if (!opt && label != null)
    opt = opts.find(o => norm(o.text) === norm(label)) || opts.find(o => norm(o.label) === norm(label))
      || opts.find(o => norm(o.text).includes(norm(label)));
  if (!opt) return { error: 'no matching option', options: opts.slice(0, 50).map(o => o.text.trim() + ' (' + o.value + ')') };
  if (opt.disabled) return { error: 'that option is disabled' };
  this.focus();
  this.value = opt.value;
  opt.selected = true;
  this.dispatchEvent(new Event('input', { bubbles: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return { selected: opt.text.trim(), value: opt.value };
}"#;

/// Why typing into this field is refused, if it is a password/payment field.
fn sensitive_field_reason(info: &Value) -> Option<&'static str> {
    let s = |k: &str| info[k].as_str().unwrap_or_default().to_ascii_lowercase();
    if s("type") == "password" {
        return Some("password field");
    }
    let autocomplete = s("autocomplete");
    for token in autocomplete.split_whitespace() {
        if matches!(token, "current-password" | "new-password" | "one-time-code") {
            return Some("password field");
        }
        if token.starts_with("cc-") {
            return Some("payment card field");
        }
    }
    let hints: String = ["name", "id", "placeholder", "ariaLabel", "label"]
        .iter()
        .map(|k| s(k))
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = hints.split_whitespace().collect();
    if words.iter().any(|w| w.contains("password") || matches!(*w, "passwd" | "passcode")) {
        return Some("password field");
    }
    const PAYMENT_WORDS: &[&str] = &["cvv", "cvv2", "cvc", "cvc2", "csc", "iban", "ccnum", "cardnum"];
    const PAYMENT_PHRASES: &[&[&str]] = &[
        &["card", "number"],
        &["card", "no"],
        &["cc", "number"],
        &["cc", "num"],
        &["credit", "card"],
        &["debit", "card"],
        &["security", "code"],
        &["card", "verification"],
        &["expiry", "date"],
        &["expiration", "date"],
        &["exp", "date"],
        &["routing", "number"],
        &["sort", "code"],
        &["account", "number"],
        &["bank", "account"],
    ];
    // Phrases match as consecutive words ("card number") or squashed into
    // one camelCase/joined word ("cardNumber" -> "cardnumber").
    let phrase_hit = PAYMENT_PHRASES.iter().any(|p| {
        let squashed = p.concat();
        words.windows(p.len()).any(|w| w == *p) || words.iter().any(|w| *w == squashed)
    });
    if phrase_hit || words.iter().any(|w| PAYMENT_WORDS.contains(w)) {
        return Some("payment field");
    }
    None
}

fn exception_text(ex: &Value) -> String {
    let desc =
        ex["exception"]["description"].as_str().or_else(|| ex["text"].as_str()).unwrap_or("JavaScript exception");
    desc.lines().next().unwrap_or(desc).to_string()
}

fn stale(e: anyhow::Error) -> anyhow::Error {
    let msg = e.to_string();
    if msg.contains("No node") || msg.contains("Could not find node") || msg.contains("Node is detached") {
        anyhow!("the element no longer exists; take a new snapshot")
    } else if msg.contains("Could not compute") || msg.contains("not visible") {
        anyhow!("the element is not rendered (hidden or zero-sized)")
    } else {
        e
    }
}

fn quad_area(q: &[f64]) -> f64 {
    let mut area = 0.0;
    for i in 0..4 {
        let (x1, y1) = (q[i * 2], q[i * 2 + 1]);
        let (x2, y2) = (q[(i * 2 + 2) % 8], q[(i * 2 + 3) % 8]);
        area += x1 * y2 - x2 * y1;
    }
    (area / 2.0).abs()
}

fn normalize_ref(raw: &str) -> String {
    let r = raw.trim().trim_start_matches('[').trim_end_matches(']').trim_start_matches('@');
    r.strip_prefix("ref=").unwrap_or(r).trim().to_string()
}

/// Add a scheme to bare hosts: `localhost:3000` -> `http://...`, `example.com` -> `https://...`.
fn normalize_url(raw: &str) -> String {
    let u = raw.trim();
    let lower = u.to_ascii_lowercase();
    if lower.contains("://")
        || ["about:", "data:", "file:", "javascript:", "blob:", "chrome:", "edge:"].iter().any(|p| lower.starts_with(p))
    {
        return u.to_string();
    }
    let local = ["localhost", "127.", "[::1]", "0.0.0.0"].iter().any(|p| lower.starts_with(p));
    if local {
        format!("http://{u}")
    } else {
        format!("https://{u}")
    }
}

fn usize_arg(args: &Value, key: &str, default: usize) -> usize {
    args[key].as_u64().map(|v| v as usize).unwrap_or(default)
}

fn u64_arg(args: &Value, key: &str, default: u64) -> u64 {
    args[key].as_u64().unwrap_or(default)
}

fn key_name(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => "Enter".into(),
        "esc" | "escape" => "Escape".into(),
        "space" => " ".into(),
        "tab" => "Tab".into(),
        "backspace" => "Backspace".into(),
        "delete" | "del" => "Delete".into(),
        "up" | "arrowup" => "ArrowUp".into(),
        "down" | "arrowdown" => "ArrowDown".into(),
        "left" | "arrowleft" => "ArrowLeft".into(),
        "right" | "arrowright" => "ArrowRight".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        _ => key.to_string(),
    }
}

/// (code, windowsVirtualKeyCode, text) for a key.
fn key_info(key: &str) -> Option<(String, i64, Option<String>)> {
    let named = |code: &str, vk: i64, text: Option<&str>| Some((code.to_string(), vk, text.map(str::to_string)));
    match key_name(key).as_str() {
        "Enter" => named("Enter", 13, Some("\r")),
        "Escape" => named("Escape", 27, None),
        " " => named("Space", 32, Some(" ")),
        "Tab" => named("Tab", 9, None),
        "Backspace" => named("Backspace", 8, None),
        "Delete" => named("Delete", 46, None),
        "ArrowUp" => named("ArrowUp", 38, None),
        "ArrowDown" => named("ArrowDown", 40, None),
        "ArrowLeft" => named("ArrowLeft", 37, None),
        "ArrowRight" => named("ArrowRight", 39, None),
        "PageUp" => named("PageUp", 33, None),
        "PageDown" => named("PageDown", 34, None),
        "Home" => named("Home", 36, None),
        "End" => named("End", 35, None),
        other => {
            let mut chars = other.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            let upper = c.to_ascii_uppercase();
            let code = if c.is_ascii_alphabetic() {
                format!("Key{upper}")
            } else if c.is_ascii_digit() {
                format!("Digit{c}")
            } else {
                String::new()
            };
            let vk = if c.is_ascii_alphanumeric() { upper as i64 } else { 0 };
            Some((code, vk, Some(c.to_string())))
        }
    }
}

struct Scaled {
    png: Vec<u8>,
    size: (u32, u32),
    original: (u32, u32),
}

/// Downscale a PNG so its width (full page) or longest edge (viewport) is at
/// most `max_edge`.
fn downscale_png(png: Vec<u8>, max_edge: u32, width_only: bool) -> Result<Scaled> {
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).context("decoding screenshot")?;
    let (w, h) = (img.width(), img.height());
    let edge = if width_only { w } else { w.max(h) };
    if max_edge == 0 || edge <= max_edge {
        return Ok(Scaled { png, size: (w, h), original: (w, h) });
    }
    let scale = f64::from(max_edge) / f64::from(edge);
    let nw = ((f64::from(w) * scale).round() as u32).max(1);
    let nh = ((f64::from(h) * scale).round() as u32).max(1);
    let resized = img.resize_exact(nw, nh, image::imageops::FilterType::Triangle);
    let mut out = Cursor::new(Vec::new());
    resized.write_to(&mut out, image::ImageFormat::Png).context("encoding screenshot")?;
    Ok(Scaled { png: out.into_inner(), size: (nw, nh), original: (w, h) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CdpEvent, TabInfo};
    use async_trait::async_trait;

    #[test]
    fn sensitive_fields_are_detected() {
        let f = |v: Value| sensitive_field_reason(&v);
        assert_eq!(f(json!({"type": "password"})), Some("password field"));
        assert_eq!(f(json!({"type": "text", "autocomplete": "username current-password"})), Some("password field"));
        assert_eq!(f(json!({"type": "text", "autocomplete": "cc-number"})), Some("payment card field"));
        assert_eq!(f(json!({"type": "tel", "name": "cardNumber"})), Some("payment field"));
        assert_eq!(f(json!({"type": "text", "id": "card-number"})), Some("payment field"));
        assert_eq!(f(json!({"type": "text", "placeholder": "CVC"})), Some("payment field"));
        assert_eq!(f(json!({"type": "text", "label": "Security code"})), Some("payment field"));
        assert_eq!(f(json!({"type": "text", "name": "user_password_confirm"})), Some("password field"));
        assert_eq!(f(json!({"type": "text", "name": "q", "placeholder": "Search docs"})), None);
        assert_eq!(f(json!({"type": "email", "autocomplete": "email", "label": "Email"})), None);
        assert_eq!(f(json!({"type": "text", "name": "discount", "label": "Card title"})), None);
        assert_eq!(f(json!({"type": "text", "label": "Card notes"})), None);
        assert_eq!(f(json!({"type": "text", "name": "billing_ccNumber"})), Some("payment field"));
    }

    #[test]
    fn url_and_ref_normalization() {
        assert_eq!(normalize_url("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalize_url("example.com/a"), "https://example.com/a");
        assert_eq!(normalize_url("http://x"), "http://x");
        assert_eq!(normalize_url("about:blank"), "about:blank");
        assert_eq!(normalize_ref("[e12]"), "e12");
        assert_eq!(normalize_ref("@e3"), "e3");
        assert_eq!(normalize_ref("ref=e4"), "e4");
    }

    #[test]
    fn png_downscaling() {
        let img = image::RgbaImage::from_pixel(400, 100, image::Rgba([10, 20, 30, 255]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img).write_to(&mut buf, image::ImageFormat::Png).unwrap();
        let png = buf.into_inner();
        let small = downscale_png(png.clone(), 200, false).unwrap();
        assert_eq!((small.size, small.original), ((200, 50), (400, 100)));
        assert!(small.png.starts_with(b"\x89PNG"));
        let same = downscale_png(png.clone(), 1000, false).unwrap();
        assert_eq!(same.size, (400, 100));
        assert_eq!(same.png, png);
        // Full-page shots only limit the width.
        let tall = downscale_png(png.clone(), 300, true).unwrap();
        assert_eq!(tall.size, (300, 75));
    }

    /// Scripted transport for logic that does not need a real browser.
    struct FakeTransport {
        calls: Mutex<Vec<String>>,
        url: String,
    }

    #[async_trait]
    impl CdpTransport for FakeTransport {
        async fn send(&self, method: &str, params: Value) -> Result<Value> {
            self.calls.lock().unwrap().push(method.to_string());
            match method {
                "Runtime.evaluate" if params["expression"] == "[location.href, document.title]" => {
                    Ok(json!({"result": {"type": "object", "value": [self.url, "Fake"]}}))
                }
                _ => Ok(json!({})),
            }
        }
        async fn drain_events(&self) -> Vec<CdpEvent> {
            Vec::new()
        }
        async fn tabs(&self) -> Result<Vec<TabInfo>> {
            Ok(vec![TabInfo { id: "T1".into(), url: self.url.clone(), title: "Fake".into(), active: true }])
        }
        async fn new_tab(&self, _url: &str) -> Result<TabInfo> {
            bail!("not supported")
        }
        async fn select_tab(&self, _id: &str) -> Result<()> {
            Ok(())
        }
        async fn close_tab(&self, _id: &str) -> Result<()> {
            Ok(())
        }
    }

    fn fake() -> BrowserSession<FakeTransport> {
        let t = FakeTransport { calls: Mutex::new(Vec::new()), url: "http://localhost/".into() };
        BrowserSession::new(t, BrowserOptions { blocked_sites: vec!["evil.test".into()], ..BrowserOptions::default() })
    }

    #[tokio::test]
    async fn site_policy_is_enforced_before_navigation() {
        let s = fake();
        let r = s.execute("navigate", &json!({"url": "https://evil.test/login"})).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("blocked"));
        let r = s.execute("navigate", &json!({"url": "https://unknown.example"})).await;
        assert!(r.error.unwrap().contains("ask the user"));
        let r = s.execute("new_tab", &json!({"url": "https://unknown.example"})).await;
        assert!(r.error.unwrap().contains("ask the user"));
        assert!(!s.transport().calls.lock().unwrap().iter().any(|c| c == "Page.navigate"));
        // After the user approves the site for the session it is allowed (still blocked list wins).
        s.allow_site("*.example");
        assert_eq!(s.site_decision_for("https://unknown.example"), SiteDecision::Allowed);
        s.allow_site("evil.test");
        assert_eq!(s.site_decision_for("https://evil.test"), SiteDecision::Blocked);
    }

    #[tokio::test]
    async fn developer_only_actions_and_bad_input() {
        let s = fake();
        let r = s.execute("eval", &json!({"expression": "1+1"})).await;
        assert!(!r.ok);
        assert!(r.error.unwrap().contains("developer mode"));
        let r = s.execute("cdp", &json!({"method": "Browser.getVersion"})).await;
        assert!(r.error.unwrap().contains("developer mode"));
        let r = s.execute("teleport", &json!({})).await;
        assert!(r.error.unwrap().contains("unknown browser action"));
        let r = s.execute("click", &json!({"ref": "e99"})).await;
        assert!(r.error.unwrap().contains("take a new snapshot"));
        let r = s.execute("tabs", &Value::Null).await;
        assert!(r.ok);
        assert_eq!(r.url.as_deref(), Some("http://localhost/"));
        assert!(r.text.unwrap().contains("* T1"));
    }
}
