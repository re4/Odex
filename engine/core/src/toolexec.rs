//! Tool dispatch: argument hygiene, approvals, sandboxed execution, items.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use odex_config::Settings;
use odex_context::ToolMeta;
use odex_execpolicy::ShellKind;
use odex_llm::types::{ToolCall, ToolSpec};
use odex_llm::ModelHandle;
use odex_protocol::*;
use odex_tools::{output, specs};

use crate::approval::{self, ExecCtx, ExecPlan};
use crate::engine::Engine;
use crate::thread::ThreadRt;
use crate::turn::complete_item;

pub struct TurnCtx {
    pub turn_id: String,
    pub turn_index: u32,
    pub cancel: CancellationToken,
    pub model: ModelHandle,
    pub mode: TurnMode,
    pub settings: Arc<Settings>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolOutcome {
    pub text: String,
    pub images: Vec<String>,
    pub meta: ToolMeta,
    pub abort_turn: bool,
    pub nudge: Option<String>,
}

const READ_ONLY_TOOLS: &[&str] = &[
    "shell",
    "read_file",
    "list_dir",
    "grep",
    "glob",
    "read_output",
    "recall",
    "view_image",
    "update_plan",
    "read_terminal",
    "spawn_agent",
    "wait_agents",
];

/// The tool list for a thread/model/mode, in a stable order.
pub fn tool_specs(
    engine: &Engine,
    rt: &ThreadRt,
    t: &Thread,
    s: &Settings,
    handle: Option<&ModelHandle>,
    profile: ToolProfile,
    mode: TurnMode,
) -> Vec<ToolSpec> {
    let window = handle.map(|h| h.context_window).unwrap_or(32768);
    let small = window <= specs::SMALL_WINDOW;
    let mut tools = if small { specs::compact(cfg!(windows)) } else { specs::builtin(profile, cfg!(windows)) };
    if !small && engine.emitter().sink().capabilities().approvals && t.kind != ThreadKind::Subagent {
        tools.push(specs::read_terminal());
    }
    if !small && t.kind != ThreadKind::Subagent && profile != ToolProfile::Minimal {
        tools.push(specs::spawn_agent());
        tools.push(specs::wait_agents());
    }
    tools.extend(crate::extensions::extra_tools(engine, rt, t, s, handle));
    if matches!(mode, TurnMode::Plan | TurnMode::Review) {
        tools.retain(|tool| {
            READ_ONLY_TOOLS.contains(&tool.name.as_str()) || crate::extensions::is_read_only_extra(&tool.name)
        });
        if mode == TurnMode::Review {
            tools.retain(|t| t.name != "spawn_agent" && t.name != "update_plan");
        }
    }
    tools
}

fn hash_of<T: Hash>(t: &T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

struct Out<'a> {
    engine: &'a Engine,
    rt: &'a ThreadRt,
    tctx: &'a TurnCtx,
    call: &'a ToolCall,
    args: Value,
    summary: String,
}

impl Out<'_> {
    fn meta(&self, success: bool) -> ToolMeta {
        ToolMeta {
            tool: self.call.name.clone(),
            call_id: self.call.id.clone(),
            args_summary: self.summary.clone(),
            success,
            call_fingerprint: hash_of(&(&self.call.name, self.args.to_string())),
            ..Default::default()
        }
    }

    fn cwd(&self) -> PathBuf {
        PathBuf::from(self.rt.thread().cwd)
    }

    /// Cap output for the model (Tier 0), storing the full text when large.
    fn finalize(&self, full: &str, meta: &mut ToolMeta) -> String {
        let window = self.tctx.model.context_window;
        let cap_tokens = output::cap_tokens_for_window(window, self.tctx.settings.context.tool_output_max_tokens);
        let ratio = self.engine.registry.estimator(&self.tctx.model.model.key).ratio();
        let max_chars = (cap_tokens as f64 * ratio) as usize;
        meta.full_chars = full.chars().count();
        meta.lines = full.lines().count();
        meta.fingerprint = hash_of(&(&self.call.name, self.args.to_string(), full));
        let r = if full.len() > 1024 { self.rt.outputs.save(full).ok() } else { None };
        meta.output_ref = r.clone();
        output::cap(full, max_chars, r.as_deref()).text
    }

    fn generic_item(&self, status: ItemStatus, output: Option<String>) -> ThreadItem {
        ThreadItem::ToolCall {
            id: format!("item_{}", self.call.id),
            tool: self.call.name.clone(),
            arguments: self.args.clone(),
            status,
            output: output.map(|o| clip_display(&o)),
            summary: Some(self.summary.clone()),
        }
    }

    fn start_generic(&self) {
        let item = self.generic_item(ItemStatus::InProgress, None);
        self.engine.emitter().item_started(&self.rt.id, &self.tctx.turn_id, &item);
    }

    fn done_generic(&self, ok: bool, out: &str) {
        let item =
            self.generic_item(if ok { ItemStatus::Completed } else { ItemStatus::Failed }, Some(out.to_string()));
        complete_item(self.engine, self.rt, &self.tctx.turn_id, item);
    }

    fn err(&self, msg: impl Into<String>) -> ToolOutcome {
        let msg = msg.into();
        ToolOutcome { text: format!("Error: {msg}"), meta: self.meta(false), ..Default::default() }
    }
}

