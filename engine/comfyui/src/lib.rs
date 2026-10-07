//! ComfyUI client for image and 3D generation.
//!
//! Workflows are API-format exports with `{{prompt}}`-style placeholders (see
//! [`workflow`]). A run queues the filled graph on `/prompt`, polls
//! `/history/<id>` until it finishes, then downloads the files its output
//! nodes saved (`SaveImage`, `SaveGLB`, ...) through `/view`.

pub mod workflow;

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _};
use serde_json::{json, Value};

pub use workflow::{import_workflow, list_workflows, Inputs, Workflow, PLACEHOLDERS};

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
}

impl ComfyClient {
    pub fn new(url: &str) -> Self {
        Self {
            base: url.trim().trim_end_matches('/').to_string(),
            http: reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build().unwrap_or_default(),
            client_id: uuid::Uuid::new_v4().to_string(),
        }
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
        json_of(r).await
    }

    pub fn version(stats: &Value) -> Option<String> {
        stats["system"]["comfyui_version"].as_str().map(String::from)
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
        let v = json_of(r).await?;
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
            .json(&json!({"prompt": graph, "client_id": self.client_id}))
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .with_context(|| format!("cannot reach ComfyUI at {}", self.base))?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() || v["node_errors"].as_object().is_some_and(|e| !e.is_empty()) {
            bail!("ComfyUI rejected the workflow: {}", describe_rejection(&v, status));
        }
        v["prompt_id"].as_str().map(String::from).ok_or_else(|| anyhow!("ComfyUI returned no prompt id"))
    }

    /// Wait for a queued run and return the files it produced.
    pub async fn wait(&self, prompt_id: &str, timeout: Duration) -> anyhow::Result<Vec<OutputFile>> {
        let deadline = Instant::now() + timeout;
        let mut failures = 0;
        loop {
            match self.history(prompt_id).await {
                Ok(Some(entry)) => return outputs_of(&entry),
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
        let mut v = json_of(r).await?;
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
            bail!("downloading {} failed: HTTP {}", f.filename, r.status());
        }
        Ok(r.bytes().await?.to_vec())
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

async fn json_of(r: reqwest::Response) -> anyhow::Result<Value> {
    let status = r.status();
    let text = r.text().await?;
    if !status.is_success() {
        bail!("HTTP {status}: {}", text.chars().take(300).collect::<String>());
    }
    serde_json::from_str(&text).context("ComfyUI returned invalid JSON")
}

/// A `/prompt` rejection: `{"error": {message, details}, "node_errors": {id: {class_type, errors}}}`.
fn describe_rejection(v: &Value, status: reqwest::StatusCode) -> String {
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
    fn describes_rejections() {
        let v = json!({
            "error": {"type": "prompt_outputs_failed_validation", "message": "Prompt outputs failed validation", "details": ""},
            "node_errors": {"4": {"class_type": "CheckpointLoaderSimple", "errors": [
                {"message": "Value not in list", "details": "ckpt_name: 'x.safetensors' not in []"}
            ]}}
        });
        assert_eq!(
            describe_rejection(&v, reqwest::StatusCode::BAD_REQUEST),
            "Prompt outputs failed validation; node 4 (CheckpointLoaderSimple): Value not in list (ckpt_name: 'x.safetensors' not in [])"
        );
    }
}
