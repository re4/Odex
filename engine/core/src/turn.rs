//! The agent turn loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use odex_context::{Budget, ContextState, EntryKind, HistoryEntry, PromptInputs};
use odex_llm::loopguard::LoopVerdict;
use odex_llm::types::{ChatEvent, ChatMessage, ChatRequest, ContentPart, FinishReason, Role, ToolSpec};
use odex_llm::{LlmError, ModelHandle};
use odex_protocol::*;

use crate::engine::{EResult, Engine, EngineError};
use crate::rollout::RolloutLine;
use crate::summarizer::ModelSummarizer;
use crate::thread::{new_id, now_ms, RunningTurn, ThreadRt};
use crate::toolexec::{self, TurnCtx};

/// Per-turn overrides from `turn/start`.
#[derive(Debug, Clone, Default)]
pub struct TurnOpts {
    pub mode: TurnMode,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub permission_mode: Option<PermissionMode>,
    /// Synthetic input (goal continuation, automation) — not shown as typed by the user.
    pub synthetic: bool,
    /// Extra text for the model only (review diff, plan approval).
    pub hidden_context: Option<String>,
}

pub async fn start_turn(engine: &Engine, p: TurnStartParams) -> EResult<TurnStartResponse> {
    let rt = engine.thread(&p.thread_id)?;
    if p.input.is_empty() {
        return Err(EngineError::new(jsonrpc::error_codes::INVALID_PARAMS, "input is empty"));
    }
    if rt.is_running() {
        if p.if_busy.as_deref() == Some("steer") {
            steer(engine, &rt, p.input)?;
            return Ok(TurnStartResponse { turn: None, queued: false, steered: true });
        }
        rt.queue.lock().unwrap().push(p.input);
        engine.emitter().queue(&rt.id, rt.queue.lock().unwrap().clone());
        return Ok(TurnStartResponse { turn: None, queued: true, steered: false });
    }
    // per-turn overrides persist on the thread (like upstream)
    if p.model.is_some() || p.effort.is_some() || p.permission_mode.is_some() {
        engine.update_thread(&rt, |t| {
            if let Some(m) = &p.model {
                t.model = Some(m.clone());
            }
            if let Some(e) = p.effort {
                t.effort = Some(e);
            }
            if let Some(pm) = p.permission_mode {
                t.permission_mode = pm;
            }
        });
    }
    let opts = TurnOpts { mode: p.mode.unwrap_or_default(), ..Default::default() };
    let turn = spawn_turn(engine, rt, p.input, opts);
    Ok(TurnStartResponse { turn: Some(turn), queued: false, steered: false })
}

pub fn steer(engine: &Engine, rt: &ThreadRt, input: Vec<UserInput>) -> EResult<()> {
    let running = rt.running.lock().unwrap();
    match running.as_ref() {
        Some(r) => {
            r.steer
                .send(input)
                .map_err(|_| EngineError::new(jsonrpc::error_codes::THREAD_BUSY, "turn is finishing"))?;
            Ok(())
        }
        None => {
            drop(running);
            let _ = engine;
            Err(EngineError::new(jsonrpc::error_codes::INVALID_PARAMS, "no turn is running"))
        }
    }
}

pub fn interrupt(rt: &ThreadRt) {
    if let Some(r) = rt.running.lock().unwrap().as_ref() {
        r.cancel.cancel();
    }
    // unblock pending approvals
    let mut pa = rt.pending_approvals.lock().unwrap();
    for p in pa.values_mut() {
        if let Some(tx) = p.reply.take() {
            let _ = tx.send(ApprovalDecision::Abort);
        }
    }
}

/// Start a turn in the background and return its initial record.
pub fn spawn_turn(engine: &Engine, rt: Arc<ThreadRt>, input: Vec<UserInput>, opts: TurnOpts) -> Turn {
    let turn = Turn {
        id: new_id("turn"),
        thread_id: rt.id.clone(),
        status: TurnStatus::InProgress,
        mode: opts.mode,
        started_at: now_ms(),
        completed_at: None,
        error: None,
        usage: TokenUsage::default(),
        items: vec![],
    };
    let cancel = CancellationToken::new();
    let (tx, rx) = mpsc::unbounded_channel();
    *rt.running.lock().unwrap() =
        Some(RunningTurn { turn_id: turn.id.clone(), cancel: cancel.clone(), steer: tx, mode: opts.mode });
    rt.turns.lock().unwrap().push(turn.clone());
    rt.log(RolloutLine::TurnStarted { turn: turn.clone() });
    engine.update_thread(&rt, |t| {
        t.status = ThreadStatus::Running;
        t.last_error = None;
    });
    engine.emitter().turn_started(&rt.id, &turn);
    let e = engine.clone();
    let t2 = turn.clone();
    tokio::spawn(async move {
        let finished = run_turn(&e, rt.clone(), t2, input, opts, cancel, rx).await;
        after_turn(&e, rt, finished).await;
    });
    turn
}

/// Everything the UI and follow-up logic need about a finished turn.
pub struct Finished {
    pub turn: Turn,
    pub final_text: String,
    pub mode: TurnMode,
}

struct StreamUi {
    engine: Engine,
    thread_id: String,
    turn_id: String,
    agent: Option<(String, String)>,
    reasoning: Option<(String, String)>,
}

