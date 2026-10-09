//! ComfyUI client for image and 3D generation.
//!
//! Workflows are API-format exports with `{{prompt}}`-style placeholders (see
//! [`workflow`]). A run queues the filled graph on `/prompt`, polls
//! `/history/<id>` until it finishes, then downloads the files its output
//! nodes saved (`SaveImage`, `SaveGLB`, ...) through `/view`.
//!
//! Servers behind an authenticating proxy get an API key on every request
//! ([`ComfyClient::with_auth`]).

pub mod ideogram;
pub mod recipes;
pub mod templates;
pub mod ui_format;
pub mod workflow;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION};
use reqwest::StatusCode;
use serde_json::{json, Value};

pub use workflow::{import_workflow, list_workflows, save_workflow, Inputs, Kind, Workflow, PLACEHOLDERS};

const POLL: Duration = Duration::from_millis(750);
/// Consecutive failed polls before giving up on an unreachable server.
const MAX_POLL_FAILURES: u32 = 12;

/// A file a workflow produced.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputFile {
    pub node: String,
    pub filename: String,
    pub subfolder: String,
    /// `output` for saved files, `temp` for previews.
    pub kind: String,
}

impl OutputFile {
    /// Lowercase extension, `bin` when there is none.
    pub fn extension(&self) -> String {
        Path::new(&self.filename)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "bin".into())
    }
}

pub struct ComfyClient {
    base: String,
    http: reqwest::Client,
    client_id: String,
    /// An API key goes out with every request.
    has_key: bool,
    /// Comfy.org key for partner nodes, sent with each run.
    comfy_org_key: Option<String>,
}

impl ComfyClient {
    pub fn new(url: &str) -> Self {
        Self::build(url, HeaderMap::new(), false)
    }

    /// A client that sends `api_key` with every request: as `Authorization: Bearer <key>`, or as-is in
    /// `header` when one is named (e.g. `X-API-Key`). `headers` are added to every request too.
    pub fn with_auth(
        url: &str,
        api_key: Option<&str>,
        header: Option<&str>,
        headers: &BTreeMap<String, String>,
    ) -> anyhow::Result<Self> {
        let mut map = HeaderMap::new();
        for (k, v) in headers {
            let name =
                HeaderName::from_bytes(k.trim().as_bytes()).with_context(|| format!("invalid header name `{k}`"))?;
            let value = HeaderValue::from_str(v.trim()).with_context(|| format!("invalid value for header `{k}`"))?;
            map.insert(name, value);
        }
        let key = api_key.map(str::trim).filter(|k| !k.is_empty());
        if let Some(key) = key {
            let (name, value) = match header.map(str::trim).filter(|h| !h.is_empty()) {
                Some(h) => (
                    HeaderName::from_bytes(h.as_bytes()).with_context(|| format!("invalid API key header `{h}`"))?,
                    key.to_string(),
                ),
                // a pasted "Bearer …" / "Basic …" value is sent as it is
                None if has_scheme(key) => (AUTHORIZATION, key.to_string()),
                None => (AUTHORIZATION, format!("Bearer {key}")),
            };
            let mut value =
                HeaderValue::from_str(&value).context("the API key has characters that can't go in an HTTP header")?;
            value.set_sensitive(true);
            map.insert(name, value);
        }
        Ok(Self::build(url, map, key.is_some()))
    }

    fn build(url: &str, headers: HeaderMap, has_key: bool) -> Self {
        Self {
            base: url.trim().trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .default_headers(headers)
                .build()
                .unwrap_or_default(),
            client_id: uuid::Uuid::new_v4().to_string(),
            has_key,
            comfy_org_key: None,
        }
    }

