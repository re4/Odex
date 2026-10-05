use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::mpsc::UnboundedSender;

use crate::types::McpEvent;
use crate::LOG_CAPACITY;

const MAX_LINE_LEN: usize = 4000;

/// Per-server log ring buffer that also forwards every line as an event.
pub(crate) struct LogSink {
    server: String,
    lines: Mutex<VecDeque<String>>,
    last_stderr: Mutex<Option<String>>,
    events: UnboundedSender<McpEvent>,
}

impl LogSink {
    pub(crate) fn new(server: &str, events: UnboundedSender<McpEvent>) -> Self {
        LogSink {
            server: server.to_string(),
            lines: Mutex::new(VecDeque::with_capacity(64)),
            last_stderr: Mutex::new(None),
            events,
        }
    }

    pub(crate) fn push(&self, line: impl Into<String>) {
        let mut line: String = line.into();
        if line.len() > MAX_LINE_LEN {
            let mut cut = MAX_LINE_LEN;
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            line.truncate(cut);
            line.push_str(" …");
        }
        {
            let mut buf = self.lines.lock().unwrap();
            if buf.len() >= LOG_CAPACITY {
                buf.pop_front();
            }
            buf.push_back(line.clone());
        }
        let _ = self.events.send(McpEvent::Log { server: self.server.clone(), line });
    }

    /// A line the server wrote to stderr.
    pub(crate) fn stderr(&self, line: &str) {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            return;
        }
        *self.last_stderr.lock().unwrap() = Some(trimmed.to_string());
        self.push(trimmed);
    }

    /// Lifecycle / client-side message.
    pub(crate) fn info(&self, msg: impl std::fmt::Display) {
        self.push(format!("[odex] {msg}"));
    }

    /// Protocol or transport error.
    pub(crate) fn error(&self, msg: impl std::fmt::Display) {
        self.push(format!("[error] {msg}"));
    }

    pub(crate) fn last_stderr(&self) -> Option<String> {
        self.last_stderr.lock().unwrap().clone()
    }

    pub(crate) fn clear_last_stderr(&self) {
        *self.last_stderr.lock().unwrap() = None;
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_keeps_last_lines() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = LogSink::new("s", tx);
        for i in 0..(LOG_CAPACITY + 10) {
            sink.push(format!("line {i}"));
        }
        let lines = sink.lines();
        assert_eq!(lines.len(), LOG_CAPACITY);
        assert_eq!(lines[0], "line 10");
        assert!(matches!(rx.try_recv(), Ok(McpEvent::Log { .. })));
        sink.stderr("boom\r\n");
        assert_eq!(sink.last_stderr().as_deref(), Some("boom"));
    }
}
