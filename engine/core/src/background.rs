//! Background jobs: automation scheduler and memory proposals.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use odex_llm::types::{ChatMessage, ChatRequest, StructuredOutput};
use odex_protocol::*;

use crate::engine::Engine;
use crate::thread::ThreadRt;
use crate::turn::{spawn_turn, TurnOpts};

/// The automation store lives in the main SQLite index.
pub fn automations(engine: &Engine) -> anyhow::Result<odex_automations::AutomationStore> {
    Ok(odex_automations::AutomationStore::new(engine.store.conn())?)
}

/// Scheduler loop: every 20s, start due automations.
pub async fn scheduler(engine: Engine, shutdown: CancellationToken) {
    if let Ok(store) = automations(&engine) {
        let _ = store.fail_stale_running("The app stopped while this run was in progress.");
    }
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(20)) => {}
            _ = shutdown.cancelled() => return,
        }
        let Ok(store) = automations(&engine) else { continue };
        let now = chrono::Local::now();
        for a in store.due(now) {
            let _ = store.mark_started(&a.id, now);
            let e2 = engine.clone();
            tokio::spawn(async move {
                run_automation(&e2, a).await;
            });
        }
    }
}

fn emit_run(engine: &Engine, run: &AutomationRun) {
    engine.emitter().raw(notification::AUTOMATION_RUN_UPDATED, &AutomationRunNotification { run: run.clone() });
}

/// Run one automation now and record the result in the review queue.
pub async fn run_automation(engine: &Engine, a: Automation) {
    let Ok(store) = automations(engine) else { return };
    // target thread (heartbeat) or a fresh thread per run
    let rt: Result<Arc<ThreadRt>, String> = if a.target == "thread" {
        match a.thread_id.as_deref().map(|id| engine.thread(id)) {
            Some(Ok(rt)) => Ok(rt),
            Some(Err(e)) => Err(e.message),
            None => Err("no target thread".into()),
        }
    } else {
        let params = ThreadStartParams {
            project_id: a.project_id.clone(),
            cwd: a.cwd.clone(),
            kind: Some(ThreadKind::Automation),
            name: Some(a.name.clone()),
            model: a.model.clone(),
            effort: a.effort,
            permission_mode: Some(a.permission_mode),
            run_mode: Some(a.run_mode),
            ..Default::default()
        };
        crate::api::start_thread_inner(engine, params).await.map_err(|e| e.message)
    };
    let rt = match rt {
        Ok(rt) => rt,
        Err(e) => {
            if let Ok(run) = store.create_run(&a, None) {
                if let Ok(run) = store.finish_run(&run.id, "failed", None, Some(e)) {
                    emit_run(engine, &run);
                }
            }
            return;
        }
    };
    let Ok(run) = store.create_run(&a, Some(rt.id.clone())) else { return };
    emit_run(engine, &run);
    if rt.is_running() {
        if let Ok(run) =
            store.finish_run(&run.id, "skipped", None, Some("The thread was busy; this run was skipped.".into()))
        {
            emit_run(engine, &run);
        }
        return;
    }
    // Thread automations keep the thread's own permission mode; new threads use the automation's.
    let (tx, mut rx) = tokio::sync::watch::channel::<Option<Turn>>(None);
    let turn = spawn_turn(
        engine,
        rt.clone(),
        vec![UserInput::text(a.prompt.clone())],
        TurnOpts { synthetic: a.target == "thread", ..Default::default() },
    );
    let rt2 = rt.clone();
    let tid = turn.id.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let done = rt2
                .turns
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.id == tid)
                .filter(|t| t.status != TurnStatus::InProgress)
                .cloned();
            if let Some(t) = done {
                let _ = tx.send(Some(t));
                return;
            }
        }
    });
    let finished = loop {
        if rx.changed().await.is_err() {
            break None;
        }
        if let Some(t) = rx.borrow().clone() {
            break Some(t);
        }
    };
    let final_text = rt.turns.lock().unwrap().iter().find(|t| t.id == turn.id).and_then(|t| {
        t.items.iter().rev().find_map(|i| match i {
            ThreadItem::AgentMessage { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
    });
    let (status, error) = match &finished {
        Some(t) if t.status == TurnStatus::Completed => ("completed", None),
        Some(t) => ("failed", t.error.as_ref().map(|e| e.message.clone()).or(Some(format!("{:?}", t.status)))),
        None => ("failed", Some("run did not finish".into())),
    };
    let summary = final_text.map(|s| s.chars().take(1200).collect());
    if let Ok(run) = store.finish_run(&run.id, status, summary, error) {
        emit_run(engine, &run);
    }
}

/// Propose memories from a thread with the utility model.
pub async fn propose_memories(engine: &Engine, rt: &ThreadRt) -> anyhow::Result<Vec<Memory>> {
    let t = rt.thread();
    let h = engine.role_model(ModelRole::Utility, Some(&t)).ok_or_else(|| anyhow::anyhow!("no utility model"))?;
    let mut digest = String::new();
    for turn in rt.turns.lock().unwrap().iter() {
        for item in &turn.items {
            match item {
                ThreadItem::UserMessage { content, .. } => {
                    digest.push_str(&format!("USER: {}\n", crate::turn::inputs_text(content)))
                }
                ThreadItem::AgentMessage { text, .. } => {
                    digest.push_str(&format!("ASSISTANT: {}\n", text.chars().take(1500).collect::<String>()))
                }
                _ => {}
            }
        }
    }
    let digest: String = digest.chars().rev().take(16_000).collect::<Vec<_>>().into_iter().rev().collect();
    let store = odex_memories::MemoryStore::new(engine.home.memories_dir());
    let root = engine.thread_root(&t);
    let existing = store.list(Some(&root), None);
    let req = odex_memories::proposal_request(&digest, &existing);
    let chat = ChatRequest {
        messages: vec![ChatMessage::system(req.system), ChatMessage::user(req.user)],
        max_tokens: Some(800),
        structured: Some(StructuredOutput { name: "memories".into(), schema: req.json_schema }),
        effort: Some(ReasoningEffort::None),
        ..Default::default()
    };
    let resp = h.client.chat(&h.model, &chat, &CancellationToken::new()).await?;
    let (v, _) = odex_llm::repair::parse_lenient(&resp.content).map_err(anyhow::Error::msg)?;
    let proposals = odex_memories::parse_proposals(&v, Some(&root), Some(&rt.id));
    let mut saved = Vec::new();
    for m in proposals {
        if let Ok(m) = store.upsert(m) {
            saved.push(m);
        }
    }
    if !saved.is_empty() {
        engine.emitter().raw(notification::MEMORY_PROPOSED, &MemoriesProposedNotification { memories: saved.clone() });
    }
    Ok(saved)
}

/// After a thread goes idle for a while, propose memories (opt-in).
pub fn schedule_memory_proposal(engine: Engine, rt: Arc<ThreadRt>) {
    let t = rt.thread();
    let s = engine.thread_settings(&t);
    if !(s.memories_enabled && s.memories_generate && t.memories_enabled) || t.kind == ThreadKind::Subagent {
        return;
    }
    let at = rt.turn_counter.load(std::sync::atomic::Ordering::SeqCst);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(90)).await;
        if rt.is_running() || rt.turn_counter.load(std::sync::atomic::Ordering::SeqCst) != at {
            return;
        }
        if let Err(e) = propose_memories(&engine, &rt).await {
            tracing::debug!("memory proposal skipped: {e:#}");
        }
    });
}