fn clip_display(s: &str) -> String {
    const MAX: usize = 64 * 1024;
    if s.len() <= MAX {
        return s.to_string();
    }
    let start = (s.len() - MAX..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(0);
    format!("[… {} earlier bytes not shown …]\n{}", start, &s[start..])
}

pub async fn run_tool(engine: &Engine, rt: &ThreadRt, tctx: &TurnCtx, call: &ToolCall) -> ToolOutcome {
    let Some(spec) = tctx.tools.iter().find(|t| t.name == call.name) else {
        let names: Vec<&str> = tctx.tools.iter().map(|t| t.name.as_str()).collect();
        return ToolOutcome {
            text: format!("Error: unknown tool `{}`. Available tools: {}.", call.name, names.join(", ")),
            meta: ToolMeta { tool: call.name.clone(), call_id: call.id.clone(), ..Default::default() },
            ..Default::default()
        };
    };
    let args = match odex_tools::parse_args(spec, &call.arguments) {
        Ok((v, _)) => v,
        Err(msg) => {
            let n = {
                let mut f = rt.arg_failures.lock().unwrap();
                let c = f.entry(call.name.clone()).or_insert(0);
                *c += 1;
                *c
            };
            if n == 3 {
                crate::turn::notice(
                    engine,
                    rt,
                    &tctx.turn_id,
                    NoticeLevel::Warning,
                    format!("The model keeps sending invalid arguments to `{}`: {msg}", call.name),
                    Some("invalidArguments"),
                );
            }
            let hint = if n >= 3 {
                " This has failed several times; try a different tool or approach."
            } else {
                " Fix the arguments and call the tool again."
            };
            return ToolOutcome {
                text: format!("Error: {msg}.{hint}"),
                meta: ToolMeta {
                    tool: call.name.clone(),
                    call_id: call.id.clone(),
                    success: false,
                    ..Default::default()
                },
                ..Default::default()
            };
        }
    };
    // PreToolUse hooks may block or rewrite the input
    let mut args = args;
    let pre = crate::hooks_rt::run(
        engine,
        rt,
        HookEvent::PreToolUse,
        json!({"tool_name": call.name, "tool_input": args}),
        Some(&call.name),
    )
    .await;
    if pre.blocked {
        return ToolOutcome {
            text: format!("Blocked by a hook: {}", pre.reason.unwrap_or_else(|| "no reason given".into())),
            meta: ToolMeta { tool: call.name.clone(), call_id: call.id.clone(), success: false, ..Default::default() },
            ..Default::default()
        };
    }
    if let Some(m) = pre.modified_input.filter(|m| m.is_object()) {
        args = m;
    }
    let summary = odex_tools::args_summary(&call.name, &args);
    let o = Out { engine, rt, tctx, call, args, summary };
    let mut out = dispatch(&o).await;
    let post = crate::hooks_rt::run(
        engine,
        rt,
        HookEvent::PostToolUse,
        json!({"tool_name": call.name, "tool_input": o.args, "tool_response": out.text}),
        Some(&call.name),
    )
    .await;
    for c in post.additional_context {
        out.text.push_str(&format!("\n[hook] {c}"));
    }
    if post.blocked {
        out.text.push_str(&format!("\n[hook] {}", post.reason.unwrap_or_default()));
    }
    out
}

async fn dispatch(o: &Out<'_>) -> ToolOutcome {
    let (engine, rt, tctx, call) = (o.engine, o.rt, o.tctx, o.call);
    match call.name.as_str() {
        "shell" => shell(o).await,
        "exec_command" => exec_command(o).await,
        "write_stdin" => write_stdin(o).await,
        "apply_patch" => {
            let text = o.args["patch"].as_str().unwrap_or("").to_string();
            apply_patch(o, &text, None).await
        }
        "edit_file" | "write_file" => edit_or_write(o).await,
        "read_file" => read_file(o).await,
        "list_dir" => list_dir(o).await,
        "grep" => grep(o).await,
        "glob" => glob(o).await,
        "view_image" => view_image(o).await,
        "update_plan" => update_plan(o),
        "read_output" => read_output(o),
        "recall" => recall(o),
        "read_terminal" => read_terminal(o).await,
        "spawn_agent" => crate::subagents::spawn_tool(engine, rt, tctx, call, &o.args).await,
        "wait_agents" => crate::subagents::wait_tool(engine, rt, tctx, call, &o.args).await,
        other => match crate::extensions::run_extra_tool(engine, rt, tctx, call, &o.args, &o.summary).await {
            Some(out) => out,
            None => o.err(format!("tool `{other}` is not available")),
        },
    }
}

// ------------------------------------------------------------------ shell

fn shell_kind(s: &Settings) -> ShellKind {
    ShellKind::from_name(&s.default_shell)
}

fn sandbox_request(
    engine: &Engine,
    rt: &ThreadRt,
    s: &Settings,
    sandboxed: bool,
    readonly: bool,
) -> odex_sandbox::ExecRequest {
    let t = rt.thread();
    let policy = if !sandboxed {
        odex_sandbox::SandboxPolicy::FullAccess
    } else if readonly {
        odex_sandbox::SandboxPolicy::ReadOnly
    } else {
        odex_sandbox::SandboxPolicy::WorkspaceWrite {
            writable_roots: engine.writable_roots(&t, s),
            network: s.sandbox.network_access,
        }
    };
    // the thread's environment variables (`.odex/environments.toml`), then Odex's own
    let mut env = crate::worktrees::thread_env_vars(engine, &t);
    env.insert("ODEX".into(), "1".into());
    env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    env.insert("PAGER".into(), "cat".into());
    env.insert("GIT_PAGER".into(), "cat".into());
    env.insert("NO_COLOR".into(), "1".into());
    odex_sandbox::ExecRequest {
        argv: vec![],
        cwd: PathBuf::from(&t.cwd),
        env,
        timeout: None,
        policy,
        stdin: None,
        max_output_bytes: 8 * 1024 * 1024,
        backend: odex_sandbox::Backend::parse(&s.sandbox.windows_backend).unwrap_or_default(),
        sandbox_temp: Some(engine.sandbox_temp()),
    }
}

fn sandbox_available(engine: &Engine, s: &Settings) -> bool {
    let _ = engine;
    odex_sandbox::status(
        odex_sandbox::Backend::parse(&s.sandbox.windows_backend).unwrap_or_default(),
        "workspace-write",
    )
    .available
}

/// Is every command allowed by an `allow` rule of the user's exec policy
/// (`~/.odex/rules/`, where "Always allow" approvals are persisted)?
pub fn rules_allow_all(engine: &Engine, argv_list: &[Vec<String>]) -> bool {
    let policy = engine.policy.read().unwrap();
    !argv_list.is_empty()
        && argv_list.iter().all(|argv| policy.evaluate_argv(argv).decision == Some(odex_execpolicy::Decision::Allow))
}

/// "Always allow": append an allow rule for `prefix` to
/// `~/.odex/rules/default.toml` and reload the user exec policy so the next
/// command with that prefix runs without asking.
pub fn remember_exec_rule(engine: &Engine, prefix: &[String]) -> anyhow::Result<()> {
    if prefix.is_empty() {
        anyhow::bail!("no command prefix to remember");
    }
    if !rules_allow_all(engine, &[prefix.to_vec()]) {
        let file = engine.home.rules_dir().join("default.toml");
        odex_execpolicy::append_allow_rule(&file, prefix, odex_execpolicy::Decision::Allow)?;
    }
    let (policy, warnings) = odex_execpolicy::Policy::load(&[engine.home.rules_dir()]);
    for w in warnings {
        tracing::warn!("exec policy: {w}");
    }
    *engine.policy.write().unwrap() = policy;
    Ok(())
}

/// Run the approval flow for an exec request. Returns Ok(sandboxed?) to run,
/// or Err(outcome) when it must not run.
#[allow(clippy::too_many_arguments)]
async fn approve_exec(
    o: &Out<'_>,
    command: &str,
    workdir: &Path,
    escalated: bool,
    justification: Option<String>,
    item_id: &str,
) -> Result<(bool, Option<String>, Option<String>), ToolOutcome> {
    let s = &o.tctx.settings;
    let t = o.rt.thread();
    let eval = o.engine.exec_policy_eval(&t, command, shell_kind(s));
    let argv_list = eval.commands.clone().unwrap_or_default();
    // persisted "Always allow" rules count like a session approval (Auto mode only)
    let session_allows = o.rt.session_allow.lock().unwrap().allows_command(&argv_list)
        || (t.permission_mode == PermissionMode::Auto && rules_allow_all(o.engine, &argv_list));
    let plan = approval::plan_exec(&ExecCtx {
        mode: t.permission_mode,
        eval: &eval,
        escalated,
        network_allowed: s.sandbox.network_access,
        session_allows,
        sandbox_available: sandbox_available(o.engine, s),
        plan_mode: o.tctx.mode == TurnMode::Plan,
        policy: s.raw.approval_policy.unwrap_or_default(),
    });
    match plan {
        ExecPlan::Refuse(msg) => Err(o.err(msg)),
        ExecPlan::Run { sandbox } => Ok((sandbox, None, None)),
        ExecPlan::Ask { reason, unsandboxed_if_approved } => {
            let prefix = argv_list.first().map(|a| odex_execpolicy::approval_prefix(a)).unwrap_or_default();
            let kind = ApprovalKind::Exec {
                command: command.to_string(),
                cwd: workdir.to_string_lossy().to_string(),
                justification: justification.or(eval.justification.clone()),
                reason: reason.clone(),
                prefix: prefix.clone(),
                sandbox_output: None,
            };
            let d = approval::request(o.engine, o.rt, &o.tctx.turn_id, Some(item_id.to_string()), kind, &o.tctx.cancel)
                .await;
            let sandboxed_after = !unsandboxed_if_approved && sandbox_available(o.engine, s);
            match d {
                ApprovalDecision::Approve => Ok((sandboxed_after, None, None)),
                ApprovalDecision::ApproveForSession => {
                    let mut sa = o.rt.session_allow.lock().unwrap();
                    for argv in &argv_list {
                        let p = odex_execpolicy::approval_prefix(argv);
                        if !p.is_empty() && !sa.command_prefixes.contains(&p) {
                            sa.command_prefixes.push(p);
                        }
                    }
                    Ok((sandboxed_after, None, None))
                }
                ApprovalDecision::Custom { command: c, feedback } => Ok((sandboxed_after, c, feedback)),
                ApprovalDecision::Deny { feedback } => Err(ToolOutcome {
                    text: format!(
                        "The user denied this command{}.{}",
                        if reason == "network" { " (it needs network access)" } else { "" },
                        feedback.map(|f| format!(" User feedback: {f}")).unwrap_or_default()
                    ),
                    meta: o.meta(false),
                    ..Default::default()
                }),
                ApprovalDecision::Abort => Err(ToolOutcome {
                    text: "The user stopped the turn.".into(),
                    meta: o.meta(false),
                    abort_turn: true,
                    ..Default::default()
                }),
            }
        }
    }
}

async fn shell(o: &Out<'_>) -> ToolOutcome {
    let mut command = o.args["command"].as_str().unwrap_or("").to_string();
    let cwd = o.cwd();
    let workdir = o
        .args
        .get("workdir")
        .and_then(|v| v.as_str())
        .map(|w| odex_tools::edit::resolve(&cwd, w))
        .unwrap_or(cwd.clone());
    if !workdir.is_dir() {
        return o.err(format!("workdir does not exist: {}", workdir.display()));
    }
    // apply_patch sent through the shell → real patch tool
    if let Some(ex) = odex_apply_patch::extract_patch_from_command(&command) {
        let dir = ex.workdir.map(|d| odex_tools::edit::resolve(&workdir, &d));
        return apply_patch(o, &ex.patch, dir).await;
    }
    let timeout_ms = o.args.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(120_000).clamp(1000, 3_600_000);
    let escalated = o.args.get("escalated").and_then(|v| v.as_bool()).unwrap_or(false);
    let justification = o.args.get("justification").and_then(|v| v.as_str()).map(String::from);
    let item_id = format!("item_{}", o.call.id);
    let (mut sandboxed, replaced, feedback) =
        match approve_exec(o, &command, &workdir, escalated, justification, &item_id).await {
            Ok(x) => x,
            Err(mut out) => {
                let item = command_item(
                    &item_id,
                    &command,
                    &workdir,
                    ItemStatus::Declined,
                    None,
                    None,
                    &out.text,
                    None,
                    false,
                );
                o.engine.emitter().item_started(&o.rt.id, &o.tctx.turn_id, &item);
                complete_item(o.engine, o.rt, &o.tctx.turn_id, item);
                out.meta.exit_code = None;
                return out;
            }
        };
    if let Some(c) = replaced {
        command = c;
    }
    let readonly = o.rt.thread().permission_mode == PermissionMode::ReadOnly || o.tctx.mode == TurnMode::Plan;
    let mut attempt = 0;
    loop {
        attempt += 1;
        let result = run_command(o, &command, &workdir, sandboxed, readonly, timeout_ms, &item_id).await;
        let (out, exit_code, duration, timed_out, denied) = match result {
            Ok(r) => r,
            Err(e) if e.contains("sandbox unavailable") && sandboxed => {
                // never silently run unsandboxed: ask
                let kind = ApprovalKind::Exec {
                    command: command.clone(),
                    cwd: workdir.to_string_lossy().to_string(),
                    justification: None,
                    reason: "sandboxUnavailable".into(),
                    prefix: vec![],
                    sandbox_output: Some(e.clone()),
                };
                match approval::request(o.engine, o.rt, &o.tctx.turn_id, Some(item_id.clone()), kind, &o.tctx.cancel)
                    .await
                {
                    ApprovalDecision::Approve
                    | ApprovalDecision::ApproveForSession
                    | ApprovalDecision::Custom { .. } => {
                        sandboxed = false;
                        continue;
                    }
                    ApprovalDecision::Abort => {
                        return ToolOutcome {
                            text: "The user stopped the turn.".into(),
                            meta: o.meta(false),
                            abort_turn: true,
                            ..Default::default()
                        }
                    }
                    ApprovalDecision::Deny { .. } => {
                        return o
                            .err(format!("the sandbox is unavailable ({e}) and the user declined to run without it"))
                    }
                }
            }
            Err(e) => return o.err(e),
        };
        // sandbox denied something → offer to retry unsandboxed (on-failure)
        if denied
            && sandboxed
            && attempt == 1
            && o.rt.thread().permission_mode == PermissionMode::Auto
            && o.tctx.settings.raw.approval_policy != Some(ApprovalPolicy::Never)
        {
            let kind = ApprovalKind::Exec {
                command: command.clone(),
                cwd: workdir.to_string_lossy().to_string(),
                justification: Some(
                    "The command failed inside the sandbox (access denied). Retry without the sandbox?".into(),
                ),
                reason: "sandboxFailure".into(),
                prefix: vec![],
                sandbox_output: Some(out.chars().rev().take(2000).collect::<String>().chars().rev().collect()),
            };
            match approval::request(o.engine, o.rt, &o.tctx.turn_id, Some(item_id.clone()), kind, &o.tctx.cancel).await
            {
                ApprovalDecision::Approve | ApprovalDecision::ApproveForSession | ApprovalDecision::Custom { .. } => {
                    sandboxed = false;
                    continue;
                }
                ApprovalDecision::Abort => {
                    return ToolOutcome {
                        text: "The user stopped the turn.".into(),
                        meta: o.meta(false),
                        abort_turn: true,
                        ..Default::default()
                    }
                }
                ApprovalDecision::Deny { .. } => {}
            }
        }
        let mut meta = o.meta(exit_code == Some(0));
        meta.exit_code = exit_code;
        let inline = o.finalize(&out, &mut meta);
        let mut text = format!(
            "Exit code: {}\nWall time: {:.1}s\n",
            exit_code.map(|c| c.to_string()).unwrap_or_else(|| "none".into()),
            duration.as_secs_f64()
        );
        if timed_out {
            text.push_str(&format!("Timed out after {}s (process tree killed).\n", timeout_ms / 1000));
        }
        if denied && sandboxed {
            text.push_str("Note: the sandbox blocked an operation. If this command legitimately needs it, retry with escalated=true and a justification.\n");
        }
        text.push_str("Output:\n");
        text.push_str(if inline.trim().is_empty() { "(no output)" } else { &inline });
        if let Some(f) = &feedback {
            text.push_str(&format!("\nUser note: {f}"));
        }
        let item = command_item(
            &item_id,
            &command,
            &workdir,
            if exit_code == Some(0) { ItemStatus::Completed } else { ItemStatus::Failed },
            exit_code,
            Some(duration.as_millis() as u64),
            &out,
            meta.output_ref.clone(),
            sandboxed,
        );
        complete_item(o.engine, o.rt, &o.tctx.turn_id, item);
        return ToolOutcome { text, meta, ..Default::default() };
    }
}

#[allow(clippy::too_many_arguments)]
fn command_item(
    id: &str,
    command: &str,
    cwd: &Path,
    status: ItemStatus,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    output: &str,
    output_ref: Option<String>,
    sandboxed: bool,
) -> ThreadItem {
    ThreadItem::CommandExecution {
        id: id.to_string(),
        command: command.to_string(),
        cwd: cwd.to_string_lossy().to_string(),
        status,
        exit_code,
        duration_ms,
        output: clip_display(output),
        output_ref,
        sandboxed,
        session_id: None,
    }
}

type RunResult = Result<(String, Option<i32>, Duration, bool, bool), String>;

async fn run_command(
    o: &Out<'_>,
    command: &str,
    workdir: &Path,
    sandboxed: bool,
    readonly: bool,
    timeout_ms: u64,
    item_id: &str,
) -> RunResult {
    let s = &o.tctx.settings;
    let mut req = sandbox_request(o.engine, o.rt, s, sandboxed, readonly);
    req.argv = odex_sandbox::shell_argv(&s.default_shell, command);
    req.cwd = workdir.to_path_buf();
    req.timeout = Some(Duration::from_millis(timeout_ms));
    let started = command_item(item_id, command, workdir, ItemStatus::InProgress, None, None, "", None, sandboxed);
    o.engine.emitter().item_started(&o.rt.id, &o.tctx.turn_id, &started);
    let em = o.engine.emitter();
    let (thread_id, turn_id, iid) = (o.rt.id.clone(), o.tctx.turn_id.clone(), item_id.to_string());
    let on_output = move |chunk: odex_sandbox::OutputChunk| {
        let text = String::from_utf8_lossy(chunk.bytes()).to_string();
        em.item_delta(
            &thread_id,
            &turn_id,
            &iid,
            ItemDelta::CommandOutput {
                chunk: text,
                stream: if chunk.is_stderr() { "stderr".into() } else { "stdout".into() },
            },
        );
    };
    match odex_sandbox::exec(req, on_output, o.tctx.cancel.clone()).await {
        Ok(r) => Ok((r.aggregated, r.exit_code, r.duration, r.timed_out, r.sandbox_denied)),
        Err(odex_sandbox::SandboxError::Unavailable(m)) => Err(format!("sandbox unavailable: {m}")),
        Err(e) => Err(e.to_string()),
    }
}

async fn exec_command(o: &Out<'_>) -> ToolOutcome {
    let s = o.tctx.settings.clone();
    let command = o.args["command"].as_str().unwrap_or("").to_string();
    let cwd = o.cwd();
    let workdir =
        o.args.get("workdir").and_then(|v| v.as_str()).map(|w| odex_tools::edit::resolve(&cwd, w)).unwrap_or(cwd);
    let item_id = format!("item_{}", o.call.id);
    let (sandboxed, replaced, _fb) = match approve_exec(o, &command, &workdir, false, None, &item_id).await {
        Ok(x) => x,
        Err(out) => return out,
    };
    let command = replaced.unwrap_or(command);
    let readonly = o.rt.thread().permission_mode == PermissionMode::ReadOnly;
    let spec = crate::sessions::StartSpec {
        thread_id: Some(o.rt.id.clone()),
        command: command.clone(),
        argv: odex_sandbox::shell_argv(&s.default_shell, &command),
        cwd: workdir.clone(),
        env: crate::worktrees::thread_env_vars(o.engine, &o.rt.thread())
            .into_iter()
            .chain([("ODEX".to_string(), "1".to_string())])
            .collect(),
        sandbox: if sandboxed { Some(sandbox_request(o.engine, o.rt, &s, true, readonly)) } else { None },
        on_url: Some(crate::sessions::dev_url_hook(o.engine.emitter())),
    };
    let session = match o.engine.sessions.start(spec).await {
        Ok(s) => s,
        Err(e) => return o.err(format!("could not start the process: {e}")),
    };
    let yield_ms = o.args.get("yield_ms").and_then(|v| v.as_u64()).unwrap_or(2000).clamp(100, 30_000);
    let out = session.read_new(Duration::from_millis(yield_ms)).await;
    let info = session.info();
    let mut meta = o.meta(true);
    let inline = o.finalize(&out, &mut meta);
    let status_line = if info.running {
        format!("Session {} is running (pid {}). Use write_stdin with session_id=\"{}\" to send input or poll output; kill=true stops it.", info.id, info.pid.unwrap_or(0), info.id)
    } else {
        format!("Process exited with code {}.", info.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()))
    };
    let item = ThreadItem::CommandExecution {
        id: item_id,
        command: command.clone(),
        cwd: workdir.to_string_lossy().to_string(),
        status: if info.running || info.exit_code == Some(0) { ItemStatus::Completed } else { ItemStatus::Failed },
        exit_code: info.exit_code,
        duration_ms: None,
        output: clip_display(&out),
        output_ref: meta.output_ref.clone(),
        sandboxed,
        session_id: Some(info.id.clone()),
    };
    o.engine.emitter().item_started(&o.rt.id, &o.tctx.turn_id, &item);
    complete_item(o.engine, o.rt, &o.tctx.turn_id, item);
    ToolOutcome {
        text: format!("{status_line}\nOutput:\n{}", if inline.trim().is_empty() { "(no output yet)" } else { &inline }),
        meta,
        ..Default::default()
    }
}

