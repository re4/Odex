//! API-format workflow templates and their `{{placeholder}}` inputs.
//!
//! Export a workflow from ComfyUI with Workflow → Export (API) after typing
//! `{{prompt}}` (and optionally `{{negative_prompt}}`, `{{width}}`,
//! `{{height}}`, `{{seed}}`, `{{image}}`) into the widgets they should fill.

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
        let mut out = BTreeSet::new();
        for node in self.graph.as_object().into_iter().flat_map(|o| o.values()) {
            walk_strings(&node["inputs"], &mut |s| out.extend(find_placeholders(s).into_iter().map(|(_, n)| n)));
        }
        out.into_iter().collect()
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
        let seed = inputs.seed.unwrap_or_else(|| rand::random::<u32>() as u64);
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

/// Copy an exported workflow into `dir` after checking it; returns its name.
pub fn import_workflow(dir: &Path, src: &Path) -> Result<String, String> {
    let w = Workflow::load(src)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let dest = dir.join(format!("{}.json", w.name));
    if dest != src {
        std::fs::copy(src, &dest).map_err(|e| format!("cannot copy to {}: {e}", dest.display()))?;
    }
    Ok(w.name)
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
