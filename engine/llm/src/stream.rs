//! SSE decoding and chat-completion chunk assembly.

use serde_json::Value;

use odex_protocol::TokenUsage;

use crate::fallback;
use crate::types::{ChatEvent, ChatResponse, FinishReason, ToolCall};

/// Incremental Server-Sent-Events decoder (handles `\n` and `\r\n`, multi-line data).
#[derive(Default)]
pub struct SseDecoder {
    buf: String,
    data: Vec<String>,
}

impl SseDecoder {
    /// Feed bytes, returning complete event payloads (`data:` joined by `\n`).
    pub fn feed(&mut self, chunk: &str) -> Vec<String> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.find('\n') {
            let mut line = self.buf[..pos].to_string();
            self.buf.drain(..=pos);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / keepalive
            }
            if let Some(rest) = line.strip_prefix("data:") {
                self.data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // event:, id:, retry: are ignored
        }
        out
    }

    /// Flush a trailing event without a blank line.
    pub fn finish(&mut self) -> Option<String> {
        if !self.buf.trim().is_empty() {
            let line = std::mem::take(&mut self.buf);
            if let Some(rest) = line.trim_end().strip_prefix("data:") {
                self.data.push(rest.trim_start().to_string());
            }
        }
        if self.data.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.data).join("\n"))
        }
    }
}

#[derive(Debug, Clone, Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
    announced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkState {
    /// Haven't decided whether content starts with reasoning.
    Undecided,
    InThink,
    Content,
}

/// Assembles streamed chunks into events and a final [`ChatResponse`].
pub struct Assembler {
    content: String,
    reasoning: String,
    calls: Vec<PartialCall>,
    usage: Option<TokenUsage>,
    finish: Option<FinishReason>,
    model: Option<String>,
    /// Server sent reasoning in a dedicated field at least once.
    server_reasoning: bool,
    think: ThinkState,
    /// Model may emit `<think>` in content (strip client-side).
    strip_think: bool,
    /// Hold early content to detect a template-prefilled thought ending in `</think>`.
    hold_for_closer: bool,
    /// Content held back while deciding think-state or possible tool markup.
    pending: String,
    /// Content streamed to the UI so far (for fallback suppression).
    emitted_len: usize,
    /// Tool-call markup detected in content: stop streaming content.
    suppress: bool,
    tools_offered: bool,
    pub saw_done: bool,
}

impl Assembler {
    pub fn new(strip_think: bool, tools_offered: bool) -> Self {
        Self::with_options(strip_think, strip_think, tools_offered)
    }

    pub fn with_options(strip_think: bool, hold_for_closer: bool, tools_offered: bool) -> Self {
        Self {
            hold_for_closer: hold_for_closer && strip_think,
            content: String::new(),
            reasoning: String::new(),
            calls: Vec::new(),
            usage: None,
            finish: None,
            model: None,
            server_reasoning: false,
            think: if strip_think { ThinkState::Undecided } else { ThinkState::Content },
            strip_think,
            pending: String::new(),
            emitted_len: 0,
            suppress: false,
            tools_offered,
            saw_done: false,
        }
    }

    pub fn has_output(&self) -> bool {
        !self.content.is_empty() || !self.reasoning.is_empty() || !self.calls.is_empty() || !self.pending.is_empty()
    }

