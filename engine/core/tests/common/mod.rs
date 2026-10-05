#![allow(dead_code)]
//! Shared harness: an engine on a temp home wired to the mock vLLM server.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use odex_config::OdexHome;
use odex_core::{Engine, EngineOptions, EventSink};
use odex_mock_vllm::{MockConfig, MockModel, MockServer};
use odex_protocol::*;

type Decider = Box<dyn Fn(&ApprovalRequestParams) -> ApprovalDecision + Send>;

pub struct TestSink {
    pub events: Mutex<Vec<(String, Value)>>,
    pub approvals: Mutex<Vec<ApprovalRequestParams>>,
    pub decide: Mutex<Decider>,
}

impl TestSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(vec![]),
            approvals: Mutex::new(vec![]),
            decide: Mutex::new(Box::new(|_| ApprovalDecision::Approve)),
        })
    }
    pub fn count(&self, method: &str) -> usize {
        self.events.lock().unwrap().iter().filter(|(m, _)| m == method).count()
    }
    pub fn items(&self) -> Vec<ThreadItem> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == notification::ITEM_COMPLETED)
            .filter_map(|(_, v)| serde_json::from_value::<ItemNotification>(v.clone()).ok())
            .map(|n| n.item)
            .collect()
    }
}

#[async_trait]
impl EventSink for TestSink {
    fn notify(&self, method: &str, params: Value) {
        self.events.lock().unwrap().push((method.to_string(), params));
    }
    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        if method == server_request::APPROVAL_REQUEST {
            let p: ApprovalRequestParams = serde_json::from_value(params)?;
            let d = (self.decide.lock().unwrap())(&p);
            self.approvals.lock().unwrap().push(p);
            return Ok(serde_json::to_value(ApprovalResponse { decision: d })?);
        }
        anyhow::bail!("unsupported {method}")
    }
    fn capabilities(&self) -> ClientCapabilities {
        ClientCapabilities { approvals: true, ..Default::default() }
    }
}

pub struct Harness {
    pub server: MockServer,
    pub engine: Engine,
    pub sink: Arc<TestSink>,
    pub home: tempfile::TempDir,
    pub work: tempfile::TempDir,
}

pub async fn harness(max_model_len: u32, extra_config: &str) -> Harness {
    harness_cpt(max_model_len, 4.0, extra_config).await
}

/// `chars_per_token` sets the mock tokenizer (lower = server counts more tokens than we estimate).
pub async fn harness_cpt(max_model_len: u32, chars_per_token: f64, extra_config: &str) -> Harness {
    let server = MockServer::start(MockConfig {
        models: vec![MockModel { id: "mock-coder".into(), max_model_len }],
        chars_per_token,
        ..Default::default()
    })
    .await;
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let cfg = format!(
        "model = \"mock:mock-coder\"\n[features]\nfollow_up_suggestions = false\nauto_title = false\n[model_providers.mock]\nbase_url = \"{}\"\nstream_idle_timeout_ms = 5000\n{extra_config}\n[projects.'{}']\ntrust_level = \"trusted\"\n",
        server.url,
        work.path().display()
    );
    std::fs::write(home.path().join("config.toml"), cfg).unwrap();
    let engine = Engine::new(EngineOptions { home: OdexHome::at(home.path()), profile: None }).unwrap();
    let sink = TestSink::new();
    engine.set_sink(sink.clone());
    engine.registry.refresh().await;
    Harness { server, engine, sink, home, work }
}

impl Harness {
    pub async fn thread(&self, mode: PermissionMode) -> String {
        let t = odex_core::api::thread_start(
            &self.engine,
            ThreadStartParams {
                cwd: Some(self.work.path().to_string_lossy().to_string()),
                permission_mode: Some(mode),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        t.thread.id
    }

    pub async fn run(&self, thread_id: &str, text: &str) -> Turn {
        let r = odex_core::turn::start_turn(
            &self.engine,
            TurnStartParams { thread_id: thread_id.into(), input: vec![UserInput::text(text)], ..Default::default() },
        )
        .await
        .unwrap();
        let turn_id = r.turn.unwrap().id;
        self.wait(thread_id, &turn_id, Duration::from_secs(120)).await
    }

    pub async fn wait(&self, thread_id: &str, turn_id: &str, timeout: Duration) -> Turn {
        let rt = self.engine.thread(thread_id).unwrap();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(t) = rt.turns.lock().unwrap().iter().find(|t| t.id == turn_id) {
                if t.status != TurnStatus::InProgress && !rt.is_running() {
                    return t.clone();
                }
            }
            assert!(std::time::Instant::now() < deadline, "turn did not finish in time");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
