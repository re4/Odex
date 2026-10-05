//! End-to-end tests of the vLLM client against the mock server.

use std::time::Duration;

use odex_config::{Presets, ResolvedModel, ResolvedProvider};
use odex_llm::types::*;
use odex_llm::{LlmClient, LlmError};
use odex_mock_vllm::{MockCall, MockConfig, MockModel, MockReply, MockServer};
use odex_protocol::config_types::ModelProviderToml;
use serde_json::json;
use tokio_util::sync::CancellationToken;

async fn setup(max_len: u32) -> (MockServer, LlmClient, ResolvedModel) {
    let server = MockServer::start(MockConfig {
        models: vec![MockModel { id: "mock-coder".into(), max_model_len: max_len }],
        ..Default::default()
    })
    .await;
    let p = ModelProviderToml {
        base_url: Some(server.url.clone()),
        stream_idle_timeout_ms: Some(400),
        request_max_retries: Some(3),
        stream_max_retries: Some(3),
        ..Default::default()
    };
    let client = LlmClient::new(ResolvedProvider::from_toml("mock", &p), None);
    let model = ResolvedModel::discovered("mock", "mock-coder", &Presets::builtin());
    (server, client, model)
}

fn tools() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "read_file".into(),
        description: "Read a file".into(),
        parameters: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}),
    }]
}

fn req(text: &str) -> ChatRequest {
    ChatRequest { messages: vec![ChatMessage::user(text)], tools: tools(), max_tokens: Some(256), ..Default::default() }
}

async fn run(
    client: &LlmClient,
    model: &ResolvedModel,
    r: &ChatRequest,
) -> (Result<ChatResponse, LlmError>, Vec<ChatEvent>) {
    let mut events = Vec::new();
    let mut sink = |e: ChatEvent| events.push(e);
    let res = client.stream_chat(model, r, &CancellationToken::new(), &mut sink).await;
    (res, events)
}

#[tokio::test]
async fn streams_text_with_usage() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::text("Hello from the mock server, streaming in chunks."));
    let (res, events) = run(&client, &model, &req("hi")).await;
    let r = res.unwrap();
    assert_eq!(r.content, "Hello from the mock server, streaming in chunks.");
    assert!(events.iter().filter(|e| matches!(e, ChatEvent::ContentDelta(_))).count() > 3);
    let u = r.usage.unwrap();
    assert!(u.input_tokens > 0 && u.output_tokens > 0);
    let body = server.last_request().unwrap().body;
    assert_eq!(body["stream_options"]["include_usage"], json!(true));
    assert_eq!(body["tool_choice"], json!("auto"));
}

#[tokio::test]
async fn reasoning_field_and_think_tags() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::Reasoning { reasoning: "Let me think.".into(), text: "Answer.".into() });
    let r = run(&client, &model, &req("q")).await.0.unwrap();
    assert_eq!(r.reasoning, "Let me think.");
    assert_eq!(r.content, "Answer.");

    server.push(MockReply::ThinkInContent { reasoning: "inline thoughts".into(), text: "Final.".into() });
    let r = run(&client, &model, &req("q")).await.0.unwrap();
    assert_eq!(r.reasoning, "inline thoughts");
    assert_eq!(r.content, "Final.");
}

#[tokio::test]
async fn native_parallel_tool_calls() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::tools(vec![("read_file", json!({"path":"a.rs"})), ("read_file", json!({"path":"b.rs"}))]));
    let (res, events) = run(&client, &model, &req("read both")).await;
    let r = res.unwrap();
    assert_eq!(r.tool_calls.len(), 2);
    assert_eq!(r.tool_calls[1].arguments, json!({"path":"b.rs"}).to_string());
    assert!(!r.fallback_parsed);
    assert_eq!(r.finish_reason, Some(FinishReason::ToolCalls));
    assert_eq!(events.iter().filter(|e| matches!(e, ChatEvent::ToolCallStart { .. })).count(), 2);
}

#[tokio::test]
async fn fallback_tool_call_in_content() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::ContentToolCall {
        content:
            "<tool_call>\n<function=read_file>\n<parameter=path>\nsrc/main.rs\n</parameter>\n</function>\n</tool_call>"
                .into(),
    });
    let r = run(&client, &model, &req("read")).await.0.unwrap();
    assert!(r.fallback_parsed);
    assert_eq!(r.tool_calls[0].name, "read_file");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&r.tool_calls[0].arguments).unwrap(),
        json!({"path":"src/main.rs"})
    );
}

