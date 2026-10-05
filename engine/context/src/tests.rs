use super::*;
use odex_llm::types::{ChatMessage, ToolCall};
use pretty_assertions::assert_eq;

fn settings() -> ContextSettings {
    ContextSettings::default()
}

fn sys() -> SystemParts {
    SystemParts {
        base: "You are Odex.".into(),
        extra: "cwd: /x".into(),
        agents_md: "# AGENTS\nuse tabs".into(),
        memories: String::new(),
    }
}

struct Fixture {
    system: SystemParts,
    tools: Vec<ToolSpec>,
    pinned: Pinned,
    est: TokenEstimator,
}

impl Fixture {
    fn new() -> Self {
        Self { system: sys(), tools: vec![], pinned: Pinned::default(), est: TokenEstimator::default() }
    }
    fn inputs(&self, turn: u32) -> PromptInputs<'_> {
        PromptInputs {
            system: &self.system,
            tools: &self.tools,
            pinned: &self.pinned,
            estimator: &self.est,
            reasoning_history: ReasoningHistory::CurrentTurn,
            current_turn: turn,
        }
    }
}

fn user(st: &mut ContextState, f: &Fixture, turn: u32, text: &str) {
    st.push(HistoryEntry::new(&format!("t{turn}"), turn, EntryKind::User, ChatMessage::user(text)), &f.est, 2);
}

fn tool_step(st: &mut ContextState, f: &Fixture, turn: u32, tool: &str, args: &str, output: &str, read: Option<&str>) {
    let id = format!("c{}", st.entries.len());
    let call = ToolCall { id: id.clone(), name: tool.into(), arguments: args.into() };
    let mut a = ChatMessage::assistant_tool_calls(Some("step".into()), vec![call]);
    a.reasoning = Some("thinking about it".repeat(5));
    st.push(HistoryEntry::new(&format!("t{turn}"), turn, EntryKind::Assistant, a), &f.est, 2);
    let meta = ToolMeta {
        tool: tool.into(),
        call_id: id.clone(),
        args_summary: args.into(),
        exit_code: Some(0),
        success: true,
        output_ref: Some(format!("ref:out_{}", st.entries.len())),
        full_chars: output.len(),
        lines: output.lines().count(),
        file_read: read.map(String::from),
        file_writes: vec![],
        fingerprint: hash(&(tool, args, output)),
        call_fingerprint: hash(&(tool, args)),
    };
    st.push(
        HistoryEntry::new(&format!("t{turn}"), turn, EntryKind::ToolResult, ChatMessage::tool_result(id, tool, output))
            .with_tool(meta),
        &f.est,
        2,
    );
}

fn hash<T: std::hash::Hash>(t: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

#[test]
fn budget_math() {
    let b = Budget::new(32768, 16384, &settings());
    assert_eq!(b.reserved_output, 8192); // 25% cap
    assert_eq!(b.margin, 984);
    assert_eq!(b.budget(), 32768 - 8192 - 984);
    assert_eq!(b.max_tokens_for(30000, 16384), 32768 - 30000 - 984);
    let small = Budget::new(4096, 2048, &settings());
    assert_eq!(small.reserved_output, 1024);
}

#[test]
fn messages_are_ordered_and_normalized() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    user(&mut st, &f, 0, "do the thing");
    tool_step(&mut st, &f, 0, "read_file", "path=a.rs", "fn a() {}", Some("a.rs"));
    let msgs = st.build_messages(&f.inputs(1));
    assert_eq!(msgs[0].role, odex_llm::types::Role::System);
    assert!(msgs[0].text_content().contains("# AGENTS"));
    assert_eq!(msgs.len(), 4);
    // reasoning of a previous turn is dropped under current_turn policy
    assert!(msgs[2].reasoning.is_none());
    let msgs = st.build_messages(&f.inputs(0));
    assert!(msgs[2].reasoning.is_some());
}

