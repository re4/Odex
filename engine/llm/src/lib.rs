//! Odex model layer for self-hosted OpenAI-compatible servers (vLLM first).
//!
//! - [`client::LlmClient`]: streaming chat completions with retries, idle
//!   timeouts, per-endpoint concurrency queues and context-overflow parsing.
//! - [`stream`]: SSE decoding, tool-call assembly by index, reasoning from
//!   `reasoning`/`reasoning_content`, client-side `<think>` stripping.
//! - [`fallback`]: tool calls recovered from `content` in many model formats.
//! - [`repair`] / [`validate`]: argument hygiene with precise errors.
//! - [`registry::ModelRegistry`]: configured + discovered models and roles.
//! - [`doctor`]: endpoint health checks with `vllm serve` fix suggestions.

pub mod client;
pub mod discovery;
pub mod doctor;
pub mod error;
pub mod fallback;
pub mod loopguard;
pub mod registry;
pub mod repair;
pub mod stream;
pub mod tokens;
pub mod types;
pub mod validate;

pub use client::LlmClient;
pub use error::{LlmError, Overflow};
pub use registry::{ModelHandle, ModelRegistry};
pub use types::*;