impl StreamUi {
    fn on(&mut self, ev: &ChatEvent) {
        let em = self.engine.emitter();
        match ev {
            ChatEvent::ContentDelta(s) if !s.is_empty() => {
                if self.agent.is_none() {
                    let id = new_id("item");
                    em.item_started(
                        &self.thread_id,
                        &self.turn_id,
                        &ThreadItem::AgentMessage { id: id.clone(), text: String::new() },
                    );
                    self.agent = Some((id, String::new()));
                }
                let (id, text) = self.agent.as_mut().unwrap();
                text.push_str(s);
                em.item_delta(&self.thread_id, &self.turn_id, id, ItemDelta::AgentMessage { text: s.clone() });
            }
            ChatEvent::ReasoningDelta(s) if !s.is_empty() => {
                if self.reasoning.is_none() {
                    let id = new_id("item");
                    em.item_started(
                        &self.thread_id,
                        &self.turn_id,
                        &ThreadItem::Reasoning { id: id.clone(), text: String::new() },
                    );
                    self.reasoning = Some((id, String::new()));
                }
                let (id, text) = self.reasoning.as_mut().unwrap();
                text.push_str(s);
                em.item_delta(&self.thread_id, &self.turn_id, id, ItemDelta::Reasoning { text: s.clone() });
            }
            ChatEvent::Retrying { attempt, reason, delay_ms } => {
                // discard partial output: complete partial items empty (the UI hides them)
                self.discard();
                let id = new_id("item");
                let item = ThreadItem::Notice {
                    id,
                    level: NoticeLevel::Warning,
                    message: format!(
                        "Reconnecting… (attempt {attempt}, retrying in {:.1}s): {reason}",
                        *delay_ms as f64 / 1000.0
                    ),
                    code: Some("reconnecting".into()),
                };
                em.item_started(&self.thread_id, &self.turn_id, &item);
                em.item_completed(&self.thread_id, &self.turn_id, &item);
                if let Some(rt) = self.engine.loaded(&self.thread_id) {
                    self.engine.set_status(&rt, ThreadStatus::Reconnecting);
                }
            }
            _ => {}
        }
    }

    fn discard(&mut self) {
        let em = self.engine.emitter();
        if let Some((id, _)) = self.agent.take() {
            em.item_completed(&self.thread_id, &self.turn_id, &ThreadItem::AgentMessage { id, text: String::new() });
        }
        if let Some((id, _)) = self.reasoning.take() {
            em.item_completed(&self.thread_id, &self.turn_id, &ThreadItem::Reasoning { id, text: String::new() });
        }
    }
}

fn emit_item(engine: &Engine, rt: &ThreadRt, turn_id: &str, item: ThreadItem) {
    let em = engine.emitter();
    em.item_started(&rt.id, turn_id, &item);
    complete_item(engine, rt, turn_id, item);
}

pub fn complete_item(engine: &Engine, rt: &ThreadRt, turn_id: &str, item: ThreadItem) {
    engine.emitter().item_completed(&rt.id, turn_id, &item);
    rt.upsert_item(turn_id, &item);
    rt.log(RolloutLine::Item { turn_id: turn_id.to_string(), item: item.clone() });
    match &item {
        ThreadItem::AgentMessage { text, .. } => engine.store.index_content(&rt.id, text),
        ThreadItem::UserMessage { content, .. } => engine.store.index_content(&rt.id, &inputs_text(content)),
        _ => {}
    }
}

pub fn notice(
    engine: &Engine,
    rt: &ThreadRt,
    turn_id: &str,
    level: NoticeLevel,
    message: impl Into<String>,
    code: Option<&str>,
) {
    emit_item(
        engine,
        rt,
        turn_id,
        ThreadItem::Notice { id: new_id("item"), level, message: message.into(), code: code.map(String::from) },
    );
}

pub fn inputs_text(inputs: &[UserInput]) -> String {
    let mut s = String::new();
    for i in inputs {
        match i {
            UserInput::Text { text } => s.push_str(text),
            UserInput::File { path, .. } | UserInput::Mention { path } | UserInput::LocalImage { path } => {
                s.push_str(&format!(" @{path}"))
            }
            UserInput::Skill { name } => s.push_str(&format!(" ${name}")),
            UserInput::ReviewComments { comments } => s.push_str(&format!(" [{} review comments]", comments.len())),
            UserInput::BrowserComment { comment, .. } => s.push_str(comment),
            _ => {}
        }
    }
    s.trim().to_string()
}

