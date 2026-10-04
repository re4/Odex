//! Errors and vLLM error-message parsing.

use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub struct Overflow {
    /// The model's max context length.
    pub max_context: Option<u32>,
    /// Prompt (input) tokens, when reported.
    pub prompt_tokens: Option<u32>,
    /// Total requested (prompt + completion), when reported.
    pub requested: Option<u32>,
    /// The prompt fits; only `max_tokens` was too large (just lower it).
    pub max_tokens_only: bool,
    pub message: String,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LlmError {
    #[error("context window exceeded: {}", .0.message)]
    ContextOverflow(Overflow),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("cannot reach endpoint: {0}")]
    Connect(String),
    #[error("stream failed after {attempts} attempts: {last}")]
    Exhausted { attempts: u32, last: String },
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

impl LlmError {
    pub fn code(&self) -> &'static str {
        match self {
            LlmError::ContextOverflow(_) => "contextOverflow",
            LlmError::BadRequest(_) => "badRequest",
            LlmError::Unauthorized(_) => "unauthorized",
            LlmError::ModelNotFound(_) => "modelNotFound",
            LlmError::Http { .. } => "http",
            LlmError::Connect(_) => "endpointUnavailable",
            LlmError::Exhausted { .. } => "endpointUnavailable",
            LlmError::Cancelled => "interrupted",
            LlmError::Other(_) => "internal",
        }
    }
}

static MAX_CTX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)maximum (?:context|model) length (?:is|of) (\d+)").unwrap());
static REQUESTED: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)(?:you )?requested (\d+) tokens").unwrap());
static TOTAL: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)for a total of (?:at least )?(\d+) tokens").unwrap());
static MAX_TOKENS_GT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)max_(?:completion_)?tokens=(\d+) cannot be greater than max_model_len=(?:max_total_tokens=)?(\d+)")
        .unwrap()
});
static CHARS_PRECHECK: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)prompt contains (\d+) characters").unwrap());
static IN_MESSAGES: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\((\d+) in the messages").unwrap());
static INPUT_TOKENS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:request|prompt|input) (?:has|contains|is) (?:at least )?(\d+) (?:input |prompt )?tokens")
        .unwrap()
});
static MAX_TOKENS_TOO_LARGE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)'?max_(?:completion_)?tokens'?(?: or '?max_completion_tokens'?)? is too large").unwrap()
});
static PROMPT_LEN: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)prompt \(length (\d+)\) is longer than").unwrap());

/// Recognize vLLM / OpenAI-style context-length errors in an HTTP 400 body.
pub fn parse_overflow(body: &str) -> Option<Overflow> {
    let lower = body.to_lowercase();
    let looks = lower.contains("context length")
        || lower.contains("maximum model length")
        || lower.contains("context_length_exceeded")
        || lower.contains("is longer than the maximum")
        || (lower.contains("too large") && lower.contains("max_tokens"))
        || lower.contains("exceeds the model's")
        || lower.contains("cannot be greater than max_model_len");
    if !looks {
        return None;
    }
    let num = |re: &Regex| re.captures(body).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse::<u32>().ok());
    let mut max_context = num(&MAX_CTX);
    let requested = num(&TOTAL).or_else(|| num(&REQUESTED));
    let prompt_tokens = num(&IN_MESSAGES).or_else(|| num(&INPUT_TOKENS)).or_else(|| num(&PROMPT_LEN));
    let mut max_tokens_only = MAX_TOKENS_TOO_LARGE.is_match(body);
    if let Some(c) = MAX_TOKENS_GT.captures(body) {
        // max_tokens alone exceeds the window
        max_tokens_only = true;
        max_context = max_context.or_else(|| c.get(2).and_then(|m| m.as_str().parse().ok()));
    }
    if let (Some(max), Some(p)) = (max_context, prompt_tokens) {
        // prompt fits with room to spare → only the completion budget is the issue
        max_tokens_only = p + 16 < max;
    }
    if CHARS_PRECHECK.is_match(body) {
        max_tokens_only = false; // prompt text itself is too long
    }
    let message = extract_message(body);
    Some(Overflow { max_context, prompt_tokens, requested, max_tokens_only, message })
}

