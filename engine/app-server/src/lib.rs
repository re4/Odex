//! The app-server: newline-delimited JSON-RPC 2.0 over stdio (or any
//! AsyncRead/AsyncWrite pair). Requests run concurrently; responses and
//! notifications are serialized through one writer.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use odex_core::{api, EResult, Engine, EngineError, EventSink};
use odex_protocol::jsonrpc::{
    error_codes, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};
use odex_protocol::*;

/// Sink that writes notifications/requests to the client.
pub struct RpcSink {
    out: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<RequestId, oneshot::Sender<Result<Value, String>>>>,
    next_id: AtomicI64,
    caps: RwLock<ClientCapabilities>,
}

impl RpcSink {
    pub fn new(out: mpsc::UnboundedSender<String>) -> Self {
        Self {
            out,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            caps: RwLock::new(ClientCapabilities::default()),
        }
    }

    fn resolve(&self, resp: JsonRpcResponse) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&resp.id) {
            let r = match resp.error {
                Some(e) => Err(e.message),
                None => Ok(resp.result.unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
        }
    }

    fn fail_all(&self) {
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err("client disconnected".into()));
        }
    }
}

#[async_trait]
impl EventSink for RpcSink {
    fn notify(&self, method: &str, params: Value) {
        let n = JsonRpcNotification::new(method, Some(params));
        if let Ok(s) = serde_json::to_string(&n) {
            let _ = self.out.send(s);
        }
    }

    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = RequestId::Num(self.next_id.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id.clone(), tx);
        let req = JsonRpcRequest::new(id, method, Some(params));
        self.out.send(serde_json::to_string(&req)?).map_err(|_| anyhow::anyhow!("client disconnected"))?;
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(anyhow::anyhow!(e)),
            Err(_) => Err(anyhow::anyhow!("client disconnected")),
        }
    }

    fn capabilities(&self) -> ClientCapabilities {
        self.caps.read().unwrap().clone()
    }
}

fn params<T: DeserializeOwned>(v: Option<Value>) -> EResult<T> {
    let v = match v {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(v) => v,
    };
    serde_json::from_value(v).map_err(|e| EngineError::new(error_codes::INVALID_PARAMS, format!("invalid params: {e}")))
}

fn ok<T: serde::Serialize>(v: T) -> EResult<Value> {
    serde_json::to_value(v).map_err(|e| EngineError::new(error_codes::INTERNAL_ERROR, e.to_string()))
}