    /// Process one SSE payload. Returns events to forward.
    pub fn push(&mut self, payload: &str) -> Vec<ChatEvent> {
        let mut ev = Vec::new();
        let payload = payload.trim();
        if payload == "[DONE]" {
            self.saw_done = true;
            return ev;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else { return ev };
        if let Some(err) = v.get("error") {
            // mid-stream error object
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("stream error").to_string();
            self.finish = Some(FinishReason::Other);
            ev.push(ChatEvent::ContentDelta(String::new()));
            tracing::warn!("mid-stream error: {msg}");
            return ev;
        }
        if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
            self.model.get_or_insert_with(|| m.to_string());
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            let usage = parse_usage(u);
            self.usage = Some(usage);
            ev.push(ChatEvent::Usage(usage));
        }
        let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else { return ev };
        for choice in choices {
            if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                self.finish = Some(FinishReason::parse(fr));
            }
            // non-stream responses carry `message`, streams carry `delta`
            let delta = choice.get("delta").or_else(|| choice.get("message"));
            let Some(delta) = delta else { continue };
            for key in ["reasoning_content", "reasoning"] {
                if let Some(r) = delta.get(key).and_then(|r| r.as_str()) {
                    if !r.is_empty() {
                        self.server_reasoning = true;
                        self.reasoning.push_str(r);
                        ev.push(ChatEvent::ReasoningDelta(r.to_string()));
                        break; // some servers send both with the same text
                    }
                }
            }
            if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
                if !c.is_empty() {
                    self.on_content(c, &mut ev);
                }
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                for (pos, tc) in tcs.iter().enumerate() {
                    self.on_tool_delta(tc, pos, &mut ev);
                }
            }
        }
        ev
    }

    fn on_tool_delta(&mut self, tc: &Value, pos: usize, ev: &mut Vec<ChatEvent>) {
        let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
        let index = tc.get("index").and_then(|i| i.as_u64()).map(|i| i as usize).unwrap_or_else(|| {
            // No index: match by id, else treat as a new call when an id appears.
            if !id.is_empty() {
                self.calls.iter().position(|c| c.id == id).unwrap_or(self.calls.len())
            } else {
                self.calls.len().saturating_sub(1).max(pos.min(self.calls.len()))
            }
        });
        while self.calls.len() <= index {
            self.calls.push(PartialCall::default());
        }
        let call = &mut self.calls[index];
        if !id.is_empty() && call.id.is_empty() {
            call.id = id.to_string();
        }
        if let Some(f) = tc.get("function") {
            if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
                if !n.is_empty() {
                    if call.name.is_empty() {
                        call.name = n.to_string();
                    } else if !call.name.ends_with(n) && call.announced {
                        // name streamed in pieces (rare)
                        call.name.push_str(n);
                    }
                }
            }
            if !call.announced && !call.name.is_empty() {
                call.announced = true;
                if call.id.is_empty() {
                    call.id = format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
                }
                ev.push(ChatEvent::ToolCallStart { index, id: call.id.clone(), name: call.name.clone() });
            }
            match f.get("arguments") {
                Some(Value::String(a)) if !a.is_empty() => {
                    call.args.push_str(a);
                    ev.push(ChatEvent::ToolCallDelta { index, arguments: a.clone() });
                }
                Some(obj @ Value::Object(_)) => {
                    // some servers send parsed objects
                    let a = obj.to_string();
                    call.args.push_str(&a);
                    ev.push(ChatEvent::ToolCallDelta { index, arguments: a });
                }
                _ => {}
            }
        }
    }

    fn on_content(&mut self, c: &str, ev: &mut Vec<ChatEvent>) {
        if self.strip_think && !self.server_reasoning {
            self.pending.push_str(c);
            self.drain_pending(ev, false);
        } else {
            self.emit_content(c, ev);
        }
    }

    /// Resolve `<think>` handling for pending text.
    fn drain_pending(&mut self, ev: &mut Vec<ChatEvent>, at_end: bool) {
        loop {
            match self.think {
                ThinkState::Undecided => {
                    let t = self.pending.trim_start();
                    if t.starts_with("<think>") {
                        let lead = self.pending.len() - t.len();
                        self.pending.drain(..lead + "<think>".len());
                        self.think = ThinkState::InThink;
                        continue;
                    }
                    if "<think>".starts_with(t) && !t.is_empty() && !at_end {
                        return; // could still become <think>
                    }
                    // Template-prefilled `<think>`: output begins inside the thought and
                    // only shows `</think>`. Hold a bounded prefix to detect it.
                    if let Some(i) = self.pending.find("</think>") {
                        let thought: String = self.pending[..i].to_string();
                        self.pending.drain(..i + "</think>".len());
                        if !thought.is_empty() {
                            self.reasoning.push_str(&thought);
                            ev.push(ChatEvent::ReasoningDelta(thought));
                        }
                        self.think = ThinkState::Content;
                        let rest = self.pending.trim_start().to_string();
                        self.pending.clear();
                        if !rest.is_empty() {
                            self.emit_content(&rest, ev);
                        }
                        return;
                    }
                    if self.hold_for_closer && self.pending.len() < 1500 && !at_end {
                        return;
                    }
                    self.think = ThinkState::Content;
                }
                ThinkState::InThink => {
                    if let Some(i) = self.pending.find("</think>") {
                        let thought: String = self.pending[..i].to_string();
                        self.pending.drain(..i + "</think>".len());
                        if !thought.is_empty() {
                            self.reasoning.push_str(&thought);
                            ev.push(ChatEvent::ReasoningDelta(thought));
                        }
                        self.think = ThinkState::Content;
                        let rest = self.pending.trim_start().to_string();
                        self.pending = rest;
                        continue;
                    }
                    // stream thought text, keeping a tail that could be a partial closer
                    let keep = partial_suffix_len(&self.pending, "</think>");
                    let flush_to = if at_end { self.pending.len() } else { self.pending.len() - keep };
                    if flush_to > 0 {
                        let thought: String = self.pending.drain(..flush_to).collect();
                        self.reasoning.push_str(&thought);
                        ev.push(ChatEvent::ReasoningDelta(thought));
                    }
                    return;
                }
                ThinkState::Content => {
                    if !self.pending.is_empty() {
                        let p = std::mem::take(&mut self.pending);
                        self.emit_content(&p, ev);
                    }
                    return;
                }
            }
        }
    }

    fn emit_content(&mut self, c: &str, ev: &mut Vec<ChatEvent>) {
        self.content.push_str(c);
        if self.suppress {
            return;
        }
        if self.tools_offered && self.calls.is_empty() {
            if let Some(m) = fallback::marker_start(&self.content) {
                // stream only up to the marker; hold the rest
                if m > self.emitted_len {
                    let safe = self.content[self.emitted_len..m].to_string();
                    self.emitted_len = m;
                    ev.push(ChatEvent::ContentDelta(safe));
                }
                // a complete marker means real tool markup: suppress for good
                let tail = &self.content[m..];
                if tail.len() >= 12 || !could_be_partial_marker(tail) {
                    self.suppress = true;
                }
                return;
            }
        }
        if self.content.len() > self.emitted_len {
            let s = self.content[self.emitted_len..].to_string();
            self.emitted_len = self.content.len();
            ev.push(ChatEvent::ContentDelta(s));
        }
    }

    /// Finish the stream: flush pending text, apply fallback parsing.
    pub fn finish(mut self, tools: &[crate::types::ToolSpec]) -> (ChatResponse, Vec<ChatEvent>) {
        let mut ev = Vec::new();
        if self.strip_think && !self.server_reasoning {
            self.drain_pending(&mut ev, true);
        }
        if !self.pending.is_empty() {
            let p = std::mem::take(&mut self.pending);
            self.emit_content(&p, &mut ev);
        }
        // Stray `</think>` left in content: either a duplicate (server parsed
        // reasoning) or a long prefilled thought that outran the hold window.
        if let Some(i) = self.content.find("</think>") {
            if !self.server_reasoning && self.strip_think {
                let thought = self.content[..i].trim_start_matches("<think>").to_string();
                self.reasoning.push_str(&thought);
            }
            self.content = self.content[i + 8..].trim_start().to_string();
        }
        let mut calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .map(|c| ToolCall {
                id: if c.id.is_empty() {
                    format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
                } else {
                    c.id
                },
                name: c.name,
                arguments: if c.args.trim().is_empty() { "{}".into() } else { c.args },
            })
            .collect();
        let mut fallback_parsed = false;
        let mut content = self.content;
        if calls.is_empty() && !tools.is_empty() {
            let r = fallback::parse(&content, tools);
            if !r.calls.is_empty() {
                calls = r.calls;
                content = r.content;
                fallback_parsed = true;
            }
        }
        if !fallback_parsed && self.suppress && content.len() > self.emitted_len {
            // markup turned out not to be a tool call: release the held text
            ev.push(ChatEvent::ContentDelta(content[self.emitted_len..].to_string()));
        }
        let finish = if !calls.is_empty() && self.finish != Some(FinishReason::Length) {
            Some(FinishReason::ToolCalls)
        } else {
            self.finish
        };
        let resp = ChatResponse {
            content,
            reasoning: self.reasoning,
            tool_calls: calls,
            finish_reason: finish,
            usage: self.usage,
            fallback_parsed,
            model: self.model,
            ttft_ms: None,
            total_ms: 0,
            retries: 0,
        };
        (resp, ev)
    }
}

