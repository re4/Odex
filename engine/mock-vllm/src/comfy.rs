//! A mock ComfyUI server: `/system_stats`, `/prompt`, `/history/{id}`,
//! `/view`, `/upload/image`, `/queue` and `/interrupt`. Every run finishes
//! after one pending poll with the configured outputs. [`MockComfy::require_header`]
//! puts it behind an API-key check like an authenticating proxy. `/userdata` serves
//! saved (UI-format) workflows and `/object_info` the node definitions they use; `/templates`
//! a small template library.

use std::collections::{BTreeMap, HashMap};
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

/// A flat gray 64x64 PNG: how Ideogram 4 answers a prompt its safety filter refuses.
pub const PNG_GRAY_64: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAS0lEQVR42u3PMQ0AAAwDoEqv9ErYvQQckD4XAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAYHLAB8+AWnmfUycAAAAAElFTkSuQmCC";

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
    /// Saved workflows by path under `workflows/`.
    saved: BTreeMap<String, Value>,
    /// `extra_data` of each `/prompt`.
    extra: Vec<Value>,
    /// Runs without this `extra_data.api_key_comfy_org` fail like a partner node that can't sign in.
    comfy_org_key: Option<String>,
    /// Per-run `(node_type, message)` execution errors.
    failing: HashMap<String, (String, String)>,
    /// Image outputs come back as a flat gray square (a refused prompt).
    gray: bool,
    /// `/userdata` listing: 0 = like ComfyUI, 1 = route missing (only `/v2/userdata`), 2 = failing.
    userdata_mode: u8,
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
        {
            let mut i = state.inner.lock().unwrap();
            i.pending_polls = 1;
            i.saved.insert("txt2img.json".into(), serde_json::from_str(SAVED_TXT2IMG).unwrap());
            i.saved.insert("3d/image to mesh.json".into(), serde_json::from_str(SAVED_IMAGE_TO_MESH).unwrap());
        }
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

    /// Save a workflow (UI format) the way ComfyUI does, under `workflows/<path>`.
    pub fn save_workflow(&self, path: &str, workflow: Value) {
        self.state.inner.lock().unwrap().saved.insert(path.into(), workflow);
    }

    /// Fail runs that don't carry this Comfy.org key, like a partner node (e.g. Ideogram) that can't sign in.
    pub fn require_comfy_org_key(&self, key: &str) {
        self.state.inner.lock().unwrap().comfy_org_key = Some(key.into());
    }

    /// `extra_data` sent with each run (`Null` when there was none).
    pub fn extra_data(&self) -> Vec<Value> {
        self.state.inner.lock().unwrap().extra.clone()
    }

    /// Serve every image output as a flat gray square, like a prompt Ideogram 4 refused.
    pub fn serve_gray_images(&self) {
        self.state.inner.lock().unwrap().gray = true;
    }

    /// Forget the saved workflows (a server where nothing was saved yet).
    pub fn clear_saved(&self) {
        self.state.inner.lock().unwrap().saved.clear();
    }

    /// Serve the saved-workflow listing only through `/v2/userdata` (`/userdata` answers 404).
    pub fn v2_userdata_only(&self) {
        self.state.inner.lock().unwrap().userdata_mode = 1;
    }

    /// Make every saved-workflow listing fail with HTTP 500.
    pub fn break_userdata(&self) {
        self.state.inner.lock().unwrap().userdata_mode = 2;
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
        .route("/userdata", get(list_userdata))
        .route("/templates/:file", get(template_file))
        .route("/v2/userdata", get(list_userdata_v2))
        .route("/userdata/:file", get(get_userdata))
        .route("/object_info", get(|| async { ([("content-type", "application/json")], OBJECT_INFO) }))
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
    i.extra.push(body["extra_data"].clone());
    let id = format!("prompt-{}", i.prompts.len());
    if let Some(key) = i.comfy_org_key.clone() {
        if body["extra_data"]["api_key_comfy_org"].as_str() != Some(key.as_str()) {
            let error = ("IdeogramV4".to_string(), "Unauthorized: Please login first to use this node.".to_string());
            i.failing.insert(id.clone(), error);
        }
    }
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
    let entry = match i.failing.get(&id).or(i.fail.as_ref()) {
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

async fn view(State(st): State<ComfyState>, Query(q): Query<HashMap<String, String>>) -> Response {
    let name = q.get("filename").cloned().unwrap_or_default();
    let gray = st.inner.lock().unwrap().gray;
    let bytes = if name.ends_with(".png") {
        base64::engine::general_purpose::STANDARD.decode(if gray { PNG_GRAY_64 } else { PNG_1X1 }).unwrap()
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

async fn list_userdata(State(st): State<ComfyState>, Query(q): Query<HashMap<String, String>>) -> Response {
    let i = st.inner.lock().unwrap();
    match i.userdata_mode {
        1 => return (StatusCode::NOT_FOUND, "404: Not Found").into_response(),
        2 => return (StatusCode::INTERNAL_SERVER_ERROR, "500 Internal Server Error").into_response(),
        _ => {}
    }
    // like ComfyUI: the folder only exists once something was saved
    if q.get("dir").map(String::as_str) != Some("workflows") || i.saved.is_empty() {
        return (StatusCode::NOT_FOUND, "Directory not found").into_response();
    }
    Json(i.saved.keys().cloned().collect::<Vec<_>>()).into_response()
}

/// `/v2/userdata?path=workflows/<dir>`: the files and folders directly inside one folder.
async fn list_userdata_v2(State(st): State<ComfyState>, Query(q): Query<HashMap<String, String>>) -> Response {
    let i = st.inner.lock().unwrap();
    if i.userdata_mode == 2 {
        return (StatusCode::INTERNAL_SERVER_ERROR, "500 Internal Server Error").into_response();
    }
    let Some(dir) = q.get("path").and_then(|p| p.strip_prefix("workflows")).map(|d| d.trim_matches('/')) else {
        return (StatusCode::NOT_FOUND, "Directory not found").into_response();
    };
    let mut entries = BTreeMap::new();
    for key in i.saved.keys() {
        let rest = if dir.is_empty() { Some(key.as_str()) } else { key.strip_prefix(&format!("{dir}/")) };
        let Some(rest) = rest else { continue };
        let (name, kind) = match rest.split_once('/') {
            Some((folder, _)) => (folder, "directory"),
            None => (rest, "file"),
        };
        let full = if dir.is_empty() { format!("workflows/{name}") } else { format!("workflows/{dir}/{name}") };
        entries.insert(name.to_string(), json!({"name": name, "path": full, "type": kind}));
    }
    Json(entries.into_values().collect::<Vec<_>>()).into_response()
}

async fn get_userdata(State(st): State<ComfyState>, Path(file): Path<String>) -> Response {
    let saved = file.strip_prefix("workflows/").and_then(|p| st.inner.lock().unwrap().saved.get(p).cloned());
    match saved {
        Some(w) => Json(w).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Node definitions for the saved workflows, shaped like ComfyUI's `/object_info`.
const OBJECT_INFO: &str = r#"{
  "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["sd15.safetensors"], {}]}}, "output": ["MODEL", "CLIP", "VAE"]},
  "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}], "clip": ["CLIP", {}]}}, "output": ["CONDITIONING"]},
  "EmptyLatentImage": {"input": {"required": {"width": ["INT", {"default": 512}], "height": ["INT", {"default": 512}], "batch_size": ["INT", {"default": 1}]}}, "output": ["LATENT"]},
  "KSampler": {
    "input": {"required": {
      "model": ["MODEL", {}], "seed": ["INT", {"control_after_generate": true}], "steps": ["INT", {}], "cfg": ["FLOAT", {}],
      "sampler_name": ["COMBO", {"options": ["euler"]}], "scheduler": [["normal"], {}],
      "positive": ["CONDITIONING", {}], "negative": ["CONDITIONING", {}], "latent_image": ["LATENT", {}], "denoise": ["FLOAT", {}]
    }},
    "input_order": {"required": ["model", "seed", "steps", "cfg", "sampler_name", "scheduler", "positive", "negative", "latent_image", "denoise"]},
    "output": ["LATENT"]
  },
  "VAEDecode": {"input": {"required": {"samples": ["LATENT", {}], "vae": ["VAE", {}]}}, "output": ["IMAGE"]},
  "SaveImage": {"input": {"required": {"images": ["IMAGE", {}], "filename_prefix": ["STRING", {"default": "ComfyUI"}]}}, "output": [], "output_node": true},
  "LoadImage": {"input": {"required": {"image": [["example.png"], {"image_upload": true}]}}, "output": ["IMAGE", "MASK"]},
  "ImageToMesh": {"input": {"required": {"image": ["IMAGE", {}], "steps": ["INT", {"default": 30}]}, "optional": {"model": ["MODEL", {}]}}, "output": ["MESH"]},
  "SaveGLB": {"input": {"required": {"mesh": ["MESH", {}], "filename_prefix": ["STRING", {"default": "mesh/ComfyUI"}]}}, "output": [], "output_node": true},
  "UNETLoader": {"input": {"required": {
    "unet_name": [["ideogram4_int8_convrot.safetensors", "pixal3d_int8_convrot.safetensors"], {}], "weight_dtype": [["default", "fp8_e4m3fn"], {}]
  }}, "output": ["MODEL"]},
  "CLIPLoader": {"input": {"required": {
    "clip_name": [["qwen3vl_8b_fp8_scaled.safetensors"], {}], "type": [["stable_diffusion", "ideogram4"], {}]
  }, "optional": {"device": [["default", "cpu"], {}]}}, "output": ["CLIP"]},
  "VAELoader": {"input": {"required": {"vae_name": [["flux2-vae.safetensors"], {}]}}, "output": ["VAE"]},
  "PrimitiveInt": {"input": {"required": {"value": ["INT", {"control_after_generate": true}]}}, "output": ["INT"]},
  "StringReplace": {"input": {"required": {"string": ["STRING", {}], "find": ["STRING", {}], "replace": ["STRING", {}]}}, "output": ["STRING"]},
  "PreviewAny": {"input": {"required": {"source": ["*", {}]}}, "output": [], "output_node": true},
  "PreviewImage": {"input": {"required": {"images": ["IMAGE", {}]}}, "output": [], "output_node": true},
  "IdeogramV4": {"input": {"required": {
    "prompt": ["STRING", {"multiline": true}], "aspect_ratio": ["COMBO", {"options": ["Auto", "1:1", "16:9"]}],
    "rendering_speed": [["DEFAULT", "TURBO", "QUALITY"], {}], "seed": ["INT", {"control_after_generate": true}]
  }}, "input_order": {"required": ["prompt", "aspect_ratio", "rendering_speed", "seed"]}, "output": ["IMAGE"], "api_node": true},
  "GeminiNodeV3": {"input": {"required": {"model": [["Gemini"], {}], "prompt": ["STRING", {}]}}, "output": ["STRING"], "api_node": true},
  "Ideogram4PipelineLoader": {"input": {"required": {"model_weights": [["4.0 NF4", "4.0 FP8"], {"default": "4.0 NF4"}]}},
    "input_order": {"required": ["model_weights"], "hidden": ["unique_id"]}, "output": ["IDEOGRAM4_PIPELINE"], "python_module": "custom_nodes.ComfyUI-Ideogram4"},
  "Ideogram4Generate": {"input": {"required": {
    "pipeline": ["IDEOGRAM4_PIPELINE", {"forceInput": true}], "prompt": ["STRING", {"default": "", "multiline": true}],
    "width": ["INT", {"default": 2048, "min": 256, "max": 2048, "step": 16}], "height": ["INT", {"default": 2048, "min": 256, "max": 2048, "step": 16}],
    "sampler_preset": [["custom", "4.0 Quality 48", "4.0 Default 20", "4.0 Turbo 12"], {"default": "4.0 Default 20"}],
    "num_steps": ["INT", {"default": 20}], "guidance_scale": ["FLOAT", {"default": 7.0}], "mu": ["FLOAT", {"default": 0.0}],
    "std": ["FLOAT", {"default": 1.75}], "seed": ["INT", {"default": 0, "control_after_generate": true}]
  }}, "input_order": {"required": ["pipeline", "prompt", "width", "height", "sampler_preset", "num_steps", "guidance_scale", "mu", "std", "seed"]},
    "output": ["IMAGE"], "python_module": "custom_nodes.ComfyUI-Ideogram4"},
  "RunningHubPixal3DModelLoader": {"input": {"required": {
    "attention_backend": [["flash_attn", "sdpa"], {"default": "flash_attn"}], "sparse_conv_backend": [["flex_gemm", "spconv"], {"default": "flex_gemm"}],
    "low_vram": ["BOOLEAN", {"default": false}]
  }}, "output": ["PIXAL3D_PIPE"], "python_module": "custom_nodes.ComfyUI_RH_Pixal3D"},
  "RunningHubPixal3DImageTo3D": {"input": {"required": {
    "pipe": ["PIXAL3D_PIPE"], "image": ["IMAGE"], "seed": ["INT", {"default": 42, "min": 0, "max": 2147483647}],
    "resolution": [["1024", "1536"], {"default": "1024"}]
  }, "optional": {"mask": ["MASK"]}}, "output": ["PIXAL3D_ASSET", "STRING"], "python_module": "custom_nodes.ComfyUI_RH_Pixal3D"},
  "RunningHubPixal3DSaveGLB": {"input": {"required": {
    "asset": ["PIXAL3D_ASSET"], "decimation_target": ["INT", {"default": 200000}], "texture_size": ["INT", {"default": 2048}],
    "remesh": ["BOOLEAN", {"default": true}], "filename_prefix": ["STRING", {"default": "3d/Pixal3D"}]
  }}, "output": ["STRING"], "output_node": true, "python_module": "custom_nodes.ComfyUI_RH_Pixal3D"}
}"#;

/// ComfyUI's default text-to-image graph, saved from the UI (no placeholders).
const SAVED_TXT2IMG: &str = r#"{
  "last_node_id": 9, "last_link_id": 9, "version": 0.4,
  "nodes": [
    {"id": 7, "type": "CLIPTextEncode", "mode": 0, "inputs": [{"name": "clip", "type": "CLIP", "link": 5}], "outputs": [{"name": "CONDITIONING", "type": "CONDITIONING", "links": [6]}], "widgets_values": ["text, watermark"]},
    {"id": 6, "type": "CLIPTextEncode", "mode": 0, "inputs": [{"name": "clip", "type": "CLIP", "link": 3}], "outputs": [{"name": "CONDITIONING", "type": "CONDITIONING", "links": [4]}], "widgets_values": ["beautiful scenery nature glass bottle landscape"]},
    {"id": 5, "type": "EmptyLatentImage", "mode": 0, "outputs": [{"name": "LATENT", "type": "LATENT", "links": [2]}], "widgets_values": [512, 512, 1]},
    {"id": 3, "type": "KSampler", "mode": 0, "inputs": [
      {"name": "model", "type": "MODEL", "link": 1}, {"name": "positive", "type": "CONDITIONING", "link": 4},
      {"name": "negative", "type": "CONDITIONING", "link": 6}, {"name": "latent_image", "type": "LATENT", "link": 2}
    ], "outputs": [{"name": "LATENT", "type": "LATENT", "links": [7]}], "widgets_values": [156680208700286, "randomize", 20, 8, "euler", "normal", 1]},
    {"id": 8, "type": "VAEDecode", "mode": 0, "inputs": [{"name": "samples", "type": "LATENT", "link": 7}, {"name": "vae", "type": "VAE", "link": 8}], "outputs": [{"name": "IMAGE", "type": "IMAGE", "links": [9]}]},
    {"id": 9, "type": "SaveImage", "mode": 0, "inputs": [{"name": "images", "type": "IMAGE", "link": 9}], "widgets_values": ["ComfyUI"]},
    {"id": 4, "type": "CheckpointLoaderSimple", "mode": 0, "outputs": [{"name": "MODEL", "type": "MODEL", "links": [1]}, {"name": "CLIP", "type": "CLIP", "links": [3, 5]}, {"name": "VAE", "type": "VAE", "links": [8]}], "widgets_values": ["sd15.safetensors"]}
  ],
  "links": [[1, 4, 0, 3, 0, "MODEL"], [2, 5, 0, 3, 3, "LATENT"], [3, 4, 1, 6, 0, "CLIP"], [4, 6, 0, 3, 1, "CONDITIONING"], [5, 4, 1, 7, 0, "CLIP"], [6, 7, 0, 3, 2, "CONDITIONING"], [7, 3, 0, 8, 0, "LATENT"], [8, 4, 2, 8, 1, "VAE"], [9, 8, 0, 9, 0, "IMAGE"]]
}"#;

/// An image-to-3D graph saved from the UI: Load Image -> ImageToMesh -> SaveGLB.
const SAVED_IMAGE_TO_MESH: &str = r#"{
  "nodes": [
    {"id": 1, "type": "LoadImage", "mode": 0, "outputs": [{"name": "IMAGE", "type": "IMAGE", "links": [1]}, {"name": "MASK", "type": "MASK", "links": []}], "widgets_values": ["chair.png", "image"]},
    {"id": 2, "type": "ImageToMesh", "mode": 0, "inputs": [{"name": "image", "type": "IMAGE", "link": 1}], "outputs": [{"name": "MESH", "type": "MESH", "links": [2]}], "widgets_values": [30]},
    {"id": 3, "type": "SaveGLB", "mode": 0, "inputs": [{"name": "mesh", "type": "MESH", "link": 2}], "widgets_values": ["mesh/odex"]},
    {"id": 4, "type": "Note", "mode": 0, "widgets_values": ["Drop a photo of one object into Load Image."]}
  ],
  "links": [{"id": 1, "origin_id": 1, "origin_slot": 0, "target_id": 2, "target_slot": 0, "type": "IMAGE"}, {"id": 2, "origin_id": 2, "origin_slot": 0, "target_id": 3, "target_slot": 0, "type": "MESH"}]
}"#;

/// `/templates/index.json` and `/templates/<name>.json`: ComfyUI's template library.
async fn template_file(Path(file): Path<String>) -> Response {
    let body = match file.as_str() {
        "index.json" => TEMPLATE_INDEX,
        "image_ideogram4_t2i_int8.json" => TEMPLATE_IDEOGRAM_LOCAL,
        "api_ideogram_v4_t2i.json" => TEMPLATE_IDEOGRAM_API,
        "3d_pixal3d_trellis2_image_to_model.json" => TEMPLATE_PIXAL3D,
        "flux_dev_full_text_to_image.json" => TEMPLATE_FLUX_MISSING_MODEL,
        "api_tripo_image_to_model.json" => TEMPLATE_TRIPO_MISSING_NODE,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([("content-type", "application/json")], body).into_response()
}

/// The library index: image and 3D categories (plus an edit template that isn't text-to-image).
const TEMPLATE_INDEX: &str = r#"[
  {"moduleName": "default", "type": "image", "title": "Image", "templates": [
    {"name": "image_ideogram4_t2i_int8", "title": "Ideogram v4 Int8: Text to Image", "tags": ["Text to Image", "Int8"], "models": ["Ideogram"], "openSource": true},
    {"name": "api_ideogram_v4_t2i", "title": "Ideogram v4: Text to Image (API)", "tags": ["API", "Text to Image"], "models": ["Ideogram"], "openSource": false},
    {"name": "flux_dev_full_text_to_image", "title": "Flux.1 Dev: Text to Image", "tags": ["Text to Image"], "models": ["Flux"], "openSource": true},
    {"name": "qwen_image_edit", "title": "Qwen Image Edit", "tags": ["Image Edit"], "io": {"inputs": [{"nodeType": "LoadImage", "mediaType": "image"}]}}
  ]},
  {"moduleName": "default", "type": "3d", "title": "3D Model", "templates": [
    {"name": "3d_pixal3d_trellis2_image_to_model", "title": "Pixal3D & TRELLIS.2: Image to Model", "tags": ["3D", "Image to 3D", "Int8"], "models": ["Pixal3D", "TRELLIS.2"], "openSource": true},
    {"name": "api_tripo_image_to_model", "title": "Tripo: Image to Model", "tags": ["3D", "API", "Image to 3D"], "models": ["Tripo"], "openSource": false}
  ]}
]"#;

/// Local Ideogram v4: the pipeline in a subgraph (prompt promoted from inside, size from a node
/// outside), plus a helper subgraph that only feeds a preview and has literal `{{...}}` text.
const TEMPLATE_IDEOGRAM_LOCAL: &str = r#"{
  "nodes": [
    {"id": 37, "type": "PrimitiveInt", "mode": 0, "outputs": [{"name": "INT", "type": "INT", "links": [161]}], "widgets_values": [1024, "fixed"]},
    {"id": 98, "type": "83e6e004-pipeline", "mode": 0, "inputs": [
      {"name": "text", "type": "STRING", "link": null, "widget": {"name": "text"}},
      {"name": "value", "type": "INT", "link": 161, "widget": {"name": "value"}}
    ], "outputs": [{"name": "IMAGE", "type": "IMAGE", "links": [224]}], "widgets_values": [], "properties": {"proxyWidgets": [["24", "text"], ["27", "value"]]}},
    {"id": 158, "type": "SaveImage", "mode": 0, "inputs": [{"name": "images", "type": "IMAGE", "link": 224}], "widgets_values": ["Ideogram_4.0"]},
    {"id": 134, "type": "f5f04613-helper", "mode": 0, "inputs": [], "outputs": [{"name": "STRING", "type": "STRING", "links": [252]}]},
    {"id": 111, "type": "PreviewAny", "mode": 0, "inputs": [{"name": "source", "type": "*", "link": 252}]},
    {"id": 100, "type": "MarkdownNote", "mode": 0, "widgets_values": ["Ideogram 4 is trained on structured JSON captions."]}
  ],
  "links": [[161, 37, 0, 98, 1, "INT"], [224, 98, 0, 158, 0, "IMAGE"], [252, 134, 0, 111, 0, "STRING"]],
  "definitions": {"subgraphs": [
    {"id": "83e6e004-pipeline", "name": "Text to Image (Ideogram v4)",
     "inputs": [{"name": "text", "type": "STRING"}, {"name": "value", "type": "INT"}],
     "outputs": [{"name": "IMAGE", "type": "IMAGE"}], "inputNode": {"id": -10}, "outputNode": {"id": -20},
     "nodes": [
       {"id": 23, "type": "UNETLoader", "mode": 0, "widgets_values": ["ideogram4_int8_convrot.safetensors", "default"]},
       {"id": 14, "type": "CLIPLoader", "mode": 0, "widgets_values": ["qwen3vl_8b_fp8_scaled.safetensors", "ideogram4", "default"]},
       {"id": 9, "type": "VAELoader", "mode": 0, "widgets_values": ["flux2-vae.safetensors"]},
       {"id": 24, "type": "CLIPTextEncode", "mode": 0, "inputs": [{"name": "clip", "type": "CLIP", "link": 1}, {"name": "text", "type": "STRING", "link": 2, "widget": {"name": "text"}}],
        "widgets_values": ["{\"high_level_description\": \"A knight on a horse, stone sculpture style\"}"]},
       {"id": 10, "type": "CLIPTextEncode", "mode": 0, "inputs": [{"name": "clip", "type": "CLIP", "link": 3}], "widgets_values": [""]},
       {"id": 27, "type": "PrimitiveInt", "mode": 0, "inputs": [{"name": "value", "type": "INT", "link": 4, "widget": {"name": "value"}}], "widgets_values": [1024, "fixed"]},
       {"id": 11, "type": "EmptyLatentImage", "mode": 0, "inputs": [{"name": "width", "type": "INT", "link": 5, "widget": {"name": "width"}}], "widgets_values": [1024, 1024, 1]},
       {"id": 12, "type": "KSampler", "mode": 0, "inputs": [
         {"name": "model", "type": "MODEL", "link": 6}, {"name": "positive", "type": "CONDITIONING", "link": 7},
         {"name": "negative", "type": "CONDITIONING", "link": 8}, {"name": "latent_image", "type": "LATENT", "link": 9}
       ], "widgets_values": [71584314815009, "randomize", 20, 4, "euler", "normal", 1]},
       {"id": 13, "type": "VAEDecode", "mode": 0, "inputs": [{"name": "samples", "type": "LATENT", "link": 10}, {"name": "vae", "type": "VAE", "link": 11}]}
     ],
     "links": [
       {"id": 1, "origin_id": 14, "origin_slot": 0, "target_id": 24, "target_slot": 0, "type": "CLIP"},
       {"id": 2, "origin_id": -10, "origin_slot": 0, "target_id": 24, "target_slot": 1, "type": "STRING"},
       {"id": 3, "origin_id": 14, "origin_slot": 0, "target_id": 10, "target_slot": 0, "type": "CLIP"},
       {"id": 4, "origin_id": -10, "origin_slot": 1, "target_id": 27, "target_slot": 0, "type": "INT"},
       {"id": 5, "origin_id": 27, "origin_slot": 0, "target_id": 11, "target_slot": 0, "type": "INT"},
       {"id": 6, "origin_id": 23, "origin_slot": 0, "target_id": 12, "target_slot": 0, "type": "MODEL"},
       {"id": 7, "origin_id": 24, "origin_slot": 0, "target_id": 12, "target_slot": 1, "type": "CONDITIONING"},
       {"id": 8, "origin_id": 10, "origin_slot": 0, "target_id": 12, "target_slot": 2, "type": "CONDITIONING"},
       {"id": 9, "origin_id": 11, "origin_slot": 0, "target_id": 12, "target_slot": 3, "type": "LATENT"},
       {"id": 10, "origin_id": 12, "origin_slot": 0, "target_id": 13, "target_slot": 0, "type": "LATENT"},
       {"id": 11, "origin_id": 9, "origin_slot": 0, "target_id": 13, "target_slot": 1, "type": "VAE"},
       {"id": 12, "origin_id": 13, "origin_slot": 0, "target_id": -20, "target_slot": 0, "type": "IMAGE"}
     ]},
    {"id": "f5f04613-helper", "name": "Ideogram4 Caption Prompt Template", "inputs": [], "outputs": [{"name": "STRING", "type": "STRING"}],
     "inputNode": {"id": -10}, "outputNode": {"id": -20},
     "nodes": [{"id": 163, "type": "StringReplace", "mode": 0, "widgets_values": ["User idea: {{original_prompt}} at {{width}}", "{{original_prompt}}", ""]}],
     "links": [{"id": 1, "origin_id": 163, "origin_slot": 0, "target_id": -20, "target_slot": 0, "type": "STRING"}]}
  ]}
}"#;

/// Ideogram v4 through Comfy.org (a partner node), as the library ships it.
const TEMPLATE_IDEOGRAM_API: &str = r#"{
  "nodes": [
    {"id": 6, "type": "SaveImage", "mode": 0, "inputs": [{"name": "images", "type": "IMAGE", "link": 3}], "widgets_values": ["ideogram_v4"]},
    {"id": 7, "type": "IdeogramV4", "mode": 0, "outputs": [{"name": "IMAGE", "type": "IMAGE", "links": [3]}],
     "widgets_values": ["{\"high_level_description\": \"A vintage train in the clouds\"}", "Auto", "DEFAULT", 0, "randomize"]},
    {"id": 8, "type": "MarkdownNote", "mode": 0, "widgets_values": ["Partner node: needs a Comfy.org account."]},
    {"id": 18, "type": "GeminiNodeV3", "mode": 4, "widgets_values": ["Gemini", "{{aspect_ratio}} {{original_prompt}}"]}
  ],
  "links": [[3, 7, 0, 6, 0, "IMAGE"]]
}"#;

/// Image to 3D with a local model, shaped like the Pixal3D template's ends: Load Image in, a model
/// loader, a preview, and a saved GLB.
const TEMPLATE_PIXAL3D: &str = r#"{
  "nodes": [
    {"id": 122, "type": "LoadImage", "mode": 0, "outputs": [{"name": "IMAGE", "type": "IMAGE", "links": [1, 4]}, {"name": "MASK", "type": "MASK", "links": []}], "widgets_values": ["viking_wolf_rune_axe.png", "image"]},
    {"id": 319, "type": "UNETLoader", "mode": 0, "outputs": [{"name": "MODEL", "type": "MODEL", "links": [2]}], "widgets_values": ["pixal3d_int8_convrot.safetensors", "default"]},
    {"id": 2, "type": "ImageToMesh", "mode": 0, "inputs": [{"name": "image", "type": "IMAGE", "link": 1}, {"name": "model", "type": "MODEL", "link": 2}], "outputs": [{"name": "MESH", "type": "MESH", "links": [3]}], "widgets_values": [30]},
    {"id": 322, "type": "SaveGLB", "mode": 0, "inputs": [{"name": "mesh", "type": "MESH", "link": 3}], "widgets_values": ["3d/ComfyUI"]},
    {"id": 164, "type": "PreviewImage", "mode": 0, "title": "Input", "inputs": [{"name": "images", "type": "IMAGE", "link": 4}]}
  ],
  "links": [[1, 122, 0, 2, 0, "IMAGE"], [2, 319, 0, 2, 1, "MODEL"], [3, 2, 0, 322, 0, "MESH"], [4, 122, 0, 164, 0, "IMAGE"]]
}"#;

/// Needs a model file this server doesn't have.
const TEMPLATE_FLUX_MISSING_MODEL: &str = r#"{
  "nodes": [
    {"id": 1, "type": "UNETLoader", "mode": 0, "outputs": [{"name": "MODEL", "type": "MODEL", "links": [1]}], "widgets_values": ["flux1-dev.safetensors", "default"]},
    {"id": 2, "type": "SaveImage", "mode": 0, "inputs": [{"name": "images", "type": "IMAGE", "link": 1}], "widgets_values": ["flux"]}
  ],
  "links": [[1, 1, 0, 2, 0, "MODEL"]]
}"#;

/// Needs a node type this server doesn't have.
const TEMPLATE_TRIPO_MISSING_NODE: &str = r#"{
  "nodes": [
    {"id": 1, "type": "LoadImage", "mode": 0, "widgets_values": ["chair.png", "image"]},
    {"id": 2, "type": "TripoImageToModelNode", "mode": 0, "inputs": [{"name": "image", "type": "IMAGE", "link": 1}]}
  ],
  "links": [[1, 1, 0, 2, 0, "IMAGE"]]
}"#;
