//! Request handlers: one function per protocol method.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use odex_git::Git;
use odex_llm::types::{ChatMessage, ChatRequest, StructuredOutput};
use odex_protocol::*;

use crate::engine::{EResult, Engine, EngineError};
use crate::rollout::RolloutLine;
use crate::thread::{new_id, now_ms, ThreadRt};
use crate::turn::{self, TurnOpts};

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::new(jsonrpc::error_codes::INVALID_PARAMS, msg)
}

fn git_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::new(jsonrpc::error_codes::GIT_ERROR, e.to_string())
}

// =================================================================== threads

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Create a thread (and its worktree in worktree mode). Shared by `thread/start`,
/// subagents and automations.
pub async fn start_thread_inner(engine: &Engine, p: ThreadStartParams) -> EResult<Arc<ThreadRt>> {
    let run_mode = p.run_mode.unwrap_or_default();
    let base_branch = p.base_branch.clone();
    let env_id = p.environment_id.clone();
    let rt = engine.create_thread(p)?;
    if run_mode == RunMode::Worktree {
        if let Err(e) = create_worktree(engine, &rt, base_branch.as_deref(), env_id.as_deref()).await {
            let msg = e.message.clone();
            engine.update_thread(&rt, |t| t.last_error = Some(format!("worktree: {msg}")));
            return Err(e);
        }
    } else {
        let cwd = rt.thread().cwd;
        if let Ok(Some(b)) = Git::new(&cwd).current_branch().await {
            engine.update_thread(&rt, |t| t.branch = Some(b));
        }
    }
    let t = rt.thread();
    engine.emitter().thread_started(&t);
    let e2 = engine.clone();
    let rt2 = rt.clone();
    tokio::spawn(async move {
        let _ = crate::hooks_rt::run(&e2, &rt2, HookEvent::SessionStart, json!({"source": "startup"}), None).await;
    });
    Ok(rt)
}

async fn create_worktree(engine: &Engine, rt: &ThreadRt, base: Option<&str>, env_id: Option<&str>) -> EResult<()> {
    let t = rt.thread();
    let root = engine.thread_root(&t);
    let g = Git::new(&root);
    if !g.is_repo().await {
        return Err(bad(format!("{} is not a git repository; worktree mode needs git", root.display())));
    }
    let repo_root = g.repo_root().await.map_err(git_err)?;
    let s = engine.thread_settings(&t);
    let project_slug =
        slug(&repo_root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "project".into()));
    let short = &t.id[t.id.len().saturating_sub(8)..];
    let path = s.worktrees_dir.join(&project_slug).join(short);
    let branch =
        format!("odex/{}", if let Some(n) = &t.name { format!("{}-{short}", slug(n)) } else { short.to_string() });
    let current = g.current_branch().await.ok().flatten();
    let base_branch = base.map(String::from).or(current.clone()).unwrap_or_else(|| "HEAD".into());
    if base == Some("current-with-changes") {
        g.worktree_add_with_changes(&path, &branch).await.map_err(git_err)?;
    } else {
        g.worktree_add(&path, &branch, &base_branch).await.map_err(git_err)?;
    }
    let _ = g.copy_worktree_include(&repo_root, &path).await;
    let base_commit = Git::new(&path).head_sha().await.ok().flatten();
    // keep the cwd's relative position inside the repo
    let rel = PathBuf::from(&t.cwd).strip_prefix(&repo_root).map(|r| r.to_path_buf()).unwrap_or_default();
    let cwd = path.join(rel);
    let wt = WorktreeInfo {
        path: path.to_string_lossy().to_string(),
        branch: branch.clone(),
        base_branch: Some(base_branch),
        base_commit,
        repo_root: repo_root.to_string_lossy().to_string(),
        setup_status: None,
    };
    engine.update_thread(rt, |t| {
        t.run_mode = RunMode::Worktree;
        t.worktree = Some(wt.clone());
        t.branch = Some(branch.clone());
        t.cwd = cwd.to_string_lossy().to_string();
    });
    // setup script from the project's environment (trusted projects only)
    if engine.is_trusted(&root) {
        let envs = load_environments(&root);
        let env = env_id.and_then(|id| envs.iter().find(|e| e.id == id)).or_else(|| envs.first()).cloned();
        if let Some(script) = env.and_then(|e| e.setup_script).filter(|s| !s.trim().is_empty()) {
            engine.update_thread(rt, |t| {
                if let Some(w) = &mut t.worktree {
                    w.setup_status = Some("running".into());
                }
            });
            let mut req =
                odex_sandbox::ExecRequest::new(odex_sandbox::shell_argv(&s.default_shell, &script), path.clone());
            req.policy = odex_sandbox::SandboxPolicy::FullAccess;
            req.timeout = Some(std::time::Duration::from_secs(1800));
            let out = odex_sandbox::exec(req, |_| {}, CancellationToken::new()).await;
            let status = match out {
                Ok(o) if o.exit_code == Some(0) => "ok".to_string(),
                Ok(o) => format!(
                    "failed (exit {:?}): {}",
                    o.exit_code,
                    o.aggregated.chars().rev().take(500).collect::<String>().chars().rev().collect::<String>()
                ),
                Err(e) => format!("failed: {e}"),
            };
            engine.update_thread(rt, |t| {
                if let Some(w) = &mut t.worktree {
                    w.setup_status = Some(status.clone());
                }
            });
        }
    }
    Ok(())
}

pub async fn thread_start(engine: &Engine, p: ThreadStartParams) -> EResult<ThreadResponse> {
    let rt = start_thread_inner(engine, p).await?;
    Ok(ThreadResponse { thread: rt.thread() })
}

pub async fn thread_read(engine: &Engine, p: ThreadIdParams) -> EResult<ThreadReadResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let ctx = {
        let c = rt.ctx.lock().await;
        engine.context_status(&rt, &c)
    };
    Ok(engine.read_response(&rt, Some(ctx)))
}

pub async fn thread_fork(engine: &Engine, p: ThreadForkParams) -> EResult<ThreadResponse> {
    let src = engine.thread(&p.thread_id)?;
    let st = src.thread();
    let turns = src.turns.lock().unwrap().clone();
    let cut = match &p.turn_id {
        Some(tid) => turns.iter().position(|t| &t.id == tid).map(|i| i + 1).ok_or_else(|| bad("turn not found"))?,
        None => turns.len(),
    };
    let keep: Vec<Turn> = turns[..cut].to_vec();
    let keep_ids: Vec<String> = keep.iter().map(|t| t.id.clone()).collect();
    let kind = p.kind.unwrap_or(ThreadKind::Normal);
    let params = ThreadStartParams {
        project_id: st.project_id.clone(),
        cwd: Some(st.worktree.as_ref().map(|w| w.repo_root.clone()).unwrap_or(st.cwd.clone())),
        kind: Some(kind),
        name: Some(format!("{} (fork)", st.name.clone().unwrap_or_else(|| "Thread".into()))),
        model: st.model.clone(),
        effort: st.effort,
        permission_mode: Some(st.permission_mode),
        run_mode: p.run_mode.or(Some(RunMode::Local)),
        parent_thread_id: Some(st.id.clone()),
        ephemeral: p.ephemeral.or(Some(kind == ThreadKind::Side)),
        ..Default::default()
    };
    let rt = start_thread_inner(engine, params).await?;
    // copy history
    let ctx = {
        let c = src.ctx.lock().await;
        let mut c2 = c.clone();
        c2.entries.retain(|e| keep_ids.contains(&e.turn_id));
        c2.last_exact = None;
        c2
    };
    *rt.ctx.lock().await = ctx.clone();
    rt.log(RolloutLine::Checkpoint { state: ctx });
    for t in &keep {
        let mut t2 = t.clone();
        t2.thread_id = rt.id.clone();
        rt.log(RolloutLine::TurnStarted { turn: Turn { items: vec![], ..t2.clone() } });
        for it in &t.items {
            rt.log(RolloutLine::Item { turn_id: t.id.clone(), item: it.clone() });
        }
        rt.log(RolloutLine::TurnCompleted { turn: Turn { items: vec![], ..t2.clone() } });
        rt.turns.lock().unwrap().push(t2);
    }
    rt.turn_counter.store(keep.len() as u32, std::sync::atomic::Ordering::SeqCst);
    let ot = src.original_task.lock().unwrap().clone();
    *rt.original_task.lock().unwrap() = ot.clone();
    rt.log(RolloutLine::Pinned { original_task: ot });
    *rt.plan.lock().unwrap() = src.plan.lock().unwrap().clone();
    engine.store.copy_recall(&st.id, &rt.id);
    let preview = st.preview.clone();
    engine.update_thread(&rt, |t| t.preview = preview);
    Ok(ThreadResponse { thread: rt.thread() })
}

pub fn thread_list(engine: &Engine, p: ThreadListParams) -> EResult<ThreadListResponse> {
    let mut threads = engine.store.list_threads(&p).map_err(EngineError::from)?;
    // live status for loaded threads
    for t in threads.iter_mut() {
        if let Some(rt) = engine.loaded(&t.id) {
            *t = rt.thread();
        }
    }
    Ok(ThreadListResponse { threads, next_cursor: None })
}

