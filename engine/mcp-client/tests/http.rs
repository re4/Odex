//! Streamable HTTP, legacy SSE and OAuth tests against an in-process axum server.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Form, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use odex_mcp_client::{McpError, McpEvent, McpManager};
use odex_protocol::config_types::McpServerToml;
use odex_protocol::McpServerState;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

// ---------------------------------------------------------------------------
// Fake server

#[derive(Default)]
struct Inner {
    base: String,
    sessions: HashSet<String>,
    next_session: u32,
    init_count: u32,
    protocol_headers: Vec<String>,
    deleted: Vec<String>,
    registered_redirects: Vec<String>,
    codes: HashMap<String, (String, String)>,
    authorize_params: HashMap<String, String>,
    refreshes: u32,
    legacy_tx: Option<UnboundedSender<String>>,
}

type St = Arc<Mutex<Inner>>;

const VALID_TOKEN: &str = "valid-token";

fn tools_list() -> Value {
    json!({"tools": [
        {"name": "echo", "description": "Echo", "inputSchema": {"type": "object",
            "properties": {"text": {"type": "string"}}, "required": ["text"]}},
        {"name": "expire", "description": "Expire the session", "inputSchema": {"type": "object"}}
    ]})
}

/// Result for a request (shared by the streamable and legacy endpoints).
fn result_for(st: &St, method: &str, params: &Value, session: Option<&str>) -> Value {
    match method {
        "tools/list" => tools_list(),
        "tools/call" => match params["name"].as_str() {
            Some("expire") => {
                if let Some(s) = session {
                    st.lock().unwrap().sessions.remove(s);
                }
                json!({"content": [{"type": "text", "text": "expired"}]})
            }
            _ => json!({"content": [{"type": "text", "text": params["arguments"]["text"]}]}),
        },
        _ => json!({}),
    }
}

fn initialize_result() -> Value {
    json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
           "serverInfo": {"name": "http-test", "version": "0.1"}, "instructions": "HTTP server"})
}

fn sse_response(messages: Vec<Value>) -> Response {
    let mut text = String::from(": keep-alive\n\n");
    for m in messages {
        text.push_str(&format!("event: message\r\ndata: {m}\r\n\r\n"));
    }
    // Tiny chunks to exercise the incremental parser.
    let chunks: Vec<Result<String, Infallible>> =
        text.as_bytes().chunks(7).map(|c| Ok(String::from_utf8_lossy(c).into_owned())).collect();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(futures::stream::iter(chunks)))
        .unwrap()
}

async fn handle_mcp(st: St, headers: HeaderMap, body: Bytes, sse: bool) -> Response {
    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(accept.contains("application/json") && accept.contains("text/event-stream"), "accept: {accept}");
    let msg: Value = serde_json::from_slice(&body).unwrap();
    let Some(method) = msg["method"].as_str().map(str::to_string) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let id = msg.get("id").cloned();
    if method == "initialize" {
        let sid = {
            let mut g = st.lock().unwrap();
            g.next_session += 1;
            g.init_count += 1;
            let sid = format!("sess-{}", g.next_session);
            g.sessions.insert(sid.clone());
            sid
        };
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": initialize_result()});
        let mut resp = if sse { sse_response(vec![reply]) } else { Json(reply).into_response() };
        resp.headers_mut().insert("mcp-session-id", sid.parse().unwrap());
        return resp;
    }
    let session = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_string);
    match &session {
        None => return (StatusCode::BAD_REQUEST, "missing session").into_response(),
        Some(s) if !st.lock().unwrap().sessions.contains(s) => return StatusCode::NOT_FOUND.into_response(),
        _ => {}
    }
    if let Some(v) = headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok()) {
        st.lock().unwrap().protocol_headers.push(v.to_string());
    }
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let result = result_for(&st, &method, &params, session.as_deref());
    let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
    if sse {
        let mut messages = vec![
            json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "from sse"}}),
        ];
        if let Some(token) = params.get("_meta").and_then(|m| m.get("progressToken")) {
            messages.push(json!({"jsonrpc": "2.0", "method": "notifications/progress",
                                 "params": {"progressToken": token, "progress": 1, "total": 1}}));
        }
        messages.push(reply);
        sse_response(messages)
    } else {
        Json(reply).into_response()
    }
}

