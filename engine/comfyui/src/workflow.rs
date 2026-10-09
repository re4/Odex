//! API-format workflow templates and their `{{placeholder}}` inputs.
//!
//! Workflows come from ComfyUI's Workflow → Export (API), or from the workflows
//! saved on the server (converted by [`crate::ui_format`]). `{{prompt}}`
//! (and optionally `{{negative_prompt}}`, `{{width}}`, `{{height}}`,
//! `{{seed}}`, `{{image}}`) mark the widgets to fill; when a workflow has none,
//! [`add_placeholders`] finds the prompt box and the input image.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Value};

use odex_protocol::ComfyWorkflowInfo;

/// Placeholders a workflow may use.
pub const PLACEHOLDERS: &[&str] = &["prompt", "negative_prompt", "width", "height", "seed", "image"];

/// Node inputs given a fresh seed per run when the workflow has no `{{seed}}`.
const SEED_INPUTS: &[&str] = &["seed", "noise_seed"];

#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub prompt: Option<String>,
    pub negative_prompt: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub seed: Option<u64>,
    /// Name of an uploaded input image (from `ComfyClient::upload_image`).
    pub image: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Workflow {
    pub name: String,
    /// Node id → `{class_type, inputs, _meta}`.
    pub graph: Value,
}

impl Workflow {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(name_of(path), &text)
    }

    pub fn parse(name: String, text: &str) -> Result<Self, String> {
        let graph: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
        validate(&graph)?;
        Ok(Self { name, graph })
    }

    pub fn nodes(&self) -> usize {
        self.graph.as_object().map(|o| o.len()).unwrap_or(0)
    }

    /// Placeholder names used in node inputs, sorted.
    pub fn placeholders(&self) -> Vec<String> {
        placeholders_of(&self.graph)
    }

    pub fn has(&self, name: &str) -> bool {
        self.placeholders().iter().any(|p| p == name)
    }

    /// The graph with placeholders filled in, ready for `/prompt`.
    pub fn fill(&self, inputs: &Inputs) -> Result<Value, String> {
        let used = self.placeholders();
        if let Some(bad) = used.iter().find(|p| !PLACEHOLDERS.contains(&p.as_str())) {
            return Err(format!("unknown placeholder {{{{{bad}}}}}; supported: {}", PLACEHOLDERS.join(", ")));
        }
        if used.iter().any(|p| p == "prompt") && inputs.prompt.is_none() {
            return Err("this workflow needs a prompt".into());
        }
        if used.iter().any(|p| p == "image") && inputs.image.is_none() {
            return Err("this workflow needs an input image (pass `image`)".into());
        }
        // below 2^31: some nodes (Pixal3D) cap their seed there
        let seed = inputs.seed.unwrap_or_else(|| (rand::random::<u32>() >> 1) as u64);
        let value = |name: &str| -> Value {
            match name {
                "prompt" => json!(inputs.prompt.clone().unwrap_or_default()),
                "negative_prompt" => json!(inputs.negative_prompt.clone().unwrap_or_default()),
                "width" => json!(inputs.width.unwrap_or(1024)),
                "height" => json!(inputs.height.unwrap_or(1024)),
                "seed" => json!(seed),
                _ => json!(inputs.image.clone().unwrap_or_default()),
            }
        };
        let mut graph = self.graph.clone();
        for node in graph.as_object_mut().into_iter().flat_map(|o| o.values_mut()) {
            let Some(ins) = node.get_mut("inputs") else { continue };
            replace_strings(ins, &|s| {
                let found = find_placeholders(s);
                if found.is_empty() {
                    return None;
                }
                // A widget holding exactly one placeholder takes the input's type (numbers stay numbers).
                if found.len() == 1 && s.trim() == found[0].0 {
                    return Some(value(&found[0].1));
                }
                let mut out = s.to_string();
                for (raw, name) in found {
                    let v = value(&name);
                    out = out.replace(&raw, &v.as_str().map(String::from).unwrap_or_else(|| v.to_string()));
                }
                Some(Value::String(out))
            });
            if !used.iter().any(|p| p == "seed") {
                // Like ComfyUI's default "randomize" control: a new seed every run.
                if let Some(ins) = ins.as_object_mut() {
                    for k in SEED_INPUTS {
                        if ins.get(*k).is_some_and(|v| v.is_u64()) {
                            ins.insert(k.to_string(), json!(seed));
                        }
                    }
                }
            }
        }
        Ok(graph)
    }
}