pub fn thread_search(engine: &Engine, p: ThreadSearchParams) -> EResult<ThreadSearchResponse> {
    Ok(ThreadSearchResponse {
        hits: engine.store.search_threads(&p.query, p.limit.unwrap_or(30)).map_err(EngineError::from)?,
    })
}

pub async fn thread_archive(engine: &Engine, p: ThreadArchiveParams) -> EResult<EmptyResponse> {
    let rt = engine.thread(&p.thread_id)?;
    turn::interrupt(&rt);
    engine.sessions.kill_thread(&rt.id).await;
    let t = rt.thread();
    if p.remove_worktree {
        if let Some(wt) = &t.worktree {
            let g = Git::new(&wt.repo_root);
            // snapshot before deleting so the work can be restored
            let _ =
                Git::new(&wt.path).snapshot(&format!("refs/odex/archived/{}", t.id), "odex: archived worktree").await;
            let _ = g.worktree_remove(Path::new(&wt.path), true).await;
        }
    }
    engine.update_thread(&rt, |t| {
        t.archived = true;
        t.status = ThreadStatus::Idle;
    });
    Ok(EmptyResponse {})
}

pub async fn thread_unarchive(engine: &Engine, p: ThreadIdParams) -> EResult<ThreadResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let t = engine.update_thread(&rt, |t| t.archived = false);
    Ok(ThreadResponse { thread: t })
}

pub async fn thread_delete(engine: &Engine, p: ThreadIdParams) -> EResult<EmptyResponse> {
    if let Some(rt) = engine.loaded(&p.thread_id) {
        turn::interrupt(&rt);
        let _ = std::fs::remove_file(&rt.rollout_path);
        let _ = std::fs::remove_dir_all(rt.outputs.dir());
    }
    engine.threads.lock().unwrap().remove(&p.thread_id);
    engine.store.delete_thread(&p.thread_id).map_err(EngineError::from)?;
    engine.emitter().thread_deleted(&p.thread_id);
    Ok(EmptyResponse {})
}

pub async fn thread_rollback(engine: &Engine, p: ThreadRollbackParams) -> EResult<ThreadReadResponse> {
    let rt = engine.thread(&p.thread_id)?;
    if rt.is_running() {
        return Err(EngineError::new(jsonrpc::error_codes::THREAD_BUSY, "stop the running turn before rolling back"));
    }
    let pos = rt.turns.lock().unwrap().iter().position(|t| t.id == p.turn_id).ok_or_else(|| bad("turn not found"))?;
    let dropped: Vec<String> = rt.turns.lock().unwrap()[pos..].iter().map(|t| t.id.clone()).collect();
    if p.restore_files {
        let t = rt.thread();
        let g = Git::new(&t.cwd);
        if g.is_repo().await {
            g.restore_snapshot(&crate::workspace::snapshot_ref(&rt.id, &p.turn_id)).await.map_err(git_err)?;
        }
    }
    rt.turns.lock().unwrap().truncate(pos);
    {
        let mut c = rt.ctx.lock().await;
        c.entries.retain(|e| !dropped.contains(&e.turn_id));
        c.last_exact = None;
    }
    rt.turn_counter.store(pos as u32, std::sync::atomic::Ordering::SeqCst);
    rt.log(RolloutLine::Rollback { turn_id: p.turn_id.clone() });
    rt.followups.lock().unwrap().clear();
    engine.publish(&rt);
    thread_read(engine, ThreadIdParams { thread_id: p.thread_id }).await
}

pub async fn thread_update(engine: &Engine, p: ThreadUpdateParams) -> EResult<ThreadResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let model_changed = p.model.is_some();
    let t = engine.update_thread(&rt, |t| {
        if let Some(n) = &p.name {
            t.name = if n.trim().is_empty() { None } else { Some(n.clone()) };
        }
        if let Some(v) = p.pinned {
            t.pinned = v;
        }
        if let Some(v) = p.unread {
            t.unread = v;
        }
        if let Some(m) = &p.model {
            t.model = if m.is_empty() { None } else { Some(m.clone()) };
        }
        if let Some(e) = p.effort {
            t.effort = Some(e);
        }
        if let Some(pm) = p.permission_mode {
            t.permission_mode = pm;
        }
        if let Some(m) = p.memories_enabled {
            t.memories_enabled = m;
        }
        if let Some(pid) = &p.project_id {
            t.project_id = if pid.is_empty() { None } else { Some(pid.clone()) };
        }
        if let Some(c) = &p.cwd {
            t.cwd = c.clone();
        }
        if let Some(rm) = p.run_mode {
            t.run_mode = rm;
        }
    });
    if model_changed {
        // re-budget for the new model's window (compacts on next turn if needed)
        engine.emit_context(&rt).await;
    }
    Ok(ThreadResponse { thread: t })
}

pub async fn thread_compact(engine: &Engine, p: ThreadCompactParams) -> EResult<EmptyResponse> {
    let rt = engine.thread(&p.thread_id)?;
    if rt.is_running() {
        return Err(EngineError::new(
            jsonrpc::error_codes::THREAD_BUSY,
            "a turn is running; compaction happens automatically, or try again when it finishes",
        ));
    }
    let t = rt.thread();
    let handle = engine.main_model(&t)?;
    let s = engine.thread_settings(&t);
    let turn = Turn {
        id: new_id("turn"),
        thread_id: rt.id.clone(),
        status: TurnStatus::InProgress,
        mode: TurnMode::Default,
        started_at: now_ms(),
        completed_at: None,
        error: None,
        usage: TokenUsage::default(),
        items: vec![],
    };
    rt.turns.lock().unwrap().push(turn.clone());
    rt.log(RolloutLine::TurnStarted { turn: turn.clone() });
    engine.emitter().turn_started(&rt.id, &turn);
    let e2 = engine.clone();
    tokio::spawn(async move {
        let system = e2.system_parts(&t, &s, TurnMode::Default);
        let tools =
            crate::toolexec::tool_specs(&e2, &rt, &t, &s, Some(&handle), handle.model.tool_profile, TurnMode::Default);
        let pinned = e2.pinned(&rt, &s);
        let est = e2.registry.estimator(&handle.model.key);
        let inputs = odex_context::PromptInputs {
            system: &system,
            tools: &tools,
            pinned: &pinned,
            estimator: &est,
            reasoning_history: handle.model.reasoning_history,
            current_turn: rt.turn_counter.load(std::sync::atomic::Ordering::SeqCst),
        };
        {
            let mut ctx = rt.ctx.lock().await;
            turn::manage_context(
                &e2,
                &rt,
                &turn.id,
                &handle,
                &inputs,
                &mut ctx,
                Some((CompactionTrigger::Manual, p.focus.clone())),
            )
            .await;
        }
        let mut done = turn.clone();
        done.status = TurnStatus::Completed;
        done.completed_at = Some(now_ms());
        {
            let mut turns = rt.turns.lock().unwrap();
            if let Some(t) = turns.iter_mut().find(|t| t.id == done.id) {
                t.status = done.status;
                t.completed_at = done.completed_at;
            }
        }
        rt.log(RolloutLine::TurnCompleted { turn: done.clone() });
        e2.emitter().turn_completed(&rt.id, &done);
        e2.emit_context(&rt).await;
    });
    Ok(EmptyResponse {})
}

pub async fn thread_context(engine: &Engine, p: ThreadIdParams) -> EResult<ContextGetResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let c = rt.ctx.lock().await;
    Ok(ContextGetResponse { context: engine.context_status(&rt, &c) })
}

pub async fn goal_set(engine: &Engine, p: GoalSetParams) -> EResult<ThreadResponse> {
    let rt = engine.thread(&p.thread_id)?;
    if p.objective.trim().is_empty() {
        return Err(bad("objective is empty"));
    }
    let goal = Goal {
        objective: p.objective.clone(),
        status: "active".into(),
        started_at: now_ms(),
        time_budget_secs: p.time_budget_secs,
        token_budget: p.token_budget,
        tokens_used: 0,
        turns: 0,
        last_update: None,
    };
    let t = engine.update_thread(&rt, |t| t.goal = Some(goal));
    if !rt.is_running() {
        turn::spawn_turn(
            engine,
            rt.clone(),
            vec![UserInput::text(format!("Goal: {}", p.objective))],
            TurnOpts::default(),
        );
    }
    Ok(ThreadResponse { thread: t })
}

pub async fn goal_clear(engine: &Engine, p: ThreadIdParams) -> EResult<ThreadResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let t = engine.update_thread(&rt, |t| {
        if let Some(g) = &mut t.goal {
            g.status = "cleared".into();
        }
    });
    Ok(ThreadResponse { thread: t })
}

