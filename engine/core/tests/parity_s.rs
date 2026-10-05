//! Parity tests: persisted "Always allow" exec rules, the built-in
//! skill-creator skill and the thread summary card (`thread/read`, `thread/context`).

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use common::*;
use odex_core::EventSink;
use odex_mock_vllm::MockReply;
use odex_protocol::*;

/// Approves every request with "approve for session" + `persist: true`
/// (the desktop's "Always allow `<prefix>`" button).
struct RememberSink {
    inner: Arc<TestSink>,
}

#[async_trait]
impl EventSink for RememberSink {
    fn notify(&self, method: &str, params: Value) {
        self.inner.notify(method, params);
    }
    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        if method == server_request::APPROVAL_REQUEST {
            let p: ApprovalRequestParams = serde_json::from_value(params)?;
            self.inner.approvals.lock().unwrap().push(p);
            return Ok(json!({ "decision": { "type": "approveForSession" }, "persist": true }));
        }
        anyhow::bail!("unsupported {method}")
    }
    fn capabilities(&self) -> ClientCapabilities {
        ClientCapabilities { approvals: true, ..Default::default() }
    }
}

fn echo(text: &str) -> String {
    if cfg!(windows) {
        format!("Write-Output {text}")
    } else {
        format!("echo {text}")
    }
}

#[tokio::test]
async fn always_allow_persists_a_rule_that_the_next_command_honors() {
    let h = harness(32768, "").await;
    h.engine.set_sink(Arc::new(RememberSink { inner: h.sink.clone() }));
    h.server.push_all([
        MockReply::tool("shell", json!({"command": echo("first-run"), "escalated": true, "justification": "test"})),
        MockReply::text("ran it"),
    ]);
    let t1 = h.thread(PermissionMode::Auto).await;
    let turn = h.run(&t1, "run it").await;
    assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
    let prefix = {
        let approvals = h.sink.approvals.lock().unwrap();
        assert_eq!(approvals.len(), 1, "the first escalated command asks");
        match &approvals[0].approval {
            ApprovalKind::Exec { prefix, .. } => prefix.clone(),
            other => panic!("unexpected approval {other:?}"),
        }
    };
    assert!(!prefix.is_empty());

    // the rule is on disk
    let rules = std::fs::read_to_string(h.home.path().join("rules").join("default.toml")).unwrap();
    assert!(rules.contains("[[rule]]") && rules.contains("decision = \"allow\""), "{rules}");
    assert!(rules.contains(&prefix[0]), "{rules}");

    // a new thread (no session approvals) runs the same prefix without asking
    h.server.push_all([
        MockReply::tool("shell", json!({"command": echo("second-run"), "escalated": true, "justification": "test"})),
        MockReply::text("ran it again"),
    ]);
    let t2 = h.thread(PermissionMode::Auto).await;
    let turn = h.run(&t2, "run it again").await;
    assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
    assert_eq!(h.sink.approvals.lock().unwrap().len(), 1, "the persisted rule skips the approval");
    let ran = h.sink.items().into_iter().any(|i| {
        matches!(i, ThreadItem::CommandExecution { output, exit_code: Some(0), .. } if output.contains("second-run"))
    });
    assert!(ran, "the second command ran");

    // a fresh engine loads the rule from disk
    let e2 = odex_core::Engine::new(odex_core::EngineOptions {
        home: odex_config::OdexHome::at(h.home.path()),
        profile: None,
    })
    .unwrap();
    let argv: Vec<String> = prefix.iter().cloned().chain(["later".to_string()]).collect();
    assert!(odex_core::toolexec::rules_allow_all(&e2, &[argv]));
    // remembering the same prefix again does not duplicate the rule
    odex_core::toolexec::remember_exec_rule(&h.engine, &prefix).unwrap();
    let again = std::fs::read_to_string(h.home.path().join("rules").join("default.toml")).unwrap();
    assert_eq!(again.matches("[[rule]]").count(), 1, "{again}");
}

