//! `odex-engine exec`: run a single task headlessly and print the result.

use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use clap::Args;
use serde_json::Value;
use tokio::sync::watch;

use odex_core::{api, Engine, EventSink};
use odex_protocol::*;

#[derive(Debug, Clone, Args)]
pub struct ExecArgs {
    /// The task. Use `-` to read it from stdin.
    pub prompt: String,
    /// Working directory (default: current directory).
    #[arg(long, short = 'C')]
    pub cwd: Option<String>,
    /// Model key (`[models.<key>]`, `provider:model`, or a served model id).
    #[arg(long, short = 'm')]
    pub model: Option<String>,
    /// read-only | auto | full-access
    #[arg(long, default_value = "auto")]
    pub permission_mode: String,
    /// Reasoning effort: none|minimal|low|medium|high|xhigh
    #[arg(long)]
    pub effort: Option<String>,
    /// Approve every approval request (otherwise they are denied).
    #[arg(long)]
    pub auto_approve: bool,
    /// Print every event as JSON lines on stdout.
    #[arg(long)]
    pub json: bool,
    /// Planning mode (read-only, ends with a plan).
    #[arg(long)]
    pub plan: bool,
    /// Continue an existing thread.
    #[arg(long)]
    pub resume: Option<String>,
    /// Write the final agent message to this file.
    #[arg(long)]
    pub output_last_message: Option<String>,
    /// Abort after this many seconds.
    #[arg(long, default_value_t = 3600)]
    pub timeout: u64,
}

struct ExecSink {
    json: bool,
    auto_approve: bool,
    done: watch::Sender<Option<(String, Turn)>>,
    last_agent: Mutex<String>,
}

#[async_trait]
impl EventSink for ExecSink {
    fn notify(&self, method: &str, params: Value) {
        if self.json {
            let line = serde_json::json!({"method": method, "params": params});
            println!("{line}");
            let _ = std::io::stdout().flush();
        } else {
            human(method, &params);
        }
        if method == notification::ITEM_COMPLETED {
            if let Ok(n) = serde_json::from_value::<ItemNotification>(params.clone()) {
                if let ThreadItem::AgentMessage { text, .. } = n.item {
                    if !text.trim().is_empty() {
                        *self.last_agent.lock().unwrap() = text;
                    }
                }
            }
        }
        if method == notification::TURN_COMPLETED {
            if let Ok(n) = serde_json::from_value::<TurnNotification>(params) {
                let _ = self.done.send(Some((n.thread_id, n.turn)));
            }
        }
    }

    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        match method {
            server_request::APPROVAL_REQUEST => {
                let p: ApprovalRequestParams = serde_json::from_value(params)?;
                let decision = if self.auto_approve {
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Deny {
                        feedback: Some(
                            "Running non-interactively without --auto-approve; this action was denied.".into(),
                        ),
                    }
                };
                if !self.json {
                    eprintln!(
                        "[approval] {} → {}",
                        describe(&p.approval),
                        if self.auto_approve { "approved" } else { "denied" }
                    );
                }
                Ok(serde_json::to_value(ApprovalResponse { decision, persist: None })?)
            }
            server_request::ELICITATION_REQUEST => Ok(serde_json::json!({"action": "decline"})),
            _ => anyhow::bail!("`{method}` is not available in exec mode"),
        }
    }
}

fn describe(k: &ApprovalKind) -> String {
    match k {
        ApprovalKind::Exec { command, reason, .. } => format!("run `{command}` ({reason})"),
        ApprovalKind::Patch { changes, .. } => format!("edit {} file(s)", changes.len()),
        ApprovalKind::Mcp { server, tool, .. } => format!("MCP {server}.{tool}"),
        ApprovalKind::ComputerUse { app, action, .. } => format!("computer {action} in {app}"),
        ApprovalKind::Browser { site, action, .. } => format!("browser {action} on {site}"),
        ApprovalKind::Download { url, .. } => format!("download {url}"),
        ApprovalKind::Hook { command, .. } => format!("hook {command}"),
    }
}

