//! Minimal JSON Schema validation for tool arguments, producing precise,
//! model-friendly error messages ("`path` is required", "`limit` must be an
//! integer, got string"). Covers the subset tool schemas actually use.

use serde_json::Value;

pub fn validate(schema: &Value, value: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    check(schema, value, "", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn type_name(v: &Value) -> &'static str {
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

fn matches_type(t: &str, v: &Value) -> bool {
    match t {
        "null" => v.is_null(),
        "boolean" => v.is_boolean(),
        "integer" => {
            v.as_i64().is_some() || v.as_u64().is_some() || v.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false)
        }
        "number" => v.is_number(),
        "string" => v.is_string(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    }
}

fn label(path: &str) -> String {
    if path.is_empty() {
        "arguments".to_string()
    } else {
        format!("`{path}`")
    }
}

fn check(schema: &Value, v: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(obj) = schema.as_object() else { return };

    // anyOf / oneOf: accept if any branch validates
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = obj.get(key).and_then(|b| b.as_array()) {
            let ok = branches.iter().any(|b| {
                let mut e = Vec::new();
                check(b, v, path, &mut e);
                e.is_empty()
            });
            if !ok {
                errors.push(format!("{} does not match any allowed form", label(path)));
            }
            return;
        }
    }
    if let Some(all) = obj.get("allOf").and_then(|b| b.as_array()) {
        for b in all {
            check(b, v, path, errors);
        }
    }

    match obj.get("type") {
        Some(Value::String(t)) => {
            if !matches_type(t, v) {
                errors.push(format!("{} must be {} {}, got {}", label(path), article(t), t, type_name(v)));
                return;
            }
        }
        Some(Value::Array(ts)) => {
            let ok = ts.iter().filter_map(|t| t.as_str()).any(|t| matches_type(t, v));
            if !ok {
                let names: Vec<&str> = ts.iter().filter_map(|t| t.as_str()).collect();
                errors.push(format!(
                    "{} must be one of types [{}], got {}",
                    label(path),
                    names.join(", "),
                    type_name(v)
                ));
                return;
            }
        }
        _ => {}
    }

    if let Some(e) = obj.get("enum").and_then(|e| e.as_array()) {
        if !e.contains(v) {
            let opts: Vec<String> = e.iter().map(|x| x.to_string()).collect();
            errors.push(format!("{} must be one of {}, got {}", label(path), opts.join(", "), v));
        }
    }
    if let Some(c) = obj.get("const") {
        if c != v {
            errors.push(format!("{} must equal {c}", label(path)));
        }
    }

    match v {
        Value::Object(map) => {
            if let Some(req) = obj.get("required").and_then(|r| r.as_array()) {
                for r in req.iter().filter_map(|r| r.as_str()) {
                    if !map.contains_key(r) || map.get(r).map(|x| x.is_null()).unwrap_or(false) && !allows_null(obj, r)
                    {
                        errors.push(format!("{} is required", label(&join(path, r))));
                    }
                }
            }
            let props = obj.get("properties").and_then(|p| p.as_object());
            if let Some(props) = props {
                for (k, sub) in props {
                    if let Some(val) = map.get(k) {
                        if val.is_null()
                            && !obj
                                .get("required")
                                .and_then(|r| r.as_array())
                                .map(|r| r.iter().any(|x| x == k))
                                .unwrap_or(false)
                        {
                            continue; // optional field explicitly null: tolerate
                        }
                        check(sub, val, &join(path, k), errors);
                    }
                }
            }
            if obj.get("additionalProperties") == Some(&Value::Bool(false)) {
                if let Some(props) = props {
                    for k in map.keys() {
                        if !props.contains_key(k) {
                            let known: Vec<&String> = props.keys().collect();
                            errors.push(format!(
                                "unknown property {} (allowed: {})",
                                label(&join(path, k)),
                                known.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                            ));
                        }
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = obj.get("items") {
                for (i, it) in items.iter().enumerate() {
                    check(item_schema, it, &format!("{path}[{i}]"), errors);
                }
            }
            if let Some(min) = obj.get("minItems").and_then(|m| m.as_u64()) {
                if (items.len() as u64) < min {
                    errors.push(format!("{} needs at least {min} items", label(path)));
                }
            }
        }
        Value::String(s) => {
            if let Some(min) = obj.get("minLength").and_then(|m| m.as_u64()) {
                if (s.chars().count() as u64) < min {
                    errors.push(format!("{} must be at least {min} characters", label(path)));
                }
            }
        }
        Value::Number(n) => {
            if let (Some(min), Some(x)) = (obj.get("minimum").and_then(|m| m.as_f64()), n.as_f64()) {
                if x < min {
                    errors.push(format!("{} must be >= {min}", label(path)));
                }
            }
            if let (Some(max), Some(x)) = (obj.get("maximum").and_then(|m| m.as_f64()), n.as_f64()) {
                if x > max {
                    errors.push(format!("{} must be <= {max}", label(path)));
                }
            }
        }
        _ => {}
    }
}

fn allows_null(obj: &serde_json::Map<String, Value>, key: &str) -> bool {
    obj.get("properties")
        .and_then(|p| p.get(key))
        .map(|s| match s.get("type") {
            Some(Value::String(t)) => t == "null",
            Some(Value::Array(ts)) => ts.iter().any(|t| t == "null"),
            _ => false,
        })
        .unwrap_or(false)
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn article(t: &str) -> &'static str {
    match t.chars().next() {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

/// Coerce common type slips in-place before validation (stringified numbers
/// and booleans, single values where arrays are expected). Returns true if
/// anything changed.
pub fn coerce(schema: &Value, v: &mut Value) -> bool {
    let mut changed = false;
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else { return false };
    let Some(map) = v.as_object_mut() else { return false };
    for (k, sub) in props {
        let Some(val) = map.get_mut(k) else { continue };
        let t = sub.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match (t, &*val) {
            ("integer", Value::String(s)) => {
                if let Ok(n) = s.trim().parse::<i64>() {
                    *val = Value::from(n);
                    changed = true;
                }
            }
            ("number", Value::String(s)) => {
                if let Ok(n) = s.trim().parse::<f64>() {
                    *val = serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null);
                    changed = true;
                }
            }
            ("boolean", Value::String(s)) => match s.trim().to_lowercase().as_str() {
                "true" | "yes" | "1" => {
                    *val = Value::Bool(true);
                    changed = true;
                }
                "false" | "no" | "0" => {
                    *val = Value::Bool(false);
                    changed = true;
                }
                _ => {}
            },
            ("string", Value::Number(n)) => {
                *val = Value::String(n.to_string());
                changed = true;
            }
            ("array", Value::String(s)) => {
                let s = s.clone();
                if let Ok(arr @ Value::Array(_)) = serde_json::from_str::<Value>(&s) {
                    *val = arr;
                } else {
                    *val = Value::Array(vec![Value::String(s)]);
                }
                changed = true;
            }
            ("object", Value::String(s)) => {
                if let Ok(obj @ Value::Object(_)) = serde_json::from_str::<Value>(s) {
                    *val = obj;
                    changed = true;
                }
            }
            ("integer", Value::Number(n)) if n.as_i64().is_none() => {
                if let Some(f) = n.as_f64() {
                    if f.fract() == 0.0 {
                        *val = Value::from(f as i64);
                        changed = true;
                    }
                }
            }
            _ => {}
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1},
                "mode": {"type": "string", "enum": ["a", "b"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "opt": {"type": ["string", "null"]}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    #[test]
    fn ok() {
        assert!(validate(&schema(), &json!({"path": "a", "limit": 3, "tags": ["x"]})).is_ok());
    }

    #[test]
    fn errors_are_precise() {
        let e = validate(&schema(), &json!({"limit": "x", "mode": "c", "zzz": 1})).unwrap_err();
        assert!(e.iter().any(|m| m == "`path` is required"), "{e:?}");
        assert!(e.iter().any(|m| m.contains("`limit` must be an integer, got string")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("`mode` must be one of")), "{e:?}");
        assert!(e.iter().any(|m| m.contains("unknown property `zzz`")), "{e:?}");
    }

    #[test]
    fn nested_items() {
        let e = validate(&schema(), &json!({"path": "a", "tags": ["x", 2]})).unwrap_err();
        assert_eq!(e, vec!["`tags[1]` must be a string, got integer"]);
    }

    #[test]
    fn coercion() {
        let mut v = json!({"path": 5, "limit": "10", "tags": "one"});
        assert!(coerce(&schema(), &mut v));
        assert_eq!(v, json!({"path": "5", "limit": 10, "tags": ["one"]}));
        assert!(validate(&schema(), &v).is_ok());
    }

    #[test]
    fn any_of() {
        let s = json!({"anyOf": [{"type": "string"}, {"type": "integer"}]});
        assert!(validate(&s, &json!(1)).is_ok());
        assert!(validate(&s, &json!([1])).is_err());
    }
}
