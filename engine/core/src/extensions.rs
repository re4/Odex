//! Tools beyond the built-ins: MCP (with lazy loading), computer use and
//! browser use.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use odex_browser_bridge::{BrowserOptions, BrowserSession, CdpEvent, CdpTransport, TabInfo};
use odex_config::Settings;
use odex_context::ToolMeta;
use odex_llm::types::{ToolCall, ToolSpec};
use odex_llm::ModelHandle;
use odex_mcp_client::{McpEvent, McpManager};
use odex_protocol::*;

use crate::approval;
use crate::engine::Engine;
use crate::thread::ThreadRt;
use crate::toolexec::{ToolOutcome, TurnCtx};
use crate::turn::complete_item;

type DynSession = BrowserSession<Arc<dyn CdpTransport>>;

pub struct Extensions {
    pub mcp: McpManager,
    browsers: Mutex<HashMap<String, Arc<DynSession>>>,
    /// Last screenshot geometry per thread (for coordinate mapping).
    last_shot: Mutex<HashMap<String, odex_computer_use::Screenshot>>,
    headless: tokio::sync::Mutex<Option<(tokio::process::Child, String)>>,
}

impl Extensions {
    pub fn new(token_store: std::path::PathBuf) -> (Self, tokio::sync::mpsc::UnboundedReceiver<McpEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                mcp: McpManager::new(token_store, tx),
                browsers: Mutex::new(HashMap::new()),
                last_shot: Mutex::new(HashMap::new()),
                headless: tokio::sync::Mutex::new(None),
            },
            rx,
        )
    }
}

/// Forward MCP events to the client.
pub async fn pump_mcp_events(engine: Engine, mut rx: tokio::sync::mpsc::UnboundedReceiver<McpEvent>) {
    while let Some(ev) = rx.recv().await {
        match ev {
            McpEvent::Status(s) => {
                engine.emitter().raw(notification::MCP_STATUS_UPDATED, &McpStatusNotification { server: s })
            }
            McpEvent::ToolsChanged { .. } => {}
            McpEvent::Log { server, line } => tracing::debug!(target: "mcp", "{server}: {line}"),
            McpEvent::Elicitation { server, message, schema, respond } => {
                let e2 = engine.clone();
                tokio::spawn(async move {
                    let params =
                        ElicitationRequestParams { thread_id: None, server, message, requested_schema: schema };
                    let resp = e2
                        .emitter()
                        .request(server_request::ELICITATION_REQUEST, &params)
                        .await
                        .ok()
                        .and_then(|v| serde_json::from_value::<ElicitationResponse>(v).ok())
                        .unwrap_or(ElicitationResponse { action: "cancel".into(), content: None });
                    let _ = respond.send(resp);
                });
            }
            McpEvent::Progress { .. } => {}
        }
    }
}

/// MCP servers from config plus enabled, trusted plugins.
pub fn mcp_config(engine: &Engine) -> std::collections::BTreeMap<String, config_types::McpServerToml> {
    let mut servers = engine.user_settings().mcp_servers;
    for (name, cfg) in crate::plugins::mcp_servers(engine) {
        servers.entry(name).or_insert(cfg);
    }
    servers
}

pub async fn sync_mcp(engine: &Engine) {
    let servers = mcp_config(engine);
    engine.ext.mcp.set_servers(servers).await;
}

// ------------------------------------------------------------------ specs

fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec { name: name.into(), description: description.into(), parameters }
}

fn computer_specs() -> Vec<ToolSpec> {
    let window = json!({"type": "string", "description": "Window handle from window(op=list), or an app name like \"notepad.exe\"."});
    vec![
        spec(
            "screenshot",
            "Capture a screenshot of a window (works in the background), a monitor or the whole screen. Coordinates in later mouse calls refer to the latest screenshot.",
            json!({"type": "object", "properties": {
                "target": {"type": "string", "enum": ["window", "screen", "monitor"]},
                "window": window.clone(), "monitor": {"type": "integer"}
            }}),
        ),
        spec(
            "ui_tree",
            "Read a window's accessibility (UI Automation) tree: element ids, roles, names, values, states. Prefer this plus ui_action over mouse/keyboard.",
            json!({"type": "object", "properties": {"window": window.clone(), "depth": {"type": "integer"}, "filter": {"type": "string"}}, "required": ["window"]}),
        ),
        spec(
            "ui_action",
            "Act on an element from ui_tree without moving the user's mouse: invoke (click), focus, set_value, toggle, expand, collapse, select, scroll_into_view.",
            json!({"type": "object", "properties": {
                "window": window.clone(), "element": {"type": "string"},
                "action": {"type": "string", "enum": ["invoke", "focus", "set_value", "toggle", "expand", "collapse", "select", "scroll_into_view"]},
                "value": {"type": "string"}
            }, "required": ["window", "element", "action"]}),
        ),
        spec(
            "mouse",
            "Real mouse input (takes over the cursor; use only when ui_action can't). x/y are in the latest screenshot's coordinates; or give `target` to have the vision model locate an element.",
            json!({"type": "object", "properties": {
                "action": {"type": "string", "enum": ["move", "click", "double_click", "right_click", "drag", "scroll"]},
                "x": {"type": "number"}, "y": {"type": "number"}, "to_x": {"type": "number"}, "to_y": {"type": "number"},
                "dx": {"type": "integer"}, "dy": {"type": "integer"}, "target": {"type": "string"}, "window": window.clone()
            }, "required": ["action"]}),
        ),
        spec(
            "keyboard",
            "Real keyboard input to the focused window: `text` to type, or `keys` like \"ctrl+s\", \"enter\". Never type passwords.",
            json!({"type": "object", "properties": {"text": {"type": "string"}, "keys": {"type": "string"}, "window": window.clone()}}),
        ),
        spec(
            "window",
            "Manage windows: list, focus, move, resize, minimize, maximize, restore, close, launch (app name or path).",
            json!({"type": "object", "properties": {
                "op": {"type": "string", "enum": ["list", "focus", "move", "resize", "minimize", "maximize", "restore", "close", "launch"]},
                "window": window, "app": {"type": "string"}, "x": {"type": "integer"}, "y": {"type": "integer"},
                "width": {"type": "integer"}, "height": {"type": "integer"}
            }, "required": ["op"]}),
        ),
        spec(
            "clipboard",
            "Get or set the clipboard text.",
            json!({"type": "object", "properties": {"op": {"type": "string", "enum": ["get", "set"]}, "text": {"type": "string"}}, "required": ["op"]}),
        ),
        spec("wait", "Wait for the UI to settle.", json!({"type": "object", "properties": {"ms": {"type": "integer"}}, "required": ["ms"]})),
    ]
}

