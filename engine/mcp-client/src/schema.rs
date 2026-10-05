//! JSON Schema simplification for vLLM chat templates.
//!
//! Jinja tool-call templates (Qwen, Llama, Mistral, ...) render tool
//! parameters by walking `properties` and printing `type`/`description`; they
//! choke on `$ref`, `anyOf` unions, type arrays and nested definitions. The
//! sanitized schema is only what the model sees; arguments are validated
//! against the original schema.

use serde_json::{json, Map, Value};

type Obj = Map<String, Value>;

/// Property name used when a non-object root schema has to be wrapped.
pub(crate) const WRAPPED_ARG: &str = "value";

const MAX_DEPTH: usize = 40;
const MAX_REF_EXPANSIONS: usize = 2_000;

/// Keywords removed outright (metadata, or too complex for templates).
const DROPPED: &[&str] = &[
    "$schema",
    "$id",
    "$defs",
    "definitions",
    "$comment",
    "$anchor",
    "$dynamicAnchor",
    "$dynamicRef",
    "$recursiveRef",
    "$recursiveAnchor",
    "$vocabulary",
    "examples",
    "example",
    "nullable",
    "not",
    "if",
    "then",
    "else",
    "dependentSchemas",
    "dependentRequired",
    "dependencies",
    "patternProperties",
    "unevaluatedProperties",
    "unevaluatedItems",
    "propertyNames",
    "contains",
    "minContains",
    "maxContains",
    "prefixItems",
    "additionalItems",
    "contentSchema",
    "readOnly",
    "writeOnly",
    "deprecated",
    "discriminator",
    "xml",
    "externalDocs",
];

/// Flatten `schema` into a template-friendly shape:
///
/// * `$ref` (local JSON pointers) are inlined; `$defs`/`definitions` dropped;
///   a reference cycle becomes `{"type":"object"}`;
/// * `anyOf`/`oneOf`: null branches removed, const unions become `enum`,
///   same-type unions are merged, otherwise the first object-ish (or most
///   general) branch wins and the alternatives are noted in `description`;
/// * `allOf` branches are merged (properties and `required`);
/// * type arrays collapse to one type; metadata (`$schema`, `$id`,
///   `examples`, non-scalar `default`s, ...) is dropped;
/// * every `properties` entry gets a `type` (`string` when nothing else can
///   be inferred) and the root is always `{"type":"object","properties":{..}}`
///   (a non-object root is wrapped under a `value` property).
pub fn sanitize_schema(schema: &Value) -> Value {
    Value::Object(ensure_top_level_object(sanitize_inner(schema)))
}

/// True when the root of `schema` is not an object, so [`sanitize_schema`]
/// wrapped it under [`WRAPPED_ARG`] and calls must be unwrapped.
pub(crate) fn root_is_wrapped(schema: &Value) -> bool {
    matches!(sanitize_inner(schema).get("type").and_then(Value::as_str), Some(t) if t != "object")
}

fn sanitize_inner(schema: &Value) -> Obj {
    let mut s = Sanitizer { root: schema, ref_stack: vec!["#".to_string()], expansions: 0 };
    s.node(schema, 0)
}

struct Sanitizer<'a> {
    root: &'a Value,
    ref_stack: Vec<String>,
    expansions: usize,
}