pub async fn approve_override(engine: &Engine, p: ApproveOverrideParams) -> EResult<EmptyResponse> {
    let rt = engine.thread(&p.thread_id)?;
    if rt.denials.lock().unwrap().is_empty() {
        return Err(bad("there is no recent automatic-review denial to override"));
    }
    rt.override_next_denial.store(true, std::sync::atomic::Ordering::SeqCst);
    if !rt.is_running() {
        let reason = rt.denials.lock().unwrap().last().map(|d| d.reason.clone()).unwrap_or_default();
        turn::spawn_turn(
            engine,
            rt.clone(),
            vec![UserInput::text(format!(
                "/approve — I approve the action that automatic review denied ({reason}). Retry it."
            ))],
            TurnOpts::default(),
        );
    }
    Ok(EmptyResponse {})
}

pub async fn plan_decide(engine: &Engine, p: PlanDecisionParams) -> EResult<TurnStartResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let markdown = {
        let turns = rt.turns.lock().unwrap();
        turns.iter().flat_map(|t| t.items.iter()).find_map(|i| match i {
            ThreadItem::ProposedPlan { id, markdown, .. } if id == &p.item_id => Some(markdown.clone()),
            _ => None,
        })
    }
    .ok_or_else(|| bad("plan item not found"))?;
    match p.decision.as_str() {
        "reject" => Ok(TurnStartResponse { turn: None, queued: false, steered: false }),
        _ => {
            let plan = p.markdown.clone().unwrap_or(markdown);
            // mark approved
            let turn_id = rt
                .turns
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.items.iter().any(|i| i.id() == p.item_id))
                .map(|t| t.id.clone());
            if let Some(tid) = turn_id {
                turn::complete_item(
                    engine,
                    &rt,
                    &tid,
                    ThreadItem::ProposedPlan { id: p.item_id.clone(), markdown: plan.clone(), approved: true },
                );
            }
            let input = vec![UserInput::text(format!(
                "The plan is approved{}. Execute it now:\n\n{plan}",
                if p.decision == "edit" { " with my edits" } else { "" }
            ))];
            turn::start_turn(
                engine,
                TurnStartParams { thread_id: p.thread_id, input, mode: Some(TurnMode::Default), ..Default::default() },
            )
            .await
        }
    }
}

pub async fn init_agents_md(engine: &Engine, p: InitAgentsMdParams) -> EResult<TurnStartResponse> {
    let prompt = "Generate an AGENTS.md file for this repository (create or improve it at the project root). Explore the codebase first \
(list_dir, read key config files like package.json, Cargo.toml, pyproject.toml, Makefile, README). Include: a one-paragraph project overview, \
repository layout, exact commands to build, test, lint and run, code style and conventions, testing guidance, and gotchas. Keep it concise and accurate; \
only include commands you verified exist.";
    turn::start_turn(
        engine,
        TurnStartParams { thread_id: p.thread_id, input: vec![UserInput::text(prompt)], ..Default::default() },
    )
    .await
}

pub async fn queue_set(engine: &Engine, p: QueueSetParams) -> EResult<EmptyResponse> {
    let rt = engine.thread(&p.thread_id)?;
    *rt.queue.lock().unwrap() = p.queued.clone();
    engine.emitter().queue(&rt.id, p.queued);
    Ok(EmptyResponse {})
}

/// `!cmd`: user-initiated command, unsandboxed, recorded in the thread.
pub async fn shell_command(engine: &Engine, p: ShellCommandParams) -> EResult<EmptyResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let t = rt.thread();
    let s = engine.thread_settings(&t);
    let turn = Turn {
        id: new_id("turn"),
        thread_id: rt.id.clone(),
        status: TurnStatus::InProgress,
        mode: TurnMode::Default,
        started_at: now_ms(),
        completed_at: None,
        error: None,
        usage: TokenUsage::default(),
        items: vec![],
    };
    rt.turns.lock().unwrap().push(turn.clone());
    rt.log(RolloutLine::TurnStarted { turn: turn.clone() });
    engine.emitter().turn_started(&rt.id, &turn);
    let e2 = engine.clone();
    tokio::spawn(async move {
        let item_id = new_id("item");
        let mk = |status, code, dur, out: &str| ThreadItem::CommandExecution {
            id: item_id.clone(),
            command: p.command.clone(),
            cwd: t.cwd.clone(),
            status,
            exit_code: code,
            duration_ms: dur,
            output: out.to_string(),
            output_ref: None,
            sandboxed: false,
            session_id: None,
        };
        e2.emitter().item_started(&rt.id, &turn.id, &mk(ItemStatus::InProgress, None, None, ""));
        let mut req = odex_sandbox::ExecRequest::new(
            odex_sandbox::shell_argv(&s.default_shell, &p.command),
            PathBuf::from(&t.cwd),
        );
        req.policy = odex_sandbox::SandboxPolicy::FullAccess;
        req.timeout = Some(std::time::Duration::from_millis(p.timeout_ms.unwrap_or(600_000) as u64));
        let em = e2.emitter();
        let (tid, trn, iid) = (rt.id.clone(), turn.id.clone(), item_id.clone());
        let r = odex_sandbox::exec(
            req,
            move |c| {
                em.item_delta(
                    &tid,
                    &trn,
                    &iid,
                    ItemDelta::CommandOutput {
                        chunk: String::from_utf8_lossy(c.bytes()).into(),
                        stream: "stdout".into(),
                    },
                )
            },
            CancellationToken::new(),
        )
        .await;
        let (item, text) = match r {
            Ok(o) => (
                mk(
                    if o.exit_code == Some(0) { ItemStatus::Completed } else { ItemStatus::Failed },
                    o.exit_code,
                    Some(o.duration.as_millis() as u64),
                    &o.aggregated,
                ),
                format!("$ {}\n{}\n(exit {:?})", p.command, o.aggregated, o.exit_code),
            ),
            Err(e) => (mk(ItemStatus::Failed, None, None, &e.to_string()), format!("$ {}\nfailed: {e}", p.command)),
        };
        turn::complete_item(&e2, &rt, &turn.id, item);
        // the model sees user-run commands in the next turn
        {
            let mut c = rt.ctx.lock().await;
            let idx = rt.turn_counter.load(std::sync::atomic::Ordering::SeqCst);
            let entry = odex_context::HistoryEntry::new(
                &turn.id,
                idx,
                odex_context::EntryKind::User,
                odex_llm::types::ChatMessage::user(format!(
                    "[The user ran a command]\n{}",
                    odex_context::summary::clip_middle(&text, 6000)
                )),
            );
            rt.log(RolloutLine::History { entry: entry.clone() });
            c.push(entry, &Default::default(), 2);
        }
        let mut done = turn.clone();
        done.status = TurnStatus::Completed;
        done.completed_at = Some(now_ms());
        rt.log(RolloutLine::TurnCompleted { turn: done.clone() });
        if let Some(t) = rt.turns.lock().unwrap().iter_mut().find(|t| t.id == done.id) {
            t.status = done.status;
            t.completed_at = done.completed_at;
        }
        e2.emitter().turn_completed(&rt.id, &done);
    });
    Ok(EmptyResponse {})
}

pub async fn review_start(engine: &Engine, p: ReviewStartParams) -> EResult<TurnStartResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let t = rt.thread();
    let last_turn = rt.turns.lock().unwrap().iter().rev().find(|t| t.mode == TurnMode::Default).map(|t| t.id.clone());
    let diff =
        crate::workspace::review_diff_text(&t.cwd, &p.target, &rt.id, last_turn.as_deref()).await.map_err(git_err)?;
    if diff.trim().is_empty() {
        return Err(bad("there are no changes to review for that target"));
    }
    let what = match &p.target {
        DiffTarget::Uncommitted => "uncommitted changes".to_string(),
        DiffTarget::Staged => "staged changes".to_string(),
        DiffTarget::Unstaged => "unstaged changes".to_string(),
        DiffTarget::Base { branch } => format!("changes against {branch}"),
        DiffTarget::Commit { sha } => format!("commit {sha}"),
        DiffTarget::LastTurn { .. } => "the last turn's changes".to_string(),
    };
    let reviewer = engine.role_model(ModelRole::Reviewer, Some(&t)).map(|h| h.model.key.clone());
    let diff_clip = odex_context::summary::clip_middle(&diff, 120_000);
    let hidden = format!("Diff under review ({what}):\n```diff\n{diff_clip}\n```");
    let mut text = format!("Review {what}.");
    if let Some(i) = &p.instructions {
        text.push_str(&format!(" Focus: {i}"));
    }
    if let Some(r) = &engine.thread_settings(&t).raw.custom_instructions {
        let _ = r;
    }
    if rt.is_running() {
        return Err(EngineError::new(jsonrpc::error_codes::THREAD_BUSY, "a turn is running"));
    }
    let prev_model = t.model.clone();
    if let Some(r) = reviewer {
        engine.update_thread(&rt, |t| t.model = Some(r));
    }
    let turn = turn::spawn_turn(
        engine,
        rt.clone(),
        vec![UserInput::text(text)],
        TurnOpts { mode: TurnMode::Review, hidden_context: Some(hidden), ..Default::default() },
    );
    // restore the thread's model once the review turn is set up
    let e2 = engine.clone();
    let rt2 = rt.clone();
    tokio::spawn(async move {
        while rt2.is_running() {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        e2.update_thread(&rt2, |t| t.model = prev_model);
    });
    Ok(TurnStartResponse { turn: Some(turn), queued: false, steered: false })
}

