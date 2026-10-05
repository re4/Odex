//! Opt-in smoke suite against a real vLLM (or other Chat Completions) endpoint (PROMPT §12).
//!
//! Every test passes immediately, printing a "skipped" line, unless `ODEX_E2E_BASE_URL` is set:
//!
//! ```text
//! ODEX_E2E_BASE_URL=http://gpu-box:8000/v1 \
//! ODEX_E2E_MODEL=Qwen/Qwen3-Coder-30B-A3B-Instruct \  # optional; default: first model on /v1/models
//! ODEX_E2E_API_KEY=...                                # optional; the server's --api-key
//! ODEX_E2E_TIMEOUT_SECS=600                           # optional; per-turn timeout
//!   cargo test -p odex-core --test real_vllm -- --nocapture --test-threads=1
//! ```
//!
//! Tests: endpoint discovery plus a quick Doctor run, a real agent task (write a file, print it with a
//! shell command), fixing a failing Node test (skipped without `node`), an MCP round-trip through the
//! `odex-mcp-test-server` binary (skipped unless it is built: `cargo build -p odex-mcp-client --bins`,
//! or point `ODEX_E2E_MCP_SERVER` at it), and compaction on a model whose window is overridden to 4,096
//! tokens.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::TestSink;
use odex_config::OdexHome;
use odex_core::{Engine, EngineOptions};
use odex_protocol::*;

/// Provider id and model key used in the generated config.
const PROVIDER: &str = "e2e";
const MODEL_KEY: &str = "e2e";

struct E2e {
    base_url: String,
    model: Option<String>,
    api_key: bool,
}

/// The endpoint under test, or `None` (and a skip message) when `ODEX_E2E_BASE_URL` is unset.
fn e2e(test: &str) -> Option<E2e> {
    let Some(raw) = std::env::var("ODEX_E2E_BASE_URL").ok().filter(|s| !s.trim().is_empty()) else {
        eprintln!("real_vllm::{test}: skipped (set ODEX_E2E_BASE_URL to run against a real server)");
        return None;
    };
    let mut base_url = raw.trim().trim_end_matches('/').to_string();
    // `http://host:8000` means the vLLM root; the API routes live under /v1.
    if url::Url::parse(&base_url).map(|u| u.path() == "/" || u.path().is_empty()).unwrap_or(false) {
        base_url.push_str("/v1");
    }
    Some(E2e {
        base_url,
        model: std::env::var("ODEX_E2E_MODEL").ok().filter(|s| !s.trim().is_empty()),
        api_key: std::env::var("ODEX_E2E_API_KEY").map(|k| !k.is_empty()).unwrap_or(false),
    })
}

fn turn_timeout() -> Duration {
    let secs = std::env::var("ODEX_E2E_TIMEOUT_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(600);
    Duration::from_secs(secs)
}

fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

#[derive(Default)]
struct Opts {
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
    /// Extra TOML appended to the config (tables only).
    extra: String,
}

struct Real {
    engine: Engine,
    sink: Arc<TestSink>,
    _home: tempfile::TempDir,
    work: tempfile::TempDir,
    _bg: tokio_util::sync::CancellationToken,
}

fn config(e: &E2e, model: Option<&str>, work: &Path, opts: &Opts) -> String {
    let mut c = String::new();
    if model.is_some() {
        c.push_str(&format!("model = {}\n", toml_str(MODEL_KEY)));
    }
    c.push_str("[features]\nfollow_up_suggestions = false\nauto_title = false\n");
    c.push_str(&format!(
        "[model_providers.{PROVIDER}]\nname = \"Real vLLM (e2e)\"\nbase_url = {}\n",
        toml_str(&e.base_url)
    ));
    if e.api_key {
        c.push_str("api_key_env = \"ODEX_E2E_API_KEY\"\n");
    }
    if let Some(m) = model {
        c.push_str(&format!("[models.{MODEL_KEY}]\nprovider = \"{PROVIDER}\"\nmodel = {}\n", toml_str(m)));
        if let Some(w) = opts.context_window {
            c.push_str(&format!("context_window = {w}\n"));
        }
        if let Some(m) = opts.max_output_tokens {
            c.push_str(&format!("max_output_tokens = {m}\n"));
        }
    }
    c.push_str(&opts.extra);
    c.push_str(&format!("\n[projects.'{}']\ntrust_level = \"trusted\"\n", work.display()));
    c
}

async fn start(e: &E2e, model: Option<&str>, opts: Opts) -> Real {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), config(e, model, work.path(), &opts)).unwrap();
    let engine = Engine::new(EngineOptions { home: OdexHome::at(home.path()), profile: None }).unwrap();
    let sink = TestSink::new(); // approves every request
    engine.set_sink(sink.clone());
    let bg = engine.start_background(false);
    engine.registry.refresh().await;
    Real { engine, sink, _home: home, work, _bg: bg }
}

