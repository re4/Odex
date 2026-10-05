//! The `compactor` model as a [`odex_context::Summarizer`].

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use odex_llm::types::{ChatMessage, ChatRequest, StructuredOutput};
use odex_llm::ModelHandle;
use odex_protocol::{ReasoningEffort, StructuredOutputMode};

use crate::engine::Engine;

pub struct ModelSummarizer {
    engine: Engine,
    handle: ModelHandle,
}

impl ModelSummarizer {
    pub fn new(engine: Engine, handle: ModelHandle) -> Self {
        Self { engine, handle }
    }

    fn output_tokens(&self) -> u32 {
        (self.handle.context_window / 4).clamp(256, 4096).min(self.handle.model.max_output_tokens.max(256))
    }
}

#[async_trait]
impl odex_context::Summarizer for ModelSummarizer {
    fn input_budget(&self) -> u32 {
        let w = self.handle.context_window;
        w.saturating_sub(self.output_tokens() + (w as f64 * 0.05) as u32 + 64)
    }

    fn count(&self, text: &str) -> u32 {
        self.engine.registry.estimator(&self.handle.model.key).text(text)
    }

    async fn summarize(&self, system: &str, user: &str, schema: &Value) -> anyhow::Result<String> {
        let structured = self.handle.model.structured_output != StructuredOutputMode::None;
        let mut sys = system.to_string();
        if !structured {
            sys.push_str("\nReturn a single JSON object with exactly these keys: goal_and_requirements, decisions, plan, files_changed, codebase_facts, commands_and_tests, open_errors, next_steps, important_refs.");
        }
        let req = ChatRequest {
            messages: vec![ChatMessage::system(sys), ChatMessage::user(user)],
            max_tokens: Some(self.output_tokens()),
            structured: if structured {
                Some(StructuredOutput { name: "context_summary".into(), schema: schema.clone() })
            } else {
                None
            },
            effort: Some(ReasoningEffort::None),
            temperature_override: Some(0.2),
            ..Default::default()
        };
        let resp = self.handle.client.chat(&self.handle.model, &req, &CancellationToken::new()).await?;
        if let Some(u) = resp.usage {
            self.engine.store.record_usage(&self.handle.model.key, &u);
        }
        Ok(resp.content)
    }
}