async fn write_stdin(o: &Out<'_>) -> ToolOutcome {
    let id = o.args["session_id"].as_str().unwrap_or("");
    let Some(session) = o.engine.sessions.get(id) else {
        let known: Vec<String> = o.engine.sessions.list(Some(&o.rt.id)).into_iter().map(|s| s.id).collect();
        return o.err(format!("no session `{id}` (known: {})", known.join(", ")));
    };
    if o.args.get("kill").and_then(|v| v.as_bool()).unwrap_or(false) {
        session.kill().await;
        return ToolOutcome { text: format!("Session {id} killed."), meta: o.meta(true), ..Default::default() };
    }
    if let Some(chars) = o.args.get("chars").and_then(|v| v.as_str()) {
        if !chars.is_empty() {
            if let Err(e) = session.write(chars).await {
                return o.err(format!("write failed: {e}"));
            }
        }
    }
    let yield_ms = o.args.get("yield_ms").and_then(|v| v.as_u64()).unwrap_or(1000).clamp(50, 30_000);
    let out = session.read_new(Duration::from_millis(yield_ms)).await;
    let info = session.info();
    let mut meta = o.meta(true);
    let inline = o.finalize(&out, &mut meta);
    o.start_generic();
    o.done_generic(true, &out);
    let state = if info.running {
        "running".to_string()
    } else {
        format!("exited with code {}", info.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()))
    };
    ToolOutcome {
        text: format!("Session {id} is {state}.\nNew output:\n{}", if inline.is_empty() { "(none)" } else { &inline }),
        meta,
        ..Default::default()
    }
}