#[test]
fn prune_stubs_old_outputs_first_and_keeps_current_step() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    for turn in 0..6 {
        user(&mut st, &f, turn, &format!("task {turn}"));
        tool_step(&mut st, &f, turn, "shell", &format!("cmd {turn}"), &"output line\n".repeat(200), None);
    }
    let p = f.inputs(5);
    let before = st.estimate(&p);
    let rep = st.prune(&p, &settings(), before / 2, false);
    assert!(rep.after < rep.before);
    assert!(rep.stubbed >= 2);
    // the most recent tool output is intact
    let last = st.entries.last().unwrap();
    assert!(!last.stubbed);
    // stubs reference the full output
    let stub = st.entries.iter().find(|e| e.stubbed).unwrap();
    assert!(stub.text().contains("read_output(\"ref:out_"), "{}", stub.text());
}

#[test]
fn prune_superseded_reads_and_duplicates() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    user(&mut st, &f, 0, "go");
    tool_step(&mut st, &f, 0, "read_file", "path=a.rs", &"x\n".repeat(300), Some("a.rs"));
    tool_step(&mut st, &f, 0, "grep", "pattern=foo", &"hit\n".repeat(300), None);
    tool_step(&mut st, &f, 0, "grep", "pattern=foo", &"hit\n".repeat(300), None);
    tool_step(&mut st, &f, 0, "read_file", "path=a.rs", &"y\n".repeat(300), Some("a.rs"));
    tool_step(&mut st, &f, 0, "shell", "ls", "small", None);
    let p = f.inputs(0);
    st.prune(&p, &settings(), 10, false); // target unreachable → apply all steps
    let texts: Vec<String> = st.entries.iter().filter(|e| e.kind == EntryKind::ToolResult).map(|e| e.text()).collect();
    assert!(texts[0].contains("superseded"), "{}", texts[0]);
    assert!(texts[1].contains("duplicate"), "{}", texts[1]);
    assert!(!st.entries.iter().rfind(|e| e.kind == EntryKind::ToolResult).unwrap().stubbed);
}

#[test]
fn image_hygiene_keeps_newest_two() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    user(&mut st, &f, 0, "look");
    for i in 0..4 {
        let mut m = ChatMessage::user(format!("screenshot {i}"));
        m.content.push(ContentPart::ImageUrl { url: "data:image/png;base64,AAAA".into() });
        st.push(HistoryEntry::new("t0", 0, EntryKind::Image, m), &f.est, 2);
    }
    let imgs: usize = st.entries.iter().map(|e| e.image_count()).sum();
    assert_eq!(imgs, 2);
    assert!(st.entries[1].text().contains("image removed"));
    assert_eq!(st.entries[4].image_count(), 1);
}

#[test]
fn plan_compaction_keeps_whole_groups_and_turn_boundary() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    for turn in 0..5 {
        user(&mut st, &f, turn, &format!("task {turn}"));
        tool_step(&mut st, &f, turn, "shell", "x", &"o\n".repeat(100), None);
    }
    let split = st.plan_compaction(150).unwrap();
    assert_eq!(st.entries[split].kind, EntryKind::User);
    assert!(split > 0);
    // never splits a tool call from its result
    assert_ne!(st.entries[split].kind, EntryKind::ToolResult);
}

struct FakeSummarizer {
    fail: bool,
    calls: std::sync::Mutex<u32>,
}

#[async_trait::async_trait]
impl Summarizer for FakeSummarizer {
    fn input_budget(&self) -> u32 {
        3000
    }
    fn count(&self, text: &str) -> u32 {
        (text.len() / 4) as u32
    }
    async fn summarize(&self, _system: &str, user: &str, _schema: &Value) -> anyhow::Result<String> {
        *self.calls.lock().unwrap() += 1;
        if self.fail {
            anyhow::bail!("model down");
        }
        // paraphrases requirements (engine must re-add verbatim ones)
        let n = user.matches("### ").count();
        Ok(serde_json::json!({
            "goal_and_requirements": ["make it work"],
            "decisions": [{"decision": "use rust", "reason": "fast"}],
            "plan": [], "files_changed": [], "codebase_facts": [format!("saw {n} items")],
            "commands_and_tests": [], "open_errors": [], "next_steps": ["continue"], "important_refs": []
        })
        .to_string())
    }
}

