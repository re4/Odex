//! Folding CDP events into console/network logs and navigation counters.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::CdpEvent;

const MAX_CONSOLE: usize = 500;
const MAX_NETWORK: usize = 500;
const MAX_LINE_CHARS: usize = 2_000;

#[derive(Debug, Clone)]
struct NetEntry {
    id: String,
    method: String,
    url: String,
    kind: String,
    status: Option<i64>,
    size: Option<f64>,
    error: Option<String>,
}

/// Per-session record of what the page did.
#[derive(Debug)]
pub(crate) struct EventLog {
    console: VecDeque<String>,
    network: VecDeque<NetEntry>,
    inflight: HashSet<String>,
    last_net_activity: Instant,
    /// `Page.loadEventFired` count.
    pub load_count: u64,
    /// Main-frame document navigations that started.
    pub nav_started: u64,
    /// Main-frame navigations that committed (incl. same-document).
    pub nav_committed: u64,
    pub main_frame: Option<String>,
    /// The main-frame document was replaced since the flag was last cleared.
    pub doc_changed: bool,
    /// A JavaScript dialog is open: (type, message).
    pub pending_dialog: Option<(String, String)>,
}

impl Default for EventLog {
    fn default() -> Self {
        Self {
            console: VecDeque::new(),
            network: VecDeque::new(),
            inflight: HashSet::new(),
            last_net_activity: Instant::now(),
            load_count: 0,
            nav_started: 0,
            nav_committed: 0,
            main_frame: None,
            doc_changed: false,
            pending_dialog: None,
        }
    }
}

impl EventLog {
    pub fn apply(&mut self, ev: &CdpEvent) {
        let p = &ev.params;
        match ev.method.as_str() {
            "Runtime.consoleAPICalled" => {
                let kind = match p["type"].as_str().unwrap_or("log") {
                    "warning" => "warn",
                    "assert" => "error",
                    other => other,
                }
                .to_string();
                let args: Vec<String> =
                    p["args"].as_array().map(|a| a.iter().map(remote_object_text).collect()).unwrap_or_default();
                let mut line = format!("[{kind}] {}", args.join(" "));
                if let Some(frame) = p["stackTrace"]["callFrames"].as_array().and_then(|f| f.first()) {
                    if matches!(kind.as_str(), "error" | "warn") {
                        line.push_str(&location(frame));
                    }
                }
                self.push_console(line);
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let msg = d["exception"]["description"]
                    .as_str()
                    .or_else(|| d["exception"]["value"].as_str())
                    .or_else(|| d["text"].as_str())
                    .unwrap_or("exception");
                let first = msg.lines().next().unwrap_or(msg);
                let mut line = format!("[exception] {first}");
                if let Some(url) = d["url"].as_str().filter(|u| !u.is_empty()) {
                    line.push_str(&format!(
                        " ({url}:{}:{})",
                        d["lineNumber"].as_i64().unwrap_or(0) + 1,
                        d["columnNumber"].as_i64().unwrap_or(0) + 1
                    ));
                }
                self.push_console(line);
            }
            "Log.entryAdded" => {
                let e = &p["entry"];
                let level = e["level"].as_str().unwrap_or("info");
                if matches!(level, "error" | "warning") {
                    let mut line = format!(
                        "[{}] {}",
                        if level == "warning" { "warn" } else { "error" },
                        e["text"].as_str().unwrap_or_default()
                    );
                    if let Some(url) = e["url"].as_str().filter(|u| !u.is_empty()) {
                        line.push_str(&format!(" ({url})"));
                    }
                    self.push_console(line);
                }
            }
            "Network.requestWillBeSent" => {
                let id = p["requestId"].as_str().unwrap_or_default().to_string();
                let url = p["request"]["url"].as_str().unwrap_or_default().to_string();
                let kind = p["type"].as_str().unwrap_or("Other").to_string();
                if kind == "Document" && self.is_main(p["frameId"].as_str()) {
                    self.nav_started += 1;
                }
                if let Some(redirect) = p.get("redirectResponse").filter(|r| r.is_object()) {
                    if let Some(prev) = self.entry_mut(&id) {
                        prev.status = redirect["status"].as_i64();
                        prev.id = format!("{id}#redirected");
                    }
                }
                self.last_net_activity = Instant::now();
                if url.starts_with("data:") || url.starts_with("blob:") {
                    return;
                }
                self.inflight.insert(id.clone());
                if self.network.len() >= MAX_NETWORK {
                    self.network.pop_front();
                }
                self.network.push_back(NetEntry {
                    id,
                    method: p["request"]["method"].as_str().unwrap_or("GET").to_string(),
                    url,
                    kind,
                    status: None,
                    size: None,
                    error: None,
                });
            }
            "Network.responseReceived" => {
                let id = p["requestId"].as_str().unwrap_or_default();
                if let Some(e) = self.entry_mut(id) {
                    e.status = p["response"]["status"].as_i64();
                    if let Some(kind) = p["type"].as_str() {
                        e.kind = kind.to_string();
                    }
                }
                self.last_net_activity = Instant::now();
            }
            "Network.loadingFinished" => {
                let id = p["requestId"].as_str().unwrap_or_default().to_string();
                if let Some(e) = self.entry_mut(&id) {
                    e.size = p["encodedDataLength"].as_f64();
                }
                self.inflight.remove(&id);
                self.last_net_activity = Instant::now();
            }
            "Network.loadingFailed" => {
                let id = p["requestId"].as_str().unwrap_or_default().to_string();
                if let Some(e) = self.entry_mut(&id) {
                    e.error = Some(if p["canceled"].as_bool() == Some(true) {
                        "canceled".to_string()
                    } else {
                        p["errorText"].as_str().unwrap_or("failed").to_string()
                    });
                }
                self.inflight.remove(&id);
                self.last_net_activity = Instant::now();
            }
            "Page.loadEventFired" => self.load_count += 1,
            "Page.frameStartedLoading" => {
                if self.is_main(p["frameId"].as_str()) {
                    self.nav_started += 1;
                }
            }
            "Page.frameNavigated" => {
                let frame = &p["frame"];
                if frame.get("parentId").and_then(Value::as_str).is_none() {
                    self.main_frame = frame["id"].as_str().map(str::to_string);
                    self.nav_committed += 1;
                    self.doc_changed = true;
                    self.inflight.clear();
                }
            }
            "Page.navigatedWithinDocument" => {
                if self.is_main(p["frameId"].as_str()) {
                    self.nav_committed += 1;
                }
            }
            "Page.javascriptDialogOpening" => {
                let kind = p["type"].as_str().unwrap_or("alert").to_string();
                let message = p["message"].as_str().unwrap_or_default().to_string();
                self.push_console(format!("[dialog] {kind}: {message}"));
                self.pending_dialog = Some((kind, message));
            }
            "Page.javascriptDialogClosed" => self.pending_dialog = None,
            "Inspector.targetCrashed" | "Target.targetCrashed" => {
                self.push_console("[crash] the page crashed".to_string());
            }
            _ => {}
        }
    }

