//! Long-running agent processes (`exec_command` / `write_stdin`): ConPTY via
//! portable-pty when unsandboxed, piped stdio inside the sandbox otherwise.
//! Sessions can be listed and killed from the UI.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;

use odex_protocol::ExecSessionInfo;

const MAX_BUFFER: usize = 2 * 1024 * 1024;

/// Called with each new local dev-server URL a session prints (once per port).
pub type UrlHook = Arc<dyn Fn(String) + Send + Sync>;

/// A hook that asks the client to open dev-server URLs in its in-app browser
/// (`openUrl`, target `inApp`, source `devServer`).
pub fn dev_url_hook(emitter: crate::events::Emitter) -> UrlHook {
    Arc::new(move |url| {
        emitter.raw(
            odex_protocol::notification::OPEN_URL,
            &odex_protocol::OpenUrlNotification { url, target: "inApp".into(), source: Some("devServer".into()) },
        )
    })
}

static DEV_URL: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1?\]):(\d{2,5})(?:/[^\s'"<>()\[\]`]*)?"#).unwrap()
});

/// Local dev-server URLs (`localhost`, `127.0.0.1`, `0.0.0.0`, `[::1]` with a port) in `text`,
/// as `(port, url)`. `0.0.0.0` and `[::]` become `localhost` so the URL can be opened.
pub fn dev_server_urls(text: &str) -> Vec<(u16, String)> {
    let clean = strip_ansi(text);
    DEV_URL
        .captures_iter(&clean)
        .filter_map(|c| {
            let port: u16 = c.get(1)?.as_str().parse().ok()?;
            let url = c.get(0)?.as_str().trim_end_matches(['.', ',', ';', ':']).to_string();
            let url = url.replacen("0.0.0.0", "localhost", 1).replacen("[::]", "localhost", 1);
            Some((port, url))
        })
        .collect()
}

/// Line-buffered URL detection for one session.
struct UrlScan {
    hook: UrlHook,
    partial: String,
    seen: HashSet<u16>,
}

impl UrlScan {
    fn feed(&mut self, s: &str) {
        self.partial.push_str(s);
        let complete = match self.partial.rfind('\n') {
            Some(i) => self.partial.drain(..=i).collect::<String>(),
            // a very long line without a newline: scan what we have
            None if self.partial.len() > 4096 => std::mem::take(&mut self.partial),
            None => return,
        };
        for (port, url) in dev_server_urls(&complete) {
            if self.seen.insert(port) {
                (self.hook)(url);
            }
        }
    }
}

enum Input {
    Pty(Mutex<Box<dyn Write + Send>>),
    Piped(Arc<tokio::sync::Mutex<odex_sandbox::SpawnedProcess>>),
}

pub struct Session {
    pub id: String,
    info: Mutex<ExecSessionInfo>,
    buffer: Mutex<String>,
    /// Position up to which output was returned to the model.
    read_pos: Mutex<usize>,
    /// Bytes dropped from the front of the buffer (cap).
    dropped: Mutex<usize>,
    input: Input,
    killer: Mutex<Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    notify: tokio::sync::Notify,
    urls: Mutex<Option<UrlScan>>,
}

impl Session {
    pub fn info(&self) -> ExecSessionInfo {
        self.info.lock().unwrap().clone()
    }

    fn append(&self, s: &str) {
        if let Some(scan) = self.urls.lock().unwrap().as_mut() {
            scan.feed(s);
        }
        let mut b = self.buffer.lock().unwrap();
        b.push_str(s);
        if b.len() > MAX_BUFFER {
            let cut = b.len() - MAX_BUFFER;
            let cut = (cut..b.len()).find(|i| b.is_char_boundary(*i)).unwrap_or(b.len());
            b.drain(..cut);
            *self.dropped.lock().unwrap() += cut;
        }
        drop(b);
        self.notify.notify_waiters();
    }

