//! Endurance (PROMPT §12 "Done means"): three worktree threads on a 32k model
//! run long tasks in parallel. Each thread must finish without a context error,
//! compact many times, never overflow the window, stay isolated from the other
//! threads (no cross-thread context), and write only into its own worktree.
//!
//! Wall-clock hours aren't simulated; the step count gives each thread far more
//! context churn than an hour of real work on a 32k window.

mod common;

use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

use common::*;
use odex_mock_vllm::{MockCall, MockReply, RecordedRequest};
use odex_protocol::*;
use regex::Regex;
use serde_json::json;

const WINDOW: u32 = 32_768;
const STEPS: u32 = 600;
const TAGS: [&str; 3] = ["ALPHA", "BRAVO", "CHARLIE"];

fn tag_of(text: &str) -> Option<&'static str> {
    TAGS.iter().copied().find(|t| text.contains(&format!("TASK-{t}")))
}

fn done_for(text: &str, tag: &str) -> u32 {
    let re = Regex::new(&format!(r"(?:{tag} step|{tag} completed through step) (\d+)")).unwrap();
    re.captures_iter(text).filter_map(|c| c[1].parse::<u32>().ok()).max().unwrap_or(0)
}

fn reply(r: &RecordedRequest) -> MockReply {
    let text = r.all_text();
    let Some(tag) = tag_of(&text) else { return MockReply::text("no task") };
    let done = done_for(&text, tag);
    if r.structured_name().as_deref() == Some("context_summary") {
        let reqs: Vec<String> = text
            .lines()
            .filter_map(|l| l.find("REQUIREMENT:").map(|i| l[i..].trim_end_matches(['"', '\'', ' ']).to_string()))
            .fold(Vec::new(), |mut v, x| {
                if !v.contains(&x) {
                    v.push(x)
                }
                v
            });
        let goals: Vec<String> = std::iter::once(format!("TASK-{tag}: process {STEPS} steps")).chain(reqs).collect();
        return MockReply::Json {
            value: json!({
                "goal_and_requirements": goals,
                "decisions": [],
                "plan": [{"step": format!("{STEPS} steps"), "status": "in_progress"}],
                "files_changed": [],
                "codebase_facts": [format!("{tag} completed through step {done}")],
                "commands_and_tests": [],
                "open_errors": [],
                "next_steps": [format!("{tag} step {}", done + 1)],
                "important_refs": []
            }),
        };
    }
    if done >= STEPS {
        return MockReply::text(format!("{tag}: all {STEPS} steps complete."));
    }
    let next = done + 1;
    let call = if next.is_multiple_of(20) {
        MockCall {
            name: "write_file".into(),
            arguments: json!({"path": format!("out/{}_{next}.txt", tag.to_lowercase()), "content": format!("{tag} {next}\n")}),
            raw_arguments: None,
        }
    } else {
        MockCall {
            name: "read_file".into(),
            arguments: json!({"path": format!("data/chunk_{}.txt", next % 8)}),
            raw_arguments: None,
        }
    };
    MockReply::ToolCalls { calls: vec![call], text: Some(format!("{tag} step {next}.")) }
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.email=t@odex.test", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git")
        .success();
    assert!(ok, "git {args:?} failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn three_parallel_worktree_threads_on_32k() {
    let h = harness(WINDOW, "").await;
    let repo = h.work.path();
    let data = repo.join("data");
    std::fs::create_dir_all(&data).unwrap();
    for i in 0..8 {
        // ~1.5k tokens per read
        let body: String =
            (0..110).map(|l| format!("chunk {i} line {l}: the quick brown fox jumps over the lazy dog\n")).collect();
        std::fs::write(data.join(format!("chunk_{i}.txt")), body).unwrap();
    }
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "init"]);
    h.server.set_policy(reply);

    let mut threads = Vec::new();
    for tag in TAGS {
        let t = odex_core::api::thread_start(
            &h.engine,
            ThreadStartParams {
                cwd: Some(repo.to_string_lossy().to_string()),
                permission_mode: Some(PermissionMode::Auto),
                run_mode: Some(RunMode::Worktree),
                ..Default::default()
            },
        )
        .await
        .expect("worktree thread");
        let wt = t.thread.worktree.clone().expect("thread has a worktree");
        threads.push((tag, t.thread.id.clone(), wt));
    }

    let started = std::time::Instant::now();
    let mut turns = Vec::new();
    for (tag, tid, _) in &threads {
        let task = format!(
            "TASK-{tag}: process {STEPS} steps; at each step read the next chunk and note progress.\nREQUIREMENT: {tag} outputs go under out/."
        );
        let r = odex_core::turn::start_turn(
            &h.engine,
            TurnStartParams { thread_id: tid.clone(), input: vec![UserInput::text(task)], ..Default::default() },
        )
        .await
        .unwrap();
        turns.push((tid.clone(), r.turn.unwrap().id));
    }
    let waits = turns.iter().map(|(tid, turn)| h.wait(tid, turn, Duration::from_secs(1200)));
    let finished = futures::future::join_all(waits).await;
    let elapsed = started.elapsed();

    // every thread completed its task
    for (t, (tag, _, _)) in finished.iter().zip(&threads) {
        assert_eq!(t.status, TurnStatus::Completed, "{tag}: {:?}", t.error);
    }
    let items = h.sink.items();
    for tag in TAGS {
        let msg = format!("{tag}: all {STEPS} steps complete.");
        assert!(
            items.iter().any(|i| matches!(i, ThreadItem::AgentMessage { text, .. } if text == &msg)),
            "{tag} did not finish"
        );
    }
    // no context errors surfaced
    assert!(
        !items.iter().any(|i| matches!(i, ThreadItem::Error { .. })),
        "errors: {:?}",
        items.iter().filter(|i| matches!(i, ThreadItem::Error { .. })).collect::<Vec<_>>()
    );

    let reqs = h.server.requests();
    // never overflowed the window
    let over: Vec<_> = reqs
        .iter()
        .filter(|r| r.prompt_tokens + r.max_tokens().unwrap_or(0) > WINDOW)
        .map(|r| r.prompt_tokens)
        .collect();
    assert!(over.is_empty(), "overflowing requests: {over:?}");

    // isolation: no request mixes two threads' tasks
    for r in &reqs {
        let text = r.all_text();
        let present: Vec<_> = TAGS.iter().filter(|t| text.contains(&format!("TASK-{t}"))).collect();
        assert!(present.len() <= 1, "request mixes threads: {present:?}");
    }

    // each thread compacted many times
    let mut per_thread: HashMap<String, usize> = HashMap::new();
    for (_, tid, _) in &threads {
        let ctx =
            odex_core::api::thread_context(&h.engine, ThreadIdParams { thread_id: tid.clone() }).await.unwrap().context;
        assert!(ctx.used <= ctx.window);
        per_thread.insert(tid.clone(), ctx.compactions.len());
    }
    for (tag, tid, _) in &threads {
        assert!(per_thread[tid] >= 12, "{tag} only compacted {} times", per_thread[tid]);
    }

    // outputs landed in each thread's own worktree, never in the main checkout
    for (tag, _, wt) in &threads {
        let out = std::path::Path::new(&wt.path).join("out");
        let mine: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(mine.len(), (STEPS / 20) as usize, "{tag} outputs: {mine:?}");
        assert!(mine.iter().all(|f| f.starts_with(&tag.to_lowercase())), "{tag} worktree has foreign files: {mine:?}");
        assert_ne!(std::path::Path::new(&wt.path), repo);
    }
    assert!(!repo.join("out").exists(), "main checkout was modified");

    eprintln!(
        "parallel soak: {} requests in {:.1}s, compactions per thread {:?}",
        reqs.len(),
        elapsed.as_secs_f64(),
        threads.iter().map(|(tag, tid, _)| (tag, per_thread[tid])).collect::<Vec<_>>()
    );
}
