//! Doctor: probes an endpoint/model and reports what works, with the
//! `vllm serve` flags that would fix what doesn't.

use std::time::Instant;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use odex_protocol::{CheckStatus, DoctorCheck, DoctorReport, ModelCapabilities};

use crate::discovery;
use crate::error::LlmError;
use crate::registry::{ModelHandle, ModelRegistry};
use crate::types::{ChatMessage, ChatRequest, ContentPart, Role, StructuredOutput, ToolSpec};

/// 16x16 solid red PNG.
const RED_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAF0lEQVR4nGP4z8BAEiJN9aiGUQ1DSgMAkPn/Afnh+ngAAAAASUVORK5CYII=";

fn weather_tool() -> ToolSpec {
    ToolSpec {
        name: "get_weather".into(),
        description: "Get the current weather for a city.".into(),
        parameters: json!({
            "type": "object",
            "properties": {"city": {"type": "string", "description": "City name"}},
            "required": ["city"]
        }),
    }
}

struct Ctx<'a> {
    h: &'a ModelHandle,
    cancel: CancellationToken,
    checks: Vec<DoctorCheck>,
    caps: ModelCapabilities,
    preset_flags: PresetFlags,
}

#[derive(Default, Clone)]
struct PresetFlags {
    tool_parser: Option<String>,
    reasoning_parser: Option<String>,
}

fn flag_value(cmd: &str, flag: &str) -> Option<String> {
    let mut it = cmd.split_whitespace();
    while let Some(t) = it.next() {
        if t == flag {
            return it.next().map(|s| s.to_string());
        }
        if let Some(v) = t.strip_prefix(&format!("{flag}=")) {
            return Some(v.to_string());
        }
    }
    None
}

impl Ctx<'_> {
    fn push(
        &mut self,
        id: &str,
        name: &str,
        status: CheckStatus,
        detail: impl Into<String>,
        t0: Instant,
        fix: Vec<String>,
    ) {
        self.checks.push(DoctorCheck {
            id: id.into(),
            name: name.into(),
            status,
            detail: detail.into(),
            duration_ms: t0.elapsed().as_millis() as u64,
            fix_flags: fix,
        });
    }

    fn tool_fix(&self) -> Vec<String> {
        vec![
            "--enable-auto-tool-choice".into(),
            format!(
                "--tool-call-parser {}",
                self.preset_flags.tool_parser.clone().unwrap_or_else(|| "<parser>".into())
            ),
        ]
    }

    async fn chat(&self, req: ChatRequest) -> Result<crate::types::ChatResponse, LlmError> {
        self.h.client.chat(&self.h.model, &req, &self.cancel).await
    }
}

