//! [`McpManager`]: owns every configured server, starts them in parallel,
//! tracks status/tools/logs and routes calls.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use odex_protocol::config_types::McpServerToml;
use odex_protocol::{
    ElicitationResponse, McpPromptInfo, McpResourceInfo, McpServerState, McpServerStatus, McpToolInfo,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use url::Url;

use crate::client::{ConnEvent, Connection, InitInfo};
use crate::error::McpError;
use crate::logbuf::LogSink;
use crate::naming::qualify_tool_name;
use crate::oauth::{self, TokenSource, TokenStore};
use crate::schema::{root_is_wrapped, sanitize_schema, WRAPPED_ARG};
use crate::transport::http::{HttpShared, StreamableHttp};
use crate::transport::legacy_sse::LegacySse;
use crate::transport::stdio::{StdioConfig, StdioTransport};
use crate::transport::{AbortOnDrop, Transport};
use crate::types::{CallToolResult, McpEvent, McpTool};
use crate::validate::validate_arguments;
use crate::{Result, DEFAULT_STARTUP_TIMEOUT_MS, DEFAULT_TOOL_TIMEOUT_MS};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(600);

/// Manages the MCP servers of one engine. Cheap to clone.
#[derive(Clone)]
pub struct McpManager {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<TokenStore>,
    events: UnboundedSender<McpEvent>,
    http: reqwest::Client,
    servers: Mutex<BTreeMap<String, Arc<Server>>>,
    logins: Mutex<HashMap<String, AbortOnDrop>>,
    progress_counter: AtomicU64,
}

struct Server {
    name: String,
    config: Mutex<McpServerToml>,
    log: Arc<LogSink>,
    st: Mutex<ServerState>,
    /// Current generation, so in-flight startups notice they were superseded.
    gen_tx: tokio::sync::watch::Sender<u64>,
}

struct ServerState {
    generation: u64,
    state: McpServerState,
    error: Option<String>,
    init: Option<InitInfo>,
    tools: Vec<RawTool>,
    resources: Vec<McpResourceInfo>,
    prompts: Vec<McpPromptInfo>,
    authenticated: bool,
    conn: Option<Arc<Connection>>,
}

impl ServerState {
    fn new() -> Self {
        ServerState {
            generation: 0,
            state: McpServerState::Starting,
            error: None,
            init: None,
            tools: Vec::new(),
            resources: Vec::new(),
            prompts: Vec::new(),
            authenticated: false,
            conn: None,
        }
    }

    fn clear_runtime(&mut self) {
        self.error = None;
        self.init = None;
        self.tools.clear();
        self.resources.clear();
        self.prompts.clear();
        self.conn = None;
    }
}

#[derive(Debug, Clone)]
struct RawTool {
    name: String,
    description: Option<String>,
    input_schema: Value,
    sanitized_schema: Value,
    read_only: bool,
    destructive: bool,
    schema_tokens: u32,
}

impl RawTool {
    fn parse(v: &Value) -> Option<RawTool> {
        let name = v.get("name").and_then(Value::as_str)?.to_string();
        let annotations = v.get("annotations");
        let description = v
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| v.get("title").and_then(Value::as_str))
            .or_else(|| annotations.and_then(|a| a.get("title")).and_then(Value::as_str))
            .map(str::to_string);
        let input_schema = v.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object"}));
        let sanitized_schema = sanitize_schema(&input_schema);
        let flag = |k: &str| annotations.and_then(|a| a.get(k)).and_then(Value::as_bool);
        let read_only = flag("readOnlyHint").unwrap_or(false);
        let destructive = !read_only && flag("destructiveHint").unwrap_or(true);
        let chars = name.len() + description.as_deref().map_or(0, str::len) + sanitized_schema.to_string().len();
        Some(RawTool {
            name,
            description,
            input_schema,
            sanitized_schema,
            read_only,
            destructive,
            schema_tokens: (chars / 4 + 8) as u32,
        })
    }
}

fn parse_resource(v: &Value) -> Option<McpResourceInfo> {
    let uri = v.get("uri").and_then(Value::as_str)?.to_string();
    Some(McpResourceInfo {
        name: v.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| uri.clone()),
        description: v.get("description").and_then(Value::as_str).map(str::to_string),
        mime_type: v.get("mimeType").and_then(Value::as_str).map(str::to_string),
        uri,
    })
}

fn parse_prompt(v: &Value) -> Option<McpPromptInfo> {
    let arguments = v.get("arguments").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(|x| {
                Some(odex_protocol::McpPromptArgument {
                    name: x.get("name").and_then(Value::as_str)?.to_string(),
                    description: x.get("description").and_then(Value::as_str).map(str::to_string),
                    required: x.get("required").and_then(Value::as_bool),
                })
            })
            .collect()
    });
    Some(McpPromptInfo {
        name: v.get("name").and_then(Value::as_str)?.to_string(),
        description: v.get("description").and_then(Value::as_str).map(str::to_string),
        arguments,
    })
}

fn transport_kind(cfg: &McpServerToml) -> &'static str {
    if cfg.command.is_none() && cfg.url.is_some() {
        "http"
    } else {
        "stdio"
    }
}

fn startup_timeout(cfg: &McpServerToml) -> Duration {
    Duration::from_millis(cfg.startup_timeout_ms.map(u64::from).unwrap_or(DEFAULT_STARTUP_TIMEOUT_MS).max(1))
}