async fn mcp_json(State(st): State<St>, headers: HeaderMap, body: Bytes) -> Response {
    handle_mcp(st, headers, body, false).await
}

async fn mcp_sse(State(st): State<St>, headers: HeaderMap, body: Bytes) -> Response {
    handle_mcp(st, headers, body, true).await
}

async fn mcp_delete(State(st): State<St>, headers: HeaderMap) -> StatusCode {
    if let Some(s) = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()) {
        st.lock().unwrap().deleted.push(s.to_string());
    }
    StatusCode::OK
}

async fn secure_mcp(State(st): State<St>, headers: HeaderMap, body: Bytes) -> Response {
    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("");
    if auth != format!("Bearer {VALID_TOKEN}") {
        let base = st.lock().unwrap().base.clone();
        let challenge = format!(
            r#"Bearer realm="test", resource_metadata="{base}/.well-known/oauth-protected-resource/secure/mcp""#
        );
        return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, challenge)], "unauthorized").into_response();
    }
    handle_mcp(st, headers, body, false).await
}

async fn protected_resource(State(st): State<St>) -> Json<Value> {
    let base = st.lock().unwrap().base.clone();
    Json(json!({"resource": format!("{base}/secure/mcp"), "authorization_servers": [format!("{base}/auth")],
                "scopes_supported": ["mcp"]}))
}

async fn auth_metadata(State(st): State<St>) -> Json<Value> {
    let base = st.lock().unwrap().base.clone();
    Json(json!({
        "issuer": format!("{base}/auth"),
        "authorization_endpoint": format!("{base}/auth/authorize"),
        "token_endpoint": format!("{base}/auth/token"),
        "registration_endpoint": format!("{base}/auth/register"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"]
    }))
}

async fn register(State(st): State<St>, Json(body): Json<Value>) -> Response {
    assert_eq!(body["token_endpoint_auth_method"], "none");
    let redirect = body["redirect_uris"][0].as_str().unwrap().to_string();
    assert!(redirect.starts_with("http://127.0.0.1:") && redirect.ends_with("/callback"), "{redirect}");
    st.lock().unwrap().registered_redirects.push(redirect.clone());
    (StatusCode::CREATED, Json(json!({"client_id": "client-123", "redirect_uris": [redirect]}))).into_response()
}

async fn authorize(State(st): State<St>, Query(q): Query<HashMap<String, String>>) -> Response {
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "client-123");
    assert_eq!(q["code_challenge_method"], "S256");
    let redirect = q["redirect_uri"].clone();
    {
        let mut g = st.lock().unwrap();
        assert!(g.registered_redirects.contains(&redirect));
        g.codes.insert("code-xyz".into(), (q["code_challenge"].clone(), redirect.clone()));
        g.authorize_params = q.clone();
    }
    let location = format!("{redirect}?code=code-xyz&state={}", q["state"]);
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

async fn token(State(st): State<St>, Form(f): Form<HashMap<String, String>>) -> Response {
    let base = st.lock().unwrap().base.clone();
    assert_eq!(f["client_id"], "client-123");
    assert_eq!(f.get("resource").map(String::as_str), Some(format!("{base}/secure/mcp").as_str()));
    match f["grant_type"].as_str() {
        "authorization_code" => {
            let Some((challenge, redirect)) = st.lock().unwrap().codes.remove(&f["code"]) else {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant"}))).into_response();
            };
            assert_eq!(f["redirect_uri"], redirect);
            let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(f["code_verifier"].as_bytes()));
            if computed != challenge {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant", "error_description": "pkce"})))
                    .into_response();
            }
            Json(json!({"access_token": VALID_TOKEN, "token_type": "Bearer", "expires_in": 3600,
                        "refresh_token": "refresh-1"}))
            .into_response()
        }
        "refresh_token" if f["refresh_token"] == "refresh-1" => {
            st.lock().unwrap().refreshes += 1;
            Json(json!({"access_token": VALID_TOKEN, "token_type": "Bearer", "expires_in": 3600})).into_response()
        }
        _ => (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid_grant"}))).into_response(),
    }
}

async fn legacy_get(State(st): State<St>) -> Response {
    let (tx, rx) = unbounded_channel::<String>();
    tx.send("event: endpoint\ndata: /legacy/messages?session=abc\n\n".into()).unwrap();
    st.lock().unwrap().legacy_tx = Some(tx);
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (Ok::<String, Infallible>(chunk), rx))
    });
    Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from_stream(stream)).unwrap()
}

