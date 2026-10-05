//! Token estimation between exact server counts.
//!
//! Ground truth is `usage.prompt_tokens` on every response. Between
//! responses we estimate with a per-model calibrated chars-per-token ratio
//! (updated from usage), and callers can ask vLLM's `/tokenize` for exact
//! counts near thresholds. (A local HF `tokenizer.json` path is accepted in
//! config; see docs/DECISIONS.md D-012 for why the tokenizers crate is not
//! linked by default.)

use crate::types::{ChatMessage, ContentPart, ToolSpec};

/// Per-message overhead of chat templates (role markers, separators).
pub const MESSAGE_OVERHEAD: u32 = 6;
/// Flat estimate for one image after the server's resize.
pub const IMAGE_TOKENS: u32 = 1100;

#[derive(Debug, Clone)]
pub struct TokenEstimator {
    /// Characters per token. Code-heavy chat with modern BPE ≈ 3.2–4.0.
    chars_per_token: f64,
    samples: u32,
}

impl Default for TokenEstimator {
    fn default() -> Self {
        Self { chars_per_token: 3.3, samples: 0 }
    }
}

impl TokenEstimator {
    pub fn with_ratio(r: f64) -> Self {
        Self { chars_per_token: r.clamp(1.5, 8.0), samples: 0 }
    }

    pub fn ratio(&self) -> f64 {
        self.chars_per_token
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// Conservative text estimate (rounds up).
    pub fn text(&self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        // count chars, weighting non-ASCII (CJK etc. ≈ 1 token/char)
        let mut ascii = 0usize;
        let mut other = 0usize;
        for c in s.chars() {
            if c.is_ascii() {
                ascii += 1;
            } else {
                other += 1;
            }
        }
        ((ascii as f64 / self.chars_per_token) + other as f64 * 0.9).ceil() as u32
    }

    pub fn message(&self, m: &ChatMessage) -> u32 {
        let mut n = MESSAGE_OVERHEAD;
        for p in &m.content {
            match p {
                ContentPart::Text { text } => n += self.text(text),
                ContentPart::ImageUrl { .. } => n += IMAGE_TOKENS,
            }
        }
        for c in &m.tool_calls {
            n += self.text(&c.name) + self.text(&c.arguments) + 8;
        }
        if let Some(r) = &m.reasoning {
            n += self.text(r);
        }
        n
    }

    pub fn messages(&self, ms: &[ChatMessage]) -> u32 {
        ms.iter().map(|m| self.message(m)).sum::<u32>() + 4
    }

    pub fn tools(&self, tools: &[ToolSpec]) -> u32 {
        tools
            .iter()
            .map(|t| self.text(&t.name) + self.text(&t.description) + self.text(&t.parameters.to_string()) + 12)
            .sum()
    }

    /// Update from an exact count of a known number of characters.
    /// Uses an EMA that converges quickly at first, then stabilizes.
    pub fn calibrate(&mut self, chars: usize, actual_tokens: u32) {
        if actual_tokens < 64 || chars < 256 {
            return;
        }
        let observed = (chars as f64 / actual_tokens as f64).clamp(1.5, 8.0);
        self.samples += 1;
        let alpha = if self.samples < 4 { 0.5 } else { 0.15 };
        self.chars_per_token = self.chars_per_token * (1.0 - alpha) + observed * alpha;
    }
}

/// Character count used for calibration (text parts + tool calls).
pub fn char_count(ms: &[ChatMessage], tools: &[ToolSpec]) -> usize {
    let mut n = 0usize;
    for m in ms {
        for p in &m.content {
            if let ContentPart::Text { text } = p {
                n += text.chars().count();
            }
        }
        for c in &m.tool_calls {
            n += c.name.len() + c.arguments.chars().count();
        }
        if let Some(r) = &m.reasoning {
            n += r.chars().count();
        }
        n += MESSAGE_OVERHEAD as usize * 3;
    }
    for t in tools {
        n += t.name.len() + t.description.chars().count() + t.parameters.to_string().len();
    }
    n
}

/// Image count in messages (images are excluded from char calibration).
pub fn image_count(ms: &[ChatMessage]) -> u32 {
    ms.iter().map(|m| m.image_count() as u32).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_and_calibrates() {
        let mut e = TokenEstimator::default();
        let text = "fn main() { println!(\"hello\"); }\n".repeat(50);
        let est = e.text(&text);
        assert!(est > 300 && est < 700, "{est}");
        // server says ratio is 4.0
        let chars = text.chars().count();
        for _ in 0..10 {
            e.calibrate(chars, (chars as f64 / 4.0) as u32);
        }
        assert!((e.ratio() - 4.0).abs() < 0.1, "{}", e.ratio());
    }

    #[test]
    fn non_ascii_heavier() {
        let e = TokenEstimator::default();
        assert!(e.text("日本語のテキスト") >= 7);
    }
}