#[tokio::test]
async fn compaction_summarizes_and_keeps_requirements_verbatim() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    user(&mut st, &f, 0, "REQUIREMENT: all output must be in French.");
    for turn in 0..8 {
        if turn > 0 {
            user(&mut st, &f, turn, &format!("continue step {turn}"));
        }
        tool_step(&mut st, &f, turn, "shell", &format!("make {turn}"), &"build log\n".repeat(150), None);
    }
    let p = f.inputs(7);
    let sm = FakeSummarizer { fail: false, calls: Default::default() };
    let before = st.estimate(&p);
    let out = compact(&mut st, Some(&sm), &p, &settings(), 4096, CompactionTrigger::Auto, None, None).await.unwrap();
    assert!(out.llm);
    assert!(out.record.tokens_after < before);
    let s = st.summary.as_ref().unwrap();
    assert_eq!(s.number, 1);
    assert!(s.data.goal_and_requirements.iter().any(|r| r.contains("all output must be in French")));
    let msgs = st.build_messages(&p);
    assert!(msgs[1].text_content().contains("[Context summary #1]"));
    // second compaction increments and still carries the requirement
    for turn in 8..14 {
        user(&mut st, &f, turn, &format!("more {turn}"));
        tool_step(&mut st, &f, turn, "shell", &format!("make {turn}"), &"build log\n".repeat(150), None);
    }
    let p = f.inputs(13);
    compact(&mut st, Some(&sm), &p, &settings(), 4096, CompactionTrigger::Auto, None, None).await.unwrap();
    let s = st.summary.as_ref().unwrap();
    assert_eq!(s.number, 2);
    assert!(s.data.goal_and_requirements.iter().any(|r| r.contains("all output must be in French")));
    assert_eq!(st.compactions.len(), 2);
}

#[tokio::test]
async fn compaction_falls_back_to_extractive() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    for turn in 0..6 {
        user(&mut st, &f, turn, &format!("please do item {turn}"));
        tool_step(&mut st, &f, turn, "shell", "x", &"o\n".repeat(200), None);
    }
    let p = f.inputs(5);
    let sm = FakeSummarizer { fail: true, calls: Default::default() };
    let out = compact(&mut st, Some(&sm), &p, &settings(), 4096, CompactionTrigger::Auto, None, None).await.unwrap();
    assert!(!out.llm);
    let s = st.summary.as_ref().unwrap();
    assert!(s.data.goal_and_requirements.iter().any(|r| r == "please do item 0"));
}

#[tokio::test]
async fn map_reduce_for_large_history() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    user(&mut st, &f, 0, "big task");
    for i in 0..40 {
        tool_step(&mut st, &f, 0, "read_file", &format!("path=f{i}.rs"), &"code line\n".repeat(120), None);
    }
    user(&mut st, &f, 1, "next");
    let p = f.inputs(1);
    let sm = FakeSummarizer { fail: false, calls: Default::default() };
    compact(&mut st, Some(&sm), &p, &settings(), 8192, CompactionTrigger::Auto, None, None).await.unwrap();
    assert!(*sm.calls.lock().unwrap() > 1, "expected multiple map-reduce calls");
}

#[test]
fn emergency_trim_never_drops_newest_user_message() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    for turn in 0..4 {
        user(&mut st, &f, turn, &format!("turn {turn} request"));
        tool_step(&mut st, &f, turn, "shell", "x", &"o\n".repeat(300), None);
    }
    let p = f.inputs(3);
    st.emergency_trim(&p, 200);
    assert!(st.entries.iter().any(|e| e.kind == EntryKind::User && e.text() == "turn 3 request"));
    assert!(st.entries[0].text().contains("trimmed"));
}

#[test]
fn correction_calibrates_estimate() {
    let f = Fixture::new();
    let mut st = ContextState::new();
    user(&mut st, &f, 0, &"hello world ".repeat(100));
    let p = f.inputs(0);
    let raw = st.raw_estimate(&p);
    st.observe_usage(raw * 2, raw);
    assert_eq!(st.estimate(&p), raw * 2); // exact until something changes
    user(&mut st, &f, 0, "more");
    let e = st.estimate(&p);
    assert!(e > raw * 14 / 10, "{e} vs raw {raw}");
}

#[test]
fn pinned_render_rules() {
    let p = Pinned { original_task: Some("task".into()), goal: Some("ship it".into()), ..Default::default() };
    let pre = p.render(false).unwrap();
    assert!(pre.contains("ship it") && !pre.contains("Original task"));
    let post = p.render(true).unwrap();
    assert!(post.contains("Original task"));
    assert!(Pinned::default().render(true).is_none());
}
