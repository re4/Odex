//! The ComfyUI client against the mock server.

use std::collections::BTreeMap;
use std::time::Duration;

use odex_comfyui::{ComfyClient, Inputs, Workflow};
use odex_mock_vllm::comfy::MockComfy;
use serde_json::json;

fn workflow() -> Workflow {
    Workflow::parse(
        "test".into(),
        r#"{"6": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}"}},
            "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "odex"}}}"#,
    )
    .unwrap()
}

#[tokio::test]
async fn runs_a_workflow_and_downloads_outputs() {
    let m = MockComfy::start().await;
    let c = ComfyClient::new(&format!("{}/", m.url));
    let stats = c.system_stats().await.unwrap();
    assert_eq!(ComfyClient::version(&stats).as_deref(), Some("0.3.60-mock"));

    m.set_outputs(json!({
        "9": {"images": [{"filename": "ComfyUI_00001_.png", "subfolder": "", "type": "output"}]},
        "12": {"3d": [{"filename": "mesh.glb", "subfolder": "mesh", "type": "output"}]}
    }));
    let graph = workflow().fill(&Inputs { prompt: Some("a lighthouse".into()), ..Default::default() }).unwrap();
    let id = c.queue(&graph).await.unwrap();
    assert_eq!(m.prompts()[0]["6"]["inputs"]["text"], "a lighthouse");
    let files = c.wait(&id, Duration::from_secs(10)).await.unwrap();
    assert_eq!(files.len(), 2);
    let png = c.download(&files[0]).await.unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    let glb = c.download(&files[1]).await.unwrap();
    assert!(glb.starts_with(b"glTF"));

    let name = c.upload_image("cat.png", vec![1, 2, 3]).await.unwrap();
    assert_eq!(name, "cat.png");
    assert_eq!(m.uploads(), vec!["cat.png"]);
}

#[tokio::test]
async fn surfaces_failures() {
    let m = MockComfy::start().await;
    let c = ComfyClient::new(&m.url);
    let graph = workflow().fill(&Inputs { prompt: Some("x".into()), ..Default::default() }).unwrap();

    m.fail_with("KSampler", "CUDA out of memory");
    let id = c.queue(&graph).await.unwrap();
    let e = c.wait(&id, Duration::from_secs(10)).await.unwrap_err().to_string();
    assert_eq!(e, "ComfyUI: KSampler failed: CUDA out of memory");

    m.reject_with(json!({"error": {"message": "Prompt outputs failed validation", "details": ""}, "node_errors": {}}));
    let e = c.queue(&graph).await.unwrap_err().to_string();
    assert!(e.contains("Prompt outputs failed validation"), "{e}");

    let gone = ComfyClient::new("http://127.0.0.1:9");
    assert!(gone.system_stats().await.unwrap_err().to_string().contains("cannot reach ComfyUI"));
}

#[tokio::test]
async fn times_out_and_cancels() {
    let m = MockComfy::start().await;
    m.never_finish();
    let c = ComfyClient::new(&m.url);
    let graph = workflow().fill(&Inputs { prompt: Some("x".into()), ..Default::default() }).unwrap();
    let id = c.queue(&graph).await.unwrap();
    let e = c.wait(&id, Duration::from_millis(100)).await.unwrap_err().to_string();
    assert!(e.contains("did not finish"), "{e}");
    c.cancel(&id).await;
    assert_eq!(m.deleted(), vec![id]);
}