// ===================================================================== models

pub fn model_list(engine: &Engine) -> ModelListResponse {
    let s = engine.user_settings();
    ModelListResponse {
        models: engine.registry.list(),
        roles: s.roles.iter().map(|(k, v)| (k.as_str().to_string(), v.clone())).collect(),
    }
}

pub async fn provider_list(engine: &Engine, p: ProviderListParams) -> ProviderListResponse {
    if p.refresh {
        engine.registry.refresh().await;
    }
    ProviderListResponse { providers: engine.registry.providers() }
}

pub async fn provider_upsert(engine: &Engine, p: ProviderUpsertParams) -> EResult<ProviderListResponse> {
    if p.id.trim().is_empty() || p.id.contains(['.', '"', '\'']) {
        return Err(bad("provider id must be a simple name"));
    }
    let mut prov = p.provider.clone();
    if prov.api_key.as_deref() == Some("") {
        prov.api_key = None;
    }
    let value = serde_json::to_value(&prov).map_err(|e| bad(e.to_string()))?;
    config_write(
        engine,
        ConfigWriteParams {
            edits: vec![ConfigEdit { key_path: format!("model_providers.{}", p.id), value }],
            project_path: None,
        },
    )?;
    if let Some(k) = p.api_key {
        let key = odex_llm::registry::secret_key_for_provider(&p.id);
        engine.registry.set_secret(&key, Some(k.clone()));
        engine.secrets.write().unwrap().insert(key.clone(), k.clone());
        let _ =
            engine.emitter().request(server_request::SECRETS_STORE, &SecretsStoreParams { key, value: Some(k) }).await;
    }
    engine.registry.refresh().await;
    let list = ProviderListResponse { providers: engine.registry.providers() };
    engine.emitter().raw(notification::PROVIDERS_UPDATED, &ProvidersNotification { providers: list.providers.clone() });
    Ok(list)
}

pub async fn provider_remove(engine: &Engine, p: ProviderIdParams) -> EResult<ProviderListResponse> {
    config_write(
        engine,
        ConfigWriteParams {
            edits: vec![ConfigEdit { key_path: format!("model_providers.{}", p.id), value: Value::Null }],
            project_path: None,
        },
    )?;
    let key = odex_llm::registry::secret_key_for_provider(&p.id);
    engine.registry.set_secret(&key, None);
    let _ = engine.emitter().request(server_request::SECRETS_STORE, &SecretsStoreParams { key, value: None }).await;
    engine.registry.refresh().await;
    Ok(ProviderListResponse { providers: engine.registry.providers() })
}

pub async fn provider_test(engine: &Engine, p: ProviderTestParams) -> EResult<ProviderTestResult> {
    let t0 = std::time::Instant::now();
    let client = match (&p.provider, &p.id) {
        (Some(prov), id) => {
            let r = odex_config::ResolvedProvider::from_toml(id.as_deref().unwrap_or("test"), prov);
            Arc::new(odex_llm::LlmClient::new(r, p.api_key.clone()))
        }
        (None, Some(id)) => engine.registry.client(id).ok_or_else(|| bad(format!("unknown provider {id}")))?,
        (None, None) => return Err(bad("give a provider id or definition")),
    };
    let probe = odex_llm::discovery::probe(&client).await;
    Ok(ProviderTestResult {
        ok: probe.reachable && probe.error.is_none(),
        latency_ms: t0.elapsed().as_millis() as u64,
        version: probe.version,
        models: probe.models,
        error: probe.error,
    })
}

pub async fn doctor_run(engine: &Engine, p: DoctorRunParams) -> EResult<DoctorRunResponse> {
    engine.registry.refresh().await;
    let mut handles = Vec::new();
    if let Some(m) = &p.model {
        handles.push(engine.registry.resolve(m).ok_or_else(|| bad(format!("model {m} not found")))?);
    } else {
        let providers = engine.registry.providers();
        for prov in providers.iter().filter(|x| p.provider_id.as_deref().map(|id| id == x.id).unwrap_or(true)) {
            if let Some(m) = prov.models.first() {
                if let Some(h) = engine.registry.resolve(&format!("{}:{}", prov.id, m.id)) {
                    handles.push(h);
                }
            } else if let Some(c) = engine.registry.client(&prov.id) {
                // unreachable endpoint: report the connection failure
                let r = odex_config::ResolvedModel::discovered(&prov.id, "unknown", engine.registry.presets());
                handles.push(odex_llm::ModelHandle { model: r, client: c, context_window: 32768 });
            }
        }
    }
    let mut reports = Vec::new();
    for h in handles {
        let rep = odex_llm::doctor::run(&engine.registry, &h, p.quick).await;
        if rep.checks.iter().any(|c| c.id == "streaming") {
            engine.registry.record_doctor(&rep);
        }
        reports.push(rep);
    }
    Ok(DoctorRunResponse { reports })
}

pub fn preset_list(engine: &Engine) -> PresetListResponse {
    PresetListResponse { presets: engine.registry.presets().all().to_vec() }
}

// ===================================================================== config

pub fn config_read(engine: &Engine) -> ConfigReadResponse {
    let c = engine.config.read().unwrap();
    ConfigReadResponse {
        path: engine.home.config_path().to_string_lossy().to_string(),
        odex_home: engine.home.root().to_string_lossy().to_string(),
        user: c.user.clone(),
        effective: c.effective.clone(),
        active_profile: c.active_profile.clone(),
        profiles: c.user.profiles.keys().cloned().collect(),
        warnings: c.warnings.clone(),
    }
}

pub fn config_write(engine: &Engine, p: ConfigWriteParams) -> EResult<ConfigReadResponse> {
    let path = match &p.project_path {
        Some(root) => {
            if !engine.is_trusted(Path::new(root)) {
                return Err(EngineError::new(jsonrpc::error_codes::NOT_TRUSTED, "the project is not trusted"));
            }
            odex_config::project_dir(Path::new(root)).join("config.toml")
        }
        None => engine.home.config_path(),
    };
    odex_config::edit::write_edits(&path, &p.edits).map_err(|e| bad(format!("{e:#}")))?;
    engine.reload_config().map_err(EngineError::from)?;
    let touches_mcp = p.edits.iter().any(|e| e.key_path.starts_with("mcp_servers"));
    let touches_providers =
        p.edits.iter().any(|e| e.key_path.starts_with("model_providers") || e.key_path.starts_with("models"));
    let e2 = engine.clone();
    tokio::spawn(async move {
        if touches_mcp {
            crate::extensions::sync_mcp(&e2).await;
        }
        if touches_providers {
            e2.registry.refresh().await;
            e2.emitter()
                .raw(notification::PROVIDERS_UPDATED, &ProvidersNotification { providers: e2.registry.providers() });
        }
    });
    Ok(config_read(engine))
}

// ==================================================================== projects

fn load_actions(root: &Path) -> Vec<ProjectAction> {
    #[derive(serde::Deserialize, Default)]
    struct F {
        #[serde(default)]
        action: Vec<ProjectAction>,
    }
    std::fs::read_to_string(odex_config::project_dir(root).join("actions.toml"))
        .ok()
        .and_then(|t| toml::from_str::<F>(&t).ok())
        .map(|f| f.action)
        .unwrap_or_default()
}

