//! End-to-end: a local fixture site driven through a real headless
//! Chromium-based browser. Skips (with a message) when no browser is installed.

use std::time::Duration;

use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use odex_browser_bridge::{
    find_browser, launch_headless_browser, BrowserOptions, BrowserSession, CdpTransport, WsTransport,
};
use serde_json::{json, Value};

const FIXTURE: &str = r#"<!doctype html>
<html><head><title>Odex Fixture</title></head>
<body>
<h1>Fixture page</h1>
<p>Some <b>static</b> text.</p>
<button id="btn" onclick="this.textContent = 'Clicked!'">Click me</button>
<p><label for="name">Name</label> <input id="name" type="text"></p>
<p><label for="pw">Password</label> <input id="pw" type="password"></p>
<p><label for="color">Color</label>
  <select id="color"><option value="r">Red</option><option value="g">Green</option></select></p>
<p><a href="/other">Other page</a></p>
<p><button id="ask" onclick="document.getElementById('answer').textContent = confirm('Delete everything?') ? 'confirmed' : 'cancelled'">Ask</button> <span id="answer">unanswered</span></p>
<p id="out">waiting</p>
<script>
console.log('fixture ready', 42);
fetch('/api/data').then(r => r.json()).then(d => { document.getElementById('out').textContent = d.message; });
</script>
</body></html>"#;

