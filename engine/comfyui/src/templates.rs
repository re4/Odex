//! ComfyUI's template library (`/templates/index.json`, what "Browse Templates" shows): which
//! text-to-image and image-to-3D templates this server can run, with every node type installed and
//! every model file present, converted to API format with their placeholders added.

use serde_json::Value;

use crate::ui_format;
use crate::workflow::{add_placeholders, Kind};

/// A template from the library index.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    /// File name without `.json`, e.g. `api_ideogram_v4_t2i`.
    pub name: String,
    /// e.g. "Ideogram v4: Text to Image (API)"
    pub title: String,
    pub kind: Kind,
    /// Model families the index lists, e.g. `["Ideogram"]`.
    pub models: Vec<String>,
    /// Runs on a partner's cloud through Comfy.org (needs a Comfy.org key and credits), not this GPU.
    pub partner: bool,
}

/// A template this server can't run, and what it lacks.
#[derive(Debug, Clone, PartialEq)]
pub struct Unavailable {
    pub title: String,
    /// `node <type>` for node types that aren't installed, else model files (or why it can't be converted).
    pub missing: Vec<String>,
}

/// Node types that only exist in ComfyUI's page.
const FRONTEND_ONLY: &[&str] = &["Note", "MarkdownNote", "Reroute", "PrimitiveNode", "GetNode", "SetNode"];

