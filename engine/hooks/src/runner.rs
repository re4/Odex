//! Spawning one hook process through the platform shell.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Captured result of a finished hook process.
#[derive(Debug)]
pub(crate) struct HookRun {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub(crate) enum RunError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Timeout,
}

/// Prefix for PowerShell commands: UTF-8 pipes (best effort; there may be
/// no console to configure, hence the `try`).
#[cfg(windows)]
const POWERSHELL_PRELUDE: &str = "$OutputEncoding=[System.Text.UTF8Encoding]::new($false);try{[Console]::InputEncoding=$OutputEncoding;[Console]::OutputEncoding=$OutputEncoding}catch{};";

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    // CREATE_NO_WINDOW: never flash a console window from a GUI-hosted engine.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]);
    cmd.arg(format!("{POWERSHELL_PRELUDE}{command}"));
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command);
    cmd
}

/// Run `command` in `cwd`, feeding `input` on stdin, and wait up to `timeout`.
/// The process is killed when it times out.
pub(crate) async fn run_hook(
    command: &str,
    cwd: &Path,
    input: &[u8],
    timeout: Duration,
    env: &[(&str, &str)],
) -> Result<HookRun, RunError> {
    let mut cmd = shell_command(command);
    cmd.current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().map_err(RunError::Spawn)?;

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let write = async move {
        if let Some(mut stdin) = stdin {
            // The hook may exit without reading stdin; a broken pipe is fine.
            let _ = stdin.write_all(input).await;
            let _ = stdin.shutdown().await;
        }
    };
    let read_stdout = async move {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout {
            let _ = out.read_to_end(&mut buf).await;
        }
        buf
    };
    let read_stderr = async move {
        let mut buf = Vec::new();
        if let Some(mut err) = stderr {
            let _ = err.read_to_end(&mut buf).await;
        }
        buf
    };

    let finished = {
        let run = async { tokio::join!(write, read_stdout, read_stderr, child.wait()) };
        tokio::time::timeout(timeout, run).await
    };
    match finished {
        Ok((_, out, err, status)) => {
            let status = status.map_err(RunError::Wait)?;
            Ok(HookRun {
                status,
                stdout: String::from_utf8_lossy(&out).into_owned(),
                stderr: String::from_utf8_lossy(&err).into_owned(),
            })
        }
        Err(_) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            Err(RunError::Timeout)
        }
    }
}
