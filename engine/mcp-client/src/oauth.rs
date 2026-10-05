//! OAuth 2.1 for HTTP MCP servers.
//!
//! Flow: probe the server for a `WWW-Authenticate` challenge → protected
//! resource metadata (RFC 9728) → authorization server metadata (RFC 8414,
//! OpenID discovery fallback) → dynamic client registration (RFC 7591) →
//! authorization code + PKCE S256 with a loopback redirect
//! (`http://127.0.0.1:<port>/callback`) → token exchange → persisted tokens,
//! refreshed on expiry or 401.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use reqwest::header::{HeaderMap, ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::error::McpError;
use crate::logbuf::LogSink;
use crate::transport::truncate_body;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Refresh this long before the access token actually expires.
const EXPIRY_SKEW_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// PKCE / state

/// PKCE verifier + S256 challenge (RFC 7636).
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// 32 random bytes → 43-char base64url verifier, plus its S256 challenge.
pub fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    Pkce { challenge: pkce_challenge(&verifier), verifier }
}

/// `BASE64URL(SHA256(verifier))`.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn random_token(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Discovery

/// Parse the auth-params of a `WWW-Authenticate` header (keys lowercased).
/// Lenient: params of every challenge are collected.
pub fn parse_www_authenticate(header: &str) -> HashMap<String, String> {
    let chars: Vec<char> = header.chars().collect();
    let is_tchar = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    let mut out = HashMap::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && is_tchar(chars[i]) {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i < chars.len() && chars[i] == '=' && !token.is_empty() {
            i += 1;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            let mut value = String::new();
            if i < chars.len() && chars[i] == '"' {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    value.push(chars[i]);
                    i += 1;
                }
                i += 1; // closing quote
            } else {
                while i < chars.len() && chars[i] != ',' && !chars[i].is_whitespace() {
                    value.push(chars[i]);
                    i += 1;
                }
            }
            out.insert(token.to_ascii_lowercase(), value);
        } else if token.is_empty() {
            i += 1; // unexpected character
        }
    }
    out
}

/// RFC 9728 protected resource metadata.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProtectedResourceMetadata {
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
}

/// RFC 8414 authorization server metadata (the fields we use).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AuthServerMetadata {
    #[serde(default)]
    pub issuer: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
}

fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Candidate RFC 9728 metadata URLs for an MCP server URL (path-aware first).
pub fn protected_resource_metadata_urls(server: &Url) -> Vec<Url> {
    let o = origin(server);
    let path = server.path().trim_end_matches('/');
    let mut out = Vec::new();
    if !path.is_empty() {
        out.extend(Url::parse(&format!("{o}/.well-known/oauth-protected-resource{path}")).ok());
    }
    out.extend(Url::parse(&format!("{o}/.well-known/oauth-protected-resource")).ok());
    out
}

/// Candidate metadata URLs for an issuer: RFC 8414, then OpenID discovery.
pub fn authorization_server_metadata_urls(issuer: &Url) -> Vec<Url> {
    let o = origin(issuer);
    let path = issuer.path().trim_end_matches('/');
    let candidates = if path.is_empty() {
        vec![format!("{o}/.well-known/oauth-authorization-server"), format!("{o}/.well-known/openid-configuration")]
    } else {
        vec![
            format!("{o}/.well-known/oauth-authorization-server{path}"),
            format!("{o}/.well-known/openid-configuration{path}"),
            format!("{o}{path}/.well-known/openid-configuration"),
            format!("{o}/.well-known/oauth-authorization-server"),
        ]
    };
    candidates.iter().filter_map(|c| Url::parse(c).ok()).collect()
}

/// Canonical resource indicator (RFC 8707) for an MCP server URL.
pub fn canonical_resource(server: &Url) -> String {
    let mut u = server.clone();
    u.set_fragment(None);
    let s = u.to_string();
    if u.path() == "/" && u.query().is_none() {
        s.trim_end_matches('/').to_string()
    } else {
        s
    }
}

