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

#[tokio::test]
async fn uses_workflows_saved_on_the_server() {
    let comfy = MockComfy::start().await;
    let h = harness(32768, &format!("[comfyui]\nurl = \"{}\"\n", comfy.url)).await;
    let s = api::comfy_status(&h.engine).await;
    assert_eq!(s.server_workflows, vec!["3d/image to mesh.json", "txt2img.json"]);

    let import = |path: &str, role: Option<&str>| {
        api::comfy_import_server(&h.engine, ComfyImportServerParams { path: path.into(), role: role.map(String::from) })
    };
    let s = import("txt2img.json", Some("image")).await.unwrap();
    assert_eq!(s.image_workflow.as_deref(), Some("txt2img"));
    assert_eq!(s.workflows.iter().find(|w| w.name == "txt2img").unwrap().placeholders, vec!["prompt"]);
    let s = import("3d/image to mesh.json", Some("model3d")).await.unwrap();
    assert_eq!(s.model3d_workflow.as_deref(), Some("image to mesh"));
    assert_eq!(s.workflows.iter().find(|w| w.name == "image to mesh").unwrap().placeholders, vec!["image"]);
    assert!(import("txt2img.json", Some("video")).await.unwrap_err().to_string().contains("unknown role"));
    assert!(import("nope.json", None).await.is_err());

    // the agent's prompt goes into the positive prompt box; the negative one is kept
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "A red fox"})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "draw a fox").await;
    let g = &comfy.prompts()[0];
    assert_eq!(g["6"]["inputs"]["text"], "A red fox");
    assert_eq!(g["7"]["inputs"]["text"], "text, watermark");
    assert_eq!(g["3"]["inputs"]["positive"], json!(["6", 0]));
    assert!(h.work.path().join("generated/a-red-fox.png").exists());

    // image to 3D: the agent's image goes into Load Image
    comfy.set_outputs(json!({"3": {"3d": [{"filename": "odex_00001_.glb", "subfolder": "mesh", "type": "output"}]}}));
    std::fs::write(h.work.path().join("chair.png"), b"\x89PNG fake").unwrap();
    h.server.push(MockReply::tool("generate_3d", json!({"image": "chair.png", "path": "chair.glb"})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "make it 3d").await;
    assert_eq!(comfy.prompts()[1]["1"]["inputs"]["image"], "chair.png");
    assert!(std::fs::read(h.work.path().join("chair.glb")).unwrap().starts_with(b"glTF"));

    // a saved (not API) workflow file is converted on import too
    let ui = odex_comfyui::ComfyClient::new(&comfy.url).server_workflow("txt2img.json").await.unwrap();
    let file = h.work.path().join("from disk.json");
    std::fs::write(&file, ui.to_string()).unwrap();
    let s = api::comfy_import(&h.engine, PathParams { path: file.to_string_lossy().into() }).await.unwrap();
    assert_eq!(s.workflows.iter().find(|w| w.name == "from disk").unwrap().placeholders, vec!["prompt"]);
}

#[tokio::test]
async fn reports_why_saved_workflows_are_missing() {
    let comfy = MockComfy::start().await;
    let h = harness(32768, &format!("[comfyui]\nurl = \"{}\"\n", comfy.url)).await;
    comfy.clear_saved();
    let s = api::comfy_status(&h.engine).await;
    assert!(s.reachable && s.server_workflows.is_empty() && s.server_workflows_error.is_none());
    comfy.break_userdata();
    let s = api::comfy_status(&h.engine).await;
    assert!(s.reachable, "the server itself is up");
    assert_eq!(s.server_workflows_error.as_deref(), Some("HTTP 500 Internal Server Error: 500 Internal Server Error"));
}