/// Build the model-facing user message from inputs.
pub async fn build_user_message(engine: &Engine, t: &Thread, inputs: &[UserInput], vision: bool) -> ChatMessage {
    let cwd = PathBuf::from(&t.cwd);
    let mut parts: Vec<ContentPart> = Vec::new();
    let mut text = String::new();
    let push_text = |text: &mut String, s: &str| {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(s);
    };
    let mut images: Vec<(String, String)> = Vec::new(); // (label, data url)
    for inp in inputs {
        match inp {
            UserInput::Text { text: t2 } => push_text(&mut text, t2),
            UserInput::Image { url, name } => {
                images.push((name.clone().unwrap_or_else(|| "pasted image".into()), url.clone()))
            }
            UserInput::LocalImage { path } => {
                let p = odex_tools::edit::resolve(&cwd, path);
                match odex_tools::load_image_data_url(&p, 1568) {
                    Ok((url, _, _)) => images.push((path.clone(), url)),
                    Err(e) => push_text(&mut text, &format!("[could not attach image {path}: {e}]")),
                }
            }
            UserInput::File { path, .. } | UserInput::Mention { path } if !path.is_empty() => {
                let p = odex_tools::edit::resolve(&cwd, path);
                if p.is_dir() {
                    let listing = odex_file_search::list_dir(&p, 2, 200).unwrap_or_default();
                    push_text(&mut text, &format!("<directory path=\"{path}\">\n{listing}\n</directory>"));
                } else if is_image(&p) {
                    if let Ok((url, _, _)) = odex_tools::load_image_data_url(&p, 1568) {
                        images.push((path.clone(), url));
                    }
                } else {
                    match std::fs::metadata(&p) {
                        Ok(m) if m.len() <= 48 * 1024 => match odex_file_search::read_file_paged(&p, 1, 2000, 2000) {
                            Ok(page) if !page.is_binary => {
                                push_text(&mut text, &format!("<file path=\"{path}\">\n{}\n</file>", page.text));
                            }
                            _ => push_text(
                                &mut text,
                                &format!("[Attached file: {path} (binary); inspect it with tools if needed]"),
                            ),
                        },
                        Ok(m) => push_text(
                            &mut text,
                            &format!("[Attached file: {path} ({} KB); read it with read_file]", m.len() / 1024),
                        ),
                        Err(_) => push_text(&mut text, &format!("[Mentioned path not found: {path}]")),
                    }
                }
            }
            UserInput::File { .. } | UserInput::Mention { .. } => {}
            UserInput::Skill { name } => {
                let root = engine.thread_root(t);
                let s = engine.thread_settings(t);
                match crate::skills::find(engine, Some(&root), &s, name) {
                    Some((info, body)) => push_text(
                        &mut text,
                        &format!("<skill name=\"{}\" path=\"{}\">\n{}\n</skill>", info.name, info.path, body.trim()),
                    ),
                    None => push_text(&mut text, &format!("[skill `{name}` not found]")),
                }
            }
            UserInput::McpResource { server, uri } => {
                push_text(&mut text, &crate::extensions::mcp_resource_text(engine, server, uri).await);
            }
            UserInput::Appshot { title, app, image_url, ui_tree } => {
                push_text(
                    &mut text,
                    &format!(
                        "[Appshot of window \"{title}\"{}]",
                        app.as_ref().map(|a| format!(" ({a})")).unwrap_or_default()
                    ),
                );
                if let Some(tree) = ui_tree {
                    push_text(&mut text, &format!("<ui_tree>\n{tree}\n</ui_tree>"));
                }
                images.push((format!("appshot {title}"), image_url.clone()));
            }
            UserInput::BrowserComment { url, selector, bounds, comment, screenshot_url } => {
                let mut c = format!("[Comment on page {url}");
                if let Some(s) = selector {
                    c.push_str(&format!(", element `{s}`"));
                }
                if let Some(b) = bounds {
                    c.push_str(&format!(", region x={:.0} y={:.0} w={:.0} h={:.0}", b.x, b.y, b.width, b.height));
                }
                c.push_str(&format!("]\n{comment}"));
                push_text(&mut text, &c);
                if let Some(s) = screenshot_url {
                    images.push(("page comment".into(), s.clone()));
                }
            }
            UserInput::ReviewComments { comments } => {
                let mut c = String::from("Review comments on the current changes (address each one):\n");
                for rc in comments {
                    let loc = match (rc.line, rc.end_line) {
                        (Some(a), Some(b)) if b != a => format!("{}:{a}-{b}", rc.path),
                        (Some(a), _) => format!("{}:{a}", rc.path),
                        _ => rc.path.clone(),
                    };
                    c.push_str(&format!("- {loc}: {}\n", rc.body));
                    if let Some(sn) = &rc.snippet {
                        c.push_str(&format!("  ```\n{}\n  ```\n", sn.trim_end()));
                    }
                }
                push_text(&mut text, &c);
            }
        }
    }
    if !images.is_empty() && !vision {
        // main model can't see images: describe them with the vision role
        for (label, url) in images.drain(..) {
            let desc = describe_image(engine, t, &url, "Describe this image in detail for a coding agent that cannot see it. Transcribe any visible text, UI labels, errors and layout.").await;
            push_text(&mut text, &format!("[Image: {label}]\n{desc}"));
        }
    }
    parts.push(ContentPart::Text { text });
    for (_, url) in images {
        parts.push(ContentPart::ImageUrl { url });
    }
    ChatMessage {
        role: Role::User,
        content: parts,
        tool_calls: vec![],
        tool_call_id: None,
        name: None,
        reasoning: None,
    }
}

fn is_image(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
    )
}

/// Ask the vision role to describe an image (used when `main` can't see).
pub async fn describe_image(engine: &Engine, t: &Thread, url: &str, prompt: &str) -> String {
    let Some(h) = engine.role_model(ModelRole::Vision, Some(t)).filter(|h| h.model.capabilities.vision) else {
        return "(no vision-capable model is configured, so the image could not be described; assign one to the `vision` role)".into();
    };
    let req = ChatRequest {
        messages: vec![ChatMessage {
            role: Role::User,
            content: vec![ContentPart::ImageUrl { url: url.into() }, ContentPart::Text { text: prompt.into() }],
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
            reasoning: None,
        }],
        max_tokens: Some(1024),
        effort: Some(ReasoningEffort::None),
        ..Default::default()
    };
    match h.client.chat(&h.model, &req, &CancellationToken::new()).await {
        Ok(r) => r.content,
        Err(e) => format!("(image description failed: {e})"),
    }
}

