//! Integration tests against the stdio test server (`tests/support/test_server.rs`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use odex_mcp_client::{McpError, McpEvent, McpManager};
use odex_protocol::config_types::McpServerToml;
use odex_protocol::{ElicitationResponse, McpServerState};
use serde_json::json;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

const SERVER: &str = env!("CARGO_BIN_EXE_odex-mcp-test-server");

fn stdio(args: &[&str]) -> McpServerToml {
    McpServerToml {
        command: Some(SERVER.to_string()),
        args: args.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn manager() -> (McpManager, UnboundedReceiver<McpEvent>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = unbounded_channel();
    (McpManager::new(dir.path().join("tokens.json"), tx), rx, dir)
}

fn servers(list: Vec<(&str, McpServerToml)>) -> BTreeMap<String, McpServerToml> {
    list.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn state(m: &McpManager, name: &str) -> (McpServerState, Option<String>) {
    let s = m.statuses().into_iter().find(|s| s.name == name).expect("server status");
    (s.state, s.error)
}

async fn wait_for<T>(rx: &mut UnboundedReceiver<McpEvent>, mut f: impl FnMut(McpEvent) -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let ev = rx.recv().await.expect("event channel closed");
            if let Some(t) = f(ev) {
                return t;
            }
        }
    })
    .await
    .expect("timed out waiting for an event")
}