// ------------------------------------------------------------ file edits

fn to_change(cwd: &Path, p: &odex_apply_patch::FileChangePreview) -> FileChange {
    let disp = odex_tools::edit::display_path(cwd, &p.path);
    FileChange {
        path: disp,
        kind: match p.kind {
            odex_apply_patch::ChangeKind::Add => FileChangeKind::Add,
            odex_apply_patch::ChangeKind::Delete => FileChangeKind::Delete,
            odex_apply_patch::ChangeKind::Update if p.move_to.is_some() => FileChangeKind::Move,
            odex_apply_patch::ChangeKind::Update => FileChangeKind::Update,
        },
        move_path: p.move_to.as_ref().map(|m| odex_tools::edit::display_path(cwd, m)),
        diff: p.unified_diff.clone(),
        additions: p.additions,
        deletions: p.deletions,
    }
}

/// Permission for writing `paths`. Ok(()) to proceed.
async fn approve_writes(
    o: &Out<'_>,
    paths: &[PathBuf],
    changes: &[FileChange],
    item_id: &str,
) -> Result<(), ToolOutcome> {
    let t = o.rt.thread();
    if o.tctx.mode != TurnMode::Default {
        return Err(o.err("this turn is read-only (planning/review mode); describe the change instead of making it"));
    }
    let roots = o.engine.writable_roots(&t, &o.tctx.settings);
    let outside: Vec<String> =
        paths.iter().filter(|p| !approval::within_roots(p, &roots)).map(|p| p.to_string_lossy().to_string()).collect();
    let needs = match t.permission_mode {
        PermissionMode::FullAccess => false,
        PermissionMode::Auto => !outside.is_empty() && !o.rt.session_allow.lock().unwrap().patch_outside_workspace,
        PermissionMode::ReadOnly => !o.rt.session_allow.lock().unwrap().patch_outside_workspace,
    };
    if !needs {
        return Ok(());
    }
    let reason = if t.permission_mode == PermissionMode::ReadOnly { "readOnly" } else { "outsideWorkspace" };
    let kind = ApprovalKind::Patch { changes: changes.to_vec(), reason: reason.into(), outside_workspace: outside };
    match approval::request(o.engine, o.rt, &o.tctx.turn_id, Some(item_id.to_string()), kind, &o.tctx.cancel).await {
        ApprovalDecision::Approve | ApprovalDecision::Custom { .. } => Ok(()),
        ApprovalDecision::ApproveForSession => {
            o.rt.session_allow.lock().unwrap().patch_outside_workspace = true;
            Ok(())
        }
        ApprovalDecision::Deny { feedback } => Err(ToolOutcome {
            text: format!(
                "The user rejected this change.{}",
                feedback.map(|f| format!(" Feedback: {f}")).unwrap_or_default()
            ),
            meta: o.meta(false),
            ..Default::default()
        }),
        ApprovalDecision::Abort => Err(ToolOutcome {
            text: "The user stopped the turn.".into(),
            meta: o.meta(false),
            abort_turn: true,
            ..Default::default()
        }),
    }
}