/// Context management before a model call. Returns false if the context
/// could not be brought under budget (caller fails the turn).
pub async fn manage_context(
    engine: &Engine,
    rt: &ThreadRt,
    turn_id: &str,
    handle: &ModelHandle,
    inputs: &PromptInputs<'_>,
    ctx: &mut ContextState,
    forced: Option<(CompactionTrigger, Option<String>)>,
) -> bool {
    let t = rt.thread();
    let s = engine.thread_settings(&t).context;
    let budget = Budget::new(handle.context_window, handle.model.max_output_tokens, &s);
    let b = budget.budget() as f64;
    let mut changed = false;

    let est = ctx.estimate(inputs) as f64;
    if forced.is_none() && est >= s.prune_at * b {
        let target = ((s.prune_at - 0.25).max(0.3) * b) as u32;
        let rep = ctx.prune(inputs, &s, target, false);
        changed |= rep.stubbed > 0 || rep.reasoning_dropped > 0;
        tracing::debug!(thread = %rt.id, "pruned {} → {}", rep.before, rep.after);
    }
    let est = ctx.estimate(inputs) as f64;
    let trigger = match &forced {
        Some((tr, _)) => Some(*tr),
        None if est >= s.compact_at * b => Some(CompactionTrigger::Auto),
        None => None,
    };
    if let Some(trigger) = trigger {
        let focus = forced.as_ref().and_then(|f| f.1.clone());
        changed |=
            run_compaction(engine, rt, turn_id, ctx, inputs, handle.context_window, trigger, focus.as_deref()).await;
    }
    if (ctx.estimate(inputs) as f64) > b {
        // Tier 3 locally (estimate says we'd overflow)
        emergency(engine, rt, turn_id, ctx, inputs, handle, budget).await;
        changed = true;
    }
    if changed {
        rt.log(RolloutLine::Checkpoint { state: ctx.clone() });
    }
    ctx.estimate(inputs) <= budget.budget()
}

#[allow(clippy::too_many_arguments)]
async fn run_compaction(
    engine: &Engine,
    rt: &ThreadRt,
    turn_id: &str,
    ctx: &mut ContextState,
    inputs: &PromptInputs<'_>,
    window: u32,
    trigger: CompactionTrigger,
    focus: Option<&str>,
) -> bool {
    let t = rt.thread();
    let s = engine.thread_settings(&t).context;
    if ctx.plan_compaction((window as f64 * s.keep_recent_ratio) as u32).is_none() {
        return false; // nothing old enough to summarize
    }
    let item_id = new_id("item");
    let n = ctx.summary.as_ref().map(|x| x.number + 1).unwrap_or(1);
    let before = ctx.estimate(inputs);
    let em = engine.emitter();
    let started = ThreadItem::ContextCompaction {
        id: item_id.clone(),
        summary_number: n,
        tokens_before: before,
        tokens_after: before,
        trigger,
        llm: true,
        status: ItemStatus::InProgress,
    };
    em.item_started(&rt.id, turn_id, &started);
    let prev_status = t.status;
    engine.set_status(rt, ThreadStatus::Compacting);
    // working-set snapshot for the pinned block
    let mut pinned = inputs.pinned.clone();
    pinned.working_set = crate::workspace::working_set(&t.cwd, &rt.files_touched()).await;
    let inputs2 = PromptInputs { pinned: &pinned, ..*inputs };
    let summarizer = engine.role_model(ModelRole::Compactor, Some(&t)).map(|h| ModelSummarizer::new(engine.clone(), h));
    let out = odex_context::compact(
        ctx,
        summarizer.as_ref().map(|s| s as &dyn odex_context::Summarizer),
        &inputs2,
        &s,
        window,
        trigger,
        focus,
        Some(turn_id.to_string()),
    )
    .await;
    engine.set_status(rt, if prev_status == ThreadStatus::Compacting { ThreadStatus::Running } else { prev_status });
    let (after, llm, ok) = match &out {
        Some(o) => (o.record.tokens_after, o.llm, true),
        None => (before, false, false),
    };
    let item = ThreadItem::ContextCompaction {
        id: item_id,
        summary_number: n,
        tokens_before: before,
        tokens_after: after,
        trigger,
        llm,
        status: if ok { ItemStatus::Completed } else { ItemStatus::Failed },
    };
    complete_item(engine, rt, turn_id, item);
    ok
}

async fn emergency(
    engine: &Engine,
    rt: &ThreadRt,
    turn_id: &str,
    ctx: &mut ContextState,
    inputs: &PromptInputs<'_>,
    handle: &ModelHandle,
    budget: Budget,
) {
    let t = rt.thread();
    let s = engine.thread_settings(&t).context;
    let target = (budget.budget() as f64 * 0.6) as u32;
    // 1. prune aggressively
    ctx.prune(inputs, &s, target, true);
    // 2. compact
    if ctx.estimate(inputs) > target {
        run_compaction(engine, rt, turn_id, ctx, inputs, handle.context_window, CompactionTrigger::Emergency, None)
            .await;
    }
    // 3. hard-trim oldest non-pinned items
    if ctx.estimate(inputs) > target {
        ctx.emergency_trim(inputs, target);
    }
    rt.log(RolloutLine::Checkpoint { state: ctx.clone() });
}

