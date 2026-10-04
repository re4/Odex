//! Asking the utility model for memory proposals and parsing its answer.

use std::collections::HashSet;
use std::path::Path;

use odex_protocol::Memory;
use serde_json::{json, Value};

use crate::{normalize_category, now_ms, one_line, redact_secrets, CATEGORIES};

/// Upper bound on proposals accepted from one extraction.
pub const MAX_PROPOSALS: usize = 5;

const MAX_TEXT_CHARS: usize = 400;

/// A structured-output request for the utility model.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalRequest {
    pub system: String,
    pub user: String,
    /// JSON schema for the reply (`{"memories": [{text, category, scope}]}`).
    pub json_schema: Value,
}

const SYSTEM_PROMPT: &str = "\
You curate a small set of long-lived memories for a coding assistant. Read the conversation digest and pick out \
facts that will still help in future, unrelated sessions:
- preference: how the user likes to work (tone, verbosity, review style, tools they favor or avoid);
- convention: rules of this codebase (naming, layout, testing, formatting, commit or branch style);
- stack: languages, frameworks, package managers, runtimes, services and versions in use;
- other: any other durable fact that clearly matters later.

Do not record details of the current task, temporary state, file contents, one-off decisions, guesses, or anything \
sensitive (credentials, keys, tokens, personal data). Do not repeat anything the existing memories already cover, even \
if worded differently. Return at most 5 memories; returning none is fine and often right.