fn validate(graph: &Value) -> Result<(), String> {
    let Some(obj) = graph.as_object() else { return Err("expected a JSON object of nodes".into()) };
    if obj.contains_key("nodes") && obj.contains_key("links") {
        return Err("this is a UI-format workflow; in ComfyUI use Workflow → Export (API) and import that file".into());
    }
    if obj.is_empty() {
        return Err("the workflow has no nodes".into());
    }
    for (id, node) in obj {
        if node.get("class_type").and_then(|c| c.as_str()).is_none()
            || !node.get("inputs").is_some_and(|i| i.is_object())
        {
            return Err(format!("node {id} has no class_type or inputs; export with Workflow → Export (API)"));
        }
    }
    Ok(())
}

/// `{{ name }}` occurrences in a string: (exact text, name).
fn find_placeholders(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        let name = after[..end].trim();
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.push((rest[start..start + 2 + end + 2].to_string(), name.to_ascii_lowercase()));
        }
        rest = &after[end + 2..];
    }
    out
}

fn walk_strings(v: &Value, f: &mut dyn FnMut(&str)) {
    match v {
        Value::String(s) => f(s),
        Value::Array(a) => a.iter().for_each(|x| walk_strings(x, f)),
        Value::Object(o) => o.values().for_each(|x| walk_strings(x, f)),
        _ => {}
    }
}

fn replace_strings(v: &mut Value, f: &dyn Fn(&str) -> Option<Value>) {
    match v {
        Value::String(s) => {
            if let Some(n) = f(s) {
                *v = n;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| replace_strings(x, f)),
        Value::Object(o) => o.values_mut().for_each(|x| replace_strings(x, f)),
        _ => {}
    }
}

fn name_of(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

/// Workflow files (`*.json`) in `dir`, sorted by name; unusable ones carry an error.
pub fn list_workflows(dir: &Path) -> Vec<ComfyWorkflowInfo> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<ComfyWorkflowInfo> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
        .map(|p| {
            let path = p.to_string_lossy().to_string();
            match Workflow::load(&p) {
                Ok(w) => ComfyWorkflowInfo {
                    placeholders: w.placeholders(),
                    nodes: w.nodes() as u32,
                    name: w.name,
                    path,
                    error: None,
                },
                Err(e) => ComfyWorkflowInfo { name: name_of(&p), path, placeholders: vec![], nodes: 0, error: Some(e) },
            }
        })
        .collect();
    out.sort_by_key(|w| w.name.to_lowercase());
    out
}

/// What a workflow makes: image workflows take the agent's prompt, image-to-3D ones only its image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Model3d,
    /// Not known yet (a file imported without a role): any placeholder may apply.
    Unknown,
}

/// Save an exported (API-format) workflow file into `dir`; returns its name. See [`save_workflow`].
pub fn import_workflow(dir: &Path, src: &Path) -> Result<String, String> {
    let w = Workflow::load(src)?;
    save_workflow(dir, &w.name, w.graph, Kind::Unknown)
}