fn tool_timeout(cfg: &McpServerToml) -> Duration {
    Duration::from_millis(cfg.tool_timeout_ms.map(u64::from).unwrap_or(DEFAULT_TOOL_TIMEOUT_MS).max(1))
}

fn list_matches(list: &[String], name: &str) -> bool {
    list.iter().any(|t| t == name || t == "*")
}

fn tool_enabled(cfg: &McpServerToml, name: &str) -> bool {
    let allowed = match &cfg.enabled_tools {
        Some(list) => list_matches(list, name),
        None => true,
    };
    allowed && !list_matches(&cfg.disabled_tools, name)
}

/// Fields whose change requires reconnecting (everything but tool filters).
fn same_connection(a: &McpServerToml, b: &McpServerToml) -> bool {
    let strip = |c: &McpServerToml| McpServerToml {
        enabled_tools: None,
        disabled_tools: Vec::new(),
        auto_approve_tools: Vec::new(),
        ..c.clone()
    };
    strip(a) == strip(b)
}

enum ConnectError {
    NeedsAuth(String),
    Failed(String),
}

struct Ready {
    conn: Arc<Connection>,
    init: InitInfo,
    tools: Vec<RawTool>,
    resources: Vec<McpResourceInfo>,
    prompts: Vec<McpPromptInfo>,
    authenticated: bool,
}

impl Server {
    fn new(name: &str, config: McpServerToml, events: UnboundedSender<McpEvent>) -> Self {
        Server {
            name: name.to_string(),
            config: Mutex::new(config),
            log: Arc::new(LogSink::new(name, events)),
            st: Mutex::new(ServerState::new()),
            gen_tx: tokio::sync::watch::channel(0).0,
        }
    }

    fn config(&self) -> McpServerToml {
        self.config.lock().unwrap().clone()
    }

    fn generation(&self) -> u64 {
        self.st.lock().unwrap().generation
    }
}

impl McpManager {
    /// `token_store`: JSON file for OAuth tokens. `events`: status/log/elicitation stream.
    pub fn new(token_store: PathBuf, events: UnboundedSender<McpEvent>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .user_agent(concat!("odex-mcp-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        McpManager {
            inner: Arc::new(Inner {
                store: Arc::new(TokenStore::new(token_store)),
                events,
                http,
                servers: Mutex::new(BTreeMap::new()),
                logins: Mutex::new(HashMap::new()),
                progress_counter: AtomicU64::new(1),
            }),
        }
    }

    /// Apply a new server set: start new/changed servers (in parallel), stop
    /// removed ones; disabled servers get the `Disabled` state. Returns once
    /// every started server settled (Ready / Failed / NeedsAuth) — spawn it if
    /// you don't want to wait for slow servers. Changes to tool filters only
    /// (`enabled_tools`, `disabled_tools`, `auto_approve_tools`) don't restart.
    pub async fn set_servers(&self, servers: BTreeMap<String, McpServerToml>) {
        let mut to_start = Vec::new();
        let mut to_stop = Vec::new();
        let mut filters_changed = Vec::new();
        {
            let mut map = self.inner.servers.lock().unwrap();
            let removed: Vec<String> = map.keys().filter(|k| !servers.contains_key(*k)).cloned().collect();
            for name in removed {
                to_stop.extend(map.remove(&name));
            }
            for (name, cfg) in servers {
                match map.get(&name) {
                    Some(existing) => {
                        let old = existing.config();
                        if old == cfg {
                            continue;
                        }
                        let reconnect = !same_connection(&old, &cfg);
                        *existing.config.lock().unwrap() = cfg;
                        if reconnect {
                            to_start.push(existing.clone());
                        } else {
                            filters_changed.push(existing.clone());
                        }
                    }
                    None => {
                        let s = Arc::new(Server::new(&name, cfg, self.inner.events.clone()));
                        map.insert(name, s.clone());
                        to_start.push(s);
                    }
                }
            }
        }
        for s in &filters_changed {
            self.inner.emit_status(s);
            self.inner.emit(McpEvent::ToolsChanged { server: s.name.clone() });
        }
        let mut handles = Vec::new();
        for s in to_stop {
            let inner = self.inner.clone();
            handles.push(tokio::spawn(async move { inner.stop_server(&s, true).await }));
        }
        for s in to_start {
            let inner = self.inner.clone();
            handles.push(tokio::spawn(async move { inner.start_server(s).await }));
        }
        futures::future::join_all(handles).await;
    }

    /// Reconnect one server. Errors if the server is unknown or failed to start.
    pub async fn restart(&self, name: &str) -> Result<()> {
        let server = self.inner.get(name)?;
        self.inner.clone().start_server(server.clone()).await;
        let st = server.st.lock().unwrap();
        match st.state {
            McpServerState::Failed => Err(McpError::Other(st.error.clone().unwrap_or_else(|| "failed".into())).into()),
            _ => Ok(()),
        }
    }

    pub fn statuses(&self) -> Vec<McpServerStatus> {
        let servers = self.inner.server_list();
        let names = self.inner.qualified_names(&servers);
        servers.iter().map(|s| self.inner.build_status(s, &names)).collect()
    }

    /// Tools of Ready servers that pass `enabled_tools` / `disabled_tools`.
    pub fn tools(&self) -> Vec<McpTool> {
        self.inner.all_tools().into_iter().filter(|t| t.enabled).collect()
    }

    /// Look up a tool by qualified name (also returns disabled tools, with `enabled == false`).
    pub fn find_tool(&self, qualified: &str) -> Option<McpTool> {
        self.inner.all_tools().into_iter().find(|t| t.qualified_name == qualified)
    }

    /// Validate `args` against the tool's original schema, then call it with
    /// the server's `tool_timeout_ms`.
    pub async fn call_tool(&self, qualified: &str, args: Value) -> Result<CallToolResult> {
        let tool = self.find_tool(qualified).ok_or_else(|| McpError::UnknownTool(qualified.to_string()))?;
        if !tool.enabled {
            return Err(McpError::ToolDisabled(qualified.to_string()).into());
        }
        let server = self.inner.get(&tool.server)?;
        let conn = self.inner.ready_conn(&server)?;
        let mut args = if args.is_null() { json!({}) } else { args };
        if root_is_wrapped(&tool.input_schema) {
            if let Some(inner) = args.get(WRAPPED_ARG) {
                args = inner.clone();
            }
        }
        validate_arguments(&tool.input_schema, &args)
            .map_err(|message| McpError::InvalidArguments { tool: qualified.to_string(), message })?;
        let token = format!("odex-{}", self.inner.progress_counter.fetch_add(1, Ordering::SeqCst));
        let params = json!({"name": tool.name, "arguments": args, "_meta": {"progressToken": token}});
        let timeout = tool_timeout(&server.config());
        let result = conn.request("tools/call", Some(params), timeout).await?;
        Ok(CallToolResult::from_value(result))
    }

    /// Fresh `resources/list` (also refreshes the cached list in the status).
    pub async fn list_resources(&self, server: &str) -> Result<Vec<McpResourceInfo>> {
        let s = self.inner.get(server)?;
        let conn = self.inner.ready_conn(&s)?;
        let items = conn.list_all("resources/list", "resources", tool_timeout(&s.config())).await?;
        let resources: Vec<McpResourceInfo> = items.iter().filter_map(parse_resource).collect();
        s.st.lock().unwrap().resources = resources.clone();
        Ok(resources)
    }

    /// `resources/read`; returns the raw result (`{"contents": [...]}`).
    pub async fn read_resource(&self, server: &str, uri: &str) -> Result<Value> {
        let s = self.inner.get(server)?;
        let conn = self.inner.ready_conn(&s)?;
        Ok(conn.request("resources/read", Some(json!({"uri": uri})), tool_timeout(&s.config())).await?)
    }

    /// `prompts/get`; returns the raw result (`{"description", "messages": [...]}`).
    /// Non-string argument values are stringified (MCP prompt args are strings).
    pub async fn get_prompt(&self, server: &str, name: &str, args: Value) -> Result<Value> {
        let s = self.inner.get(server)?;
        let conn = self.inner.ready_conn(&s)?;
        let mut params = json!({"name": name});
        if let Value::Object(map) = args {
            let stringified: serde_json::Map<String, Value> = map
                .into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| match v {
                    Value::String(_) => (k, v),
                    other => (k, Value::String(other.to_string())),
                })
                .collect();
            params["arguments"] = Value::Object(stringified);
        }
        Ok(conn.request("prompts/get", Some(params), tool_timeout(&s.config())).await?)
    }

