//! Subagents: child threads that run a self-contained task in parallel and
//! report back only their final message.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::watch;

use odex_llm::types::ToolCall;
use odex_protocol::*;

use crate::engine::Engine;
use crate::thread::{nickname, ThreadRt};
use crate::toolexec::{ToolOutcome, TurnCtx};

#[derive(Debug, Clone)]
pub struct Report {
    pub text: String,
    pub status: TurnStatus,
}

struct Link {
    parent: String,
    parent_turn: String,
    item_id: String,
    task: String,
    mode: String,
    nickname: String,
    tx: watch::Sender<Option<Report>>,
}

#[derive(Default)]
pub struct SubagentHub {
    links: Mutex<HashMap<String, Link>>,
}

impl SubagentHub {
    /// Called when any thread's turn finishes.
    pub fn finished(&self, child_id: &str, text: &str, status: TurnStatus) {
        let links = self.links.lock().unwrap();
        if let Some(l) = links.get(child_id) {
            let _ = l.tx.send(Some(Report { text: text.to_string(), status }));
        }
    }

    fn receiver(&self, child_id: &str) -> Option<watch::Receiver<Option<Report>>> {
        self.links.lock().unwrap().get(child_id).map(|l| l.tx.subscribe())
    }
}

fn subagent_item(
    link: &Link,
    child: &str,
    status: ItemStatus,
    summary: Option<String>,
    diff: Option<DiffStats>,
) -> ThreadItem {
    ThreadItem::Subagent {
        id: link.item_id.clone(),
        agent_thread_id: child.to_string(),
        task: link.task.clone(),
        mode: link.mode.clone(),
        status,
        summary,
        diff_stats: diff,
        identicon_seed: child.to_string(),
        nickname: link.nickname.clone(),
    }
}

pub async fn spawn_tool(engine: &Engine, rt: &ThreadRt, tctx: &TurnCtx, call: &ToolCall, args: &Value) -> ToolOutcome {
    let parent = rt.thread();
    let task = args["task"].as_str().unwrap_or("").trim().to_string();
    if task.is_empty() {
        return ToolOutcome { text: "Error: task is required".into(), ..Default::default() };
    }
    let mode = args.get("mode").and_then(|m| m.as_str()).unwrap_or("read_only").to_string();
    let worktree = args.get("worktree").and_then(|w| w.as_bool()).unwrap_or(false) && mode == "write";
    let permission = if mode == "read_only" { PermissionMode::ReadOnly } else { parent.permission_mode };
    let params = ThreadStartParams {
        project_id: parent.project_id.clone(),
        cwd: Some(parent.cwd.clone()),
        kind: Some(ThreadKind::Subagent),
        model: parent.model.clone(),
        effort: parent.effort,
        permission_mode: Some(permission),
        parent_thread_id: Some(parent.id.clone()),
        run_mode: Some(if worktree { RunMode::Worktree } else { RunMode::Local }),
        ..Default::default()
    };
    let child = match crate::api::start_thread_inner(engine, params).await {
        Ok(c) => c,
        Err(e) => {
            return ToolOutcome {
                text: format!("Error: could not start subagent: {}", e.message),
                ..Default::default()
            }
        }
    };
    let nick = nickname(&child.id);
    engine.update_thread(&child, |t| t.name = Some(format!("{nick}: {}", task.chars().take(60).collect::<String>())));
    let (tx, _rx) = watch::channel(None);
    let link = Link {
        parent: parent.id.clone(),
        parent_turn: tctx.turn_id.clone(),
        item_id: format!("item_{}", call.id),
        task: task.clone(),
        mode: mode.clone(),
        nickname: nick.clone(),
        tx,
    };
    let item = subagent_item(&link, &child.id, ItemStatus::InProgress, None, None);
    engine.emitter().item_started(&rt.id, &tctx.turn_id, &item);
    rt.upsert_item(&tctx.turn_id, &item);
    engine.subagents.links.lock().unwrap().insert(child.id.clone(), link);
    rt.children.lock().unwrap().push(child.id.clone());
    let instructions = format!(
        "You are a subagent working for another agent. Complete this task on your own, then reply with a concise final report \
         (findings, files changed, verification). Mode: {mode}{}.\n\nTask:\n{task}",
        if mode == "read_only" { " (read-only: investigate, do not modify files)" } else { "" }
    );
    crate::turn::spawn_turn(engine, child.clone(), vec![UserInput::text(instructions)], Default::default());
    // finish the parent's card when the child completes
    let e2 = engine.clone();
    let child_id = child.id.clone();
    let mut rx = engine.subagents.receiver(&child_id).unwrap();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let rep = rx.borrow().clone();
            if let Some(rep) = rep {
                let (parent_id, parent_turn, item) = {
                    let links = e2.subagents.links.lock().unwrap();
                    let Some(l) = links.get(&child_id) else { return };
                    let diff = e2.loaded(&child_id).and_then(|c| c.thread().diff_stats);
                    let st =
                        if rep.status == TurnStatus::Completed { ItemStatus::Completed } else { ItemStatus::Failed };
                    (
                        l.parent.clone(),
                        l.parent_turn.clone(),
                        subagent_item(l, &child_id, st, Some(rep.text.chars().take(2000).collect()), diff),
                    )
                };
                if let Some(p) = e2.loaded(&parent_id) {
                    crate::turn::complete_item(&e2, &p, &parent_turn, item);
                }
                return;
            }
        }
    });
    ToolOutcome {
        text: format!(
            "Started subagent \"{nick}\" (id {}). It runs in parallel; call wait_agents to collect its report.",
            child.id
        ),
        meta: odex_context::ToolMeta {
            tool: call.name.clone(),
            call_id: call.id.clone(),
            args_summary: task.chars().take(80).collect(),
            success: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

pub async fn wait_tool(engine: &Engine, rt: &ThreadRt, tctx: &TurnCtx, call: &ToolCall, args: &Value) -> ToolOutcome {
    let ids: Vec<String> = match args.get("ids").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        _ => rt.children.lock().unwrap().clone(),
    };
    if ids.is_empty() {
        return ToolOutcome { text: "No subagents to wait for.".into(), ..Default::default() };
    }
    let timeout = Duration::from_millis(
        args.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(900_000).clamp(1000, 3_600_000),
    );
    let deadline = tokio::time::Instant::now() + timeout;
    let mut text = String::new();
    for id in &ids {
        let Some(mut rx) = engine.subagents.receiver(id) else {
            text.push_str(&format!("## {id}\n(unknown subagent)\n\n"));
            continue;
        };
        let nick = engine.subagents.links.lock().unwrap().get(id).map(|l| l.nickname.clone()).unwrap_or_default();
        let rep = loop {
            if let Some(r) = rx.borrow().clone() {
                break Some(r);
            }
            tokio::select! {
                r = rx.changed() => if r.is_err() { break None },
                _ = tokio::time::sleep_until(deadline) => break None,
                _ = tctx.cancel.cancelled() => break None,
            }
        };
        match rep {
            Some(r) => text.push_str(&format!("## {nick} ({id}) — {:?}\n{}\n\n", r.status, r.text.trim())),
            None => text.push_str(&format!("## {nick} ({id})\nStill running (timed out waiting).\n\n")),
        }
    }
    ToolOutcome {
        text,
        meta: odex_context::ToolMeta {
            tool: call.name.clone(),
            call_id: call.id.clone(),
            args_summary: ids.join(","),
            success: true,
            ..Default::default()
        },
        ..Default::default()
    }
}
