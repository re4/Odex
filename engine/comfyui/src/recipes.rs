//! Workflows Odex builds itself for node packs a server has installed, so their models work without a
//! saved workflow or a library template: Ideogram 4.0 (ComfyUI-Ideogram4) for images and Pixal3D
//! (ComfyUI_RH_Pixal3D) for 3D. Every widget gets the node's own default (from `/object_info`) except
//! the placeholders and links (and Pixal3D's sdpa attention and low-VRAM mode).

use serde_json::{json, Map, Value};

use crate::workflow::Kind;

/// A workflow Odex can build for this server.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    /// `odex:ideogram4`, `odex:pixal3d`: how [`crate::ComfyClient::template_workflow`] finds it.
    pub name: String,
    pub title: String,
    pub kind: Kind,
    pub models: Vec<String>,
    pub graph: Value,
}

/// Prefix of recipe names among template names.
pub const PREFIX: &str = "odex:";

/// The recipes whose nodes `object_info` has.
pub fn available(object_info: &Value) -> Vec<Recipe> {
    let has = |types: &[&str]| types.iter().all(|t| object_info.get(*t).is_some());
    let node = |class: &str, set: Value| with_defaults(object_info, class, set);
    let mut out = Vec::new();
    if has(&["Ideogram4PipelineLoader", "Ideogram4Generate", "SaveImage"]) {
        let graph = json!({
            "1": node("Ideogram4PipelineLoader", json!({})),
            "2": node("Ideogram4Generate", json!({"pipeline": ["1", 0], "prompt": "{{prompt}}", "width": "{{width}}", "height": "{{height}}"})),
            "3": node("SaveImage", json!({"images": ["2", 0], "filename_prefix": "odex/ideogram4"})),
        });
        out.push(Recipe {
            name: format!("{PREFIX}ideogram4"),
            title: "Ideogram 4.0 (ComfyUI-Ideogram4 nodes)".into(),
            kind: Kind::Image,
            models: vec!["Ideogram".into()],
            graph,
        });
    }
    let pixal3d =
        ["RunningHubPixal3DModelLoader", "RunningHubPixal3DImageTo3D", "RunningHubPixal3DSaveGLB", "LoadImage"];
    if has(&pixal3d) {
        let graph = json!({
            // PyTorch's own attention (sdpa) instead of the pack's default flash_attn, which needs a
            // matching flash-attention build; low VRAM mode offloads between stages to fit smaller GPUs
            "1": node("RunningHubPixal3DModelLoader", json!({"attention_backend": "sdpa", "low_vram": true})),
            "2": node("LoadImage", json!({"image": "{{image}}"})),
            "3": node("RunningHubPixal3DImageTo3D", json!({"pipe": ["1", 0], "image": ["2", 0]})),
            "4": node("RunningHubPixal3DSaveGLB", json!({"asset": ["3", 0]})),
        });
        out.push(Recipe {
            name: format!("{PREFIX}pixal3d"),
            title: "Pixal3D (ComfyUI_RH_Pixal3D nodes)".into(),
            kind: Kind::Model3d,
            models: vec!["Pixal3D".into()],
            graph,
        });
    }
    out
}

/// A node with `set` applied over the defaults of its required widgets (a dropdown's default, else
/// its first choice). Inputs without a default stay unset unless `set` has them.
fn with_defaults(object_info: &Value, class: &str, set: Value) -> Value {
    let def = &object_info[class];
    let required = &def["input"]["required"];
    let order: Vec<String> = match def["input_order"]["required"].as_array() {
        Some(o) => o.iter().filter_map(|n| n.as_str().map(String::from)).collect(),
        None => required.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default(),
    };
    let mut inputs = Map::new();
    for name in order {
        let spec = &required[&name];
        let default = spec[1].get("default").cloned().or_else(|| match &spec[0] {
            Value::Array(options) => options.first().cloned(),
            Value::String(t) if t == "COMBO" => spec[1]["options"].get(0).cloned(),
            _ => None,
        });
        if let Some(v) = set.get(&name).cloned().or(default) {
            inputs.insert(name, v);
        }
    }
    json!({"class_type": class, "inputs": inputs, "_meta": {"title": class}})
}

#[cfg(test)]
mod tests {
    use super::*;