fn human(method: &str, params: &Value) {
    match method {
        notification::ITEM_STARTED => {
            if let Ok(n) = serde_json::from_value::<ItemNotification>(params.clone()) {
                match n.item {
                    ThreadItem::CommandExecution { command, .. } => eprintln!("$ {command}"),
                    ThreadItem::ToolCall { summary: Some(s), .. } => eprintln!("· {s}"),
                    ThreadItem::ContextCompaction { .. } => eprintln!("· compacting context…"),
                    _ => {}
                }
            }
        }
        notification::ITEM_COMPLETED => {
            if let Ok(n) = serde_json::from_value::<ItemNotification>(params.clone()) {
                match n.item {
                    ThreadItem::AgentMessage { text, .. } if !text.is_empty() => eprintln!("\n{text}\n"),
                    ThreadItem::FileChange { changes, status, .. } => {
                        for c in changes {
                            eprintln!("  {:?} {} (+{} -{}) [{status:?}]", c.kind, c.path, c.additions, c.deletions);
                        }
                    }
                    ThreadItem::CommandExecution { exit_code, .. } => {
                        eprintln!("  (exit {})", exit_code.map(|c| c.to_string()).unwrap_or_else(|| "-".into()))
                    }
                    ThreadItem::ContextCompaction { tokens_before, tokens_after, .. } => {
                        eprintln!("· context compacted {tokens_before} → {tokens_after}")
                    }
                    ThreadItem::Notice { message, .. } => eprintln!("! {message}"),
                    ThreadItem::Error { message, .. } => eprintln!("✗ {message}"),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Run one task; returns the process exit code.
pub async fn run(engine: Engine, args: ExecArgs) -> anyhow::Result<i32> {
    let prompt = if args.prompt == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        s
    } else {
        args.prompt.clone()
    };
    let (done_tx, mut done_rx) = watch::channel(None);
    let sink = Arc::new(ExecSink {
        json: args.json,
        auto_approve: args.auto_approve,
        done: done_tx,
        last_agent: Mutex::new(String::new()),
    });
    engine.set_sink(sink.clone());
    let _bg = engine.start_background(false);
    engine.registry.refresh().await;
    let permission_mode: PermissionMode = serde_json::from_value(Value::String(args.permission_mode.clone()))
        .map_err(|_| anyhow::anyhow!("--permission-mode must be read-only, auto or full-access"))?;
    let effort: Option<ReasoningEffort> = args
        .effort
        .as_ref()
        .map(|e| serde_json::from_value(Value::String(e.clone())))
        .transpose()
        .map_err(|_| anyhow::anyhow!("bad --effort"))?;
    let thread_id = match &args.resume {
        Some(id) => id.clone(),
        None => {
            let cwd =
                args.cwd.clone().unwrap_or_else(|| std::env::current_dir().unwrap().to_string_lossy().to_string());
            let t = api::thread_start(
                &engine,
                ThreadStartParams {
                    cwd: Some(cwd),
                    model: args.model.clone(),
                    effort,
                    permission_mode: Some(permission_mode),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| anyhow::anyhow!(e.message))?;
            t.thread.id
        }
    };
    let resp = odex_core::turn::start_turn(
        &engine,
        TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::text(prompt)],
            mode: Some(if args.plan { TurnMode::Plan } else { TurnMode::Default }),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!(e.message))?;
    let Some(turn) = resp.turn else { anyhow::bail!("turn was queued") };
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(args.timeout);
    let status = loop {
        let finished = done_rx.borrow().clone();
        if let Some((tid, t)) = finished {
            if tid == thread_id && t.id == turn.id {
                break t.status;
            }
        }
        tokio::select! {
            r = done_rx.changed() => if r.is_err() { break TurnStatus::Failed },
            _ = tokio::time::sleep_until(deadline) => {
                if let Ok(rt) = engine.thread(&thread_id) {
                    odex_core::turn::interrupt(&rt);
                }
                eprintln!("timed out after {}s", args.timeout);
                break TurnStatus::Interrupted;
            }
        }
    };
    // goal mode / queued follow-ups may continue; wait until idle
    if let Ok(rt) = engine.thread(&thread_id) {
        while rt.is_running() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    let last = sink.last_agent.lock().unwrap().clone();
    if let Some(p) = &args.output_last_message {
        std::fs::write(p, &last)?;
    }
    if !args.json {
        println!("{last}");
        eprintln!("thread: {thread_id}");
    }
    Ok(if status == TurnStatus::Completed { 0 } else { 1 })
}