/// Save an API-format graph as `<name>.json` in `dir`, first adding the placeholders it lacks
/// ([`add_placeholders`]); returns the name it was saved under.
pub fn save_workflow(dir: &Path, name: &str, mut graph: Value, kind: Kind) -> Result<String, String> {
    validate(&graph)?;
    add_placeholders(&mut graph, kind);
    let name: String = name.chars().filter(|c| !c.is_control() && !r#"<>:"/\|?*"#.contains(*c)).collect();
    let name = match name.trim().trim_end_matches('.') {
        "" => "workflow".to_string(),
        n => n.to_string(),
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let dest = dir.join(format!("{name}.json"));
    let text = serde_json::to_string_pretty(&graph).map_err(|e| e.to_string())?;
    std::fs::write(&dest, text).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(name)
}

/// String inputs that hold the prompt in text-encoder (and string primitive) nodes.
const PROMPT_KEYS: &[&str] = &["text", "text_g", "text_l", "clip_l", "clip_g", "t5xxl", "prompt", "value", "string"];
/// How far upstream of a sampler's `positive` input to look for the prompt.
const PROMPT_DEPTH: usize = 8;

/// Fill in what a workflow saved without placeholders needs: `{{prompt}}` in the positive prompt
/// (found from a sampler's `positive` input, else the only text encoder, else the only `prompt`
/// input; not for image-to-3D workflows) and `{{image}}` in a lone Load Image node. Placeholders the
/// workflow already has are left alone. Returns those added.
pub fn add_placeholders(graph: &mut Value, kind: Kind) -> Vec<String> {
    let used = placeholders_of(graph);
    let mut added = Vec::new();
    if kind != Kind::Model3d && !used.iter().any(|p| p == "prompt") {
        if let Some((id, keys)) = positive_prompt(graph) {
            for k in keys {
                graph[&id]["inputs"][&k] = json!("{{prompt}}");
            }
            added.push("prompt".to_string());
        }
    }
    if !used.iter().any(|p| p == "image") {
        let loads: Vec<String> = graph
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, n)| n["class_type"] == "LoadImage" && n["inputs"]["image"].is_string())
            .map(|(id, _)| id.clone())
            .collect();
        if let [id] = loads.as_slice() {
            graph[id]["inputs"]["image"] = json!("{{image}}");
            added.push("image".to_string());
        }
    }
    added
}

/// The node (and its string inputs) holding the positive prompt.
fn positive_prompt(graph: &Value) -> Option<(String, Vec<String>)> {
    let nodes = graph.as_object()?;
    let prompt_keys = |id: &str| -> Vec<String> {
        nodes[id]["inputs"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, v)| PROMPT_KEYS.contains(&k.as_str()) && v.is_string())
            .map(|(k, _)| k.clone())
            .collect()
    };
    let link = |v: &Value| {
        v.as_array().and_then(|a| a.first()?.as_str()).filter(|id| nodes.contains_key(*id)).map(String::from)
    };
    // upstream of a sampler's `positive` (or a guider's `conditioning`), nearest first
    let starts = nodes.values().filter_map(|n| {
        let ins = &n["inputs"];
        link(&ins["positive"]).or_else(|| {
            n["class_type"].as_str().filter(|c| c.ends_with("Guider")).and_then(|_| link(&ins["conditioning"]))
        })
    });
    for start in starts {
        let mut queue = std::collections::VecDeque::from([(start, 0)]);
        let mut seen = BTreeSet::new();
        while let Some((id, depth)) = queue.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let keys = prompt_keys(&id);
            if !keys.is_empty() {
                return Some((id, keys));
            }
            if depth < PROMPT_DEPTH {
                for v in nodes[&id]["inputs"].as_object().into_iter().flatten().map(|(_, v)| v) {
                    queue.extend(link(v).map(|l| (l, depth + 1)));
                }
            }
        }
    }
    // no sampler to follow: the only text encoder
    let encoders: Vec<&String> = nodes
        .iter()
        .filter(|(id, n)| {
            n["class_type"].as_str().is_some_and(|c| c.contains("TextEncode")) && !prompt_keys(id).is_empty()
        })
        .map(|(id, _)| id)
        .collect();
    if let [id] = encoders.as_slice() {
        return Some(((*id).clone(), prompt_keys(id)));
    }
    // partner (API) nodes such as Ideogram: the only node with a `prompt` text input
    let prompted: Vec<&String> =
        nodes.iter().filter(|(_, n)| n["inputs"]["prompt"].is_string()).map(|(id, _)| id).collect();
    match prompted.as_slice() {
        [id] => Some(((*id).clone(), vec!["prompt".to_string()])),
        _ => None,
    }
}