fn browser_specs(developer: bool) -> Vec<ToolSpec> {
    let target = |extra: Value| {
        let mut p = json!({"ref": {"type": "string", "description": "Element ref from browser_snapshot, e.g. e12."}, "selector": {"type": "string"}});
        if let (Value::Object(a), Value::Object(b)) = (&mut p, extra) {
            a.extend(b);
        }
        json!({"type": "object", "properties": p})
    };
    let mut v = vec![
        spec(
            "browser_navigate",
            "Open a URL in the in-app browser (local dev servers and public pages).",
            json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}),
        ),
        spec(
            "browser_snapshot",
            "Get the page's accessibility/DOM outline with element refs for clicking and typing.",
            json!({"type": "object", "properties": {}}),
        ),
        spec(
            "browser_click",
            "Click an element (by ref or selector) or a point.",
            target(json!({"x": {"type": "number"}, "y": {"type": "number"}})),
        ),
        spec(
            "browser_type",
            "Type text into an element; submit=true presses Enter. Never enter passwords or payment details.",
            {
                let mut t = target(json!({"text": {"type": "string"}, "submit": {"type": "boolean"}}));
                t["required"] = json!(["text"]);
                t
            },
        ),
        spec(
            "browser_select",
            "Choose an option in a <select> element.",
            target(json!({"value": {"type": "string"}, "label": {"type": "string"}})),
        ),
        spec(
            "browser_scroll",
            "Scroll the page or an element into view.",
            target(json!({"dx": {"type": "integer"}, "dy": {"type": "integer"}})),
        ),
        spec(
            "browser_screenshot",
            "Screenshot the current page.",
            json!({"type": "object", "properties": {"full_page": {"type": "boolean"}}}),
        ),
        spec(
            "browser_console",
            "Recent console messages and exceptions from the page.",
            json!({"type": "object", "properties": {}}),
        ),
        spec(
            "browser_network",
            "Recent network requests (method, URL, status).",
            json!({"type": "object", "properties": {}}),
        ),
    ];
    if developer {
        v.push(spec(
            "browser_eval",
            "Evaluate JavaScript in the page (developer mode).",
            json!({"type": "object", "properties": {"expression": {"type": "string"}}, "required": ["expression"]}),
        ));
    }
    v
}

fn mcp_tool_spec(t: &odex_mcp_client::McpTool) -> ToolSpec {
    let mut desc = t.description.clone().unwrap_or_default();
    if desc.len() > 1500 {
        desc = format!("{}…", desc.chars().take(1500).collect::<String>());
    }
    ToolSpec {
        name: t.qualified_name.clone(),
        description: format!("[MCP {}] {desc}", t.server),
        parameters: t.sanitized_schema.clone(),
    }
}

/// Should MCP tools be loaded lazily (via `search_tools`)?
pub fn lazy_mcp(engine: &Engine, s: &Settings, handle: Option<&ModelHandle>) -> bool {
    match s.mcp_lazy_tools.as_str() {
        "always" => true,
        "never" => false,
        _ => {
            let window = handle.map(|h| h.context_window).unwrap_or(32768);
            let est = odex_llm::tokens::TokenEstimator::default();
            let specs: Vec<ToolSpec> = engine.ext.mcp.tools().iter().filter(|t| t.enabled).map(mcp_tool_spec).collect();
            est.tools(&specs) as f64 > window as f64 * s.context.mcp_tool_budget_ratio
        }
    }
}

pub fn extra_tools(
    engine: &Engine,
    rt: &ThreadRt,
    t: &Thread,
    s: &Settings,
    handle: Option<&ModelHandle>,
) -> Vec<ToolSpec> {
    let mut v = Vec::new();
    let mcp_tools: Vec<odex_mcp_client::McpTool> = engine.ext.mcp.tools().into_iter().filter(|t| t.enabled).collect();
    if !mcp_tools.is_empty() {
        if lazy_mcp(engine, s, handle) {
            v.push(odex_tools::specs::search_tools());
            let active = rt.active_mcp_tools.lock().unwrap().clone();
            let mut act: Vec<&odex_mcp_client::McpTool> =
                mcp_tools.iter().filter(|m| active.contains(&m.qualified_name)).collect();
            act.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
            v.extend(act.into_iter().map(mcp_tool_spec));
        } else {
            let mut all = mcp_tools.clone();
            all.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
            v.extend(all.iter().map(mcp_tool_spec));
        }
    }
    if s.computer_use.enabled && t.kind != ThreadKind::Subagent {
        v.extend(computer_specs());
    }
    if s.browser.enabled && t.kind != ThreadKind::Subagent {
        v.extend(browser_specs(s.browser.developer_mode));
    }
    v
}

