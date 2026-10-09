//! UI-format workflows (what ComfyUI saves, `nodes` + `links`) to API format.
//!
//! ComfyUI's frontend does this when it queues a run (`graphToPrompt`). Here the server's node
//! definitions (`/object_info`) say which inputs are widgets, so each node's `widgets_values` can be
//! matched to input names. Subgraphs are unpacked the way the frontend does it (the nodes inside
//! instance `98` become `98:24`, ...). Links are followed through subgraph inputs and outputs,
//! Reroute, Get/Set and bypassed nodes; muted and frontend-only nodes (notes, primitives) are left
//! out, and so is everything no Save node needs (previews, prompt helpers).

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

/// Input types the frontend shows as widgets; every other type is a socket.
const WIDGET_TYPES: &[&str] = &["INT", "FLOAT", "STRING", "BOOLEAN", "COMBO"];
/// Values of the "control after generate" widget the frontend adds after seed inputs.
const SEED_CONTROLS: &[&str] = &["fixed", "increment", "decrement", "randomize"];
/// Node modes: muted nodes never run, bypassed ones pass their inputs through.
const MUTED: u64 = 2;
const BYPASSED: u64 = 4;
/// Longest chain of reroutes / bypasses / subgraph boundaries followed for one link.
const MAX_HOPS: u32 = 64;
/// The special nodes inside a subgraph that stand for its inputs and outputs.
const SUBGRAPH_IN: &str = "-10";
const SUBGRAPH_OUT: &str = "-20";

/// A UI-format workflow (`nodes` + `links`) rather than an API export.
pub fn is_ui_format(v: &Value) -> bool {
    v.get("nodes").is_some_and(Value::is_array) && v.get("links").is_some()
}

/// Convert a UI-format workflow to API format with the server's `/object_info`.
pub fn to_api(ui: &Value, object_info: &Value) -> Result<Value, String> {
    if !ui["nodes"].is_array() {
        return Err("not a UI-format workflow".into());
    }
    let ctx = Ctx {
        info: object_info,
        subgraphs: ui["definitions"]["subgraphs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| Some((s["id"].as_str()?.to_string(), s)))
            .collect(),
    };
    let root = Frame::new(ui, String::new(), None);
    let mut out = Map::new();
    ctx.emit(&[&root], &mut out, 0)?;
    keep_what_saves_need(&mut out);
    if out.is_empty() {
        return Err("it has no nodes this ComfyUI server knows".into());
    }
    Ok(Value::Object(out))
}

struct Ctx<'a> {
    info: &'a Value,
    /// Subgraph definitions by id (the `type` of their instance nodes).
    subgraphs: HashMap<String, &'a Value>,
}

/// One graph level: the workflow itself, or the inside of one subgraph instance.
struct Frame<'a> {
    nodes: HashMap<String, &'a Value>,
    /// link id → (origin node id, origin output slot)
    links: HashMap<String, (String, u64)>,
    /// What ids get prefixed with here: `""` at the top, `"98:"` inside instance 98.
    prefix: String,
    /// Inside a subgraph: its instance node (one frame up) and its definition.
    instance: Option<(&'a Value, &'a Value)>,
}

impl<'a> Frame<'a> {
    fn new(graph: &'a Value, prefix: String, instance: Option<(&'a Value, &'a Value)>) -> Self {
        let nodes = graph["nodes"].as_array().into_iter().flatten().filter_map(|n| Some((key(&n["id"])?, n))).collect();
        Frame { nodes, links: links(graph), prefix, instance }
    }

    fn sorted(&self) -> Vec<&'a Value> {
        let mut v: Vec<&Value> = self.nodes.values().copied().collect();
        v.sort_by_key(|n| (n["id"].as_i64().unwrap_or(i64::MAX), key(&n["id"]).unwrap_or_default()));
        v
    }
}

/// What feeds an input.
enum Source {
    /// A real node's output: `["<id>", slot]`.
    Link(Value),
    /// A value set on a subgraph instance's own widget.
    Value(Value),
    None,
}

