//! Odex agent engine.
//!
//! The engine owns threads, runs agent turns against vLLM models, executes
//! tools under the permission/sandbox model, manages context (pruning,
//! compaction, recall), and persists everything to rollouts + a SQLite index.
//! Clients (the app-server, `exec`) drive it through [`api`] and receive
//! notifications through an [`events::EventSink`].

pub mod api;
pub mod approval;
pub mod background;
pub mod engine;
pub mod events;
pub mod extensions;
pub mod followups;
pub mod hooks_rt;
pub mod model_watch;
pub mod plugins;
pub mod prompt;
pub mod rollout;
pub mod sessions;
pub mod skills;
pub mod starters;
pub mod store;
pub mod subagents;
pub mod summarizer;
pub mod thread;
pub mod toolexec;
pub mod turn;
pub mod workspace;
pub mod worktrees;

pub use engine::{EResult, Engine, EngineError, EngineOptions};
pub use events::{EventSink, NullSink};

impl Engine {
    /// Start background work: endpoint discovery, MCP servers, the
    /// automation scheduler. Call once inside a Tokio runtime.
    pub fn start_background(&self, scheduler: bool) -> tokio_util::sync::CancellationToken {
        let shutdown = tokio_util::sync::CancellationToken::new();
        if let Some(rx) = self.mcp_rx.lock().unwrap().take() {
            let e = self.clone();
            tokio::spawn(async move { extensions::pump_mcp_events(e, rx).await });
        }
        let e = self.clone();
        tokio::spawn(async move {
            e.registry.refresh().await;
            model_watch::check_all(&e);
            e.emitter().raw(
                odex_protocol::notification::PROVIDERS_UPDATED,
                &odex_protocol::ProvidersNotification { providers: e.registry.providers() },
            );
        });
        let e = self.clone();
        tokio::spawn(async move { extensions::sync_mcp(&e).await });
        if scheduler {
            let e = self.clone();
            let s = shutdown.clone();
            tokio::spawn(async move { background::scheduler(e, s).await });
        }
        shutdown
    }
}