fn file_change_item(id: &str, changes: Vec<FileChange>, status: ItemStatus, error: Option<String>) -> ThreadItem {
    ThreadItem::FileChange { id: id.to_string(), changes, status, error }
}

async fn apply_patch(o: &Out<'_>, patch_text: &str, dir: Option<PathBuf>) -> ToolOutcome {
    let cwd = dir.unwrap_or_else(|| o.cwd());
    let item_id = format!("item_{}", o.call.id);
    let fail = |msg: String| {
        let item = file_change_item(&item_id, vec![], ItemStatus::Failed, Some(msg.clone()));
        o.engine.emitter().item_started(&o.rt.id, &o.tctx.turn_id, &item);
        complete_item(o.engine, o.rt, &o.tctx.turn_id, item);
        let mut meta = o.meta(false);
        meta.tool = "apply_patch".into();
        ToolOutcome { text: format!("Error: patch not applied. {msg}"), meta, ..Default::default() }
    };
    let patch = match odex_apply_patch::parse_patch(patch_text) {
        Ok(p) => p,
        Err(e) => return fail(e.to_string()),
    };
    let previews = match odex_apply_patch::preview(&patch, &cwd) {
        Ok(p) => p,
        Err(e) => return fail(e.to_string()),
    };
    let changes: Vec<FileChange> = previews.iter().map(|p| to_change(&cwd, p)).collect();
    let mut paths: Vec<PathBuf> = previews.iter().map(|p| p.path.clone()).collect();
    paths.extend(previews.iter().filter_map(|p| p.move_to.clone()));
    let started = file_change_item(&item_id, changes.clone(), ItemStatus::InProgress, None);
    o.engine.emitter().item_started(&o.rt.id, &o.tctx.turn_id, &started);
    if let Err(out) = approve_writes(o, &paths, &changes, &item_id).await {
        complete_item(
            o.engine,
            o.rt,
            &o.tctx.turn_id,
            file_change_item(&item_id, changes, ItemStatus::Declined, Some(out.text.clone())),
        );
        return out;
    }
    match odex_apply_patch::apply(&patch, &cwd) {
        Ok(applied) => {
            let changes: Vec<FileChange> = applied.iter().map(|p| to_change(&cwd, p)).collect();
            for c in &changes {
                let srcs = o.rt.touch_source(&c.path, false, true);
                o.engine.emitter().sources(&o.rt.id, srcs);
            }
            let summary = odex_apply_patch::summarize(&applied, &cwd);
            complete_item(
                o.engine,
                o.rt,
                &o.tctx.turn_id,
                file_change_item(&item_id, changes.clone(), ItemStatus::Completed, None),
            );
            let mut meta = o.meta(true);
            meta.tool = "apply_patch".into();
            meta.file_writes = changes.iter().map(|c| c.path.clone()).collect();
            ToolOutcome { text: format!("Patch applied.\n{summary}"), meta, ..Default::default() }
        }
        Err(e) => {
            complete_item(
                o.engine,
                o.rt,
                &o.tctx.turn_id,
                file_change_item(&item_id, changes, ItemStatus::Failed, Some(e.to_string())),
            );
            o.err(format!("patch not applied: {e}"))
        }
    }
}