/// Result of discovery.
#[derive(Debug, Clone)]
pub struct Discovery {
    pub metadata: AuthServerMetadata,
    pub resource: String,
    pub scope: Option<String>,
}

async fn fetch_json<T: serde::de::DeserializeOwned>(http: &reqwest::Client, url: Url) -> Option<T> {
    let resp = http.get(url).header(ACCEPT, "application/json").timeout(HTTP_TIMEOUT).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<T>().await.ok()
}

/// Send an unauthenticated `initialize` to read the server's `WWW-Authenticate` challenge.
pub async fn probe_www_authenticate(http: &reqwest::Client, server: &Url, headers: &HeaderMap) -> Option<String> {
    let body = json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {"protocolVersion": crate::MCP_PROTOCOL_VERSION, "capabilities": {},
                   "clientInfo": {"name": crate::CLIENT_NAME, "version": env!("CARGO_PKG_VERSION")}}
    });
    let resp = http
        .post(server.clone())
        .headers(headers.clone())
        .header(ACCEPT, "application/json, text/event-stream")
        .json(&body)
        .timeout(HTTP_TIMEOUT)
        .send()
        .await
        .ok()?;
    if resp.status().as_u16() != 401 {
        return None;
    }
    resp.headers().get(WWW_AUTHENTICATE).and_then(|v| v.to_str().ok()).map(str::to_string)
}

/// Discover the authorization server for `server`.
pub async fn discover(
    http: &reqwest::Client,
    server: &Url,
    www_authenticate: Option<&str>,
) -> Result<Discovery, McpError> {
    let params = www_authenticate.map(parse_www_authenticate).unwrap_or_default();
    let mut prm_urls: Vec<Url> = Vec::new();
    if let Some(hint) = params.get("resource_metadata") {
        prm_urls.extend(server.join(hint).ok());
    }
    for u in protected_resource_metadata_urls(server) {
        if !prm_urls.contains(&u) {
            prm_urls.push(u);
        }
    }
    let mut prm: Option<ProtectedResourceMetadata> = None;
    for u in prm_urls {
        if let Some(m) = fetch_json::<ProtectedResourceMetadata>(http, u).await {
            if !m.authorization_servers.is_empty() {
                prm = Some(m);
                break;
            }
        }
    }
    let issuer = prm
        .as_ref()
        .and_then(|m| m.authorization_servers.first())
        .and_then(|s| Url::parse(s).ok())
        .unwrap_or_else(|| Url::parse(&origin(server)).unwrap_or_else(|_| server.clone()));

    let mut metadata = None;
    for u in authorization_server_metadata_urls(&issuer) {
        if let Some(m) = fetch_json::<AuthServerMetadata>(http, u).await {
            metadata = Some(m);
            break;
        }
    }
    // 2025-03-26 fallback: default endpoints at the issuer's origin.
    let metadata = metadata.unwrap_or_else(|| {
        let o = origin(&issuer);
        AuthServerMetadata {
            issuer: Some(o.clone()),
            authorization_endpoint: format!("{o}/authorize"),
            token_endpoint: format!("{o}/token"),
            registration_endpoint: Some(format!("{o}/register")),
            scopes_supported: None,
            code_challenge_methods_supported: None,
        }
    });
    if let Some(methods) = &metadata.code_challenge_methods_supported {
        if !methods.iter().any(|m| m == "S256") {
            return Err(McpError::OAuth("the authorization server does not support PKCE S256".into()));
        }
    }
    let scope = params.get("scope").filter(|s| !s.is_empty()).cloned().or_else(|| {
        prm.as_ref().and_then(|m| m.scopes_supported.as_ref()).filter(|s| !s.is_empty()).map(|s| s.join(" "))
    });
    let resource = prm.and_then(|m| m.resource).unwrap_or_else(|| canonical_resource(server));
    Ok(Discovery { metadata, resource, scope })
}

// ---------------------------------------------------------------------------
// Token storage

/// Tokens persisted per server name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint_auth_method: Option<String>,
    /// MCP server URL the token was issued for (not sent anywhere else).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Loopback port the client was registered with (reused when free).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_port: Option<u16>,
}

