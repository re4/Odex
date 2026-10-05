//! Minimal JSON Schema validator for tool arguments.
//!
//! Checks what models usually get wrong — missing required properties,
//! primitive type mismatches, enum/const values — and produces messages the
//! model can act on. Everything it does not understand is accepted; the
//! server stays the final authority.

use serde_json::Value;

const MAX_DEPTH: usize = 64;
const MAX_ERRORS: usize = 8;

/// Validate `args` against the tool's original `schema`.
/// Returns a `; `-separated list of precise problems.
pub fn validate_arguments(schema: &Value, args: &Value) -> Result<(), String> {
    let mut v = Validator { root: schema, errors: Vec::new() };
    v.check(schema, args, &Path::root(), 0);
    if v.errors.is_empty() {
        Ok(())
    } else {
        v.errors.truncate(MAX_ERRORS);
        Err(v.errors.join("; "))
    }
}

#[derive(Clone)]
struct Path(String);

impl Path {
    fn root() -> Self {
        Path(String::new())
    }
    fn prop(&self, name: &str) -> Path {
        if self.0.is_empty() {
            Path(name.to_string())
        } else {
            Path(format!("{}.{name}", self.0))
        }
    }
    fn index(&self, i: usize) -> Path {
        Path(format!("{}[{i}]", self.0))
    }
    fn label(&self) -> String {
        if self.0.is_empty() {
            "arguments".to_string()
        } else {
            format!("`{}`", self.0)
        }
    }
}

struct Validator<'a> {
    root: &'a Value,
    errors: Vec<String>,
}

fn value_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn preview(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 60 {
        let cut: String = s.chars().take(57).collect();
        format!("{cut}...")
    } else {
        s
    }
}

fn matches_type(t: &str, v: &Value) -> bool {
    match t {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0 && f.is_finite()),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    }
}

fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

