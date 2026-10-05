//! stdio transport: newline-delimited JSON-RPC over a child process.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc::UnboundedSender, oneshot, watch};

use super::{AbortOnDrop, Inbound, Transport};
use crate::error::McpError;
use crate::logbuf::LogSink;
use crate::process::{resolve_command, ProcessTree};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub(crate) struct StdioConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

pub(crate) struct StdioTransport {
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    kill_tx: Mutex<Option<oneshot::Sender<()>>>,
    exited: watch::Receiver<bool>,
    tree: Arc<ProcessTree>,
    _tasks: Vec<AbortOnDrop>,
}

impl StdioTransport {
    pub(crate) async fn spawn(
        cfg: &StdioConfig,
        inbound: UnboundedSender<Inbound>,
        log: Arc<LogSink>,
    ) -> Result<Self, McpError> {
        if let Some(cwd) = &cfg.cwd {
            if !std::path::Path::new(cwd).is_dir() {
                return Err(McpError::Config(format!("working directory `{cwd}` does not exist")));
            }
        }
        let path_override: Option<OsString> =
            cfg.env.iter().find(|(k, _)| k.eq_ignore_ascii_case("PATH")).map(|(_, v)| OsString::from(v));
        let program = resolve_command(&cfg.command, path_override.as_deref());

        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&cfg.args)
            .envs(&cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &cfg.cwd {
            cmd.current_dir(cwd);
        }
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                McpError::Transport(format!("command not found: `{}`", cfg.command))
            } else {
                McpError::Transport(format!("failed to start `{}`: {e}", cfg.command))
            }
        })?;

        let tree = Arc::new(ProcessTree::attach(&child));

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut tasks = Vec::new();

        // stdout: JSON-RPC messages, one per line.
        if let Some(stdout) = stdout {
            let tx = inbound.clone();
            let log = log.clone();
            tasks.push(AbortOnDrop(tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut buf = Vec::with_capacity(4096);
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let line = String::from_utf8_lossy(&buf);
                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<Value>(line) {
                                Ok(v) => {
                                    if tx.send(Inbound::Message(v)).is_err() {
                                        return;
                                    }
                                }
                                Err(_) => log.push(format!("[stdout] {line}")),
                            }
                        }
                        Err(e) => {
                            log.error(format!("reading stdout failed: {e}"));
                            break;
                        }
                    }
                }
                // Normally the exit watcher reports first (with the exit code).
                tokio::time::sleep(Duration::from_millis(1500)).await;
                let _ = tx.send(Inbound::Closed("server closed stdout".into()));
            })));
        }

        // stderr: log lines.
        let (stderr_done_tx, stderr_done_rx) = oneshot::channel::<()>();
        if let Some(stderr) = stderr {
            let log = log.clone();
            tasks.push(AbortOnDrop(tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut buf = Vec::with_capacity(1024);
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => log.stderr(&String::from_utf8_lossy(&buf)),
                    }
                }
                let _ = stderr_done_tx.send(());
            })));
        } else {
            drop(stderr_done_tx);
        }

        // Exit watcher: owns the child; kills it on request or when the transport is dropped.
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let (exited_tx, exited_rx) = watch::channel(false);
        {
            let tx = inbound;
            let tree = tree.clone();
            tokio::spawn(async move {
                let status = wait_or_kill(&mut child, kill_rx, &tree).await;
                let _ = exited_tx.send(true);
                let _ = tokio::time::timeout(Duration::from_millis(500), stderr_done_rx).await;
                let reason = match status {
                    Ok(s) => format!("server process exited ({s})"),
                    Err(e) => format!("waiting for the server process failed: {e}"),
                };
                let _ = tx.send(Inbound::Closed(reason));
            });
        }

        Ok(StdioTransport {
            stdin: tokio::sync::Mutex::new(stdin),
            kill_tx: Mutex::new(Some(kill_tx)),
            exited: exited_rx,
            tree,
            _tasks: tasks,
        })
    }
}

async fn wait_or_kill(
    child: &mut Child,
    mut kill_rx: oneshot::Receiver<()>,
    tree: &ProcessTree,
) -> std::io::Result<std::process::ExitStatus> {
    let finished = tokio::select! {
        s = child.wait() => Some(s),
        _ = &mut kill_rx => None,
    };
    if let Some(s) = finished {
        return s;
    }
    // Kill requested (or the transport was dropped): SIGTERM first where that exists.
    if cfg!(unix) {
        tree.terminate_gracefully();
        if let Ok(s) = tokio::time::timeout(Duration::from_millis(500), child.wait()).await {
            tree.kill();
            return s;
        }
    }
    tree.kill();
    let _ = child.start_kill();
    child.wait().await
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, msg: Value) -> Result<(), McpError> {
        let mut line = serde_json::to_vec(&msg).map_err(McpError::transport)?;
        line.push(b'\n');
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| McpError::Closed("stdin is closed".into()))?;
        stdin.write_all(&line).await.map_err(|e| McpError::Closed(format!("writing to the server failed: {e}")))?;
        stdin.flush().await.map_err(|e| McpError::Closed(format!("writing to the server failed: {e}")))?;
        Ok(())
    }

    async fn close(&self) {
        // Closing stdin asks well-behaved servers to exit.
        self.stdin.lock().await.take();
        let mut exited = self.exited.clone();
        let graceful =
            matches!(tokio::time::timeout(Duration::from_millis(500), exited.wait_for(|e| *e)).await, Ok(Ok(_)));
        if !graceful {
            if let Some(tx) = self.kill_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let _ = tokio::time::timeout(Duration::from_secs(3), exited.wait_for(|e| *e)).await;
        }
        // Leftover grandchildren (e.g. node.exe under an npx shim).
        self.tree.kill();
    }
}