impl Sanitizer<'_> {
    fn node(&mut self, schema: &Value, depth: usize) -> Obj {
        let Value::Object(obj) = schema else {
            // `true`/`false` and malformed entries: unconstrained.
            return Obj::new();
        };
        if depth > MAX_DEPTH {
            return placeholder(obj);
        }
        if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
            let resolved = self.resolve_ref(r, depth);
            let mut siblings = obj.clone();
            for k in ["$ref", "$defs", "definitions", "$schema", "$id"] {
                siblings.remove(k);
            }
            if siblings.is_empty() {
                return resolved;
            }
            let sib = self.object(&siblings, depth);
            return overlay_ref(resolved, sib);
        }
        self.object(obj, depth)
    }

    fn resolve_ref(&mut self, r: &str, depth: usize) -> Obj {
        let key = normalize_ref(r);
        if self.ref_stack.contains(&key) || self.expansions >= MAX_REF_EXPANSIONS {
            return object_placeholder();
        }
        let Some(target) = resolve_pointer(self.root, &key) else {
            return object_placeholder();
        };
        self.expansions += 1;
        self.ref_stack.push(key);
        let out = self.node(target, depth + 1);
        self.ref_stack.pop();
        out
    }

    fn object(&mut self, obj: &Obj, depth: usize) -> Obj {
        let mut out = Obj::new();
        for (k, v) in obj {
            match k.as_str() {
                "properties" => {
                    if let Value::Object(props) = v {
                        let mut np = Obj::new();
                        for (name, ps) in props {
                            let s = self.node(ps, depth + 1);
                            np.insert(name.clone(), Value::Object(ensure_typed(s)));
                        }
                        out.insert(k.clone(), Value::Object(np));
                    }
                }
                "items" => match v {
                    Value::Array(list) => {
                        if let Some(first) = list.first() {
                            out.insert(k.clone(), Value::Object(self.node(first, depth + 1)));
                        }
                    }
                    Value::Object(_) => {
                        out.insert(k.clone(), Value::Object(self.node(v, depth + 1)));
                    }
                    _ => {}
                },
                "additionalProperties" => match v {
                    Value::Bool(_) => {
                        out.insert(k.clone(), v.clone());
                    }
                    Value::Object(_) => {
                        out.insert(k.clone(), Value::Object(self.node(v, depth + 1)));
                    }
                    _ => {}
                },
                "type" | "const" => {
                    out.insert(k.clone(), v.clone());
                }
                "enum" => {
                    if v.is_array() {
                        out.insert(k.clone(), v.clone());
                    }
                }
                "required" => {
                    if let Value::Array(list) = v {
                        let names: Vec<Value> = list.iter().filter(|x| x.is_string()).cloned().collect();
                        out.insert(k.clone(), Value::Array(names));
                    }
                }
                "allOf" | "anyOf" | "oneOf" => {}
                key if DROPPED.contains(&key) => {}
                // `default` and any other keyword: only scalar values survive.
                _ => {
                    if is_scalar(v) {
                        out.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        if !out.contains_key("items") {
            if let Some(Value::Array(prefix)) = obj.get("prefixItems") {
                if let Some(first) = prefix.first() {
                    out.insert("items".into(), Value::Object(self.node(first, depth + 1)));
                }
            }
        }
        if let Some(Value::Array(branches)) = obj.get("allOf") {
            for b in branches {
                let s = self.node(b, depth + 1);
                merge_all_of(&mut out, s);
            }
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(Value::Array(branches)) = obj.get(key) {
                let sanitized: Vec<Obj> = branches.iter().map(|b| self.node(b, depth + 1)).collect();
                if let Some((chosen, note)) = simplify_union(sanitized) {
                    merge_union(&mut out, chosen, note);
                }
            }
        }
        finalize(&mut out);
        out
    }
}

fn object_placeholder() -> Obj {
    let mut m = Obj::new();
    m.insert("type".into(), json!("object"));
    m
}

fn placeholder(obj: &Obj) -> Obj {
    let mut m = object_placeholder();
    if let Some(d) = obj.get("description").filter(|d| d.is_string()) {
        m.insert("description".into(), d.clone());
    }
    m
}

fn normalize_ref(r: &str) -> String {
    let t = r.trim();
    let t = t.strip_suffix('/').unwrap_or(t);
    if t.is_empty() {
        "#".to_string()
    } else {
        t.to_string()
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve a local reference (`#`, `#/$defs/X`, `#/properties/a/items`).
fn resolve_pointer<'v>(root: &'v Value, r: &str) -> Option<&'v Value> {
    let frag = r.strip_prefix('#')?;
    if frag.is_empty() {
        return Some(root);
    }
    let frag = frag.strip_prefix('/')?;
    let mut cur = root;
    for raw in frag.split('/') {
        let tok = percent_decode(raw).replace("~1", "/").replace("~0", "~");
        cur = match cur {
            Value::Object(m) => m.get(&tok)?,
            Value::Array(a) => a.get(tok.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn is_scalar(v: &Value) -> bool {
    matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

fn type_of(o: &Obj) -> Option<&str> {
    o.get("type").and_then(Value::as_str)
}

fn append_description(out: &mut Obj, note: &str) {
    let desc = match out.get("description").and_then(Value::as_str) {
        Some(d) if !d.trim().is_empty() => {
            let d = d.trim_end();
            if d.ends_with('.') || d.ends_with('!') || d.ends_with('?') {
                format!("{d} {note}")
            } else {
                format!("{d}. {note}")
            }
        }
        _ => note.to_string(),
    };
    out.insert("description".into(), Value::String(desc));
}

/// Merge the referencing site's siblings over the resolved target: its
/// annotations win, structure is combined.
fn overlay_ref(mut resolved: Obj, siblings: Obj) -> Obj {
    for (k, v) in siblings {
        match k.as_str() {
            "description" | "title" | "default" => {
                resolved.insert(k, v);
            }
            "properties" => merge_properties(&mut resolved, v),
            "required" => union_required(&mut resolved, v),
            _ => {
                resolved.entry(k).or_insert(v);
            }
        }
    }
    resolved
}

fn merge_properties(out: &mut Obj, v: Value) {
    let Value::Object(src) = v else { return };
    let dst = out.entry("properties").or_insert_with(|| Value::Object(Obj::new()));
    let Value::Object(dst) = dst else { return };
    for (pk, pv) in src {
        match (dst.get_mut(&pk), pv) {
            (Some(Value::Object(existing)), Value::Object(pv)) => {
                for (kk, vv) in pv {
                    existing.entry(kk).or_insert(vv);
                }
            }
            (Some(_), _) => {}
            (None, pv) => {
                dst.insert(pk, pv);
            }
        }
    }
}

fn union_required(out: &mut Obj, v: Value) {
    let Value::Array(add) = v else { return };
    let entry = out.entry("required").or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(list) = entry {
        for name in add {
            if name.is_string() && !list.contains(&name) {
                list.push(name);
            }
        }
    }
}

fn merge_all_of(out: &mut Obj, branch: Obj) {
    for (k, v) in branch {
        match k.as_str() {
            "properties" => merge_properties(out, v),
            "required" => union_required(out, v),
            _ => {
                out.entry(k).or_insert(v);
            }
        }
    }
}

fn merge_union(out: &mut Obj, chosen: Obj, note: Option<String>) {
    for (k, v) in chosen {
        match k.as_str() {
            "properties" => merge_properties(out, v),
            "required" => union_required(out, v),
            _ => {
                out.entry(k).or_insert(v);
            }
        }
    }
    if let Some(note) = note {
        append_description(out, &note);
    }
}

fn is_null_schema(b: &Obj) -> bool {
    if type_of(b) == Some("null") {
        return true;
    }
    if let Some(Value::Array(e)) = b.get("enum") {
        return !e.is_empty() && e.iter().all(Value::is_null);
    }
    false
}

/// Branches that only add constraints (`{"required": [..]}`, `{}`) carry no
/// shape information.
fn is_structural(b: &Obj) -> bool {
    ["type", "properties", "items", "enum"].iter().any(|k| b.contains_key(*k))
}

fn infer_type_from_values(values: &[Value]) -> Option<&'static str> {
    if values.is_empty() {
        return None;
    }
    if values.iter().all(Value::is_string) {
        Some("string")
    } else if values.iter().all(|v| v.is_i64() || v.is_u64()) {
        Some("integer")
    } else if values.iter().all(Value::is_number) {
        Some("number")
    } else if values.iter().all(Value::is_boolean) {
        Some("boolean")
    } else {
        None
    }
}

const GENERALITY: &[&str] = &["object", "array", "string", "number", "integer", "boolean", "null"];

fn generality_rank(t: &str) -> usize {
    GENERALITY.iter().position(|g| *g == t).unwrap_or(GENERALITY.len())
}

fn describe_branch(b: &Obj) -> String {
    let t = type_of(b).unwrap_or("any value");
    if t == "array" {
        return match b.get("items").and_then(|i| i.get("type")).and_then(Value::as_str) {
            Some(it) => format!("array of {it}"),
            None => "array".to_string(),
        };
    }
    if let Some(Value::Array(e)) = b.get("enum") {
        let vals: Vec<String> = e.iter().map(Value::to_string).collect();
        return format!("{t} ({})", vals.join(" | "));
    }
    t.to_string()
}

fn simplify_union(branches: Vec<Obj>) -> Option<(Obj, Option<String>)> {
    let non_null: Vec<Obj> = branches.into_iter().filter(|b| !is_null_schema(b)).collect();
    if non_null.is_empty() {
        let mut m = Obj::new();
        m.insert("type".into(), json!("null"));
        return Some((m, None));
    }
    let mut rest: Vec<Obj> = non_null.into_iter().filter(is_structural).collect();
    if rest.is_empty() {
        return None;
    }
    if rest.len() == 1 {
        return rest.pop().map(|b| (b, None));
    }

    // Const / enum unions → one enum.
    if rest.iter().all(|b| b.get("enum").is_some_and(Value::is_array) && !b.contains_key("properties")) {
        let mut values: Vec<Value> = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        for b in &rest {
            let vals = b.get("enum").and_then(Value::as_array).cloned().unwrap_or_default();
            if let Some(d) = b.get("description").and_then(Value::as_str) {
                let names: Vec<String> =
                    vals.iter().map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).collect();
                notes.push(format!("{}: {d}", names.join("/")));
            }
            for v in vals {
                if !values.contains(&v) {
                    values.push(v);
                }
            }
        }
        let mut m = Obj::new();
        if let Some(t) = infer_type_from_values(&values) {
            m.insert("type".into(), json!(t));
        }
        m.insert("enum".into(), Value::Array(values));
        let note = (!notes.is_empty()).then(|| format!("Values: {}.", notes.join("; ")));
        return Some((m, note));
    }

    // Same type everywhere → merge.
    if let Some(t) = common_type(&rest) {
        return Some((merge_same_type(&t, rest), None));
    }

    // Otherwise pick the first object-ish branch, else the most general one.
    let idx = rest
        .iter()
        .position(|b| type_of(b) == Some("object") || b.contains_key("properties"))
        .or_else(|| {
            let best = rest.iter().filter_map(type_of).map(generality_rank).min()?;
            rest.iter().position(|b| type_of(b).map(generality_rank) == Some(best))
        })
        .unwrap_or(0);
    let mut alternatives: Vec<String> = Vec::new();
    for (i, b) in rest.iter().enumerate() {
        let d = describe_branch(b);
        if i != idx && !alternatives.contains(&d) {
            alternatives.push(d);
        }
    }
    let chosen = rest.swap_remove(idx);
    let note = (!alternatives.is_empty()).then(|| format!("Also accepts: {}.", alternatives.join(", ")));
    Some((chosen, note))
}

fn common_type(branches: &[Obj]) -> Option<String> {
    let mut types: Vec<&str> = Vec::new();
    for b in branches {
        types.push(type_of(b)?);
    }
    let first = types[0];
    if types.iter().all(|t| *t == first) {
        return Some(first.to_string());
    }
    if types.iter().all(|t| *t == "integer" || *t == "number") {
        return Some("number".to_string());
    }
    None
}

fn merge_same_type(t: &str, branches: Vec<Obj>) -> Obj {
    let mut out = Obj::new();
    out.insert("type".into(), json!(t));
    if let Some(d) = branches.iter().find_map(|b| b.get("description").cloned()) {
        out.insert("description".into(), d);
    }
    match t {
        "object" => {
            let mut props = Obj::new();
            let mut required: Option<Vec<Value>> = None;
            for b in &branches {
                if let Some(Value::Object(p)) = b.get("properties") {
                    for (name, schema) in p {
                        match (props.get_mut(name), schema) {
                            (Some(Value::Object(existing)), Value::Object(new)) => merge_enum_into(existing, new),
                            (Some(_), _) => {}
                            (None, s) => {
                                props.insert(name.clone(), s.clone());
                            }
                        }
                    }
                }
                let req: Vec<Value> = b.get("required").and_then(Value::as_array).cloned().unwrap_or_default();
                required = Some(match required {
                    None => req,
                    Some(prev) => prev.into_iter().filter(|r| req.contains(r)).collect(),
                });
            }
            out.insert("properties".into(), Value::Object(props));
            if let Some(req) = required.filter(|r| !r.is_empty()) {
                out.insert("required".into(), Value::Array(req));
            }
        }
        "array" => {
            let items: Vec<Obj> =
                branches.iter().filter_map(|b| b.get("items").and_then(Value::as_object).cloned()).collect();
            if let Some(first) = items.first() {
                if items.iter().all(|i| i == first) {
                    out.insert("items".into(), Value::Object(first.clone()));
                } else if let Some((merged, note)) = simplify_union(items) {
                    let mut merged = merged;
                    if let Some(note) = note {
                        append_description(&mut merged, &note);
                    }
                    out.insert("items".into(), Value::Object(merged));
                }
            }
        }
        _ => {
            // Keep constraints all branches agree on; union enums only if every branch has one.
            let first = &branches[0];
            for (k, v) in first {
                if matches!(k.as_str(), "type" | "description" | "enum") {
                    continue;
                }
                if branches.iter().all(|b| b.get(k) == Some(v)) {
                    out.insert(k.clone(), v.clone());
                }
            }
            if branches.iter().all(|b| b.get("enum").is_some_and(Value::is_array)) {
                let mut values: Vec<Value> = Vec::new();
                for b in &branches {
                    for v in b.get("enum").and_then(Value::as_array).into_iter().flatten() {
                        if !values.contains(v) {
                            values.push(v.clone());
                        }
                    }
                }
                out.insert("enum".into(), Value::Array(values));
            }
        }
    }
    out
}

/// Two variants of the same property (discriminated unions): union their enums.
fn merge_enum_into(existing: &mut Obj, new: &Obj) {
    if let (Some(Value::Array(a)), Some(Value::Array(b))) = (existing.get("enum").cloned(), new.get("enum")) {
        let mut values = a;
        for v in b {
            if !values.contains(v) {
                values.push(v.clone());
            }
        }
        existing.insert("enum".into(), Value::Array(values));
    }
}

fn infer_type(o: &Obj) -> Option<&'static str> {
    if o.contains_key("properties") || o.get("additionalProperties").is_some_and(Value::is_object) {
        return Some("object");
    }
    if o.contains_key("items") {
        return Some("array");
    }
    if let Some(Value::Array(values)) = o.get("enum") {
        return infer_type_from_values(values);
    }
    if ["minLength", "maxLength", "pattern", "format"].iter().any(|k| o.contains_key(*k)) {
        return Some("string");
    }
    if ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum", "multipleOf"].iter().any(|k| o.contains_key(*k)) {
        return Some("number");
    }
    if ["minItems", "maxItems", "uniqueItems"].iter().any(|k| o.contains_key(*k)) {
        return Some("array");
    }
    None
}

fn scalar_matches_type(v: &Value, t: &str) -> bool {
    match t {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        _ => false,
    }
}

fn finalize(out: &mut Obj) {
    if let Some(c) = out.remove("const") {
        if !out.contains_key("enum") {
            out.insert("enum".into(), Value::Array(vec![c]));
        }
    }
    if let Some(Value::Array(values)) = out.get_mut("enum") {
        if values.iter().any(|v| !v.is_null()) {
            values.retain(|v| !v.is_null());
        }
    }

    let mut note = None;
    if let Some(Value::Array(types)) = out.get("type").cloned() {
        let mut names: Vec<&str> = Vec::new();
        for t in types.iter().filter_map(Value::as_str) {
            if t != "null" && !names.contains(&t) {
                names.push(t);
            }
        }
        if names.contains(&"number") {
            names.retain(|t| *t != "integer");
        }
        match names.len() {
            0 => {
                out.insert("type".into(), json!("null"));
            }
            1 => {
                out.insert("type".into(), json!(names[0]));
            }
            _ => {
                let pick = *names.iter().min_by_key(|t| generality_rank(t)).unwrap_or(&names[0]);
                let others: Vec<&str> = names.iter().copied().filter(|t| *t != pick).collect();
                out.insert("type".into(), json!(pick));
                note = Some(format!("Also accepts: {}.", others.join(", ")));
            }
        }
    } else if out.get("type").is_some_and(|t| !t.is_string()) {
        out.remove("type");
    }

    if !out.contains_key("type") {
        if let Some(t) = infer_type(out) {
            out.insert("type".into(), json!(t));
        }
    }

    if let Some(Value::Array(req)) = out.get("required") {
        let filtered: Vec<Value> = match out.get("properties") {
            Some(Value::Object(props)) => {
                req.iter().filter(|r| r.as_str().is_some_and(|n| props.contains_key(n))).cloned().collect()
            }
            _ => req.clone(),
        };
        if filtered.is_empty() {
            out.remove("required");
        } else {
            out.insert("required".into(), Value::Array(filtered));
        }
    }

    if let (Some(d), Some(t)) = (out.get("default"), type_of(out)) {
        if !scalar_matches_type(d, t) {
            out.remove("default");
        }
    }

    if let Some(n) = note {
        append_description(out, &n);
    }
}

fn ensure_typed(mut s: Obj) -> Obj {
    if !s.contains_key("type") {
        s.insert("type".into(), json!("string"));
    }
    s
}

fn ensure_top_level_object(mut root: Obj) -> Obj {
    match type_of(&root) {
        None | Some("object") => {
            root.insert("type".into(), json!("object"));
            if !matches!(root.get("properties"), Some(Value::Object(_))) {
                root.insert("properties".into(), Value::Object(Obj::new()));
            }
            root.remove("enum");
            root.remove("items");
            root
        }
        Some(_) => {
            let description = root.get("description").cloned();
            let mut props = Obj::new();
            props.insert(WRAPPED_ARG.into(), Value::Object(root));
            let mut wrapper = Obj::new();
            wrapper.insert("type".into(), json!("object"));
            wrapper.insert("properties".into(), Value::Object(props));
            wrapper.insert("required".into(), json!([WRAPPED_ARG]));
            if let Some(d) = description {
                wrapper.insert("description".into(), d);
            }
            wrapper
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: Value, expected: Value) {
        let out = sanitize_schema(&input);
        assert_eq!(out, expected, "\ninput: {input:#}\nactual: {out:#}");
        // idempotent
        assert_eq!(sanitize_schema(&out), out, "not idempotent for {input}");
    }

    #[test]
    fn inlines_defs_refs() {
        check(
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "$id": "urn:x",
                "type": "object",
                "properties": {"user": {"$ref": "#/$defs/User", "description": "Who"}},
                "required": ["user"],
                "$defs": {"User": {"type": "object", "description": "A user",
                    "properties": {"name": {"type": "string"}}, "required": ["name"]}}
            }),
            json!({
                "type": "object",
                "properties": {"user": {"type": "object", "description": "Who",
                    "properties": {"name": {"type": "string"}}, "required": ["name"]}},
                "required": ["user"]
            }),
        );
    }

    #[test]
    fn inlines_definitions_and_root_ref() {
        check(
            json!({
                "$ref": "#/definitions/Args",
                "definitions": {
                    "Args": {"type": "object", "properties": {"mode": {"$ref": "#/definitions/Mode"}}},
                    "Mode": {"type": "string", "enum": ["a", "b"]}
                }
            }),
            json!({"type": "object", "properties": {"mode": {"type": "string", "enum": ["a", "b"]}}}),
        );
    }

    #[test]
    fn cycles_become_objects() {
        check(
            json!({
                "$ref": "#/$defs/Node",
                "$defs": {"Node": {"type": "object", "properties": {
                    "name": {"type": "string"},
                    "children": {"type": "array", "items": {"$ref": "#/$defs/Node"}}
                }}}
            }),
            json!({"type": "object", "properties": {
                "name": {"type": "string"},
                "children": {"type": "array", "items": {"type": "object"}}
            }}),
        );
        // self reference to the root
        check(
            json!({"type": "object", "properties": {"next": {"$ref": "#", "description": "next node"}}}),
            json!({"type": "object", "properties": {"next": {"type": "object", "description": "next node"}}}),
        );
        // mutual recursion
        let out = sanitize_schema(&json!({
            "type": "object",
            "properties": {"a": {"$ref": "#/$defs/A"}},
            "$defs": {
                "A": {"type": "object", "properties": {"b": {"$ref": "#/$defs/B"}}},
                "B": {"type": "object", "properties": {"a": {"$ref": "#/$defs/A"}}}
            }
        }));
        assert_eq!(out["properties"]["a"]["properties"]["b"]["properties"]["a"], json!({"type": "object"}));
    }

    #[test]
    fn unresolvable_refs_degrade() {
        check(
            json!({"type": "object", "properties": {"x": {"$ref": "https://example.com/s.json"}}}),
            json!({"type": "object", "properties": {"x": {"type": "object"}}}),
        );
    }

    #[test]
    fn nullable_any_of_and_type_arrays() {
        check(
            json!({"type": "object", "properties": {
                "a": {"anyOf": [{"type": "string"}, {"type": "null"}], "default": null, "description": "A"},
                "b": {"type": ["integer", "null"], "default": 3},
                "c": {"oneOf": [{"type": "null"}, {"$ref": "#/$defs/C"}]},
                "d": {"type": "string", "nullable": true, "enum": ["x", null]}
            }, "$defs": {"C": {"type": "object", "properties": {"z": {"type": "boolean"}}}}}),
            json!({"type": "object", "properties": {
                "a": {"description": "A", "type": "string"},
                "b": {"type": "integer", "default": 3},
                "c": {"type": "object", "properties": {"z": {"type": "boolean"}}},
                "d": {"type": "string", "enum": ["x"]}
            }}),
        );
    }

    #[test]
    fn const_unions_become_enums() {
        check(
            json!({"type": "object", "properties": {
                "speed": {"oneOf": [{"const": "fast", "description": "Quick"}, {"const": "slow", "description": "Careful"}]},
                "level": {"anyOf": [{"const": 1}, {"const": 2}, {"type": "null"}]}
            }}),
            json!({"type": "object", "properties": {
                "speed": {"type": "string", "enum": ["fast", "slow"], "description": "Values: fast: Quick; slow: Careful."},
                "level": {"type": "integer", "enum": [1, 2]}
            }}),
        );
    }

    #[test]
    fn same_type_unions_merge() {
        check(
            json!({"type": "object", "properties": {
                "when": {"anyOf": [{"type": "string", "format": "date"}, {"type": "string", "format": "date-time"}]},
                "n": {"anyOf": [{"type": "integer"}, {"type": "number", "minimum": 0}]},
                "shape": {"oneOf": [
                    {"type": "object", "properties": {"kind": {"const": "circle"}, "r": {"type": "number"}}, "required": ["kind", "r"]},
                    {"type": "object", "properties": {"kind": {"const": "square"}, "side": {"type": "number"}}, "required": ["kind", "side"]}
                ]}
            }}),
            json!({"type": "object", "properties": {
                "when": {"type": "string"},
                "n": {"type": "number"},
                "shape": {"type": "object", "properties": {
                    "kind": {"type": "string", "enum": ["circle", "square"]},
                    "r": {"type": "number"},
                    "side": {"type": "number"}
                }, "required": ["kind"]}
            }}),
        );
    }

    #[test]
    fn mixed_unions_pick_object_and_note() {
        let out = sanitize_schema(&json!({"type": "object", "properties": {
            "q": {"description": "Query", "anyOf": [
                {"type": "string"},
                {"type": "object", "properties": {"text": {"type": "string"}}},
                {"type": "array", "items": {"type": "string"}}
            ]},
            "v": {"anyOf": [{"type": "boolean"}, {"type": "array", "items": {"type": "integer"}}, {"type": "integer"}]}
        }}));
        assert_eq!(
            out["properties"]["q"],
            json!({"description": "Query. Also accepts: string, array of string.", "type": "object",
                   "properties": {"text": {"type": "string"}}})
        );
        assert_eq!(
            out["properties"]["v"],
            json!({"type": "array", "items": {"type": "integer"}, "description": "Also accepts: boolean, integer."})
        );
    }

    #[test]
    fn all_of_merges_objects() {
        check(
            json!({"allOf": [
                {"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]},
                {"properties": {"b": {"type": "integer", "description": "B"}}, "required": ["b"]},
                {"$ref": "#/$defs/C"}
            ], "$defs": {"C": {"properties": {"c": {"type": "boolean"}}}}}),
            json!({"type": "object", "properties": {
                "a": {"type": "string"}, "b": {"type": "integer", "description": "B"}, "c": {"type": "boolean"}
            }, "required": ["a", "b"]}),
        );
    }

    #[test]
    fn properties_get_types_and_noise_is_dropped() {
        check(
            json!({"type": "object", "properties": {
                "path": {"description": "File path", "examples": ["a.txt"]},
                "mode": {"enum": ["r", "w"]},
                "count": {"minimum": 1},
                "tags": {"items": {"type": "string"}},
                "meta": {"properties": {"k": {}}},
                "flag": true,
                "obj_default": {"type": "object", "default": {"a": 1}},
                "weird": {"type": "string", "x-ui": {"widget": "text"}, "title": "Weird", "default": 5}
            }, "required": ["path", "missing"], "additionalProperties": false}),
            json!({"type": "object", "properties": {
                "path": {"description": "File path", "type": "string"},
                "mode": {"enum": ["r", "w"], "type": "string"},
                "count": {"minimum": 1, "type": "number"},
                "tags": {"items": {"type": "string"}, "type": "array"},
                "meta": {"properties": {"k": {"type": "string"}}, "type": "object"},
                "flag": {"type": "string"},
                "obj_default": {"type": "object"},
                "weird": {"type": "string", "title": "Weird"}
            }, "required": ["path"], "additionalProperties": false}),
        );
    }

    #[test]
    fn top_level_is_always_an_object() {
        check(json!({}), json!({"type": "object", "properties": {}}));
        check(json!(true), json!({"type": "object", "properties": {}}));
        check(json!({"type": "object"}), json!({"type": "object", "properties": {}}));
        check(
            json!({"type": "string", "description": "Text"}),
            json!({"type": "object", "properties": {"value": {"type": "string", "description": "Text"}},
                   "required": ["value"], "description": "Text"}),
        );
        assert!(root_is_wrapped(&json!({"type": "string"})));
        assert!(!root_is_wrapped(&json!({"type": "object"})));
        assert!(!root_is_wrapped(&json!({})));
    }

    #[test]
    fn tuple_items_and_prefix_items() {
        check(
            json!({"type": "object", "properties": {
                "pair": {"type": "array", "items": [{"type": "string"}, {"type": "integer"}]},
                "p2": {"type": "array", "prefixItems": [{"type": "number"}]}
            }}),
            json!({"type": "object", "properties": {
                "pair": {"type": "array", "items": {"type": "string"}},
                "p2": {"type": "array", "items": {"type": "number"}}
            }}),
        );
    }

    #[test]
    fn json_pointer_escapes() {
        check(
            json!({"type": "object", "properties": {"x": {"$ref": "#/$defs/a~1b"}, "y": {"$ref": "#/$defs/c%20d"}},
                   "$defs": {"a/b": {"type": "integer"}, "c d": {"type": "boolean"}}}),
            json!({"type": "object", "properties": {"x": {"type": "integer"}, "y": {"type": "boolean"}}}),
        );
    }

    #[test]
    fn exponential_refs_are_bounded() {
        // Each level references the next one twice: 2^30 expansions without a budget.
        let mut defs = Map::new();
        for i in 0..30 {
            defs.insert(
                format!("L{i}"),
                json!({"type": "object", "properties": {
                    "l": {"$ref": format!("#/$defs/L{}", i + 1)},
                    "r": {"$ref": format!("#/$defs/L{}", i + 1)}
                }}),
            );
        }
        defs.insert("L30".into(), json!({"type": "string"}));
        let schema = json!({"$ref": "#/$defs/L0", "$defs": defs});
        let out = sanitize_schema(&schema);
        assert_eq!(out["type"], "object");
    }
}