    fn set_exit(&self, code: Option<i32>) {
        let mut i = self.info.lock().unwrap();
        i.running = false;
        i.exit_code = code;
        drop(i);
        self.notify.notify_waiters();
    }

    pub fn running(&self) -> bool {
        self.info.lock().unwrap().running
    }

    /// Wait up to `wait` for new output (returns early ~150ms after output settles or on exit).
    pub async fn read_new(&self, wait: Duration) -> String {
        let deadline = tokio::time::Instant::now() + wait;
        let mut last_len = self.buffer.lock().unwrap().len();
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline || !self.running() {
                break;
            }
            let step = (deadline - now).min(Duration::from_millis(150));
            let _ = tokio::time::timeout(step, self.notify.notified()).await;
            let len = self.buffer.lock().unwrap().len();
            if len == last_len && len > *self.read_pos.lock().unwrap() {
                // output settled
                break;
            }
            last_len = len;
        }
        let b = self.buffer.lock().unwrap();
        let mut pos = self.read_pos.lock().unwrap();
        let dropped = *self.dropped.lock().unwrap();
        let start = pos.saturating_sub(dropped).min(b.len());
        let start = (start..=b.len()).find(|i| b.is_char_boundary(*i)).unwrap_or(b.len());
        let out = b[start..].to_string();
        *pos = dropped + b.len();
        strip_ansi(&out)
    }

    /// Full output so far (ANSI stripped), for the UI.
    pub fn snapshot(&self) -> String {
        strip_ansi(&self.buffer.lock().unwrap())
    }

    pub async fn write(&self, data: &str) -> anyhow::Result<()> {
        match &self.input {
            Input::Pty(w) => {
                let mut w = w.lock().unwrap();
                // translate \n to \r for PTYs (Enter)
                let data = if cfg!(windows) { data.replace('\n', "\r") } else { data.to_string() };
                w.write_all(data.as_bytes())?;
                w.flush()?;
            }
            Input::Piped(p) => {
                let p = p.lock().await;
                p.write_stdin(data.as_bytes()).await?;
            }
        }
        Ok(())
    }

    pub async fn kill(&self) {
        let (pid, pty) = {
            let i = self.info.lock().unwrap();
            (i.pid, matches!(self.input, Input::Pty(_)))
        };
        // the PTY child is the shell: stop what it started too (dev servers, watchers)
        if let (Some(pid), true) = (pid, pty) {
            kill_tree(pid).await;
        }
        if let Some(k) = self.killer.lock().unwrap().as_mut() {
            let _ = k.kill();
        }
        if let Input::Piped(p) = &self.input {
            p.lock().await.kill();
        }
        // (not `set_exit(self.info.lock()…)`: the guard would still be held inside set_exit)
        let code = self.info.lock().unwrap().exit_code;
        self.set_exit(code);
    }
}

/// Kill a process and its descendants (best effort).
async fn kill_tree(pid: u32) {
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]);
        c
    } else {
        // PTY children lead their own process group
        let mut c = tokio::process::Command::new("kill");
        c.args(["-KILL", "--", &format!("-{pid}")]);
        c
    };
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let _ = tokio::time::timeout(Duration::from_secs(5), cmd.status()).await;
}

#[derive(Default)]
pub struct Sessions {
    map: Mutex<HashMap<String, Arc<Session>>>,
}

pub struct StartSpec {
    pub thread_id: Option<String>,
    pub command: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    /// `Some` = run inside the sandbox (piped stdio), `None` = PTY.
    pub sandbox: Option<odex_sandbox::ExecRequest>,
    /// Told about local dev-server URLs the process prints (once per port).
    pub on_url: Option<UrlHook>,
}

