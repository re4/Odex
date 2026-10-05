//! Structured handoff summaries for compaction.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::history::{EntryKind, HistoryEntry};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decision {
    pub decision: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlanItem {
    pub step: String,
    /// pending | in_progress | completed
    pub status: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileState {
    pub path: String,
    pub purpose: String,
    pub state: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandResult {
    pub command: String,
    pub result: String,
}

/// The handoff summary sections (PROMPT §10.4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SummaryData {
    /// The user's goal and every explicit requirement, quoted verbatim.
    pub goal_and_requirements: Vec<String>,
    pub decisions: Vec<Decision>,
    pub plan: Vec<PlanItem>,
    pub files_changed: Vec<FileState>,
    pub codebase_facts: Vec<String>,
    pub commands_and_tests: Vec<CommandResult>,
    pub open_errors: Vec<String>,
    pub next_steps: Vec<String>,
    pub important_refs: Vec<String>,
}

impl SummaryData {
    pub fn json_schema() -> Value {
        let strings = json!({"type": "array", "items": {"type": "string"}});
        json!({
            "type": "object",
            "properties": {
                "goal_and_requirements": {"type": "array", "items": {"type": "string"},
                    "description": "The user's goal and EVERY explicit requirement, copied verbatim in quotes."},
                "decisions": {"type": "array", "items": {"type": "object", "properties": {
                    "decision": {"type": "string"}, "reason": {"type": "string"}}, "required": ["decision", "reason"]}},
                "plan": {"type": "array", "items": {"type": "object", "properties": {
                    "step": {"type": "string"}, "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}},
                    "required": ["step", "status"]}},
                "files_changed": {"type": "array", "items": {"type": "object", "properties": {
                    "path": {"type": "string"}, "purpose": {"type": "string"}, "state": {"type": "string"}},
                    "required": ["path", "purpose", "state"]}},
                "codebase_facts": strings.clone(),
                "commands_and_tests": {"type": "array", "items": {"type": "object", "properties": {
                    "command": {"type": "string"}, "result": {"type": "string"}}, "required": ["command", "result"]}},
                "open_errors": strings.clone(),
                "next_steps": strings.clone(),
                "important_refs": strings
            },
            "required": ["goal_and_requirements", "decisions", "plan", "files_changed", "codebase_facts",
                         "commands_and_tests", "open_errors", "next_steps", "important_refs"]
        })
    }

    /// Parse model output leniently (repairing JSON, tolerating missing sections).
    pub fn parse(text: &str) -> Option<Self> {
        let (v, _) = odex_llm::repair::parse_lenient(text).ok()?;
        let obj = v.as_object()?;
        if obj.is_empty() {
            return None;
        }
        // Parse field by field so one malformed section doesn't lose the rest.
        let strings = |k: &str| -> Vec<String> {
            obj.get(k)
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|i| match i {
                            Value::String(s) => Some(s.clone()),
                            Value::Null => None,
                            other => Some(other.to_string()),
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        fn typed<T: serde::de::DeserializeOwned>(obj: &serde_json::Map<String, Value>, k: &str) -> Option<Vec<T>> {
            let arr = obj.get(k)?.as_array()?;
            Some(arr.iter().filter_map(|i| serde_json::from_value::<T>(i.clone()).ok()).collect())
        }
        let mut s = SummaryData {
            goal_and_requirements: strings("goal_and_requirements"),
            codebase_facts: strings("codebase_facts"),
            open_errors: strings("open_errors"),
            next_steps: strings("next_steps"),
            important_refs: strings("important_refs"),
            ..Default::default()
        };
        s.decisions = typed::<Decision>(obj, "decisions").unwrap_or_default();
        s.decisions.retain(|d| !d.decision.is_empty());
        if s.decisions.is_empty() {
            s.decisions = strings("decisions")
                .into_iter()
                .filter(|d| !d.starts_with('{'))
                .map(|d| Decision { decision: d, reason: String::new() })
                .collect();
        }
        s.plan = typed::<PlanItem>(obj, "plan").unwrap_or_default();
        s.plan.retain(|p| !p.step.is_empty());
        if s.plan.is_empty() {
            s.plan = strings("plan")
                .into_iter()
                .filter(|p| !p.starts_with('{'))
                .map(|p| PlanItem { step: p, status: "pending".into() })
                .collect();
        }
        s.files_changed = typed::<FileState>(obj, "files_changed").unwrap_or_default();
        s.files_changed.retain(|f| !f.path.is_empty());
        if s.files_changed.is_empty() {
            s.files_changed = strings("files_changed")
                .into_iter()
                .filter(|p| !p.starts_with('{'))
                .map(|p| FileState { path: p, ..Default::default() })
                .collect();
        }
        s.commands_and_tests = typed::<CommandResult>(obj, "commands_and_tests").unwrap_or_default();
        s.commands_and_tests.retain(|c| !c.command.is_empty());
        if s.commands_and_tests.is_empty() {
            s.commands_and_tests = strings("commands_and_tests")
                .into_iter()
                .filter(|c| !c.starts_with('{'))
                .map(|c| CommandResult { command: c, result: String::new() })
                .collect();
        }
        if s.is_empty() {
            return None;
        }
        Some(s)
    }

    pub fn is_empty(&self) -> bool {
        self.goal_and_requirements.is_empty()
            && self.decisions.is_empty()
            && self.plan.is_empty()
            && self.files_changed.is_empty()
            && self.codebase_facts.is_empty()
            && self.commands_and_tests.is_empty()
            && self.open_errors.is_empty()
            && self.next_steps.is_empty()
    }

    /// Ensure every verbatim requirement from `must_keep` is present (the
    /// compactor sometimes paraphrases or drops them).
    pub fn ensure_requirements(&mut self, must_keep: &[String]) {
        for r in must_keep {
            let norm = normalize(r);
            if !self.goal_and_requirements.iter().any(|g| normalize(g).contains(&norm)) {
                self.goal_and_requirements.push(r.clone());
            }
        }
    }

    pub fn render(&self, number: u32) -> String {
        let mut s = format!(
            "[Context summary #{number}] Earlier parts of this conversation were compacted to save context. \
             This summary replaces them. Use `recall(query)` to look up exact details and `read_output(ref)` \
             for full tool outputs when something here is not specific enough.\n"
        );
        let section = |s: &mut String, title: &str, items: Vec<String>| {
            if items.is_empty() {
                return;
            }
            s.push_str(&format!("\n## {title}\n"));
            for i in items {
                s.push_str(&format!("- {i}\n"));
            }
        };
        section(&mut s, "Goal and requirements (verbatim)", self.goal_and_requirements.clone());
        section(
            &mut s,
            "Decisions",
            self.decisions
                .iter()
                .map(|d| {
                    if d.reason.is_empty() {
                        d.decision.clone()
                    } else {
                        format!("{} — because {}", d.decision, d.reason)
                    }
                })
                .collect(),
        );
        section(
            &mut s,
            "Plan",
            self.plan
                .iter()
                .map(|p| {
                    let mark = match p.status.as_str() {
                        "completed" => "[x]",
                        "in_progress" => "[~]",
                        _ => "[ ]",
                    };
                    format!("{mark} {}", p.step)
                })
                .collect(),
        );
        section(
            &mut s,
            "Files changed",
            self.files_changed
                .iter()
                .map(|f| {
                    let mut l = format!("`{}`", f.path);
                    if !f.purpose.is_empty() {
                        l.push_str(&format!(": {}", f.purpose));
                    }
                    if !f.state.is_empty() {
                        l.push_str(&format!(" ({})", f.state));
                    }
                    l
                })
                .collect(),
        );
        section(&mut s, "Codebase facts", self.codebase_facts.clone());
        section(
            &mut s,
            "Commands and tests (latest results)",
            self.commands_and_tests
                .iter()
                .map(|c| {
                    if c.result.is_empty() {
                        format!("`{}`", c.command)
                    } else {
                        format!("`{}` → {}", c.command, c.result)
                    }
                })
                .collect(),
        );
        section(&mut s, "Open errors", self.open_errors.clone());
        section(&mut s, "Next steps", self.next_steps.clone());
        section(&mut s, "Important refs", self.important_refs.clone());
        s
    }
}

fn normalize(s: &str) -> String {
    s.trim()
        .trim_matches('"')
        .trim_matches('“')
        .trim_matches('”')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub const COMPACTOR_SYSTEM: &str = "You compress a coding agent's working history into a precise handoff summary. \
The agent will continue the task using ONLY your summary plus the most recent messages, so anything you omit is lost. \
Rules:\n\
- goal_and_requirements: copy the user's goal and EVERY explicit requirement or constraint VERBATIM (exact wording, in quotes). Never paraphrase them. Include requirements from later user messages too.\n\
- decisions: what was decided and why.\n\
- plan: current plan steps with status (pending, in_progress, completed).\n\
- files_changed: each file created/modified/deleted, its purpose, and current state.\n\
- codebase_facts: concrete facts learned (paths, APIs, conventions, commands that work).\n\
- commands_and_tests: commands/tests run and their LATEST results (pass/fail counts, key errors).\n\
- open_errors: unresolved errors with the exact message.\n\
- next_steps: the immediate next actions.\n\
- important_refs: output refs (ref:out_N), URLs, ids, line numbers worth keeping.\n\
Be specific and terse. Prefer exact names, numbers and paths over prose. Output JSON only.";

/// Build the compactor's user message.
pub fn compactor_user_prompt(
    previous: Option<&str>,
    transcript: &str,
    focus: Option<&str>,
    must_keep: &[String],
) -> String {
    let mut s = String::new();
    if let Some(p) = previous {
        s.push_str("## Previous summary (merge it into the new one; keep everything still relevant)\n");
        s.push_str(p);
        s.push_str("\n\n");
    }
    if !must_keep.is_empty() {
        s.push_str("## User requirements that MUST appear verbatim in goal_and_requirements\n");
        for r in must_keep {
            s.push_str(&format!("- \"{r}\"\n"));
        }
        s.push('\n');
    }
    if let Some(f) = focus.filter(|f| !f.trim().is_empty()) {
        s.push_str(&format!("## Focus\nThe user asked the summary to focus on: {f}\n\n"));
    }
    s.push_str("## History to compress\n");
    s.push_str(transcript);
    s.push_str("\n\nWrite the JSON summary now.");
    s
}

/// Render entries as a compact transcript for the compactor.
pub fn render_transcript(entries: &[HistoryEntry], max_chars_per_item: usize) -> String {
    let mut out = String::new();
    for e in entries {
        let label = match e.kind {
            EntryKind::User => "USER",
            EntryKind::Steer => "USER (mid-turn)",
            EntryKind::Assistant => "ASSISTANT",
            EntryKind::ToolResult => "TOOL RESULT",
            EntryKind::Nudge => "NOTE",
            EntryKind::Image => "IMAGE",
        };
        let mut body = e.text();
        if e.kind == EntryKind::Assistant && !e.msg.tool_calls.is_empty() {
            for c in &e.msg.tool_calls {
                body.push_str(&format!("\n→ call {}({})", c.name, clip(&c.arguments, 400)));
            }
        }
        if let Some(t) = &e.tool {
            let mut head = format!("{} [{}]", t.tool, t.args_summary);
            if let Some(code) = t.exit_code {
                head.push_str(&format!(" exit={code}"));
            }
            if let Some(r) = &t.output_ref {
                head.push_str(&format!(" {r}"));
            }
            body = format!("{head}\n{body}");
        }
        // User text is never clipped: requirements must survive verbatim.
        let body = if e.is_user() { body } else { clip_middle(&body, max_chars_per_item) };
        out.push_str(&format!("### {label}\n{}\n\n", body.trim()));
    }
    out
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

pub fn clip_middle(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    let head = n * 2 / 3;
    let tail = n - head;
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().skip(count - tail).collect();
    format!("{h}\n[… {} chars omitted …]\n{t}", count - n)
}

/// Deterministic fallback when the compactor fails: user messages (verbatim),
/// plan, files touched, last errors. A thread never stops because of context.
pub fn extractive(entries: &[HistoryEntry], previous: Option<&SummaryData>, plan: &[PlanItem]) -> SummaryData {
    let mut s = previous.cloned().unwrap_or_default();
    // The first request is the task (kept nearly whole); later messages are
    // kept as short gists. The list is bounded so repeated fallbacks can't
    // grow the summary (verbatim requirements are re-added separately).
    const FIRST_CHARS: usize = 2000;
    const GIST_CHARS: usize = 300;
    const MAX_GOALS: usize = 12;
    const MAX_TOTAL_CHARS: usize = 6000;
    for e in entries.iter().filter(|e| e.is_user()) {
        let t = e.text();
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        let item = clip(t, if s.goal_and_requirements.is_empty() { FIRST_CHARS } else { GIST_CHARS });
        if !s.goal_and_requirements.iter().any(|g| g == &item || g == t) {
            s.goal_and_requirements.push(item);
        }
    }
    // keep the first (the task) and the most recent gists
    while s.goal_and_requirements.len() > MAX_GOALS
        || (s.goal_and_requirements.len() > 1
            && s.goal_and_requirements.iter().map(|g| g.len()).sum::<usize>() > MAX_TOTAL_CHARS)
    {
        s.goal_and_requirements.remove(1);
    }
    if !plan.is_empty() {
        s.plan = plan.to_vec();
    }
    let mut touched: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut commands: Vec<CommandResult> = Vec::new();
    for e in entries {
        if let Some(t) = &e.tool {
            for f in &t.file_writes {
                if !touched.contains(f) {
                    touched.push(f.clone());
                }
            }
            if t.tool == "shell" || t.tool == "exec_command" {
                let result = match t.exit_code {
                    Some(0) => "exit 0".to_string(),
                    Some(c) => format!("exit {c}"),
                    None => "no exit code".to_string(),
                };
                commands.retain(|c| c.command != t.args_summary);
                commands.push(CommandResult { command: t.args_summary.clone(), result });
            }
            if !t.success {
                let first = e
                    .text()
                    .lines()
                    .rev()
                    .find(|l| l.to_lowercase().contains("error"))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                errors.push(format!(
                    "{} [{}] failed{}",
                    t.tool,
                    t.args_summary,
                    if first.is_empty() { String::new() } else { format!(": {}", clip(&first, 200)) }
                ));
            }
            if let Some(r) = &t.output_ref {
                if !s.important_refs.contains(r) && s.important_refs.len() < 40 {
                    s.important_refs.push(format!("{r} ({} {})", t.tool, t.args_summary));
                }
            }
        }
    }
    for f in touched {
        if !s.files_changed.iter().any(|x| x.path == f) {
            s.files_changed.push(FileState { path: f, purpose: String::new(), state: "modified".into() });
        }
    }
    let keep_cmds = commands.len().saturating_sub(15);
    s.commands_and_tests.extend(commands.into_iter().skip(keep_cmds));
    let keep_errs = errors.len().saturating_sub(5);
    s.open_errors = errors.into_iter().skip(keep_errs).collect();
    if let Some(last) = entries.iter().rev().find(|e| e.kind == EntryKind::Assistant && !e.text().trim().is_empty()) {
        s.next_steps = vec![format!("Continue from the last assistant message: {}", clip(last.text().trim(), 500))];
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use odex_llm::types::ChatMessage;

    #[test]
    fn parse_and_render() {
        let txt = r#"{"goal_and_requirements": ["\"Use tabs\""], "decisions": ["use serde"], "plan": [{"step":"a","status":"completed"}],
                      "files_changed": [], "codebase_facts": ["x"], "commands_and_tests": [], "open_errors": [], "next_steps": ["b"], "important_refs": []}"#;
        let s = SummaryData::parse(txt).unwrap();
        assert_eq!(s.decisions[0].decision, "use serde");
        let r = s.render(2);
        assert!(r.contains("[Context summary #2]"));
        assert!(r.contains("[x] a"));
        assert!(r.contains("\"Use tabs\""));
    }

    #[test]
    fn ensure_requirements_adds_missing() {
        let mut s =
            SummaryData { goal_and_requirements: vec!["\"Use tabs, not spaces\"".into()], ..Default::default() };
        s.ensure_requirements(&["Use tabs, not spaces".into(), "Never touch the db folder".into()]);
        assert_eq!(s.goal_and_requirements.len(), 2);
    }

    #[test]
    fn extractive_keeps_user_text() {
        let es = vec![
            HistoryEntry::new("t", 0, EntryKind::User, ChatMessage::user("REQ: keep it fast")),
            HistoryEntry::new("t", 0, EntryKind::Assistant, ChatMessage::assistant("Working on it")),
        ];
        let s = extractive(&es, None, &[]);
        assert_eq!(s.goal_and_requirements, vec!["REQ: keep it fast"]);
        assert!(s.next_steps[0].contains("Working on it"));
    }

    #[test]
    fn extractive_stays_bounded_across_fallbacks() {
        let task = format!("TASK: {}", "build the thing ".repeat(50));
        let mut prev: Option<SummaryData> = None;
        for round in 0..30 {
            let mut es = Vec::new();
            if round == 0 {
                es.push(HistoryEntry::new("t", 0, EntryKind::User, ChatMessage::user(task.clone())));
            }
            for i in 0..3 {
                let msg = format!("round {round} follow-up {i}: {}", "more detail ".repeat(200));
                es.push(HistoryEntry::new("t", round, EntryKind::User, ChatMessage::user(msg)));
            }
            prev = Some(extractive(&es, prev.as_ref(), &[]));
        }
        let s = prev.unwrap();
        assert!(s.goal_and_requirements.len() <= 12);
        assert!(s.goal_and_requirements.iter().map(|g| g.len()).sum::<usize>() <= 6000 + 2000);
        assert!(s.goal_and_requirements[0].starts_with("TASK:"), "the task is kept first");
        assert!(s.goal_and_requirements.last().unwrap().starts_with("round 29"), "latest gist kept");
    }

    #[test]
    fn schema_is_object() {
        assert_eq!(SummaryData::json_schema()["type"], "object");
    }
}