/// Pull `error.message` (or `message`/`detail`) out of a JSON error body.
pub fn extract_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        for path in [&["error", "message"][..], &["message"], &["detail"], &["error"]] {
            let mut cur = &v;
            let mut ok = true;
            for k in path {
                match cur.get(*k) {
                    Some(x) => cur = x,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                if let Some(s) = cur.as_str() {
                    return s.to_string();
                }
            }
        }
    }
    let t = body.trim();
    if t.len() > 500 {
        format!("{}…", &t[..t.char_indices().nth(500).map(|(i, _)| i).unwrap_or(t.len())])
    } else {
        t.to_string()
    }
}

/// Map common vLLM misconfiguration errors to a fix hint.
pub fn hint_for(body: &str) -> Option<&'static str> {
    let l = body.to_lowercase();
    if l.contains("enable-auto-tool-choice")
        || l.contains("auto\" tool choice requires")
        || l.contains("tool-call-parser")
    {
        Some("Start vLLM with --enable-auto-tool-choice --tool-call-parser <parser> (see Doctor).")
    } else if l.contains("chat template") {
        Some("The model has no chat template; pass --chat-template to vllm serve.")
    } else if l.contains("image") && (l.contains("not support") || l.contains("multimodal")) {
        Some("This model does not accept images; assign a vision model to the `vision` role.")
    } else if l.contains("guided_json") || l.contains("response_format") || l.contains("structured") {
        Some("Structured outputs are unsupported here; set structured_output = \"none\" for this model.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_vllm_message() {
        let body = r#"{"object":"error","message":"This model's maximum context length is 32768 tokens. However, you requested 34000 tokens (30000 in the messages, 4000 in the completion). Please reduce the length of the messages or completion.","type":"BadRequestError","param":null,"code":400}"#;
        let o = parse_overflow(body).unwrap();
        assert_eq!(o.max_context, Some(32768));
        assert_eq!(o.requested, Some(34000));
        assert_eq!(o.prompt_tokens, Some(30000));
        assert!(o.max_tokens_only);
        assert!(o.message.starts_with("This model's maximum"));
    }

    #[test]
    fn prompt_too_long() {
        let body = r#"{"error":{"message":"This model's maximum context length is 4096 tokens. However, you requested 5200 tokens (5100 in the messages, 100 in the completion).","code":400}}"#;
        let o = parse_overflow(body).unwrap();
        assert!(!o.max_tokens_only);
        assert_eq!(o.prompt_tokens, Some(5100));
    }

    #[test]
    fn newer_format() {
        let body = "'max_tokens' or 'max_completion_tokens' is too large: 8000. This model's maximum context length is 8192 tokens and your request has 600 input tokens (8000 > 8192 - 600).";
        let o = parse_overflow(body).unwrap();
        assert_eq!(o.max_context, Some(8192));
        assert_eq!(o.prompt_tokens, Some(600));
        assert!(o.max_tokens_only);
    }

    #[test]
    fn v018_formats() {
        let body = r#"{"error":{"message":"This model's maximum context length is 4096 tokens. However, you requested 512 output tokens and your prompt contains at least 3900 input tokens, for a total of at least 4412 tokens. Please reduce the length of the input prompt or the number of requested output tokens. (parameter=input_tokens, value=3900)","type":"BadRequestError","param":"input_tokens","code":400}}"#;
        let o = parse_overflow(body).unwrap();
        assert_eq!(o.max_context, Some(4096));
        assert_eq!(o.prompt_tokens, Some(3900));
        assert_eq!(o.requested, Some(4412));
        assert!(o.max_tokens_only);
        let body = "max_tokens=9000 cannot be greater than max_model_len=max_total_tokens=8192. Please request fewer output tokens. (parameter=max_tokens, value=9000)";
        let o = parse_overflow(body).unwrap();
        assert!(o.max_tokens_only);
        assert_eq!(o.max_context, Some(8192));
        let body = "This model's maximum context length is 4096 tokens. However, you requested 100 output tokens and your prompt contains 90000 characters (more than 40000 characters, which is the upper bound for 4096 input tokens).";
        let o = parse_overflow(body).unwrap();
        assert!(!o.max_tokens_only);
        let body = "This model's maximum context length is 4096 tokens. However, your request has 5000 input tokens. Please reduce the length of the input messages.";
        let o = parse_overflow(body).unwrap();
        assert_eq!(o.prompt_tokens, Some(5000));
        assert!(!o.max_tokens_only);
    }

    #[test]
    fn not_overflow() {
        assert!(parse_overflow(r#"{"message":"model foo not found"}"#).is_none());
    }

    #[test]
    fn hints() {
        assert!(hint_for("\"auto\" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set")
            .is_some());
    }
}