/// Run Doctor for one model. `quick` skips the prefix-cache timing.
pub async fn run(registry: &ModelRegistry, h: &ModelHandle, quick: bool) -> DoctorReport {
    let preset = h.model.preset.as_deref().and_then(|p| registry.presets().get(p)).cloned();
    let preset_flags = preset
        .as_ref()
        .map(|p| PresetFlags {
            tool_parser: flag_value(&p.serve_command, "--tool-call-parser"),
            reasoning_parser: flag_value(&p.serve_command, "--reasoning-parser"),
        })
        .unwrap_or_default();
    let mut c = Ctx {
        h,
        cancel: CancellationToken::new(),
        checks: vec![],
        caps: ModelCapabilities { tools: false, vision: false, parallel_tools: false, reasoning: false },
        preset_flags,
    };
    let client = &h.client;

    // 1. connect + models
    let t0 = Instant::now();
    let models = discovery::list_models(client).await;
    let version = discovery::version(client).await;
    match &models {
        Ok(ms) => {
            let found = ms.iter().find(|m| m.id == h.model.model_id);
            match found {
                Some(m) => c.push(
                    "connect",
                    "Endpoint reachable",
                    CheckStatus::Pass,
                    format!(
                        "{} model(s); `{}` max_model_len={}{}",
                        ms.len(),
                        m.id,
                        m.max_model_len.map(|x| x.to_string()).unwrap_or_else(|| "?".into()),
                        version.as_ref().map(|v| format!(", vLLM {v}")).unwrap_or_default()
                    ),
                    t0,
                    vec![],
                ),
                None => c.push(
                    "connect",
                    "Endpoint reachable",
                    CheckStatus::Fail,
                    format!(
                        "model `{}` is not served here (available: {})",
                        h.model.model_id,
                        ms.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                    t0,
                    vec![],
                ),
            }
        }
        Err(e) => {
            c.push("connect", "Endpoint reachable", CheckStatus::Fail, e.to_string(), t0, vec![]);
            return finish(c, h, version, preset.map(|p| p.serve_command));
        }
    }
    let t0 = Instant::now();
    match discovery::health(client).await {
        Some(true) => c.push("health", "/health", CheckStatus::Pass, "healthy", t0, vec![]),
        Some(false) => c.push("health", "/health", CheckStatus::Fail, "server reports unhealthy", t0, vec![]),
        None => c.push(
            "health",
            "/health",
            CheckStatus::Warn,
            "no /health endpoint (not vLLM, or behind a proxy)",
            t0,
            vec![],
        ),
    }

    // 2. streaming + usage
    let t0 = Instant::now();
    let mut saw_delta = false;
    let mut sink = |e: crate::types::ChatEvent| {
        if matches!(e, crate::types::ChatEvent::ContentDelta(_) | crate::types::ChatEvent::ReasoningDelta(_)) {
            saw_delta = true;
        }
    };
    let req = ChatRequest {
        messages: vec![ChatMessage::user("Reply with exactly: OK")],
        max_tokens: Some(64),
        effort: Some(odex_protocol::ReasoningEffort::None),
        ..Default::default()
    };
    match client.stream_chat(&h.model, &req, &c.cancel, &mut sink).await {
        Ok(r) => {
            let usage = r.usage.is_some();
            let status = if saw_delta && usage { CheckStatus::Pass } else { CheckStatus::Warn };
            c.push(
                "streaming",
                "Streaming + usage",
                status,
                format!(
                    "{}; usage {}; ttft {}ms",
                    if saw_delta { "deltas received" } else { "no incremental deltas" },
                    if usage { "reported" } else { "missing (stream_options.include_usage ignored)" },
                    r.ttft_ms.unwrap_or(0)
                ),
                t0,
                vec![],
            );
        }
        Err(e) => {
            c.push("streaming", "Streaming + usage", CheckStatus::Fail, e.to_string(), t0, vec![]);
            return finish(c, h, version, preset.map(|p| p.serve_command));
        }
    }

    // 3. native tool call (non-thinking for reliability)
    let t0 = Instant::now();
    let req = ChatRequest {
        messages: vec![
            ChatMessage::system("You are a tool-using assistant. Use tools when they can answer."),
            ChatMessage::user("What is the weather in Paris right now? Use the get_weather tool."),
        ],
        tools: vec![weather_tool()],
        max_tokens: Some(512),
        effort: Some(odex_protocol::ReasoningEffort::None),
        ..Default::default()
    };
    let mut native_tools = false;
    match c.chat(req).await {
        Ok(r) if !r.tool_calls.is_empty() && !r.fallback_parsed => {
            native_tools = true;
            let args = &r.tool_calls[0].arguments;
            let valid =
                serde_json::from_str::<serde_json::Value>(args).map(|v| v.get("city").is_some()).unwrap_or(false);
            c.push(
                "toolCall",
                "Native tool call",
                if valid { CheckStatus::Pass } else { CheckStatus::Warn },
                format!("{}({args})", r.tool_calls[0].name),
                t0,
                vec![],
            );
        }
        Ok(r) if r.fallback_parsed => {
            let fix = c.tool_fix();
            c.push(
                "toolCall",
                "Native tool call",
                CheckStatus::Warn,
                "tool call arrived in `content` and was recovered by the client fallback parser; the server's tool parser is missing or wrong",
                t0,
                fix,
            );
        }
        Ok(r) => {
            let fix = c.tool_fix();
            c.push(
                "toolCall",
                "Native tool call",
                CheckStatus::Fail,
                format!("no tool call; model said: {}", trunc(&r.content, 120)),
                t0,
                fix,
            );
        }
        Err(e) => {
            let fix = if e.to_string().contains("tool") { c.tool_fix() } else { vec![] };
            c.push("toolCall", "Native tool call", CheckStatus::Fail, e.to_string(), t0, fix);
        }
    }
    c.caps.tools = native_tools || c.checks.iter().any(|x| x.id == "toolCall" && x.status == CheckStatus::Warn);

    // 4. streamed tool call deltas + 5. parallel
    let t0 = Instant::now();
    let mut starts = 0usize;
    let mut arg_deltas = 0usize;
    let mut sink = |e: crate::types::ChatEvent| match e {
        crate::types::ChatEvent::ToolCallStart { .. } => starts += 1,
        crate::types::ChatEvent::ToolCallDelta { .. } => arg_deltas += 1,
        _ => {}
    };
    let req = ChatRequest {
        messages: vec![
            ChatMessage::system("You are a tool-using assistant. Call tools in parallel when tasks are independent."),
            ChatMessage::user(
                "Get the current weather in Paris and in Tokyo. Call get_weather once per city, both at once.",
            ),
        ],
        tools: vec![weather_tool()],
        max_tokens: Some(512),
        effort: Some(odex_protocol::ReasoningEffort::None),
        ..Default::default()
    };
    match client.stream_chat(&h.model, &req, &c.cancel, &mut sink).await {
        Ok(r) => {
            let streamed = starts > 0 && !r.fallback_parsed;
            c.push(
                "streamedToolCall",
                "Streamed tool call",
                if streamed {
                    CheckStatus::Pass
                } else if r.tool_calls.is_empty() {
                    CheckStatus::Fail
                } else {
                    CheckStatus::Warn
                },
                format!(
                    "{starts} call start(s), {arg_deltas} argument delta(s){}",
                    if r.fallback_parsed { " (fallback-parsed)" } else { "" }
                ),
                t0,
                if streamed { vec![] } else { c.tool_fix() },
            );
            let n = r.tool_calls.len();
            c.caps.parallel_tools = n >= 2;
            c.push(
                "parallelToolCalls",
                "Parallel tool calls",
                if n >= 2 { CheckStatus::Pass } else { CheckStatus::Warn },
                if n >= 2 {
                    format!("{n} calls in one response")
                } else {
                    format!("{n} call(s); the model calls tools one at a time")
                },
                t0,
                vec![],
            );
        }
        Err(e) => c.push("streamedToolCall", "Streamed tool call", CheckStatus::Fail, e.to_string(), t0, vec![]),
    }

    // 6. reasoning parsing
    let t0 = Instant::now();
    if h.model.capabilities.reasoning || !h.model.reasoning_effort_map.is_empty() {
        let req = ChatRequest {
            messages: vec![ChatMessage::user("What is 17 * 23? Think it through, then answer with just the number.")],
            max_tokens: Some(2048),
            effort: Some(odex_protocol::ReasoningEffort::Medium),
            ..Default::default()
        };
        // Use a raw request without client-side think stripping to see what the server does.
        let mut raw = h.model.clone();
        raw.capabilities.reasoning = false;
        raw.preset = None;
        match h.client.chat(&raw, &req, &c.cancel).await {
            Ok(r) if !r.reasoning.is_empty() => {
                c.caps.reasoning = true;
                c.push(
                    "reasoning",
                    "Reasoning parser",
                    CheckStatus::Pass,
                    format!("{} chars of reasoning in a dedicated field", r.reasoning.len()),
                    t0,
                    vec![],
                );
            }
            Ok(r) if r.content.contains("</think>") => {
                c.caps.reasoning = true;
                let fix = vec![format!(
                    "--reasoning-parser {}",
                    c.preset_flags.reasoning_parser.clone().unwrap_or_else(|| "<parser>".into())
                )];
                c.push(
                    "reasoning",
                    "Reasoning parser",
                    CheckStatus::Warn,
                    "thinking is mixed into `content`; Odex strips it client-side, but a server parser is better",
                    t0,
                    fix,
                );
            }
            Ok(_) => c.push(
                "reasoning",
                "Reasoning parser",
                CheckStatus::Warn,
                "no reasoning returned (thinking may be off)",
                t0,
                vec![],
            ),
            Err(e) => c.push("reasoning", "Reasoning parser", CheckStatus::Fail, e.to_string(), t0, vec![]),
        }
    } else {
        c.push("reasoning", "Reasoning parser", CheckStatus::Skip, "model is not a reasoning model", t0, vec![]);
    }

    // 7. vision
    let t0 = Instant::now();
    let req = ChatRequest {
        messages: vec![ChatMessage {
            role: Role::User,
            content: vec![
                ContentPart::ImageUrl { url: format!("data:image/png;base64,{RED_PNG}") },
                ContentPart::Text { text: "What single color fills this image? Answer with one word.".into() },
            ],
            tool_calls: vec![],
            tool_call_id: None,
            name: None,
            reasoning: None,
        }],
        max_tokens: Some(256),
        effort: Some(odex_protocol::ReasoningEffort::None),
        ..Default::default()
    };
    match c.chat(req).await {
        Ok(r) => {
            let red = r.content.to_lowercase().contains("red");
            c.caps.vision = true;
            c.push(
                "vision",
                "Vision (image input)",
                if red { CheckStatus::Pass } else { CheckStatus::Warn },
                if red {
                    "identified the test image".to_string()
                } else {
                    format!("accepted the image but answered: {}", trunc(&r.content, 60))
                },
                t0,
                vec![],
            );
        }
        Err(e) => {
            c.push(
                "vision",
                "Vision (image input)",
                CheckStatus::Skip,
                format!("not a vision model ({})", trunc(&e.to_string(), 120)),
                t0,
                vec![],
            );
        }
    }

    // 8. tokenize
    let t0 = Instant::now();
    match discovery::tokenize_messages(client, &h.model.model_id, &[ChatMessage::user("hello world")], None).await {
        Ok(n) => {
            c.push("tokenize", "/tokenize", CheckStatus::Pass, format!("{n} tokens for a 2-word chat"), t0, vec![])
        }
        Err(e) => c.push(
            "tokenize",
            "/tokenize",
            CheckStatus::Warn,
            format!("unavailable ({e}); Odex will estimate token counts"),
            t0,
            vec![],
        ),
    }

    // 9. prefix cache speed-up
    let t0 = Instant::now();
    if quick {
        c.push("prefixCache", "Prefix caching", CheckStatus::Skip, "skipped (quick mode)", t0, vec![]);
    } else {
        let filler: String =
            (0..400).map(|i| format!("Line {i}: the quick brown fox jumps over the lazy dog.\n")).collect();
        let req = ChatRequest {
            messages: vec![
                ChatMessage::system(format!("Reference text:\n{filler}")),
                ChatMessage::user("Reply with OK."),
            ],
            max_tokens: Some(1),
            effort: Some(odex_protocol::ReasoningEffort::None),
            ..Default::default()
        };
        let a = c.chat(req.clone()).await;
        let b = c.chat(req).await;
        match (a, b) {
            (Ok(a), Ok(b)) => {
                let cached = b.usage.map(|u| u.cached_input_tokens).unwrap_or(0);
                let ta = a.ttft_ms.unwrap_or(a.total_ms).max(1) as f64;
                let tb = b.ttft_ms.unwrap_or(b.total_ms).max(1) as f64;
                let speedup = ta / tb;
                let pass = cached > 0 || speedup >= 1.3;
                c.push(
                    "prefixCache",
                    "Prefix caching",
                    if pass { CheckStatus::Pass } else { CheckStatus::Warn },
                    format!(
                        "first {ta:.0}ms, repeat {tb:.0}ms ({speedup:.1}x){}",
                        if cached > 0 { format!(", {cached} cached tokens") } else { String::new() }
                    ),
                    t0,
                    if pass { vec![] } else { vec!["--enable-prefix-caching".into()] },
                );
            }
            (Err(e), _) | (_, Err(e)) => {
                c.push("prefixCache", "Prefix caching", CheckStatus::Fail, e.to_string(), t0, vec![])
            }
        }
    }

    // 10. structured output
    let t0 = Instant::now();
    let schema = json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}, "confidence": {"type": "number"}},
        "required": ["answer", "confidence"],
        "additionalProperties": false
    });
    let mut so_model = h.model.clone();
    so_model.structured_output = odex_protocol::StructuredOutputMode::JsonSchema;
    let req = ChatRequest {
        messages: vec![ChatMessage::user("Name the capital of France. Respond as JSON.")],
        max_tokens: Some(512),
        structured: Some(StructuredOutput { name: "doctor_probe".into(), schema: schema.clone() }),
        effort: Some(odex_protocol::ReasoningEffort::None),
        ..Default::default()
    };
    match h.client.chat(&so_model, &req, &c.cancel).await {
        Ok(r) => {
            let parsed = serde_json::from_str::<serde_json::Value>(r.content.trim()).ok();
            let ok = parsed.as_ref().map(|v| crate::validate::validate(&schema, v).is_ok()).unwrap_or(false);
            c.push(
                "structuredOutput",
                "Structured output (json_schema)",
                if ok { CheckStatus::Pass } else { CheckStatus::Warn },
                if ok {
                    "response matched the schema".to_string()
                } else {
                    format!("output did not match the schema: {}", trunc(&r.content, 80))
                },
                t0,
                vec![],
            );
        }
        Err(e) => {
            c.push("structuredOutput", "Structured output (json_schema)", CheckStatus::Fail, e.to_string(), t0, vec![])
        }
    }

    finish(c, h, version, preset.map(|p| p.serve_command))
}