async fn serve_fixture() -> String {
    let app = Router::new()
        .route("/", get(|| async { Html(FIXTURE) }))
        .route("/api/data", get(|| async { Json(json!({ "message": "hello from api" })) }))
        .route("/other", get(|| async { Html("<title>Other</title><h1>Other page</h1><p>second page</p>") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// The `eN` ref on the first snapshot line containing `needle`.
fn find_ref(snapshot: &str, needle: &str) -> String {
    let line = snapshot
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle:?} in snapshot:\n{snapshot}"));
    let start = line.find("[e").expect("line has a ref") + 1;
    let end = start + line[start..].find(']').unwrap();
    line[start..end].to_string()
}

async fn snapshot<T: CdpTransport>(s: &BrowserSession<T>) -> String {
    let r = s.execute("snapshot", &json!({})).await;
    assert!(r.ok, "snapshot failed: {r:?}");
    r.text.unwrap()
}

fn ok(r: &odex_protocol::BrowserExecuteResponse, what: &str) {
    assert!(r.ok, "{what} failed: {:?}", r.error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drives_a_real_headless_browser() {
    let Some(browser) = find_browser() else {
        eprintln!("SKIPPED: no Chromium-based browser found (set ODEX_BROWSER to run this test)");
        return;
    };
    eprintln!("using browser {}", browser.display());
    let base = serve_fixture().await;
    let profile = tempfile::tempdir().unwrap();
    let (mut child, endpoint) = launch_headless_browser(profile.path()).await.expect("browser launches");

    let result = tokio::time::timeout(Duration::from_secs(120), exercise(&base, &endpoint)).await;

    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    result.expect("browser scenario finished in time");
}

async fn exercise(base: &str, endpoint: &str) {
    let transport = WsTransport::connect(endpoint).await.expect("connects over CDP");
    let opts = BrowserOptions { blocked_sites: vec!["blocked.test".into()], ..BrowserOptions::default() };
    let s = BrowserSession::new(transport, opts);

    // navigate (loopback is allowed by default)
    let r = s.execute("navigate", &json!({ "url": format!("{base}/") })).await;
    ok(&r, "navigate");
    assert_eq!(r.title.as_deref(), Some("Odex Fixture"));
    assert!(r.text.unwrap().contains("Navigated to"));

    // snapshot shows the outline with refs
    let snap = snapshot(&s).await;
    assert!(snap.contains("heading \"Fixture page\""), "{snap}");
    assert!(snap.contains("text: Some static text."), "{snap}");
    eprintln!("--- snapshot ---\n{snap}");
    assert!(!snap.contains("- text: Name\n"), "label text repeating a control name is dropped:\n{snap}");
    let button = find_ref(&snap, "button \"Click me\"");
    let link_line = snap.lines().find(|l| l.contains("link \"Other page\"")).unwrap();
    assert!(link_line.contains(&format!("href={base}/other")), "{link_line}");

    // click changes the button text
    let r = s.execute("click", &json!({ "ref": button })).await;
    ok(&r, "click");
    assert!(r.bounds.is_some());
    let snap = snapshot(&s).await;
    assert!(snap.contains("button \"Clicked!\""), "{snap}");

    // type into the text input
    let name = find_ref(&snap, "textbox \"Name\"");
    let r = s.execute("type", &json!({ "ref": name, "text": "Ada Lovelace" })).await;
    ok(&r, "type");
    let snap = snapshot(&s).await;
    assert!(snap.contains("textbox \"Name\"") && snap.contains("value=\"Ada Lovelace\""), "{snap}");
    // typing again replaces by default
    let name = find_ref(&snap, "textbox \"Name\"");
    ok(&s.execute("type", &json!({ "ref": name, "text": "Grace" })).await, "retype");
    let snap = snapshot(&s).await;
    assert!(snap.contains("value=\"Grace\"") && !snap.contains("Ada Lovelace"), "{snap}");

    // password fields are refused (by ref and by selector)
    let pw = find_ref(&snap, "textbox \"Password\"");
    let r = s.execute("type", &json!({ "ref": pw, "text": "hunter2" })).await;
    assert!(!r.ok);
    assert!(r.error.as_deref().unwrap().contains("password"), "{:?}", r.error);
    let r = s.execute("type", &json!({ "selector": "#pw", "text": "hunter2" })).await;
    assert!(!r.ok);

    // select an option by label
    let color = find_ref(&snap, "combobox \"Color\"");
    let r = s.execute("select", &json!({ "ref": color, "label": "Green" })).await;
    ok(&r, "select");
    assert!(r.text.unwrap().contains("value \"g\""));
    let snap = snapshot(&s).await;
    let color_line = snap.lines().find(|l| l.contains("combobox \"Color\"")).unwrap();
    assert!(color_line.contains("value=\"Green\""), "{color_line}");
    let r = s.execute("select", &json!({ "selector": "#color", "label": "Purple" })).await;
    assert!(r.error.unwrap().contains("Red"));

    // the fetch result shows up
    ok(&s.execute("wait", &json!({ "text": "hello from api" })).await, "wait text");

    // screenshot is a PNG data URL
    let r = s.execute("screenshot", &json!({})).await;
    ok(&r, "screenshot");
    let image = r.image.unwrap();
    let b64 = image.strip_prefix("data:image/png;base64,").expect("png data url");
    let png = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    assert!(png.len() > 1000 && png.starts_with(b"\x89PNG"));
    let r = s.execute("screenshot", &json!({ "full_page": true })).await;
    ok(&r, "full page screenshot");

    // console and network capture
    let r = s.execute("console", &json!({})).await;
    ok(&r, "console");
    assert!(r.text.as_deref().unwrap().contains("[log] fixture ready 42"), "{:?}", r.text);
    let r = s.execute("network", &json!({})).await;
    ok(&r, "network");
    let net = r.text.unwrap();
    eprintln!("--- network ---\n{net}");
    assert!(net.lines().any(|l| l.contains("/api/data") && l.contains(" 200 ") && l.starts_with("GET")), "{net}");

    // a confirm() dialog does not hang the click and is cancelled, never accepted
    let snap = snapshot(&s).await;
    let ask = find_ref(&snap, "button \"Ask\"");
    let r = s.execute("click", &json!({ "ref": ask })).await;
    ok(&r, "click opening a dialog");
    ok(&s.execute("wait", &json!({ "text": "cancelled" })).await, "dialog dismissed");
    let r = s.execute("console", &json!({})).await;
    assert!(r.text.unwrap().contains("[dialog] confirm: Delete everything?"));

    // eval is developer-mode only; blocked sites never load
    let r = s.execute("eval", &json!({ "expression": "document.title" })).await;
    assert!(!r.ok && r.error.unwrap().contains("developer mode"));
    let r = s.execute("navigate", &json!({ "url": "https://blocked.test/" })).await;
    assert!(!r.ok && r.error.unwrap().contains("blocked"));
    let r = s.execute("navigate", &json!({ "url": "https://unlisted.example/" })).await;
    assert!(!r.ok && r.error.unwrap().contains("approve"));

    // clicking a link navigates; back returns
    let snap = snapshot(&s).await;
    let link = find_ref(&snap, "link \"Other page\"");
    let r = s.execute("click", &json!({ "ref": link })).await;
    ok(&r, "click link");
    assert!(r.url.as_deref().unwrap().ends_with("/other"), "{:?}", r.url);
    let r = s.execute("click", &json!({ "ref": button })).await;
    assert!(r.error.unwrap().contains("snapshot"), "refs are invalidated by navigation");
    let r = s.execute("back", &json!({})).await;
    ok(&r, "back");
    assert_eq!(r.url.as_deref(), Some(format!("{base}/").as_str()));
    let r = s.execute("forward", &json!({})).await;
    ok(&r, "forward");
    assert!(r.url.as_deref().unwrap().ends_with("/other"));
    ok(&s.execute("back", &json!({})).await, "back again");

    // DOM-walk snapshot (the accessibility-tree fallback) yields usable refs too
    let r = s.execute("snapshot", &json!({ "mode": "dom" })).await;
    ok(&r, "dom snapshot");
    let dom = r.text.unwrap();
    eprintln!("--- dom snapshot ---\n{dom}");
    assert!(!dom.contains("- text: Name\n"), "{dom}");
    assert!(dom.contains("heading \"Fixture page\" [level=1]"), "{dom}");
    assert!(dom.contains("combobox \"Color\""), "{dom}");
    let dom_button = find_ref(&dom, "button \"Clicked!\"");
    ok(&s.execute("hover", &json!({ "ref": dom_button })).await, "hover dom ref");
    ok(&s.execute("click", &json!({ "ref": dom_button })).await, "click dom ref");
    let r = s.execute("type", &json!({ "selector": "[data-odex-ref]", "text": "x" })).await;
    assert!(r.error.unwrap().contains("not an editable"), "first tagged element is the button");

    // scrolling and waiting
    ok(&s.execute("scroll", &json!({ "dy": 200 })).await, "scroll");
    ok(&s.execute("wait", &json!({ "selector": "#btn" })).await, "wait selector");
    let r = s.execute("wait", &json!({ "selector": "#nope", "timeout_ms": 300 })).await;
    assert!(r.error.unwrap().contains("timed out"));

    // tabs
    let r = s.execute("new_tab", &json!({ "url": format!("{base}/other") })).await;
    ok(&r, "new_tab");
    assert_eq!(r.title.as_deref(), Some("Other"));
    let r = s.execute("tabs", &json!({})).await;
    let tabs = r.text.unwrap();
    assert!(tabs.lines().count() >= 2, "{tabs}");
    let other_id = tabs.lines().find(|l| l.starts_with('*')).unwrap().split_whitespace().nth(1).unwrap().to_string();
    let first_id = tabs.lines().find(|l| !l.starts_with('*')).unwrap().split_whitespace().next().unwrap().to_string();
    ok(&s.execute("select_tab", &json!({ "id": first_id })).await, "select_tab");
    ok(&s.execute("close_tab", &json!({ "id": other_id })).await, "close_tab");
    let r = s.execute("snapshot", &json!({})).await;
    assert!(r.text.unwrap().contains("Fixture page"));

    // a developer-mode session over a second connection can eval
    let dev_transport = WsTransport::connect(endpoint).await.expect("second connection");
    let dev = BrowserSession::new(dev_transport, BrowserOptions { developer_mode: true, ..BrowserOptions::default() });
    let r = dev.execute("eval", &json!({ "expression": "document.querySelector('#out').textContent" })).await;
    ok(&r, "dev eval");
    assert_eq!(r.text.as_deref(), Some("hello from api"));
    let r = dev.execute("cdp", &json!({ "method": "Runtime.evaluate", "params": { "expression": "1 + 2" } })).await;
    ok(&r, "raw cdp");
    let v: Value = serde_json::from_str(&r.text.unwrap()).unwrap();
    assert_eq!(v["result"]["value"], 3);

    s.transport().close_browser().await;
}
