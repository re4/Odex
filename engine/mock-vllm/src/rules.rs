//! JSON rule policies for out-of-process tests.
//!
//! ```json
//! {"rules": [
//!   {"when": {"last_user_contains": "create hello", "last_role": "user"},
//!    "reply": {"kind": "tool_calls", "calls": [{"name": "write_file", "arguments": {"path": "hello.txt", "content": "hi"}}]}},
//!   {"when": {"last_role": "tool"}, "reply": {"kind": "text", "text": "Done."}},
//!   {"when": {}, "reply": {"kind": "text", "text": "OK"}}
//! ]}
//! ```
//! Conditions (all must hold): `last_role`, `last_user_contains`,
//! `last_user_regex`, `last_text_contains`, `any_contains`, `system_contains`,
//! `structured` (bool), `structured_name`, `has_tool` (tool name offered),
//! `tool_results_since_user` (exact count), `min_tool_results_since_user`.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{MockReply, RecordedRequest};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct When {
    pub last_role: Option<String>,
    pub last_user_contains: Option<String>,
    pub last_user_regex: Option<String>,
    pub last_text_contains: Option<String>,
    pub any_contains: Option<String>,
    pub system_contains: Option<String>,
    pub structured: Option<bool>,
    pub structured_name: Option<String>,
    pub has_tool: Option<String>,
    pub tool_results_since_user: Option<usize>,
    pub min_tool_results_since_user: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub when: When,
    pub reply: MockReply,
    /// Use this rule at most N times.
    #[serde(default)]
    pub times: Option<usize>,
}

#[derive(Debug)]
pub struct RulePolicy {
    rules: Vec<(Rule, Option<Regex>)>,
    used: std::sync::Mutex<Vec<usize>>,
}

impl RulePolicy {
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let rules_v = v.get("rules").cloned().unwrap_or_else(|| v.clone());
        let rules: Vec<Rule> = serde_json::from_value(rules_v).map_err(|e| format!("bad rules: {e}"))?;
        let mut out = Vec::new();
        for r in rules {
            let re = match &r.when.last_user_regex {
                Some(p) => Some(Regex::new(p).map_err(|e| format!("bad regex {p}: {e}"))?),
                None => None,
            };
            out.push((r, re));
        }
        let n = out.len();
        Ok(Self { rules: out, used: std::sync::Mutex::new(vec![0; n]) })
    }

    pub fn reply(&self, req: &RecordedRequest) -> MockReply {
        let mut used = self.used.lock().unwrap();
        for (i, (rule, re)) in self.rules.iter().enumerate() {
            if let Some(t) = rule.times {
                if used[i] >= t {
                    continue;
                }
            }
            if matches(&rule.when, re.as_ref(), req) {
                used[i] += 1;
                return rule.reply.clone();
            }
        }
        crate::default_reply(req)
    }
}

fn matches(w: &When, re: Option<&Regex>, req: &RecordedRequest) -> bool {
    if let Some(r) = &w.last_role {
        if req.last_role().as_deref() != Some(r.as_str()) {
            return false;
        }
    }
    if let Some(s) = &w.last_user_contains {
        if !req.last_user_text().to_lowercase().contains(&s.to_lowercase()) {
            return false;
        }
    }
    if let Some(re) = re {
        if !re.is_match(&req.last_user_text()) {
            return false;
        }
    }
    if let Some(s) = &w.last_text_contains {
        if !req.last_text().contains(s.as_str()) {
            return false;
        }
    }
    if let Some(s) = &w.any_contains {
        if !req.all_text().contains(s.as_str()) {
            return false;
        }
    }
    if let Some(s) = &w.system_contains {
        if !req.system_text().contains(s.as_str()) {
            return false;
        }
    }
    if let Some(b) = w.structured {
        if req.structured_name().is_some() != b {
            return false;
        }
    }
    if let Some(n) = &w.structured_name {
        if req.structured_name().as_deref() != Some(n.as_str()) {
            return false;
        }
    }
    if let Some(t) = &w.has_tool {
        if !req.tool_names().iter().any(|x| x == t) {
            return false;
        }
    }
    if let Some(n) = w.tool_results_since_user {
        if req.tool_results_since_user() != n {
            return false;
        }
    }
    if let Some(n) = w.min_tool_results_since_user {
        if req.tool_results_since_user() < n {
            return false;
        }
    }
    true
}

/// A minimal value that satisfies a JSON schema (for default structured replies).
pub fn example_for_schema(schema: &Value) -> Value {
    if let Some(e) = schema.get("enum").and_then(|e| e.as_array()).and_then(|a| a.first()) {
        return e.clone();
    }
    if let Some(c) = schema.get("const") {
        return c.clone();
    }
    for k in ["anyOf", "oneOf"] {
        if let Some(first) = schema.get(k).and_then(|a| a.as_array()).and_then(|a| a.first()) {
            return example_for_schema(first);
        }
    }
    let ty = match schema.get("type") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => {
            a.iter().filter_map(|t| t.as_str()).find(|t| *t != "null").unwrap_or("null").to_string()
        }
        _ => {
            if schema.get("properties").is_some() {
                "object".into()
            } else {
                "string".into()
            }
        }
    };
    match ty.as_str() {
        "object" => {
            let mut m = serde_json::Map::new();
            if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
                for (k, v) in props {
                    m.insert(k.clone(), example_for_schema(v));
                }
            }
            Value::Object(m)
        }
        "array" => {
            let min = schema.get("minItems").and_then(|m| m.as_u64()).unwrap_or(0);
            let item = schema.get("items").map(example_for_schema).unwrap_or(json!("mock"));
            Value::Array((0..min).map(|_| item.clone()).collect())
        }
        "integer" => json!(0),
        "number" => json!(0.0),
        "boolean" => json!(false),
        "null" => Value::Null,
        _ => json!("mock"),
    }
}
