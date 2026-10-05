//! Context soak test (PROMPT §12): a 4,096-token mock model runs a 200-step
//! task that forces many compactions. Asserts that the task completes,
//! verbatim requirements survive, no request overflows, and `recall` finds a
//! detail from before compaction.

mod common;

use common::*;
use odex_mock_vllm::{MockReply, RecordedRequest};
use odex_protocol::*;
use once_cell_regex::*;
use serde_json::json;

mod once_cell_regex {
    pub use regex::Regex;
    pub fn max_step(text: &str) -> u32 {
        let re = Regex::new(r"(?:Processing step|completed through step) (\d+)").unwrap();
        re.captures_iter(text).filter_map(|c| c[1].parse::<u32>().ok()).max().unwrap_or(0)
    }
}

const STEPS: u32 = 200;
const REQ1: &str = "REQUIREMENT: Always use snake_case file names.";
const REQ2: &str = "REQUIREMENT: Never touch the vendor/ directory.";

fn compactor(r: &RecordedRequest) -> MockReply {
    let text = r.all_text();
    let done = max_step(&text);
    let mut reqs: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(i) = line.find("REQUIREMENT:") {
            let req = line[i..].trim_end_matches(['"', '\'', '.', ' ']).to_string() + ".";
            if !reqs.contains(&req) {
                reqs.push(req);
            }
        }
    }
    // A deliberately lossy summary: it never mentions the secret code.
    MockReply::Json {
        value: json!({
            "goal_and_requirements": reqs,
            "decisions": [{"decision": "process data chunks in order", "reason": "the task says so"}],
            "plan": [{"step": format!("Process {STEPS} steps"), "status": "in_progress"}],
            "files_changed": [],
            "codebase_facts": [format!("completed through step {done}")],
            "commands_and_tests": [],
            "open_errors": [],
            "next_steps": [format!("Continue with step {}", done + 1)],
            "important_refs": []
        }),
    }
}

fn agent(r: &RecordedRequest) -> MockReply {
    let text = r.all_text();
    let done = max_step(&text);
    if done >= STEPS {
        return MockReply::text(format!("All {STEPS} steps complete."));
    }
    let next = done + 1;
    if next == 150 {
        return MockReply::ToolCalls {
            calls: vec![odex_mock_vllm::MockCall {
                name: "recall".into(),
                arguments: json!({"query": "secret code"}),
                raw_arguments: None,
            }],
            text: Some(format!("Processing step {next}. Looking up an early detail.")),
        };
    }
    let path = if next == 3 { "data/secret.txt".to_string() } else { format!("data/chunk_{}.txt", next % 10) };
    MockReply::ToolCalls {
        calls: vec![odex_mock_vllm::MockCall {
            name: "read_file".into(),
            arguments: json!({"path": path}),
            raw_arguments: None,
        }],
        text: Some(format!("Processing step {next}.")),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn soak_200_steps_on_4k_window() {
    let h = harness(4096, "").await;
    let data = h.work.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    for i in 0..10 {
        let body: String = (0..40)
            .map(|l| format!("chunk {i} line {l}: lorem ipsum dolor sit amet, consectetur adipiscing elit\n"))
            .collect();
        std::fs::write(data.join(format!("chunk_{i}.txt")), body).unwrap();
    }
    std::fs::write(data.join("secret.txt"), "Configuration notes.\nThe secret code is 7319.\nEnd of notes.\n").unwrap();
    h.server.set_policy(|r| {
        if r.structured_name().as_deref() == Some("context_summary") {
            compactor(r)
        } else {
            agent(r)
        }
    });

    let tid = h.thread(PermissionMode::Auto).await;
    let task = format!(
        "Process the data files for {STEPS} steps: at each step read the next chunk file and note progress.\n{REQ1}\n{REQ2}"
    );
    let r = odex_core::turn::start_turn(
        &h.engine,
        TurnStartParams { thread_id: tid.clone(), input: vec![UserInput::text(task)], ..Default::default() },
    )
    .await
    .unwrap();
    let turn = h.wait(&tid, &r.turn.unwrap().id, std::time::Duration::from_secs(600)).await;

    // 1. the task completes
    assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
    let items = h.sink.items();
    assert!(items
        .iter()
        .any(|i| matches!(i, ThreadItem::AgentMessage { text, .. } if text.contains("All 200 steps complete"))));

    // 2. at least 10 compactions
    let compactions = items
        .iter()
        .filter(|i| matches!(i, ThreadItem::ContextCompaction { status: ItemStatus::Completed, .. }))
        .count();
    assert!(compactions >= 10, "only {compactions} compactions");

    let reqs = h.server.requests();
    // 3. no request overflowed the window
    let overflowed: Vec<(u32, Option<u32>)> = reqs
        .iter()
        .filter(|r| r.prompt_tokens + r.max_tokens().unwrap_or(0) > 4096)
        .map(|r| (r.prompt_tokens, r.max_tokens()))
        .collect();
    assert!(overflowed.is_empty(), "requests overflowed: {overflowed:?}");

    // 4. verbatim requirements survive to the end
    let last_agent = reqs.iter().rev().find(|r| r.structured_name().is_none()).unwrap();
    let all = last_agent.all_text();
    assert!(all.contains(REQ1), "requirement 1 lost");
    assert!(all.contains(REQ2), "requirement 2 lost");
    // and the early secret really was compacted away before recall
    let before_recall =
        reqs.iter().filter(|r| r.structured_name().is_none()).find(|r| max_step(&r.all_text()) == 149).unwrap();
    assert!(!before_recall.all_text().contains("7319"), "secret should have been compacted away");

    // 5. recall finds the pre-compaction detail
    let found = reqs
        .iter()
        .any(|r| r.messages().iter().any(|m| m["role"] == "tool" && RecordedRequest::message_text(m).contains("7319")));
    assert!(found, "recall did not surface the secret code");

    // the thread's context status reflects the compactions
    let ctx = odex_core::api::thread_context(&h.engine, ThreadIdParams { thread_id: tid }).await.unwrap().context;
    assert!(ctx.compactions.len() >= 10);
    assert!(ctx.used <= ctx.window);
    eprintln!("soak: {} requests, {compactions} compactions, final context {} / {}", reqs.len(), ctx.used, ctx.window);
}
