//! A mock ComfyUI server: `/system_stats`, `/prompt`, `/history/{id}`,
//! `/view`, `/upload/image`, `/queue` and `/interrupt`. Every run finishes
//! after one pending poll with the configured outputs. [`MockComfy::require_header`]
//! puts it behind an API-key check like an authenticating proxy.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use serde_json::{json, Value};

/// A 1x1 PNG served for image outputs.
pub const PNG_1X1: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

#[derive(Default)]
struct Inner {
    prompts: Vec<Value>,
    uploads: Vec<String>,
    deleted: Vec<String>,
    polls: HashMap<String, u32>,
    outputs: Option<Value>,
    /// `(node_type, message)` for an execution error.
    fail: Option<(String, String)>,
    reject: Option<Value>,
    /// Pending polls before a run finishes (u32::MAX = never).
    pending_polls: u32,
    /// `(header, value)` every request must carry, else HTTP 401.
    auth: Option<(String, String)>,
}

#[derive(Clone, Default)]
pub struct ComfyState {
    inner: Arc<Mutex<Inner>>,
}

pub struct MockComfy {
    pub url: String,
    state: ComfyState,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockComfy {
    pub async fn start() -> Self {
        Self::start_on("127.0.0.1:0").await
    }

    pub async fn start_on(bind: &str) -> Self {
        let state = ComfyState::default();
        state.inner.lock().unwrap().pending_polls = 1;
        let listener = tokio::net::TcpListener::bind(bind).await.expect("bind mock comfyui");
        let addr = listener.local_addr().unwrap();
        let app = router(state.clone());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });
        Self { url: format!("http://{addr}"), state, shutdown: Some(tx) }
    }

    /// History `outputs` for every run (default: one saved PNG from node 9).
    pub fn set_outputs(&self, outputs: Value) {
        self.state.inner.lock().unwrap().outputs = Some(outputs);
    }

    /// Make runs fail in `node_type` with `message`.
    pub fn fail_with(&self, node_type: &str, message: &str) {
        self.state.inner.lock().unwrap().fail = Some((node_type.into(), message.into()));
    }

    /// Answer `/prompt` with HTTP 400 and this body.
    pub fn reject_with(&self, body: Value) {
        self.state.inner.lock().unwrap().reject = Some(body);
    }

    /// Answer HTTP 401 (an nginx-style HTML page) unless requests carry `header: value`.
    pub fn require_header(&self, header: &str, value: &str) {
        self.state.inner.lock().unwrap().auth = Some((header.to_ascii_lowercase(), value.into()));
    }

    /// Keep runs pending forever (cancellation tests).
    pub fn never_finish(&self) {
        self.state.inner.lock().unwrap().pending_polls = u32::MAX;
    }

    /// Graphs posted to `/prompt`.
    pub fn prompts(&self) -> Vec<Value> {
        self.state.inner.lock().unwrap().prompts.clone()
    }

    /// File names posted to `/upload/image`.
    pub fn uploads(&self) -> Vec<String> {
        self.state.inner.lock().unwrap().uploads.clone()
    }

    /// Prompt ids removed through `/queue`.
    pub fn deleted(&self) -> Vec<String> {
        self.state.inner.lock().unwrap().deleted.clone()
    }
}

impl Drop for MockComfy {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

pub fn router(state: ComfyState) -> Router {
    Router::new()
        .route(
            "/system_stats",
            get(|| async { Json(json!({"system": {"comfyui_version": "0.3.60-mock", "os": "nt"}, "devices": []})) }),
        )
        .route("/prompt", post(prompt))
        .route("/history/:id", get(history))
        .route("/view", get(view))
        .route("/upload/image", post(upload))
        .route("/queue", get(|| async { Json(json!({"queue_running": [], "queue_pending": []})) }).post(queue))
        .route("/interrupt", post(|| async { StatusCode::OK }))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

async fn guard(State(st): State<ComfyState>, req: Request, next: Next) -> Response {
    let auth = st.inner.lock().unwrap().auth.clone();
    if let Some((header, value)) = auth {
        if req.headers().get(header.as_str()).and_then(|v| v.to_str().ok()) != Some(value.as_str()) {
            let page = "<html>\r\n<head><title>401 Authorization Required</title></head>\r\n<body>\r\n<center><h1>401 Authorization Required</h1></center>\r\n<hr><center>nginx</center>\r\n</body>\r\n</html>\r\n";
            return (StatusCode::UNAUTHORIZED, [("content-type", "text/html")], page).into_response();
        }
    }
    next.run(req).await
}

async fn prompt(State(st): State<ComfyState>, Json(body): Json<Value>) -> Response {
    let mut i = st.inner.lock().unwrap();
    if let Some(r) = i.reject.clone() {
        return (StatusCode::BAD_REQUEST, Json(r)).into_response();
    }
    i.prompts.push(body["prompt"].clone());
    let id = format!("prompt-{}", i.prompts.len());
    Json(json!({"prompt_id": id, "number": i.prompts.len() - 1, "node_errors": {}})).into_response()
}

async fn history(State(st): State<ComfyState>, Path(id): Path<String>) -> Json<Value> {
    let mut i = st.inner.lock().unwrap();
    let pending = i.pending_polls;
    let n = i.polls.entry(id.clone()).or_default();
    *n += 1;
    if *n <= pending || !id.starts_with("prompt-") {
        return Json(json!({}));
    }
    let entry = match &i.fail {
        Some((node, msg)) => json!({"outputs": {}, "status": {"status_str": "error", "completed": false, "messages": [
            ["execution_start", {"prompt_id": id}],
            ["execution_error", {"prompt_id": id, "node_type": node, "exception_message": msg}]
        ]}}),
        None => json!({
            "outputs": i.outputs.clone().unwrap_or_else(|| json!({
                "9": {"images": [{"filename": "ComfyUI_00001_.png", "subfolder": "", "type": "output"}]}
            })),
            "status": {"status_str": "success", "completed": true, "messages": []}
        }),
    };
    let mut out = serde_json::Map::new();
    out.insert(id, entry);
    Json(Value::Object(out))
}

async fn view(Query(q): Query<HashMap<String, String>>) -> Response {
    let name = q.get("filename").cloned().unwrap_or_default();
    let bytes = if name.ends_with(".png") {
        base64::engine::general_purpose::STANDARD.decode(PNG_1X1).unwrap()
    } else if name.ends_with(".glb") {
        b"glTF\x02\x00\x00\x00mock-mesh".to_vec()
    } else {
        return StatusCode::NOT_FOUND.into_response();
    };
    ([("content-type", "application/octet-stream")], bytes).into_response()
}

async fn upload(State(st): State<ComfyState>, body: Bytes) -> Json<Value> {
    let text = String::from_utf8_lossy(&body);
    let name = text.split("filename=\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("upload.png").to_string();
    st.inner.lock().unwrap().uploads.push(name.clone());
    Json(json!({"name": name, "subfolder": "", "type": "input"}))
}

async fn queue(State(st): State<ComfyState>, Json(body): Json<Value>) -> StatusCode {
    let mut i = st.inner.lock().unwrap();
    for id in body["delete"].as_array().into_iter().flatten().filter_map(|v| v.as_str()) {
        i.deleted.push(id.to_string());
    }
    StatusCode::OK
}