impl Sessions {
    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.map.lock().unwrap().get(id).cloned()
    }

    pub fn list(&self, thread_id: Option<&str>) -> Vec<ExecSessionInfo> {
        let mut v: Vec<ExecSessionInfo> = self
            .map
            .lock()
            .unwrap()
            .values()
            .map(|s| s.info())
            .filter(|i| thread_id.map(|t| i.thread_id.as_deref() == Some(t)).unwrap_or(true))
            .collect();
        v.sort_by_key(|i| i.started_at);
        v
    }

    pub async fn kill(&self, id: &str) -> bool {
        let s = self.get(id);
        match s {
            Some(s) => {
                s.kill().await;
                true
            }
            None => false,
        }
    }

    pub async fn kill_thread(&self, thread_id: &str) {
        let ids: Vec<String> = self.list(Some(thread_id)).into_iter().filter(|i| i.running).map(|i| i.id).collect();
        for id in ids {
            self.kill(&id).await;
        }
    }

    pub async fn start(&self, spec: StartSpec) -> anyhow::Result<Arc<Session>> {
        let id = format!("s{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let info = ExecSessionInfo {
            id: id.clone(),
            thread_id: spec.thread_id.clone(),
            command: spec.command.clone(),
            cwd: spec.cwd.to_string_lossy().to_string(),
            pid: None,
            running: true,
            exit_code: None,
            started_at: chrono::Utc::now().timestamp_millis(),
        };
        let mut urls = spec.on_url.clone().map(|hook| UrlScan { hook, partial: String::new(), seen: HashSet::new() });
        let session = match spec.sandbox {
            Some(mut req) => {
                req.argv = spec.argv.clone();
                req.cwd = spec.cwd.clone();
                req.env.extend(spec.env.clone());
                req.timeout = None;
                let mut proc = odex_sandbox::spawn(req).await?;
                let mut rx = proc.take_output().ok_or_else(|| anyhow::anyhow!("no output channel"))?;
                let mut info = info;
                info.pid = Some(proc.pid());
                let proc = Arc::new(tokio::sync::Mutex::new(proc));
                let s = Arc::new(Session {
                    id: id.clone(),
                    info: Mutex::new(info),
                    buffer: Mutex::new(String::new()),
                    read_pos: Mutex::new(0),
                    dropped: Mutex::new(0),
                    input: Input::Piped(proc.clone()),
                    killer: Mutex::new(None),
                    notify: tokio::sync::Notify::new(),
                    urls: Mutex::new(urls.take()),
                });
                let s2 = s.clone();
                tokio::spawn(async move {
                    while let Some(chunk) = rx.recv().await {
                        s2.append(&String::from_utf8_lossy(chunk.bytes()));
                    }
                });
                let s3 = s.clone();
                tokio::spawn(async move {
                    loop {
                        let r = proc.lock().await.try_wait();
                        if let Some(code) = r {
                            s3.set_exit(code);
                            break;
                        }
                        if !s3.running() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                });
                s
            }
            None => {
                let pty = portable_pty::native_pty_system();
                let pair =
                    pty.openpty(portable_pty::PtySize { rows: 40, cols: 160, pixel_width: 0, pixel_height: 0 })?;
                let mut cmd = portable_pty::CommandBuilder::new(&spec.argv[0]);
                for a in &spec.argv[1..] {
                    cmd.arg(a);
                }
                cmd.cwd(&spec.cwd);
                for (k, v) in &spec.env {
                    cmd.env(k, v);
                }
                let mut child = pair.slave.spawn_command(cmd)?;
                drop(pair.slave);
                let mut reader = pair.master.try_clone_reader()?;
                let writer = pair.master.take_writer()?;
                let killer = child.clone_killer();
                let mut info = info;
                info.pid = child.process_id();
                let s = Arc::new(Session {
                    id: id.clone(),
                    info: Mutex::new(info),
                    buffer: Mutex::new(String::new()),
                    read_pos: Mutex::new(0),
                    dropped: Mutex::new(0),
                    input: Input::Pty(Mutex::new(writer)),
                    killer: Mutex::new(Some(killer)),
                    notify: tokio::sync::Notify::new(),
                    urls: Mutex::new(urls.take()),
                });
                let s2 = s.clone();
                let master = pair.master;
                std::thread::spawn(move || {
                    let _keep = master; // keep the PTY open while reading
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => s2.append(&String::from_utf8_lossy(&buf[..n])),
                        }
                    }
                });
                let s3 = s.clone();
                std::thread::spawn(move || {
                    let status = child.wait();
                    let code = status.ok().map(|st| st.exit_code() as i32);
                    s3.set_exit(code);
                });
                s
            }
        };
        self.map.lock().unwrap().insert(id, session.clone());
        Ok(session)
    }
}