pub fn is_read_only_extra(name: &str) -> bool {
    matches!(
        name,
        "search_tools"
            | "screenshot"
            | "ui_tree"
            | "browser_snapshot"
            | "browser_screenshot"
            | "browser_console"
            | "browser_network"
            | "wait"
    )
}

// ------------------------------------------------------------------- run

fn meta(call: &ToolCall, summary: &str, success: bool) -> ToolMeta {
    ToolMeta {
        tool: call.name.clone(),
        call_id: call.id.clone(),
        args_summary: summary.to_string(),
        success,
        ..Default::default()
    }
}

fn outcome(call: &ToolCall, summary: &str, text: String, success: bool) -> ToolOutcome {
    ToolOutcome { text, meta: meta(call, summary, success), ..Default::default() }
}

pub async fn run_extra_tool(
    engine: &Engine,
    rt: &ThreadRt,
    tctx: &TurnCtx,
    call: &ToolCall,
    args: &Value,
    summary: &str,
) -> Option<ToolOutcome> {
    let name = call.name.as_str();
    if name == "search_tools" {
        return Some(search_tools(engine, rt, call, args, summary));
    }
    if name.starts_with("mcp__") {
        return Some(mcp_call(engine, rt, tctx, call, args, summary).await);
    }
    if name.starts_with("browser_") {
        return Some(browser_call(engine, rt, tctx, call, args, summary).await);
    }
    if matches!(name, "screenshot" | "ui_tree" | "ui_action" | "mouse" | "keyboard" | "window" | "clipboard" | "wait") {
        return Some(computer_call(engine, rt, tctx, call, args, summary).await);
    }
    None
}

fn search_tools(engine: &Engine, rt: &ThreadRt, call: &ToolCall, args: &Value, summary: &str) -> ToolOutcome {
    let q = args["query"].as_str().unwrap_or("").to_lowercase();
    let terms: Vec<&str> = q.split_whitespace().collect();
    let mut scored: Vec<(usize, odex_mcp_client::McpTool)> = engine
        .ext
        .mcp
        .tools()
        .into_iter()
        .filter(|t| t.enabled)
        .map(|t| {
            let hay = format!("{} {} {}", t.name, t.server, t.description.clone().unwrap_or_default()).to_lowercase();
            let score = terms.iter().filter(|w| hay.contains(*w)).count() * 10 + if hay.contains(&q) { 5 } else { 0 };
            (score, t)
        })
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by_key(|x| std::cmp::Reverse(x.0));
    let top: Vec<odex_mcp_client::McpTool> = scored.into_iter().take(8).map(|(_, t)| t).collect();
    if top.is_empty() {
        return outcome(call, summary, format!("No MCP tools match `{q}`."), true);
    }
    let mut active = rt.active_mcp_tools.lock().unwrap();
    let mut text = String::from("These tools are now available (call them directly on your next step):\n");
    for t in &top {
        active.insert(t.qualified_name.clone());
        text.push_str(&format!(
            "- {}: {}\n",
            t.qualified_name,
            t.description.clone().unwrap_or_default().chars().take(200).collect::<String>()
        ));
    }
    outcome(call, summary, text, true)
}

