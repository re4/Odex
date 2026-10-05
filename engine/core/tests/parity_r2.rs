//! Parity tests: goal pause / resume / edit and model downgrade warnings.

mod common;

use std::time::Duration;

use common::*;
use odex_core::api;
use odex_mock_vllm::MockReply;
use odex_protocol::*;

async fn idle(h: &Harness, thread_id: &str) {
    let rt = h.engine.thread(thread_id).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while rt.is_running() {
        assert!(std::time::Instant::now() < deadline, "turn did not stop");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn running(h: &Harness, thread_id: &str) {
    let rt = h.engine.thread(thread_id).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !rt.is_running() {
        assert!(std::time::Instant::now() < deadline, "turn did not start");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn goal(h: &Harness, thread_id: &str) -> Goal {
    h.engine.thread(thread_id).unwrap().thread().goal.unwrap()
}

/// The goal once its status is `status` (goal bookkeeping runs just after the turn ends).
async fn goal_settles(h: &Harness, thread_id: &str, status: &str) -> Goal {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let g = goal(h, thread_id);
        if g.status == status && !h.engine.thread(thread_id).unwrap().is_running() {
            return g;
        }
        assert!(std::time::Instant::now() < deadline, "goal stayed {}", g.status);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn goal_pause_edit_resume() {
    let h = harness(32768, "").await;
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::Stall { ms: 20_000 });
    api::goal_set(
        &h.engine,
        GoalSetParams {
            thread_id: id.clone(),
            objective: "Ship the parser".into(),
            time_budget_secs: Some(1800),
            token_budget: Some(200_000),
            edit: None,
        },
    )
    .await
    .unwrap();
    running(&h, &id).await;
    assert_eq!(goal(&h, &id).status, "active");

    // pause stops the turn and keeps the goal
    let t = api::goal_pause(&h.engine, ThreadIdParams { thread_id: id.clone() }).await.unwrap().thread;
    let g = t.goal.unwrap();
    assert_eq!(g.status, "paused");
    assert!(g.paused_at.is_some());
    idle(&h, &id).await;
    assert_eq!(goal(&h, &id).status, "paused", "an interrupted turn must not continue a paused goal");
    assert!(api::goal_pause(&h.engine, ThreadIdParams { thread_id: id.clone() }).await.is_err());

    // edit in place: progress and status kept, nothing starts while paused
    let started = goal(&h, &id).started_at;
    api::goal_set(
        &h.engine,
        GoalSetParams {
            thread_id: id.clone(),
            objective: "Ship the parser v2".into(),
            time_budget_secs: Some(3600),
            token_budget: None,
            edit: Some(true),
        },
    )
    .await
    .unwrap();
    let g = goal(&h, &id);
    assert_eq!((g.objective.as_str(), g.status.as_str()), ("Ship the parser v2", "paused"));
    assert_eq!((g.time_budget_secs, g.token_budget, g.started_at), (Some(3600), None, started));
    assert!(!h.engine.thread(&id).unwrap().is_running());

    // resume continues pursuing it (paused time does not count)
    tokio::time::sleep(Duration::from_millis(30)).await;
    h.server.push(MockReply::text("Shipped.\n\nGOAL: DONE"));
    api::goal_resume(&h.engine, ThreadIdParams { thread_id: id.clone() }).await.unwrap();
    let g = goal_settles(&h, &id, "done").await;
    assert!(g.paused_at.is_none());
    assert!(g.started_at > started, "paused time is excluded from the time budget");
    let last = h.server.last_request().unwrap();
    assert!(last.body.to_string().contains("Resume working toward the goal: Ship the parser v2"));

    // editing a finished goal reopens it and continues
    h.server.push(MockReply::text("Still done.\n\nGOAL: DONE"));
    api::goal_set(
        &h.engine,
        GoalSetParams {
            thread_id: id.clone(),
            objective: "Ship the parser v2".into(),
            time_budget_secs: None,
            token_budget: Some(500_000),
            edit: Some(true),
        },
    )
    .await
    .unwrap();
    assert_eq!(goal(&h, &id).status, "active");
    let g = goal_settles(&h, &id, "done").await;
    assert_eq!(g.token_budget, Some(500_000));
    assert!(api::goal_resume(&h.engine, ThreadIdParams { thread_id: id.clone() }).await.is_err());
}

#[tokio::test]
async fn model_warnings_window_shrink_and_unserved_model() {
    let h = harness(32768, "").await;
    let id = h.thread(PermissionMode::Auto).await;
    h.server.push(MockReply::text("Hi."));
    h.run(&id, "hello").await;
    let t = h.engine.thread(&id).unwrap().thread();
    let used = t.last_model.expect("the turn records its model");
    assert_eq!((used.model_id.as_str(), used.context_window), ("mock-coder", 32768));
    assert!(t.model_warning.is_none());

    // the endpoint restarts with a smaller max_model_len
    h.server.set_max_model_len(16384);
    h.engine.registry.refresh().await;
    odex_core::model_watch::check_all(&h.engine);
    let w = h.engine.thread(&id).unwrap().thread().model_warning.expect("window shrink is detected");
    assert_eq!(w.code, "windowShrank");
    assert_eq!((w.previous_window, w.window), (Some(32768), Some(16384)));

    // the next turn notes it in the transcript and moves on
    h.server.push(MockReply::text("Ok."));
    h.run(&id, "go on").await;
    assert!(h.sink.items().iter().any(
        |i| matches!(i, ThreadItem::Notice { code: Some(c), message, .. } if c == "modelWarning" && message.contains("16K"))
    ));
    let t = h.engine.thread(&id).unwrap().thread();
    assert!(t.model_warning.is_none());
    assert_eq!(t.last_model.unwrap().context_window, 16384);

    // a model the endpoint does not serve: the same-family alternative is named
    let t = api::thread_update(
        &h.engine,
        ThreadUpdateParams { thread_id: id.clone(), model: Some("mock:mock-coder-xl".into()), ..Default::default() },
    )
    .await
    .unwrap()
    .thread;
    let w = t.model_warning.expect("unserved model is detected");
    assert_eq!(w.code, "notServed");
    assert_eq!(w.served_model.as_deref(), Some("mock-coder"));
    assert!(w.message.contains("same family"));

    // switching back clears it
    let t = api::thread_update(
        &h.engine,
        ThreadUpdateParams { thread_id: id.clone(), model: Some("mock:mock-coder".into()), ..Default::default() },
    )
    .await
    .unwrap()
    .thread;
    assert!(t.model_warning.is_none());
}