#[tokio::test]
async fn malformed_args_pass_through_for_repair() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::ToolCalls {
        calls: vec![MockCall {
            name: "read_file".into(),
            arguments: json!({}),
            raw_arguments: Some("{\"path\": \"x.rs\",".into()),
        }],
        text: None,
    });
    let r = run(&client, &model, &req("read")).await.0.unwrap();
    let (v, repaired) = odex_llm::repair::parse_lenient(&r.tool_calls[0].arguments).unwrap();
    assert!(repaired);
    assert_eq!(v, json!({"path":"x.rs"}));
}

#[tokio::test]
async fn retries_503_and_429_then_succeeds() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::Error { status: 503, body: "overloaded".into(), retry_after: None });
    server.push(MockReply::Error { status: 429, body: "slow down".into(), retry_after: Some(0) });
    server.push(MockReply::text("finally"));
    let (res, events) = run(&client, &model, &req("q")).await;
    assert_eq!(res.unwrap().content, "finally");
    assert_eq!(events.iter().filter(|e| matches!(e, ChatEvent::Retrying { .. })).count(), 2);
    assert_eq!(server.request_count(), 3);
}

#[tokio::test]
async fn mid_stream_disconnect_is_retried() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::Disconnect {
        after_chunks: 3,
        partial: Box::new(MockReply::text("partial output that will be cut")),
    });
    server.push(MockReply::text("complete output"));
    let (res, events) = run(&client, &model, &req("q")).await;
    assert_eq!(res.unwrap().content, "complete output");
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Retrying { .. })));
}

#[tokio::test]
async fn idle_stream_times_out_and_retries() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::Stall { ms: 2000 });
    server.push(MockReply::text("after stall"));
    let t0 = std::time::Instant::now();
    let (res, _) = run(&client, &model, &req("q")).await;
    assert_eq!(res.unwrap().content, "after stall");
    assert!(t0.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let (server, client, model) = setup(32768).await;
    for _ in 0..10 {
        server.push(MockReply::Error { status: 503, body: "down".into(), retry_after: Some(0) });
    }
    let (res, _) = run(&client, &model, &req("q")).await;
    assert!(matches!(res, Err(LlmError::Exhausted { .. })), "{res:?}");
}

#[tokio::test]
async fn context_overflow_is_parsed() {
    let (server, client, model) = setup(512).await;
    server.push(MockReply::text("never"));
    let big = "word ".repeat(1000);
    let (res, _) = run(&client, &model, &req(&big)).await;
    match res {
        Err(LlmError::ContextOverflow(o)) => {
            assert_eq!(o.max_context, Some(512));
            assert!(o.prompt_tokens.unwrap() > 512);
            assert!(!o.max_tokens_only);
        }
        other => panic!("expected overflow, got {other:?}"),
    }
}

#[tokio::test]
async fn max_tokens_only_overflow() {
    let (_server, client, model) = setup(1024).await;
    let mut r = req("short");
    r.max_tokens = Some(1000);
    let (res, _) = run(&client, &model, &r).await;
    match res {
        Err(LlmError::ContextOverflow(o)) => assert!(o.max_tokens_only),
        other => panic!("expected overflow, got {other:?}"),
    }
}

#[tokio::test]
async fn unauthorized_and_bad_model() {
    let server = MockServer::start(MockConfig { api_key: Some("secret".into()), ..Default::default() }).await;
    let p = ModelProviderToml { base_url: Some(server.url.clone()), ..Default::default() };
    let client = LlmClient::new(ResolvedProvider::from_toml("mock", &p), None);
    let model = ResolvedModel::discovered("mock", "mock-coder", &Presets::builtin());
    let (res, _) = run(&client, &model, &req("q")).await;
    assert!(matches!(res, Err(LlmError::Unauthorized(_))), "{res:?}");
    client.set_api_key(Some("secret".into()));
    server.push(MockReply::text("ok"));
    assert_eq!(run(&client, &model, &req("q")).await.0.unwrap().content, "ok");
    let other = ResolvedModel::discovered("mock", "nope", &Presets::builtin());
    assert!(matches!(run(&client, &other, &req("q")).await.0, Err(LlmError::ModelNotFound(_))));
}