#[tokio::test]
async fn skill_creator_is_a_builtin_skill() {
    let h = harness(32768, "").await;
    let list = odex_core::api::skills_list(&h.engine, SkillsListParams { cwd: None });
    let sk = list.skills.iter().find(|s| s.name == "skill-creator").expect("skill-creator listed");
    assert_eq!(sk.scope, SkillScope::Builtin);
    assert!(sk.enabled);
    assert!(!sk.description.is_empty());
    let text = std::fs::read_to_string(&sk.path).expect("the SKILL.md exists on disk");
    assert!(text.starts_with("---\nname: skill-creator\n"), "{text}");
    let read = odex_core::api::skills_read(&h.engine, NameParams { name: "$skill-creator".into() }).unwrap();
    assert!(read.body.contains("Validation checklist"));
    assert!(odex_core::api::skills_delete(&h.engine, NameParams { name: "skill-creator".into() }).is_err());
    // it is offered to the agent in the system prompt
    let tid = h.thread(PermissionMode::Auto).await;
    let rt = h.engine.thread(&tid).unwrap();
    let t = rt.thread();
    let parts = h.engine.system_parts(&t, &h.engine.thread_settings(&t), TurnMode::Default);
    assert!(parts.extra.contains("- skill-creator:"), "{}", parts.extra);
    // a user skill of the same name overrides it
    odex_core::skills::write_skill(&h.engine.home.skills_dir(), "skill-creator", "mine", "custom").unwrap();
    let list = odex_core::api::skills_list(&h.engine, SkillsListParams { cwd: None });
    let mine: Vec<_> = list.skills.iter().filter(|s| s.name == "skill-creator").collect();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].scope, SkillScope::User);
}

#[tokio::test]
async fn thread_read_and_context_return_the_latest_summary() {
    let h = harness(32768, "").await;
    h.server.push_all([MockReply::text("First answer."), MockReply::text("Second answer.")]);
    let tid = h.thread(PermissionMode::Auto).await;
    h.run(&tid, "first question").await;
    h.run(&tid, "second question").await;
    let read = odex_core::api::thread_read(&h.engine, ThreadIdParams { thread_id: tid.clone() }).await.unwrap();
    assert!(read.summary.is_none(), "no summary before a compaction");

    h.server.push(MockReply::Json {
        value: json!({
            "goal_and_requirements": ["\"Ship the parser\""],
            "decisions": [{"decision": "Use serde", "reason": "already a dependency"}],
            "plan": [],
            "files_changed": [{"path": "src/parse.rs", "purpose": "parser", "state": "done"}],
            "codebase_facts": [],
            "commands_and_tests": [],
            "open_errors": ["one flaky test"],
            "next_steps": ["Add docs"],
            "important_refs": []
        }),
    });
    odex_core::api::thread_compact(&h.engine, ThreadCompactParams { thread_id: tid.clone(), focus: None })
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let summary = loop {
        let c = odex_core::api::thread_context(&h.engine, ThreadIdParams { thread_id: tid.clone() }).await.unwrap();
        if let Some(s) = c.summary {
            break s;
        }
        assert!(std::time::Instant::now() < deadline, "no summary after /compact");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(summary.number, 1);
    assert!(summary.llm);
    assert!(summary.at.is_some());
    // (the engine also carries the user's verbatim requests into the requirements)
    assert_eq!(summary.goal_and_requirements.first().map(String::as_str), Some("\"Ship the parser\""));
    assert_eq!(summary.decisions[0].decision, "Use serde");
    assert_eq!(summary.files_changed[0].path, "src/parse.rs");
    assert_eq!(summary.next_steps, vec!["Add docs".to_string()]);
    assert_eq!(summary.open_errors, vec!["one flaky test".to_string()]);
    let read = odex_core::api::thread_read(&h.engine, ThreadIdParams { thread_id: tid }).await.unwrap();
    assert_eq!(read.summary.as_ref(), Some(&summary));
}
