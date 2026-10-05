//! End-to-end engine tests against the mock vLLM server.

mod common;

use common::*;
use odex_mock_vllm::{MockCall, MockReply, RecordedRequest};
use odex_protocol::*;
use serde_json::json;

#[tokio::test]
async fn text_turn_streams_and_persists() {
    let h = harness(32768, "").await;
    h.server.push(MockReply::text("Hello! I can help with that."));
    let tid = h.thread(PermissionMode::Auto).await;
    let turn = h.run(&tid, "hi there").await;
    assert_eq!(turn.status, TurnStatus::Completed);
    assert!(h.sink.count(notification::ITEM_DELTA) > 1, "streamed deltas");
    let items = h.sink.items();
    assert!(items.iter().any(|i| matches!(i, ThreadItem::UserMessage { .. })));
    assert!(items
        .iter()
        .any(|i| matches!(i, ThreadItem::AgentMessage { text, .. } if text.contains("help with that"))));
    // persisted: reload the thread from its rollout in a fresh engine
    let read = odex_core::api::thread_read(&h.engine, ThreadIdParams { thread_id: tid.clone() }).await.unwrap();
    assert_eq!(read.turns.len(), 1);
    assert!(read.turns[0].items.len() >= 2);
    let e2 = odex_core::Engine::new(odex_core::EngineOptions {
        home: odex_config::OdexHome::at(h.home.path()),
        profile: None,
    })
    .unwrap();
    let read2 = odex_core::api::thread_read(&e2, ThreadIdParams { thread_id: tid }).await.unwrap();
    assert_eq!(read2.turns.len(), 1);
    assert_eq!(read2.turns[0].items.len(), read.turns[0].items.len());
    let usage = h.engine.store.usage(None).unwrap();
    assert!(usage.totals.input_tokens > 0);
}

#[tokio::test]
async fn tool_loop_writes_file_and_reads_it_back() {
    let h = harness(32768, "").await;
    h.server.push_all([
        MockReply::tool("write_file", json!({"path": "hello.txt", "content": "hi from odex\n"})),
        MockReply::tool("read_file", json!({"path": "hello.txt"})),
        MockReply::text("Created hello.txt."),
    ]);
    let tid = h.thread(PermissionMode::Auto).await;
    let turn = h.run(&tid, "create hello.txt").await;
    assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
    assert_eq!(std::fs::read_to_string(h.work.path().join("hello.txt")).unwrap(), "hi from odex\n");
    let items = h.sink.items();
    assert!(items.iter().any(|i| matches!(i, ThreadItem::FileChange { status: ItemStatus::Completed, .. })));
    // the read result reached the model
    let reqs = h.server.requests();
    let last = reqs.last().unwrap();
    assert!(last.all_text().contains("hi from odex"));
    // tool results carry the call ids
    let msgs = last.messages();
    assert!(msgs.iter().any(|m| m["role"] == "tool" && m["tool_call_id"].is_string()));
}

#[tokio::test]
async fn shell_runs_in_sandbox_and_streams_output() {
    let h = harness(32768, "").await;
    let cmd = if cfg!(windows) { "Write-Output 'sandboxed hello'" } else { "echo sandboxed hello" };
    h.server.push_all([MockReply::tool("shell", json!({"command": cmd})), MockReply::text("done")]);
    let tid = h.thread(PermissionMode::Auto).await;
    let turn = h.run(&tid, "say hello").await;
    assert_eq!(turn.status, TurnStatus::Completed);
    let item = h.sink.items().into_iter().find_map(|i| match i {
        ThreadItem::CommandExecution { output, exit_code, sandboxed, .. } => Some((output, exit_code, sandboxed)),
        _ => None,
    });
    let (output, code, sandboxed) = item.expect("command item");
    assert!(output.contains("sandboxed hello"), "{output}");
    assert_eq!(code, Some(0));
    assert!(sandboxed);
    assert!(h.sink.approvals.lock().unwrap().is_empty(), "no approval needed for a sandboxed command");
}

#[tokio::test]
async fn escalation_asks_and_deny_reaches_model() {
    let h = harness(32768, "").await;
    *h.sink.decide.lock().unwrap() = Box::new(|_| ApprovalDecision::Deny { feedback: Some("not now".into()) });
    h.server.push_all([
        MockReply::tool("shell", json!({"command": "echo hi", "escalated": true, "justification": "needs network"})),
        MockReply::text("ok, skipped"),
    ]);
    let tid = h.thread(PermissionMode::Auto).await;
    let turn = h.run(&tid, "do it").await;
    assert_eq!(turn.status, TurnStatus::Completed);
    let approvals = h.sink.approvals.lock().unwrap();
    assert_eq!(approvals.len(), 1);
    assert!(matches!(&approvals[0].approval, ApprovalKind::Exec { reason, .. } if reason == "escalation"));
    let last = h.server.last_request().unwrap();
    assert!(last.all_text().contains("denied") && last.all_text().contains("not now"));
}