    /// Last ~500 log lines (stderr, protocol errors, server log messages).
    pub fn logs(&self, name: &str) -> Vec<String> {
        self.inner.servers.lock().unwrap().get(name).map(|s| s.log.lines()).unwrap_or_default()
    }

    /// `instructions` from the `initialize` result of every Ready server.
    pub fn instructions(&self) -> Vec<(String, String)> {
        self.inner
            .server_list()
            .iter()
            .filter_map(|s| {
                let st = s.st.lock().unwrap();
                if st.state != McpServerState::Ready {
                    return None;
                }
                let text = st.init.as_ref()?.instructions.clone()?;
                Some((s.name.clone(), text))
            })
            .collect()
    }

    /// Start an OAuth login. Returns the authorization URL for the browser;
    /// the redirect is handled in the background, after which the server is
    /// restarted (a `Status` event reports the outcome).
    pub async fn login(&self, name: &str) -> Result<String> {
        let server = self.inner.get(name)?;
        let cfg = server.config();
        let url = cfg
            .url
            .as_deref()
            .filter(|_| cfg.command.is_none())
            .ok_or_else(|| McpError::Config(format!("`{name}` is not an HTTP server")))?;
        let url = Url::parse(url).map_err(|e| McpError::Config(format!("invalid url: {e}")))?;
        let headers = custom_headers(&cfg).map_err(McpError::Config)?;
        let previous = self.inner.store.get(name).await;
        let pending = oauth::begin_login(&self.inner.http, &url, &headers, previous).await?;
        let auth_url = pending.authorization_url.clone();
        server.log.info("OAuth login started; waiting for the browser redirect");

        let weak = Arc::downgrade(&self.inner);
        let http = self.inner.http.clone();
        let name_owned = name.to_string();
        let task = tokio::spawn(async move {
            let result = pending.complete(&http, LOGIN_TIMEOUT).await;
            let Some(inner) = weak.upgrade() else { return };
            let Ok(server) = inner.get(&name_owned) else { return };
            let result = match result {
                Ok(tok) => inner.store.put(&name_owned, tok).await,
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => {
                    server.log.info("OAuth login complete");
                    inner.start_server(server).await;
                }
                Err(e) => {
                    server.log.error(format!("OAuth login failed: {e}"));
                    let gen = server.generation();
                    inner.update_state(&server, gen, |st| {
                        if st.state == McpServerState::Ready {
                            return false;
                        }
                        st.state = McpServerState::NeedsAuth;
                        st.error = Some(format!("OAuth login failed: {e}"));
                        true
                    });
                }
            }
        });
        // Replacing an older login aborts it (and frees its port).
        self.inner.logins.lock().unwrap().insert(name.to_string(), AbortOnDrop(task));
        Ok(auth_url)
    }