/// `ODEX_E2E_MODEL`, else the first model the endpoint serves.
async fn pick_model(e: &E2e) -> String {
    if let Some(m) = &e.model {
        return m.clone();
    }
    let probe = start(e, None, Opts::default()).await;
    let providers = probe.engine.registry.providers();
    let p = providers.iter().find(|p| p.id == PROVIDER).expect("e2e provider is configured");
    assert!(p.error.is_none(), "{} is not reachable: {:?}", e.base_url, p.error);
    p.models.first().unwrap_or_else(|| panic!("{} serves no models", e.base_url)).id.clone()
}

impl Real {
    async fn thread(&self, mode: PermissionMode, effort: Option<ReasoningEffort>) -> String {
        let t = odex_core::api::thread_start(
            &self.engine,
            ThreadStartParams {
                cwd: Some(self.work.path().to_string_lossy().to_string()),
                permission_mode: Some(mode),
                effort,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        t.thread.id
    }

    async fn run(&self, thread_id: &str, text: &str) -> Turn {
        let r = odex_core::turn::start_turn(
            &self.engine,
            TurnStartParams { thread_id: thread_id.into(), input: vec![UserInput::text(text)], ..Default::default() },
        )
        .await
        .unwrap();
        let turn_id = r.turn.expect("turn started immediately").id;
        let rt = self.engine.thread(thread_id).unwrap();
        let timeout = turn_timeout();
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(t) = rt.turns.lock().unwrap().iter().find(|t| t.id == turn_id) {
                if t.status != TurnStatus::InProgress && !rt.is_running() {
                    return t.clone();
                }
            }
            if Instant::now() > deadline {
                odex_core::turn::interrupt(&rt);
                self.dump();
                panic!("turn did not finish within {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Print a one-line-per-item transcript (visible with `--nocapture`).
    fn dump(&self) {
        for item in self.sink.items() {
            let line = match &item {
                ThreadItem::UserMessage { .. } => "user message".to_string(),
                ThreadItem::AgentMessage { text, .. } => format!("agent: {}", short(text)),
                ThreadItem::Reasoning { text, .. } => format!("reasoning: {} chars", text.len()),
                ThreadItem::CommandExecution { command, exit_code, output, .. } => {
                    format!("$ {command} -> exit {exit_code:?}: {}", short(output))
                }
                ThreadItem::FileChange { changes, status, .. } => {
                    format!("file change {:?}: {:?}", status, changes.iter().map(|c| &c.path).collect::<Vec<_>>())
                }
                ThreadItem::ToolCall { tool, status, summary, .. } => format!("tool {tool} {status:?} {summary:?}"),
                ThreadItem::McpToolCall { server, tool, status, error, .. } => {
                    format!("mcp {server}.{tool} {status:?} {error:?}")
                }
                ThreadItem::ContextCompaction { tokens_before, tokens_after, llm, trigger, .. } => {
                    format!("compaction {tokens_before} -> {tokens_after} (llm={llm}, {trigger:?})")
                }
                ThreadItem::Notice { message, .. } => format!("notice: {}", short(message)),
                ThreadItem::Error { message, .. } => format!("error: {}", short(message)),
                other => format!("{:?}", std::mem::discriminant(other)),
            };
            eprintln!("  · {line}");
        }
    }

    fn assert_completed(&self, turn: &Turn) {
        if turn.status != TurnStatus::Completed {
            self.dump();
            panic!("turn ended {:?}: {:?}", turn.status, turn.error);
        }
    }
}

fn short(s: &str) -> String {
    let one = s.replace(['\r', '\n'], " ");
    let t: String = one.chars().take(160).collect();
    if one.chars().count() > 160 {
        format!("{t}…")
    } else {
        t
    }
}

fn unique_token() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("odex-smoke-{:x}", n & 0xffff_ffff)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovery_and_doctor() {
    let Some(e) = e2e("discovery_and_doctor") else { return };
    let model = pick_model(&e).await;
    let r = start(&e, Some(&model), Opts::default()).await;

    let providers = r.engine.registry.providers();
    let p = providers.iter().find(|p| p.id == PROVIDER).unwrap();
    eprintln!(
        "endpoint {} health={:?} version={:?} models={:?}",
        p.base_url,
        p.health,
        p.version,
        p.models.iter().map(|m| (&m.id, m.max_model_len)).collect::<Vec<_>>()
    );
    assert_ne!(p.health, EndpointHealth::Unreachable, "{:?}", p.error);
    let served = p.models.iter().find(|m| m.id == model).unwrap_or_else(|| panic!("`{model}` is not served"));
    assert!(served.max_model_len.unwrap_or(0) > 0, "/v1/models did not report max_model_len for `{model}`");

    let res = odex_core::api::doctor_run(
        &r.engine,
        DoctorRunParams { provider_id: None, model: Some(MODEL_KEY.into()), quick: true },
    )
    .await
    .unwrap();
    let rep = res.reports.first().expect("one Doctor report");
    eprintln!("Doctor: {} on {} (vLLM {:?})", rep.model_id, rep.base_url, rep.server_version);
    for c in &rep.checks {
        eprintln!("  [{:?}] {:<32} {}", c.status, c.name, c.detail);
    }
    if let Some(cmd) = &rep.suggested_command {
        eprintln!("  suggested: {cmd}");
    }
    for id in ["connect", "streaming", "toolCall"] {
        let c = rep.checks.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("Doctor did not run `{id}`"));
        assert_ne!(c.status, CheckStatus::Fail, "Doctor `{id}` failed: {}", c.detail);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_writes_file_and_runs_command() {
    let Some(e) = e2e("agent_writes_file_and_runs_command") else { return };
    let model = pick_model(&e).await;
    let r = start(&e, Some(&model), Opts::default()).await;
    let token = unique_token();
    let tid = r.thread(PermissionMode::Auto, None).await;
    let turn = r
        .run(
            &tid,
            &format!(
                "Create a file named `odex_smoke.txt` in the current working directory whose entire content is \
                 exactly `{token}` (no quotes, nothing else). Then run a shell command that prints the contents \
                 of `odex_smoke.txt`. Finish with one short sentence."
            ),
        )
        .await;
    r.dump();
    r.assert_completed(&turn);

    let path = r.work.path().join("odex_smoke.txt");
    assert!(path.exists(), "odex_smoke.txt was not created");
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content.trim(), token, "unexpected file content");
    let printed = r.sink.items().iter().any(|i| {
        matches!(i, ThreadItem::CommandExecution { output, status: ItemStatus::Completed, .. } if output.contains(&token))
    });
    assert!(printed, "no completed shell command printed the file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_fixes_failing_node_test() {
    let Some(e) = e2e("agent_fixes_failing_node_test") else { return };
    let node_ok = std::process::Command::new("node").arg("--version").output().map(|o| o.status.success());
    if !node_ok.unwrap_or(false) {
        eprintln!("real_vllm::agent_fixes_failing_node_test: skipped (node is not on PATH)");
        return;
    }
    let model = pick_model(&e).await;
    let r = start(&e, Some(&model), Opts::default()).await;
    let test_js = "const assert = require('assert');\nconst sum = require('./sum.js');\n\
                   assert.strictEqual(sum(2, 3), 5);\nassert.strictEqual(sum(-4, 4), 0);\nconsole.log('all tests passed');\n";
    std::fs::write(r.work.path().join("test.js"), test_js).unwrap();
    std::fs::write(r.work.path().join("sum.js"), "module.exports = function sum(a, b) {\n  return a - b;\n};\n")
        .unwrap();

    let tid = r.thread(PermissionMode::Auto, None).await;
    let turn = r
        .run(
            &tid,
            "The test in this folder fails. Run `node test.js` to see the failure, fix the bug in sum.js \
             (do not modify test.js), then run `node test.js` again to confirm that it passes.",
        )
        .await;
    r.dump();
    r.assert_completed(&turn);

    assert_eq!(std::fs::read_to_string(r.work.path().join("test.js")).unwrap(), test_js, "test.js was modified");
    let out = std::process::Command::new("node").arg("test.js").current_dir(r.work.path()).output().unwrap();
    assert!(
        out.status.success(),
        "node test.js still fails: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ODEX_E2E_MCP_SERVER`, else the test server next to this test binary (target/<profile>/).
fn mcp_test_server() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ODEX_E2E_MCP_SERVER") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?;
    let p = dir.join(format!("odex-mcp-test-server{}", std::env::consts::EXE_SUFFIX));
    p.exists().then_some(p)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_round_trip() {
    let Some(e) = e2e("mcp_round_trip") else { return };
    let Some(server) = mcp_test_server() else {
        eprintln!(
            "real_vllm::mcp_round_trip: skipped (build it with `cargo build -p odex-mcp-client --bins` or set ODEX_E2E_MCP_SERVER)"
        );
        return;
    };
    let model = pick_model(&e).await;
    let extra = format!(
        "\n[mcp_servers.calc]\ncommand = '{}'\nenabled_tools = [\"add\", \"echo\"]\nauto_approve_tools = [\"add\"]\n",
        server.display()
    );
    let r = start(&e, Some(&model), Opts { extra, ..Default::default() }).await;

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let list = odex_core::api::mcp_list(&r.engine);
        let calc = list.servers.iter().find(|s| s.name == "calc");
        match calc.map(|s| s.state) {
            Some(McpServerState::Ready) => break,
            Some(McpServerState::Failed) => panic!("MCP server failed: {:?}", calc.and_then(|s| s.error.clone())),
            _ if Instant::now() > deadline => panic!("MCP server did not start: {calc:?}"),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }

    let tid = r.thread(PermissionMode::Auto, None).await;
    let turn = r
        .run(&tid, "Use the `add` tool from the `calc` MCP server to add 1234 and 4321. Reply with just the result.")
        .await;
    r.dump();
    r.assert_completed(&turn);
    let items = r.sink.items();
    let called = items.iter().any(|i| {
        matches!(i, ThreadItem::McpToolCall { server, tool, status: ItemStatus::Completed, .. } if server == "calc" && tool == "add")
    });
    assert!(called, "the model did not call calc.add");
    let answered = items.iter().any(|i| matches!(i, ThreadItem::AgentMessage { text, .. } if text.contains("5555")));
    assert!(answered, "the final answer does not contain 5555");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn small_window_compacts_without_overflow() {
    let Some(e) = e2e("small_window_compacts_without_overflow") else { return };
    let model = pick_model(&e).await;
    let r = start(
        &e,
        Some(&model),
        Opts { context_window: Some(4096), max_output_tokens: Some(1024), ..Default::default() },
    )
    .await;
    let tid = r.thread(PermissionMode::Auto, Some(ReasoningEffort::None)).await;

    let mut compacted_at: Option<usize> = None;
    for i in 1..=14 {
        let errors = (i % 3) + 1;
        let mut chunk = String::new();
        for l in 0..24 {
            let level = if l < errors { "ERROR" } else { "INFO" };
            chunk.push_str(&format!("[{level}] build step {i}.{l}: compiled module_{l:02} in {} ms\n", 40 + l * 7));
        }
        let msg = format!(
            "Here is log chunk {i} of a long build log. Do not use any tools. Count the lines that start with \
             [ERROR] and reply with one line in the form `chunk {i}: N errors`.\n\n{chunk}"
        );
        let turn = r.run(&tid, &msg).await;
        if turn.error.as_ref().and_then(|e| e.code.as_deref()) == Some("contextOverflow") {
            r.dump();
            panic!("turn {i} hit a context overflow: {:?}", turn.error);
        }
        r.assert_completed(&turn);

        let ctx =
            odex_core::api::thread_context(&r.engine, ThreadIdParams { thread_id: tid.clone() }).await.unwrap().context;
        eprintln!("turn {i}: context {} / {} tokens, {} compaction(s)", ctx.used, ctx.window, ctx.compactions.len());
        assert_eq!(ctx.window, 4096, "the context_window override was not applied");
        assert!(ctx.used <= ctx.window, "context {} exceeds the window {}", ctx.used, ctx.window);
        if !ctx.compactions.is_empty() && compacted_at.is_none() {
            compacted_at = Some(i);
        }
        // Run two more turns after the first compaction to show the thread keeps working.
        if compacted_at.is_some_and(|c| i >= c + 2) {
            break;
        }
    }
    r.dump();
    let at = compacted_at.expect("no compaction happened within 14 turns on a 4,096-token window");
    let mut compactions = 0;
    for item in r.sink.items() {
        if let ThreadItem::ContextCompaction {
            status: ItemStatus::Completed, tokens_before, tokens_after, llm, ..
        } = item
        {
            compactions += 1;
            if !llm {
                eprintln!(
                    "warning: compaction used the extractive fallback (the compactor's structured output failed)"
                );
            }
            if tokens_after >= tokens_before {
                eprintln!("warning: compaction did not shrink the context ({tokens_before} -> {tokens_after})");
            }
        }
    }
    assert!(compactions >= 1, "no completed compaction item");
    eprintln!("first compaction after turn {at}; {compactions} compaction item(s)");
}