pub fn load_environments(root: &Path) -> Vec<Environment> {
    #[derive(serde::Deserialize)]
    struct E {
        id: String,
        name: Option<String>,
        setup_script: Option<String>,
        #[serde(default)]
        setup_scripts: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        env: std::collections::BTreeMap<String, String>,
    }
    #[derive(serde::Deserialize, Default)]
    struct F {
        #[serde(default)]
        environment: Vec<E>,
    }
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    std::fs::read_to_string(odex_config::project_dir(root).join("environments.toml"))
        .ok()
        .and_then(|t| toml::from_str::<F>(&t).ok())
        .map(|f| {
            f.environment
                .into_iter()
                .map(|e| Environment {
                    name: e.name.unwrap_or_else(|| e.id.clone()),
                    setup_script: e.setup_scripts.get(os).cloned().or(e.setup_script),
                    id: e.id,
                    env: e.env,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn save_actions(root: &Path, actions: &[ProjectAction]) -> std::io::Result<()> {
    #[derive(serde::Serialize)]
    struct F<'a> {
        action: &'a [ProjectAction],
    }
    std::fs::create_dir_all(odex_config::project_dir(root))?;
    let text = toml::to_string_pretty(&F { action: actions }).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(odex_config::project_dir(root).join("actions.toml"), text)
}

fn save_environments(root: &Path, envs: &[Environment]) -> std::io::Result<()> {
    #[derive(serde::Serialize)]
    struct E<'a> {
        id: &'a str,
        name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        setup_script: &'a Option<String>,
        env: &'a std::collections::BTreeMap<String, String>,
    }
    #[derive(serde::Serialize)]
    struct F<'a> {
        environment: Vec<E<'a>>,
    }
    std::fs::create_dir_all(odex_config::project_dir(root))?;
    let f = F {
        environment: envs
            .iter()
            .map(|e| E { id: &e.id, name: &e.name, setup_script: &e.setup_script, env: &e.env })
            .collect(),
    };
    let text = toml::to_string_pretty(&f).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(odex_config::project_dir(root).join("environments.toml"), text)
}

fn hydrate(engine: &Engine, mut p: Project) -> Project {
    let root = PathBuf::from(p.primary_folder());
    p.trusted = engine.is_trusted(&root);
    p.is_git = root.join(".git").exists();
    if p.trusted {
        p.actions = load_actions(&root);
        p.environments = load_environments(&root);
    } else {
        p.actions = vec![];
        p.environments = vec![];
    }
    p
}

pub fn project_list(engine: &Engine) -> EResult<ProjectListResponse> {
    let projects =
        engine.store.projects().map_err(EngineError::from)?.into_iter().map(|p| hydrate(engine, p)).collect();
    Ok(ProjectListResponse { projects })
}

fn notify_projects(engine: &Engine) {
    if let Ok(r) = project_list(engine) {
        engine.emitter().raw(notification::PROJECTS_CHANGED, &ProjectsChangedNotification { projects: r.projects });
    }
}

pub async fn project_add(engine: &Engine, p: ProjectAddParams) -> EResult<ProjectResponse> {
    if p.folders.is_empty() {
        return Err(bad("at least one folder is required"));
    }
    let folders: Vec<String> =
        p.folders.iter().map(|f| odex_config::home::dunce_like(Path::new(f)).to_string_lossy().to_string()).collect();
    let primary = p.primary.unwrap_or(0).min(folders.len() as u32 - 1);
    let root = PathBuf::from(&folders[primary as usize]);
    if p.create && !root.exists() {
        std::fs::create_dir_all(&root).map_err(|e| bad(e.to_string()))?;
        let _ = Git::new(&root).init().await;
    }
    if !root.is_dir() {
        return Err(bad(format!("{} is not a folder", root.display())));
    }
    // existing project with the same primary folder?
    let existing = engine
        .store
        .projects()
        .map_err(EngineError::from)?
        .into_iter()
        .find(|x| odex_config::normalize_path(Path::new(x.primary_folder())) == odex_config::normalize_path(&root));
    let mut project = existing.unwrap_or(Project {
        id: new_id("proj"),
        name: p.name.clone().unwrap_or_else(|| {
            root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Project".into())
        }),
        folders: folders.clone(),
        primary,
        trusted: false,
        actions: vec![],
        environments: vec![],
        default_environment: None,
        created_at: now_ms(),
        last_opened_at: now_ms(),
        collapsed: false,
        is_git: false,
    });
    project.last_opened_at = now_ms();
    engine.store.upsert_project(&project).map_err(EngineError::from)?;
    let project = hydrate(engine, project);
    notify_projects(engine);
    Ok(ProjectResponse { project })
}

pub fn project_update(engine: &Engine, p: ProjectUpdateParams) -> EResult<ProjectResponse> {
    let mut project = engine.project(&p.id)?;
    if let Some(n) = p.name {
        project.name = n;
    }
    if let Some(f) = p.folders {
        if f.is_empty() {
            return Err(bad("a project needs at least one folder"));
        }
        project.folders = f;
    }
    if let Some(pr) = p.primary {
        project.primary = pr.min(project.folders.len() as u32 - 1);
    }
    if let Some(c) = p.collapsed {
        project.collapsed = c;
    }
    if let Some(d) = p.default_environment {
        project.default_environment = Some(d);
    }
    let root = PathBuf::from(project.primary_folder());
    if p.actions.is_some() || p.environments.is_some() {
        if !engine.is_trusted(&root) {
            return Err(EngineError::new(
                jsonrpc::error_codes::NOT_TRUSTED,
                "trust the project before editing its actions or environments",
            ));
        }
        if let Some(a) = &p.actions {
            save_actions(&root, a).map_err(|e| bad(e.to_string()))?;
        }
        if let Some(e) = &p.environments {
            save_environments(&root, e).map_err(|e| bad(e.to_string()))?;
        }
    }
    engine.store.upsert_project(&project).map_err(EngineError::from)?;
    let project = hydrate(engine, project);
    notify_projects(engine);
    Ok(ProjectResponse { project })
}

pub fn project_remove(engine: &Engine, p: ProjectIdParams) -> EResult<EmptyResponse> {
    engine.store.delete_project(&p.id).map_err(EngineError::from)?;
    notify_projects(engine);
    Ok(EmptyResponse {})
}

pub fn trust_check(engine: &Engine, p: PathParams) -> TrustCheckResponse {
    let path = PathBuf::from(&p.path);
    let level = engine.config.read().unwrap().trust_level(&path);
    TrustCheckResponse {
        path: p.path.clone(),
        trusted: level.unwrap_or(false),
        unknown: level.is_none(),
        has_odex_dir: odex_config::project_dir(&path).is_dir(),
        has_agents_md: path.join("AGENTS.md").is_file(),
    }
}

pub fn trust_set(engine: &Engine, p: TrustParams) -> EResult<TrustCheckResponse> {
    let norm = odex_config::home::dunce_like(Path::new(&p.path)).to_string_lossy().to_string();
    let key = format!("projects.\"{}\".trust_level", norm.replace('\\', "\\\\").replace('"', "\\\""));
    config_write(
        engine,
        ConfigWriteParams {
            edits: vec![ConfigEdit { key_path: key, value: json!(if p.trusted { "trusted" } else { "untrusted" }) }],
            project_path: None,
        },
    )?;
    notify_projects(engine);
    Ok(trust_check(engine, PathParams { path: p.path }))
}

pub fn fs_search(engine: &Engine, p: FileSearchParams) -> FileSearchResponse {
    let key = p.roots.join("|");
    let roots: Vec<PathBuf> = p.roots.iter().map(PathBuf::from).collect();
    let mut idx = engine.file_indexes.lock().unwrap();
    let entry = idx.entry(key).or_insert_with(|| odex_file_search::FileIndex::build(&roots, 200_000));
    if p.query.is_empty() {
        entry.refresh();
    }
    FileSearchResponse { files: entry.search(&p.query, p.limit.unwrap_or(50) as usize) }
}

// ========================================================================= git

pub async fn git_status(p: CwdParams) -> EResult<GitStatus> {
    Git::new(&p.cwd).status().await.map_err(git_err)
}

pub async fn git_diff(engine: &Engine, p: GitDiffParams) -> EResult<GitDiffResponse> {
    let mut repos = Vec::new();
    for cwd in &p.cwds {
        let g = Git::new(cwd);
        if !g.is_repo().await {
            continue;
        }
        let d = match &p.target {
            DiffTarget::LastTurn { thread_id } => {
                let rt = engine.thread(thread_id)?;
                let last =
                    rt.turns.lock().unwrap().iter().rev().find(|t| t.mode == TurnMode::Default).map(|t| t.id.clone());
                match last {
                    Some(tid) => {
                        let mut d = g
                            .diff_from_ref(&crate::workspace::snapshot_ref(thread_id, &tid), p.ignore_whitespace)
                            .await
                            .map_err(git_err)?;
                        d.target = p.target.clone();
                        d
                    }
                    None => continue,
                }
            }
            t => g.diff(t, p.ignore_whitespace, p.context_lines).await.map_err(git_err)?,
        };
        repos.push(d);
    }
    Ok(GitDiffResponse { repos })
}

async fn hunk_file(g: &Git, h: &HunkRef) -> EResult<DiffFile> {
    let d = g.diff(&h.target, h.ignore_whitespace, None).await.map_err(git_err)?;
    d.files
        .into_iter()
        .find(|f| f.path == h.path)
        .ok_or_else(|| bad(format!("{} has no changes in that view any more; refresh", h.path)))
}

pub async fn git_stage(p: GitPathOpParams) -> EResult<EmptyResponse> {
    let g = Git::new(&p.cwd);
    match &p.hunk {
        Some(h) => {
            let f = hunk_file(&g, h).await?;
            g.stage_hunk(&f, h.hunk_index).await.map_err(git_err)?
        }
        None => g.stage(&p.paths).await.map_err(git_err)?,
    }
    Ok(EmptyResponse {})
}

pub async fn git_unstage(p: GitPathOpParams) -> EResult<EmptyResponse> {
    let g = Git::new(&p.cwd);
    match &p.hunk {
        Some(h) => {
            let f = hunk_file(&g, h).await?;
            g.unstage_hunk(&f, h.hunk_index).await.map_err(git_err)?
        }
        None => g.unstage(&p.paths).await.map_err(git_err)?,
    }
    Ok(EmptyResponse {})
}

pub async fn git_revert(p: GitPathOpParams) -> EResult<EmptyResponse> {
    let g = Git::new(&p.cwd);
    match &p.hunk {
        Some(h) => {
            let f = hunk_file(&g, h).await?;
            g.revert_hunk(&f, h.hunk_index).await.map_err(git_err)?
        }
        None => {
            if p.paths.is_empty() {
                return Err(bad("refusing to revert everything without explicit paths"));
            }
            g.revert(&p.paths).await.map_err(git_err)?
        }
    }
    Ok(EmptyResponse {})
}

pub async fn git_commit(p: GitCommitParams) -> EResult<GitCommitResponse> {
    if p.message.trim().is_empty() {
        return Err(bad("commit message is empty"));
    }
    let (sha, summary) = Git::new(&p.cwd).commit(&p.message, p.all, p.amend).await.map_err(git_err)?;
    Ok(GitCommitResponse { sha, summary })
}

async fn diff_for_message(cwd: &str, staged_only: Option<bool>) -> EResult<String> {
    let g = Git::new(cwd);
    let staged = g.diff(&DiffTarget::Staged, false, Some(2)).await.map_err(git_err)?;
    let use_staged = staged_only.unwrap_or(!staged.files.is_empty());
    let d =
        if use_staged { staged } else { g.diff(&DiffTarget::Uncommitted, false, Some(2)).await.map_err(git_err)? };
    let mut s = String::new();
    for f in &d.files {
        s.push_str(&format!("--- {} ({}, +{} -{})\n", f.path, f.status, f.additions, f.deletions));
        for h in &f.hunks {
            s.push_str(&h.header);
            s.push('\n');
            for l in &h.lines {
                let c = match l.kind {
                    DiffLineKind::Add => '+',
                    DiffLineKind::Del => '-',
                    _ => ' ',
                };
                s.push(c);
                s.push_str(&l.text);
                s.push('\n');
            }
        }
    }
    Ok(s)
}

pub async fn git_commit_message(engine: &Engine, p: CommitMessageParams) -> EResult<CommitMessageResponse> {
    let diff = diff_for_message(&p.cwd, p.staged_only).await?;
    if diff.trim().is_empty() {
        return Err(bad("there are no changes to commit"));
    }
    let t = p.thread_id.as_deref().and_then(|id| engine.thread(id).ok()).map(|rt| rt.thread());
    let task =
        p.thread_id.as_deref().and_then(|id| engine.loaded(id)).and_then(|rt| rt.original_task.lock().unwrap().clone());
    let fallback = || {
        let files: Vec<String> = diff
            .lines()
            .filter_map(|l| l.strip_prefix("--- ").map(|x| x.split(' ').next().unwrap_or("").to_string()))
            .take(4)
            .collect();
        format!("Update {}", files.join(", "))
    };
    let Some(h) = engine.role_model(ModelRole::Utility, t.as_ref()) else {
        return Ok(CommitMessageResponse { message: fallback() });
    };
    let schema = json!({"type": "object", "properties": {"subject": {"type": "string"}, "body": {"type": "string"}}, "required": ["subject", "body"]});
    let user = format!(
        "{}Diff:\n{}",
        task.map(|t| format!("Task context: {}\n\n", t.chars().take(800).collect::<String>())).unwrap_or_default(),
        odex_context::summary::clip_middle(&diff, 24_000)
    );
    let req = ChatRequest {
        messages: vec![
            ChatMessage::system("Write a git commit message for this diff. Subject: imperative mood, at most 72 characters, no trailing period. Body: 1-4 short lines explaining what and why (empty for trivial changes). JSON only."),
            ChatMessage::user(user),
        ],
        max_tokens: Some(400),
        structured: Some(StructuredOutput { name: "commit_message".into(), schema }),
        effort: Some(ReasoningEffort::None),
        temperature_override: Some(0.2),
        ..Default::default()
    };
    let msg = match h.client.chat(&h.model, &req, &CancellationToken::new()).await {
        Ok(r) => match odex_llm::repair::parse_lenient(&r.content) {
            Ok((v, _)) => {
                let subject = v.get("subject").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                if subject.is_empty() {
                    fallback()
                } else if body.is_empty() {
                    subject
                } else {
                    format!("{subject}\n\n{body}")
                }
            }
            Err(_) => r.content.trim().to_string(),
        },
        Err(_) => fallback(),
    };
    Ok(CommitMessageResponse { message: msg })
}

pub async fn git_push(p: GitPushParams) -> EResult<CommandOutputResponse> {
    match Git::new(&p.cwd).push(p.remote.as_deref(), p.branch.as_deref(), p.set_upstream, p.force_with_lease).await {
        Ok(out) => Ok(CommandOutputResponse { ok: true, output: out }),
        Err(e) => Ok(CommandOutputResponse { ok: false, output: e.to_string() }),
    }
}

pub async fn git_branches(p: CwdParams) -> EResult<GitBranchesResponse> {
    Ok(GitBranchesResponse { branches: Git::new(&p.cwd).branches().await.map_err(git_err)? })
}

pub async fn git_log(p: GitLogParams) -> EResult<GitLogResponse> {
    Ok(GitLogResponse { commits: Git::new(&p.cwd).log(p.limit.unwrap_or(50)).await.map_err(git_err)? })
}

pub async fn worktree_handoff(engine: &Engine, p: HandoffParams) -> EResult<HandoffResult> {
    let rt = engine.thread(&p.thread_id)?;
    let t = rt.thread();
    let wt = t.worktree.clone().ok_or_else(|| bad("this thread does not run in a worktree"))?;
    let g = Git::new(&wt.repo_root);
    let target = p.target_branch.clone().or(wt.base_branch.clone());
    let res = g
        .handoff(Path::new(&wt.path), p.strategy, target.as_deref(), p.commit_message.as_deref())
        .await
        .map_err(git_err)?;
    if res.ok && p.strategy == HandoffStrategy::Checkout {
        // the thread continues in the local checkout
        engine.update_thread(&rt, |t| {
            t.cwd = wt.repo_root.clone();
            t.run_mode = RunMode::Local;
        });
    }
    Ok(res)
}

pub async fn worktree_list(engine: &Engine) -> EResult<WorktreeListResponse> {
    let threads = engine
        .store
        .list_threads(&ThreadListParams { archived: None, limit: Some(5000), ..Default::default() })
        .map_err(EngineError::from)?;
    Ok(WorktreeListResponse { worktrees: threads.into_iter().filter_map(|t| t.worktree).collect() })
}

pub async fn worktree_remove(engine: &Engine, p: ThreadIdParams) -> EResult<EmptyResponse> {
    let rt = engine.thread(&p.thread_id)?;
    let t = rt.thread();
    let wt = t.worktree.clone().ok_or_else(|| bad("no worktree"))?;
    let _ = Git::new(&wt.path).snapshot(&format!("refs/odex/archived/{}", t.id), "odex: removed worktree").await;
    Git::new(&wt.repo_root).worktree_remove(Path::new(&wt.path), true).await.map_err(git_err)?;
    engine.update_thread(&rt, |t| {
        t.worktree = None;
        t.run_mode = RunMode::Local;
        t.cwd = wt.repo_root.clone();
    });
    Ok(EmptyResponse {})
}

fn github_token(engine: &Engine) -> Option<String> {
    engine.secrets.read().unwrap().get("github:token").cloned()
}

pub async fn pr_create(engine: &Engine, p: PrCreateParams) -> EResult<PrCreateResponse> {
    let (url, number) = odex_git::pr::create(
        Path::new(&p.cwd),
        &p.title,
        &p.body,
        p.base.as_deref(),
        p.draft,
        github_token(engine).as_deref(),
    )
    .await
    .map_err(git_err)?;
    Ok(PrCreateResponse { url, number })
}

pub async fn pr_view(engine: &Engine, p: PrViewParams) -> EResult<PrViewResponse> {
    match odex_git::pr::view(Path::new(&p.cwd), p.number, github_token(engine).as_deref()).await {
        Ok(pr) => Ok(PrViewResponse { pr, error: None }),
        Err(e) => Ok(PrViewResponse { pr: None, error: Some(e.to_string()) }),
    }
}

pub async fn pr_comment(engine: &Engine, p: PrCommentParams) -> EResult<CommandOutputResponse> {
    if !p.confirmed {
        return Err(bad("posting review comments requires explicit confirmation"));
    }
    let event = "COMMENT";
    match odex_git::pr::submit_review(
        Path::new(&p.cwd),
        p.number,
        &p.comments,
        p.body.as_deref(),
        event,
        github_token(engine).as_deref(),
    )
    .await
    {
        Ok(out) => Ok(CommandOutputResponse { ok: true, output: out }),
        Err(e) => Ok(CommandOutputResponse { ok: false, output: e.to_string() }),
    }
}

pub async fn pr_draft(engine: &Engine, p: CommitMessageParams) -> EResult<PrDraftResponse> {
    let g = Git::new(&p.cwd);
    let base = g.default_branch().await.unwrap_or_else(|_| "main".into());
    let d = g.diff(&DiffTarget::Base { branch: base }, false, Some(1)).await.map_err(git_err)?;
    let log = g.log(20).await.unwrap_or_default();
    let mut summary = String::new();
    for c in log.iter().take(20) {
        summary.push_str(&format!("- {}\n", c.subject));
    }
    for f in d.files.iter().take(80) {
        summary.push_str(&format!("{} +{} -{}\n", f.path, f.additions, f.deletions));
    }
    let t = p.thread_id.as_deref().and_then(|id| engine.thread(id).ok()).map(|rt| rt.thread());
    let Some(h) = engine.role_model(ModelRole::Utility, t.as_ref()) else {
        return Ok(PrDraftResponse {
            title: log.first().map(|c| c.subject.clone()).unwrap_or_else(|| "Update".into()),
            body: summary,
        });
    };
    let schema = json!({"type": "object", "properties": {"title": {"type": "string"}, "body": {"type": "string"}}, "required": ["title", "body"]});
    let req = ChatRequest {
        messages: vec![
            ChatMessage::system("Write a GitHub pull request title (under 72 chars) and a Markdown body with a Summary section and a Testing section, from the commits and changed files. JSON only."),
            ChatMessage::user(summary.clone()),
        ],
        max_tokens: Some(800),
        structured: Some(StructuredOutput { name: "pr".into(), schema }),
        effort: Some(ReasoningEffort::None),
        ..Default::default()
    };
    let r = h
        .client
        .chat(&h.model, &req, &CancellationToken::new())
        .await
        .map_err(|e| EngineError::from(anyhow::anyhow!(e)))?;
    let (v, _) = odex_llm::repair::parse_lenient(&r.content).map_err(bad)?;
    Ok(PrDraftResponse {
        title: v.get("title").and_then(|x| x.as_str()).unwrap_or("Update").to_string(),
        body: v.get("body").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
}

// ========================================================================== mcp

pub fn mcp_list(engine: &Engine) -> McpListResponse {
    McpListResponse { servers: engine.ext.mcp.statuses() }
}

pub async fn mcp_upsert(engine: &Engine, p: McpUpsertParams) -> EResult<McpListResponse> {
    let value = serde_json::to_value(&p.server).map_err(|e| bad(e.to_string()))?;
    config_write(
        engine,
        ConfigWriteParams {
            edits: vec![ConfigEdit { key_path: format!("mcp_servers.\"{}\"", p.name), value }],
            project_path: None,
        },
    )?;
    crate::extensions::sync_mcp(engine).await;
    Ok(mcp_list(engine))
}

pub async fn mcp_remove(engine: &Engine, p: NameParams) -> EResult<McpListResponse> {
    config_write(
        engine,
        ConfigWriteParams {
            edits: vec![ConfigEdit { key_path: format!("mcp_servers.\"{}\"", p.name), value: Value::Null }],
            project_path: None,
        },
    )?;
    crate::extensions::sync_mcp(engine).await;
    Ok(mcp_list(engine))
}

pub async fn mcp_restart(engine: &Engine, p: NameParams) -> EResult<McpListResponse> {
    let _ = engine.ext.mcp.restart(&p.name).await;
    Ok(mcp_list(engine))
}

pub fn mcp_logs(engine: &Engine, p: NameParams) -> McpLogsResponse {
    McpLogsResponse { lines: engine.ext.mcp.logs(&p.name) }
}

pub async fn mcp_login(engine: &Engine, p: NameParams) -> EResult<McpLoginResponse> {
    let url = engine.ext.mcp.login(&p.name).await.map_err(|e| bad(format!("{e:#}")))?;
    engine.emitter().raw(notification::OPEN_URL, &OpenUrlNotification { url: url.clone(), target: "external".into() });
    Ok(McpLoginResponse { authorization_url: Some(url), message: "Complete sign-in in your browser.".into() })
}

pub async fn mcp_logout(engine: &Engine, p: NameParams) -> EResult<EmptyResponse> {
    engine.ext.mcp.logout(&p.name).await.map_err(|e| bad(format!("{e:#}")))?;
    Ok(EmptyResponse {})
}

pub async fn mcp_read_resource(engine: &Engine, p: McpReadResourceParams) -> EResult<McpReadResourceResponse> {
    Ok(McpReadResourceResponse {
        contents: engine.ext.mcp.read_resource(&p.server, &p.uri).await.map_err(|e| bad(format!("{e:#}")))?,
    })
}

// ============================================================ skills & plugins

pub fn skills_list(engine: &Engine, p: SkillsListParams) -> SkillsListResponse {
    let root = p.cwd.as_ref().map(PathBuf::from);
    let s = engine.settings_for(root.as_deref());
    SkillsListResponse { skills: crate::skills::list(engine, root.as_deref(), &s) }
}

pub fn skills_read(engine: &Engine, p: NameParams) -> EResult<SkillReadResponse> {
    let s = engine.user_settings();
    let (skill, body) = crate::skills::find(engine, None, &s, &p.name).ok_or_else(|| bad("skill not found"))?;
    Ok(SkillReadResponse { skill, body })
}

pub fn skills_write(engine: &Engine, p: SkillWriteParams) -> EResult<SkillReadResponse> {
    let dir = match (p.scope, &p.project_path) {
        (SkillScope::Project, Some(root)) => odex_config::project_dir(Path::new(root)).join("skills"),
        _ => engine.home.skills_dir(),
    };
    let path = crate::skills::write_skill(&dir, &p.name, &p.description, &p.body).map_err(|e| bad(e.to_string()))?;
    Ok(SkillReadResponse {
        skill: SkillInfo {
            name: p.name,
            description: p.description,
            path: path.to_string_lossy().to_string(),
            scope: p.scope,
            enabled: true,
            plugin: None,
        },
        body: p.body,
    })
}

pub fn skills_delete(engine: &Engine, p: NameParams) -> EResult<EmptyResponse> {
    let s = engine.user_settings();
    let (skill, _) = crate::skills::find(engine, None, &s, &p.name).ok_or_else(|| bad("skill not found"))?;
    if skill.scope == SkillScope::Plugin {
        return Err(bad("plugin skills are removed with their plugin"));
    }
    if let Some(dir) = Path::new(&skill.path).parent() {
        std::fs::remove_dir_all(dir).map_err(|e| bad(e.to_string()))?;
    }
    Ok(EmptyResponse {})
}

pub async fn skills_import(engine: &Engine, p: SkillImportParams) -> EResult<SkillsListResponse> {
    let dest_root = match (p.scope, &p.project_path) {
        (SkillScope::Project, Some(root)) => odex_config::project_dir(Path::new(root)).join("skills"),
        _ => engine.home.skills_dir(),
    };
    let src = if p.source.starts_with("http") || p.source.ends_with(".git") {
        let tmp = engine.home.tmp_dir().join(format!("skill-{}", uuid::Uuid::new_v4().simple()));
        let out = tokio::process::Command::new("git")
            .args(["clone", "--depth", "1", &p.source])
            .arg(&tmp)
            .output()
            .await
            .map_err(|e| bad(e.to_string()))?;
        if !out.status.success() {
            return Err(bad(format!("git clone failed: {}", String::from_utf8_lossy(&out.stderr))));
        }
        tmp
    } else {
        PathBuf::from(&p.source)
    };
    let src = if src.is_file() { src.parent().map(|x| x.to_path_buf()).unwrap_or(src) } else { src };
    // a folder with SKILL.md, or a folder of skill folders
    let mut candidates = vec![];
    if src.join("SKILL.md").is_file() {
        candidates.push(src.clone());
    } else if let Ok(rd) = std::fs::read_dir(&src) {
        for e in rd.flatten() {
            if e.path().join("SKILL.md").is_file() {
                candidates.push(e.path());
            }
        }
    }
    if candidates.is_empty() {
        return Err(bad("no SKILL.md found at that source"));
    }
    for c in candidates {
        let text = std::fs::read_to_string(c.join("SKILL.md")).unwrap_or_default();
        let (name, _, _) = crate::skills::parse_skill_md(&text);
        let name = name.unwrap_or_else(|| {
            c.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "skill".into())
        });
        let dest = dest_root.join(slug(&name));
        copy_tree(&c, &dest).map_err(|e| bad(e.to_string()))?;
    }
    Ok(skills_list(engine, SkillsListParams { cwd: p.project_path }))
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let p = e.path();
        if p.file_name().map(|n| n == ".git").unwrap_or(false) {
            continue;
        }
        if p.is_dir() {
            copy_tree(&p, &to.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(())
}

pub fn skills_set_enabled(engine: &Engine, p: SetEnabledParams) -> EResult<SkillsListResponse> {
    let mut disabled = engine.user_settings().disabled_skills;
    disabled.retain(|n| n != &p.name);
    if !p.enabled {
        disabled.push(p.name.clone());
    }
    config_write(
        engine,
        ConfigWriteParams {
            edits: vec![ConfigEdit { key_path: "skills.disabled".into(), value: json!(disabled) }],
            project_path: None,
        },
    )?;
    Ok(skills_list(engine, SkillsListParams::default()))
}

pub fn plugins_list(engine: &Engine) -> PluginsListResponse {
    PluginsListResponse { plugins: crate::plugins::list(engine) }
}

pub async fn plugins_install(engine: &Engine, p: PluginInstallParams) -> EResult<PluginResponse> {
    Ok(PluginResponse { plugin: crate::plugins::install(engine, &p.source).await.map_err(|e| bad(format!("{e:#}")))? })
}

pub async fn plugins_trust(engine: &Engine, p: HookTrustParams) -> EResult<PluginResponse> {
    let plugin = crate::plugins::trust(engine, &p.id, &p.hash, p.trusted).map_err(|e| bad(format!("{e:#}")))?;
    crate::extensions::sync_mcp(engine).await;
    Ok(PluginResponse { plugin })
}

pub async fn plugins_remove(engine: &Engine, p: IdParams) -> EResult<EmptyResponse> {
    crate::plugins::remove(engine, &p.id).map_err(|e| bad(format!("{e:#}")))?;
    crate::extensions::sync_mcp(engine).await;
    Ok(EmptyResponse {})
}

pub async fn plugins_set_enabled(engine: &Engine, p: SetEnabledParams) -> EResult<PluginsListResponse> {
    crate::plugins::set_enabled(engine, &p.name, p.enabled).map_err(|e| bad(format!("{e:#}")))?;
    crate::extensions::sync_mcp(engine).await;
    Ok(plugins_list(engine))
}

pub fn hooks_list(engine: &Engine, p: HooksListParams) -> HooksListResponse {
    let root = p.cwd.as_ref().map(PathBuf::from);
    HooksListResponse { hooks: crate::hooks_rt::registry(engine, root.as_deref()).list() }
}

pub fn hooks_trust(engine: &Engine, p: HookTrustParams) -> EResult<HooksListResponse> {
    let mut reg = crate::hooks_rt::registry(engine, None);
    // project hooks need the project root to be found: search all known projects
    if !reg.list().iter().any(|h| h.id == p.id) {
        if let Ok(projects) = engine.store.projects() {
            for pr in projects {
                let r = crate::hooks_rt::registry(engine, Some(Path::new(pr.primary_folder())));
                if r.list().iter().any(|h| h.id == p.id) {
                    reg = r;
                    break;
                }
            }
        }
    }
    reg.set_trust(&p.id, &p.hash, p.trusted).map_err(|e| bad(e.to_string()))?;
    Ok(HooksListResponse { hooks: reg.list() })
}

// ============================================================ automations

fn astore(engine: &Engine) -> EResult<odex_automations::AutomationStore> {
    crate::background::automations(engine).map_err(EngineError::from)
}

pub fn automation_list(engine: &Engine) -> EResult<AutomationListResponse> {
    Ok(AutomationListResponse { automations: astore(engine)?.list() })
}

pub fn automation_upsert(engine: &Engine, p: AutomationUpsertParams) -> EResult<AutomationResponse> {
    odex_automations::parse_schedule(&p.automation.schedule).map_err(bad)?;
    Ok(AutomationResponse { automation: astore(engine)?.upsert(p.automation).map_err(|e| bad(e.to_string()))? })
}

pub fn automation_delete(engine: &Engine, p: IdParams) -> EResult<EmptyResponse> {
    astore(engine)?.delete(&p.id).map_err(|e| bad(e.to_string()))?;
    Ok(EmptyResponse {})
}

pub fn automation_run_now(engine: &Engine, p: IdParams) -> EResult<EmptyResponse> {
    let a = astore(engine)?.get(&p.id).ok_or_else(|| bad("automation not found"))?;
    let e2 = engine.clone();
    tokio::spawn(async move { crate::background::run_automation(&e2, a).await });
    Ok(EmptyResponse {})
}

pub fn automation_runs(engine: &Engine, p: AutomationRunsParams) -> EResult<AutomationRunsResponse> {
    let s = astore(engine)?;
    Ok(AutomationRunsResponse {
        runs: s.runs(p.automation_id.as_deref(), p.unread_only, p.include_archived, p.limit.unwrap_or(200) as usize),
        unread_count: s.unread_count(),
    })
}

pub fn automation_mark_read(engine: &Engine, p: IdsParams) -> EResult<EmptyResponse> {
    astore(engine)?.mark_read(&p.ids).map_err(|e| bad(e.to_string()))?;
    Ok(EmptyResponse {})
}

pub fn automation_archive(engine: &Engine, p: IdsParams) -> EResult<EmptyResponse> {
    astore(engine)?.archive_runs(&p.ids).map_err(|e| bad(e.to_string()))?;
    Ok(EmptyResponse {})
}

pub fn automation_validate(p: ScheduleValidateParams) -> ScheduleValidateResponse {
    odex_automations::validate(&p.schedule, chrono::Local::now())
}

// ================================================================= memories

pub fn memory_list(engine: &Engine, p: MemoryListParams) -> MemoryListResponse {
    let store = odex_memories::MemoryStore::new(engine.home.memories_dir());
    let mut memories = match &p.project_path {
        Some(pp) => store.list(Some(Path::new(pp)), p.status.as_deref()),
        None => store.all(),
    };
    if let Some(st) = &p.status {
        memories.retain(|m| &m.status == st);
    }
    MemoryListResponse { memories }
}

pub fn memory_upsert(engine: &Engine, p: MemoryUpsertParams) -> EResult<MemoryResponse> {
    let store = odex_memories::MemoryStore::new(engine.home.memories_dir());
    Ok(MemoryResponse { memory: store.upsert(p.memory).map_err(|e| bad(e.to_string()))? })
}

pub fn memory_delete(engine: &Engine, p: IdParams) -> EResult<EmptyResponse> {
    let store = odex_memories::MemoryStore::new(engine.home.memories_dir());
    store.delete(&p.id).map_err(|e| bad(e.to_string()))?;
    Ok(EmptyResponse {})
}

pub async fn memory_propose(engine: &Engine, p: ThreadIdParams) -> EResult<MemoryListResponse> {
    let rt = engine.thread(&p.thread_id)?;
    Ok(MemoryListResponse {
        memories: crate::background::propose_memories(engine, &rt).await.map_err(EngineError::from)?,
    })
}

pub fn usage_stats(engine: &Engine, p: UsageStatsParams) -> EResult<UsageStats> {
    engine.store.usage(p.since.as_deref()).map_err(EngineError::from)
}

// ============================================================= computer use

pub fn computer_status(engine: &Engine) -> ComputerUseStatus {
    let s = engine.user_settings();
    let mut st = odex_computer_use::status(s.computer_use.enabled, &s.computer_use.allowed_apps);
    st.killed = engine.kill_switch.load(std::sync::atomic::Ordering::SeqCst);
    st
}

pub async fn computer_windows(engine: &Engine) -> EResult<WindowListResponse> {
    let allowed = engine.user_settings().computer_use.allowed_apps;
    let w = tokio::task::spawn_blocking(move || odex_computer_use::list_windows(&allowed))
        .await
        .map_err(|e| bad(e.to_string()))?;
    Ok(WindowListResponse { windows: w.map_err(|e| bad(e.to_string()))? })
}

pub fn kill_switch(engine: &Engine, p: KillSwitchParams) -> ComputerUseStatus {
    engine.kill_switch.store(p.engaged, std::sync::atomic::Ordering::SeqCst);
    if p.engaged {
        odex_computer_use::kill_switch::engage();
        // stop every running turn that is using computer or browser tools
        for rt in engine.threads.lock().unwrap().values() {
            if rt.is_running() {
                turn::interrupt(rt);
            }
        }
    } else {
        odex_computer_use::kill_switch::release();
    }
    computer_status(engine)
}

pub async fn appshot(p: AppshotParams) -> EResult<AppshotResponse> {
    let handle = p.handle.as_deref().and_then(|h| h.parse::<isize>().ok());
    let shot = tokio::task::spawn_blocking(move || odex_computer_use::appshot(handle, p.include_ui_tree, 1600))
        .await
        .map_err(|e| bad(e.to_string()))?;
    Ok(AppshotResponse { appshot: shot.map_err(|e| bad(e.to_string()))? })
}

pub fn sandbox_status(engine: &Engine) -> SandboxStatus {
    let s = engine.user_settings();
    odex_sandbox::status(
        odex_sandbox::Backend::parse(&s.sandbox.windows_backend).unwrap_or_default(),
        "workspace-write",
    )
}

pub fn exec_sessions(engine: &Engine) -> ExecSessionsResponse {
    ExecSessionsResponse { sessions: engine.sessions.list(None) }
}

pub async fn exec_kill(engine: &Engine, p: IdParams) -> EResult<EmptyResponse> {
    if !engine.sessions.kill(&p.id).await {
        return Err(bad("no such session"));
    }
    Ok(EmptyResponse {})
}
