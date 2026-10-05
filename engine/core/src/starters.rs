//! Context-aware starter prompts for the home composer (utility model).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use odex_git::Git;
use odex_llm::types::{ChatMessage, ChatRequest, StructuredOutput};
use odex_protocol::*;

use crate::engine::Engine;

const SYSTEM: &str = "You suggest starter tasks for a coding agent that works inside a software project. \
Each suggestion is one short imperative prompt (under 12 words) the user could send as-is, specific to this \
project. Reply with JSON only.";

fn head(path: &Path, max_chars: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let t: String = text.chars().take(max_chars).collect();
    (!t.trim().is_empty()).then_some(t)
}

/// What the utility model sees: README / AGENTS.md heads, top-level files, git state.
async fn project_context(project: &Project, root: &Path) -> String {
    let mut ctx = format!("Project name: {}\n", project.name);
    if let Ok(listing) = odex_file_search::list_dir(root, 1, 60) {
        ctx.push_str(&format!("\nTop-level entries:\n{listing}\n"));
    }
    for name in ["README.md", "README", "readme.md", "AGENTS.md"] {
        if let Some(t) = head(&root.join(name), 1500) {
            ctx.push_str(&format!("\n<{name}>\n{t}\n</{name}>\n"));
        }
    }
    let g = Git::new(root);
    if g.is_repo().await {
        if let Ok(st) = g.status().await {
            ctx.push_str(&format!(
                "\nGit branch: {}; {} changed file(s)",
                st.branch.unwrap_or_else(|| "(detached)".into()),
                st.files.len()
            ));
            let changed: Vec<String> =
                st.files.iter().take(15).map(|f| format!("{} {}", f.code.trim(), f.path)).collect();
            if !changed.is_empty() {
                ctx.push_str(&format!(":\n{}", changed.join("\n")));
            }
            ctx.push('\n');
        }
        if let Ok(log) = g.log(5).await {
            let subjects: Vec<String> = log.into_iter().map(|c| format!("- {}", c.subject)).collect();
            if !subjects.is_empty() {
                ctx.push_str(&format!("Recent commits:\n{}\n", subjects.join("\n")));
            }
        }
    }
    ctx
}

fn clean(list: &serde_json::Value) -> Vec<String> {
    list.get("prompts")
        .and_then(|s| s.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().trim_matches('"').trim().to_string()))
                .filter(|s| !s.is_empty() && s.chars().count() <= 120)
                .take(4)
                .collect()
        })
        .unwrap_or_default()
}

/// Up to 4 starter prompts for `project`; empty when the feature is off, no
/// utility model is reachable, or the model's reply can't be parsed.
pub async fn suggest(engine: &Engine, project: &Project) -> Vec<String> {
    let root = PathBuf::from(project.primary_folder());
    if !engine.settings_for(Some(&root)).follow_up_suggestions {
        return vec![];
    }
    let Some(h) = engine.role_model(ModelRole::Utility, None) else { return vec![] };
    let user = format!(
        "{}\nSuggest 4 starter prompts for this project (explain, fix, test, review, improve...). JSON only: {{\"prompts\": [...]}}",
        project_context(project, &root).await
    );
    let req = ChatRequest {
        messages: vec![ChatMessage::system(SYSTEM), ChatMessage::user(&user)],
        max_tokens: Some(300),
        structured: Some(StructuredOutput {
            name: "starter_prompts".into(),
            schema: json!({"type": "object", "properties": {"prompts": {"type": "array", "items": {"type": "string"}, "maxItems": 4}}, "required": ["prompts"]}),
        }),
        effort: Some(ReasoningEffort::None),
        temperature_override: Some(0.5),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let out = match tokio::time::timeout(Duration::from_secs(25), h.client.chat(&h.model, &req, &cancel)).await {
        Ok(Ok(r)) => {
            if let Some(u) = r.usage {
                engine.store.record_usage(&h.model.key, &u);
            }
            r.content
        }
        Ok(Err(e)) => {
            tracing::debug!("starter prompts failed: {e}");
            return vec![];
        }
        Err(_) => return vec![],
    };
    match odex_llm::repair::parse_lenient(&out) {
        Ok((v, _)) => clean(&v),
        Err(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_prompts() {
        let v = json!({"prompts": ["  Explain the parser ", "", "\"Add tests\"", "a", "b", "c"]});
        assert_eq!(clean(&v), vec!["Explain the parser", "Add tests", "a", "b"]);
        assert!(clean(&json!({"x": 1})).is_empty());
    }
}
