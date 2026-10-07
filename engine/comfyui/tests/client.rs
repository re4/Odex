//! The ComfyUI client against the mock server.

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