/// Text-to-image templates (an `image` category, tagged "Text to Image", no input image) and
/// image-to-3D ones (a `3d` category, tagged "Image to 3D"), in index order.
pub fn candidates(index: &Value) -> Vec<Template> {
    let mut out: Vec<Template> = Vec::new();
    for category in index.as_array().into_iter().flatten() {
        let ty = category["type"].as_str().unwrap_or_default();
        for t in category["templates"].as_array().into_iter().flatten() {
            let tags: Vec<&str> = t["tags"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let takes_image = t["io"]["inputs"].as_array().into_iter().flatten().any(|i| i["mediaType"] == "image");
            let kind = match ty {
                "image" if tags.contains(&"Text to Image") && !takes_image => Kind::Image,
                "3d" if tags.contains(&"Image to 3D") => Kind::Model3d,
                _ => continue,
            };
            let (Some(name), Some(title)) = (t["name"].as_str(), t["title"].as_str()) else { continue };
            if out.iter().any(|o| o.name == name) {
                continue;
            }
            let models =
                t["models"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(String::from)).collect();
            let partner = tags.contains(&"API") || t["openSource"] == Value::Bool(false);
            out.push(Template { name: name.into(), title: title.into(), kind, models, partner });
        }
    }
    out
}

/// Extensions of model files, the dropdown values a missing download shows up in.
const MODEL_FILES: &[&str] = &[".safetensors", ".sft", ".ckpt", ".pt", ".pth", ".bin", ".gguf", ".onnx"];

/// A template converted for this server (API format, placeholders added), or what the server lacks:
/// node types that aren't installed, then model files its loaders don't offer.
pub fn prepare(ui: &Value, object_info: &Value, kind: Kind) -> Result<Value, Vec<String>> {
    let mut missing: Vec<String> = Vec::new();
    // the workflow's nodes and those inside its subgraphs; a subgraph instance's type is the subgraph's id
    let subgraphs: Vec<&Value> = ui["definitions"]["subgraphs"].as_array().into_iter().flatten().collect();
    let ids: Vec<&str> = subgraphs.iter().filter_map(|s| s["id"].as_str()).collect();
    let nodes =
        [ui].into_iter().chain(subgraphs.iter().copied()).flat_map(|g| g["nodes"].as_array().into_iter().flatten());
    for n in nodes {
        let ty = n["type"].as_str().unwrap_or_default();
        // muted (2) and bypassed (4) nodes don't run
        let off = matches!(n["mode"].as_u64(), Some(2 | 4));
        if off || ids.contains(&ty) || FRONTEND_ONLY.contains(&ty) || object_info.get(ty).is_some() {
            continue;
        }
        let m = format!("node {ty}");
        if !missing.contains(&m) {
            missing.push(m);
        }
    }
    if !missing.is_empty() {
        return Err(missing);
    }
    let mut graph = ui_format::to_api(ui, object_info).map_err(|e| vec![e])?;
    add_placeholders(&mut graph, kind);
    for node in graph.as_object().into_iter().flatten().map(|(_, n)| n) {
        let def = &object_info[node["class_type"].as_str().unwrap_or_default()];
        for (name, spec) in ui_format::input_specs(def) {
            let Some(options) = combo_options(&spec) else { continue };
            // only model files: other dropdowns (presets, modes) can be filled in by the page
            let Some(value) = node["inputs"][&name].as_str().filter(|v| {
                let v = v.to_lowercase();
                MODEL_FILES.iter().any(|ext| v.ends_with(ext))
            }) else {
                continue;
            };
            if !options.iter().any(|o| o == value) && !missing.iter().any(|m| m == value) {
                missing.push(value.to_string());
            }
        }
    }
    if missing.is_empty() {
        Ok(graph)
    } else {
        Err(missing)
    }
}

/// A dropdown's choices: `[[...], {...}]`, or `["COMBO", {"options": [...]}]` in newer servers.
/// `None` for other inputs, and for lists the page fetches itself (`remote`).
fn combo_options(spec: &Value) -> Option<Vec<String>> {
    if spec[1].get("remote").is_some() {
        return None;
    }
    let list = match &spec[0] {
        Value::Array(a) => a,
        Value::String(t) if t == "COMBO" => spec[1]["options"].as_array()?,
        _ => return None,
    };
    Some(list.iter().filter_map(|v| v.as_str().map(String::from)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn index() -> Value {
        json!([
            {"type": "image", "title": "Image", "templates": [
                {"name": "api_ideogram_v4_t2i", "title": "Ideogram v4: Text to Image (API)", "tags": ["API", "Text to Image"], "models": ["Ideogram"],
                 "io": {"outputs": [{"nodeId": 6, "nodeType": "SaveImage", "mediaType": "image"}]}},
                {"name": "qwen_edit", "title": "Qwen Image Edit", "tags": ["Image Edit"], "io": {"inputs": [{"nodeType": "LoadImage", "mediaType": "image"}]}},
                {"name": "kontext", "title": "Kontext", "tags": ["Text to Image"], "io": {"inputs": [{"nodeType": "LoadImage", "mediaType": "image"}]}}
            ]},
            {"type": "3d", "title": "3D Model", "templates": [
                {"name": "3d_pixal3d_trellis2_image_to_model", "title": "Pixal3D & TRELLIS.2: Image to Model", "tags": ["3D", "Image to 3D"], "models": ["Pixal3D", "TRELLIS.2"], "openSource": true}
            ]},
            {"type": "video", "templates": [{"name": "wan", "title": "Wan", "tags": ["Text to Video"]}]},
            {"type": "image", "title": "Popular", "templates": [{"name": "api_ideogram_v4_t2i", "title": "Ideogram v4: Text to Image (API)", "tags": ["Text to Image"]}]}
        ])
    }

    #[test]
    fn picks_text_to_image_and_image_to_3d() {
        let c = candidates(&index());
        assert_eq!(
            c.iter().map(|t| (t.name.as_str(), t.kind)).collect::<Vec<_>>(),
            vec![("api_ideogram_v4_t2i", Kind::Image), ("3d_pixal3d_trellis2_image_to_model", Kind::Model3d)]
        );
        assert_eq!(c[1].models, vec!["Pixal3D", "TRELLIS.2"]);
        assert!(c[0].partner && !c[1].partner);
    }

    fn info() -> Value {
        json!({
            "IdeogramV4": {
                "input": {"required": {
                    "prompt": ["STRING", {"multiline": true}], "aspect_ratio": ["COMBO", {"options": ["Auto", "1:1"]}],
                    "rendering_speed": [["DEFAULT", "TURBO"]], "seed": ["INT", {"control_after_generate": true}]
                }},
                "input_order": {"required": ["prompt", "aspect_ratio", "rendering_speed", "seed"]}
            },
            "SaveImage": {"input": {"required": {"images": ["IMAGE"], "filename_prefix": ["STRING", {}]}}},
            "UNETLoader": {"input": {"required": {"unet_name": [["pixal3d_int8_convrot.safetensors"]], "weight_dtype": [["default"]]}}},
            "LoadImage": {"input": {"required": {"image": [[], {"image_upload": true}]}}},
            "Pixal3DMesh": {"input": {"required": {"model": ["MODEL"], "image": ["IMAGE"], "model_list": [[], {"remote": {"route": "/models"}}], "preset": [["Quality"]]}}}
        })
    }

    #[test]
    fn prepares_an_ideogram_template() {
        let ui = json!({
            "nodes": [
                {"id": 6, "type": "SaveImage", "inputs": [{"name": "images", "type": "IMAGE", "link": 3}], "widgets_values": ["ideogram_v4"]},
                {"id": 7, "type": "IdeogramV4", "widgets_values": ["{\"high_level_description\": \"a train\"}", "Auto", "DEFAULT", 0, "randomize"]},
                {"id": 8, "type": "MarkdownNote", "widgets_values": ["notes"]},
                {"id": 18, "type": "GeminiNodeV3", "mode": 4}
            ],
            "links": [[3, 7, 0, 6, 0, "IMAGE"]]
        });
        let g = prepare(&ui, &info(), Kind::Image).unwrap();
        assert_eq!(
            g["7"]["inputs"],
            json!({"prompt": "{{prompt}}", "aspect_ratio": "Auto", "rendering_speed": "DEFAULT", "seed": 0})
        );
        assert_eq!(g["6"]["inputs"]["images"], json!(["7", 0]));
    }

    #[test]
    fn says_what_a_template_lacks() {
        // a node type the server doesn't have
        let ui = json!({"nodes": [{"id": 1, "type": "TripoImageTo3D"}], "links": []});
        assert_eq!(prepare(&ui, &info(), Kind::Model3d).unwrap_err(), vec!["node TripoImageTo3D"]);
        // a model file that isn't there; the template's sample image becomes {{image}} and doesn't count,
        // nor does a list the page fetches itself
        let ui = json!({
            "nodes": [
                {"id": 1, "type": "UNETLoader", "widgets_values": ["trellis_2_int8_convrot.safetensors", "default"]},
                {"id": 2, "type": "LoadImage", "widgets_values": ["viking_axe.png", "image"]},
                {"id": 3, "type": "Pixal3DMesh", "inputs": [{"name": "model", "link": 1}, {"name": "image", "link": 2}], "widgets_values": ["anything.safetensors", "Default"]}
            ],
            "links": [[1, 1, 0, 3, 0, "MODEL"], [2, 2, 0, 3, 1, "IMAGE"]]
        });
        assert_eq!(prepare(&ui, &info(), Kind::Model3d).unwrap_err(), vec!["trellis_2_int8_convrot.safetensors"]);
        let mut ok = ui.clone();
        ok["nodes"][0]["widgets_values"][0] = json!("pixal3d_int8_convrot.safetensors");
        let g = prepare(&ok, &info(), Kind::Model3d).unwrap();
        assert_eq!(g["2"]["inputs"]["image"], "{{image}}");
    }
}