#[tokio::test]
async fn partner_nodes_get_the_comfy_org_key() {
    let comfy = MockComfy::start().await;
    comfy.require_comfy_org_key("comfyui-org-key");
    let h = harness(32768, &format!("[comfyui]\nurl = \"{}\"\nimage_workflow = \"ideogram\"\n", comfy.url)).await;
    // an Ideogram workflow exported as it is: its plain `prompt` input becomes {{prompt}}
    let file = h.work.path().join("ideogram.json");
    let graph = json!({
        "1": {"class_type": "IdeogramV4", "inputs": {"prompt": "a cat", "aspect_ratio": "1:1", "seed": 0}},
        "9": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "ideogram"}}
    });
    std::fs::write(&file, graph.to_string()).unwrap();
    let s = api::comfy_import(&h.engine, PathParams { path: file.to_string_lossy().into() }).await.unwrap();
    assert!(!s.has_comfy_org_key);
    assert_eq!(s.workflows.iter().find(|w| w.name == "ideogram").unwrap().placeholders, vec!["prompt"]);

    // without the key the partner node can't sign in, and the agent is told why
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "a lighthouse"})));
    h.server.push(MockReply::text("It failed."));
    h.run(&id, "draw a lighthouse").await;
    let result = h.server.requests()[1].body["messages"].to_string();
    assert!(result.contains("needs a Comfy.org API key"), "{result}");
    assert!(comfy.extra_data()[0].is_null());

    // with the key (the desktop's encrypted store) it goes along as extra_data
    api::secrets_set(
        &h.engine,
        SecretsStoreParams { key: api::COMFY_ORG_KEY_SECRET.into(), value: Some("comfyui-org-key".into()) },
    );
    assert!(api::comfy_status(&h.engine).await.has_comfy_org_key);
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "a lighthouse"})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "again").await;
    assert_eq!(comfy.extra_data()[1], json!({"api_key_comfy_org": "comfyui-org-key"}));
    // the IdeogramV4 node gets a JSON caption
    assert_eq!(comfy.prompts()[1]["1"]["inputs"]["prompt"], r#"{"high_level_description":"a lighthouse"}"#);
    assert!(h.work.path().join("generated/a-lighthouse.png").exists());
}