    /// Forget stored OAuth tokens and reconnect (→ `NeedsAuth` if the server requires auth).
    pub async fn logout(&self, name: &str) -> Result<()> {
        let server = self.inner.get(name)?;
        self.inner.logins.lock().unwrap().remove(name);
        self.inner.store.remove(name).await?;
        server.log.info("OAuth tokens removed");
        self.inner.clone().start_server(server).await;
        Ok(())
    }

    /// Stop every server (graceful close, then kill).
    pub async fn shutdown(&self) {
        self.inner.logins.lock().unwrap().clear();
        let servers: Vec<Arc<Server>> =
            std::mem::take(&mut *self.inner.servers.lock().unwrap()).into_values().collect();
        let stops = servers.iter().map(|s| self.inner.stop_server(s, true));
        futures::future::join_all(stops).await;
    }
}

fn custom_headers(cfg: &McpServerToml) -> std::result::Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (k, v) in &cfg.headers {
        let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| format!("invalid header name `{k}`: {e}"))?;
        let value = HeaderValue::from_str(v).map_err(|e| format!("invalid value for header `{k}`: {e}"))?;
        headers.insert(name, value);
    }
    Ok(headers)
}

impl Inner {
    fn emit(&self, ev: McpEvent) {
        let _ = self.events.send(ev);
    }

    fn get(&self, name: &str) -> std::result::Result<Arc<Server>, McpError> {
        self.servers.lock().unwrap().get(name).cloned().ok_or_else(|| McpError::UnknownServer(name.to_string()))
    }

    fn server_list(&self) -> Vec<Arc<Server>> {
        self.servers.lock().unwrap().values().cloned().collect()
    }

    fn ready_conn(&self, server: &Server) -> std::result::Result<Arc<Connection>, McpError> {
        let st = server.st.lock().unwrap();
        match (&st.state, &st.conn) {
            (McpServerState::Ready, Some(c)) => Ok(c.clone()),
            (state, _) => {
                let mut desc = format!("{state:?}").to_ascii_lowercase();
                if let Some(e) = &st.error {
                    desc.push_str(": ");
                    desc.push_str(e);
                }
                Err(McpError::NotReady { server: server.name.clone(), state: desc })
            }
        }
    }

    /// Deterministic qualified names for every tool of every Ready server
    /// (servers by name, tools by name).
    fn qualified_names(&self, servers: &[Arc<Server>]) -> HashMap<(String, String), String> {
        let mut taken = HashSet::new();
        let mut out = HashMap::new();
        for s in servers {
            let mut names: Vec<String> = {
                let st = s.st.lock().unwrap();
                if st.state != McpServerState::Ready {
                    continue;
                }
                st.tools.iter().map(|t| t.name.clone()).collect()
            };
            names.sort();
            for n in names {
                let q = qualify_tool_name(&s.name, &n, &taken);
                taken.insert(q.clone());
                out.insert((s.name.clone(), n), q);
            }
        }
        out
    }

    fn all_tools(&self) -> Vec<McpTool> {
        let servers = self.server_list();
        let names = self.qualified_names(&servers);
        let mut out = Vec::new();
        for s in &servers {
            let cfg = s.config();
            let st = s.st.lock().unwrap();
            if st.state != McpServerState::Ready {
                continue;
            }
            for t in &st.tools {
                let Some(q) = names.get(&(s.name.clone(), t.name.clone())) else { continue };
                out.push(McpTool {
                    qualified_name: q.clone(),
                    server: s.name.clone(),
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.input_schema.clone(),
                    sanitized_schema: t.sanitized_schema.clone(),
                    read_only_hint: t.read_only,
                    destructive_hint: t.destructive,
                    auto_approve: list_matches(&cfg.auto_approve_tools, &t.name),
                    enabled: tool_enabled(&cfg, &t.name),
                });
            }
        }
        out
    }

    fn build_status(&self, s: &Server, names: &HashMap<(String, String), String>) -> McpServerStatus {
        let cfg = s.config();
        let st = s.st.lock().unwrap();
        let ready = st.state == McpServerState::Ready;
        let tools = if ready {
            st.tools
                .iter()
                .map(|t| McpToolInfo {
                    qualified_name: names
                        .get(&(s.name.clone(), t.name.clone()))
                        .cloned()
                        .unwrap_or_else(|| qualify_tool_name(&s.name, &t.name, &HashSet::new())),
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.input_schema.clone(),
                    enabled: tool_enabled(&cfg, &t.name),
                    read_only_hint: t.read_only,
                    auto_approve: list_matches(&cfg.auto_approve_tools, &t.name),
                    schema_tokens: t.schema_tokens,
                })
                .collect()
        } else {
            Vec::new()
        };
        McpServerStatus {
            name: s.name.clone(),
            transport: transport_kind(&cfg).to_string(),
            enabled: cfg.enabled.unwrap_or(true),
            state: st.state,
            error: st.error.clone(),
            server_name: st.init.as_ref().and_then(|i| i.server_name.clone()),
            server_version: st.init.as_ref().and_then(|i| i.server_version.clone()),
            tools,
            resources: if ready { st.resources.clone() } else { Vec::new() },
            prompts: if ready { st.prompts.clone() } else { Vec::new() },
            authenticated: st.authenticated,
        }
    }