/// Route one request.
pub async fn dispatch(engine: &Engine, sink: &RpcSink, method: &str, p: Option<Value>) -> EResult<Value> {
    use odex_protocol::method as m;
    match method {
        m::INITIALIZE => {
            let ip: InitializeParams = params(p)?;
            *sink.caps.write().unwrap() = ip.capabilities.clone();
            engine.set_secrets(ip.secrets.clone());
            let settings = engine.user_settings();
            ok(InitializeResponse {
                protocol_version: PROTOCOL_VERSION.into(),
                server_name: SERVER_NAME.into(),
                server_version: env!("CARGO_PKG_VERSION").into(),
                odex_home: engine.home.root().to_string_lossy().to_string(),
                platform: std::env::consts::OS.into(),
                sandbox: api::sandbox_status(engine),
                needs_onboarding: settings.unconfigured,
            })
        }
        m::THREAD_START => ok(api::thread_start(engine, params(p)?).await?),
        m::THREAD_RESUME | m::THREAD_READ => ok(api::thread_read(engine, params(p)?).await?),
        m::THREAD_FORK => ok(api::thread_fork(engine, params(p)?).await?),
        m::THREAD_LIST => ok(api::thread_list(engine, params(p)?)?),
        m::THREAD_SEARCH => ok(api::thread_search(engine, params(p)?)?),
        m::THREAD_ARCHIVE => ok(api::thread_archive(engine, params(p)?).await?),
        m::THREAD_UNARCHIVE => ok(api::thread_unarchive(engine, params(p)?).await?),
        m::THREAD_DELETE => ok(api::thread_delete(engine, params(p)?).await?),
        m::THREAD_ROLLBACK | m::THREAD_REVERT => ok(api::thread_rollback(engine, params(p)?).await?),
        m::THREAD_UPDATE => ok(api::thread_update(engine, params(p)?).await?),
        m::THREAD_COMPACT => ok(api::thread_compact(engine, params(p)?).await?),
        m::THREAD_CONTEXT => ok(api::thread_context(engine, params(p)?).await?),
        m::THREAD_GOAL_SET => ok(api::goal_set(engine, params(p)?).await?),
        m::THREAD_GOAL_CLEAR => ok(api::goal_clear(engine, params(p)?).await?),
        m::THREAD_APPROVE_OVERRIDE => ok(api::approve_override(engine, params(p)?).await?),
        m::THREAD_PLAN_DECIDE => ok(api::plan_decide(engine, params(p)?).await?),
        m::THREAD_INIT_AGENTS_MD => ok(api::init_agents_md(engine, params(p)?).await?),
        m::THREAD_QUEUE_SET => ok(api::queue_set(engine, params(p)?).await?),
        m::THREAD_SHELL_COMMAND => ok(api::shell_command(engine, params(p)?).await?),

        m::TURN_START => ok(odex_core::turn::start_turn(engine, params(p)?).await?),
        m::TURN_STEER => {
            let sp: TurnSteerParams = params(p)?;
            let rt = engine.thread(&sp.thread_id)?;
            odex_core::turn::steer(engine, &rt, sp.input)?;
            ok(EmptyResponse {})
        }
        m::TURN_INTERRUPT => {
            let ip: ThreadIdParams = params(p)?;
            let rt = engine.thread(&ip.thread_id)?;
            odex_core::turn::interrupt(&rt);
            ok(EmptyResponse {})
        }
        m::REVIEW_START => ok(api::review_start(engine, params(p)?).await?),

        m::MODEL_LIST => ok(api::model_list(engine)),
        m::PROVIDER_LIST => ok(api::provider_list(engine, params(p)?).await),
        m::PROVIDER_UPSERT => ok(api::provider_upsert(engine, params(p)?).await?),
        m::PROVIDER_REMOVE => ok(api::provider_remove(engine, params(p)?).await?),
        m::PROVIDER_TEST => ok(api::provider_test(engine, params(p)?).await?),
        m::DOCTOR_RUN => ok(api::doctor_run(engine, params(p)?).await?),
        m::PRESET_LIST => ok(api::preset_list(engine)),

        m::CONFIG_READ => ok(api::config_read(engine)),
        m::CONFIG_WRITE => ok(api::config_write(engine, params(p)?)?),

        m::PROJECT_LIST => ok(api::project_list(engine)?),
        m::PROJECT_ADD => ok(api::project_add(engine, params(p)?).await?),
        m::PROJECT_UPDATE => ok(api::project_update(engine, params(p)?)?),
        m::PROJECT_REMOVE => ok(api::project_remove(engine, params(p)?)?),
        m::TRUST_CHECK => ok(api::trust_check(engine, params(p)?)),
        m::TRUST_SET => ok(api::trust_set(engine, params(p)?)?),
        m::FS_SEARCH => {
            let fp: FileSearchParams = params(p)?;
            let e2 = engine.clone();
            let r = tokio::task::spawn_blocking(move || api::fs_search(&e2, fp))
                .await
                .map_err(|e| EngineError::new(error_codes::INTERNAL_ERROR, e.to_string()))?;
            ok(r)
        }

        m::GIT_STATUS => ok(api::git_status(params(p)?).await?),
        m::GIT_DIFF => ok(api::git_diff(engine, params(p)?).await?),
        m::GIT_STAGE => ok(api::git_stage(params(p)?).await?),
        m::GIT_UNSTAGE => ok(api::git_unstage(params(p)?).await?),
        m::GIT_REVERT => ok(api::git_revert(params(p)?).await?),
        m::GIT_COMMIT => ok(api::git_commit(params(p)?).await?),
        m::GIT_COMMIT_MESSAGE => ok(api::git_commit_message(engine, params(p)?).await?),
        m::GIT_PUSH => ok(api::git_push(params(p)?).await?),
        m::GIT_BRANCHES => ok(api::git_branches(params(p)?).await?),
        m::GIT_LOG => ok(api::git_log(params(p)?).await?),
        m::WORKTREE_HANDOFF => ok(api::worktree_handoff(engine, params(p)?).await?),
        m::WORKTREE_LIST => ok(api::worktree_list(engine).await?),
        m::WORKTREE_REMOVE => ok(api::worktree_remove(engine, params(p)?).await?),
        m::PR_CREATE => ok(api::pr_create(engine, params(p)?).await?),
        m::PR_VIEW => ok(api::pr_view(engine, params(p)?).await?),
        m::PR_COMMENT => ok(api::pr_comment(engine, params(p)?).await?),
        m::PR_DRAFT => ok(api::pr_draft(engine, params(p)?).await?),

        m::EXEC_SESSIONS => ok(api::exec_sessions(engine)),
        m::EXEC_KILL => ok(api::exec_kill(engine, params(p)?).await?),

        m::MCP_LIST => ok(api::mcp_list(engine)),
        m::MCP_UPSERT => ok(api::mcp_upsert(engine, params(p)?).await?),
        m::MCP_REMOVE => ok(api::mcp_remove(engine, params(p)?).await?),
        m::MCP_RESTART => ok(api::mcp_restart(engine, params(p)?).await?),
        m::MCP_LOGS => ok(api::mcp_logs(engine, params(p)?)),
        m::MCP_LOGIN => ok(api::mcp_login(engine, params(p)?).await?),
        m::MCP_LOGOUT => ok(api::mcp_logout(engine, params(p)?).await?),
        m::MCP_READ_RESOURCE => ok(api::mcp_read_resource(engine, params(p)?).await?),

        m::SKILLS_LIST => ok(api::skills_list(engine, params(p)?)),
        m::SKILLS_READ => ok(api::skills_read(engine, params(p)?)?),
        m::SKILLS_WRITE => ok(api::skills_write(engine, params(p)?)?),
        m::SKILLS_DELETE => ok(api::skills_delete(engine, params(p)?)?),
        m::SKILLS_IMPORT => ok(api::skills_import(engine, params(p)?).await?),
        m::SKILLS_SET_ENABLED => ok(api::skills_set_enabled(engine, params(p)?)?),
        m::PLUGINS_LIST => ok(api::plugins_list(engine)),
        m::PLUGINS_INSTALL => ok(api::plugins_install(engine, params(p)?).await?),
        m::PLUGINS_TRUST => ok(api::plugins_trust(engine, params(p)?).await?),
        m::PLUGINS_REMOVE => ok(api::plugins_remove(engine, params(p)?).await?),
        m::PLUGINS_SET_ENABLED => ok(api::plugins_set_enabled(engine, params(p)?).await?),
        m::HOOKS_LIST => ok(api::hooks_list(engine, params(p)?)),
        m::HOOKS_TRUST => ok(api::hooks_trust(engine, params(p)?)?),

        m::AUTOMATION_LIST => ok(api::automation_list(engine)?),
        m::AUTOMATION_UPSERT => ok(api::automation_upsert(engine, params(p)?)?),
        m::AUTOMATION_DELETE => ok(api::automation_delete(engine, params(p)?)?),
        m::AUTOMATION_RUN_NOW => ok(api::automation_run_now(engine, params(p)?)?),
        m::AUTOMATION_RUNS => ok(api::automation_runs(engine, params(p)?)?),
        m::AUTOMATION_RUNS_MARK_READ => ok(api::automation_mark_read(engine, params(p)?)?),
        m::AUTOMATION_RUNS_ARCHIVE => ok(api::automation_archive(engine, params(p)?)?),
        m::AUTOMATION_VALIDATE_SCHEDULE => ok(api::automation_validate(params(p)?)),

        m::MEMORY_LIST => ok(api::memory_list(engine, params(p)?)),
        m::MEMORY_UPSERT => ok(api::memory_upsert(engine, params(p)?)?),
        m::MEMORY_DELETE => ok(api::memory_delete(engine, params(p)?)?),
        m::MEMORY_PROPOSE => ok(api::memory_propose(engine, params(p)?).await?),
        m::USAGE_STATS => ok(api::usage_stats(engine, params(p)?)?),

        m::COMPUTER_USE_STATUS => ok(api::computer_status(engine)),
        m::COMPUTER_USE_WINDOWS => ok(api::computer_windows(engine).await?),
        m::COMPUTER_USE_KILL_SWITCH => ok(api::kill_switch(engine, params(p)?)),
        m::APPSHOT_CAPTURE => ok(api::appshot(params(p)?).await?),
        m::SANDBOX_STATUS => ok(api::sandbox_status(engine)),
        other => Err(EngineError::new(error_codes::METHOD_NOT_FOUND, format!("unknown method `{other}`"))),
    }
}