/// Placeholder names used in a graph's node inputs, sorted.
fn placeholders_of(graph: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    for node in graph.as_object().into_iter().flat_map(|o| o.values()) {
        walk_strings(&node["inputs"], &mut |s| out.extend(find_placeholders(s).into_iter().map(|(_, n)| n)));
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn txt2img() -> Workflow {
        Workflow::parse(
            "flux".into(),
            r#"{
              "3": {"class_type": "KSampler", "inputs": {"seed": 42, "steps": 20, "model": ["4", 0]}},
              "5": {"class_type": "EmptyLatentImage", "inputs": {"width": "{{width}}", "height": "{{ height }}", "batch_size": 1}},
              "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}", "clip": ["4", 1]}},
              "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry, {{negative_prompt}}", "clip": ["4", 1]}},
              "9": {"class_type": "SaveImage", "inputs": {"filename_prefix": "odex", "images": ["8", 0]}}
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn finds_placeholders() {
        assert_eq!(txt2img().placeholders(), vec!["height", "negative_prompt", "prompt", "width"]);
        assert!(txt2img().has("prompt"));
        assert!(!txt2img().has("image"));
    }

    #[test]
    fn fills_typed_values_and_randomizes_seed() {
        let w = txt2img();
        let g = w.fill(&Inputs { prompt: Some("a red fox".into()), width: Some(768), ..Default::default() }).unwrap();
        assert_eq!(g["6"]["inputs"]["text"], "a red fox");
        assert_eq!(g["7"]["inputs"]["text"], "blurry, ");
        assert_eq!(g["5"]["inputs"]["width"], 768);
        assert_eq!(g["5"]["inputs"]["height"], 1024);
        assert_eq!(g["3"]["inputs"]["model"], json!(["4", 0]), "links are untouched");
        assert!(g["3"]["inputs"]["seed"].is_u64());
        let fixed = w.fill(&Inputs { prompt: Some("x".into()), seed: Some(7), ..Default::default() }).unwrap();
        assert_eq!(fixed["3"]["inputs"]["seed"], 7);
    }

    #[test]
    fn requires_inputs_the_workflow_uses() {
        assert!(txt2img().fill(&Inputs::default()).unwrap_err().contains("needs a prompt"));
        let w = Workflow::parse(
            "trellis".into(),
            r#"{"1": {"class_type": "LoadImage", "inputs": {"image": "{{image}}"}}}"#,
        )
        .unwrap();
        assert!(w.fill(&Inputs::default()).unwrap_err().contains("input image"));
        let g = w.fill(&Inputs { image: Some("cat.png".into()), ..Default::default() }).unwrap();
        assert_eq!(g["1"]["inputs"]["image"], "cat.png");
        let bad = Workflow::parse("x".into(), r#"{"1": {"class_type": "A", "inputs": {"t": "{{style}}"}}}"#).unwrap();
        assert!(bad.fill(&Inputs::default()).unwrap_err().contains("unknown placeholder {{style}}"));
    }

    #[test]
    fn rejects_ui_format() {
        let e = Workflow::parse("ui".into(), r#"{"nodes": [], "links": [], "version": 0.4}"#).unwrap_err();
        assert!(e.contains("Export (API)"), "{e}");
        assert!(Workflow::parse("x".into(), "[]").is_err());
        assert!(Workflow::parse("x".into(), r#"{"1": {"inputs": {}}}"#).is_err());
    }

    #[test]
    fn adds_missing_placeholders() {
        // the positive prompt, found through the sampler (the negative one stays as it was)
        let mut g = json!({
            "3": {"class_type": "KSampler", "inputs": {"positive": ["10", 0], "negative": ["7", 0], "seed": 1}},
            "10": {"class_type": "FluxGuidance", "inputs": {"conditioning": ["6", 0], "guidance": 3.5}},
            "6": {"class_type": "CLIPTextEncode", "inputs": {"text": "a bottle", "clip": ["4", 1]}},
            "7": {"class_type": "CLIPTextEncode", "inputs": {"text": "blurry", "clip": ["4", 1]}},
            "4": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "flux.safetensors"}}
        });
        assert_eq!(add_placeholders(&mut g, Kind::Unknown), vec!["prompt"]);
        assert_eq!(g["6"]["inputs"]["text"], "{{prompt}}");
        assert_eq!(g["7"]["inputs"]["text"], "blurry");
        assert_eq!(g["4"]["inputs"]["ckpt_name"], "flux.safetensors");
        assert!(add_placeholders(&mut g, Kind::Unknown).is_empty(), "already there");

        // a guider pipeline with a dual-encoder node: both of its prompt boxes
        let mut g = json!({
            "1": {"class_type": "BasicGuider", "inputs": {"conditioning": ["2", 0], "model": ["5", 0]}},
            "2": {"class_type": "CLIPTextEncodeFlux", "inputs": {"clip_l": "x", "t5xxl": "y", "guidance": 3.5, "clip": ["5", 1]}}
        });
        assert_eq!(add_placeholders(&mut g, Kind::Unknown), vec!["prompt"]);
        assert_eq!(
            (g["2"]["inputs"]["clip_l"].as_str(), g["2"]["inputs"]["t5xxl"].as_str()),
            (Some("{{prompt}}"), Some("{{prompt}}"))
        );

        // image to 3D: the lone Load Image node; no text encoder, so no prompt
        let mut g = json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "chair.png"}},
            "2": {"class_type": "SaveGLB", "inputs": {"mesh": ["1", 0], "filename_prefix": "mesh"}}
        });
        assert_eq!(add_placeholders(&mut g, Kind::Unknown), vec!["image"]);
        assert_eq!(g["1"]["inputs"]["image"], "{{image}}");
        // a partner node (Ideogram) with a plain `prompt` input
        let mut g = json!({
            "1": {"class_type": "IdeogramV4", "inputs": {"prompt": "a cat", "aspect_ratio": "1:1", "seed": 0}},
            "2": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "ideogram"}}
        });
        assert_eq!(add_placeholders(&mut g, Kind::Unknown), vec!["prompt"]);
        assert_eq!(g["1"]["inputs"]["prompt"], "{{prompt}}");
        // image to 3D: only the input image, even with a text encoder in the graph
        let mut g = json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "chair.png"}},
            "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a chair", "clip": ["3", 0]}}
        });
        assert_eq!(add_placeholders(&mut g, Kind::Model3d), vec!["image"]);
        assert_eq!(g["2"]["inputs"]["text"], "a chair");
        // two Load Image nodes: ambiguous, left alone
        let mut g = json!({
            "1": {"class_type": "LoadImage", "inputs": {"image": "a.png"}},
            "2": {"class_type": "LoadImage", "inputs": {"image": "b.png"}}
        });
        assert!(add_placeholders(&mut g, Kind::Unknown).is_empty());
    }

    #[test]
    fn saves_with_placeholders_and_a_safe_name() {
        let dir = tempfile::tempdir().unwrap();
        let g = json!({"6": {"class_type": "CLIPTextEncode", "inputs": {"text": "x", "clip": ["4", 1]}}});
        assert_eq!(save_workflow(dir.path(), "3d/Flux: dev?", g, Kind::Image).unwrap(), "3dFlux dev");
        let w = Workflow::load(&dir.path().join("3dFlux dev.json")).unwrap();
        assert_eq!(w.placeholders(), vec!["prompt"]);
        assert!(save_workflow(dir.path(), "ui", json!({"nodes": [], "links": []}), Kind::Image).is_err());
    }

    #[test]
    fn lists_and_imports() {
        let src = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let good = src.path().join("Flux Dev.json");
        std::fs::write(&good, serde_json::to_string(&txt2img().graph).unwrap()).unwrap();
        let ui = src.path().join("ui.json");
        std::fs::write(&ui, r#"{"nodes": [], "links": []}"#).unwrap();
        assert_eq!(import_workflow(dir.path(), &good).unwrap(), "Flux Dev");
        assert!(import_workflow(dir.path(), &ui).is_err());
        std::fs::copy(&ui, dir.path().join("broken.json")).unwrap();
        let list = list_workflows(dir.path());
        assert_eq!(list.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(), vec!["broken", "Flux Dev"]);
        assert!(list[0].error.is_some());
        assert_eq!(list[1].nodes, 5);
        assert!(list_workflows(&dir.path().join("missing")).is_empty());
    }
}
