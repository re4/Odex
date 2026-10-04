//! Incremental `text/event-stream` parser (WHATWG server-sent events).

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

#[derive(Default)]
pub(crate) struct SseParser {
    line: Vec<u8>,
    after_cr: bool,
    started: bool,
    event: Option<String>,
    data: String,
    has_data: bool,
    id: Option<String>,
}

impl SseParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk; returns the events completed by it.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        for &b in chunk {
            match b {
                b'\n' if self.after_cr => {
                    self.after_cr = false;
                }
                b'\n' | b'\r' => {
                    self.after_cr = b == b'\r';
                    let line = std::mem::take(&mut self.line);
                    if let Some(ev) = self.process_line(&line) {
                        out.push(ev);
                    }
                }
                _ => {
                    self.after_cr = false;
                    self.line.push(b);
                }
            }
        }
        out
    }

    /// End of stream: dispatch a trailing event that lacked its blank line.
    pub(crate) fn finish(&mut self) -> Option<SseEvent> {
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            if let Some(ev) = self.process_line(&line) {
                return Some(ev);
            }
        }
        self.dispatch()
    }

    fn process_line(&mut self, raw: &[u8]) -> Option<SseEvent> {
        let mut line = String::from_utf8_lossy(raw).into_owned();
        if !self.started {
            self.started = true;
            if let Some(rest) = line.strip_prefix('\u{feff}') {
                line = rest.to_string();
            }
        }
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.find(':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line.as_str(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.id = Some(value.to_string()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        if !self.has_data {
            self.data.clear();
            return None;
        }
        self.has_data = false;
        Some(SseEvent { event, data: std::mem::take(&mut self.data), id: self.id.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events_across_chunks() {
        let mut p = SseParser::new();
        let mut events = Vec::new();
        let input =
            "\u{feff}: comment\r\nevent: message\r\nid: 7\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\ndata:x\n\ndata: tail";
        for chunk in input.as_bytes().chunks(3) {
            events.extend(p.feed(chunk));
        }
        events.extend(p.finish());
        assert_eq!(
            events,
            vec![
                SseEvent { event: Some("message".into()), data: "{\"a\":\n1}".into(), id: Some("7".into()) },
                SseEvent { event: None, data: "x".into(), id: Some("7".into()) },
                SseEvent { event: None, data: "tail".into(), id: Some("7".into()) },
            ]
        );
    }

    #[test]
    fn bare_cr_and_empty_events() {
        let mut p = SseParser::new();
        let events = p.feed(b"event: endpoint\rdata: /messages?s=1\r\revent: ping\n\n");
        assert_eq!(events, vec![SseEvent { event: Some("endpoint".into()), data: "/messages?s=1".into(), id: None }]);
    }
}
