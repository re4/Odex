//! Removing models from the list, and ComfyUI image / 3D generation tools.

mod common;

use common::*;
use odex_core::api;
use odex_mock_vllm::comfy::MockComfy;
use odex_mock_vllm::MockReply;
use odex_protocol::*;
use serde_json::json;

fn keys(r: &ModelListResponse) -> Vec<String> {
    r.models.iter().map(|m| m.key.clone()).collect()
}

#[tokio::test]
async fn remove_configured_and_discovered_models() {
    let h =
        harness(32768, "[roles]\ncompactor = \"small\"\n[models.small]\nprovider = \"mock\"\nmodel = \"mock-small\"\n")
            .await;
    let before = api::model_list(&h.engine);
    assert_eq!(keys(&before), vec!["small", "mock:mock-coder"]);

    // a configured model: its entry and the role that used it go away
    let r = api::model_remove(&h.engine, ModelRemoveParams { key: "small".into() }).unwrap();
    assert_eq!(keys(&r), vec!["mock:mock-coder"]);
    assert!(r.hidden.is_empty(), "nothing served it, so nothing to hide");
    assert!(!r.roles.contains_key("compactor"));
    let user = api::config_read(&h.engine).user;
    assert!(user.models.is_empty());

    // a discovered model: hidden, and `model = ...` no longer points at it
    let r = api::model_remove(&h.engine, ModelRemoveParams { key: "mock:mock-coder".into() }).unwrap();
    assert!(r.models.is_empty());
    assert_eq!(r.hidden, vec!["mock:mock-coder"]);
    assert_eq!(api::config_read(&h.engine).user.model, None);
    assert!(h.engine.registry.resolve("mock-coder").is_none(), "a bare id skips hidden models");

    // restoring is just dropping it from hidden_models
    api::config_write(
        &h.engine,
        ConfigWriteParams {
            edits: vec![ConfigEdit { key_path: "hidden_models".into(), value: json!([]) }],
            project_path: None,
        },
    )
    .unwrap();
    assert_eq!(keys(&api::model_list(&h.engine)), vec!["mock:mock-coder"]);
    assert!(api::model_remove(&h.engine, ModelRemoveParams { key: "nope".into() }).is_err());
}

const TXT2IMG: &str = r#"{
  "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}", "clip": ["4", 1]}},
  "3": {"class_type": "KSampler", "inputs": {"seed": 1, "model": ["4", 0]}},
  "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "odex", "images": ["8", 0]}}
}"#;

const IMG2MESH: &str = r#"{
  "1": {"class_type": "LoadImage", "inputs": {"image": "{{image}}"}},
  "2": {"class_type": "SaveGLB", "inputs": {"filename_prefix": "mesh", "mesh": ["1", 0]}}
}"#;

async fn comfy_harness(comfy: &MockComfy) -> Harness {
    let cfg =
        format!("[comfyui]\nurl = \"{}\"\nimage_workflow = \"flux\"\nmodel3d_workflow = \"trellis\"\n", comfy.url);
    let h = harness(32768, &cfg).await;
    let dir = h.home.path().join("comfyui");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("flux.json"), TXT2IMG).unwrap();
    std::fs::write(dir.join("trellis.json"), IMG2MESH).unwrap();
    h
}

fn tool_names(body: &serde_json::Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["function"]["name"].as_str().map(String::from))
        .collect()
}

#[tokio::test]
async fn generate_image_saves_into_the_workspace() {
    let comfy = MockComfy::start().await;
    let h = comfy_harness(&comfy).await;
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "A red fox, watercolor"})));
    h.server.push(MockReply::text("Saved it."));
    h.run(&id, "draw a fox").await;

    let reqs = h.server.requests();
    let names = tool_names(&reqs[0].body);
    assert!(names.contains(&"generate_image".to_string()) && names.contains(&"generate_3d".to_string()), "{names:?}");

    assert_eq!(comfy.prompts()[0]["6"]["inputs"]["text"], "A red fox, watercolor");
    let file = h.work.path().join("generated/a-red-fox-watercolor.png");
    assert!(std::fs::read(&file).unwrap().starts_with(b"\x89PNG"));
    let result = reqs[1].body["messages"].to_string();
    assert!(result.contains("saved generated/a-red-fox-watercolor.png"), "{result}");
    assert!(h.sink.items().iter().any(|i| matches!(i,
        ThreadItem::ImageView { path, prompt: Some(p), .. } if path == "generated/a-red-fox-watercolor.png" && p == "A red fox, watercolor")));

    // the same prompt again does not overwrite the first image
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "A red fox, watercolor"})));
    h.server.push(MockReply::text("Again."));
    h.run(&id, "again").await;
    assert!(h.work.path().join("generated/a-red-fox-watercolor-2.png").exists());
}