    /// How long the network has been quiet (zero while requests are in flight).
    pub fn network_idle_for(&self) -> Duration {
        if self.inflight.is_empty() {
            self.last_net_activity.elapsed()
        } else {
            Duration::ZERO
        }
    }

    pub fn console_text(&self, limit: usize) -> String {
        let skip = self.console.len().saturating_sub(limit);
        let lines: Vec<&str> = self.console.iter().skip(skip).map(String::as_str).collect();
        if lines.is_empty() {
            "(no console messages)".to_string()
        } else {
            lines.join("\n")
        }
    }

    pub fn network_text(&self, limit: usize, filter: Option<&str>) -> String {
        let filter = filter.map(str::to_ascii_lowercase);
        let matching: Vec<&NetEntry> = self
            .network
            .iter()
            .filter(|e| match &filter {
                Some(f) => e.url.to_ascii_lowercase().contains(f.as_str()),
                None => true,
            })
            .collect();
        let skip = matching.len().saturating_sub(limit);
        let lines: Vec<String> = matching
            .into_iter()
            .skip(skip)
            .map(|e| {
                let status = match (&e.error, e.status) {
                    (Some(err), _) => format!("ERR({err})"),
                    (None, Some(s)) => s.to_string(),
                    (None, None) => "pending".to_string(),
                };
                let size = e.size.map(human_size).unwrap_or_else(|| "-".to_string());
                format!("{} {status} {} {size} {}", e.method, e.kind, truncate(&e.url, 300))
            })
            .collect();
        if lines.is_empty() {
            "(no network requests)".to_string()
        } else {
            lines.join("\n")
        }
    }

    pub fn clear_console(&mut self) {
        self.console.clear();
    }

    pub fn clear_network(&mut self) {
        self.network.clear();
    }

    /// Forget per-page state (used when switching tabs).
    pub fn reset_page(&mut self) {
        self.inflight.clear();
        self.main_frame = None;
        self.pending_dialog = None;
        self.doc_changed = true;
    }

    fn is_main(&self, frame: Option<&str>) -> bool {
        match (&self.main_frame, frame) {
            (Some(main), Some(f)) => main == f,
            _ => false,
        }
    }

    fn entry_mut(&mut self, id: &str) -> Option<&mut NetEntry> {
        self.network.iter_mut().rev().find(|e| e.id == id)
    }

    fn push_console(&mut self, line: String) {
        if self.console.len() >= MAX_CONSOLE {
            self.console.pop_front();
        }
        self.console.push_back(truncate(&line, MAX_LINE_CHARS));
    }
}

fn location(frame: &Value) -> String {
    match frame["url"].as_str().filter(|u| !u.is_empty()) {
        Some(url) => format!(
            " ({url}:{}:{})",
            frame["lineNumber"].as_i64().unwrap_or(0) + 1,
            frame["columnNumber"].as_i64().unwrap_or(0) + 1
        ),
        None => String::new(),
    }
}

pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn human_size(bytes: f64) -> String {
    if bytes < 1024.0 {
        format!("{bytes:.0} B")
    } else if bytes < 1024.0 * 1024.0 {
        format!("{:.1} KB", bytes / 1024.0)
    } else {
        format!("{:.1} MB", bytes / (1024.0 * 1024.0))
    }
}