    fn emit_status(&self, s: &Server) {
        let servers = self.server_list();
        let names = self.qualified_names(&servers);
        self.emit(McpEvent::Status(self.build_status(s, &names)));
    }

    /// Mutate the state if `gen` is still current; emits a status when `f` returns true.
    fn update_state(&self, s: &Server, gen: u64, f: impl FnOnce(&mut ServerState) -> bool) -> bool {
        let changed = {
            let mut st = s.st.lock().unwrap();
            if st.generation != gen {
                return false;
            }
            f(&mut st)
        };
        if changed {
            self.emit_status(s);
        }
        changed
    }

    /// Bump the generation (invalidating in-flight work) and detach the connection.
    fn begin_generation(&self, s: &Server) -> (u64, Option<Arc<Connection>>, bool) {
        let mut st = s.st.lock().unwrap();
        st.generation += 1;
        s.gen_tx.send_replace(st.generation);
        let was_ready = st.state == McpServerState::Ready;
        (st.generation, st.conn.take(), was_ready)
    }

    async fn stop_server(&self, s: &Arc<Server>, removed: bool) {
        let (gen, conn, was_ready) = self.begin_generation(s);
        {
            let mut st = s.st.lock().unwrap();
            if st.generation == gen {
                st.clear_runtime();
                st.state = McpServerState::Stopped;
            }
        }
        if let Some(conn) = conn {
            s.log.info("stopping");
            conn.close().await;
        }
        if removed {
            self.emit(McpEvent::Status(self.build_status(s, &HashMap::new())));
        } else {
            self.emit_status(s);
        }
        if was_ready {
            self.emit(McpEvent::ToolsChanged { server: s.name.clone() });
        }
    }

    async fn start_server(self: Arc<Self>, s: Arc<Server>) {
        let (gen, old, was_ready) = self.begin_generation(&s);
        if let Some(old) = old {
            old.close().await;
        }
        let cfg = s.config();
        if !cfg.enabled.unwrap_or(true) {
            self.update_state(&s, gen, |st| {
                st.clear_runtime();
                st.state = McpServerState::Disabled;
                true
            });
            if was_ready {
                self.emit(McpEvent::ToolsChanged { server: s.name.clone() });
            }
            return;
        }
        self.update_state(&s, gen, |st| {
            st.clear_runtime();
            st.state = McpServerState::Starting;
            true
        });
        if was_ready {
            self.emit(McpEvent::ToolsChanged { server: s.name.clone() });
        }

        let startup = startup_timeout(&cfg);
        let mut gen_rx = s.gen_tx.subscribe();
        let result = tokio::select! {
            r = tokio::time::timeout(startup, self.connect(&s, gen, &cfg, startup)) => match r {
                Ok(r) => r,
                Err(_) => Err(ConnectError::Failed(format!("startup timed out after {} ms", startup.as_millis()))),
            },
            // Superseded (restart / stop / new config): dropping the connect
            // future kills the half-started server right away.
            _ = gen_rx.wait_for(|g| *g != gen) => return,
        };
        match result {
            Ok(ready) => {
                let summary = format!(
                    "ready: {} {} (protocol {}), {} tools, {} resources, {} prompts",
                    ready.init.server_name.as_deref().unwrap_or("server"),
                    ready.init.server_version.as_deref().unwrap_or(""),
                    ready.init.protocol_version,
                    ready.tools.len(),
                    ready.resources.len(),
                    ready.prompts.len()
                );
                let conn = ready.conn.clone();
                let applied = self.update_state(&s, gen, move |st| {
                    st.state = McpServerState::Ready;
                    st.error = None;
                    st.init = Some(ready.init);
                    st.tools = ready.tools;
                    st.resources = ready.resources;
                    st.prompts = ready.prompts;
                    st.authenticated = ready.authenticated;
                    st.conn = Some(ready.conn);
                    true
                });
                if applied {
                    s.log.info(summary);
                    self.emit(McpEvent::ToolsChanged { server: s.name.clone() });
                } else {
                    conn.close().await;
                }
            }
            Err(ConnectError::NeedsAuth(msg)) => {
                let line = format!("authentication required: {msg}");
                if self.update_state(&s, gen, |st| {
                    st.state = McpServerState::NeedsAuth;
                    st.error = Some(msg);
                    st.authenticated = false;
                    true
                }) {
                    s.log.info(line);
                }
            }
            Err(ConnectError::Failed(msg)) => {
                let line = format!("failed to start: {msg}");
                if self.update_state(&s, gen, |st| {
                    st.state = McpServerState::Failed;
                    st.error = Some(msg);
                    true
                }) {
                    s.log.error(line);
                }
            }
        }
    }