async fn run_turn(
    engine: &Engine,
    rt: Arc<ThreadRt>,
    mut turn: Turn,
    input: Vec<UserInput>,
    opts: TurnOpts,
    cancel: CancellationToken,
    mut steer_rx: mpsc::UnboundedReceiver<Vec<UserInput>>,
) -> Finished {
    let mode = opts.mode;
    let turn_index = rt.next_turn_index();
    let mut final_text = String::new();
    let t0 = rt.thread();

    // Undo snapshot before anything changes.
    let settings = engine.thread_settings(&t0);
    if settings.undo_snapshots {
        crate::workspace::snapshot_turn(&t0.cwd, &rt.id, &turn.id).await;
    }

    // User message
    let main = engine.main_model(&t0);
    let vision = main.as_ref().map(|h| h.model.capabilities.vision).unwrap_or(false);
    let mut user_msg = build_user_message(engine, &t0, &input, vision).await;
    if let Some(h) = &opts.hidden_context {
        user_msg.content.insert(0, ContentPart::Text { text: format!("{h}\n\n") });
    }
    // UserPromptSubmit hooks may block or add context
    let hook = crate::hooks_rt::run(
        engine,
        &rt,
        HookEvent::UserPromptSubmit,
        serde_json::json!({"prompt": inputs_text(&input)}),
        None,
    )
    .await;
    if hook.blocked {
        let user_item = ThreadItem::UserMessage { id: new_id("item"), content: input.clone(), steer: false };
        emit_item(engine, &rt, &turn.id, user_item);
        notice(
            engine,
            &rt,
            &turn.id,
            NoticeLevel::Warning,
            format!("A hook blocked this prompt: {}", hook.reason.clone().unwrap_or_default()),
            Some("hookBlocked"),
        );
        turn.status = TurnStatus::Completed;
        turn.completed_at = Some(now_ms());
        return Finished { turn, final_text: String::new(), mode };
    }
    for c in &hook.additional_context {
        user_msg.content.push(ContentPart::Text { text: format!("\n\n[hook context]\n{c}") });
    }
    let user_item = ThreadItem::UserMessage { id: new_id("item"), content: input.clone(), steer: false };
    if !opts.synthetic || mode == TurnMode::Review {
        emit_item(engine, &rt, &turn.id, user_item);
    } else {
        notice(engine, &rt, &turn.id, NoticeLevel::Info, inputs_text(&input), Some("synthetic"));
    }
    let text = inputs_text(&input);
    {
        let mut ot = rt.original_task.lock().unwrap();
        if ot.is_none() && !opts.synthetic {
            *ot = Some(text.clone());
            rt.log(RolloutLine::Pinned { original_task: ot.clone() });
        }
    }
    if t0.preview.is_empty() && !text.is_empty() {
        let preview: String = text.chars().take(160).collect();
        engine.update_thread(&rt, |t| t.preview = preview);
    }
    if t0.name.is_none() && settings.auto_title && !opts.synthetic && t0.kind != ThreadKind::Subagent {
        crate::followups::spawn_title(engine.clone(), rt.clone(), text.clone());
    }
    {
        let mut ctx = rt.ctx.lock().await;
        let entry = HistoryEntry::new(&turn.id, turn_index, EntryKind::User, user_msg);
        index_entry(engine, &rt, &entry);
        rt.log(RolloutLine::History { entry: entry.clone() });
        let est = main.as_ref().map(|h| engine.registry.estimator(&h.model.key)).unwrap_or_default();
        ctx.push(entry, &est, settings.context.max_images);
    }
    rt.arg_failures.lock().unwrap().clear();
    rt.loop_guard.lock().unwrap().reset();

    let mut overflow_retries = 0u32;
    let mut max_tokens_cap: Option<u32> = None;
    let mut empty_responses = 0u32;
    let mut status = TurnStatus::Completed;
    let mut error: Option<TurnError> = None;
    let mut steps = 0u32;

    'outer: loop {
        if cancel.is_cancelled() {
            status = TurnStatus::Interrupted;
            break;
        }
        // steering messages
        while let Ok(inp) = steer_rx.try_recv() {
            let t = rt.thread();
            let vision = engine.main_model(&t).map(|h| h.model.capabilities.vision).unwrap_or(false);
            let msg = build_user_message(engine, &t, &inp, vision).await;
            emit_item(engine, &rt, &turn.id, ThreadItem::UserMessage { id: new_id("item"), content: inp, steer: true });
            let mut ctx = rt.ctx.lock().await;
            let entry = HistoryEntry::new(&turn.id, turn_index, EntryKind::Steer, msg);
            index_entry(engine, &rt, &entry);
            rt.log(RolloutLine::History { entry: entry.clone() });
            ctx.push(entry, &Default::default(), settings.context.max_images);
        }
        let t = rt.thread();
        let s = engine.thread_settings(&t);
        let handle = match engine.main_model(&t) {
            Ok(h) => h,
            Err(e) => {
                status = TurnStatus::Failed;
                error = Some(TurnError { message: e.message.clone(), code: Some("modelUnavailable".into()) });
                emit_item(engine, &rt, &turn.id, ThreadItem::Error { id: new_id("item"), message: e.message });
                break;
            }
        };
        let tools: Vec<ToolSpec> =
            toolexec::tool_specs(engine, &rt, &t, &s, Some(&handle), handle.model.tool_profile, mode);
        let system = engine.system_parts(&t, &s, mode);
        let pinned = engine.pinned(&rt, &s);
        let est = engine.registry.estimator(&handle.model.key);
        let inputs = PromptInputs {
            system: &system,
            tools: &tools,
            pinned: &pinned,
            estimator: &est,
            reasoning_history: handle.model.reasoning_history,
            current_turn: turn_index,
        };
        // context management before every model call
        let (messages, prompt_est, raw_est) = {
            let mut ctx = rt.ctx.lock().await;
            if !manage_context(engine, &rt, &turn.id, &handle, &inputs, &mut ctx, None).await {
                tracing::warn!(thread = %rt.id, "context still over budget after all tiers");
            }
            let msgs = ctx.build_messages(&inputs);
            (msgs, ctx.estimate(&inputs), ctx.raw_estimate(&inputs))
        };
        engine.emit_context(&rt).await;
        let budget = Budget::new(handle.context_window, handle.model.max_output_tokens, &s.context);
        let mut max_tokens = budget.max_tokens_for(prompt_est, handle.model.max_output_tokens);
        if let Some(cap) = max_tokens_cap {
            max_tokens = max_tokens.min(cap);
        }
        let max_tokens = max_tokens.max(64);
        let effort = t.effort;
        let req = ChatRequest {
            messages,
            tools: tools.clone(),
            max_tokens: Some(max_tokens),
            effort,
            include_reasoning: handle.model.reasoning_history != ReasoningHistory::Drop,
            ..Default::default()
        };
        let chars = odex_llm::tokens::char_count(&req.messages, &req.tools);
        let mut ui = StreamUi {
            engine: engine.clone(),
            thread_id: rt.id.clone(),
            turn_id: turn.id.clone(),
            agent: None,
            reasoning: None,
        };
        let res = {
            let mut sink = |ev: ChatEvent| ui.on(&ev);
            handle.client.stream_chat(&handle.model, &req, &cancel, &mut sink).await
        };
        if rt.thread().status == ThreadStatus::Reconnecting {
            engine.set_status(&rt, ThreadStatus::Running);
        }
        let resp = match res {
            Ok(r) => r,
            Err(LlmError::Cancelled) => {
                // keep what was streamed
                if let Some((id, text)) = ui.agent.take() {
                    complete_item(engine, &rt, &turn.id, ThreadItem::AgentMessage { id, text });
                }
                if let Some((id, text)) = ui.reasoning.take() {
                    complete_item(engine, &rt, &turn.id, ThreadItem::Reasoning { id, text });
                }
                status = TurnStatus::Interrupted;
                break;
            }
            Err(LlmError::ContextOverflow(o)) => {
                ui.discard();
                overflow_retries += 1;
                if overflow_retries > 4 {
                    status = TurnStatus::Failed;
                    error = Some(TurnError {
                        message: format!("Context overflow persisted after recovery: {}", o.message),
                        code: Some("contextOverflow".into()),
                    });
                    emit_item(
                        engine,
                        &rt,
                        &turn.id,
                        ThreadItem::Error { id: new_id("item"), message: o.message.clone() },
                    );
                    break;
                }
                if o.max_tokens_only {
                    // the prompt fits; just ask for fewer output tokens
                    let room = match (o.max_context, o.prompt_tokens) {
                        (Some(m), Some(p)) => m.saturating_sub(p + budget.margin).max(64),
                        _ => max_tokens / 2,
                    };
                    max_tokens_cap = Some(room);
                } else {
                    // estimation was wrong: calibrate and run the emergency path
                    let mut ctx = rt.ctx.lock().await;
                    if let Some(p) = o.prompt_tokens {
                        ctx.observe_usage(p, raw_est);
                        ctx.last_exact = None;
                        engine.registry.calibrate(&handle.model.key, chars, p);
                    } else {
                        ctx.correction = (ctx.correction * 1.25).min(3.0);
                    }
                    notice(
                        engine,
                        &rt,
                        &turn.id,
                        NoticeLevel::Warning,
                        "The request exceeded the model's context window; recovering automatically.",
                        Some("contextOverflow"),
                    );
                    emergency(engine, &rt, &turn.id, &mut ctx, &inputs, &handle, budget).await;
                }
                continue;
            }
            Err(e) => {
                ui.discard();
                status = TurnStatus::Failed;
                let msg = e.to_string();
                error = Some(TurnError { message: msg.clone(), code: Some(e.code().into()) });
                emit_item(engine, &rt, &turn.id, ThreadItem::Error { id: new_id("item"), message: msg });
                break;
            }
        };
        overflow_retries = 0;
        steps += 1;

        // usage accounting + calibration
        if let Some(u) = resp.usage {
            turn.usage.add(&u);
            let total = engine.update_thread(&rt, |t| t.usage.add(&u)).usage;
            engine.store.record_usage(&handle.model.key, &u);
            engine.emitter().token_usage(&rt.id, &turn.id, u, total);
            if u.input_tokens > 0 {
                engine.registry.calibrate(&handle.model.key, chars, u.input_tokens as u32);
                let mut ctx = rt.ctx.lock().await;
                ctx.observe_usage(u.input_tokens as u32, raw_est);
            }
        }

        // finalize streamed items with the authoritative text
        let reasoning_text = resp.reasoning.clone();
        if let Some((id, _)) = ui.reasoning.take() {
            complete_item(engine, &rt, &turn.id, ThreadItem::Reasoning { id, text: reasoning_text.clone() });
        } else if !reasoning_text.is_empty() {
            emit_item(
                engine,
                &rt,
                &turn.id,
                ThreadItem::Reasoning { id: new_id("item"), text: reasoning_text.clone() },
            );
        }
        let content = resp.content.trim().to_string();
        if let Some((id, _)) = ui.agent.take() {
            complete_item(engine, &rt, &turn.id, ThreadItem::AgentMessage { id, text: content.clone() });
        } else if !content.is_empty() {
            emit_item(engine, &rt, &turn.id, ThreadItem::AgentMessage { id: new_id("item"), text: content.clone() });
        }
        if resp.fallback_parsed && steps == 1 {
            tracing::info!("tool calls recovered by the fallback parser for {}", handle.model.key);
        }

        // record assistant step
        let mut amsg = ChatMessage::assistant_tool_calls(Some(content.clone()), resp.tool_calls.clone());
        if !reasoning_text.is_empty() {
            amsg.reasoning = Some(reasoning_text);
        }
        {
            let mut ctx = rt.ctx.lock().await;
            let entry = HistoryEntry::new(&turn.id, turn_index, EntryKind::Assistant, amsg);
            index_entry(engine, &rt, &entry);
            rt.log(RolloutLine::History { entry: entry.clone() });
            ctx.push(entry, &est, s.context.max_images);
        }

        if resp.tool_calls.is_empty() {
            if content.is_empty() && resp.reasoning.is_empty() {
                empty_responses += 1;
                if empty_responses <= 2 {
                    push_nudge(
                        engine,
                        &rt,
                        &turn.id,
                        turn_index,
                        "Your last response was empty. Continue the task, or summarize the result if you are done.",
                    )
                    .await;
                    continue;
                }
            }
            if resp.finish_reason == Some(FinishReason::Length) && content.len() < 20 {
                push_nudge(engine, &rt, &turn.id, turn_index, "Your output hit the token limit. Continue concisely.")
                    .await;
                continue;
            }
            let verdict = rt.loop_guard.lock().unwrap().observe_text(&content);
            match verdict {
                LoopVerdict::Nudge(n) => {
                    push_nudge(engine, &rt, &turn.id, turn_index, &n).await;
                    continue;
                }
                LoopVerdict::Stop(m) => {
                    notice(engine, &rt, &turn.id, NoticeLevel::Warning, m, Some("loopBreaker"));
                }
                LoopVerdict::Ok => {}
            }
            final_text = content;
            break;
        }

        // tool calls
        let call_verdict = rt.loop_guard.lock().unwrap().observe_calls(&resp.tool_calls);
        match call_verdict {
            LoopVerdict::Ok => {}
            LoopVerdict::Nudge(n) => {
                notice(
                    engine,
                    &rt,
                    &turn.id,
                    NoticeLevel::Warning,
                    "Repeated identical tool calls detected; nudging the model.",
                    Some("loopBreaker"),
                );
                // still answer the calls (results must exist), then nudge
                for c in &resp.tool_calls {
                    record_tool_result(engine, &rt, &turn.id, turn_index, c, &format!("Not executed: {n}"), None, &s)
                        .await;
                }
                push_nudge(engine, &rt, &turn.id, turn_index, &n).await;
                continue;
            }
            LoopVerdict::Stop(m) => {
                for c in &resp.tool_calls {
                    record_tool_result(
                        engine,
                        &rt,
                        &turn.id,
                        turn_index,
                        c,
                        "Not executed: loop breaker stopped the turn.",
                        None,
                        &s,
                    )
                    .await;
                }
                notice(engine, &rt, &turn.id, NoticeLevel::Error, m.clone(), Some("loopBreaker"));
                status = TurnStatus::Failed;
                error = Some(TurnError { message: m, code: Some("loop".into()) });
                break;
            }
        }
        let tctx = TurnCtx {
            turn_id: turn.id.clone(),
            turn_index,
            cancel: cancel.clone(),
            model: handle.clone(),
            mode,
            settings: Arc::new(s.clone()),
            tools: tools.clone(),
        };
        for call in &resp.tool_calls {
            if cancel.is_cancelled() {
                record_tool_result(
                    engine,
                    &rt,
                    &turn.id,
                    turn_index,
                    call,
                    "Not executed: the user interrupted the turn.",
                    None,
                    &s,
                )
                .await;
                continue;
            }
            let outcome = toolexec::run_tool(engine, &rt, &tctx, call).await;
            let abort = outcome.abort_turn;
            let nudge = outcome.nudge.clone();
            {
                let mut ctx = rt.ctx.lock().await;
                let tool_msg = ChatMessage::tool_result(call.id.clone(), call.name.clone(), outcome.text.clone());
                let entry = HistoryEntry::new(&turn.id, turn_index, EntryKind::ToolResult, tool_msg)
                    .with_tool(outcome.meta.clone());
                index_entry(engine, &rt, &entry);
                rt.log(RolloutLine::History { entry: entry.clone() });
                ctx.push(entry, &est, s.context.max_images);
                if !outcome.images.is_empty() {
                    let mut m =
                        ChatMessage::user(format!("[image result of {}({})]", call.name, outcome.meta.args_summary));
                    for url in &outcome.images {
                        m.content.push(ContentPart::ImageUrl { url: url.clone() });
                    }
                    let img_entry = HistoryEntry::new(&turn.id, turn_index, EntryKind::Image, m);
                    rt.log(RolloutLine::History { entry: img_entry.clone() });
                    ctx.push(img_entry, &est, s.context.max_images);
                }
            }
            if let Some(n) = nudge {
                push_nudge(engine, &rt, &turn.id, turn_index, &n).await;
            }
            if abort {
                status = TurnStatus::Interrupted;
                // answer remaining calls so history stays well-formed
                let idx = resp.tool_calls.iter().position(|c| c.id == call.id).unwrap_or(0);
                for c in &resp.tool_calls[idx + 1..] {
                    record_tool_result(
                        engine,
                        &rt,
                        &turn.id,
                        turn_index,
                        c,
                        "Not executed: the user stopped the turn.",
                        None,
                        &s,
                    )
                    .await;
                }
                break 'outer;
            }
        }
        if cancel.is_cancelled() {
            status = TurnStatus::Interrupted;
            break;
        }
    }

    // plan / review results
    if status == TurnStatus::Completed {
        match mode {
            TurnMode::Plan if !final_text.is_empty() => {
                emit_item(
                    engine,
                    &rt,
                    &turn.id,
                    ThreadItem::ProposedPlan { id: new_id("item"), markdown: final_text.clone(), approved: false },
                );
            }
            TurnMode::Review => {
                crate::workspace::emit_review(engine, &rt, &turn.id, &final_text).await;
            }
            _ => {}
        }
    }

    turn.status = status;
    turn.error = error;
    turn.completed_at = Some(now_ms());
    Finished { turn, final_text, mode }
}