async fn edit_or_write(o: &Out<'_>) -> ToolOutcome {
    let cwd = o.cwd();
    let path = o.args["path"].as_str().unwrap_or("").to_string();
    let plan = if o.call.name == "write_file" {
        Ok(odex_tools::edit::plan_write(&cwd, &path, o.args["content"].as_str().unwrap_or("")))
    } else {
        odex_tools::edit::plan_edit(
            &cwd,
            &path,
            o.args["old_string"].as_str().unwrap_or(""),
            o.args["new_string"].as_str().unwrap_or(""),
            o.args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false),
        )
    };
    let item_id = format!("item_{}", o.call.id);
    let w = match plan {
        Ok(w) => w,
        Err(e) => {
            o.start_generic();
            o.done_generic(false, &e);
            return o.err(e);
        }
    };
    let started = file_change_item(&item_id, vec![w.change.clone()], ItemStatus::InProgress, None);
    o.engine.emitter().item_started(&o.rt.id, &o.tctx.turn_id, &started);
    if let Err(out) = approve_writes(o, std::slice::from_ref(&w.path), std::slice::from_ref(&w.change), &item_id).await
    {
        complete_item(
            o.engine,
            o.rt,
            &o.tctx.turn_id,
            file_change_item(&item_id, vec![w.change.clone()], ItemStatus::Declined, Some(out.text.clone())),
        );
        return out;
    }
    if let Err(e) = odex_tools::edit::commit(&w) {
        complete_item(
            o.engine,
            o.rt,
            &o.tctx.turn_id,
            file_change_item(&item_id, vec![w.change.clone()], ItemStatus::Failed, Some(e.to_string())),
        );
        return o.err(format!("could not write {path}: {e}"));
    }
    let srcs = o.rt.touch_source(&w.change.path, false, true);
    o.engine.emitter().sources(&o.rt.id, srcs);
    complete_item(
        o.engine,
        o.rt,
        &o.tctx.turn_id,
        file_change_item(&item_id, vec![w.change.clone()], ItemStatus::Completed, None),
    );
    let mut meta = o.meta(true);
    meta.file_writes = vec![w.change.path.clone()];
    let verb = if w.old.is_some() { "Updated" } else { "Created" };
    ToolOutcome {
        text: format!("{verb} {} (+{} -{}).", w.change.path, w.change.additions, w.change.deletions),
        meta,
        ..Default::default()
    }
}

// ----------------------------------------------------------------- reading

