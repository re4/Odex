//! Utility-model side jobs: titles, follow-up suggestions, goal continuation.

use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use odex_llm::types::{ChatMessage, ChatRequest, StructuredOutput};
use odex_protocol::*;

use crate::engine::Engine;
use crate::thread::{now_ms, ThreadRt};
use crate::turn::{spawn_turn, Finished, TurnOpts};

async fn utility(
    engine: &Engine,
    t: &Thread,
    system: &str,
    user: &str,
    schema: Option<serde_json::Value>,
    max_tokens: u32,
) -> Option<String> {
    let h = engine.role_model(ModelRole::Utility, Some(t))?;
    let req = ChatRequest {
        messages: vec![ChatMessage::system(system), ChatMessage::user(user)],
        max_tokens: Some(max_tokens),
        structured: schema.map(|s| StructuredOutput { name: "utility".into(), schema: s }),
        effort: Some(ReasoningEffort::None),
        temperature_override: Some(0.3),
        ..Default::default()
    };
    match h.client.chat(&h.model, &req, &CancellationToken::new()).await {
        Ok(r) => {
            if let Some(u) = r.usage {
                engine.store.record_usage(&h.model.key, &u);
            }
            Some(r.content)
        }
        Err(e) => {
            tracing::debug!("utility call failed: {e}");
            None
        }
    }
}

pub fn spawn_title(engine: Engine, rt: Arc<ThreadRt>, first_message: String) {
    tokio::spawn(async move {
        let t = rt.thread();
        let msg: String = first_message.chars().take(2000).collect();
        let fallback = fallback_title(&msg);
        let title = utility(
            &engine,
            &t,
            "Write a short title (3 to 7 words) for a coding task. Reply with the title only, no quotes or punctuation at the end.",
            &msg,
            None,
            32,
        )
        .await
        .map(|s| clean_title(&s))
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback);
        if rt.thread().name.is_none() {
            engine.update_thread(&rt, |t| t.name = Some(title));
        }
    });
}

pub fn fallback_title(msg: &str) -> String {
    let words: Vec<&str> = msg.split_whitespace().take(7).collect();
    let mut s = words.join(" ");
    if s.chars().count() > 60 {
        s = s.chars().take(60).collect();
    }
    if s.is_empty() {
        "New thread".into()
    } else {
        s
    }
}

fn clean_title(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let line = line.trim_matches(|c| c == '"' || c == '\'' || c == '*' || c == '#').trim();
    let line = line.strip_prefix("Title:").unwrap_or(line).trim();
    line.trim_end_matches('.').chars().take(80).collect()
}

pub fn spawn_followups(engine: Engine, rt: Arc<ThreadRt>, final_text: String) {
    tokio::spawn(async move {
        let t = rt.thread();
        let task = rt.original_task.lock().unwrap().clone().unwrap_or_default();
        let schema = json!({"type": "object", "properties": {"suggestions": {"type": "array", "items": {"type": "string"}, "maxItems": 3}}, "required": ["suggestions"]});
        let user = format!(
            "Task: {}\n\nAgent's latest reply:\n{}\n\nSuggest up to 3 short next prompts (under 10 words each) the user is likely to send next. JSON only.",
            task.chars().take(1500).collect::<String>(),
            final_text.chars().take(3000).collect::<String>()
        );
        let Some(out) = utility(
            &engine,
            &t,
            "You suggest helpful follow-up prompts for a coding assistant conversation.",
            &user,
            Some(schema),
            200,
        )
        .await
        else {
            return;
        };
        let Ok((v, _)) = odex_llm::repair::parse_lenient(&out) else { return };
        let suggestions: Vec<String> = v
            .get("suggestions")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty() && s.len() < 120)
                    .take(3)
                    .collect()
            })
            .unwrap_or_default();
        if suggestions.is_empty() || rt.is_running() {
            return;
        }
        *rt.followups.lock().unwrap() = suggestions.clone();
        engine.emitter().followups(&rt.id, suggestions);
    });
}

/// After a completed turn with an active goal: update progress and decide
/// whether to continue automatically. Returns true if a new turn started.
pub async fn goal_after_turn(engine: &Engine, rt: &Arc<ThreadRt>, f: &Finished) -> bool {
    let t = rt.thread();
    let Some(goal) = t.goal.clone().filter(|g| g.status == "active") else { return false };
    let text = f.final_text.as_str();
    let upper = text.to_uppercase();
    let mut g = goal.clone();
    g.turns += 1;
    g.tokens_used += f.turn.usage.total_tokens;
    g.last_update = Some(text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(200).collect());
    let elapsed = ((now_ms() - g.started_at) / 1000).max(0) as u32;
    if upper.contains("GOAL: DONE") {
        g.status = "done".into();
    } else if upper.contains("GOAL: BLOCKED") {
        g.status = "blocked".into();
    } else if g.time_budget_secs.map(|b| elapsed >= b).unwrap_or(false)
        || g.token_budget.map(|b| g.tokens_used >= b).unwrap_or(false)
    {
        g.status = "budgetExhausted".into();
    }
    let active = g.status == "active";
    engine.update_thread(rt, |t| t.goal = Some(g.clone()));
    if !active {
        return false;
    }
    if !rt.queue.lock().unwrap().is_empty() {
        return false; // the user's queued message goes first; goal resumes after
    }
    let input = vec![UserInput::text(format!(
        "[goal] Continue working toward the goal: {}. Check progress, then take the next concrete step.",
        g.objective
    ))];
    spawn_turn(engine, rt.clone(), input, TurnOpts { synthetic: true, ..Default::default() });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(clean_title("\"Fix login bug.\"\n"), "Fix login bug");
        assert_eq!(clean_title("Title: Add tests"), "Add tests");
        assert_eq!(
            fallback_title("please refactor the parser module so that it is faster and cleaner"),
            "please refactor the parser module so that"
        );
    }
}
