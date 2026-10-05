//! Chat-completions request/response types (OpenAI-compatible, vLLM flavored).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use odex_protocol::TokenUsage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// `url` is a data URL or http(s) URL.
    ImageUrl {
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments string as produced by the model.
    pub arguments: String,
}

/// One message in the conversation sent to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default)]
    pub content: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool name for `role: tool` messages (some chat templates need it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Prior reasoning text (replayed per `reasoning_history`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

impl ChatMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(Role::Assistant, text)
    }
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
            reasoning: None,
        }
    }
    pub fn tool_result(call_id: impl Into<String>, name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::Text { text: text.into() }],
            tool_calls: vec![],
            tool_call_id: Some(call_id.into()),
            name: Some(name.into()),
            reasoning: None,
        }
    }
    pub fn assistant_tool_calls(text: Option<String>, calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: text.filter(|t| !t.is_empty()).map(|t| vec![ContentPart::Text { text: t }]).unwrap_or_default(),
            tool_calls: calls,
            tool_call_id: None,
            name: None,
            reasoning: None,
        }
    }

    /// Concatenated text parts.
    pub fn text_content(&self) -> String {
        let mut s = String::new();
        for p in &self.content {
            if let ContentPart::Text { text } = p {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(text);
            }
        }
        s
    }

    pub fn image_count(&self) -> usize {
        self.content.iter().filter(|p| matches!(p, ContentPart::ImageUrl { .. })).count()
    }

    /// Serialize to the OpenAI wire format.
    pub fn to_wire(&self, include_reasoning: bool) -> Value {
        let role = match self.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut m = serde_json::Map::new();
        m.insert("role".into(), json!(role));
        let has_images = self.image_count() > 0;
        let content: Value = if has_images && matches!(self.role, Role::User | Role::System) {
            Value::Array(
                self.content
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => json!({"type": "text", "text": text}),
                        ContentPart::ImageUrl { url } => json!({"type": "image_url", "image_url": {"url": url}}),
                    })
                    .collect(),
            )
        } else {
            let t = self.text_content();
            if t.is_empty() && self.role == Role::Assistant && !self.tool_calls.is_empty() {
                Value::Null
            } else {
                Value::String(t)
            }
        };
        m.insert("content".into(), content);
        if !self.tool_calls.is_empty() {
            m.insert(
                "tool_calls".into(),
                Value::Array(
                    self.tool_calls
                        .iter()
                        .map(|c| {
                            json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments}})
                        })
                        .collect(),
                ),
            );
        }
        if let Some(id) = &self.tool_call_id {
            m.insert("tool_call_id".into(), json!(id));
        }
        if let Some(n) = &self.name {
            if self.role == Role::Tool {
                m.insert("name".into(), json!(n));
            }
        }
        if include_reasoning && self.role == Role::Assistant {
            if let Some(r) = &self.reasoning {
                if !r.is_empty() {
                    m.insert("reasoning_content".into(), json!(r));
                }
            }
        }
        Value::Object(m)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl ToolSpec {
    pub fn to_wire(&self) -> Value {
        json!({"type": "function", "function": {"name": self.name, "description": self.description, "parameters": self.parameters}})
    }
}

/// Structured-output request (JSON schema).
#[derive(Debug, Clone, PartialEq)]
pub struct StructuredOutput {
    pub name: String,
    pub schema: Value,
}

#[derive(Debug, Clone, Default)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolSpec>,
    /// Upper bound for generated tokens (budgeted by the caller/context engine).
    pub max_tokens: Option<u32>,
    pub structured: Option<StructuredOutput>,
    pub effort: Option<odex_protocol::ReasoningEffort>,
    /// Extra JSON merged into the body last (per-request overrides).
    pub extra: Option<Value>,
    /// Replay reasoning for assistant messages that carry it.
    pub include_reasoning: bool,
    /// Disable tools even if listed (e.g. for final summaries).
    pub tool_choice_none: bool,
    pub temperature_override: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other,
}

impl FinishReason {
    pub fn parse(s: &str) -> Self {
        match s {
            "stop" | "eos" | "end_turn" => FinishReason::Stop,
            "length" | "max_tokens" => FinishReason::Length,
            "tool_calls" | "function_call" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            _ => FinishReason::Other,
        }
    }
}

/// Aggregated result of one model call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatResponse {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<FinishReason>,
    pub usage: Option<TokenUsage>,
    /// Tool calls were recovered from content by the client-side fallback parser.
    pub fallback_parsed: bool,
    /// Server-side model id that answered.
    pub model: Option<String>,
    /// Time to first token.
    pub ttft_ms: Option<u64>,
    pub total_ms: u64,
    pub retries: u32,
}

/// Streaming events from a model call.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    ContentDelta(String),
    ReasoningDelta(String),
    /// A tool call started (name known).
    ToolCallStart {
        index: usize,
        id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        arguments: String,
    },
    Usage(TokenUsage),
    /// The request is being retried; partial output so far must be discarded.
    Retrying {
        attempt: u32,
        reason: String,
        delay_ms: u64,
    },
    Done(ChatResponse),
}