async fn read_file(o: &Out<'_>) -> ToolOutcome {
    let cwd = o.cwd();
    let path = o.args["path"].as_str().unwrap_or("").to_string();
    let full = odex_tools::edit::resolve(&cwd, &path);
    let window = o.tctx.model.context_window;
    let default_limit = ((window / 24) as usize).clamp(120, 2000);
    let offset = o.args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
    let limit =
        o.args.get("limit").and_then(|v| v.as_u64()).map(|l| l as usize).unwrap_or(default_limit).clamp(1, 5000);
    o.start_generic();
    let r = tokio::task::spawn_blocking(move || odex_file_search::read_file_paged(&full, offset, limit, 2000)).await;
    let page = match r {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            o.done_generic(false, &e.to_string());
            return o.err(format!("{e}"));
        }
        Err(e) => return o.err(e.to_string()),
    };
    if page.is_binary {
        o.done_generic(true, "binary file");
        return ToolOutcome {
            text: format!("{path} is a binary file ({}); it cannot be shown as text.", page.encoding),
            meta: o.meta(true),
            ..Default::default()
        };
    }
    let mut text = page.text.clone();
    if page.total_lines == 0 {
        text = "(empty file)".into();
    } else if page.truncated || page.start_line > 1 {
        text.push_str(&format!(
            "\n[showing lines {}-{} of {}{}]",
            page.start_line,
            page.end_line,
            page.total_lines,
            if page.truncated { format!("; continue with offset={}", page.end_line + 1) } else { String::new() }
        ));
    }
    let disp = odex_tools::edit::display_path(&cwd, &odex_tools::edit::resolve(&cwd, &path));
    let srcs = o.rt.touch_source(&disp, true, false);
    o.engine.emitter().sources(&o.rt.id, srcs);
    let mut meta = o.meta(true);
    meta.file_read = Some(disp.clone());
    let inline = o.finalize(&text, &mut meta);
    let item = ThreadItem::ToolCall {
        id: format!("item_{}", o.call.id),
        tool: "read_file".into(),
        arguments: o.args.clone(),
        status: ItemStatus::Completed,
        output: None,
        summary: Some(format!("Read {disp} (lines {}-{} of {})", page.start_line, page.end_line, page.total_lines)),
    };
    complete_item(o.engine, o.rt, &o.tctx.turn_id, item);
    ToolOutcome { text: inline, meta, ..Default::default() }
}

async fn list_dir(o: &Out<'_>) -> ToolOutcome {
    let cwd = o.cwd();
    let path = o.args.get("path").and_then(|v| v.as_str()).map(|p| odex_tools::edit::resolve(&cwd, p)).unwrap_or(cwd);
    let depth = o.args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2).clamp(1, 6) as usize;
    o.start_generic();
    let r = tokio::task::spawn_blocking(move || odex_file_search::list_dir(&path, depth, 500)).await;
    match r {
        Ok(Ok(text)) => {
            let mut meta = o.meta(true);
            let inline = o.finalize(&text, &mut meta);
            o.done_generic(true, &text);
            ToolOutcome { text: inline, meta, ..Default::default() }
        }
        Ok(Err(e)) => {
            o.done_generic(false, &e.to_string());
            o.err(e.to_string())
        }
        Err(e) => o.err(e.to_string()),
    }
}

async fn grep(o: &Out<'_>) -> ToolOutcome {
    let cwd = o.cwd();
    let mut opts = odex_file_search::GrepOptions::new(
        o.args["pattern"].as_str().unwrap_or(""),
        o.args.get("path").and_then(|v| v.as_str()).map(|p| odex_tools::edit::resolve(&cwd, p)).unwrap_or(cwd.clone()),
    );
    opts.glob = o.args.get("glob").and_then(|v| v.as_str()).map(String::from);
    opts.case_insensitive = o.args.get("case_insensitive").and_then(|v| v.as_bool()).unwrap_or(false);
    let ctx_lines = o.args.get("context").and_then(|v| v.as_u64()).unwrap_or(0).min(20) as usize;
    opts.context_before = ctx_lines;
    opts.context_after = ctx_lines;
    opts.max_results = o.args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(200).clamp(1, 2000) as usize;
    opts.mode = match o.args.get("output_mode").and_then(|v| v.as_str()) {
        Some("files_with_matches") => odex_file_search::GrepMode::FilesWithMatches,
        Some("count") => odex_file_search::GrepMode::Count,
        _ => odex_file_search::GrepMode::Content,
    };
    o.start_generic();
    let r = tokio::task::spawn_blocking(move || odex_file_search::grep(&opts)).await;
    match r {
        Ok(Ok(res)) => {
            let mut text = res.text.clone();
            // show paths relative to cwd
            let prefix = format!("{}{}", cwd.display(), std::path::MAIN_SEPARATOR);
            text = text.replace(&prefix, "");
            if res.matches == 0 {
                text = "No matches.".into();
            } else if res.truncated {
                text.push_str("\n[results truncated; narrow the pattern or path]");
            }
            let mut meta = o.meta(true);
            let inline = o.finalize(&text, &mut meta);
            o.done_generic(true, &format!("{} matches in {} files", res.matches, res.files));
            ToolOutcome { text: inline, meta, ..Default::default() }
        }
        Ok(Err(e)) => {
            o.done_generic(false, &e.to_string());
            o.err(e.to_string())
        }
        Err(e) => o.err(e.to_string()),
    }
}

async fn glob(o: &Out<'_>) -> ToolOutcome {
    let cwd = o.cwd();
    let root =
        o.args.get("path").and_then(|v| v.as_str()).map(|p| odex_tools::edit::resolve(&cwd, p)).unwrap_or(cwd.clone());
    let pattern = o.args["pattern"].as_str().unwrap_or("").to_string();
    o.start_generic();
    let r2 = root.clone();
    let r = tokio::task::spawn_blocking(move || odex_file_search::glob(&r2, &pattern, 300)).await;
    match r {
        Ok(Ok(paths)) => {
            let text = if paths.is_empty() {
                "No files matched.".to_string()
            } else {
                paths.iter().map(|p| odex_tools::edit::display_path(&cwd, p)).collect::<Vec<_>>().join("\n")
            };
            let mut meta = o.meta(true);
            let inline = o.finalize(&text, &mut meta);
            o.done_generic(true, &format!("{} files", paths.len()));
            ToolOutcome { text: inline, meta, ..Default::default() }
        }
        Ok(Err(e)) => {
            o.done_generic(false, &e.to_string());
            o.err(e.to_string())
        }
        Err(e) => o.err(e.to_string()),
    }
}