async fn mcp_call(
    engine: &Engine,
    rt: &ThreadRt,
    tctx: &TurnCtx,
    call: &ToolCall,
    args: &Value,
    summary: &str,
) -> ToolOutcome {
    let Some(tool) = engine.ext.mcp.find_tool(&call.name) else {
        return outcome(
            call,
            summary,
            format!("Error: MCP tool `{}` is not available (server not ready?).", call.name),
            false,
        );
    };
    let t = rt.thread();
    let item_id = format!("item_{}", call.id);
    let started = ThreadItem::McpToolCall {
        id: item_id.clone(),
        server: tool.server.clone(),
        tool: tool.name.clone(),
        arguments: args.clone(),
        status: ItemStatus::InProgress,
        result: None,
        error: None,
        duration_ms: None,
    };
    engine.emitter().item_started(&rt.id, &tctx.turn_id, &started);
    let session_ok = rt.session_allow.lock().unwrap().mcp_tools.contains(&call.name);
    let needs = match t.permission_mode {
        PermissionMode::FullAccess => false,
        PermissionMode::Auto => !(tool.auto_approve || tool.read_only_hint || session_ok),
        PermissionMode::ReadOnly => !(tool.auto_approve || session_ok) && !tool.read_only_hint,
    } || (tctx.mode != TurnMode::Default && !tool.read_only_hint);
    if needs {
        let kind = ApprovalKind::Mcp {
            server: tool.server.clone(),
            tool: tool.name.clone(),
            arguments: args.clone(),
            description: tool.description.clone(),
            read_only: tool.read_only_hint,
        };
        match approval::request(engine, rt, &tctx.turn_id, Some(item_id.clone()), kind, &tctx.cancel).await {
            ApprovalDecision::Approve | ApprovalDecision::Custom { .. } => {}
            ApprovalDecision::ApproveForSession => {
                rt.session_allow.lock().unwrap().mcp_tools.insert(call.name.clone());
            }
            ApprovalDecision::Deny { feedback } => {
                let msg = format!(
                    "The user declined this MCP call.{}",
                    feedback.map(|f| format!(" Feedback: {f}")).unwrap_or_default()
                );
                let mut it = started.clone();
                if let ThreadItem::McpToolCall { status, error, .. } = &mut it {
                    *status = ItemStatus::Declined;
                    *error = Some(msg.clone());
                }
                complete_item(engine, rt, &tctx.turn_id, it);
                return outcome(call, summary, msg, false);
            }
            ApprovalDecision::Abort => {
                return ToolOutcome {
                    text: "The user stopped the turn.".into(),
                    abort_turn: true,
                    meta: meta(call, summary, false),
                    ..Default::default()
                }
            }
        }
    }
    let t0 = std::time::Instant::now();
    let res = tokio::select! {
        r = engine.ext.mcp.call_tool(&call.name, args.clone()) => r,
        _ = tctx.cancel.cancelled() => Err(anyhow::anyhow!("interrupted")),
    };
    let duration = Some(t0.elapsed().as_millis() as u64);
    match res {
        Ok(r) => {
            let text = r.to_text();
            let images: Vec<String> =
                r.images().into_iter().map(|(mime, b64)| format!("data:{mime};base64,{b64}")).collect();
            let item = ThreadItem::McpToolCall {
                id: item_id,
                server: tool.server.clone(),
                tool: tool.name.clone(),
                arguments: args.clone(),
                status: if r.is_error { ItemStatus::Failed } else { ItemStatus::Completed },
                result: Some(json!({"content": r.content, "structuredContent": r.structured_content})),
                error: None,
                duration_ms: duration,
            };
            complete_item(engine, rt, &tctx.turn_id, item);
            let mut m = meta(call, summary, !r.is_error);
            let window = tctx.model.context_window;
            let cap = odex_tools::output::cap_tokens_for_window(window, tctx.settings.context.tool_output_max_tokens)
                as usize
                * 3;
            let r_ref = if text.len() > 1024 { rt.outputs.save(&text).ok() } else { None };
            m.output_ref = r_ref.clone();
            m.full_chars = text.len();
            let inline = odex_tools::output::cap(&text, cap, r_ref.as_deref()).text;
            let vision = tctx.model.model.capabilities.vision;
            ToolOutcome {
                text: if r.is_error { format!("MCP tool error: {inline}") } else { inline },
                images: if vision { images } else { vec![] },
                meta: m,
                ..Default::default()
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            let item = ThreadItem::McpToolCall {
                id: item_id,
                server: tool.server.clone(),
                tool: tool.name.clone(),
                arguments: args.clone(),
                status: ItemStatus::Failed,
                result: None,
                error: Some(msg.clone()),
                duration_ms: duration,
            };
            complete_item(engine, rt, &tctx.turn_id, item);
            outcome(call, summary, format!("Error calling MCP tool: {msg}"), false)
        }
    }
}

pub async fn mcp_resource_text(engine: &Engine, server: &str, uri: &str) -> String {
    match engine.ext.mcp.read_resource(server, uri).await {
        Ok(v) => {
            let mut s = format!("<resource server=\"{server}\" uri=\"{uri}\">\n");
            if let Some(items) = v.get("contents").and_then(|c| c.as_array()) {
                for it in items {
                    if let Some(t) = it.get("text").and_then(|t| t.as_str()) {
                        s.push_str(t);
                        s.push('\n');
                    } else if it.get("blob").is_some() {
                        s.push_str("[binary content omitted]\n");
                    }
                }
            } else {
                s.push_str(&v.to_string());
            }
            s.push_str("</resource>");
            s
        }
        Err(e) => format!("[could not read MCP resource {uri} from {server}: {e}]"),
    }
}

// --------------------------------------------------------------- browser

/// CDP over the desktop client (the in-app browser's webContents.debugger).
struct ClientCdp {
    engine: Engine,
    thread_id: String,
}

impl ClientCdp {
    async fn call(&self, action: &str, args: Value) -> anyhow::Result<Value> {
        let params = BrowserExecuteParams { thread_id: self.thread_id.clone(), action: action.into(), args };
        let v = self.engine.emitter().request(server_request::BROWSER_EXECUTE, &params).await?;
        let r: BrowserExecuteResponse = serde_json::from_value(v)?;
        if !r.ok {
            anyhow::bail!(r.error.unwrap_or_else(|| "browser command failed".into()));
        }
        let text = r.text.unwrap_or_default();
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}

#[async_trait]
impl CdpTransport for ClientCdp {
    async fn send(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.call("cdp", json!({"method": method, "params": params})).await
    }
    async fn drain_events(&self) -> Vec<CdpEvent> {
        self.call("events", json!({})).await.ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
    }
    async fn tabs(&self) -> anyhow::Result<Vec<TabInfo>> {
        Ok(serde_json::from_value(self.call("tabs", json!({})).await?)?)
    }
    async fn new_tab(&self, url: &str) -> anyhow::Result<TabInfo> {
        Ok(serde_json::from_value(self.call("new_tab", json!({"url": url})).await?)?)
    }
    async fn select_tab(&self, id: &str) -> anyhow::Result<()> {
        self.call("select_tab", json!({"id": id})).await.map(|_| ())
    }
    async fn close_tab(&self, id: &str) -> anyhow::Result<()> {
        self.call("close_tab", json!({"id": id})).await.map(|_| ())
    }
}

async fn browser_session(engine: &Engine, rt: &ThreadRt, s: &Settings) -> anyhow::Result<Arc<DynSession>> {
    if let Some(b) = engine.ext.browsers.lock().unwrap().get(&rt.id).cloned() {
        return Ok(b);
    }
    let transport: Arc<dyn CdpTransport> = if engine.emitter().sink().capabilities().browser {
        Arc::new(ClientCdp { engine: engine.clone(), thread_id: rt.id.clone() })
    } else {
        let endpoint = match &s.browser.cdp_url {
            Some(u) => u.clone(),
            None => {
                let mut g = engine.ext.headless.lock().await;
                if g.is_none() {
                    let dir = engine.home.tmp_dir().join("browser-profile");
                    let _ = std::fs::create_dir_all(&dir);
                    *g = Some(odex_browser_bridge::launch_headless_browser(&dir).await?);
                }
                g.as_ref().unwrap().1.clone()
            }
        };
        Arc::new(odex_browser_bridge::WsTransport::connect(&endpoint).await?)
    };
    let opts = BrowserOptions {
        developer_mode: s.browser.developer_mode,
        allowed_sites: s.browser.allowed_sites.clone(),
        blocked_sites: s.browser.blocked_sites.clone(),
        ..Default::default()
    };
    let session = Arc::new(BrowserSession::new(transport, opts));
    engine.ext.browsers.lock().unwrap().insert(rt.id.clone(), session.clone());
    Ok(session)
}

fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| url.to_string())
}