#[tokio::test]
async fn read_only_mode_asks_before_writes() {
    let h = harness(32768, "").await;
    *h.sink.decide.lock().unwrap() = Box::new(|_| ApprovalDecision::Approve);
    h.server
        .push_all([MockReply::tool("write_file", json!({"path": "a.txt", "content": "x"})), MockReply::text("done")]);
    let tid = h.thread(PermissionMode::ReadOnly).await;
    h.run(&tid, "write a").await;
    assert_eq!(h.sink.approvals.lock().unwrap().len(), 1);
    assert!(h.work.path().join("a.txt").exists());
}

#[tokio::test]
async fn forbidden_command_is_refused() {
    let h = harness(32768, "").await;
    let cmd = if cfg!(windows) { "Remove-Item -Recurse -Force C:\\" } else { "rm -rf /" };
    h.server.push_all([MockReply::tool("shell", json!({"command": cmd})), MockReply::text("ok")]);
    let tid = h.thread(PermissionMode::FullAccess).await;
    h.run(&tid, "clean up").await;
    let last = h.server.last_request().unwrap();
    assert!(last.all_text().contains("forbidden"), "{}", last.all_text());
}

#[tokio::test]
async fn invalid_args_get_precise_error_then_retry() {
    let h = harness(32768, "").await;
    h.server.push_all([
        MockReply::ToolCalls {
            calls: vec![MockCall {
                name: "read_file".into(),
                arguments: json!({}),
                raw_arguments: Some("{\"limit\": 5}".into()),
            }],
            text: None,
        },
        MockReply::text("sorry"),
    ]);
    let tid = h.thread(PermissionMode::Auto).await;
    h.run(&tid, "read").await;
    let last = h.server.last_request().unwrap();
    assert!(last.all_text().contains("`path` is required"));
}

#[tokio::test]
async fn fallback_parsed_tool_calls_execute() {
    let h = harness(32768, "").await;
    h.server.push_all([
        MockReply::ContentToolCall {
            content: "<tool_call>\n{\"name\": \"list_dir\", \"arguments\": {\"path\": \".\"}}\n</tool_call>".into(),
        },
        MockReply::text("listed"),
    ]);
    std::fs::write(h.work.path().join("marker.txt"), "x").unwrap();
    let tid = h.thread(PermissionMode::Auto).await;
    let turn = h.run(&tid, "list").await;
    assert_eq!(turn.status, TurnStatus::Completed);
    assert!(h.server.last_request().unwrap().all_text().contains("marker.txt"));
}

#[tokio::test]
async fn overflow_400_recovers_transparently() {
    // The server counts ~2x the tokens we estimate and the very first prompt is
    // too large: the first request gets an overflow 400, the engine calibrates,
    // runs the emergency path and retries without user involvement.
    let h = harness_cpt(6000, 1.2, "").await;
    h.server.set_policy(|_| MockReply::text("summary of the log"));
    let tid = h.thread(PermissionMode::Auto).await;
    let big = format!(
        "Here is a build log, summarize it:
{}",
        "error[E0432]: unresolved import in module foo::bar
"
        .repeat(120)
    );
    let turn = h.run(&tid, &big).await;
    assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
    let over = h.server.requests().iter().filter(|r| r.prompt_tokens + r.max_tokens().unwrap_or(0) > 6000).count();
    assert!(over >= 1, "expected the first estimate to be wrong");
    let ok_req = h.server.requests().iter().filter(|r| r.prompt_tokens + r.max_tokens().unwrap_or(0) <= 6000).count();
    assert!(ok_req >= 1);
    assert!(h
        .sink
        .items()
        .iter()
        .any(|i| matches!(i, ThreadItem::AgentMessage { text, .. } if text.contains("summary of the log"))));
    assert!(h
        .sink
        .items()
        .iter()
        .any(|i| matches!(i, ThreadItem::Notice { code: Some(c), .. } if c == "contextOverflow")));
}