/// Serve JSON-RPC on the given reader/writer until the reader closes.
pub async fn serve<R, W>(engine: Engine, reader: R, mut writer: W) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let sink = Arc::new(RpcSink::new(tx.clone()));
    engine.set_sink(sink.clone());
    let writer_task = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err() || writer.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = writer.flush().await;
        }
    });
    let initialized = Arc::new(AtomicBool::new(false));
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg = match JsonRpcMessage::parse(&line) {
            Ok(m) => m,
            Err(e) => {
                let r = JsonRpcResponse::err(
                    RequestId::Num(0),
                    error_codes::PARSE_ERROR,
                    format!("parse error: {e}"),
                    None,
                );
                let _ = tx.send(serde_json::to_string(&r)?);
                continue;
            }
        };
        match msg {
            JsonRpcMessage::Response(r) => sink.resolve(r),
            JsonRpcMessage::Notification(_) => {} // `initialized`, cancellations: nothing to do
            JsonRpcMessage::Request(req) => {
                let engine = engine.clone();
                let sink = sink.clone();
                let tx = tx.clone();
                let initialized = initialized.clone();
                tokio::spawn(async move {
                    let is_init = req.method == odex_protocol::method::INITIALIZE;
                    let result = if !is_init && !initialized.load(Ordering::SeqCst) {
                        Err(EngineError::new(error_codes::NOT_INITIALIZED, "Not initialized: call `initialize` first"))
                    } else {
                        dispatch(&engine, &sink, &req.method, req.params).await
                    };
                    if is_init && result.is_ok() {
                        initialized.store(true, Ordering::SeqCst);
                    }
                    let resp = match result {
                        Ok(v) => JsonRpcResponse::ok(req.id, v),
                        Err(e) => JsonRpcResponse::err(req.id, e.code, e.message, None),
                    };
                    if let Ok(s) = serde_json::to_string(&resp) {
                        let _ = tx.send(s);
                    }
                });
            }
        }
    }
    // client went away: stop running turns, fail pending requests
    sink.fail_all();
    for rt in engine.threads.lock().unwrap().values() {
        odex_core::turn::interrupt(rt);
    }
    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer_task).await;
    Ok(())
}

pub async fn run_stdio(engine: Engine) -> anyhow::Result<()> {
    serve(engine, tokio::io::stdin(), tokio::io::stdout()).await
}