/// Render a `Runtime.RemoteObject` the way a console would, roughly.
pub(crate) fn remote_object_text(o: &Value) -> String {
    let scalar = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    };
    match o["type"].as_str().unwrap_or_default() {
        "string" => o["value"].as_str().unwrap_or_default().to_string(),
        "undefined" => "undefined".to_string(),
        "number" | "boolean" | "bigint" => {
            o.get("unserializableValue").or_else(|| o.get("value")).map(scalar).unwrap_or_default()
        }
        "object" if o["subtype"] == "null" => "null".to_string(),
        "object" => match o.get("preview") {
            Some(preview) => render_preview(preview),
            None => o["description"].as_str().unwrap_or("Object").to_string(),
        },
        _ => o["description"].as_str().map(|d| d.lines().next().unwrap_or(d).to_string()).unwrap_or_default(),
    }
}

fn render_preview(p: &Value) -> String {
    let props = p["properties"].as_array().cloned().unwrap_or_default();
    let more = if p["overflow"].as_bool() == Some(true) { ", …" } else { "" };
    let value = |prop: &Value| {
        let v = prop["value"].as_str().unwrap_or_default();
        if prop["type"] == "string" {
            format!("{v:?}")
        } else {
            v.to_string()
        }
    };
    if p["subtype"] == "array" {
        let items: Vec<String> = props.iter().map(value).collect();
        format!("[{}{more}]", items.join(", "))
    } else if matches!(p["subtype"].as_str(), Some("error") | Some("regexp") | Some("date")) {
        p["description"].as_str().unwrap_or_default().to_string()
    } else {
        let items: Vec<String> =
            props.iter().map(|prop| format!("{}: {}", prop["name"].as_str().unwrap_or("?"), value(prop))).collect();
        format!("{{{}{more}}}", items.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(method: &str, params: Value) -> CdpEvent {
        CdpEvent { method: method.to_string(), params }
    }

    #[test]
    fn console_and_exceptions() {
        let mut log = EventLog::default();
        log.apply(&ev(
            "Runtime.consoleAPICalled",
            json!({"type": "log", "args": [
                {"type": "string", "value": "hello"},
                {"type": "number", "value": 3},
                {"type": "object", "subtype": "null"},
                {"type": "object", "preview": {"properties": [{"name": "a", "type": "number", "value": "1"}, {"name": "b", "type": "string", "value": "x"}], "overflow": false}}
            ]}),
        ));
        log.apply(&ev(
            "Runtime.exceptionThrown",
            json!({"exceptionDetails": {"text": "Uncaught", "url": "http://x/app.js", "lineNumber": 4, "columnNumber": 2,
                "exception": {"description": "Error: boom\n    at f (app.js:5:3)"}}}),
        ));
        let text = log.console_text(10);
        assert_eq!(text, "[log] hello 3 null {a: 1, b: \"x\"}\n[exception] Error: boom (http://x/app.js:5:3)");
        assert_eq!(log.console_text(1), "[exception] Error: boom (http://x/app.js:5:3)");
        log.clear_console();
        assert_eq!(log.console_text(10), "(no console messages)");
    }

    #[test]
    fn network_and_navigation_tracking() {
        let mut log = EventLog::default();
        log.apply(&ev("Page.frameNavigated", json!({"frame": {"id": "F1", "url": "http://x/"}})));
        assert_eq!(log.main_frame.as_deref(), Some("F1"));
        assert!(log.doc_changed);
        log.apply(&ev(
            "Network.requestWillBeSent",
            json!({"requestId": "1", "type": "Document", "frameId": "F1", "request": {"method": "GET", "url": "http://x/next"}}),
        ));
        assert_eq!(log.nav_started, 1);
        log.apply(&ev(
            "Network.requestWillBeSent",
            json!({"requestId": "2", "type": "Fetch", "frameId": "F1", "request": {"method": "POST", "url": "http://x/api"}}),
        ));
        assert_eq!(log.network_idle_for(), Duration::ZERO);
        log.apply(&ev(
            "Network.responseReceived",
            json!({"requestId": "2", "type": "Fetch", "response": {"status": 201}}),
        ));
        log.apply(&ev("Network.loadingFinished", json!({"requestId": "2", "encodedDataLength": 2048.0})));
        log.apply(&ev("Network.loadingFailed", json!({"requestId": "1", "errorText": "net::ERR_ABORTED"})));
        assert!(log.inflight.is_empty());
        let text = log.network_text(10, None);
        assert_eq!(text, "GET ERR(net::ERR_ABORTED) Document - http://x/next\nPOST 201 Fetch 2.0 KB http://x/api");
        assert_eq!(log.network_text(10, Some("API")), "POST 201 Fetch 2.0 KB http://x/api");
        log.apply(&ev("Page.loadEventFired", json!({})));
        assert_eq!(log.load_count, 1);
        log.apply(&ev("Page.javascriptDialogOpening", json!({"type": "confirm", "message": "Sure?"})));
        assert_eq!(log.pending_dialog, Some(("confirm".to_string(), "Sure?".to_string())));
    }
}