static ANSI: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"\x1b\[[0-9;?<=>]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[PX^_][^\x1b]*\x1b\\|\x1b[@-Z\\-_]|\r",
    )
    .unwrap()
});

/// Strip ANSI escape sequences and carriage returns.
pub fn strip_ansi(s: &str) -> String {
    ANSI.replace_all(s, "").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn kill_stops_a_running_session() {
        let sessions = Sessions::default();
        let cmd = if cfg!(windows) { "Start-Sleep -Seconds 60" } else { "sleep 60" };
        let s = sessions
            .start(StartSpec {
                thread_id: Some("t".into()),
                command: cmd.into(),
                argv: odex_sandbox::shell_argv(&odex_config::default_shell(), cmd),
                cwd: std::env::temp_dir(),
                env: Default::default(),
                sandbox: None,
                on_url: None,
            })
            .await
            .unwrap();
        assert!(s.running());
        tokio::time::timeout(Duration::from_secs(15), sessions.kill(&s.id)).await.expect("kill returns (no deadlock)");
        assert!(!s.running());
        assert!(!sessions.list(Some("t"))[0].running);
    }

    #[test]
    fn detects_dev_server_urls_once_per_port() {
        let vite =
            "  \x1b[32m➜\x1b[39m  \x1b[1mLocal\x1b[22m:   \x1b[36mhttp://localhost:\x1b[1m5173\x1b[22m/\x1b[39m\r\n";
        assert_eq!(dev_server_urls(vite), vec![(5173, "http://localhost:5173/".to_string())]);
        assert_eq!(
            dev_server_urls("Listening on http://0.0.0.0:8080, docs at https://example.com:443/x"),
            vec![(8080, "http://localhost:8080".to_string())]
        );
        assert_eq!(
            dev_server_urls("see http://127.0.0.1:3000/app?x=1."),
            vec![(3000, "http://127.0.0.1:3000/app?x=1".into())]
        );
        assert!(dev_server_urls("http://localhost/ without a port").is_empty());

        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        let mut scan = UrlScan {
            hook: Arc::new(move |u| g2.lock().unwrap().push(u)),
            partial: String::new(),
            seen: HashSet::new(),
        };
        // split across chunks, repeated, then a second port
        scan.feed("server at http://local");
        scan.feed("host:4000/ ready\n");
        scan.feed("http://localhost:4000/ again\nhttp://localhost:4001\n");
        assert_eq!(
            *got.lock().unwrap(),
            vec!["http://localhost:4000/".to_string(), "http://localhost:4001".to_string()]
        );
    }

    #[test]
    fn ansi() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m\r\n\x1b]0;title\x07ok"), "red\nok");
    }

    #[tokio::test]
    async fn pty_session_roundtrip() {
        let sessions = Sessions::default();
        let argv = odex_sandbox::shell_argv(&odex_config::default_shell(), "echo hello-pty");
        let s = sessions
            .start(StartSpec {
                thread_id: Some("t".into()),
                command: "echo".into(),
                argv,
                cwd: std::env::temp_dir(),
                env: Default::default(),
                sandbox: None,
                on_url: None,
            })
            .await
            .unwrap();
        let mut out = String::new();
        for _ in 0..40 {
            out.push_str(&s.read_new(Duration::from_millis(500)).await);
            if out.contains("hello-pty") && !s.running() {
                break;
            }
        }
        assert!(out.contains("hello-pty"), "{out:?}");
        assert_eq!(sessions.list(Some("t")).len(), 1);
    }
}
