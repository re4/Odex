use odex_protocol::{ElicitationResponse, McpServerStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A tool exposed by an MCP server, as seen by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    /// Model-facing name `mcp__<server>__<tool>` (sanitized, <= 64 chars).
    pub qualified_name: String,
    pub server: String,
    /// Tool name as the server knows it.
    pub name: String,
    pub description: Option<String>,
    /// The schema exactly as the server sent it (used for validation).
    pub input_schema: Value,
    /// Flattened schema safe for vLLM chat templates (send this to the model).
    pub sanitized_schema: Value,
    pub read_only_hint: bool,
    /// `annotations.destructiveHint` (spec default: true unless read-only).
    pub destructive_hint: bool,
    /// Listed in the server's `auto_approve_tools`.
    pub auto_approve: bool,
    /// Allowed by `enabled_tools` / `disabled_tools`.
    pub enabled: bool,
}

/// Result of `tools/call`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallToolResult {
    /// Raw MCP content blocks (`text`, `image`, `audio`, `resource`, `resource_link`).
    pub content: Vec<Value>,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

impl CallToolResult {
    pub(crate) fn from_value(v: Value) -> Self {
        let content = match v.get("content") {
            Some(Value::Array(items)) => items.clone(),
            _ => Vec::new(),
        };
        CallToolResult {
            content,
            structured_content: v.get("structuredContent").filter(|s| !s.is_null()).cloned(),
            is_error: v.get("isError").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    /// Text rendering for the model: text blocks verbatim, other blocks as
    /// short placeholders. Falls back to the structured content as JSON.
    pub fn to_text(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for block in &self.content {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
            let mime = block.get("mimeType").and_then(Value::as_str).unwrap_or("unknown");
            match kind {
                "text" => parts.push(block.get("text").and_then(Value::as_str).unwrap_or("").to_string()),
                "image" => parts.push(format!("[image: {mime}]")),
                "audio" => parts.push(format!("[audio: {mime}]")),
                "resource" => {
                    let res = block.get("resource").cloned().unwrap_or(Value::Null);
                    if let Some(text) = res.get("text").and_then(Value::as_str) {
                        parts.push(text.to_string());
                    } else {
                        let uri = res.get("uri").and_then(Value::as_str).unwrap_or("?");
                        parts.push(format!("[resource: {uri}]"));
                    }
                }
                "resource_link" => {
                    let uri = block.get("uri").and_then(Value::as_str).unwrap_or("?");
                    match block.get("name").and_then(Value::as_str) {
                        Some(name) => parts.push(format!("[resource link: {name} <{uri}>]")),
                        None => parts.push(format!("[resource link: {uri}]")),
                    }
                }
                _ => parts.push(block.to_string()),
            }
        }
        if parts.is_empty() {
            if let Some(s) = &self.structured_content {
                return s.to_string();
            }
        }
        parts.join("\n")
    }

    /// `(mime, base64)` for every image block (and embedded image resources).
    pub fn images(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for block in &self.content {
            match block.get("type").and_then(Value::as_str) {
                Some("image") => {
                    if let Some(data) = block.get("data").and_then(Value::as_str) {
                        let mime = block.get("mimeType").and_then(Value::as_str).unwrap_or("image/png");
                        out.push((mime.to_string(), data.to_string()));
                    }
                }
                Some("resource") => {
                    let res = block.get("resource");
                    let mime = res.and_then(|r| r.get("mimeType")).and_then(Value::as_str).unwrap_or("");
                    let blob = res.and_then(|r| r.get("blob")).and_then(Value::as_str);
                    if let (true, Some(blob)) = (mime.starts_with("image/"), blob) {
                        out.push((mime.to_string(), blob.to_string()));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Events emitted to the host.
#[derive(Debug)]
pub enum McpEvent {
    /// A server's status changed (state, tools, error, ...).
    Status(McpServerStatus),
    /// The set of tools exposed by `server` changed (ready, list_changed, stopped).
    ToolsChanged { server: String },
    /// A new log line (stderr, protocol error, server log message, lifecycle).
    Log { server: String, line: String },
    /// `elicitation/create`: ask the user, then answer through `respond`.
    /// Dropping `respond` answers `cancel`.
    Elicitation {
        server: String,
        message: String,
        schema: Value,
        respond: tokio::sync::oneshot::Sender<ElicitationResponse>,
    },
    /// `notifications/progress` for an in-flight request.
    Progress { server: String, token: Value, progress: f64, total: Option<f64>, message: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn to_text_and_images() {
        let r = CallToolResult::from_value(json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                {"type": "resource", "resource": {"uri": "file:///a.txt", "text": "file body"}},
                {"type": "resource", "resource": {"uri": "file:///b.png", "blob": "BBBB", "mimeType": "image/jpeg"}},
                {"type": "resource_link", "uri": "file:///c", "name": "c"}
            ],
            "isError": true
        }));
        assert!(r.is_error);
        assert_eq!(
            r.to_text(),
            "hello\n[image: image/png]\nfile body\n[resource: file:///b.png]\n[resource link: c <file:///c>]"
        );
        assert_eq!(
            r.images(),
            vec![("image/png".to_string(), "AAAA".to_string()), ("image/jpeg".to_string(), "BBBB".to_string())]
        );
    }

    #[test]
    fn structured_fallback() {
        let r = CallToolResult::from_value(json!({"content": [], "structuredContent": {"sum": 3}}));
        assert_eq!(r.to_text(), r#"{"sum":3}"#);
        assert!(!r.is_error);
    }
}