#[tokio::test]
async fn cancellation_stops_request() {
    let (server, client, model) = setup(32768).await;
    server.push(MockReply::Stall { ms: 5000 });
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c2.cancel();
    });
    let mut sink = |_e: ChatEvent| {};
    let res = client.stream_chat(&model, &req("q"), &cancel, &mut sink).await;
    assert!(matches!(res, Err(LlmError::Cancelled)));
}

#[tokio::test]
async fn discovery_and_tokenize() {
    let (server, client, _model) = setup(4096).await;
    let p = odex_llm::discovery::probe(&client).await;
    assert!(p.reachable);
    assert_eq!(p.healthy, Some(true));
    assert_eq!(p.models[0].max_model_len, Some(4096));
    assert!(p.version.unwrap().contains("mock"));
    let n = odex_llm::discovery::tokenize_messages(&client, "mock-coder", &[ChatMessage::user("hello world")], None)
        .await
        .unwrap();
    assert!(n > 0);
    drop(server);
}

#[tokio::test]
async fn concurrency_queue_limits_in_flight() {
    let server = MockServer::start(MockConfig { chunk_delay: Duration::from_millis(20), ..Default::default() }).await;
    let p = ModelProviderToml {
        base_url: Some(server.url.clone()),
        max_concurrent_requests: Some(2),
        ..Default::default()
    };
    let client = std::sync::Arc::new(LlmClient::new(ResolvedProvider::from_toml("mock", &p), None));
    let model = ResolvedModel::discovered("mock", "mock-coder", &Presets::builtin());
    let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut handles = vec![];
    for _ in 0..6 {
        let c = client.clone();
        let m = model.clone();
        let ms = max_seen.clone();
        handles.push(tokio::spawn(async move {
            let mut sink = |_e: ChatEvent| {
                let cur = c.stats.in_flight.load(std::sync::atomic::Ordering::Relaxed);
                ms.fetch_max(cur, std::sync::atomic::Ordering::Relaxed);
            };
            c.stream_chat(&m, &req("hello there"), &CancellationToken::new(), &mut sink).await.unwrap();
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert!(max_seen.load(std::sync::atomic::Ordering::Relaxed) <= 2);
    assert_eq!(server.request_count(), 6);
}

#[tokio::test]
async fn doctor_against_mock() {
    use odex_config::{OdexHome, Settings};
    use odex_protocol::config_types::ConfigToml;
    let server = MockServer::start(MockConfig::default()).await;
    // Script replies for each Doctor probe in order.
    server.set_policy(|r| {
        let text = r.last_user_text();
        if r.structured_name().is_some() {
            MockReply::Json { value: json!({"answer": "Paris", "confidence": 0.99}) }
        } else if text.contains("Paris and in Tokyo") {
            MockReply::tools(vec![("get_weather", json!({"city":"Paris"})), ("get_weather", json!({"city":"Tokyo"}))])
        } else if text.contains("weather in Paris") {
            MockReply::tool("get_weather", json!({"city":"Paris"}))
        } else if text.contains("What single color") {
            MockReply::text("Red")
        } else {
            MockReply::text("OK")
        }
    });
    let mut cfg = ConfigToml::default();
    cfg.model_providers
        .insert("mock".into(), ModelProviderToml { base_url: Some(server.url.clone()), ..Default::default() });
    let home = OdexHome::at(tempfile::tempdir().unwrap().path());
    let settings = Settings::resolve(&cfg, &Presets::builtin(), &home);
    let reg = odex_llm::ModelRegistry::new(settings, Presets::builtin(), None);
    reg.refresh().await;
    let h = reg.resolve("mock-coder").expect("resolves discovered model");
    assert_eq!(h.context_window, 32768);
    let report = odex_llm::doctor::run(&reg, &h, false).await;
    let status = |id: &str| report.checks.iter().find(|c| c.id == id).map(|c| c.status);
    use odex_protocol::CheckStatus::*;
    assert_eq!(status("connect"), Some(Pass));
    assert_eq!(status("streaming"), Some(Pass));
    assert_eq!(status("toolCall"), Some(Pass));
    assert_eq!(status("streamedToolCall"), Some(Pass));
    assert_eq!(status("parallelToolCalls"), Some(Pass));
    assert_eq!(status("vision"), Some(Pass));
    assert_eq!(status("tokenize"), Some(Pass));
    assert_eq!(status("structuredOutput"), Some(Pass));
    assert!(report.inferred_capabilities.tools && report.inferred_capabilities.parallel_tools);
}
