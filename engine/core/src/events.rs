//! Event plumbing from the engine to its client (app-server or `exec`).

use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use odex_protocol::*;

/// Where engine notifications and server→client requests go.
#[async_trait]
pub trait EventSink: Send + Sync {
    fn notify(&self, method: &str, params: Value);
    /// Send a request to the client and await its result.
    async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value>;
    fn capabilities(&self) -> ClientCapabilities {
        ClientCapabilities::default()
    }
}

/// A sink that drops everything and fails requests (headless defaults).
pub struct NullSink;

#[async_trait]
impl EventSink for NullSink {
    fn notify(&self, _method: &str, _params: Value) {}
    async fn request(&self, method: &str, _params: Value) -> anyhow::Result<Value> {
        anyhow::bail!("no client attached to answer `{method}`")
    }
}

/// Typed helpers over a sink.
#[derive(Clone)]
pub struct Emitter {
    sink: Arc<dyn EventSink>,
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

impl Emitter {
    pub fn new(sink: Arc<dyn EventSink>) -> Self {
        Self { sink }
    }

    pub fn sink(&self) -> &Arc<dyn EventSink> {
        &self.sink
    }

    pub fn raw<T: Serialize>(&self, method: &str, params: &T) {
        self.sink.notify(method, to_value(params));
    }

    pub async fn request<T: Serialize>(&self, method: &str, params: &T) -> anyhow::Result<Value> {
        self.sink.request(method, to_value(params)).await
    }

    pub fn thread_started(&self, t: &Thread) {
        self.raw(notification::THREAD_STARTED, &ThreadNotification { thread: t.clone() });
    }
    pub fn thread_updated(&self, t: &Thread) {
        self.raw(notification::THREAD_UPDATED, &ThreadNotification { thread: t.clone() });
    }
    pub fn thread_deleted(&self, id: &str) {
        self.raw(notification::THREAD_DELETED, &ThreadIdNotification { thread_id: id.into() });
    }
    pub fn turn_started(&self, thread_id: &str, turn: &Turn) {
        self.raw(notification::TURN_STARTED, &TurnNotification { thread_id: thread_id.into(), turn: turn.clone() });
    }
    pub fn turn_completed(&self, thread_id: &str, turn: &Turn) {
        self.raw(notification::TURN_COMPLETED, &TurnNotification { thread_id: thread_id.into(), turn: turn.clone() });
    }
    pub fn item_started(&self, thread_id: &str, turn_id: &str, item: &ThreadItem) {
        self.raw(
            notification::ITEM_STARTED,
            &ItemNotification { thread_id: thread_id.into(), turn_id: turn_id.into(), item: item.clone() },
        );
    }
    pub fn item_completed(&self, thread_id: &str, turn_id: &str, item: &ThreadItem) {
        self.raw(
            notification::ITEM_COMPLETED,
            &ItemNotification { thread_id: thread_id.into(), turn_id: turn_id.into(), item: item.clone() },
        );
    }
    pub fn item_delta(&self, thread_id: &str, turn_id: &str, item_id: &str, delta: ItemDelta) {
        self.raw(
            notification::ITEM_DELTA,
            &ItemDeltaNotification {
                thread_id: thread_id.into(),
                turn_id: turn_id.into(),
                item_id: item_id.into(),
                delta,
            },
        );
    }
    pub fn plan_updated(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        explanation: Option<String>,
        plan: Vec<PlanStep>,
    ) {
        self.raw(
            notification::PLAN_UPDATED,
            &PlanUpdatedNotification {
                thread_id: thread_id.into(),
                turn_id: turn_id.map(String::from),
                explanation,
                plan,
            },
        );
    }
    pub fn diff_updated(&self, thread_id: &str, turn_id: Option<&str>, stats: DiffStats) {
        self.raw(
            notification::DIFF_UPDATED,
            &DiffUpdatedNotification { thread_id: thread_id.into(), turn_id: turn_id.map(String::from), stats },
        );
    }
    pub fn context_updated(&self, thread_id: &str, context: ContextStatus) {
        self.raw(notification::CONTEXT_UPDATED, &ContextUpdatedNotification { thread_id: thread_id.into(), context });
    }
    pub fn token_usage(&self, thread_id: &str, turn_id: &str, last: TokenUsage, total: TokenUsage) {
        self.raw(
            notification::TOKEN_USAGE_UPDATED,
            &TokenUsageNotification { thread_id: thread_id.into(), turn_id: turn_id.into(), last, total },
        );
    }
    pub fn followups(&self, thread_id: &str, suggestions: Vec<String>) {
        self.raw(notification::FOLLOWUPS, &FollowupsNotification { thread_id: thread_id.into(), suggestions });
    }
    pub fn sources(&self, thread_id: &str, sources: Vec<SourceEntry>) {
        self.raw(notification::SOURCES_UPDATED, &SourcesNotification { thread_id: thread_id.into(), sources });
    }
    pub fn queue(&self, thread_id: &str, queued: Vec<Vec<UserInput>>) {
        self.raw(notification::QUEUE_UPDATED, &QueueNotification { thread_id: thread_id.into(), queued });
    }
    pub fn approval_resolved(&self, thread_id: &str, approval_id: &str) {
        self.raw(
            notification::APPROVAL_RESOLVED,
            &ApprovalResolvedNotification { thread_id: thread_id.into(), approval_id: approval_id.into() },
        );
    }
    pub fn log(&self, level: &str, message: impl Into<String>) {
        self.raw(notification::LOG, &LogNotification { level: level.into(), message: message.into(), target: None });
    }
}