impl<'a> Ctx<'a> {
    fn emit(&self, frames: &[&Frame<'a>], out: &mut Map<String, Value>, depth: u32) -> Result<(), String> {
        let frame = frames[frames.len() - 1];
        for n in frame.sorted() {
            let ty = n["type"].as_str().unwrap_or_default();
            if ty.starts_with("workflow>") || ty.starts_with("workflow/") {
                return Err("it uses group nodes, which Odex can't convert. In ComfyUI, use Workflow → Export (API) and import that file".into());
            }
            if matches!(mode(n), MUTED | BYPASSED) {
                continue;
            }
            let Some(id) = key(&n["id"]) else { continue };
            if let Some(def) = self.subgraphs.get(ty) {
                if depth > 16 {
                    return Err("its subgraphs nest too deeply".into());
                }
                let inner = Frame::new(def, format!("{}{id}:", frame.prefix), Some((n, def)));
                let chain: Vec<&Frame> = frames.iter().copied().chain([&inner]).collect();
                self.emit(&chain, out, depth + 1)?;
                continue;
            }
            // Types the server doesn't know are frontend-only (notes, primitives, ...) or not installed;
            // the latter fail in `resolve` as soon as a real node needs them.
            let Some(def) = self.info.get(ty) else { continue };
            out.insert(format!("{}{id}", frame.prefix), self.node(frames, n, ty, def)?);
        }
        Ok(())
    }

    fn node(&self, frames: &[&Frame<'a>], n: &Value, ty: &str, def: &Value) -> Result<Value, String> {
        let sockets = n["inputs"].as_array().map(Vec::as_slice).unwrap_or_default();
        let link_of = |name: &str| sockets.iter().find(|i| i["name"].as_str() == Some(name)).map(|i| &i["link"]);
        let values = &n["widgets_values"];
        let mut next = 0;
        let mut inputs = Map::new();
        let defined = input_specs(def);
        for (name, spec) in &defined {
            let source = match link_of(name) {
                Some(l) => self.resolve(frames, l, 0)?,
                None => Source::None,
            };
            if !is_widget(spec) {
                if let Source::Link(l) = source {
                    inputs.insert(name.clone(), l);
                }
                continue;
            }
            // every widget has a slot in widgets_values, linked or not
            let own = match values {
                Value::Array(a) => {
                    let v = a.get(next).cloned();
                    next += 1;
                    let seed_like = spec[1]["control_after_generate"].as_bool() == Some(true) || name.contains("seed");
                    if spec[0] == "INT"
                        && seed_like
                        && a.get(next).and_then(Value::as_str).is_some_and(|s| SEED_CONTROLS.contains(&s))
                    {
                        next += 1;
                    }
                    v
                }
                Value::Object(o) => o.get(name).cloned(),
                _ => None,
            };
            let value = match source {
                Source::Link(l) | Source::Value(l) => Some(l),
                Source::None => own,
            };
            if let Some(v) = value {
                inputs.insert(name.clone(), v);
            }
        }
        // sockets the definition doesn't list (inputs that grow as you connect them)
        for s in sockets {
            let Some(name) = s["name"].as_str() else { continue };
            if defined.iter().any(|(d, _)| d == name) {
                continue;
            }
            if let Source::Link(l) = self.resolve(frames, &s["link"], 0)? {
                inputs.insert(name.to_string(), l);
            }
        }
        Ok(json!({"class_type": ty, "inputs": inputs, "_meta": {"title": n["title"].as_str().unwrap_or(ty)}}))
    }

    /// What feeds a link in the innermost frame of `frames`.
    fn resolve(&self, frames: &[&Frame<'a>], link: &Value, hops: u32) -> Result<Source, String> {
        let frame = frames[frames.len() - 1];
        let Some((origin, slot)) = key(link).and_then(|l| frame.links.get(&l)) else { return Ok(Source::None) };
        if hops > MAX_HOPS {
            return Ok(Source::None);
        }
        // a subgraph input: whatever feeds that input on the instance, one frame up
        if origin == SUBGRAPH_IN {
            let (Some((instance, def)), Some(parents)) = (frame.instance, frames.get(..frames.len() - 1)) else {
                return Ok(Source::None);
            };
            let name = def["inputs"][*slot as usize]["name"].as_str();
            let ins = instance["inputs"].as_array().map(Vec::as_slice).unwrap_or_default();
            let Some(input) =
                ins.iter().find(|i| name.is_some() && i["name"].as_str() == name).or(ins.get(*slot as usize))
            else {
                return Ok(Source::None);
            };
            if !input["link"].is_null() {
                return self.resolve(parents, &input["link"], hops + 1);
            }
            // a widget on the instance itself (older subgraphs); promoted widgets keep their value inside
            let widgets: Vec<&Value> = ins.iter().filter(|i| i.get("widget").is_some()).collect();
            let at = widgets.iter().position(|i| std::ptr::eq(*i, input));
            return Ok(match at.and_then(|k| instance["widgets_values"].get(k)) {
                Some(v) if !v.is_null() => Source::Value(v.clone()),
                _ => Source::None,
            });
        }
        let Some(n) = frame.nodes.get(origin) else { return Ok(Source::None) };
        let ty = n["type"].as_str().unwrap_or_default();
        match mode(n) {
            MUTED => return Ok(Source::None),
            BYPASSED => {
                // the input in the same slot if its type matches, else the first input of that type
                let out_ty = &n["outputs"][*slot as usize]["type"];
                let ins = n["inputs"].as_array().map(Vec::as_slice).unwrap_or_default();
                let fits = |i: &&Value| &i["type"] == out_ty && !i["link"].is_null();
                return match ins.get(*slot as usize).filter(fits).or_else(|| ins.iter().find(fits)) {
                    Some(i) => self.resolve(frames, &i["link"], hops + 1),
                    None => Ok(Source::None),
                };
            }
            _ => {}
        }
        // a subgraph instance's output: whatever feeds that output inside it
        if let Some(def) = self.subgraphs.get(ty) {
            let inner = Frame::new(def, format!("{}{origin}:", frame.prefix), Some((n, def)));
            let to_output = def["links"].as_array().into_iter().flatten().find_map(|l| {
                let (id, target, target_slot) = match l {
                    Value::Array(a) => (a.first(), a.get(3), a.get(4)),
                    _ => (l.get("id"), l.get("target_id"), l.get("target_slot")),
                };
                (target.and_then(key).as_deref() == Some(SUBGRAPH_OUT)
                    && target_slot.and_then(Value::as_u64) == Some(*slot))
                .then(|| id.cloned())
                .flatten()
            });
            let Some(link) = to_output else { return Ok(Source::None) };
            let chain: Vec<&Frame> = frames.iter().copied().chain([&inner]).collect();
            return self.resolve(&chain, &link, hops + 1);
        }
        match ty {
            "Reroute" => self.resolve(frames, &n["inputs"][0]["link"], hops + 1),
            "GetNode" => {
                let name = &n["widgets_values"][0];
                match frame.nodes.values().find(|s| s["type"] == "SetNode" && &s["widgets_values"][0] == name) {
                    Some(set) => self.resolve(frames, &set["inputs"][0]["link"], hops + 1),
                    None => Ok(Source::None),
                }
            }
            // a frontend primitive: the target keeps the value in its own widget
            "PrimitiveNode" => Ok(Source::None),
            _ if self.info.get(ty).is_some() => Ok(Source::Link(json!([format!("{}{origin}", frame.prefix), slot]))),
            _ => Err(format!("it needs the node `{ty}`, which isn't installed on this ComfyUI server")),
        }
    }
}

/// Drop what no Save node needs (previews, prompt helpers, notes' neighbours): ComfyUI's page runs
/// those for display only. A workflow without a Save node keeps everything.
fn keep_what_saves_need(out: &mut Map<String, Value>) {
    let mut stack: Vec<String> = out
        .iter()
        .filter(|(_, n)| n["class_type"].as_str().is_some_and(|c| c.starts_with("Save")))
        .map(|(id, _)| id.clone())
        .collect();
    if stack.is_empty() {
        return;
    }
    let mut keep = HashSet::new();
    while let Some(id) = stack.pop() {
        if !keep.insert(id.clone()) {
            continue;
        }
        for v in out[&id]["inputs"].as_object().into_iter().flatten().map(|(_, v)| v) {
            if let Some(src) = v.as_array().filter(|a| a.len() == 2 && a[1].is_u64()).and_then(|a| a[0].as_str()) {
                stack.push(src.to_string());
            }
        }
    }
    out.retain(|id, _| keep.contains(id));
}

/// `[link id, origin id, origin slot, target id, target slot, type]` rows, or objects in newer files.
fn links(graph: &Value) -> HashMap<String, (String, u64)> {
    let mut out = HashMap::new();
    for l in graph["links"].as_array().into_iter().flatten() {
        let (id, origin, slot) = match l {
            Value::Array(a) => (a.first(), a.get(1), a.get(2)),
            _ => (l.get("id"), l.get("origin_id"), l.get("origin_slot")),
        };
        if let (Some(id), Some(origin), Some(slot)) =
            (id.and_then(key), origin.and_then(key), slot.and_then(Value::as_u64))
        {
            out.insert(id, (origin, slot));
        }
    }
    out
}

/// Inputs in the order the frontend creates widgets: required, then optional.
pub(crate) fn input_specs(def: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for group in ["required", "optional"] {
        let specs = &def["input"][group];
        let order: Vec<String> = match def["input_order"][group].as_array() {
            Some(o) => o.iter().filter_map(|n| n.as_str().map(String::from)).collect(),
            None => specs.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default(),
        };
        out.extend(order.into_iter().filter_map(|n| Some((n.clone(), specs.get(&n)?.clone()))));
    }
    out
}

/// `[type, options]`: a list (or `COMBO`) is a dropdown; `forceInput` makes a widget type a socket.
fn is_widget(spec: &Value) -> bool {
    if spec[1]["forceInput"].as_bool() == Some(true) {
        return false;
    }
    match &spec[0] {
        Value::Array(_) => true,
        Value::String(t) => WIDGET_TYPES.contains(&t.as_str()),
        _ => false,
    }
}

fn mode(n: &Value) -> u64 {
    n["mode"].as_u64().unwrap_or(0)
}

/// Node and link ids are numbers in most files and strings in some.
fn key(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Node definitions as `/object_info` reports them (old and new combo styles, `input_order`).
    fn info() -> Value {
        json!({
            "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [["sd15.safetensors", "flux.safetensors"]]}}},
            "CLIPTextEncode": {"input": {"required": {"text": ["STRING", {"multiline": true}], "clip": ["CLIP"]}}},
            "EmptyLatentImage": {"input": {"required": {"width": ["INT", {}], "height": ["INT", {}], "batch_size": ["INT", {}]}}},
            "KSampler": {
                "input": {"required": {
                    "model": ["MODEL"], "seed": ["INT", {"control_after_generate": true}], "steps": ["INT", {}],
                    "cfg": ["FLOAT", {}], "sampler_name": ["COMBO", {"options": ["euler"]}], "scheduler": [["normal"]],
                    "positive": ["CONDITIONING"], "negative": ["CONDITIONING"], "latent_image": ["LATENT"], "denoise": ["FLOAT", {}]
                }},
                "input_order": {"required": ["model", "seed", "steps", "cfg", "sampler_name", "scheduler", "positive", "negative", "latent_image", "denoise"]}
            },
            "VAEDecode": {"input": {"required": {"samples": ["LATENT"], "vae": ["VAE"]}}},
            "SaveImage": {"input": {"required": {"images": ["IMAGE"], "filename_prefix": ["STRING", {}]}}},
            "LoadImage": {"input": {"required": {"image": [["cat.png"], {"image_upload": true}]}}},
            "PrimitiveInt": {"input": {"required": {"value": ["INT", {"control_after_generate": true}]}}},
            "StringReplace": {"input": {"required": {"string": ["STRING", {}], "find": ["STRING", {}], "replace": ["STRING", {}]}}},
            "PreviewAny": {"input": {"required": {"source": ["*", {}]}}},
            "ImageScale": {"input": {"required": {"image": ["IMAGE"], "upscale_method": [["nearest-exact"]], "width": ["INT", {}], "height": ["INT", {}], "crop": [["disabled"]]}}}
        })
    }

    /// ComfyUI's default text-to-image graph as the frontend saves it.
    fn default_graph() -> Value {
        json!({
            "last_node_id": 9, "last_link_id": 9, "version": 0.4,
            "nodes": [
                {"id": 7, "type": "CLIPTextEncode", "mode": 0, "inputs": [{"name": "clip", "type": "CLIP", "link": 5}], "widgets_values": ["text, watermark"]},
                {"id": 6, "type": "CLIPTextEncode", "mode": 0, "inputs": [{"name": "clip", "type": "CLIP", "link": 3}], "widgets_values": ["purple galaxy bottle"]},
                {"id": 5, "type": "EmptyLatentImage", "mode": 0, "widgets_values": [512, 512, 1]},
                {"id": 3, "type": "KSampler", "mode": 0, "inputs": [
                    {"name": "model", "type": "MODEL", "link": 1}, {"name": "positive", "type": "CONDITIONING", "link": 4},
                    {"name": "negative", "type": "CONDITIONING", "link": 6}, {"name": "latent_image", "type": "LATENT", "link": 2}
                ], "widgets_values": [156680208700286_u64, "randomize", 20, 8, "euler", "normal", 1]},
                {"id": 8, "type": "VAEDecode", "mode": 0, "inputs": [{"name": "samples", "type": "LATENT", "link": 7}, {"name": "vae", "type": "VAE", "link": 8}]},
                {"id": 9, "type": "SaveImage", "mode": 0, "inputs": [{"name": "images", "type": "IMAGE", "link": 9}], "widgets_values": ["ComfyUI"]},
                {"id": 4, "type": "CheckpointLoaderSimple", "mode": 0, "outputs": [{"type": "MODEL"}, {"type": "CLIP"}, {"type": "VAE"}], "widgets_values": ["sd15.safetensors"]},
                {"id": 10, "type": "Note", "mode": 0, "widgets_values": ["just a note"]}
            ],
            "links": [[1, 4, 0, 3, 0, "MODEL"], [2, 5, 0, 3, 3, "LATENT"], [3, 4, 1, 6, 0, "CLIP"], [4, 6, 0, 3, 1, "CONDITIONING"],
                      [5, 4, 1, 7, 0, "CLIP"], [6, 7, 0, 3, 2, "CONDITIONING"], [7, 3, 0, 8, 0, "LATENT"], [8, 4, 2, 8, 1, "VAE"], [9, 8, 0, 9, 0, "IMAGE"]]
        })
    }

    #[test]
    fn converts_the_default_graph() {
        let api = to_api(&default_graph(), &info()).unwrap();
        assert_eq!(
            api.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["3", "4", "5", "6", "7", "8", "9"],
            "the note is dropped"
        );
        assert_eq!(
            api["3"]["inputs"],
            json!({"model": ["4", 0], "seed": 156680208700286_u64, "steps": 20, "cfg": 8, "sampler_name": "euler", "scheduler": "normal",
                   "positive": ["6", 0], "negative": ["7", 0], "latent_image": ["5", 0], "denoise": 1})
        );
        assert_eq!(api["3"]["class_type"], "KSampler");
        assert_eq!(api["6"]["inputs"], json!({"text": "purple galaxy bottle", "clip": ["4", 1]}));
        assert_eq!(api["5"]["inputs"], json!({"width": 512, "height": 512, "batch_size": 1}));
        assert_eq!(api["8"]["inputs"], json!({"samples": ["3", 0], "vae": ["4", 2]}));
        assert_eq!(api["9"]["inputs"], json!({"images": ["8", 0], "filename_prefix": "ComfyUI"}));
        assert_eq!(api["4"]["_meta"]["title"], "CheckpointLoaderSimple");
    }

    #[test]
    fn follows_reroutes_bypasses_and_get_set_nodes() {
        let ui = json!({
            "nodes": [
                {"id": 1, "type": "LoadImage", "widgets_values": ["cat.png", "image"], "outputs": [{"type": "IMAGE"}, {"type": "MASK"}]},
                {"id": 2, "type": "Reroute", "inputs": [{"name": "", "type": "*", "link": 1}]},
                // bypassed: its IMAGE output passes its IMAGE input through
                {"id": 3, "type": "ImageScale", "mode": 4, "inputs": [{"name": "image", "type": "IMAGE", "link": 2}], "outputs": [{"type": "IMAGE"}], "widgets_values": ["nearest-exact", 512, 512, "disabled"]},
                {"id": 4, "type": "SetNode", "inputs": [{"name": "IMAGE", "type": "IMAGE", "link": 3}], "widgets_values": ["picture"]},
                {"id": 5, "type": "GetNode", "outputs": [{"type": "IMAGE"}], "widgets_values": ["picture"]},
                {"id": 6, "type": "SaveImage", "inputs": [{"name": "images", "type": "IMAGE", "link": 4}, {"name": "filename_prefix", "type": "STRING", "widget": {"name": "filename_prefix"}, "link": 5}], "widgets_values": ["from-primitive"]},
                {"id": 7, "type": "PrimitiveNode", "outputs": [{"type": "STRING"}], "widgets_values": ["from-primitive"]},
                // muted: left out, and nothing it feeds gets a link
                {"id": 8, "type": "SaveImage", "mode": 2, "inputs": [{"name": "images", "type": "IMAGE", "link": 6}], "widgets_values": ["muted"]}
            ],
            "links": [
                {"id": 1, "origin_id": 1, "origin_slot": 0, "target_id": 2, "target_slot": 0, "type": "IMAGE"},
                {"id": 2, "origin_id": 2, "origin_slot": 0, "target_id": 3, "target_slot": 0, "type": "IMAGE"},
                {"id": 3, "origin_id": 3, "origin_slot": 0, "target_id": 4, "target_slot": 0, "type": "IMAGE"},
                {"id": 4, "origin_id": 5, "origin_slot": 0, "target_id": 6, "target_slot": 0, "type": "IMAGE"},
                {"id": 5, "origin_id": 7, "origin_slot": 0, "target_id": 6, "target_slot": 1, "type": "STRING"},
                {"id": 6, "origin_id": 1, "origin_slot": 0, "target_id": 8, "target_slot": 0, "type": "IMAGE"}
            ]
        });
        let api = to_api(&ui, &info()).unwrap();
        assert_eq!(api.as_object().unwrap().keys().collect::<Vec<_>>(), vec!["1", "6"]);
        assert_eq!(api["1"]["inputs"], json!({"image": "cat.png"}), "the upload button's value is not an input");
        assert_eq!(api["6"]["inputs"], json!({"images": ["1", 0], "filename_prefix": "from-primitive"}));
    }

    #[test]
    fn explains_what_it_cannot_convert() {
        let mut ui = default_graph();
        ui["nodes"][4]["type"] = json!("FancyDecode");
        assert_eq!(
            to_api(&ui, &info()).unwrap_err(),
            "it needs the node `FancyDecode`, which isn't installed on this ComfyUI server"
        );
        let group = json!({"nodes": [{"id": 1, "type": "workflow>My group"}], "links": []});
        assert!(to_api(&group, &info()).unwrap_err().contains("group nodes"));
        assert!(to_api(&json!({"nodes": [{"id": 1, "type": "Note"}], "links": []}), &info()).is_err());
        assert!(
            is_ui_format(&default_graph()) && !is_ui_format(&json!({"3": {"class_type": "KSampler", "inputs": {}}}))
        );
    }

    /// Shaped like ComfyUI's local Ideogram v4 template: the pipeline inside a subgraph whose width
    /// comes from a node outside, the prompt a widget promoted from inside, and a second subgraph that
    /// only builds an example prompt for a preview (with literal `{{...}}` in it).
    #[test]
    fn unpacks_subgraphs_and_drops_preview_branches() {
        let pipeline = json!({
            "id": "aaaa-pipeline", "name": "Text to Image",
            "inputs": [{"name": "text", "type": "STRING"}, {"name": "value", "type": "INT"}],
            "outputs": [{"name": "IMAGE", "type": "IMAGE"}],
            "inputNode": {"id": -10}, "outputNode": {"id": -20},
            "nodes": [
                {"id": 4, "type": "CheckpointLoaderSimple", "widgets_values": ["sd15.safetensors"]},
                {"id": 24, "type": "CLIPTextEncode", "inputs": [{"name": "clip", "link": 1}, {"name": "text", "link": 2, "widget": {"name": "text"}}], "widgets_values": ["{\"high_level_description\": \"a knight\"}"]},
                {"id": 7, "type": "CLIPTextEncode", "inputs": [{"name": "clip", "link": 3}], "widgets_values": [""]},
                {"id": 27, "type": "PrimitiveInt", "inputs": [{"name": "value", "link": 4, "widget": {"name": "value"}}], "widgets_values": [512, "fixed"]},
                {"id": 5, "type": "EmptyLatentImage", "inputs": [{"name": "width", "link": 5, "widget": {"name": "width"}}], "widgets_values": [512, 768, 1]},
                {"id": 3, "type": "KSampler", "inputs": [
                    {"name": "model", "link": 6}, {"name": "positive", "link": 7}, {"name": "negative", "link": 8}, {"name": "latent_image", "link": 9}
                ], "widgets_values": [7, "randomize", 20, 7, "euler", "normal", 1]},
                {"id": 8, "type": "VAEDecode", "inputs": [{"name": "samples", "link": 10}, {"name": "vae", "link": 11}]}
            ],
            "links": [
                {"id": 1, "origin_id": 4, "origin_slot": 1, "target_id": 24, "target_slot": 0},
                {"id": 2, "origin_id": -10, "origin_slot": 0, "target_id": 24, "target_slot": 1},
                {"id": 3, "origin_id": 4, "origin_slot": 1, "target_id": 7, "target_slot": 0},
                {"id": 4, "origin_id": -10, "origin_slot": 1, "target_id": 27, "target_slot": 0},
                {"id": 5, "origin_id": 27, "origin_slot": 0, "target_id": 5, "target_slot": 0},
                {"id": 6, "origin_id": 4, "origin_slot": 0, "target_id": 3, "target_slot": 0},
                {"id": 7, "origin_id": 24, "origin_slot": 0, "target_id": 3, "target_slot": 1},
                {"id": 8, "origin_id": 7, "origin_slot": 0, "target_id": 3, "target_slot": 2},
                {"id": 9, "origin_id": 5, "origin_slot": 0, "target_id": 3, "target_slot": 3},
                {"id": 10, "origin_id": 3, "origin_slot": 0, "target_id": 8, "target_slot": 0},
                {"id": 11, "origin_id": 4, "origin_slot": 2, "target_id": 8, "target_slot": 1},
                {"id": 12, "origin_id": 8, "origin_slot": 0, "target_id": -20, "target_slot": 0}
            ]
        });
        let helper = json!({
            "id": "bbbb-helper", "inputs": [], "outputs": [{"name": "STRING", "type": "STRING"}],
            "nodes": [{"id": 163, "type": "StringReplace", "widgets_values": ["Idea: {{original_prompt}} at {{width}}", "{{original_prompt}}", ""]}],
            "links": [{"id": 1, "origin_id": 163, "origin_slot": 0, "target_id": -20, "target_slot": 0}]
        });
        let ui = json!({
            "nodes": [
                {"id": 37, "type": "PrimitiveInt", "widgets_values": [1024, "fixed"]},
                {"id": 98, "type": "aaaa-pipeline", "inputs": [
                    {"name": "text", "type": "STRING", "link": null, "widget": {"name": "text"}},
                    {"name": "value", "type": "INT", "link": 161, "widget": {"name": "value"}}
                ], "widgets_values": [], "properties": {"proxyWidgets": [["24", "text"], ["27", "value"]]}},
                {"id": 158, "type": "SaveImage", "inputs": [{"name": "images", "link": 224}], "widgets_values": ["Ideogram_4.0"]},
                {"id": 134, "type": "bbbb-helper", "inputs": []},
                {"id": 111, "type": "PreviewAny", "inputs": [{"name": "source", "link": 252}]}
            ],
            "links": [[161, 37, 0, 98, 1, "INT"], [224, 98, 0, 158, 0, "IMAGE"], [252, 134, 0, 111, 0, "STRING"]],
            "definitions": {"subgraphs": [pipeline, helper]}
        });
        let api = to_api(&ui, &info()).unwrap();
        assert_eq!(
            api.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["37", "98:3", "98:4", "98:5", "98:7", "98:8", "98:24", "98:27", "158"],
            "inside nodes become 98:<id>; the preview branch and its {{...}} text are dropped"
        );
        assert_eq!(api["158"]["inputs"]["images"], json!(["98:8", 0]), "through the subgraph's output");
        assert_eq!(api["98:27"]["inputs"]["value"], json!(["37", 0]), "through the subgraph's input");
        assert_eq!(
            api["98:24"]["inputs"]["text"], "{\"high_level_description\": \"a knight\"}",
            "a promoted widget keeps its value inside"
        );
        assert_eq!(api["98:3"]["inputs"]["positive"], json!(["98:24", 0]));
        assert_eq!(api["98:5"]["inputs"], json!({"width": ["98:27", 0], "height": 768, "batch_size": 1}));
    }
}