fn mcp_err(e: &anyhow::Error) -> &McpError {
    e.downcast_ref::<McpError>().unwrap_or_else(|| panic!("not an McpError: {e:#}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_list_call_resources_prompts() {
    let (m, mut rx, _dir) = manager();
    m.set_servers(servers(vec![("test", stdio(&[]))])).await;
    let status = m.statuses().remove(0);
    assert_eq!(status.state, McpServerState::Ready, "{:?}", status.error);
    assert_eq!(status.transport, "stdio");
    assert_eq!(status.server_name.as_deref(), Some("odex-test-server"));
    assert_eq!(status.server_version.as_deref(), Some("1.2.3"));
    assert_eq!(status.resources.len(), 1);
    assert_eq!(status.prompts[0].name, "greet");
    assert!(status.tools.iter().any(|t| t.qualified_name == "mcp__test__echo" && t.read_only_hint));

    let tools = m.tools();
    let names: Vec<&str> = tools.iter().map(|t| t.qualified_name.as_str()).collect();
    for expected in ["mcp__test__echo", "mcp__test__add", "mcp__test__slow", "mcp__test__complex"] {
        assert!(names.contains(&expected), "{names:?}");
    }
    assert!(!names.contains(&"mcp__test__dynamic"));

    // sanitized vs original schema
    let complex = m.find_tool("mcp__test__complex").unwrap();
    assert!(complex.input_schema.get("definitions").is_some());
    let s = &complex.sanitized_schema;
    assert_eq!(s["type"], "object");
    assert!(s.get("definitions").is_none() && s.get("$schema").is_none());
    assert_eq!(s["properties"]["target"]["properties"]["path"]["type"], "string");
    assert_eq!(s["properties"]["mode"], json!({"type": "string", "enum": ["fast", "safe"]}));
    assert_eq!(s["properties"]["limit"], json!({"type": "integer"}));

    // calls
    let r = m.call_tool("mcp__test__echo", json!({"text": "hi"})).await.unwrap();
    assert_eq!(r.to_text(), "hi");
    assert!(!r.is_error);
    let r = m.call_tool("mcp__test__add", json!({"a": 1, "b": 2})).await.unwrap();
    assert_eq!(r.to_text(), "3");
    assert_eq!(r.structured_content, Some(json!({"sum": 3.0})));
    let r = m.call_tool("mcp__test__fail", json!({})).await.unwrap();
    assert!(r.is_error);
    assert_eq!(r.to_text(), "something went wrong");
    let r = m.call_tool("mcp__test__ping_client", json!({})).await.unwrap();
    assert_eq!(r.to_text(), "pong");
    let r = m.call_tool("mcp__test__image", serde_json::Value::Null).await.unwrap();
    assert_eq!(r.images(), vec![("image/png".to_string(), "iVBORw0KGgo=".to_string())]);
    let r = m.call_tool("mcp__test__complex", json!({"target": {"path": "a.txt"}, "mode": "fast"})).await.unwrap();
    assert_eq!(r.to_text(), r#"{"target":{"path":"a.txt"},"mode":"fast"}"#);

    // argument validation against the original schema
    let e = m.call_tool("mcp__test__add", json!({"a": "x"})).await.unwrap_err();
    match mcp_err(&e) {
        McpError::InvalidArguments { message, .. } => {
            assert!(message.contains("`a`: expected number, got string \"x\""), "{message}");
            assert!(message.contains("missing required property `b`"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    let e = m.call_tool("mcp__test__complex", json!({"target": {}})).await.unwrap_err();
    assert!(e.to_string().contains("missing required property `target.path`"), "{e}");
    let e = m.call_tool("mcp__test__nope", json!({})).await.unwrap_err();
    assert!(matches!(mcp_err(&e), McpError::UnknownTool(_)));

    // progress + log notifications
    let r = m.call_tool("mcp__test__progress", json!({})).await.unwrap();
    assert_eq!(r.to_text(), "progressed");
    let (progress, total, message) = wait_for(&mut rx, |ev| match ev {
        McpEvent::Progress { server, progress, total, message, .. } if server == "test" && progress == 2.0 => {
            Some((progress, total, message))
        }
        _ => None,
    })
    .await;
    assert_eq!((progress, total, message.as_deref()), (2.0, Some(2.0), Some("step 2")));
    m.call_tool("mcp__test__log", json!({})).await.unwrap();
    wait_for(&mut rx, |ev| match ev {
        McpEvent::Log { line, .. } if line == "[warning] test: hello log" => Some(()),
        _ => None,
    })
    .await;
    let logs = m.logs("test");
    assert!(logs.iter().any(|l| l == "test server starting"), "{logs:?}");

    // resources / prompts / instructions
    let res = m.list_resources("test").await.unwrap();
    assert_eq!(res[0].uri, "test://greeting");
    assert_eq!(res[0].mime_type.as_deref(), Some("text/plain"));
    let content = m.read_resource("test", "test://greeting").await.unwrap();
    assert_eq!(content["contents"][0]["text"], "Hello from the test server");
    let e = m.read_resource("test", "test://missing").await.unwrap_err();
    assert!(matches!(mcp_err(&e), McpError::Rpc { code: -32002, .. }));
    let prompt = m.get_prompt("test", "greet", json!({"name": "Bob"})).await.unwrap();
    assert_eq!(prompt["messages"][0]["content"]["text"], "Please greet Bob");
    assert_eq!(m.instructions(), vec![("test".to_string(), "Use echo for testing.".to_string())]);
    assert!(m.list_resources("other").await.is_err());

    m.shutdown().await;
    assert!(m.statuses().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tool_timeout_then_recovers() {
    let (m, _rx, _dir) = manager();
    let cfg = McpServerToml { tool_timeout_ms: Some(300), ..stdio(&[]) };
    m.set_servers(servers(vec![("t", cfg)])).await;
    let started = Instant::now();
    let e = m.call_tool("mcp__t__slow", json!({"ms": 5000})).await.unwrap_err();
    assert!(matches!(mcp_err(&e), McpError::Timeout { ms: 300, .. }), "{e}");
    assert!(started.elapsed() < Duration::from_secs(3));
    // the server keeps working and receives the cancellation
    let r = m.call_tool("mcp__t__echo", json!({"text": "still alive"})).await.unwrap();
    assert_eq!(r.to_text(), "still alive");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(m.logs("t").iter().any(|l| l.starts_with("cancelled:") && l.contains("timeout")), "{:?}", m.logs("t"));

    // A caller that gives up (dropped future) also cancels the request.
    let m2 = m.clone();
    let call = tokio::spawn(async move { m2.call_tool("mcp__t__slow", json!({"ms": 250})).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    call.abort();
    let _ = call.await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        m.logs("t").iter().any(|l| l.starts_with("cancelled:") && l.contains("cancelled by the client")),
        "{:?}",
        m.logs("t")
    );
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_changed_refreshes_tools() {
    let (m, mut rx, _dir) = manager();
    m.set_servers(servers(vec![("test", stdio(&[]))])).await;
    assert!(m.find_tool("mcp__test__dynamic").is_none());
    m.call_tool("mcp__test__add_tool", json!({})).await.unwrap();
    wait_for(&mut rx, |ev| match ev {
        McpEvent::ToolsChanged { server } if server == "test" && m.find_tool("mcp__test__dynamic").is_some() => {
            Some(())
        }
        _ => None,
    })
    .await;
    let r = m.call_tool("mcp__test__dynamic", json!({})).await.unwrap();
    assert_eq!(r.to_text(), "dynamic!");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_spawns_a_new_process() {
    let (m, _rx, _dir) = manager();
    m.set_servers(servers(vec![("test", stdio(&[]))])).await;
    let pid1 = m.call_tool("mcp__test__pid", json!({})).await.unwrap().to_text();
    m.restart("test").await.unwrap();
    assert_eq!(state(&m, "test").0, McpServerState::Ready);
    let pid2 = m.call_tool("mcp__test__pid", json!({})).await.unwrap().to_text();
    assert_ne!(pid1, pid2);
    assert!(m.logs("test").iter().filter(|l| l.as_str() == "test server starting").count() >= 2);
    assert!(m.restart("missing").await.is_err());
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_servers_do_not_block_others() {
    let (m, _rx, _dir) = manager();
    let started = Instant::now();
    m.set_servers(servers(vec![
        ("good", stdio(&[])),
        ("missing", McpServerToml { command: Some("odex-definitely-not-a-command".into()), ..Default::default() }),
        ("crash", stdio(&["--crash"])),
        ("hang", McpServerToml { startup_timeout_ms: Some(1500), ..stdio(&["--hang"]) }),
        ("empty", McpServerToml::default()),
    ]))
    .await;
    // All start in parallel: bounded by the slowest (hang, 1.5 s), not the sum.
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());

    assert_eq!(state(&m, "good").0, McpServerState::Ready);
    let (st, err) = state(&m, "missing");
    assert_eq!(st, McpServerState::Failed);
    assert!(err.unwrap().contains("command not found"));
    let (st, err) = state(&m, "crash");
    assert_eq!(st, McpServerState::Failed);
    let err = err.unwrap();
    assert!(err.contains("crashing on purpose"), "{err}");
    let (st, err) = state(&m, "hang");
    assert_eq!(st, McpServerState::Failed);
    assert!(err.unwrap().contains("timed out"));
    let (st, err) = state(&m, "empty");
    assert_eq!(st, McpServerState::Failed);
    assert!(err.unwrap().contains("either `command`"));

    assert!(m.tools().iter().all(|t| t.server == "good"));
    let r = m.call_tool("mcp__good__echo", json!({"text": "ok"})).await.unwrap();
    assert_eq!(r.to_text(), "ok");
    assert!(m.logs("crash").iter().any(|l| l.contains("crashing on purpose")));
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn env_and_cwd_are_passed() {
    let (m, _rx, dir) = manager();
    let mut env = BTreeMap::new();
    env.insert("ODEX_MCP_TEST_VAR".to_string(), "hello-env".to_string());
    let cfg = McpServerToml { env, cwd: Some(dir.path().to_string_lossy().into_owned()), ..stdio(&[]) };
    m.set_servers(servers(vec![("e", cfg)])).await;
    let r = m.call_tool("mcp__e__env", json!({"name": "ODEX_MCP_TEST_VAR"})).await.unwrap();
    assert_eq!(r.to_text(), "hello-env");

    let bad = McpServerToml { cwd: Some(dir.path().join("nope").to_string_lossy().into_owned()), ..stdio(&[]) };
    m.set_servers(servers(vec![("e", bad)])).await;
    let (st, err) = state(&m, "e");
    assert_eq!(st, McpServerState::Failed);
    assert!(err.unwrap().contains("does not exist"));
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn elicitation_roundtrip() {
    let (m, mut rx, _dir) = manager();
    m.set_servers(servers(vec![("test", stdio(&[]))])).await;

    let m2 = m.clone();
    let call = tokio::spawn(async move { m2.call_tool("mcp__test__ask", json!({})).await });
    let (message, schema, respond) = wait_for(&mut rx, |ev| match ev {
        McpEvent::Elicitation { server, message, schema, respond } if server == "test" => {
            Some((message, schema, respond))
        }
        _ => None,
    })
    .await;
    assert_eq!(message, "What is your name?");
    assert_eq!(schema["properties"]["name"]["type"], "string");
    respond.send(ElicitationResponse { action: "accept".into(), content: Some(json!({"name": "Ada"})) }).unwrap();
    let r = call.await.unwrap().unwrap();
    assert_eq!(r.to_text(), "action=accept name=Ada");

    // dropping the responder answers `cancel`
    let m2 = m.clone();
    let call = tokio::spawn(async move { m2.call_tool("mcp__test__ask", json!({})).await });
    let respond = wait_for(&mut rx, |ev| match ev {
        McpEvent::Elicitation { respond, .. } => Some(respond),
        _ => None,
    })
    .await;
    drop(respond);
    let r = call.await.unwrap().unwrap();
    assert_eq!(r.to_text(), "action=cancel name=-");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn elicitation_pauses_the_tool_timeout() {
    let (m, mut rx, _dir) = manager();
    let cfg = McpServerToml { tool_timeout_ms: Some(300), ..stdio(&[]) };
    m.set_servers(servers(vec![("test", cfg)])).await;
    let m2 = m.clone();
    let call = tokio::spawn(async move { m2.call_tool("mcp__test__ask", json!({})).await });
    let respond = wait_for(&mut rx, |ev| match ev {
        McpEvent::Elicitation { respond, .. } => Some(respond),
        _ => None,
    })
    .await;
    tokio::time::sleep(Duration::from_millis(900)).await; // the user takes a while
    respond.send(ElicitationResponse { action: "decline".into(), content: None }).unwrap();
    let r = call.await.unwrap().unwrap();
    assert_eq!(r.to_text(), "action=decline name=-");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disabled_servers_and_tool_filters() {
    let (m, mut rx, _dir) = manager();
    m.set_servers(servers(vec![
        ("off", McpServerToml { enabled: Some(false), ..stdio(&[]) }),
        (
            "test",
            McpServerToml {
                disabled_tools: vec!["fail".into()],
                auto_approve_tools: vec!["echo".into()],
                ..stdio(&[])
            },
        ),
    ]))
    .await;
    let (st, _) = state(&m, "off");
    assert_eq!(st, McpServerState::Disabled);
    assert!(!m.statuses().iter().find(|s| s.name == "off").unwrap().enabled);
    assert!(m.tools().iter().all(|t| t.server == "test"));
    assert!(m.tools().iter().all(|t| t.name != "fail"));
    let fail = m.find_tool("mcp__test__fail").unwrap();
    assert!(!fail.enabled);
    let e = m.call_tool("mcp__test__fail", json!({})).await.unwrap_err();
    assert!(matches!(mcp_err(&e), McpError::ToolDisabled(_)));
    assert!(m.find_tool("mcp__test__echo").unwrap().auto_approve);
    assert!(!m.find_tool("mcp__test__add").unwrap().auto_approve);
    let status = m.statuses().into_iter().find(|s| s.name == "test").unwrap();
    assert!(!status.tools.iter().find(|t| t.name == "fail").unwrap().enabled);

    // Changing only the filters keeps the same process.
    let pid = m.call_tool("mcp__test__pid", json!({})).await.unwrap().to_text();
    while rx.try_recv().is_ok() {}
    m.set_servers(servers(vec![
        ("off", McpServerToml { enabled: Some(false), ..stdio(&[]) }),
        ("test", McpServerToml { enabled_tools: Some(vec!["pid".into(), "fail".into()]), ..stdio(&[]) }),
    ]))
    .await;
    wait_for(&mut rx, |ev| matches!(ev, McpEvent::ToolsChanged { server } if server == "test").then_some(())).await;
    let names: Vec<String> = m.tools().into_iter().map(|t| t.name).collect();
    assert_eq!(names, vec!["fail".to_string(), "pid".to_string()]);
    assert_eq!(m.call_tool("mcp__test__pid", json!({})).await.unwrap().to_text(), pid);

    // Enabling the disabled server starts it.
    m.set_servers(servers(vec![("off", stdio(&[])), ("test", stdio(&[]))])).await;
    assert_eq!(state(&m, "off").0, McpServerState::Ready);
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_a_server_stops_it() {
    let (m, mut rx, _dir) = manager();
    m.set_servers(servers(vec![("a", stdio(&[])), ("b", stdio(&[]))])).await;
    assert_eq!(
        m.tools().iter().filter(|t| t.server == "a").count(),
        m.tools().iter().filter(|t| t.server == "b").count()
    );
    m.set_servers(servers(vec![("b", stdio(&[]))])).await;
    wait_for(&mut rx, |ev| match ev {
        McpEvent::Status(s) if s.name == "a" && s.state == McpServerState::Stopped => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(m.statuses().len(), 1);
    assert!(m.tools().iter().all(|t| t.server == "b"));
    assert!(m.logs("a").is_empty());
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crashed_server_reports_failed() {
    let (m, mut rx, _dir) = manager();
    m.set_servers(servers(vec![("test", stdio(&[]))])).await;
    // Simulate a crash by killing the server process from outside.
    let pid: u32 = m.call_tool("mcp__test__pid", json!({})).await.unwrap().to_text().parse().unwrap();
    kill_pid(pid);
    let err = wait_for(&mut rx, |ev| match ev {
        McpEvent::Status(s) if s.name == "test" && s.state == McpServerState::Failed => s.error,
        _ => None,
    })
    .await;
    assert!(err.contains("exited"), "{err}");
    assert!(m.tools().is_empty());
    let e = m.call_tool("mcp__test__echo", json!({"text": "x"})).await.unwrap_err();
    assert!(matches!(mcp_err(&e), McpError::UnknownTool(_)));
    m.restart("test").await.unwrap();
    assert_eq!(m.call_tool("mcp__test__echo", json!({"text": "back"})).await.unwrap().to_text(), "back");
    m.shutdown().await;
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    let status = std::process::Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output();
    #[cfg(not(windows))]
    let status = std::process::Command::new("kill").args(["-9", &pid.to_string()]).output();
    assert!(status.unwrap().status.success());
}

/// `npx`-style launchers are `.cmd` shims on Windows; `command = "name"` must find them.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn windows_cmd_shims_are_resolved() {
    let (m, _rx, dir) = manager();
    std::fs::write(dir.path().join("odex-shim-server.cmd"), format!("@\"{SERVER}\" %*\r\n")).unwrap();
    let path = format!("{};{}", dir.path().display(), std::env::var("PATH").unwrap_or_default());
    let cfg = McpServerToml {
        command: Some("odex-shim-server".into()),
        env: BTreeMap::from([("PATH".to_string(), path)]),
        ..Default::default()
    };
    m.set_servers(servers(vec![("shim", cfg)])).await;
    let (st, err) = state(&m, "shim");
    assert_eq!(st, McpServerState::Ready, "{err:?} {:?}", m.logs("shim"));
    assert_eq!(m.call_tool("mcp__shim__echo", json!({"text": "via cmd"})).await.unwrap().to_text(), "via cmd");
    m.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn superseded_startup_is_cancelled() {
    let (m, _rx, _dir) = manager();
    let hang = McpServerToml { startup_timeout_ms: Some(60_000), ..stdio(&["--hang"]) };
    let m2 = m.clone();
    let pending = tokio::spawn(async move { m2.set_servers(servers(vec![("h", hang)])).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(state(&m, "h").0, McpServerState::Starting);
    let started = Instant::now();
    m.set_servers(BTreeMap::new()).await;
    tokio::time::timeout(Duration::from_secs(5), pending).await.expect("startup was not cancelled").unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(m.statuses().is_empty());
}