impl StoredToken {
    pub fn is_expired(&self, skew_secs: u64) -> bool {
        self.expires_at.is_some_and(|e| now_secs() + skew_secs >= e)
    }

    /// Whether this token may be sent to `url`.
    pub fn matches_server(&self, url: &str) -> bool {
        match &self.server_url {
            Some(s) => s.trim_end_matches('/') == url.trim_end_matches('/'),
            None => true,
        }
    }

    fn apply_token_response(&mut self, v: &Value) -> Result<(), McpError> {
        let access = v
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::OAuth("token response has no access_token".into()))?;
        self.access_token = access.to_string();
        if let Some(r) = v.get("refresh_token").and_then(Value::as_str) {
            self.refresh_token = Some(r.to_string());
        }
        let expires_in = match v.get("expires_in") {
            Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f as u64)),
            Some(Value::String(s)) => s.parse().ok(),
            _ => None,
        };
        self.expires_at = expires_in.map(|e| now_secs() + e);
        if let Some(s) = v.get("scope").and_then(Value::as_str) {
            self.scope = Some(s.to_string());
        }
        Ok(())
    }
}

/// JSON file `{ "<server>": StoredToken, ... }`.
pub struct TokenStore {
    path: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl TokenStore {
    pub fn new(path: PathBuf) -> Self {
        TokenStore { path, lock: tokio::sync::Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn read_unlocked(&self) -> BTreeMap<String, StoredToken> {
        match tokio::fs::read(&self.path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("ignoring unreadable MCP token store {}: {e}", self.path.display());
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        }
    }

    async fn write_unlocked(&self, map: &BTreeMap<String, StoredToken>) -> Result<(), McpError> {
        let io = |e: std::io::Error| McpError::Other(format!("writing {}: {e}", self.path.display()));
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            tokio::fs::create_dir_all(parent).await.map_err(io)?;
        }
        let data = serde_json::to_vec_pretty(map).map_err(|e| McpError::Other(e.to_string()))?;
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        tokio::fs::write(&tmp, &data).await.map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        tokio::fs::rename(&tmp, &self.path).await.map_err(io)?;
        Ok(())
    }

    pub async fn load(&self) -> BTreeMap<String, StoredToken> {
        let _g = self.lock.lock().await;
        self.read_unlocked().await
    }

    pub async fn get(&self, server: &str) -> Option<StoredToken> {
        self.load().await.remove(server)
    }

    pub async fn put(&self, server: &str, token: StoredToken) -> Result<(), McpError> {
        let _g = self.lock.lock().await;
        let mut map = self.read_unlocked().await;
        map.insert(server.to_string(), token);
        self.write_unlocked(&map).await
    }

    /// Returns whether a token was removed.
    pub async fn remove(&self, server: &str) -> Result<bool, McpError> {
        let _g = self.lock.lock().await;
        let mut map = self.read_unlocked().await;
        if map.remove(server).is_none() {
            return Ok(false);
        }
        self.write_unlocked(&map).await?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Token endpoint

#[derive(Debug, Clone)]
struct ClientCreds {
    id: String,
    secret: Option<String>,
    auth_method: Option<String>,
}

impl ClientCreds {
    fn from_token(t: &StoredToken) -> Self {
        ClientCreds {
            id: t.client_id.clone(),
            secret: t.client_secret.clone(),
            auth_method: t.token_endpoint_auth_method.clone(),
        }
    }
}

async fn token_request(
    http: &reqwest::Client,
    endpoint: &str,
    mut form: Vec<(&str, String)>,
    client: &ClientCreds,
) -> Result<Value, McpError> {
    let mut req = http.post(endpoint).header(ACCEPT, "application/json").timeout(HTTP_TIMEOUT);
    match (&client.secret, client.auth_method.as_deref()) {
        (Some(secret), Some("client_secret_basic")) => req = req.basic_auth(&client.id, Some(secret)),
        (Some(secret), _) => {
            form.push(("client_id", client.id.clone()));
            form.push(("client_secret", secret.clone()));
        }
        (None, _) => form.push(("client_id", client.id.clone())),
    }
    let resp = req.form(&form).send().await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .map(|v| {
                let e = v.get("error").and_then(Value::as_str).unwrap_or("error");
                match v.get("error_description").and_then(Value::as_str) {
                    Some(d) => format!("{e}: {d}"),
                    None => e.to_string(),
                }
            })
            .unwrap_or_else(|| truncate_body(&text, 300));
        return Err(McpError::OAuth(format!("token endpoint returned HTTP {}: {detail}", status.as_u16())));
    }
    serde_json::from_str(&text).map_err(|e| McpError::OAuth(format!("invalid token response: {e}")))
}

/// Exchange a refresh token. Keeps the old refresh token if none is returned.
pub(crate) async fn refresh_access_token(http: &reqwest::Client, tok: &StoredToken) -> Result<StoredToken, McpError> {
    let refresh = tok.refresh_token.clone().ok_or_else(|| McpError::OAuth("no refresh token".into()))?;
    let mut form = vec![("grant_type", "refresh_token".to_string()), ("refresh_token", refresh)];
    if let Some(r) = &tok.resource {
        form.push(("resource", r.clone()));
    }
    let v = token_request(http, &tok.token_endpoint, form, &ClientCreds::from_token(tok)).await?;
    let mut new = tok.clone();
    new.apply_token_response(&v)?;
    Ok(new)
}

/// Bearer token provider used by the HTTP transports.
pub(crate) struct TokenSource {
    server: String,
    store: Arc<TokenStore>,
    http: reqwest::Client,
    log: Arc<LogSink>,
    token: tokio::sync::Mutex<StoredToken>,
}

impl TokenSource {
    pub(crate) fn new(
        server: &str,
        token: StoredToken,
        store: Arc<TokenStore>,
        http: reqwest::Client,
        log: Arc<LogSink>,
    ) -> Self {
        TokenSource { server: server.to_string(), store, http, log, token: tokio::sync::Mutex::new(token) }
    }

    async fn refresh_locked(&self, tok: &mut StoredToken) -> bool {
        match refresh_access_token(&self.http, tok).await {
            Ok(new) => {
                *tok = new;
                if let Err(e) = self.store.put(&self.server, tok.clone()).await {
                    self.log.error(format!("saving refreshed token failed: {e}"));
                }
                self.log.info("OAuth access token refreshed");
                true
            }
            Err(e) => {
                self.log.error(format!("OAuth token refresh failed: {e}"));
                false
            }
        }
    }

    /// Current access token, refreshed first when it is (about to be) expired.
    pub(crate) async fn access_token(&self) -> String {
        let mut tok = self.token.lock().await;
        if tok.is_expired(EXPIRY_SKEW_SECS) && tok.refresh_token.is_some() {
            self.refresh_locked(&mut tok).await;
        }
        tok.access_token.clone()
    }

    /// The server rejected `used`; refresh. Returns whether a retry makes sense.
    pub(crate) async fn refresh_after_unauthorized(&self, used: &str) -> bool {
        let mut tok = self.token.lock().await;
        if tok.access_token != used {
            return true; // another request refreshed meanwhile
        }
        if tok.refresh_token.is_none() {
            return false;
        }
        self.refresh_locked(&mut tok).await
    }
}

// ---------------------------------------------------------------------------
// Login

async fn register_client(
    http: &reqwest::Client,
    endpoint: &str,
    redirect_uri: &str,
    scope: Option<&str>,
) -> Result<ClientCreds, McpError> {
    let mut body = json!({
        "client_name": "Odex",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(s) = scope {
        body["scope"] = json!(s);
    }
    let resp = http
        .post(endpoint)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .json(&body)
        .timeout(HTTP_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(McpError::OAuth(format!(
            "dynamic client registration failed: HTTP {}: {}",
            status.as_u16(),
            truncate_body(&text, 300)
        )));
    }
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| McpError::OAuth(format!("invalid client registration response: {e}")))?;
    let id = v
        .get("client_id")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::OAuth("client registration response has no client_id".into()))?;
    Ok(ClientCreds {
        id: id.to_string(),
        secret: v.get("client_secret").and_then(Value::as_str).map(str::to_string),
        auth_method: v.get("token_endpoint_auth_method").and_then(Value::as_str).map(str::to_string),
    })
}

/// A started login: the loopback listener is bound and the authorization URL built.
pub(crate) struct PendingLogin {
    listener: TcpListener,
    port: u16,
    state: String,
    verifier: String,
    redirect_uri: String,
    client: ClientCreds,
    token_endpoint: String,
    resource: String,
    scope: Option<String>,
    server_url: String,
    pub(crate) authorization_url: String,
}

pub(crate) async fn begin_login(
    http: &reqwest::Client,
    server: &Url,
    headers: &HeaderMap,
    previous: Option<StoredToken>,
) -> Result<PendingLogin, McpError> {
    let challenge = probe_www_authenticate(http, server, headers).await;
    let disc = discover(http, server, challenge.as_deref()).await?;

    // Reuse the previous port (and its registered client) when possible:
    // many servers match redirect URIs exactly, including the port.
    let mut listener = None;
    if let Some(p) = previous.as_ref().and_then(|t| t.redirect_port) {
        listener = TcpListener::bind(("127.0.0.1", p)).await.ok();
    }
    let reuse_client = listener.is_some()
        && previous
            .as_ref()
            .is_some_and(|t| !t.client_id.is_empty() && t.token_endpoint == disc.metadata.token_endpoint);
    let listener = match listener {
        Some(l) => l,
        None => TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| McpError::OAuth(format!("cannot bind the loopback redirect listener: {e}")))?,
    };
    let port = listener.local_addr().map_err(|e| McpError::OAuth(e.to_string()))?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let client = match (&previous, reuse_client, &disc.metadata.registration_endpoint) {
        (Some(prev), true, _) => ClientCreds::from_token(prev),
        (_, _, Some(reg)) => register_client(http, reg, &redirect_uri, disc.scope.as_deref()).await?,
        (Some(prev), false, None) if !prev.client_id.is_empty() => ClientCreds::from_token(prev),
        _ => {
            return Err(McpError::OAuth(
                "the authorization server has no registration endpoint and no client id is known".into(),
            ))
        }
    };

    let pkce = generate_pkce();
    let state = random_token(16);
    let mut auth = Url::parse(&disc.metadata.authorization_endpoint)
        .map_err(|e| McpError::OAuth(format!("invalid authorization endpoint: {e}")))?;
    {
        let mut q = auth.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &client.id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", &disc.resource);
        if let Some(s) = &disc.scope {
            q.append_pair("scope", s);
        }
    }
    Ok(PendingLogin {
        listener,
        port,
        state,
        verifier: pkce.verifier,
        redirect_uri,
        client,
        token_endpoint: disc.metadata.token_endpoint,
        resource: disc.resource,
        scope: disc.scope,
        server_url: server.to_string(),
        authorization_url: auth.to_string(),
    })
}

impl PendingLogin {
    /// Wait for the browser redirect, then exchange the code for tokens.
    pub(crate) async fn complete(self, http: &reqwest::Client, timeout: Duration) -> Result<StoredToken, McpError> {
        let code = wait_for_callback(&self.listener, &self.state, timeout).await?;
        drop(self.listener);
        let form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code),
            ("redirect_uri", self.redirect_uri.clone()),
            ("code_verifier", self.verifier.clone()),
            ("resource", self.resource.clone()),
        ];
        let v = token_request(http, &self.token_endpoint, form, &self.client).await?;
        let mut tok = StoredToken {
            client_id: self.client.id.clone(),
            client_secret: self.client.secret.clone(),
            token_endpoint: self.token_endpoint.clone(),
            token_endpoint_auth_method: self.client.auth_method.clone(),
            server_url: Some(self.server_url.clone()),
            resource: Some(self.resource.clone()),
            scope: self.scope.clone(),
            redirect_port: Some(self.port),
            ..Default::default()
        };
        tok.apply_token_response(&v)?;
        Ok(tok)
    }
}