async fn browser_call(
    engine: &Engine,
    rt: &ThreadRt,
    tctx: &TurnCtx,
    call: &ToolCall,
    args: &Value,
    summary: &str,
) -> ToolOutcome {
    if engine.kill_switch.load(std::sync::atomic::Ordering::SeqCst) {
        return outcome(
            call,
            summary,
            "Error: the kill switch is engaged; browser actions are stopped. Ask the user to release it.".into(),
            false,
        );
    }
    let s = &tctx.settings;
    let action = call.name.trim_start_matches("browser_").to_string();
    let session = match browser_session(engine, rt, s).await {
        Ok(b) => b,
        Err(e) => return outcome(call, summary, format!("Error: browser unavailable: {e:#}"), false),
    };
    let item_id = format!("item_{}", call.id);
    let mut item = ThreadItem::Browser {
        id: item_id.clone(),
        action: action.clone(),
        arguments: args.clone(),
        status: ItemStatus::InProgress,
        url: None,
        output: None,
        image: None,
    };
    engine.emitter().item_started(&rt.id, &tctx.turn_id, &item);
    // site permission for navigation
    if action == "navigate" {
        let url = args["url"].as_str().unwrap_or("");
        if session.site_decision_for(url) == odex_browser_bridge::SiteDecision::Ask
            && !rt.session_allow.lock().unwrap().sites.contains(&host_of(url))
        {
            let kind = ApprovalKind::Browser { site: host_of(url), action: action.clone(), arguments: args.clone() };
            match approval::request(engine, rt, &tctx.turn_id, Some(item_id.clone()), kind, &tctx.cancel).await {
                ApprovalDecision::Approve | ApprovalDecision::Custom { .. } => session.allow_site(&host_of(url)),
                ApprovalDecision::ApproveForSession => {
                    session.allow_site(&host_of(url));
                    rt.session_allow.lock().unwrap().sites.insert(host_of(url));
                }
                ApprovalDecision::Deny { .. } => {
                    if let ThreadItem::Browser { status, output, .. } = &mut item {
                        *status = ItemStatus::Declined;
                        *output = Some("site not approved".into());
                    }
                    complete_item(engine, rt, &tctx.turn_id, item);
                    return outcome(call, summary, format!("The user did not allow visiting {}.", host_of(url)), false);
                }
                ApprovalDecision::Abort => {
                    return ToolOutcome {
                        text: "The user stopped the turn.".into(),
                        abort_turn: true,
                        meta: meta(call, summary, false),
                        ..Default::default()
                    }
                }
            }
        }
    }
    let r = session.execute(&action, args).await;
    let ok = r.ok;
    let mut text = String::new();
    if let Some(u) = &r.url {
        text.push_str(&format!("URL: {u}\n"));
    }
    if let Some(t) = &r.title {
        text.push_str(&format!("Title: {t}\n"));
    }
    if let Some(t) = &r.text {
        text.push_str(t);
    }
    if let Some(e) = &r.error {
        text.push_str(&format!("Error: {e}"));
    }
    let mut images = vec![];
    if let Some(img) = &r.image {
        if tctx.model.model.capabilities.vision {
            images.push(img.clone());
            text.push_str("\n[screenshot attached]");
        } else {
            let t = rt.thread();
            let desc = crate::turn::describe_image(
                engine,
                &t,
                img,
                "Describe this web page screenshot for a coding agent: layout, visible text, errors.",
            )
            .await;
            text.push_str(&format!("\nScreenshot (described by the vision model):\n{desc}"));
        }
    }
    if let ThreadItem::Browser { status, url, output, image, .. } = &mut item {
        *status = if ok { ItemStatus::Completed } else { ItemStatus::Failed };
        *url = r.url.clone();
        *output = Some(text.chars().take(4000).collect());
        *image = r.image.clone();
    }
    complete_item(engine, rt, &tctx.turn_id, item);
    let mut m = meta(call, summary, ok);
    let cap = odex_tools::output::cap_tokens_for_window(tctx.model.context_window, s.context.tool_output_max_tokens)
        as usize
        * 3;
    let r_ref = if text.len() > 1024 { rt.outputs.save(&text).ok() } else { None };
    m.output_ref = r_ref.clone();
    ToolOutcome {
        text: odex_tools::output::cap(&text, cap, r_ref.as_deref()).text,
        images,
        meta: m,
        ..Default::default()
    }
}

// --------------------------------------------------------- computer use

fn resolve_window(spec: &str, allowed: &[String]) -> Result<WindowInfo, String> {
    let spec = spec.trim();
    let windows = odex_computer_use::list_windows(allowed).map_err(|e| e.to_string())?;
    if let Ok(h) = spec.parse::<isize>() {
        if let Some(w) = windows.iter().find(|w| w.handle == h.to_string()) {
            return Ok(w.clone());
        }
    }
    let l = spec.to_lowercase();
    windows
        .iter()
        .find(|w| {
            w.app.to_lowercase() == l || w.app.to_lowercase().trim_end_matches(".exe") == l.trim_end_matches(".exe")
        })
        .or_else(|| windows.iter().find(|w| w.title.to_lowercase().contains(&l)))
        .cloned()
        .ok_or_else(|| format!("no window matches `{spec}`; call window(op=list) to see windows"))
}