async fn view_image(o: &Out<'_>) -> ToolOutcome {
    let cwd = o.cwd();
    let path = o.args["path"].as_str().unwrap_or("").to_string();
    let full = odex_tools::edit::resolve(&cwd, &path);
    let max_px = o.tctx.model.model.max_image_px;
    let r = tokio::task::spawn_blocking(move || odex_tools::load_image_data_url(&full, max_px)).await;
    let (url, w, h) = match r {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return o.err(e),
        Err(e) => return o.err(e.to_string()),
    };
    complete_item(
        o.engine,
        o.rt,
        &o.tctx.turn_id,
        ThreadItem::ImageView { id: format!("item_{}", o.call.id), path: path.clone() },
    );
    if o.tctx.model.model.capabilities.vision {
        ToolOutcome {
            text: format!("Loaded image {path} ({w}x{h}); it is attached below."),
            images: vec![url],
            meta: o.meta(true),
            ..Default::default()
        }
    } else {
        let t = o.rt.thread();
        let desc = crate::turn::describe_image(
            o.engine,
            &t,
            &url,
            "Describe this image in detail for a coding agent. Transcribe visible text.",
        )
        .await;
        ToolOutcome {
            text: format!("Image {path} ({w}x{h}), described by the vision model:\n{desc}"),
            meta: o.meta(true),
            ..Default::default()
        }
    }
}

fn update_plan(o: &Out<'_>) -> ToolOutcome {
    let explanation = o.args.get("explanation").and_then(|v| v.as_str()).map(String::from);
    let steps: Vec<PlanStep> = o.args["plan"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| {
                    let step = s.get("step")?.as_str()?.to_string();
                    let status = match s.get("status").and_then(|x| x.as_str()).unwrap_or("pending") {
                        "completed" | "done" => PlanStepStatus::Completed,
                        "in_progress" | "inProgress" | "active" => PlanStepStatus::InProgress,
                        _ => PlanStepStatus::Pending,
                    };
                    Some(PlanStep { step, status })
                })
                .collect()
        })
        .unwrap_or_default();
    *o.rt.plan.lock().unwrap() = (explanation.clone(), steps.clone());
    o.rt.log(crate::rollout::RolloutLine::Plan { explanation: explanation.clone(), plan: steps.clone() });
    o.engine.emitter().plan_updated(&o.rt.id, Some(&o.tctx.turn_id), explanation.clone(), steps.clone());
    complete_item(
        o.engine,
        o.rt,
        &o.tctx.turn_id,
        ThreadItem::Plan { id: format!("item_{}", o.call.id), explanation, steps },
    );
    ToolOutcome { text: "Plan updated.".into(), meta: o.meta(true), ..Default::default() }
}

fn read_output(o: &Out<'_>) -> ToolOutcome {
    let r = o.args["ref"].as_str().unwrap_or("");
    let offset = o.args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
    let limit = o.args.get("limit").and_then(|v| v.as_u64()).unwrap_or(300).clamp(1, 3000) as usize;
    match o.rt.outputs.read(r, offset, limit) {
        Ok(text) => {
            let mut meta = o.meta(true);
            // paged reads are never re-stored
            meta.full_chars = text.len();
            ToolOutcome { text, meta, ..Default::default() }
        }
        Err(e) => o.err(e),
    }
}

fn recall(o: &Out<'_>) -> ToolOutcome {
    let q = o.args["query"].as_str().unwrap_or("");
    let limit = o.args.get("limit").and_then(|v| v.as_u64()).unwrap_or(6).clamp(1, 20) as usize;
    o.start_generic();
    match o.engine.store.recall(&o.rt.id, q, limit) {
        Ok(hits) if !hits.is_empty() => {
            let mut text = format!("{} match(es) in earlier history:\n", hits.len());
            for (i, h) in hits.iter().enumerate() {
                let detail = if i < 3 { odex_context::summary::clip_middle(&h.text, 1200) } else { h.snippet.clone() };
                text.push_str(&format!(
                    "\n--- [turn {}] {} (entry {})\n{}\n",
                    h.turn_index + 1,
                    h.label,
                    h.entry_id,
                    detail.trim()
                ));
            }
            let mut meta = o.meta(true);
            let inline = o.finalize(&text, &mut meta);
            o.done_generic(true, &format!("{} matches", hits.len()));
            ToolOutcome { text: inline, meta, ..Default::default() }
        }
        Ok(_) => {
            o.done_generic(true, "no matches");
            ToolOutcome {
                text: format!("No earlier history matches `{q}`. Try other keywords."),
                meta: o.meta(true),
                ..Default::default()
            }
        }
        Err(e) => o.err(e.to_string()),
    }
}

async fn read_terminal(o: &Out<'_>) -> ToolOutcome {
    let params = json!({
        "threadId": o.rt.id,
        "terminalId": o.args.get("terminal_id").and_then(|v| v.as_str()),
        "lines": o.args.get("lines").and_then(|v| v.as_u64()).unwrap_or(200),
    });
    o.start_generic();
    match o.engine.emitter().sink().request(server_request::TERMINAL_READ, params).await {
        Ok(v) => {
            let r: TerminalReadResponse = match serde_json::from_value(v) {
                Ok(r) => r,
                Err(e) => return o.err(format!("bad terminal response: {e}")),
            };
            let mut text = String::new();
            if !r.terminals.is_empty() {
                text.push_str("Terminals: ");
                text.push_str(
                    &r.terminals
                        .iter()
                        .map(|t| format!("{} ({}, {})", t.id, t.title, if t.running { "running" } else { "exited" }))
                        .collect::<Vec<_>>()
                        .join("; "),
                );
                text.push('\n');
            }
            text.push_str(&crate::sessions::strip_ansi(&r.text));
            let mut meta = o.meta(true);
            let inline = o.finalize(&text, &mut meta);
            o.done_generic(true, &text);
            ToolOutcome { text: inline, meta, ..Default::default() }
        }
        Err(e) => {
            o.done_generic(false, &e.to_string());
            o.err(format!("no integrated terminal is available ({e})"))
        }
    }
}