#[tokio::test]
async fn steer_and_queue() {
    let h = harness(32768, "").await;
    h.server.set_policy(|r| {
        if r.last_text().contains("second") {
            MockReply::text("handled second")
        } else if r.tool_results_since_user() < 2 {
            MockReply::tool("list_dir", json!({}))
        } else {
            MockReply::text("first done")
        }
    });
    let tid = h.thread(PermissionMode::Auto).await;
    let r = odex_core::turn::start_turn(
        &h.engine,
        TurnStartParams { thread_id: tid.clone(), input: vec![UserInput::text("first")], ..Default::default() },
    )
    .await
    .unwrap();
    let q = odex_core::turn::start_turn(
        &h.engine,
        TurnStartParams { thread_id: tid.clone(), input: vec![UserInput::text("second")], ..Default::default() },
    )
    .await
    .unwrap();
    assert!(q.queued || q.turn.is_some());
    h.wait(&tid, &r.turn.unwrap().id, std::time::Duration::from_secs(30)).await;
    // the queued message runs next
    for _ in 0..200 {
        if h.sink.items().iter().any(|i| matches!(i, ThreadItem::AgentMessage { text, .. } if text == "handled second"))
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("queued message never ran");
}

#[tokio::test]
async fn interrupt_stops_turn() {
    let h = harness(32768, "").await;
    h.server.push(MockReply::Stall { ms: 10_000 });
    let tid = h.thread(PermissionMode::Auto).await;
    let r = odex_core::turn::start_turn(
        &h.engine,
        TurnStartParams { thread_id: tid.clone(), input: vec![UserInput::text("x")], ..Default::default() },
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let rt = h.engine.thread(&tid).unwrap();
    odex_core::turn::interrupt(&rt);
    let t = h.wait(&tid, &r.turn.unwrap().id, std::time::Duration::from_secs(10)).await;
    assert_eq!(t.status, TurnStatus::Interrupted);
}

#[tokio::test]
async fn plan_mode_is_read_only_and_proposes_plan() {
    let h = harness(32768, "").await;
    h.server.push_all([
        MockReply::tool("write_file", json!({"path": "nope.txt", "content": "x"})),
        MockReply::text("## Plan\n1. Do the thing\n2. Test it"),
    ]);
    let tid = h.thread(PermissionMode::FullAccess).await;
    let r = odex_core::turn::start_turn(
        &h.engine,
        TurnStartParams {
            thread_id: tid.clone(),
            input: vec![UserInput::text("plan it")],
            mode: Some(TurnMode::Plan),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    h.wait(&tid, &r.turn.unwrap().id, std::time::Duration::from_secs(30)).await;
    assert!(!h.work.path().join("nope.txt").exists());
    // write tools are not even offered in plan mode
    let first = &h.server.requests()[0];
    assert!(!first.tool_names().contains(&"write_file".to_string()));
    assert!(h
        .sink
        .items()
        .iter()
        .any(|i| matches!(i, ThreadItem::ProposedPlan { markdown, .. } if markdown.contains("## Plan"))));
}

#[tokio::test]
async fn fork_and_rollback() {
    let h = harness(32768, "").await;
    h.server.push_all([MockReply::text("one"), MockReply::text("two")]);
    let tid = h.thread(PermissionMode::Auto).await;
    let t1 = h.run(&tid, "first").await;
    h.run(&tid, "second").await;
    let fork = odex_core::api::thread_fork(
        &h.engine,
        ThreadForkParams { thread_id: tid.clone(), turn_id: Some(t1.id.clone()), ..Default::default() },
    )
    .await
    .unwrap();
    let fr =
        odex_core::api::thread_read(&h.engine, ThreadIdParams { thread_id: fork.thread.id.clone() }).await.unwrap();
    assert_eq!(fr.turns.len(), 1);
    let rb = odex_core::api::thread_rollback(
        &h.engine,
        ThreadRollbackParams { thread_id: tid.clone(), turn_id: t1.id.clone(), restore_files: false },
    )
    .await
    .unwrap();
    assert_eq!(rb.turns.len(), 0);
}

#[tokio::test]
async fn recall_finds_earlier_history() {
    let h = harness(32768, "").await;
    h.server.push_all([
        MockReply::text("Noted: the deploy key id is ZEBRA-4412."),
        MockReply::tool("recall", json!({"query": "deploy key"})),
        MockReply::text("found it"),
    ]);
    let tid = h.thread(PermissionMode::Auto).await;
    h.run(&tid, "remember the deploy key id ZEBRA-4412").await;
    h.run(&tid, "what was the key?").await;
    let last = h.server.last_request().unwrap();
    let tool_msgs: Vec<String> = last
        .messages()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(odex_mock_vllm::RecordedRequest::message_text)
        .collect();
    assert!(tool_msgs.iter().any(|t| t.contains("ZEBRA-4412")), "{tool_msgs:?}");
}

#[tokio::test]
async fn project_rules_from_dot_odex_apply() {
    let h = harness(32768, "").await;
    let rules = h.work.path().join(".odex").join("rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("project.toml"),
        "[[rule]]\nprefix = [\"echo\", \"forbidden-marker\"]\ndecision = \"forbid\"\njustification = \"project says no\"\n",
    )
    .unwrap();
    h.server.push_all([
        MockReply::ToolCalls {
            calls: vec![MockCall {
                name: "shell".into(),
                arguments: json!({"command": "echo forbidden-marker"}),
                raw_arguments: None,
            }],
            text: None,
        },
        MockReply::text("understood"),
    ]);
    let tid = h.thread(PermissionMode::FullAccess).await;
    let turn = h.run(&tid, "run it").await;
    assert_eq!(turn.status, TurnStatus::Completed);
    let reqs = h.server.requests();
    let tool_msg = reqs
        .last()
        .unwrap()
        .messages()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(RecordedRequest::message_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(tool_msg.contains("forbidden by policy"), "tool result: {tool_msg}");
    assert!(tool_msg.contains("project says no"), "tool result: {tool_msg}");
}