    fn spawn_event_loop(self: &Arc<Self>, s: &Arc<Server>, gen: u64, events: UnboundedReceiver<ConnEvent>) {
        tokio::spawn(conn_event_loop(Arc::downgrade(self), Arc::downgrade(s), gen, events));
    }

    fn failure_message(&self, s: &Server, e: &McpError) -> String {
        let mut msg = e.to_string();
        if matches!(e, McpError::Closed(_) | McpError::Timeout { .. }) {
            if let Some(line) = s.log.last_stderr() {
                msg.push_str(&format!(" — last stderr: {line}"));
            }
        }
        msg
    }

    async fn connect(
        self: &Arc<Self>,
        s: &Arc<Server>,
        gen: u64,
        cfg: &McpServerToml,
        startup: Duration,
    ) -> std::result::Result<Ready, ConnectError> {
        s.log.clear_last_stderr();
        let (conn, init, authenticated) = match (&cfg.command, &cfg.url) {
            (Some(_), Some(_)) => {
                return Err(ConnectError::Failed("both `command` and `url` are set; use one".into()));
            }
            (Some(command), None) => {
                let (conn, init) = self.connect_stdio(s, gen, cfg, command, startup).await?;
                (conn, init, false)
            }
            (None, Some(url)) => self.connect_http(s, gen, cfg, url, startup).await?,
            (None, None) => {
                return Err(ConnectError::Failed("set either `command` (stdio) or `url` (http)".into()));
            }
        };

        let tools = if init.has_capability("tools") {
            list_tools(&conn, startup).await.map_err(|e| ConnectError::Failed(format!("tools/list failed: {e}")))?
        } else {
            // Some servers omit capabilities; try anyway.
            list_tools(&conn, startup).await.unwrap_or_default()
        };
        let mut resources = Vec::new();
        if init.has_capability("resources") {
            match conn.list_all("resources/list", "resources", startup).await {
                Ok(items) => resources = items.iter().filter_map(parse_resource).collect(),
                Err(e) => s.log.error(format!("resources/list failed: {e}")),
            }
        }
        let mut prompts = Vec::new();
        if init.has_capability("prompts") {
            match conn.list_all("prompts/list", "prompts", startup).await {
                Ok(items) => prompts = items.iter().filter_map(parse_prompt).collect(),
                Err(e) => s.log.error(format!("prompts/list failed: {e}")),
            }
        }
        Ok(Ready { conn, init, tools, resources, prompts, authenticated })
    }

    async fn connect_stdio(
        self: &Arc<Self>,
        s: &Arc<Server>,
        gen: u64,
        cfg: &McpServerToml,
        command: &str,
        startup: Duration,
    ) -> std::result::Result<(Arc<Connection>, InitInfo), ConnectError> {
        let mut shown = command.to_string();
        for a in &cfg.args {
            shown.push(' ');
            shown.push_str(a);
        }
        s.log.info(format!("starting `{shown}`"));
        let (tx, rx) = unbounded_channel();
        let stdio = StdioConfig {
            command: command.to_string(),
            args: cfg.args.clone(),
            env: cfg.env.clone(),
            cwd: cfg.cwd.clone(),
        };
        let transport =
            StdioTransport::spawn(&stdio, tx, s.log.clone()).await.map_err(|e| ConnectError::Failed(e.to_string()))?;
        let (conn, events) = Connection::new(Arc::new(transport), rx, s.log.clone());
        self.spawn_event_loop(s, gen, events);
        let conn = Arc::new(conn);
        match conn.initialize(startup).await {
            Ok(init) => Ok((conn, init)),
            Err(e) => Err(ConnectError::Failed(self.failure_message(s, &e))),
        }
    }