fn partial_suffix_len(s: &str, marker: &str) -> usize {
    for k in (1..marker.len()).rev() {
        if s.ends_with(&marker[..k]) {
            return k;
        }
    }
    0
}

fn could_be_partial_marker(tail: &str) -> bool {
    ["<tool_call>", "<function=", "[TOOL_CALLS]", "<|tool_call", "<|python_tag|>", "<｜tool"]
        .iter()
        .any(|m| m.starts_with(tail) || tail.starts_with(m))
        && tail.len() < 12
}

pub fn parse_usage(u: &Value) -> TokenUsage {
    let g = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let cached =
        u.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")).and_then(|v| v.as_u64()).unwrap_or(0);
    let reasoning = u
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let input = g("prompt_tokens");
    let output = g("completion_tokens");
    let total = if g("total_tokens") > 0 { g("total_tokens") } else { input + output };
    TokenUsage {
        input_tokens: input,
        cached_input_tokens: cached,
        output_tokens: output,
        reasoning_tokens: reasoning,
        total_tokens: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolSpec;
    use serde_json::json;

    fn chunk(delta: Value) -> String {
        json!({"choices": [{"index": 0, "delta": delta}]}).to_string()
    }

    #[test]
    fn sse_decoder_splits() {
        let mut d = SseDecoder::default();
        let mut got = d.feed("data: {\"a\":1}\n\nda");
        got.extend(d.feed("ta: [DONE]\r\n\r\n: keepalive\n\n"));
        assert_eq!(got, vec!["{\"a\":1}".to_string(), "[DONE]".to_string()]);
    }

    #[test]
    fn assembles_tool_calls_by_index() {
        let mut a = Assembler::new(false, true);
        a.push(&chunk(
            json!({"tool_calls": [{"index": 0, "id": "c1", "function": {"name": "f", "arguments": "{\"x\""}}]}),
        ));
        a.push(&chunk(json!({"tool_calls": [{"index": 1, "id": "c2", "function": {"name": "g", "arguments": ""}}]})));
        a.push(&chunk(json!({"tool_calls": [{"index": 0, "function": {"arguments": ": 1}"}}]})));
        a.push(&chunk(json!({"tool_calls": [{"index": 1, "function": {"arguments": "{}"}}]})));
        a.push(&json!({"choices": [{"index":0,"delta":{},"finish_reason":"tool_calls"}], "usage": {"prompt_tokens": 10, "completion_tokens": 5}}).to_string());
        let (r, _) = a.finish(&[]);
        assert_eq!(r.tool_calls.len(), 2);
        assert_eq!(r.tool_calls[0].arguments, "{\"x\": 1}");
        assert_eq!(r.tool_calls[1].name, "g");
        assert_eq!(r.usage.unwrap().input_tokens, 10);
        assert_eq!(r.finish_reason, Some(FinishReason::ToolCalls));
    }

    #[test]
    fn reasoning_fields() {
        let mut a = Assembler::new(true, false);
        let ev = a.push(&chunk(json!({"reasoning": "hmm"})));
        assert_eq!(ev, vec![ChatEvent::ReasoningDelta("hmm".into())]);
        a.push(&chunk(json!({"reasoning_content": " more"})));
        a.push(&chunk(json!({"content": "answer"})));
        let (r, _) = a.finish(&[]);
        assert_eq!(r.reasoning, "hmm more");
        assert_eq!(r.content, "answer");
    }

    #[test]
    fn strips_think_tags_split_across_chunks() {
        let mut a = Assembler::new(true, false);
        let mut evs = Vec::new();
        for part in ["<thi", "nk>pla", "nning</th", "ink>\n\nHello", " world"] {
            evs.extend(a.push(&chunk(json!({"content": part}))));
        }
        let (r, tail) = a.finish(&[]);
        evs.extend(tail);
        assert_eq!(r.reasoning, "planning");
        assert_eq!(r.content, "Hello world");
        let content: String = evs
            .iter()
            .filter_map(|e| if let ChatEvent::ContentDelta(c) = e { Some(c.clone()) } else { None })
            .collect();
        assert_eq!(content, "Hello world");
    }

    #[test]
    fn prefilled_think_only_closer() {
        let mut a = Assembler::new(true, false);
        a.push(&chunk(json!({"content": "reasoning here</think>Final"})));
        let (r, _) = a.finish(&[]);
        assert_eq!(r.reasoning, "reasoning here");
        assert_eq!(r.content, "Final");
    }

    #[test]
    fn no_think_passes_through_at_end() {
        let mut a = Assembler::new(true, false);
        a.push(&chunk(json!({"content": "short"})));
        let (r, ev) = a.finish(&[]);
        assert_eq!(r.content, "short");
        assert_eq!(ev, vec![ChatEvent::ContentDelta("short".into())]);
    }

    #[test]
    fn fallback_tool_markup_is_suppressed_and_parsed() {
        let tools =
            vec![ToolSpec { name: "ls".into(), description: String::new(), parameters: json!({"type":"object"}) }];
        let mut a = Assembler::new(false, true);
        let mut evs = Vec::new();
        for part in ["Checking. ", "<tool_", "call>{\"name\":\"ls\",", "\"arguments\":{}}</tool_call>"] {
            evs.extend(a.push(&chunk(json!({"content": part}))));
        }
        let (r, tail) = a.finish(&tools);
        evs.extend(tail);
        assert!(r.fallback_parsed);
        assert_eq!(r.tool_calls[0].name, "ls");
        assert_eq!(r.content, "Checking.");
        let streamed: String = evs
            .iter()
            .filter_map(|e| if let ChatEvent::ContentDelta(c) = e { Some(c.clone()) } else { None })
            .collect();
        assert_eq!(streamed, "Checking. ");
    }

    #[test]
    fn usage_details() {
        let u = parse_usage(
            &json!({"prompt_tokens": 100, "completion_tokens": 20, "prompt_tokens_details": {"cached_tokens": 64}}),
        );
        assert_eq!(u.cached_input_tokens, 64);
        assert_eq!(u.total_tokens, 120);
    }
}
