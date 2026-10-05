//! Long-running agent processes (`exec_command` / `write_stdin`): ConPTY via
//! portable-pty when unsandboxed, piped stdio inside the sandbox otherwise.
//! Sessions can be listed and killed from the UI.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;

use odex_protocol::ExecSessionInfo;

const MAX_BUFFER: usize = 2 * 1024 * 1024;

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
}

impl Session {
    pub fn info(&self) -> ExecSessionInfo {
        self.info.lock().unwrap().clone()
    }

    fn append(&self, s: &str) {
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
        if let Some(k) = self.killer.lock().unwrap().as_mut() {
            let _ = k.kill();
        }
        if let Input::Piped(p) = &self.input {
            p.lock().await.kill();
        }
        self.set_exit(self.info.lock().unwrap().exit_code);
    }
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