    async fn connect_http(
        self: &Arc<Self>,
        s: &Arc<Server>,
        gen: u64,
        cfg: &McpServerToml,
        url: &str,
        startup: Duration,
    ) -> std::result::Result<(Arc<Connection>, InitInfo, bool), ConnectError> {
        let url = Url::parse(url).map_err(|e| ConnectError::Failed(format!("invalid url `{url}`: {e}")))?;
        let mut headers = custom_headers(cfg).map_err(ConnectError::Failed)?;
        let static_bearer = match (&cfg.bearer_token, &cfg.bearer_token_env_var) {
            (Some(t), _) => Some(t.clone()),
            (None, Some(var)) => Some(std::env::var(var).map_err(|_| {
                ConnectError::Failed(format!("environment variable `{var}` (bearer_token_env_var) is not set"))
            })?),
            (None, None) => None,
        };
        if let Some(t) = &static_bearer {
            let v = HeaderValue::from_str(&format!("Bearer {t}"))
                .map_err(|_| ConnectError::Failed("bearer token contains invalid characters".into()))?;
            headers.insert(AUTHORIZATION, v);
        }
        let oauth_allowed = static_bearer.is_none() && cfg.oauth != Some(false);
        let tokens =
            if oauth_allowed {
                self.store.get(&s.name).await.filter(|t| t.matches_server(url.as_str())).map(|t| {
                    Arc::new(TokenSource::new(&s.name, t, self.store.clone(), self.http.clone(), s.log.clone()))
                })
            } else {
                None
            };
        let authenticated = tokens.is_some();
        if cfg.oauth == Some(true) && tokens.is_none() {
            return Err(ConnectError::NeedsAuth("sign in to this server (OAuth)".into()));
        }
        let classify = |e: McpError| -> ConnectError {
            match e {
                McpError::Unauthorized { .. } if oauth_allowed => {
                    ConnectError::NeedsAuth("the server requires authentication; sign in (OAuth)".into())
                }
                McpError::Unauthorized { .. } if static_bearer.is_some() => {
                    ConnectError::Failed("HTTP 401 Unauthorized — check the bearer token".into())
                }
                other => ConnectError::Failed(other.to_string()),
            }
        };
        let shared = |inbound| HttpShared {
            http: self.http.clone(),
            url: url.clone(),
            headers: headers.clone(),
            tokens: tokens.clone(),
            inbound,
            log: s.log.clone(),
        };

        // Streamable HTTP first.
        let (tx, rx) = unbounded_channel();
        let transport: Arc<dyn Transport> = Arc::new(StreamableHttp::new(shared(tx)));
        let (conn, events) = Connection::new(transport, rx, s.log.clone());
        self.spawn_event_loop(s, gen, events);
        let status = match conn.initialize(startup).await {
            Ok(init) => return Ok((Arc::new(conn), init, authenticated)),
            Err(McpError::Http { status, .. }) if matches!(status, 400 | 404 | 405) => status,
            Err(e) => return Err(classify(e)),
        };
        drop(conn);

        // Legacy HTTP+SSE fallback.
        s.log.info(format!("Streamable HTTP rejected (HTTP {status}); trying the legacy HTTP+SSE transport"));
        let (tx, rx) = unbounded_channel();
        let legacy = LegacySse::connect(shared(tx), startup).await.map_err(|e| match e {
            McpError::Http { .. } => {
                ConnectError::Failed(format!("server rejected Streamable HTTP (HTTP {status}) and legacy SSE ({e})"))
            }
            other => classify(other),
        })?;
        let (conn, events) = Connection::new(Arc::new(legacy), rx, s.log.clone());
        self.spawn_event_loop(s, gen, events);
        let init = conn.initialize(startup).await.map_err(classify)?;
        Ok((Arc::new(conn), init, authenticated))
    }