    /// Send a Comfy.org API key with each run (`extra_data.api_key_comfy_org`), which partner nodes
    /// such as Ideogram need when a workflow is queued over the API rather than from ComfyUI's page.
    pub fn with_comfy_org_key(mut self, key: Option<&str>) -> Self {
        self.comfy_org_key = key.map(str::trim).filter(|k| !k.is_empty()).map(String::from);
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// `/system_stats`: shows the server is up and reports its version.
    pub async fn system_stats(&self) -> anyhow::Result<Value> {
        let r = self
            .http
            .get(self.url("system_stats"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
        self.json_of(r).await
    }

    pub fn version(stats: &Value) -> Option<String> {
        stats["system"]["comfyui_version"].as_str().map(String::from)
    }

    /// Workflows saved in ComfyUI (`user/<user>/workflows`), as paths like `flux.json` or `3d/mesh.json`.
    /// Lists them with `/userdata`, or the newer `/v2/userdata` where that route is missing.
    pub async fn server_workflows(&self) -> anyhow::Result<Vec<String>> {
        let r = self
            .http
            .get(self.url("userdata"))
            .query(&[("dir", "workflows"), ("recurse", "true"), ("split", "false")])
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
        let status = r.status();
        let text = r.text().await?;
        let mut out: Vec<String> = if status.is_success() {
            let v: Value = serde_json::from_str(&text).context("ComfyUI listed its saved workflows as invalid JSON")?;
            v.as_array().into_iter().flatten().filter_map(listed_path).collect()
        } else if status == StatusCode::NOT_FOUND && text.contains("Directory not found") {
            // ComfyUI's answer until a workflow has been saved
            vec![]
        } else if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
            self.server_workflows_v2()
                .await
                .map_err(|e| anyhow!("/userdata: {}; /v2/userdata: {e:#}", self.http_error(status, &text)))?
        } else {
            bail!("{}", self.http_error(status, &text));
        };
        out.retain(|p| p.to_ascii_lowercase().ends_with(".json"));
        out.sort_by_key(|p| p.to_lowercase());
        out.dedup();
        Ok(out)
    }

    /// `/v2/userdata?path=…`: one folder per request, `{name, path, type}` entries.
    async fn server_workflows_v2(&self) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        // folders relative to `workflows/`, with their depth
        let mut dirs = vec![(String::new(), 0)];
        while let Some((dir, depth)) = dirs.pop() {
            let path = if dir.is_empty() { "workflows".to_string() } else { format!("workflows/{dir}") };
            let r = self
                .http
                .get(self.url("v2/userdata"))
                .query(&[("path", path.as_str())])
                .timeout(Duration::from_secs(15))
                .send()
                .await
                .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
            let v = self.json_of(r).await?;
            for e in v.as_array().into_iter().flatten() {
                let Some(name) = e["name"].as_str().or(e.as_str()).map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n))
                else {
                    continue;
                };
                let rel = if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") };
                if e["type"] == "directory" {
                    if depth < 4 {
                        dirs.push((rel, depth + 1));
                    }
                } else {
                    out.push(rel);
                }
            }
        }
        Ok(out)
    }

    /// One saved workflow (UI format), by a path from [`Self::server_workflows`].
    pub async fn server_workflow(&self, path: &str) -> anyhow::Result<Value> {
        let file = percent_encode(&format!("workflows/{}", path.trim_start_matches('/')));
        let r = self
            .http
            .get(self.url(&format!("userdata/{file}")))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
        self.json_of(r).await.with_context(|| format!("reading the workflow {path} from ComfyUI"))
    }

    /// Every node type's inputs and outputs (`/object_info`), to convert UI-format workflows.
    pub async fn object_info(&self) -> anyhow::Result<Value> {
        let r = self
            .http
            .get(self.url("object_info"))
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
        self.json_of(r).await.context("reading the node definitions from ComfyUI")
    }

    /// ComfyUI's template library index (`/templates/index.json`).
    pub async fn template_index(&self) -> anyhow::Result<Value> {
        let r = self
            .http
            .get(self.url("templates/index.json"))
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
        self.json_of(r).await.context("reading ComfyUI's template library")
    }

    /// One library template (UI format), by its index name.
    pub async fn template(&self, name: &str) -> anyhow::Result<Value> {
        let r = self
            .http
            .get(self.url(&format!("templates/{}.json", percent_encode(name))))
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
        self.json_of(r).await.with_context(|| format!("reading the template {name}"))
    }

    /// What this server can run for images and 3D: Odex's own workflows for installed node packs
    /// ([`recipes`]) first, then the library's text-to-image and image-to-3D templates it has every
    /// node and model for; and what the other templates lack ([`templates::prepare`]).
    pub async fn usable_templates(&self) -> anyhow::Result<(Vec<templates::Template>, Vec<templates::Unavailable>)> {
        use futures::StreamExt as _;
        let info = self.object_info().await?;
        let mut ready: Vec<templates::Template> = recipes::available(&info)
            .into_iter()
            .map(|r| templates::Template {
                name: r.name,
                title: r.title,
                kind: r.kind,
                models: r.models,
                partner: false,
            })
            .collect();
        // a server without the template library still has the recipes
        let index = match self.template_index().await {
            Ok(index) => index,
            Err(e) if !ready.is_empty() => {
                return Ok((
                    ready,
                    vec![templates::Unavailable {
                        title: "ComfyUI's template library".into(),
                        missing: vec![format!("{e:#}")],
                    }],
                ));
            }
            Err(e) => return Err(e),
        };
        let fetched: Vec<_> = futures::stream::iter(templates::candidates(&index))
            .map(|t| async move {
                let ui = self.template(&t.name).await;
                (t, ui)
            })
            .buffered(8)
            .collect()
            .await;
        let mut unavailable = Vec::new();
        for (t, ui) in fetched {
            let prepared = ui.map_err(|e| vec![format!("{e:#}")]).and_then(|ui| templates::prepare(&ui, &info, t.kind));
            match prepared {
                Ok(_) => ready.push(t),
                Err(missing) => unavailable.push(templates::Unavailable { title: t.title, missing }),
            }
        }
        Ok((ready, unavailable))
    }

    /// A recipe or library template built for this server, ready to save: `(title, graph)`.
    pub async fn template_workflow(&self, name: &str, kind: Kind) -> anyhow::Result<(String, Value)> {
        if name.starts_with(recipes::PREFIX) {
            let info = self.object_info().await?;
            let r = recipes::available(&info)
                .into_iter()
                .find(|r| r.name == name)
                .ok_or_else(|| anyhow!("this ComfyUI no longer has the nodes for {name}"))?;
            return Ok((r.title, r.graph));
        }
        let (ui, info, index) = tokio::try_join!(self.template(name), self.object_info(), self.template_index())?;
        let graph = templates::prepare(&ui, &info, kind)
            .map_err(|m| anyhow!("{name} can't run on this ComfyUI: it needs {}", m.join(", ")))?;
        let title = templates::candidates(&index).into_iter().find(|t| t.name == name).map(|t| t.title);
        Ok((title.unwrap_or_else(|| name.to_string()), graph))
    }

    /// Upload an input image for a `LoadImage` node; returns the name to use.
    pub async fn upload_image(&self, filename: &str, bytes: Vec<u8>) -> anyhow::Result<String> {
        let boundary = format!("odex-{}", uuid::Uuid::new_v4().simple());
        let safe: String = filename.chars().filter(|c| !matches!(c, '"' | '\r' | '\n' | '/' | '\\')).collect();
        let mut body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"{safe}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(&bytes);
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"overwrite\"\r\n\r\ntrue\r\n--{boundary}--\r\n")
                .as_bytes(),
        );
        let r = self
            .http
            .post(self.url("upload/image"))
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(body)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .context("uploading the input image to ComfyUI")?;
        let v = self.json_of(r).await?;
        let name = v["name"].as_str().ok_or_else(|| anyhow!("ComfyUI did not return the uploaded image's name"))?;
        Ok(match v["subfolder"].as_str().filter(|s| !s.is_empty()) {
            Some(sub) => format!("{sub}/{name}"),
            None => name.to_string(),
        })
    }

    /// Queue a filled workflow; returns its prompt id.
    pub async fn queue(&self, graph: &Value) -> anyhow::Result<String> {
        let r = self
            .http
            .post(self.url("prompt"))
            .json(&self.prompt_body(graph))
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
        let status = r.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            bail!("{}", self.http_error(status, ""));
        }
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() || v["node_errors"].as_object().is_some_and(|e| !e.is_empty()) {
            bail!("ComfyUI rejected the workflow: {}", describe_rejection(&v, status));
        }
        v["prompt_id"].as_str().map(String::from).ok_or_else(|| anyhow!("ComfyUI returned no prompt id"))
    }

    fn prompt_body(&self, graph: &Value) -> Value {
        let mut body = json!({"prompt": graph, "client_id": self.client_id});
        if let Some(k) = &self.comfy_org_key {
            body["extra_data"] = json!({"api_key_comfy_org": k});
        }
        body
    }

    /// Wait for a queued run and return the files it produced.
    pub async fn wait(&self, prompt_id: &str, timeout: Duration) -> anyhow::Result<Vec<OutputFile>> {
        let deadline = Instant::now() + timeout;
        let mut failures = 0;
        loop {
            match self.history(prompt_id).await {
                Ok(Some(entry)) => return outputs_of(&entry).map_err(|e| self.partner_hint(e)),
                Ok(None) => failures = 0,
                Err(e) => {
                    failures += 1;
                    if failures >= MAX_POLL_FAILURES {
                        return Err(e.context("lost contact with ComfyUI"));
                    }
                }
            }
            if Instant::now() >= deadline {
                bail!("ComfyUI did not finish within {} s", timeout.as_secs());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// The finished run's history entry, `None` while it is queued or running.
    async fn history(&self, prompt_id: &str) -> anyhow::Result<Option<Value>> {
        let r =
            self.http.get(self.url(&format!("history/{prompt_id}"))).timeout(Duration::from_secs(30)).send().await?;
        let mut v = self.json_of(r).await?;
        Ok(v.get_mut(prompt_id).map(Value::take))
    }

    pub async fn download(&self, f: &OutputFile) -> anyhow::Result<Vec<u8>> {
        let r = self
            .http
            .get(self.url("view"))
            .query(&[("filename", f.filename.as_str()), ("subfolder", f.subfolder.as_str()), ("type", f.kind.as_str())])
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .with_context(|| format!("downloading {}", f.filename))?;
        if !r.status().is_success() {
            bail!("downloading {} failed: {}", f.filename, self.http_error(r.status(), ""));
        }
        Ok(r.bytes().await?.to_vec())
    }

    /// A partner node that couldn't sign in to Comfy.org: say which key is missing.
    fn partner_hint(&self, e: anyhow::Error) -> anyhow::Error {
        let msg = e.to_string().to_lowercase();
        let auth = ["login", "log in", "unauthorized", "api key", "api_key", "credits"].iter().any(|w| msg.contains(w));
        if auth && self.comfy_org_key.is_none() {
            return e.context(
                "this looks like a partner (API) node such as Ideogram, which needs a Comfy.org API key (platform.comfy.org) in Odex's ComfyUI settings",
            );
        }
        e
    }

    async fn json_of(&self, r: reqwest::Response) -> anyhow::Result<Value> {
        let status = r.status();
        let text = r.text().await?;
        if !status.is_success() {
            bail!("{}", self.http_error(status, &text));
        }
        serde_json::from_str(&text).context("ComfyUI returned invalid JSON")
    }

    /// A failed response, readably: auth failures say what to fix, HTML error pages shrink to their title.
    fn http_error(&self, status: StatusCode, body: &str) -> String {
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return if self.has_key {
                format!("HTTP {status}: the server rejected the API key")
            } else {
                format!("HTTP {status}: the server needs an API key")
            };
        }
        let body = body.trim();
        let detail = if body.starts_with('<') { html_title(body) } else { Some(body.chars().take(300).collect()) };
        match detail.filter(|d| !d.is_empty()) {
            Some(d) => format!("HTTP {status}: {d}"),
            None => format!("HTTP {status}"),
        }
    }

    /// Drop a run: take it off the queue, or interrupt it if it is running.
    pub async fn cancel(&self, prompt_id: &str) {
        let t = Duration::from_secs(5);
        let _ = self.http.post(self.url("queue")).json(&json!({"delete": [prompt_id]})).timeout(t).send().await;
        let running = async {
            let v: Value = self.http.get(self.url("queue")).timeout(t).send().await.ok()?.json().await.ok()?;
            Some(v["queue_running"].as_array()?.iter().any(|e| e.get(1).and_then(|x| x.as_str()) == Some(prompt_id)))
        }
        .await;
        if running == Some(true) {
            let _ =
                self.http.post(self.url("interrupt")).json(&json!({"prompt_id": prompt_id})).timeout(t).send().await;
        }
    }
}

/// `<title>` of an HTML error page (proxies answer with those).
fn html_title(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let start = lower.find("<title>")? + "<title>".len();
    let end = start + lower[start..].find("</title>")?;
    Some(body[start..end].trim().to_string()).filter(|t| !t.is_empty())
}

/// One `/userdata` listing entry: a path, a `[path, ...parts]` row (`split`), or a `{path}` object (`full_info`).
fn listed_path(e: &Value) -> Option<String> {
    let p = match e {
        Value::String(s) => s.as_str(),
        Value::Array(a) => a.first()?.as_str()?,
        Value::Object(o) => o.get("path")?.as_str()?,
        _ => return None,
    };
    Some(p.replace('\\', "/"))
}

/// Percent-encode a path segment, `/` included (ComfyUI's userdata routes take the file as one segment).
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// An `Authorization` value that already names its scheme.
fn has_scheme(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    ["bearer ", "basic ", "token "].iter().any(|s| lower.starts_with(s))
}

/// A `/prompt` rejection: `{"error": {message, details}, "node_errors": {id: {class_type, errors}}}`.
fn describe_rejection(v: &Value, status: StatusCode) -> String {
    let mut parts = Vec::new();
    let e = &v["error"];
    if let Some(m) = e["message"].as_str().or(e.as_str()) {
        parts.push(match e["details"].as_str().filter(|d| !d.is_empty()) {
            Some(d) => format!("{m}: {d}"),
            None => m.to_string(),
        });
    }
    for (id, n) in v["node_errors"].as_object().into_iter().flatten() {
        let class = n["class_type"].as_str().unwrap_or("?");
        for err in n["errors"].as_array().into_iter().flatten() {
            let m = err["message"].as_str().unwrap_or("error");
            let d = err["details"].as_str().filter(|d| !d.is_empty()).map(|d| format!(" ({d})")).unwrap_or_default();
            parts.push(format!("node {id} ({class}): {m}{d}"));
        }
    }
    if parts.is_empty() {
        format!("HTTP {status}")
    } else {
        parts.join("; ")
    }
}

/// Files from a history entry, in node order; saved files win over previews.
fn outputs_of(entry: &Value) -> anyhow::Result<Vec<OutputFile>> {
    let status = &entry["status"];
    if status["status_str"].as_str() == Some("error") {
        let msg = status["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|m| {
                let d = m.get(1)?;
                match m.get(0)?.as_str()? {
                    "execution_error" => Some(format!(
                        "{} failed: {}",
                        d["node_type"].as_str().unwrap_or("a node"),
                        d["exception_message"].as_str().unwrap_or("error").trim()
                    )),
                    "execution_interrupted" => Some("the run was interrupted".into()),
                    _ => None,
                }
            })
            .unwrap_or_else(|| "the workflow failed".into());
        bail!("ComfyUI: {msg}");
    }
    let mut nodes: Vec<(&String, &Value)> =
        entry["outputs"].as_object().map(|o| o.iter().collect()).unwrap_or_default();
    nodes.sort_by_key(|(id, _)| (id.parse::<u64>().unwrap_or(u64::MAX), id.to_string()));
    let mut files = Vec::new();
    for (node, out) in nodes {
        for list in out.as_object().into_iter().flat_map(|o| o.values()) {
            for f in list.as_array().into_iter().flatten() {
                if let Some(name) = f["filename"].as_str() {
                    files.push(OutputFile {
                        node: node.clone(),
                        filename: name.to_string(),
                        subfolder: f["subfolder"].as_str().unwrap_or("").to_string(),
                        kind: f["type"].as_str().unwrap_or("output").to_string(),
                    });
                }
            }
        }
    }
    if files.iter().any(|f| f.kind == "output") {
        files.retain(|f| f.kind == "output");
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_outputs_in_node_order() {
        let entry = json!({
            "status": {"status_str": "success", "completed": true},
            "outputs": {
                "12": {"3d": [{"filename": "mesh_00001_.glb", "subfolder": "mesh", "type": "output"}]},
                "9": {"images": [{"filename": "a.png", "subfolder": "", "type": "output"}], "text": ["ignored"]},
                "5": {"images": [{"filename": "preview.png", "subfolder": "", "type": "temp"}]}
            }
        });
        let files = outputs_of(&entry).unwrap();
        assert_eq!(files.iter().map(|f| f.filename.as_str()).collect::<Vec<_>>(), vec!["a.png", "mesh_00001_.glb"]);
        assert_eq!(files[1].extension(), "glb");
        assert_eq!(files[1].subfolder, "mesh");
        // previews only: keep them
        let previews = json!({"outputs": {"5": {"images": [{"filename": "p.png", "type": "temp"}]}}});
        assert_eq!(outputs_of(&previews).unwrap()[0].kind, "temp");
    }

    #[test]
    fn reports_execution_errors() {
        let entry = json!({"status": {"status_str": "error", "messages": [
            ["execution_start", {}],
            ["execution_error", {"node_type": "CheckpointLoaderSimple", "exception_message": "flux.safetensors not found\n"}]
        ]}, "outputs": {}});
        let e = outputs_of(&entry).unwrap_err().to_string();
        assert_eq!(e, "ComfyUI: CheckpointLoaderSimple failed: flux.safetensors not found");
    }

    #[test]
    fn readable_http_errors() {
        let c = ComfyClient::new("http://x");
        let page = "<html>\r\n<head><title>502 Bad Gateway</title></head>\r\n<body>nginx</body></html>";
        assert_eq!(c.http_error(StatusCode::BAD_GATEWAY, page), "HTTP 502 Bad Gateway: 502 Bad Gateway");
        assert_eq!(c.http_error(StatusCode::NOT_FOUND, "no such route"), "HTTP 404 Not Found: no such route");
        assert_eq!(c.http_error(StatusCode::FORBIDDEN, page), "HTTP 403 Forbidden: the server needs an API key");
        assert!(has_scheme("Basic dXNlcjpwYXNz") && has_scheme("bearer x") && !has_scheme("sk-123"));
        assert_eq!(listed_path(&json!(r"3d\a.json")).as_deref(), Some("3d/a.json"));
        assert_eq!(listed_path(&json!(["3d/a.json", "3d", "a.json"])).as_deref(), Some("3d/a.json"));
        assert_eq!(listed_path(&json!({"path": "b.json", "size": 10})).as_deref(), Some("b.json"));
        assert_eq!(percent_encode("workflows/3d/My Mesh é.json"), "workflows%2F3d%2FMy%20Mesh%20%C3%A9.json");
    }

    #[test]
    fn describes_rejections() {
        let v = json!({
            "error": {"type": "prompt_outputs_failed_validation", "message": "Prompt outputs failed validation", "details": ""},
            "node_errors": {"4": {"class_type": "CheckpointLoaderSimple", "errors": [
                {"message": "Value not in list", "details": "ckpt_name: 'x.safetensors' not in []"}
            ]}}
        });
        assert_eq!(
            describe_rejection(&v, StatusCode::BAD_REQUEST),
            "Prompt outputs failed validation; node 4 (CheckpointLoaderSimple): Value not in list (ckpt_name: 'x.safetensors' not in [])"
        );
    }
}