impl<'a> Validator<'a> {
    fn resolve(&self, r: &str) -> Option<&'a Value> {
        let root: &'a Value = self.root;
        let frag = r.strip_prefix('#')?;
        if frag.is_empty() {
            return Some(root);
        }
        let mut cur = root;
        for raw in frag.strip_prefix('/')?.split('/') {
            let tok = raw.replace("~1", "/").replace("~0", "~");
            cur = match cur {
                Value::Object(m) => m.get(&tok)?,
                Value::Array(a) => a.get(tok.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    fn check(&mut self, schema: &Value, value: &Value, path: &Path, depth: usize) {
        if depth > MAX_DEPTH || self.errors.len() >= MAX_ERRORS {
            return;
        }
        let Value::Object(s) = schema else {
            if schema == &Value::Bool(false) {
                self.errors.push(format!("{}: no value is allowed here", path.label()));
            }
            return;
        };

        if let Some(r) = s.get("$ref").and_then(Value::as_str) {
            if let Some(target) = self.resolve(r) {
                self.check(target, value, path, depth + 1);
            }
        }

        // type
        if let Some(t) = s.get("type") {
            let types: Vec<&str> = match t {
                Value::String(one) => vec![one.as_str()],
                Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            if !types.is_empty() && !types.iter().any(|t| matches_type(t, value)) {
                let expected = types.join(" or ");
                self.errors.push(format!(
                    "{}: expected {expected}, got {} {}",
                    path.label(),
                    value_kind(value),
                    preview(value)
                ));
                return;
            }
        }

        // enum / const
        if let Some(Value::Array(options)) = s.get("enum") {
            if !options.iter().any(|o| json_eq(o, value)) {
                let opts: Vec<String> = options.iter().map(preview).collect();
                self.errors.push(format!("{}: {} is not one of [{}]", path.label(), preview(value), opts.join(", ")));
                return;
            }
        }
        if let Some(c) = s.get("const") {
            if !json_eq(c, value) {
                self.errors.push(format!("{}: must be {}, got {}", path.label(), preview(c), preview(value)));
                return;
            }
        }

        // objects
        if let Value::Object(obj) = value {
            if let Some(Value::Array(required)) = s.get("required") {
                for name in required.iter().filter_map(Value::as_str) {
                    if !obj.contains_key(name) {
                        self.errors.push(format!("missing required property `{}`", path.prop(name).0));
                    }
                }
            }
            if let Some(Value::Object(props)) = s.get("properties") {
                for (name, sub) in props {
                    if let Some(child) = obj.get(name) {
                        self.check(sub, child, &path.prop(name), depth + 1);
                    }
                }
            }
        }

        // arrays
        if let (Value::Array(items), Some(item_schema)) = (value, s.get("items")) {
            if item_schema.is_object() {
                for (i, item) in items.iter().enumerate() {
                    self.check(item_schema, item, &path.index(i), depth + 1);
                }
            }
        }

        // combinators
        if let Some(Value::Array(all)) = s.get("allOf") {
            for sub in all {
                self.check(sub, value, path, depth + 1);
            }
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(Value::Array(branches)) = s.get(key) {
                if branches.is_empty() {
                    continue;
                }
                let mut first_failure: Option<String> = None;
                let mut ok = false;
                for b in branches {
                    let mut sub = Validator { root: self.root, errors: Vec::new() };
                    sub.check(b, value, path, depth + 1);
                    if sub.errors.is_empty() {
                        ok = true;
                        break;
                    }
                    if first_failure.is_none() {
                        first_failure = sub.errors.into_iter().next();
                    }
                }
                if !ok {
                    let detail = first_failure.map(|f| format!(" (e.g. {f})")).unwrap_or_default();
                    self.errors.push(format!(
                        "{}: {} does not match any allowed alternative{detail}",
                        path.label(),
                        preview(value)
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "a": {"type": "number"},
                "n": {"type": "integer"},
                "mode": {"type": "string", "enum": ["fast", "slow"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "opt": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                "user": {"$ref": "#/$defs/User"}
            },
            "required": ["a", "mode"],
            "$defs": {"User": {"type": "object", "properties": {"id": {"type": "integer"}}, "required": ["id"]}}
        })
    }

    #[test]
    fn accepts_valid() {
        let ok = json!({"a": 1.5, "n": 3.0, "mode": "fast", "tags": ["x"], "opt": null, "user": {"id": 7}, "extra": 1});
        assert_eq!(validate_arguments(&schema(), &ok), Ok(()));
        assert_eq!(validate_arguments(&json!({}), &json!({"anything": true})), Ok(()));
    }

    #[test]
    fn reports_precise_errors() {
        let err = validate_arguments(&schema(), &json!({"a": "1", "n": 1.5})).unwrap_err();
        assert!(err.contains("missing required property `mode`"), "{err}");
        assert!(err.contains("`a`: expected number, got string \"1\""), "{err}");
        assert!(err.contains("`n`: expected integer, got number 1.5"), "{err}");

        let err = validate_arguments(&schema(), &json!({"a": 1, "mode": "medium"})).unwrap_err();
        assert_eq!(err, "`mode`: \"medium\" is not one of [\"fast\", \"slow\"]");

        let err = validate_arguments(&schema(), &json!({"a": 1, "mode": "slow", "tags": ["x", 2]})).unwrap_err();
        assert_eq!(err, "`tags[1]`: expected string, got integer 2");

        let err = validate_arguments(&schema(), &json!({"a": 1, "mode": "slow", "user": {}})).unwrap_err();
        assert_eq!(err, "missing required property `user.id`");

        let err = validate_arguments(&schema(), &json!({"a": 1, "mode": "slow", "opt": 5})).unwrap_err();
        assert!(err.starts_with("`opt`: 5 does not match any allowed alternative"), "{err}");

        let err = validate_arguments(&schema(), &json!("nope")).unwrap_err();
        assert_eq!(err, "arguments: expected object, got string \"nope\"");
    }

    #[test]
    fn cyclic_refs_terminate() {
        let s = json!({"$ref": "#"});
        assert_eq!(validate_arguments(&s, &json!({})), Ok(()));
    }
}