#[tokio::test]
async fn sends_the_api_key() {
    let m = MockComfy::start().await;
    m.require_header("authorization", "Bearer s3cret");
    let none = BTreeMap::new();
    let e = ComfyClient::new(&m.url).system_stats().await.unwrap_err().to_string();
    assert_eq!(e, "HTTP 401 Unauthorized: the server needs an API key");
    let wrong = ComfyClient::with_auth(&m.url, Some("nope"), None, &none).unwrap();
    assert_eq!(
        wrong.system_stats().await.unwrap_err().to_string(),
        "HTTP 401 Unauthorized: the server rejected the API key"
    );
    let graph = workflow().fill(&Inputs { prompt: Some("x".into()), ..Default::default() }).unwrap();
    assert_eq!(
        wrong.queue(&graph).await.unwrap_err().to_string(),
        "HTTP 401 Unauthorized: the server rejected the API key"
    );

    // every call carries it: stats, upload, queue, history, view
    let c = ComfyClient::with_auth(&m.url, Some(" s3cret "), None, &none).unwrap();
    c.system_stats().await.unwrap();
    c.upload_image("in.png", vec![1]).await.unwrap();
    let id = c.queue(&graph).await.unwrap();
    let files = c.wait(&id, Duration::from_secs(10)).await.unwrap();
    assert!(c.download(&files[0]).await.unwrap().starts_with(b"\x89PNG"));
    // a pasted "Bearer ..." isn't doubled
    ComfyClient::with_auth(&m.url, Some("Bearer s3cret"), None, &none).unwrap().system_stats().await.unwrap();

    // the key as-is in a named header
    m.require_header("x-api-key", "s3cret");
    ComfyClient::with_auth(&m.url, Some("s3cret"), Some("X-API-Key"), &none).unwrap().system_stats().await.unwrap();
    // extra headers (e.g. Cloudflare Access service tokens)
    m.require_header("cf-access-client-id", "abc");
    let extra = BTreeMap::from([("CF-Access-Client-Id".to_string(), "abc".to_string())]);
    ComfyClient::with_auth(&m.url, None, None, &extra).unwrap().system_stats().await.unwrap();

    assert!(ComfyClient::with_auth(&m.url, Some("k"), Some("bad header"), &none).is_err());
    assert!(ComfyClient::with_auth(&m.url, Some("line\nbreak"), None, &none).is_err());
}

#[tokio::test]
async fn reads_saved_workflows_and_converts_them() {
    let m = MockComfy::start().await;
    let c = ComfyClient::new(&m.url);
    assert_eq!(c.server_workflows().await.unwrap(), vec!["3d/image to mesh.json", "txt2img.json"]);
    let info = c.object_info().await.unwrap();
    // a subfolder and a space in the name: one percent-encoded path segment
    let mesh = c.server_workflow("3d/image to mesh.json").await.unwrap();
    let api = odex_comfyui::ui_format::to_api(&mesh, &info).unwrap();
    assert_eq!(api["2"]["inputs"], json!({"image": ["1", 0], "steps": 30}));
    assert_eq!(api["1"]["inputs"], json!({"image": "chair.png"}));

    let txt2img = odex_comfyui::ui_format::to_api(&c.server_workflow("txt2img.json").await.unwrap(), &info).unwrap();
    let id = c.queue(&txt2img).await.unwrap();
    assert_eq!(m.prompts()[0]["3"]["inputs"]["positive"], json!(["6", 0]));
    assert_eq!(c.wait(&id, Duration::from_secs(10)).await.unwrap()[0].filename, "ComfyUI_00001_.png");
    assert!(c.server_workflow("missing.json").await.is_err());
}

#[tokio::test]
async fn lists_saved_workflows_old_and_new_api() {
    let m = MockComfy::start().await;
    let c = ComfyClient::new(&m.url);
    let both = vec!["3d/image to mesh.json", "txt2img.json"];
    // a server (or proxy) without /userdata: the /v2/userdata listing, folders included
    m.v2_userdata_only();
    assert_eq!(c.server_workflows().await.unwrap(), both);
    // a failing listing is an error, not an empty list
    m.break_userdata();
    let e = c.server_workflows().await.unwrap_err().to_string();
    assert_eq!(e, "HTTP 500 Internal Server Error: 500 Internal Server Error");
    // nothing saved yet: ComfyUI answers 404 "Directory not found"
    let empty = MockComfy::start().await;
    empty.clear_saved();
    assert!(ComfyClient::new(&empty.url).server_workflows().await.unwrap().is_empty());
}