fn finish(c: Ctx<'_>, h: &ModelHandle, version: Option<String>, preset_cmd: Option<String>) -> DoctorReport {
    let mut flags: Vec<String> = Vec::new();
    for ch in &c.checks {
        for f in &ch.fix_flags {
            for part in split_flags(f) {
                if !flags.contains(&part) {
                    flags.push(part);
                }
            }
        }
    }
    let suggested_command = if flags.is_empty() {
        None
    } else {
        Some(
            preset_cmd
                .map(|p| p.replace("<model>", &h.model.model_id))
                .unwrap_or_else(|| format!("vllm serve {} {}", h.model.model_id, flags.join(" "))),
        )
    };
    DoctorReport {
        provider_id: h.model.provider_id.clone(),
        model_id: h.model.model_id.clone(),
        base_url: h.client.base_url().to_string(),
        server_version: version,
        checks: c.checks,
        suggested_flags: flags,
        suggested_command,
        ran_at: chrono::Utc::now().timestamp_millis(),
        inferred_capabilities: c.caps,
    }
}

fn split_flags(f: &str) -> Vec<String> {
    // keep "--flag value" pairs together
    let mut out = Vec::new();
    let mut cur = String::new();
    for t in f.split_whitespace() {
        if t.starts_with("--") && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(t);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn trunc(s: &str, n: usize) -> String {
    let t: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        format!("{t}…")
    } else {
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_parsing() {
        let cmd = "vllm serve x --enable-auto-tool-choice --tool-call-parser qwen3_coder --reasoning-parser=qwen3";
        assert_eq!(flag_value(cmd, "--tool-call-parser").as_deref(), Some("qwen3_coder"));
        assert_eq!(flag_value(cmd, "--reasoning-parser").as_deref(), Some("qwen3"));
        assert_eq!(
            split_flags("--enable-auto-tool-choice --tool-call-parser hermes"),
            vec!["--enable-auto-tool-choice", "--tool-call-parser hermes"]
        );
    }
}