async fn legacy_post(State(st): State<St>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> StatusCode {
    assert_eq!(q.get("session").map(String::as_str), Some("abc"));
    let msg: Value = serde_json::from_slice(&body).unwrap();
    if let (Some(method), Some(id)) = (msg["method"].as_str(), msg.get("id")) {
        let result = if method == "initialize" {
            initialize_result()
        } else {
            result_for(&st, method, msg.get("params").unwrap_or(&json!({})), None)
        };
        let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
        if let Some(tx) = &st.lock().unwrap().legacy_tx {
            let _ = tx.send(format!("event: message\ndata: {reply}\n\n"));
        }
    }
    StatusCode::ACCEPTED
}

async fn spawn_server() -> (String, St) {
    let st: St = Arc::new(Mutex::new(Inner::default()));
    let app = Router::new()
        .route("/mcp", post(mcp_json).delete(mcp_delete).get(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .route("/sse", post(mcp_sse).get(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .route("/secure/mcp", post(secure_mcp).get(|| async { StatusCode::UNAUTHORIZED }))
        .route("/.well-known/oauth-protected-resource/secure/mcp", get(protected_resource))
        .route("/.well-known/oauth-authorization-server/auth", get(auth_metadata))
        .route("/auth/register", post(register))
        .route("/auth/authorize", get(authorize))
        .route("/auth/token", post(token))
        .route("/legacy", get(legacy_get).post(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .route("/legacy/messages", post(legacy_post))
        .with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    st.lock().unwrap().base = base.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, st)
}

// ---------------------------------------------------------------------------
// Helpers

fn http(url: String) -> McpServerToml {
    McpServerToml { url: Some(url), startup_timeout_ms: Some(10_000), ..Default::default() }
}

fn manager() -> (McpManager, UnboundedReceiver<McpEvent>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = unbounded_channel();
    (McpManager::new(dir.path().join("tokens.json"), tx), rx, dir)
}

fn one(name: &str, cfg: McpServerToml) -> BTreeMap<String, McpServerToml> {
    BTreeMap::from([(name.to_string(), cfg)])
}

fn state(m: &McpManager, name: &str) -> (McpServerState, Option<String>) {
    let s = m.statuses().into_iter().find(|s| s.name == name).expect("server status");
    (s.state, s.error)
}

async fn wait_for<T>(rx: &mut UnboundedReceiver<McpEvent>, mut f: impl FnMut(McpEvent) -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let ev = rx.recv().await.expect("event channel closed");
            if let Some(t) = f(ev) {
                return t;
            }
        }
    })
    .await
    .expect("timed out waiting for an event")
}

fn read_tokens(dir: &tempfile::TempDir) -> Value {
    serde_json::from_slice(&std::fs::read(dir.path().join("tokens.json")).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------
// Tests

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamable_http_json_with_session() {
    let (base, st) = spawn_server().await;
    let (m, _rx, _dir) = manager();
    m.set_servers(one("j", http(format!("{base}/mcp")))).await;
    let (s, err) = state(&m, "j");
    assert_eq!(s, McpServerState::Ready, "{err:?}");
    let status = m.statuses().remove(0);
    assert_eq!(status.transport, "http");
    assert_eq!(status.server_name.as_deref(), Some("http-test"));
    assert_eq!(m.instructions(), vec![("j".to_string(), "HTTP server".to_string())]);
    let names: Vec<String> = m.tools().into_iter().map(|t| t.qualified_name).collect();
    assert_eq!(names, vec!["mcp__j__echo", "mcp__j__expire"]);

    let r = m.call_tool("mcp__j__echo", json!({"text": "over http"})).await.unwrap();
    assert_eq!(r.to_text(), "over http");
    {
        let g = st.lock().unwrap();
        assert_eq!(g.init_count, 1);
        assert!(!g.protocol_headers.is_empty());
        assert!(g.protocol_headers.iter().all(|v| v == "2025-06-18"));
    }

    // Session expiry → 404 → transparent re-initialize.
    m.call_tool("mcp__j__expire", json!({})).await.unwrap();
    let r = m.call_tool("mcp__j__echo", json!({"text": "again"})).await.unwrap();
    assert_eq!(r.to_text(), "again");
    assert_eq!(st.lock().unwrap().init_count, 2);

    m.shutdown().await;
    assert_eq!(st.lock().unwrap().deleted, vec!["sess-2".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streamable_http_sse_responses() {
    let (base, _st) = spawn_server().await;
    let (m, mut rx, _dir) = manager();
    m.set_servers(one("s", http(format!("{base}/sse")))).await;
    assert_eq!(state(&m, "s").0, McpServerState::Ready, "{:?}", state(&m, "s").1);
    assert_eq!(m.tools().len(), 2);
    let r = m.call_tool("mcp__s__echo", json!({"text": "streamed"})).await.unwrap();
    assert_eq!(r.to_text(), "streamed");
    wait_for(&mut rx, |ev| match ev {
        McpEvent::Progress { server, progress, total, .. } if server == "s" => {
            assert_eq!((progress, total), (1.0, Some(1.0)));
            Some(())
        }
        _ => None,
    })
    .await;
    assert!(m.logs("s").iter().any(|l| l == "[info] from sse"), "{:?}", m.logs("s"));
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn legacy_sse_fallback() {
    let (base, _st) = spawn_server().await;
    let (m, _rx, _dir) = manager();
    m.set_servers(one("old", http(format!("{base}/legacy")))).await;
    let (s, err) = state(&m, "old");
    assert_eq!(s, McpServerState::Ready, "{err:?} {:?}", m.logs("old"));
    let r = m.call_tool("mcp__old__echo", json!({"text": "legacy"})).await.unwrap();
    assert_eq!(r.to_text(), "legacy");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unauthorized_server_needs_auth_and_bad_bearer_fails() {
    let (base, _st) = spawn_server().await;
    let (m, _rx, _dir) = manager();
    m.set_servers(BTreeMap::from([
        ("secure".to_string(), http(format!("{base}/secure/mcp"))),
        (
            "bearer".to_string(),
            McpServerToml { bearer_token: Some("wrong".into()), ..http(format!("{base}/secure/mcp")) },
        ),
        (
            "envvar".to_string(),
            McpServerToml {
                bearer_token_env_var: Some("ODEX_MCP_TEST_UNSET_VAR".into()),
                ..http(format!("{base}/secure/mcp"))
            },
        ),
        ("forced".to_string(), McpServerToml { oauth: Some(true), ..http(format!("{base}/mcp")) }),
    ]))
    .await;
    let (s, err) = state(&m, "secure");
    assert_eq!(s, McpServerState::NeedsAuth);
    assert!(err.unwrap().contains("authentication"));
    let (s, err) = state(&m, "bearer");
    assert_eq!(s, McpServerState::Failed);
    assert!(err.unwrap().contains("bearer token"));
    let (s, err) = state(&m, "envvar");
    assert_eq!(s, McpServerState::Failed);
    assert!(err.unwrap().contains("ODEX_MCP_TEST_UNSET_VAR"));
    assert_eq!(state(&m, "forced").0, McpServerState::NeedsAuth);
    let e = m.call_tool("mcp__secure__echo", json!({})).await.unwrap_err();
    assert!(matches!(e.downcast_ref::<McpError>(), Some(McpError::UnknownTool(_))));
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn static_bearer_token_works() {
    let (base, _st) = spawn_server().await;
    let (m, _rx, _dir) = manager();
    let cfg = McpServerToml { bearer_token: Some(VALID_TOKEN.into()), ..http(format!("{base}/secure/mcp")) };
    m.set_servers(one("b", cfg)).await;
    assert_eq!(state(&m, "b").0, McpServerState::Ready);
    assert_eq!(m.call_tool("mcp__b__echo", json!({"text": "auth"})).await.unwrap().to_text(), "auth");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oauth_login_end_to_end() {
    let (base, st) = spawn_server().await;
    let (m, mut rx, dir) = manager();
    m.set_servers(one("secure", http(format!("{base}/secure/mcp")))).await;
    assert_eq!(state(&m, "secure").0, McpServerState::NeedsAuth);
    assert!(!m.statuses()[0].authenticated);

    let auth_url = m.login("secure").await.unwrap();
    assert!(auth_url.starts_with(&format!("{base}/auth/authorize?")), "{auth_url}");

    // The "browser": follow the authorize redirect to the loopback callback.
    let page = reqwest::get(&auth_url).await.unwrap().text().await.unwrap();
    assert!(page.contains("Odex is connected"), "{page}");
    {
        let g = st.lock().unwrap();
        assert_eq!(g.authorize_params["resource"], format!("{base}/secure/mcp"));
        assert_eq!(g.authorize_params["scope"], "mcp");
        assert_eq!(g.authorize_params["code_challenge"].len(), 43);
    }

    wait_for(&mut rx, |ev| match ev {
        McpEvent::Status(s) if s.name == "secure" && s.state == McpServerState::Ready => Some(()),
        _ => None,
    })
    .await;
    let status = m.statuses().remove(0);
    assert!(status.authenticated);
    assert_eq!(m.call_tool("mcp__secure__echo", json!({"text": "authed"})).await.unwrap().to_text(), "authed");

    let tokens = read_tokens(&dir);
    let t = &tokens["secure"];
    assert_eq!(t["access_token"], VALID_TOKEN);
    assert_eq!(t["refresh_token"], "refresh-1");
    assert_eq!(t["client_id"], "client-123");
    assert_eq!(t["token_endpoint"], format!("{base}/auth/token"));
    assert!(t["expires_at"].as_u64().unwrap() > 1_700_000_000);

    // logout → tokens gone → NeedsAuth again
    m.logout("secure").await.unwrap();
    assert_eq!(state(&m, "secure").0, McpServerState::NeedsAuth);
    assert!(read_tokens(&dir).get("secure").is_none());
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oauth_refreshes_expired_and_rejected_tokens() {
    let (base, st) = spawn_server().await;
    let (m, _rx, dir) = manager();
    let stored = |expires_at: Value| {
        json!({"secure": {
            "access_token": "stale", "refresh_token": "refresh-1", "expires_at": expires_at,
            "client_id": "client-123", "token_endpoint": format!("{base}/auth/token"),
            "server_url": format!("{base}/secure/mcp"), "resource": format!("{base}/secure/mcp")
        }})
    };
    // 1) expired → refreshed before the first request
    std::fs::write(dir.path().join("tokens.json"), stored(json!(1)).to_string()).unwrap();
    m.set_servers(one("secure", http(format!("{base}/secure/mcp")))).await;
    assert_eq!(state(&m, "secure").0, McpServerState::Ready, "{:?}", m.logs("secure"));
    assert_eq!(st.lock().unwrap().refreshes, 1);
    let t = read_tokens(&dir);
    assert_eq!(t["secure"]["access_token"], VALID_TOKEN);
    assert_eq!(t["secure"]["refresh_token"], "refresh-1");
    m.shutdown().await;

    // 2) not expired but rejected with 401 → refresh + retry
    std::fs::write(dir.path().join("tokens.json"), stored(Value::Null).to_string()).unwrap();
    let (tx, _rx2) = unbounded_channel();
    let m = McpManager::new(dir.path().join("tokens.json"), tx);
    m.set_servers(one("secure", http(format!("{base}/secure/mcp")))).await;
    assert_eq!(state(&m, "secure").0, McpServerState::Ready, "{:?}", m.logs("secure"));
    assert_eq!(st.lock().unwrap().refreshes, 2);

    // 3) a token for another URL is never sent
    let (tx, _rx3) = unbounded_channel();
    let m3 = McpManager::new(dir.path().join("tokens.json"), tx);
    let mut other = read_tokens(&dir);
    other["secure"]["server_url"] = json!("https://elsewhere.example/mcp");
    std::fs::write(dir.path().join("tokens.json"), other.to_string()).unwrap();
    m3.set_servers(one("secure", http(format!("{base}/secure/mcp")))).await;
    assert_eq!(state(&m3, "secure").0, McpServerState::NeedsAuth);
    m.shutdown().await;
    m3.shutdown().await;
}