    fn on_notification(self: &Arc<Self>, s: &Arc<Server>, gen: u64, method: &str, params: Value) {
        match method {
            "notifications/tools/list_changed" => {
                tokio::spawn(refresh_list(self.clone(), s.clone(), gen, ListKind::Tools));
            }
            "notifications/resources/list_changed" => {
                tokio::spawn(refresh_list(self.clone(), s.clone(), gen, ListKind::Resources));
            }
            "notifications/prompts/list_changed" => {
                tokio::spawn(refresh_list(self.clone(), s.clone(), gen, ListKind::Prompts));
            }
            "notifications/message" => {
                let level = params.get("level").and_then(Value::as_str).unwrap_or("info");
                let data = match params.get("data") {
                    Some(Value::String(t)) => t.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                match params.get("logger").and_then(Value::as_str) {
                    Some(logger) => s.log.push(format!("[{level}] {logger}: {data}")),
                    None => s.log.push(format!("[{level}] {data}")),
                }
            }
            "notifications/progress" => self.emit(McpEvent::Progress {
                server: s.name.clone(),
                token: params.get("progressToken").cloned().unwrap_or(Value::Null),
                progress: params.get("progress").and_then(Value::as_f64).unwrap_or(0.0),
                total: params.get("total").and_then(Value::as_f64),
                message: params.get("message").and_then(Value::as_str).map(str::to_string),
            }),
            "notifications/cancelled" => {
                let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
                s.log.info(format!(
                    "server cancelled request {}: {reason}",
                    params.get("requestId").unwrap_or(&Value::Null)
                ));
            }
            other => tracing::debug!(server = %s.name, "ignoring MCP notification {other}"),
        }
    }

    fn on_elicitation(&self, s: &Server, params: Value, respond: oneshot::Sender<Value>) {
        let message = params.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        let schema =
            params.get("requestedSchema").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        s.log.info(format!("elicitation: {message}"));
        let (tx, rx) = oneshot::channel::<ElicitationResponse>();
        if self.events.send(McpEvent::Elicitation { server: s.name.clone(), message, schema, respond: tx }).is_err() {
            let _ = respond.send(json!({"action": "cancel"}));
            return;
        }
        tokio::spawn(async move {
            let reply = match rx.await {
                Ok(r) => {
                    let action = match r.action.to_ascii_lowercase().as_str() {
                        a @ ("accept" | "decline" | "cancel") => a.to_string(),
                        _ => "cancel".to_string(),
                    };
                    let mut v = json!({"action": action});
                    if action == "accept" {
                        v["content"] = r.content.unwrap_or_else(|| json!({}));
                    }
                    v
                }
                Err(_) => json!({"action": "cancel"}),
            };
            let _ = respond.send(reply);
        });
    }

    fn on_closed(&self, s: &Server, gen: u64, reason: String) {
        let msg = self.failure_message(s, &McpError::Closed(reason));
        let mut was_ready = false;
        self.update_state(s, gen, |st| {
            if st.state != McpServerState::Ready {
                return false; // startup failures are reported by start_server
            }
            was_ready = true;
            st.clear_runtime();
            st.state = McpServerState::Failed;
            st.error = Some(msg.clone());
            true
        });
        if was_ready {
            s.log.error(msg);
            self.emit(McpEvent::ToolsChanged { server: s.name.clone() });
        }
    }
}

async fn list_tools(conn: &Connection, timeout: Duration) -> std::result::Result<Vec<RawTool>, McpError> {
    let items = conn.list_all("tools/list", "tools", timeout).await?;
    let mut seen = HashSet::new();
    Ok(items.iter().filter_map(RawTool::parse).filter(|t| seen.insert(t.name.clone())).collect())
}

#[derive(Clone, Copy)]
enum ListKind {
    Tools,
    Resources,
    Prompts,
}

async fn refresh_list(inner: Arc<Inner>, s: Arc<Server>, gen: u64, kind: ListKind) {
    // A list_changed during startup: wait for the initial listing to land.
    let mut conn = None;
    for _ in 0..100 {
        {
            let st = s.st.lock().unwrap();
            if st.generation != gen {
                return;
            }
            match st.state {
                McpServerState::Ready => {
                    conn = st.conn.clone();
                    break;
                }
                McpServerState::Starting => {}
                _ => return,
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let Some(conn) = conn else { return };
    let timeout = startup_timeout(&s.config());
    match kind {
        ListKind::Tools => match list_tools(&conn, timeout).await {
            Ok(tools) => {
                let n = tools.len();
                if inner.update_state(&s, gen, |st| {
                    st.tools = tools;
                    true
                }) {
                    s.log.info(format!("tool list changed ({n} tools)"));
                    inner.emit(McpEvent::ToolsChanged { server: s.name.clone() });
                }
            }
            Err(e) => s.log.error(format!("refreshing tools failed: {e}")),
        },
        ListKind::Resources => match conn.list_all("resources/list", "resources", timeout).await {
            Ok(items) => {
                let resources = items.iter().filter_map(parse_resource).collect();
                inner.update_state(&s, gen, |st| {
                    st.resources = resources;
                    true
                });
            }
            Err(e) => s.log.error(format!("refreshing resources failed: {e}")),
        },
        ListKind::Prompts => match conn.list_all("prompts/list", "prompts", timeout).await {
            Ok(items) => {
                let prompts = items.iter().filter_map(parse_prompt).collect();
                inner.update_state(&s, gen, |st| {
                    st.prompts = prompts;
                    true
                });
            }
            Err(e) => s.log.error(format!("refreshing prompts failed: {e}")),
        },
    }
}

async fn conn_event_loop(inner: Weak<Inner>, server: Weak<Server>, gen: u64, mut rx: UnboundedReceiver<ConnEvent>) {
    while let Some(ev) = rx.recv().await {
        let (Some(inner), Some(s)) = (inner.upgrade(), server.upgrade()) else { break };
        if s.generation() != gen {
            if let ConnEvent::Elicitation { respond, .. } = ev {
                let _ = respond.send(json!({"action": "cancel"}));
            }
            continue;
        }
        match ev {
            ConnEvent::Notification { method, params } => inner.on_notification(&s, gen, &method, params),
            ConnEvent::Elicitation { params, respond } => inner.on_elicitation(&s, params, respond),
            ConnEvent::Closed(reason) => inner.on_closed(&s, gen, reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_arguments_are_parsed() {
        let p = parse_prompt(&json!({"name": "greet", "arguments": [{"name": "who", "description": "Name", "required": true}, {"x": 1}]}))
            .unwrap();
        let args = p.arguments.unwrap();
        assert_eq!(args.len(), 1);
        assert_eq!((args[0].name.as_str(), args[0].required), ("who", Some(true)));
        assert!(parse_prompt(&json!({"name": "bare"})).unwrap().arguments.is_none());
    }

    #[test]
    fn tool_filters() {
        let cfg = McpServerToml {
            enabled_tools: Some(vec!["a".into(), "b".into()]),
            disabled_tools: vec!["b".into()],
            ..Default::default()
        };
        assert!(tool_enabled(&cfg, "a"));
        assert!(!tool_enabled(&cfg, "b"));
        assert!(!tool_enabled(&cfg, "c"));
        assert!(tool_enabled(&McpServerToml::default(), "anything"));
        let star = McpServerToml { disabled_tools: vec!["*".into()], ..Default::default() };
        assert!(!tool_enabled(&star, "a"));
    }

    #[test]
    fn filter_changes_do_not_reconnect() {
        let a = McpServerToml { command: Some("x".into()), ..Default::default() };
        let b = McpServerToml { disabled_tools: vec!["t".into()], auto_approve_tools: vec!["u".into()], ..a.clone() };
        assert!(same_connection(&a, &b));
        let c = McpServerToml { args: vec!["--flag".into()], ..a.clone() };
        assert!(!same_connection(&a, &c));
    }

    #[test]
    fn raw_tool_parsing() {
        let t = RawTool::parse(&json!({
            "name": "write",
            "description": "Write a file",
            "inputSchema": {"type": "object", "properties": {"p": {"type": ["string", "null"]}}},
            "annotations": {"readOnlyHint": false}
        }))
        .unwrap();
        assert!(t.destructive && !t.read_only);
        assert_eq!(t.sanitized_schema["properties"]["p"]["type"], "string");
        let r = RawTool::parse(&json!({"name": "read", "annotations": {"readOnlyHint": true}})).unwrap();
        assert!(r.read_only && !r.destructive);
        assert_eq!(r.sanitized_schema, json!({"type": "object", "properties": {}}));
        assert!(RawTool::parse(&json!({"description": "no name"})).is_none());
    }
}
