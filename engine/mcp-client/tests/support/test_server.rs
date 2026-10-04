//! Tiny stdio MCP server for the integration tests.
//!
//! Flags: `--crash` (exit immediately with an error), `--hang` (never answer).
//! Tools: echo, add, slow, fail, env, ask (elicitation), add_tool
//! (list_changed), progress, log, image, pid, complex (schema with $ref/anyOf),
//! and `dynamic` once `add_tool` ran.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

struct Server {
    out: Mutex<std::io::Stdout>,
    pending: Mutex<HashMap<i64, mpsc::Sender<Value>>>,
    next_id: AtomicI64,
    extra_tool: AtomicBool,
}

impl Server {
    fn write(&self, msg: Value) {
        let mut out = self.out.lock().unwrap();
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }

    fn notify(&self, method: &str, params: Value) {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Server → client request; blocks until the client answers.
    fn request(&self, method: &str, params: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        rx.recv_timeout(Duration::from_secs(30)).unwrap_or(json!({"error": "timeout"}))
    }

    fn tools(&self) -> Vec<Value> {
        let mut tools = vec![
            json!({"name": "echo", "description": "Echo text back",
                   "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
                   "annotations": {"readOnlyHint": true}}),
            json!({"name": "add", "description": "Add two numbers",
                   "inputSchema": {"type": "object", "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
                                   "required": ["a", "b"]}}),
            json!({"name": "slow", "description": "Sleep for `ms` milliseconds",
                   "inputSchema": {"type": "object", "properties": {"ms": {"type": "integer"}}}}),
            json!({"name": "fail", "description": "Always fails", "inputSchema": {"type": "object"}}),
            json!({"name": "env", "description": "Read an environment variable",
                   "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}}),
            json!({"name": "ask", "description": "Ask the user for their name", "inputSchema": {"type": "object"}}),
            json!({"name": "add_tool", "description": "Register the `dynamic` tool", "inputSchema": {"type": "object"}}),
            json!({"name": "progress", "description": "Report progress", "inputSchema": {"type": "object"}}),
            json!({"name": "log", "description": "Emit a log message", "inputSchema": {"type": "object"}}),
            json!({"name": "image", "description": "Return an image", "inputSchema": {"type": "object"}}),
            json!({"name": "pid", "description": "Process id", "inputSchema": {"type": "object"}}),
            json!({"name": "ping_client", "description": "Ping the client", "inputSchema": {"type": "object"}}),
            json!({"name": "complex", "description": "Complex schema",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "target": {"$ref": "#/definitions/Target"},
                    "mode": {"anyOf": [{"const": "fast"}, {"const": "safe"}]},
                    "limit": {"anyOf": [{"type": "integer"}, {"type": "null"}], "default": null}
                },
                "required": ["target"],
                "definitions": {"Target": {"type": "object", "properties": {"path": {"type": "string"}},
                                           "required": ["path"]}}
            }}),
        ];
        if self.extra_tool.load(Ordering::SeqCst) {
            tools
                .push(json!({"name": "dynamic", "description": "Added at runtime", "inputSchema": {"type": "object"}}));
        }
        tools
    }

    fn text(t: impl Into<String>) -> Value {
        json!({"content": [{"type": "text", "text": t.into()}]})
    }

    fn call_tool(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params["name"].as_str().unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let token = params.get("_meta").and_then(|m| m.get("progressToken")).cloned();
        Ok(match name {
            "echo" => Self::text(args["text"].as_str().unwrap_or("")),
            "add" => {
                let sum = args["a"].as_f64().unwrap_or(0.0) + args["b"].as_f64().unwrap_or(0.0);
                json!({"content": [{"type": "text", "text": sum.to_string()}], "structuredContent": {"sum": sum}})
            }
            "slow" => {
                std::thread::sleep(Duration::from_millis(args["ms"].as_u64().unwrap_or(1000)));
                Self::text("done")
            }
            "fail" => json!({"content": [{"type": "text", "text": "something went wrong"}], "isError": true}),
            "env" => {
                let var = args["name"].as_str().unwrap_or("");
                Self::text(std::env::var(var).unwrap_or_else(|_| "<unset>".into()))
            }
            "ask" => {
                let answer = self.request(
                    "elicitation/create",
                    json!({"message": "What is your name?",
                           "requestedSchema": {"type": "object", "properties": {"name": {"type": "string"}},
                                               "required": ["name"]}}),
                );
                let result = &answer["result"];
                let action = result["action"].as_str().unwrap_or("?");
                let who = result["content"]["name"].as_str().unwrap_or("-");
                Self::text(format!("action={action} name={who}"))
            }
            "add_tool" => {
                self.extra_tool.store(true, Ordering::SeqCst);
                self.notify("notifications/tools/list_changed", json!({}));
                Self::text("added")
            }
            "progress" => {
                if let Some(token) = token {
                    for i in 1..=2 {
                        self.notify(
                            "notifications/progress",
                            json!({"progressToken": token, "progress": i, "total": 2, "message": format!("step {i}")}),
                        );
                    }
                }
                Self::text("progressed")
            }
            "log" => {
                self.notify(
                    "notifications/message",
                    json!({"level": "warning", "logger": "test", "data": "hello log"}),
                );
                Self::text("logged")
            }
            "image" => json!({"content": [
                {"type": "text", "text": "an image"},
                {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"}
            ]}),
            "pid" => Self::text(std::process::id().to_string()),
            "ping_client" => {
                let answer = self.request("ping", json!({}));
                Self::text(if answer.get("result") == Some(&json!({})) {
                    "pong".to_string()
                } else {
                    answer.to_string()
                })
            }
            "complex" => Self::text(args.to_string()),
            "dynamic" if self.extra_tool.load(Ordering::SeqCst) => Self::text("dynamic!"),
            other => return Err((-32602, format!("unknown tool: {other}"))),
        })
    }

    fn handle_request(&self, msg: Value) {
        let id = msg["id"].clone();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result: Result<Value, (i64, String)> = match msg["method"].as_str().unwrap_or("") {
            "initialize" => Ok(json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {"tools": {"listChanged": true}, "resources": {}, "prompts": {}, "logging": {}},
                "serverInfo": {"name": "odex-test-server", "version": "1.2.3"},
                "instructions": "Use echo for testing."
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": self.tools()})),
            "tools/call" => self.call_tool(&params),
            "resources/list" => Ok(json!({"resources": [
                {"uri": "test://greeting", "name": "greeting", "description": "A greeting", "mimeType": "text/plain"}
            ]})),
            "resources/read" => match params["uri"].as_str() {
                Some("test://greeting") => Ok(json!({"contents": [
                    {"uri": "test://greeting", "mimeType": "text/plain", "text": "Hello from the test server"}
                ]})),
                _ => Err((-32002, "resource not found".into())),
            },
            "prompts/list" => Ok(json!({"prompts": [
                {"name": "greet", "description": "Greets someone", "arguments": [{"name": "name", "required": true}]}
            ]})),
            "prompts/get" => {
                let who = params["arguments"]["name"].as_str().unwrap_or("nobody").to_string();
                Ok(json!({"description": "Greeting", "messages": [
                    {"role": "user", "content": {"type": "text", "text": format!("Please greet {who}")}}
                ]}))
            }
            other => Err((-32601, format!("method not found: {other}"))),
        };
        let reply = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        };
        self.write(reply);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--crash") {
        eprintln!("fatal: crashing on purpose");
        std::process::exit(3);
    }
    if args.iter().any(|a| a == "--hang") {
        eprintln!("hanging forever");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    eprintln!("test server starting");
    let server = Arc::new(Server {
        out: Mutex::new(std::io::stdout()),
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicI64::new(1000),
        extra_tool: AtomicBool::new(false),
    });
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bad json: {e}");
                continue;
            }
        };
        let has_id = msg.get("id").is_some_and(|i| !i.is_null());
        if msg.get("method").is_some() {
            if has_id {
                let server = server.clone();
                std::thread::spawn(move || server.handle_request(msg));
            } else if msg["method"] == "notifications/cancelled" {
                eprintln!("cancelled: {}", msg["params"]);
            }
        } else if let Some(id) = msg.get("id").and_then(Value::as_i64) {
            if let Some(tx) = server.pending.lock().unwrap().remove(&id) {
                let _ = tx.send(msg);
            }
        }
    }
    eprintln!("stdin closed; exiting");
}