Write each memory as one short, self-contained sentence (\"Prefers ...\", \"The project uses ...\"). Use scope \
\"global\" for facts about the user that apply everywhere and \"project\" for facts about this codebase. \
Reply with JSON only.";

/// Build the extraction prompt for the utility model.
pub fn proposal_request(transcript_digest: &str, existing: &[Memory]) -> ProposalRequest {
    let mut user = String::from("Existing memories (do not repeat these):\n");
    if existing.is_empty() {
        user.push_str("(none)\n");
    } else {
        for m in existing {
            user.push_str(&format!("- [{}/{}] {}\n", m.scope, m.category, one_line(&m.text)));
        }
    }
    user.push_str("\nConversation digest:\n<<<\n");
    user.push_str(redact_secrets(transcript_digest.trim()).as_str());
    user.push_str("\n>>>\n\nReturn {\"memories\": [...]} with up to 5 new memories.");

    let json_schema = json!({
        "type": "object",
        "properties": {
            "memories": {
                "type": "array",
                "maxItems": MAX_PROPOSALS,
                "items": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "One short self-contained sentence." },
                        "category": { "type": "string", "enum": CATEGORIES },
                        "scope": { "type": "string", "enum": ["global", "project"] }
                    },
                    "required": ["text", "category", "scope"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["memories"],
        "additionalProperties": false
    });
    ProposalRequest { system: SYSTEM_PROMPT.to_string(), user, json_schema }
}

/// Turn the model's reply into `proposed` memories.
///
/// Tolerates common deviations: a bare array, other wrapper keys
/// (`proposals`, `items`, `facts`), a JSON document inside a string (with or
/// without code fences), plain strings instead of objects, alternative field
/// names (`memory`, `fact`, `content`, `type`, `kind`) and unknown
/// category/scope values. Duplicates and empty entries are dropped and at
/// most [`MAX_PROPOSALS`] are returned.
pub fn parse_proposals(json: &Value, project_path: Option<&Path>, source_thread_id: Option<&str>) -> Vec<Memory> {
    let now = now_ms();
    let project =
        project_path.map(|p| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().into_owned());
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in collect_items(json, 0) {
        let (text, category, scope) = match &item {
            Value::String(s) => (s.clone(), String::new(), String::new()),
            Value::Object(o) => {
                let pick = |keys: &[&str]| {
                    keys.iter().find_map(|k| o.get(*k).and_then(Value::as_str)).unwrap_or_default().to_string()
                };
                (
                    pick(&["text", "memory", "fact", "content", "statement", "value", "description"]),
                    pick(&["category", "type", "kind"]),
                    pick(&["scope", "level", "applies_to"]),
                )
            }
            _ => continue,
        };
        let text = clean_text(&text);
        if text.is_empty() {
            continue;
        }
        let key: String = text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        let category = normalize_category(&category);
        let wants_project = match scope.trim().to_ascii_lowercase().as_str() {
            "project" | "repo" | "repository" | "workspace" | "codebase" | "local" => true,
            "global" | "user" | "personal" | "all" | "everywhere" => false,
            _ => matches!(category, "convention" | "stack"),
        };
        let project_path = if wants_project { project.clone() } else { None };
        out.push(Memory {
            id: uuid::Uuid::new_v4().to_string(),
            text,
            scope: if project_path.is_some() { "project" } else { "global" }.to_string(),
            project_path,
            status: "proposed".to_string(),
            category: category.to_string(),
            source_thread_id: source_thread_id.map(str::to_string),
            created_at: now,
            updated_at: now,
        });
        if out.len() == MAX_PROPOSALS {
            break;
        }
    }
    out
}

fn collect_items(v: &Value, depth: usize) -> Vec<Value> {
    if depth > 4 {
        return Vec::new();
    }
    match v {
        Value::Array(items) => items.clone(),
        Value::Object(o) => {
            for key in ["memories", "proposals", "items", "facts", "results", "data", "output"] {
                if let Some(inner) = o.get(key) {
                    return collect_items(inner, depth + 1);
                }
            }
            if ["text", "memory", "fact", "content"].iter().any(|k| o.contains_key(*k)) {
                vec![v.clone()]
            } else {
                Vec::new()
            }
        }
        Value::String(s) => {
            let body = strip_fences(s);
            match serde_json::from_str::<Value>(body) {
                Ok(parsed) if !parsed.is_string() => collect_items(&parsed, depth + 1),
                _ => body
                    .lines()
                    .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
                    .map(|l| l.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['.', ')']).trim())
                    .filter(|l| !l.is_empty())
                    .map(|l| Value::String(l.to_string()))
                    .collect(),
            }
        }
        _ => Vec::new(),
    }
}

fn strip_fences(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let rest = rest.split_once('\n').map(|(_, body)| body).unwrap_or(rest);
    rest.trim_end().strip_suffix("```").unwrap_or(rest).trim()
}

fn clean_text(raw: &str) -> String {
    let text = one_line(&redact_secrets(raw));
    let text = text.trim_start_matches(['-', '*', '•']).trim().to_string();
    if text.chars().count() <= MAX_TEXT_CHARS {
        return text;
    }
    let mut cut: String = text.chars().take(MAX_TEXT_CHARS - 1).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn existing() -> Vec<Memory> {
        vec![Memory {
            id: "m1".into(),
            text: "Prefers pnpm".into(),
            scope: "global".into(),
            project_path: None,
            status: "approved".into(),
            category: "preference".into(),
            source_thread_id: None,
            created_at: 1,
            updated_at: 1,
        }]
    }

    #[test]
    fn request_contains_schema_existing_and_digest() {
        let r = proposal_request("User said: always run cargo fmt. api_key=sekrit", &existing());
        assert!(r.system.contains("at most 5"));
        assert!(r.user.contains("Prefers pnpm"));
        assert!(r.user.contains("always run cargo fmt"));
        assert!(!r.user.contains("sekrit"), "digest is redacted");
        assert_eq!(r.json_schema["properties"]["memories"]["maxItems"], 5);
        assert_eq!(
            r.json_schema["properties"]["memories"]["items"]["properties"]["category"]["enum"],
            json!(["preference", "convention", "stack", "other"])
        );
        assert!(proposal_request("x", &[]).user.contains("(none)"));
    }

    #[test]
    fn parses_well_formed_reply() {
        let project = PathBuf::from("/work/app");
        let reply = json!({"memories": [
            {"text": "Prefers terse commit messages", "category": "preference", "scope": "global"},
            {"text": "Tests live next to sources", "category": "convention", "scope": "project"}
        ]});
        let got = parse_proposals(&reply, Some(&project), Some("th_9"));
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|m| m.status == "proposed" && !m.id.is_empty()));
        assert_eq!(got[0].scope, "global");
        assert_eq!(got[0].project_path, None);
        assert_eq!(got[1].scope, "project");
        assert!(got[1].project_path.as_deref().unwrap().ends_with("app"));
        assert_eq!(got[1].source_thread_id.as_deref(), Some("th_9"));
    }

    #[test]
    fn tolerates_schema_deviations() {
        // Bare array of strings and objects with alternative keys.
        let reply = json!([
            "Uses Rust 2021 edition",
            {"memory": "Likes small PRs", "type": "Preferences"},
            {"fact": "Deploys with Docker", "kind": "tooling", "scope": "repo"},
            {"text": "  "},
            {"text": "likes small PRs!"},
            42
        ]);
        let got = parse_proposals(&reply, None, None);
        let texts: Vec<&str> = got.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["Uses Rust 2021 edition", "Likes small PRs", "Deploys with Docker"]);
        assert_eq!(got[1].category, "preference");
        assert_eq!(got[2].category, "stack");
        // No project path -> everything is global.
        assert!(got.iter().all(|m| m.scope == "global" && m.project_path.is_none()));

        // JSON inside a fenced string, with a different wrapper key.
        let fenced =
            Value::String("```json\n{\"proposals\": [{\"text\": \"Uses tabs\", \"category\": \"weird\"}]}\n```".into());
        let got = parse_proposals(&fenced, Some(Path::new("/p")), None);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].category, "other");
        assert_eq!(got[0].scope, "global");

        // Plain bullet list text.
        let bullets = Value::String("- Prefers dark mode\n2. Uses Vite".into());
        let got = parse_proposals(&bullets, None, None);
        assert_eq!(got.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["Prefers dark mode", "Uses Vite"]);

        // Single object, caps at five, redaction applies.
        let one = json!({"text": "CI token: abcdef123456", "category": "stack"});
        let got = parse_proposals(&one, Some(Path::new("/p")), None);
        assert_eq!(got[0].text, "CI token: [REDACTED]");
        assert_eq!(got[0].scope, "project", "stack facts default to the project");
        let many = json!({"memories": (0..9).map(|i| format!("fact number {i}")).collect::<Vec<_>>()});
        assert_eq!(parse_proposals(&many, None, None).len(), MAX_PROPOSALS);

        assert!(parse_proposals(&json!(null), None, None).is_empty());
        assert!(parse_proposals(&json!({"unrelated": true}), None, None).is_empty());
    }
}