#[tokio::test]
async fn generate_3d_uploads_the_input_image() {
    let comfy = MockComfy::start().await;
    comfy.set_outputs(json!({"2": {"3d": [{"filename": "mesh_00001_.glb", "subfolder": "mesh", "type": "output"}]}}));
    let h = comfy_harness(&comfy).await;
    std::fs::write(h.work.path().join("cat.png"), b"\x89PNG fake").unwrap();
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::tool("generate_3d", json!({"image": "cat.png", "path": "assets/cat.glb"})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "make a 3d cat").await;

    let spec = h.server.requests()[0].body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "generate_3d")
        .cloned()
        .unwrap();
    assert_eq!(spec["function"]["parameters"]["required"], json!(["image"]));
    assert_eq!(comfy.uploads(), vec!["cat.png"]);
    assert_eq!(comfy.prompts()[0]["1"]["inputs"]["image"], "cat.png");
    assert!(std::fs::read(h.work.path().join("assets/cat.glb")).unwrap().starts_with(b"glTF"));
}

#[tokio::test]
async fn generation_errors_reach_the_model() {
    let comfy = MockComfy::start().await;
    comfy.fail_with("KSampler", "CUDA out of memory");
    let h = comfy_harness(&comfy).await;
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "a cat"})));
    h.server.push(MockReply::text("It failed."));
    h.run(&id, "draw a cat").await;
    let result = h.server.requests()[1].body["messages"].to_string();
    assert!(result.contains("ComfyUI: KSampler failed: CUDA out of memory"), "{result}");
    assert!(!h.work.path().join("generated").exists());
}

#[tokio::test]
async fn comfy_api_key_from_the_secret_store_or_config() {
    let comfy = MockComfy::start().await;
    comfy.require_header("authorization", "Bearer s3cret");
    let h = comfy_harness(&comfy).await;
    let s = api::comfy_status(&h.engine).await;
    assert!(!s.reachable && !s.has_api_key);
    assert_eq!(s.error.as_deref(), Some("HTTP 401 Unauthorized: the server needs an API key"));

    // the key the desktop stored encrypted and pushed with secrets/set
    api::secrets_set(
        &h.engine,
        SecretsStoreParams { key: api::COMFY_API_KEY_SECRET.into(), value: Some("s3cret".into()) },
    );
    let s = api::comfy_status(&h.engine).await;
    assert!(s.reachable && s.has_api_key, "{:?}", s.error);

    // generation sends it too
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "a key"})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "draw a key").await;
    assert!(h.work.path().join("generated/a-key.png").exists());

    // without the secret: `api_key` from config.toml, in a custom header
    api::secrets_set(&h.engine, SecretsStoreParams { key: api::COMFY_API_KEY_SECRET.into(), value: None });
    comfy.require_header("x-api-key", "from-config");
    api::config_write(
        &h.engine,
        ConfigWriteParams {
            edits: vec![
                ConfigEdit { key_path: "comfyui.api_key".into(), value: json!("from-config") },
                ConfigEdit { key_path: "comfyui.api_key_header".into(), value: json!("X-API-Key") },
            ],
            project_path: None,
        },
    )
    .unwrap();
    let s = api::comfy_status(&h.engine).await;
    assert!(s.reachable && s.has_api_key, "{:?}", s.error);
    assert_eq!(s.api_key_header.as_deref(), Some("X-API-Key"));
}