    /// As the server with both packs reports them.
    fn info() -> Value {
        json!({
            "SaveImage": {"input": {"required": {"images": ["IMAGE"], "filename_prefix": ["STRING", {"default": "ComfyUI"}]}}},
            "LoadImage": {"input": {"required": {"image": [["example.png"], {"image_upload": true}]}}},
            "Ideogram4PipelineLoader": {"input": {"required": {"model_weights": [["4.0 NF4", "4.0 FP8"], {"default": "4.0 NF4"}]}},
                "input_order": {"required": ["model_weights"], "hidden": ["unique_id"]}},
            "Ideogram4Generate": {"input": {"required": {
                "pipeline": ["IDEOGRAM4_PIPELINE", {"forceInput": true}], "prompt": ["STRING", {"default": "", "multiline": true}],
                "width": ["INT", {"default": 2048}], "height": ["INT", {"default": 2048}],
                "sampler_preset": [["custom", "4.0 Quality 48", "4.0 Default 20", "4.0 Turbo 12"], {"default": "4.0 Default 20"}],
                "num_steps": ["INT", {"default": 20}], "guidance_scale": ["FLOAT", {"default": 7.0}],
                "mu": ["FLOAT", {"default": 0.0}], "std": ["FLOAT", {"default": 1.75}],
                "seed": ["INT", {"default": 0, "control_after_generate": true}]
            }}, "input_order": {"required": ["pipeline", "prompt", "width", "height", "sampler_preset", "num_steps", "guidance_scale", "mu", "std", "seed"]}},
            "RunningHubPixal3DModelLoader": {"input": {"required": {
                "attention_backend": [["flash_attn", "sdpa"], {"default": "flash_attn"}],
                "sparse_conv_backend": [["flex_gemm", "spconv"], {"default": "flex_gemm"}], "low_vram": ["BOOLEAN", {"default": false}]
            }}},
            "RunningHubPixal3DImageTo3D": {"input": {"required": {
                "pipe": ["PIXAL3D_PIPE"], "image": ["IMAGE"], "seed": ["INT", {"default": 42, "max": 2147483647}],
                "resolution": [["1024", "1536"], {"default": "1024"}], "ss_sampling_steps": ["INT", {"default": 12}]
            }, "optional": {"mask": ["MASK"], "mesh_scale": ["FLOAT", {"default": 1.0}]}}},
            "RunningHubPixal3DSaveGLB": {"input": {"required": {
                "asset": ["PIXAL3D_ASSET"], "decimation_target": ["INT", {"default": 200000}], "texture_size": ["INT", {"default": 2048}],
                "remesh": ["BOOLEAN", {"default": true}], "filename_prefix": ["STRING", {"default": "3d/Pixal3D"}]
            }}}
        })
    }

    #[test]
    fn builds_ideogram4_and_pixal3d_from_the_installed_nodes() {
        let all = available(&info());
        assert_eq!(
            all.iter().map(|r| (r.name.as_str(), r.kind)).collect::<Vec<_>>(),
            vec![("odex:ideogram4", Kind::Image), ("odex:pixal3d", Kind::Model3d)]
        );

        let g = &all[0].graph;
        assert_eq!(g["1"]["inputs"], json!({"model_weights": "4.0 NF4"}));
        assert_eq!(
            g["2"]["inputs"],
            json!({"pipeline": ["1", 0], "prompt": "{{prompt}}", "width": "{{width}}", "height": "{{height}}", "sampler_preset": "4.0 Default 20",
                   "num_steps": 20, "guidance_scale": 7.0, "mu": 0.0, "std": 1.75, "seed": 0})
        );
        assert_eq!(g["3"]["inputs"], json!({"images": ["2", 0], "filename_prefix": "odex/ideogram4"}));
        let w = crate::Workflow::parse("x".into(), &g.to_string()).unwrap();
        assert_eq!(w.placeholders(), vec!["height", "prompt", "width"]);

        let g = &all[1].graph;
        assert_eq!(
            g["1"]["inputs"],
            json!({"attention_backend": "sdpa", "sparse_conv_backend": "flex_gemm", "low_vram": true})
        );
        assert_eq!(g["2"]["inputs"], json!({"image": "{{image}}"}));
        assert_eq!(
            g["3"]["inputs"],
            json!({"pipe": ["1", 0], "image": ["2", 0], "seed": 42, "resolution": "1024", "ss_sampling_steps": 12})
        );
        assert_eq!(g["4"]["inputs"]["asset"], json!(["3", 0]));
        assert_eq!(g["4"]["inputs"]["filename_prefix"], "3d/Pixal3D");
    }

    #[test]
    fn only_when_the_nodes_are_there() {
        let mut partial = info();
        partial.as_object_mut().unwrap().remove("RunningHubPixal3DSaveGLB");
        assert_eq!(available(&partial).iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["odex:ideogram4"]);
        assert!(available(&json!({})).is_empty());
    }
}