pub async fn push_nudge(engine: &Engine, rt: &ThreadRt, turn_id: &str, turn_index: u32, text: &str) {
    let _ = engine;
    let mut ctx = rt.ctx.lock().await;
    let entry =
        HistoryEntry::new(turn_id, turn_index, EntryKind::Nudge, ChatMessage::user(format!("[system note] {text}")));
    rt.log(RolloutLine::History { entry: entry.clone() });
    ctx.push(entry, &Default::default(), 2);
}

#[allow(clippy::too_many_arguments)]
async fn record_tool_result(
    engine: &Engine,
    rt: &ThreadRt,
    turn_id: &str,
    turn_index: u32,
    call: &odex_llm::types::ToolCall,
    text: &str,
    meta: Option<odex_context::ToolMeta>,
    s: &odex_config::Settings,
) {
    let _ = engine;
    let meta = meta.unwrap_or_else(|| odex_context::ToolMeta {
        tool: call.name.clone(),
        call_id: call.id.clone(),
        args_summary: String::new(),
        success: false,
        ..Default::default()
    });
    let mut ctx = rt.ctx.lock().await;
    let entry = HistoryEntry::new(
        turn_id,
        turn_index,
        EntryKind::ToolResult,
        ChatMessage::tool_result(call.id.clone(), call.name.clone(), text),
    )
    .with_tool(meta);
    rt.log(RolloutLine::History { entry: entry.clone() });
    ctx.push(entry, &Default::default(), s.context.max_images);
}