fn thumb(png: &[u8]) -> Option<String> {
    use base64::Engine as _;
    let small = odex_tools::downscale_png(png, 480).ok()?;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(small)))
}

#[allow(clippy::too_many_arguments)]
async fn computer_call(
    engine: &Engine,
    rt: &ThreadRt,
    tctx: &TurnCtx,
    call: &ToolCall,
    args: &Value,
    summary: &str,
) -> ToolOutcome {
    let s = tctx.settings.clone();
    let cu = &s.computer_use;
    if !cu.enabled {
        return outcome(call, summary, "Error: computer use is disabled (Settings → Computer Use).".into(), false);
    }
    if engine.kill_switch.load(std::sync::atomic::Ordering::SeqCst) || odex_computer_use::kill_switch::is_engaged() {
        return outcome(
            call,
            summary,
            "Error: the kill switch is engaged; computer actions are stopped. Ask the user to release it.".into(),
            false,
        );
    }
    let action = call.name.clone();
    let allowed = cu.allowed_apps.clone();
    // resolve the target window
    let win = match args.get("window").and_then(|w| w.as_str()) {
        Some(w) => match tokio::task::spawn_blocking({
            let w = w.to_string();
            let a = allowed.clone();
            move || resolve_window(&w, &a)
        })
        .await
        {
            Ok(Ok(w)) => Some(w),
            Ok(Err(e)) => return outcome(call, summary, format!("Error: {e}"), false),
            Err(e) => return outcome(call, summary, format!("Error: {e}"), false),
        },
        None => None,
    };
    let is_input = matches!(action.as_str(), "ui_action" | "mouse" | "keyboard" | "clipboard")
        || (action == "window" && args["op"].as_str() != Some("list"));
    let app = win
        .as_ref()
        .map(|w| w.app.clone())
        .or_else(|| args.get("app").and_then(|a| a.as_str()).map(String::from))
        .unwrap_or_else(|| "desktop".into());
    let item_id = format!("item_{}", call.id);
    // per-app access + per-action approval
    let app_allowed = win.as_ref().map(|w| w.allowed).unwrap_or(
        action == "wait" || (action == "window" && args["op"].as_str() == Some("list")) || action == "clipboard",
    );
    let session_ok = rt.session_allow.lock().unwrap().apps.contains(&app.to_lowercase());
    let needs = (!app_allowed || (cu.require_approval && is_input))
        && !session_ok
        && rt.thread().permission_mode != PermissionMode::FullAccess
        || (!app_allowed && !session_ok);
    if needs && action != "wait" {
        let shot = win
            .as_ref()
            .and_then(|w| w.handle.parse::<isize>().ok())
            .and_then(|h| odex_computer_use::screenshot(odex_computer_use::CaptureTarget::Window(h), 800).ok())
            .map(|s| s.data_url());
        let kind = ApprovalKind::ComputerUse {
            app: app.clone(),
            action: action.clone(),
            arguments: args.clone(),
            screenshot: shot,
        };
        match approval::request(engine, rt, &tctx.turn_id, Some(item_id.clone()), kind, &tctx.cancel).await {
            ApprovalDecision::Approve | ApprovalDecision::Custom { .. } => {}
            ApprovalDecision::ApproveForSession => {
                rt.session_allow.lock().unwrap().apps.insert(app.to_lowercase());
            }
            ApprovalDecision::Deny { feedback } => {
                return outcome(
                    call,
                    summary,
                    format!("The user declined this action.{}", feedback.map(|f| format!(" {f}")).unwrap_or_default()),
                    false,
                )
            }
            ApprovalDecision::Abort => {
                return ToolOutcome {
                    text: "The user stopped the turn.".into(),
                    abort_turn: true,
                    meta: meta(call, summary, false),
                    ..Default::default()
                }
            }
        }
    }
    let hwnd = win.as_ref().and_then(|w| w.handle.parse::<isize>().ok());
    let before = if is_input {
        hwnd.and_then(|h| odex_computer_use::screenshot(odex_computer_use::CaptureTarget::Window(h), 960).ok())
            .and_then(|s| thumb(&s.png))
    } else {
        None
    };
    let takeover = matches!(action.as_str(), "mouse" | "keyboard");
    if is_input {
        engine.emitter().raw(
            notification::COMPUTER_USE_ACTIVE,
            &ComputerUseActiveNotification {
                active: true,
                thread_id: Some(rt.id.clone()),
                app: Some(app.clone()),
                takeover,
            },
        );
    }
    let max_px = tctx.model.model.max_image_px;
    let space = tctx.model.model.coordinate_space;
    let vision = tctx.model.model.capabilities.vision;
    let last = engine.ext.last_shot.lock().unwrap().get(&rt.id).cloned();
    let a = args.clone();
    let t_for_vision = rt.thread();
    // locate a described target with the vision model when no coordinates were given
    let mut located: Option<(f64, f64)> = None;
    if action == "mouse" && a.get("x").is_none() {
        if let (Some(target), Some(shot)) = (a.get("target").and_then(|t| t.as_str()), last.as_ref()) {
            located = locate(engine, &t_for_vision, shot, target).await;
        }
    }
    let res: Result<(String, Option<odex_computer_use::Screenshot>), String> =
        tokio::task::spawn_blocking(move || -> Result<(String, Option<odex_computer_use::Screenshot>), String> {
            use odex_computer_use as cu;
            cu::init_dpi_awareness();
            let e = |x: cu::Error| x.to_string();
            match action.as_str() {
                "screenshot" => {
                    let target = match (a["target"].as_str(), hwnd) {
                        (Some("screen"), _) => cu::CaptureTarget::Screen,
                        (Some("monitor"), _) => cu::CaptureTarget::Monitor(a["monitor"].as_u64().unwrap_or(0) as usize),
                        (_, Some(h)) => cu::CaptureTarget::Window(h),
                        (_, None) => cu::CaptureTarget::Screen,
                    };
                    let shot = cu::screenshot(target, max_px).map_err(e)?;
                    Ok((
                        format!(
                            "Screenshot {}x{}{}",
                            shot.width,
                            shot.height,
                            shot.note.as_ref().map(|n| format!(" ({n})")).unwrap_or_default()
                        ),
                        Some(shot),
                    ))
                }
                "ui_tree" => {
                    let h = hwnd.ok_or("window is required")?;
                    let tree = cu::ui_tree(h, a["depth"].as_u64().unwrap_or(12) as u32, a["filter"].as_str(), 12_000)
                        .map_err(e)?;
                    Ok((tree.text, None))
                }
                "ui_action" => {
                    cu::security_check().map_err(e)?;
                    let h = hwnd.ok_or("window is required")?;
                    let el = a["element"].as_str().unwrap_or("");
                    let kind = match a["action"].as_str().unwrap_or("") {
                        "invoke" => cu::UiActionKind::Invoke,
                        "focus" => cu::UiActionKind::Focus,
                        "set_value" => cu::UiActionKind::SetValue(a["value"].as_str().unwrap_or("").to_string()),
                        "toggle" => cu::UiActionKind::Toggle,
                        "expand" => cu::UiActionKind::Expand,
                        "collapse" => cu::UiActionKind::Collapse,
                        "select" => cu::UiActionKind::Select,
                        "scroll_into_view" => cu::UiActionKind::ScrollIntoView,
                        other => return Err(format!("unknown ui action {other}")),
                    };
                    Ok((cu::ui_action(h, el, kind).map_err(e)?, None))
                }
                "mouse" => {
                    cu::security_check().map_err(e)?;
                    let shot = last.as_ref().ok_or("take a screenshot first so coordinates can be mapped")?;
                    let (x, y) = match (a["x"].as_f64(), a["y"].as_f64(), located) {
                        (Some(x), Some(y), _) => (x, y),
                        (_, _, Some(p)) => p,
                        _ => return Err("give x/y (from the latest screenshot) or a target description".into()),
                    };
                    let (px, py) = cu::map_point(shot, x, y, space);
                    if let Some(target_win) = cu::window_at(px, py, &allowed).map_err(e)? {
                        if !target_win.allowed && !allowed.is_empty() {
                            return Err(format!("the point is over `{}`, which is not an allowed app", target_win.app));
                        }
                    }
                    let act = match a["action"].as_str().unwrap_or("click") {
                        "move" => cu::MouseAction::Move,
                        "double_click" => cu::MouseAction::Click { button: cu::MouseButton::Left, double: true },
                        "right_click" => cu::MouseAction::Click { button: cu::MouseButton::Right, double: false },
                        "drag" => {
                            let (tx, ty) = cu::map_point(
                                shot,
                                a["to_x"].as_f64().unwrap_or(x),
                                a["to_y"].as_f64().unwrap_or(y),
                                space,
                            );
                            cu::MouseAction::Drag { to_x: tx, to_y: ty }
                        }
                        "scroll" => cu::MouseAction::Scroll {
                            dx: a["dx"].as_i64().unwrap_or(0) as i32,
                            dy: a["dy"].as_i64().unwrap_or(-3) as i32,
                        },
                        _ => cu::MouseAction::Click { button: cu::MouseButton::Left, double: false },
                    };
                    cu::mouse(act, px, py).map_err(e)?;
                    Ok((format!("Mouse {} at ({px}, {py}) physical", a["action"].as_str().unwrap_or("click")), None))
                }
                "keyboard" => {
                    cu::security_check().map_err(e)?;
                    if let Some(h) = hwnd {
                        cu::window_op(h, cu::WindowOp::Focus).map_err(e)?;
                        cu::expect_foreground(h).map_err(e)?;
                    }
                    if let Some(t) = a["text"].as_str() {
                        cu::keyboard_type(t).map_err(e)?;
                    }
                    if let Some(k) = a["keys"].as_str() {
                        cu::keyboard_keys(k).map_err(e)?;
                    }
                    Ok(("Keyboard input sent.".into(), None))
                }
                "window" => {
                    let op = a["op"].as_str().unwrap_or("list");
                    match op {
                        "list" => {
                            let ws = cu::list_windows(&allowed).map_err(e)?;
                            let lines: Vec<String> = ws
                                .iter()
                                .map(|w| {
                                    format!(
                                        "{} | {} | \"{}\"{}",
                                        w.handle,
                                        w.app,
                                        w.title,
                                        if w.allowed { "" } else { " (not allowed)" }
                                    )
                                })
                                .collect();
                            Ok((format!("handle | app | title\n{}", lines.join("\n")), None))
                        }
                        "launch" => {
                            let app = a["app"].as_str().ok_or("app is required")?;
                            let w = cu::launch_and_wait(app, &[], std::time::Duration::from_secs(15)).map_err(e)?;
                            Ok((format!("Launched {} (window {} \"{}\")", w.app, w.handle, w.title), None))
                        }
                        other => {
                            let h = hwnd.ok_or("window is required")?;
                            let wop = match other {
                                "focus" => cu::WindowOp::Focus,
                                "move" => cu::WindowOp::Move {
                                    x: a["x"].as_i64().unwrap_or(0) as i32,
                                    y: a["y"].as_i64().unwrap_or(0) as i32,
                                },
                                "resize" => cu::WindowOp::Resize {
                                    width: a["width"].as_i64().unwrap_or(800) as i32,
                                    height: a["height"].as_i64().unwrap_or(600) as i32,
                                },
                                "minimize" => cu::WindowOp::Minimize,
                                "maximize" => cu::WindowOp::Maximize,
                                "restore" => cu::WindowOp::Restore,
                                "close" => cu::WindowOp::Close,
                                x => return Err(format!("unknown window op {x}")),
                            };
                            cu::window_op(h, wop).map_err(e)?;
                            Ok((format!("Window {other} done."), None))
                        }
                    }
                }
                "clipboard" => match a["op"].as_str().unwrap_or("get") {
                    "set" => {
                        cu::clipboard_set(a["text"].as_str().unwrap_or("")).map_err(e)?;
                        Ok(("Clipboard set.".into(), None))
                    }
                    _ => Ok((cu::clipboard_get().map_err(e)?, None)),
                },
                "wait" => {
                    std::thread::sleep(std::time::Duration::from_millis(a["ms"].as_u64().unwrap_or(500).min(30_000)));
                    Ok(("Waited.".into(), None))
                }
                other => Err(format!("unknown computer action {other}")),
            }
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
    if is_input {
        engine.emitter().raw(
            notification::COMPUTER_USE_ACTIVE,
            &ComputerUseActiveNotification {
                active: false,
                thread_id: Some(rt.id.clone()),
                app: Some(app.clone()),
                takeover: false,
            },
        );
    }
    let after = if is_input {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        hwnd.and_then(|h| odex_computer_use::screenshot(odex_computer_use::CaptureTarget::Window(h), 960).ok())
            .and_then(|s| thumb(&s.png))
    } else {
        None
    };
    let (ok, mut text, shot) = match res {
        Ok((t, s)) => (true, t, s),
        Err(e) => (false, format!("Error: {e}"), None),
    };
    let mut images = vec![];
    let mut after_img = after;
    if let Some(shot) = shot {
        let url = shot.data_url();
        after_img = thumb(&shot.png);
        engine.ext.last_shot.lock().unwrap().insert(rt.id.clone(), shot.clone());
        let coord_hint = match space {
            CoordinateSpace::Pixels => format!("Coordinates: pixels of this {}x{} image.", shot.width, shot.height),
            CoordinateSpace::Normalized1000 => "Coordinates: normalized 0-1000 on each axis.".into(),
            CoordinateSpace::Normalized1 => "Coordinates: normalized 0-1 on each axis.".into(),
        };
        if vision {
            images.push(url);
            text.push_str(&format!("\n{coord_hint} The screenshot is attached."));
        } else {
            let t = rt.thread();
            let desc = crate::turn::describe_image(
                engine,
                &t,
                &url,
                "Describe this screenshot for an agent that cannot see it: list visible UI elements with their approximate positions, all visible text, dialogs and errors.",
            )
            .await;
            text.push_str(&format!("\nDescribed by the vision model:\n{desc}\n(Prefer ui_tree + ui_action to act; or mouse with `target` to have the vision model locate an element.)"));
        }
    }
    let item = ThreadItem::ComputerUse {
        id: item_id,
        action: call.name.clone(),
        arguments: args.clone(),
        status: if ok { ItemStatus::Completed } else { ItemStatus::Failed },
        app: Some(app),
        output: Some(text.chars().take(4000).collect()),
        before_image: before,
        after_image: after_img,
    };
    engine.emitter().item_started(&rt.id, &tctx.turn_id, &item);
    complete_item(engine, rt, &tctx.turn_id, item);
    ToolOutcome { text, images, meta: meta(call, summary, ok), ..Default::default() }
}

/// Ask the vision model where `target` is in the screenshot.
async fn locate(engine: &Engine, t: &Thread, shot: &odex_computer_use::Screenshot, target: &str) -> Option<(f64, f64)> {
    let h = engine.role_model(ModelRole::Vision, Some(t)).filter(|h| h.model.capabilities.vision)?;
    let space = match h.model.coordinate_space {
        CoordinateSpace::Pixels => format!("pixel coordinates of this {}x{} image", shot.width, shot.height),
        CoordinateSpace::Normalized1000 => "coordinates normalized to 0-1000".into(),
        CoordinateSpace::Normalized1 => "coordinates normalized to 0-1".into(),
    };
    let prompt = format!("Find: {target}. Reply with JSON {{\"x\": number, \"y\": number}} giving the center of that element in {space}. If it is not visible, reply {{\"x\": null, \"y\": null}}.");
    let text = crate::turn::describe_image(engine, t, &shot.data_url(), &prompt).await;
    let (v, _) = odex_llm::repair::parse_lenient(&text).ok()?;
    let (mut x, mut y) = (v.get("x")?.as_f64()?, v.get("y")?.as_f64()?);
    // convert the vision model's space into the main model's space (pixels assumed)
    match h.model.coordinate_space {
        CoordinateSpace::Normalized1000 => {
            x = x / 1000.0 * shot.width as f64;
            y = y / 1000.0 * shot.height as f64;
        }
        CoordinateSpace::Normalized1 => {
            x *= shot.width as f64;
            y *= shot.height as f64;
        }
        CoordinateSpace::Pixels => {}
    }
    // `mouse` maps from the main model's space; express the point in that space
    Some(
        match t
            .model
            .as_ref()
            .and_then(|k| engine.registry.resolve(k))
            .map(|m| m.model.coordinate_space)
            .unwrap_or_default()
        {
            CoordinateSpace::Normalized1000 => (x / shot.width as f64 * 1000.0, y / shot.height as f64 * 1000.0),
            CoordinateSpace::Normalized1 => (x / shot.width as f64, y / shot.height as f64),
            CoordinateSpace::Pixels => (x, y),
        },
    )
}