#[tokio::test]
async fn lists_and_uses_comfyui_templates() {
    let comfy = MockComfy::start().await;
    let h = harness(32768, &format!("[comfyui]\nurl = \"{}\"\n", comfy.url)).await;
    let t = api::comfy_templates(&h.engine).await;
    assert!(t.error.is_none(), "{:?}", t.error);
    let titles = |v: &[ComfyTemplate]| v.iter().map(|t| t.title.clone()).collect::<Vec<_>>();
    // Odex's workflows for the installed node packs first, then the library's templates
    assert_eq!(
        titles(&t.image),
        vec![
            "Ideogram 4.0 (ComfyUI-Ideogram4 nodes)",
            "Ideogram v4 Int8: Text to Image",
            "Ideogram v4: Text to Image (API)"
        ]
    );
    assert!(!t.image[0].partner && !t.image[1].partner && t.image[2].partner);
    assert_eq!(titles(&t.model3d), vec!["Pixal3D (ComfyUI_RH_Pixal3D nodes)", "Pixal3D & TRELLIS.2: Image to Model"]);
    let missing: Vec<(String, Vec<String>)> =
        t.unavailable.iter().map(|u| (u.title.clone(), u.missing.clone())).collect();
    assert_eq!(
        missing,
        vec![
            ("Flux.1 Dev: Text to Image".to_string(), vec!["flux1-dev.safetensors".to_string()]),
            ("Tripo: Image to Model".to_string(), vec!["node TripoImageToModelNode".to_string()])
        ]
    );

    // the local Ideogram for images, Pixal3D for 3D
    let use_template = |name: &str, role: &str| {
        api::comfy_use_template(&h.engine, ComfyUseTemplateParams { name: name.into(), role: role.into() })
    };
    let s = use_template("image_ideogram4_t2i_int8", "image").await.unwrap();
    assert_eq!(s.image_workflow.as_deref(), Some("Ideogram v4 Int8 Text to Image"));
    let s = use_template("3d_pixal3d_trellis2_image_to_model", "model3d").await.unwrap();
    assert_eq!(s.model3d_workflow.as_deref(), Some("Pixal3D & TRELLIS.2 Image to Model"));
    let placeholders = |name: &str| s.workflows.iter().find(|w| w.name == name).unwrap().placeholders.clone();
    assert_eq!(placeholders("Ideogram v4 Int8 Text to Image"), vec!["prompt"]);
    assert_eq!(placeholders("Pixal3D & TRELLIS.2 Image to Model"), vec!["image"]);

    // the prompt goes into the text encoder inside the subgraph; the preview branch isn't queued
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "a dog in a red scarf"})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "generate me a dog image").await;
    let g = &comfy.prompts()[0];
    // Ideogram 4's local pipeline: a JSON caption in the text encoder inside the subgraph
    assert_eq!(g["98:24"]["inputs"]["text"], r#"{"high_level_description":"a dog in a red scarf"}"#);
    assert_eq!(g["98:11"]["inputs"]["width"], json!(["98:27", 0]));
    assert_eq!(g["98:27"]["inputs"]["value"], json!(["37", 0]));
    assert!(g.get("111").is_none() && g.get("134:163").is_none(), "no preview branch: {g}");
    assert!(h.work.path().join("generated/a-dog-in-a-red-scarf.png").exists());

    // image to 3D: the agent's image goes into Load Image; the preview of it isn't queued
    comfy
        .set_outputs(json!({"322": {"3d": [{"filename": "ComfyUI_00001_.glb", "subfolder": "3d", "type": "output"}]}}));
    std::fs::write(h.work.path().join("axe.png"), b"\x89PNG fake").unwrap();
    h.server.push(MockReply::tool("generate_3d", json!({"image": "axe.png", "path": "axe.glb"})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "make it 3d").await;
    let g = &comfy.prompts()[1];
    assert_eq!(g["122"]["inputs"]["image"], "axe.png");
    assert_eq!(g["319"]["inputs"]["unet_name"], "pixal3d_int8_convrot.safetensors");
    assert!(g.get("164").is_none());
    assert!(std::fs::read(h.work.path().join("axe.glb")).unwrap().starts_with(b"glTF"));

    // a template that can't run says what it needs
    let e = use_template("flux_dev_full_text_to_image", "image").await.unwrap_err();
    assert!(e.to_string().contains("it needs flux1-dev.safetensors"), "{e}");

    // the node-pack workflows: Ideogram 4.0 and Pixal3D with the nodes' own defaults
    let s = use_template("odex:ideogram4", "image").await.unwrap();
    assert_eq!(s.image_workflow.as_deref(), Some("Ideogram 4.0 (ComfyUI-Ideogram4 nodes)"));
    let s = use_template("odex:pixal3d", "model3d").await.unwrap();
    assert_eq!(s.model3d_workflow.as_deref(), Some("Pixal3D (ComfyUI_RH_Pixal3D nodes)"));
    comfy.set_outputs(
        json!({"3": {"images": [{"filename": "odex_00001_.png", "subfolder": "odex", "type": "output"}]}}),
    );
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "a lighthouse", "width": 768})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "draw a lighthouse").await;
    let g = &comfy.prompts()[2];
    assert_eq!(g["1"]["inputs"]["model_weights"], "4.0 NF4");
    // Ideogram 4 reads JSON captions: plain text becomes the high-level description
    assert_eq!(g["2"]["inputs"]["prompt"], r#"{"high_level_description":"a lighthouse"}"#);
    assert_eq!((g["2"]["inputs"]["width"].as_u64(), g["2"]["inputs"]["height"].as_u64()), (Some(768), Some(1024)));
    assert!(g["2"]["inputs"]["seed"].as_u64().unwrap() < 1 << 31);
    assert!(h.work.path().join("generated/a-lighthouse.png").exists());
    let spec = h.server.requests().last().unwrap().body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["function"]["name"] == "generate_image")
        .cloned()
        .unwrap();
    assert!(spec["function"]["description"]
        .as_str()
        .unwrap()
        .contains("Ideogram 4, which reads structured JSON captions"));

    // a caption the agent wrote goes as it is; the file is named after its description
    let caption =
        r#"{"high_level_description": "A scythe leaning on a barn wall", "style_description": {"medium": "photo"}}"#;
    h.server.push(MockReply::tool("generate_image", json!({"prompt": caption})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "draw a scythe on a farm").await;
    let sent: serde_json::Value =
        serde_json::from_str(comfy.prompts()[3]["2"]["inputs"]["prompt"].as_str().unwrap()).unwrap();
    assert_eq!(sent["style_description"]["medium"], "photo");
    assert!(h.work.path().join("generated/a-scythe-leaning-on-a-barn.png").exists());

    // a refusal (a flat gray image) isn't saved; the agent is told to rewrite the prompt
    comfy.serve_gray_images();
    h.server.push(MockReply::tool("generate_image", json!({"prompt": "scythe"})));
    h.server.push(MockReply::text("I'll describe it more fully."));
    h.run(&id, "draw a scythe").await;
    let result = h.server.requests().last().unwrap().body["messages"].to_string();
    assert!(result.contains("its safety filter refused this prompt"), "{result}");
    assert!(!h.work.path().join("generated/scythe.png").exists());

    comfy.set_outputs(json!({"4": {"3d": [{"filename": "Pixal3D_00001_.glb", "subfolder": "3d", "type": "output"}]}}));
    h.server.push(MockReply::tool("generate_3d", json!({"image": "axe.png", "path": "axe2.glb"})));
    h.server.push(MockReply::text("Done."));
    h.run(&id, "make it 3d again").await;
    let g = &comfy.prompts()[5];
    assert_eq!(g["2"]["inputs"]["image"], "axe.png");
    assert_eq!(g["3"]["inputs"]["pipe"], json!(["1", 0]));
    assert!(g["3"]["inputs"]["seed"].as_u64().unwrap() < 1 << 31, "Pixal3D caps its seed at 2^31 - 1");
    assert!(std::fs::read(h.work.path().join("axe2.glb")).unwrap().starts_with(b"glTF"));
}