/// Index an entry for `recall` (full text, including tool outputs).
fn index_entry(engine: &Engine, rt: &ThreadRt, e: &HistoryEntry) {
    let label = match e.kind {
        EntryKind::User | EntryKind::Steer => "user".to_string(),
        EntryKind::Assistant => "assistant".to_string(),
        EntryKind::ToolResult => {
            format!("tool {}", e.tool.as_ref().map(|t| format!("{} {}", t.tool, t.args_summary)).unwrap_or_default())
        }
        EntryKind::Nudge => "note".to_string(),
        EntryKind::Image => "image".to_string(),
    };
    let mut text = e.text();
    for c in &e.msg.tool_calls {
        text.push_str(&format!("\n{}({})", c.name, c.arguments));
    }
    engine.store.index_recall(&rt.id, &e.id, e.turn_index, &label, &text);
}

async fn after_turn(engine: &Engine, rt: Arc<ThreadRt>, f: Finished) {
    let turn = f.turn.clone();
    *rt.running.lock().unwrap() = None;
    // reject approvals still pending
    rt.pending_approvals.lock().unwrap().clear();
    {
        let mut turns = rt.turns.lock().unwrap();
        if let Some(t) = turns.iter_mut().find(|t| t.id == turn.id) {
            let items = std::mem::take(&mut t.items);
            *t = turn.clone();
            t.items = items;
        }
    }
    rt.log(RolloutLine::TurnCompleted { turn: turn.clone() });
    let last_error = turn.error.as_ref().map(|e| e.message.clone());
    let failed = turn.status == TurnStatus::Failed;
    let thread = engine.update_thread(&rt, |t| {
        t.status = if failed { ThreadStatus::Error } else { ThreadStatus::Idle };
        t.unread = true;
        t.last_error = last_error.clone();
    });
    engine.emitter().turn_completed(&rt.id, &turn);
    engine.emit_context(&rt).await;

    // diff stats for the review pane
    if let Some(stats) = crate::workspace::diff_stats(&thread.cwd).await {
        engine.update_thread(&rt, |t| t.diff_stats = Some(stats));
        engine.emitter().diff_updated(&rt.id, Some(&turn.id), stats);
    }
    engine.subagents.finished(&rt.id, &f.final_text, turn.status);

    // goal continuation
    let mut continued = false;
    if turn.status == TurnStatus::Completed {
        continued = crate::followups::goal_after_turn(engine, &rt, &f).await;
    }
    // queued follow-ups first
    let next = {
        let mut q = rt.queue.lock().unwrap();
        if q.is_empty() {
            None
        } else {
            Some(q.remove(0))
        }
    };
    if let Some(input) = next {
        engine.emitter().queue(&rt.id, rt.queue.lock().unwrap().clone());
        spawn_turn(engine, rt.clone(), input, TurnOpts::default());
        return;
    }
    if continued {
        return;
    }
    // Stop hooks can ask the agent to keep going
    if turn.status == TurnStatus::Completed && thread.kind != ThreadKind::Subagent {
        let hook = crate::hooks_rt::run(
            engine,
            &rt,
            HookEvent::Stop,
            serde_json::json!({"final_message": f.final_text}),
            None,
        )
        .await;
        let streak = rt.stop_hook_streak.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if hook.blocked && streak < 3 {
            let reason = hook.reason.unwrap_or_else(|| "A stop hook asked you to continue.".into());
            spawn_turn(
                engine,
                rt.clone(),
                vec![UserInput::text(format!("[stop hook] {reason}"))],
                TurnOpts { synthetic: true, ..Default::default() },
            );
            return;
        }
        rt.stop_hook_streak.store(0, std::sync::atomic::Ordering::SeqCst);
    }
    if turn.status == TurnStatus::Completed && f.mode == TurnMode::Default && thread.kind != ThreadKind::Subagent {
        let s = engine.thread_settings(&thread);
        if s.follow_up_suggestions {
            crate::followups::spawn_followups(engine.clone(), rt.clone(), f.final_text.clone());
        }
        crate::background::schedule_memory_proposal(engine.clone(), rt.clone());
    }
}
