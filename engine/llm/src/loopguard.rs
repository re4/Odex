//! Loop breaker: detects repeated identical tool calls and degenerate
//! repeated text, and produces nudges for the model.

use std::collections::VecDeque;

use crate::types::ToolCall;

#[derive(Debug, Clone, PartialEq)]
pub enum LoopVerdict {
    Ok,
    /// Inject this nudge into the conversation and continue.
    Nudge(String),
    /// Too many repeats even after nudging: stop the turn.
    Stop(String),
}

#[derive(Debug, Default)]
pub struct LoopGuard {
    recent: VecDeque<String>,
    nudges: u32,
    last_text_digest: Option<u64>,
    same_text: u32,
}

const WINDOW: usize = 12;
const NUDGE_AT: usize = 3;

impl LoopGuard {
    pub fn new() -> Self {
        Self::default()
    }

    fn signature(c: &ToolCall) -> String {
        // normalize JSON so key order / whitespace doesn't matter
        let args = serde_json::from_str::<serde_json::Value>(&c.arguments)
            .map(|v| canonical(&v))
            .unwrap_or_else(|_| c.arguments.trim().to_string());
        format!("{}|{}", c.name, args)
    }

    /// Observe one assistant step's tool calls.
    pub fn observe_calls(&mut self, calls: &[ToolCall]) -> LoopVerdict {
        if calls.is_empty() {
            return LoopVerdict::Ok;
        }
        let sig = calls.iter().map(Self::signature).collect::<Vec<_>>().join("&");
        self.recent.push_back(sig.clone());
        while self.recent.len() > WINDOW {
            self.recent.pop_front();
        }
        // consecutive identical steps
        let consecutive = self.recent.iter().rev().take_while(|s| **s == sig).count();
        // A-B-A-B oscillation
        let oscillating = self.recent.len() >= 6 && {
            let v: Vec<&String> = self.recent.iter().rev().take(6).collect();
            v[0] == v[2] && v[2] == v[4] && v[1] == v[3] && v[3] == v[5] && v[0] != v[1]
        };
        if consecutive >= NUDGE_AT || oscillating {
            self.nudges += 1;
            if self.nudges > 3 {
                return LoopVerdict::Stop(format!(
                    "Stopped: the model kept repeating `{}` with the same arguments.",
                    calls[0].name
                ));
            }
            self.recent.clear();
            let what =
                if oscillating { "alternating between the same calls" } else { "calling it with identical arguments" };
            return LoopVerdict::Nudge(format!(
                "[loop detected] You have been {what} (`{}`) and the result will not change. \
                 Stop repeating it. Re-read the previous results, try a different approach, \
                 or explain to the user what is blocking you.",
                calls[0].name
            ));
        }
        LoopVerdict::Ok
    }

    /// Observe a final assistant text (no tool calls).
    pub fn observe_text(&mut self, text: &str) -> LoopVerdict {
        if degenerate_repetition(text) {
            self.nudges += 1;
            if self.nudges > 3 {
                return LoopVerdict::Stop("Stopped: the model is producing repeated text.".into());
            }
            return LoopVerdict::Nudge(
                "[loop detected] Your last message repeated the same text many times. \
                 Continue the task concisely without repeating yourself."
                    .into(),
            );
        }
        let digest = hash(text.trim());
        if Some(digest) == self.last_text_digest && !text.trim().is_empty() {
            self.same_text += 1;
        } else {
            self.same_text = 0;
        }
        self.last_text_digest = Some(digest);
        LoopVerdict::Ok
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn canonical(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys.iter().map(|k| format!("{k}:{}", canonical(&m[*k]))).collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

/// True when the tail of `text` is a short unit repeated many times, or one
/// line repeated many times — the classic degenerate-decoding failure.
pub fn degenerate_repetition(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < 200 {
        return false;
    }
    // repeated short unit at the tail
    let tail = &chars[n.saturating_sub(600)..];
    for unit in 1..=60usize {
        if tail.len() < unit * 10 {
            break;
        }
        let pat = &tail[tail.len() - unit..];
        let mut reps = 1;
        let mut pos = tail.len() - unit;
        while pos >= unit && &tail[pos - unit..pos] == pat {
            reps += 1;
            pos -= unit;
        }
        let needed = if unit <= 3 { 60 } else { 10 };
        if reps >= needed && !pat.iter().all(|c| c.is_whitespace() || *c == '-' || *c == '=' || *c == '─') {
            return true;
        }
    }
    // same non-trivial line repeated
    let lines: Vec<&str> = text.lines().map(|l| l.trim()).filter(|l| l.len() > 8).collect();
    if lines.len() >= 12 {
        let last = lines[lines.len() - 1];
        let reps = lines.iter().rev().take_while(|l| **l == last).count();
        if reps >= 10 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall { id: "x".into(), name: name.into(), arguments: args.into() }
    }

    #[test]
    fn identical_calls_nudge_then_stop() {
        let mut g = LoopGuard::new();
        assert_eq!(g.observe_calls(&[call("read_file", r#"{"path":"a"}"#)]), LoopVerdict::Ok);
        assert_eq!(g.observe_calls(&[call("read_file", r#"{ "path" : "a" }"#)]), LoopVerdict::Ok);
        assert!(matches!(g.observe_calls(&[call("read_file", r#"{"path":"a"}"#)]), LoopVerdict::Nudge(_)));
        let mut stopped = false;
        for _ in 0..20 {
            if let LoopVerdict::Stop(_) = g.observe_calls(&[call("read_file", r#"{"path":"a"}"#)]) {
                stopped = true;
                break;
            }
        }
        assert!(stopped);
    }

    #[test]
    fn different_args_ok() {
        let mut g = LoopGuard::new();
        for i in 0..10 {
            assert_eq!(g.observe_calls(&[call("read_file", &format!(r#"{{"path":"f{i}"}}"#))]), LoopVerdict::Ok);
        }
    }

    #[test]
    fn oscillation() {
        let mut g = LoopGuard::new();
        let mut nudged = false;
        for i in 0..6 {
            let c = if i % 2 == 0 { call("a", "{}") } else { call("b", "{}") };
            if let LoopVerdict::Nudge(_) = g.observe_calls(&[c]) {
                nudged = true;
            }
        }
        assert!(nudged);
    }

    #[test]
    fn degenerate_text() {
        assert!(degenerate_repetition(&format!("Hello. {}", "I will now fix it. ".repeat(40))));
        assert!(degenerate_repetition(&"ha".repeat(200)));
        assert!(!degenerate_repetition(
            &"a normal sentence about code. ".chars().cycle().take(150).collect::<String>()
        ));
        let table = format!("{}\n", "-".repeat(300));
        assert!(!degenerate_repetition(&table));
    }
}