fn html_page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto\">\
         <h2>{title}</h2><p>{body}</p></body></html>"
    )
}

async fn respond(sock: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(resp.as_bytes()).await;
    let _ = sock.shutdown().await;
}

/// Tiny HTTP server on the loopback listener: waits for
/// `GET /callback?code=..&state=..` with the expected state.
async fn wait_for_callback(listener: &TcpListener, state: &str, timeout: Duration) -> Result<String, McpError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let (mut sock, _) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| McpError::OAuth("timed out waiting for the browser to complete the login".into()))?
            .map_err(|e| McpError::OAuth(format!("loopback listener failed: {e}")))?;
        let mut buf = vec![0u8; 16 * 1024];
        let mut n = 0;
        while n < buf.len() {
            match tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf[n..])).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                Ok(Ok(k)) => {
                    n += k;
                    if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
            }
        }
        let head = String::from_utf8_lossy(&buf[..n]).into_owned();
        let target = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/").to_string();
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            respond(&mut sock, "400 Bad Request", &html_page("Bad request", "")).await;
            continue;
        };
        if url.path() != "/callback" {
            respond(&mut sock, "404 Not Found", &html_page("Not found", "")).await;
            continue;
        }
        let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if params.get("state").map(String::as_str) != Some(state) {
            respond(&mut sock, "400 Bad Request", &html_page("Login failed", "Invalid state parameter.")).await;
            continue;
        }
        if let Some(err) = params.get("error") {
            let desc = params.get("error_description").cloned().unwrap_or_default();
            respond(&mut sock, "200 OK", &html_page("Login failed", "You can close this window.")).await;
            return Err(McpError::OAuth(format!("authorization denied: {err} {desc}").trim().to_string()));
        }
        let Some(code) = params.get("code").cloned() else {
            respond(&mut sock, "400 Bad Request", &html_page("Login failed", "Missing authorization code.")).await;
            continue;
        };
        respond(&mut sock, "200 OK", &html_page("Odex is connected", "You can close this window and return to Odex."))
            .await;
        return Ok(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_rfc7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let p = generate_pkce();
        assert_eq!(p.verifier.len(), 43);
        assert!(p.verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(p.challenge, pkce_challenge(&p.verifier));
        assert_ne!(generate_pkce().verifier, p.verifier);
    }

    #[test]
    fn parses_www_authenticate() {
        let h = r#"Bearer realm="mcp", resource_metadata="https://api.example.com/.well-known/oauth-protected-resource/mcp", scope="read write", error=invalid_token"#;
        let p = parse_www_authenticate(h);
        assert_eq!(p["realm"], "mcp");
        assert_eq!(p["resource_metadata"], "https://api.example.com/.well-known/oauth-protected-resource/mcp");
        assert_eq!(p["scope"], "read write");
        assert_eq!(p["error"], "invalid_token");
        let p = parse_www_authenticate(r#"Bearer error_description="say \"hi\"""#);
        assert_eq!(p["error_description"], "say \"hi\"");
        assert!(parse_www_authenticate("Bearer").is_empty());
    }

    #[test]
    fn metadata_urls() {
        let u = Url::parse("https://mcp.example.com/v1/mcp").unwrap();
        let prm: Vec<String> = protected_resource_metadata_urls(&u).iter().map(Url::to_string).collect();
        assert_eq!(
            prm,
            vec![
                "https://mcp.example.com/.well-known/oauth-protected-resource/v1/mcp",
                "https://mcp.example.com/.well-known/oauth-protected-resource",
            ]
        );
        let root = Url::parse("https://auth.example.com").unwrap();
        let asm: Vec<String> = authorization_server_metadata_urls(&root).iter().map(Url::to_string).collect();
        assert_eq!(
            asm,
            vec![
                "https://auth.example.com/.well-known/oauth-authorization-server",
                "https://auth.example.com/.well-known/openid-configuration",
            ]
        );
        let tenant = Url::parse("https://auth.example.com/tenant1/").unwrap();
        let asm: Vec<String> = authorization_server_metadata_urls(&tenant).iter().map(Url::to_string).collect();
        assert_eq!(
            asm,
            vec![
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration",
                "https://auth.example.com/.well-known/oauth-authorization-server",
            ]
        );
        assert_eq!(canonical_resource(&Url::parse("https://Mcp.Example.com/#x").unwrap()), "https://mcp.example.com");
        assert_eq!(canonical_resource(&u), "https://mcp.example.com/v1/mcp");
    }

    #[test]
    fn parses_metadata_documents() {
        let prm: ProtectedResourceMetadata = serde_json::from_value(json!({
            "resource": "https://mcp.example.com/mcp",
            "authorization_servers": ["https://auth.example.com"],
            "scopes_supported": ["mcp:read"],
            "bearer_methods_supported": ["header"]
        }))
        .unwrap();
        assert_eq!(prm.authorization_servers, vec!["https://auth.example.com"]);
        let asm: AuthServerMetadata = serde_json::from_value(json!({
            "issuer": "https://auth.example.com",
            "authorization_endpoint": "https://auth.example.com/authorize",
            "token_endpoint": "https://auth.example.com/token",
            "registration_endpoint": "https://auth.example.com/register",
            "code_challenge_methods_supported": ["S256"],
            "response_types_supported": ["code"]
        }))
        .unwrap();
        assert_eq!(asm.registration_endpoint.as_deref(), Some("https://auth.example.com/register"));
    }

    #[test]
    fn token_response_and_expiry() {
        let mut t = StoredToken { refresh_token: Some("old".into()), ..Default::default() };
        t.apply_token_response(&json!({"access_token": "a", "expires_in": "120", "token_type": "Bearer"})).unwrap();
        assert_eq!(t.access_token, "a");
        assert_eq!(t.refresh_token.as_deref(), Some("old"));
        assert!(!t.is_expired(60));
        assert!(t.is_expired(600));
        assert!(t.apply_token_response(&json!({"error": "x"})).is_err());
        assert!(t.matches_server("https://x"));
        t.server_url = Some("https://x/mcp".into());
        assert!(t.matches_server("https://x/mcp/"));
        assert!(!t.matches_server("https://evil/mcp"));
    }

    #[tokio::test]
    async fn token_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = TokenStore::new(dir.path().join("nested").join("tokens.json"));
        assert!(store.get("a").await.is_none());
        let tok = StoredToken {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at: Some(123),
            client_id: "cid".into(),
            token_endpoint: "https://auth/token".into(),
            ..Default::default()
        };
        store.put("a", tok.clone()).await.unwrap();
        store.put("b", StoredToken { access_token: "b".into(), ..Default::default() }).await.unwrap();
        assert_eq!(store.get("a").await, Some(tok));
        let raw: Value = serde_json::from_slice(&std::fs::read(store.path()).unwrap()).unwrap();
        assert_eq!(raw["a"]["access_token"], "at");
        assert_eq!(raw["a"]["refresh_token"], "rt");
        assert_eq!(raw["a"]["expires_at"], 123);
        assert_eq!(raw["a"]["client_id"], "cid");
        assert_eq!(raw["a"]["token_endpoint"], "https://auth/token");
        assert!(store.remove("a").await.unwrap());
        assert!(!store.remove("a").await.unwrap());
        assert_eq!(store.load().await.len(), 1);
    }

    #[tokio::test]
    async fn loopback_callback_checks_state() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiter = tokio::spawn(async move { wait_for_callback(&listener, "s1", Duration::from_secs(10)).await });
        let get = |path: String| async move {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).await.unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).await.unwrap();
            out
        };
        assert!(get("/favicon.ico".into()).await.starts_with("HTTP/1.1 404"));
        assert!(get("/callback?code=c&state=wrong".into()).await.starts_with("HTTP/1.1 400"));
        assert!(get("/callback?code=the-code&state=s1".into()).await.starts_with("HTTP/1.1 200"));
        assert_eq!(waiter.await.unwrap().unwrap(), "the-code");
    }
}
