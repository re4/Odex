//! Sandboxed command execution for the Odex engine.
//!
//! Every agent shell command goes through [`exec`] (one-shot, captured output) or [`spawn`]
//! (long-running "exec session" with piped stdio). The caller picks a [`SandboxPolicy`] derived
//! from the permission mode:
//!
//! | permission mode | policy                                   | effect                                   |
//! |-----------------|------------------------------------------|------------------------------------------|
//! | read-only       | [`SandboxPolicy::ReadOnly`]              | no writes except a private temp dir      |
//! | auto            | [`SandboxPolicy::WorkspaceWrite`]        | writes only inside the writable roots    |
//! | full-access     | [`SandboxPolicy::FullAccess`]            | no sandbox                               |
//!
//! Backends:
//!
//! * **Windows** – [`Backend::RestrictedToken`] (default): a write-restricted token whose
//!   restricting SIDs only match an Odex-specific SID that is granted (via inheritable ACEs) on
//!   the writable roots and the private temp dir. Reads behave like the user; network is *not*
//!   isolated. [`Backend::AppContainer`]: an AppContainer with no capabilities (network isolated,
//!   but reads outside system locations and granted folders are denied). Every child, sandboxed
//!   or not, runs in a Job Object with `KILL_ON_JOB_CLOSE`, so timeouts and cancellation kill the
//!   whole process tree. See the `windows` module docs for details.
//! * **Linux** – bubblewrap (`bwrap`): read-only bind of `/`, writable binds for the roots,
//!   private `/tmp`, `--unshare-net` unless network is allowed.
//! * **macOS** – `sandbox-exec` with a generated Seatbelt profile.
//!
//! If the requested backend is unavailable (or [`Backend::None`] is selected) for a sandboxed
//! policy, [`exec`]/[`spawn`] return [`SandboxError::Unavailable`] and [`status`] reports
//! `available: false` with a warning. The engine must then fall back to asking for approval and
//! re-run with [`SandboxPolicy::FullAccess`]; this crate never silently runs a command unsandboxed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

mod output;
mod process;
mod shell;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub use odex_protocol::SandboxStatus;
pub use process::SpawnedProcess;
pub use shell::shell_argv;

use output::{looks_sandbox_denied, HeadTail};

/// Default cap for captured stdout / stderr / aggregated output (1 MiB each).
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// After the main process exits, how long to wait for straggling descendants to close their
/// output pipes before the remaining process tree is killed.
const POST_EXIT_GRACE: Duration = Duration::from_millis(1000);
/// After killing the tree, how long to wait for the pipes to drain before giving up.
const KILL_DRAIN_GRACE: Duration = Duration::from_millis(2000);

/// What a command may do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SandboxPolicy {
    /// No writes (except the private sandbox temp dir). Network is isolated where the backend can.
    #[default]
    ReadOnly,
    /// Writes only inside `writable_roots` (and the private temp dir); network off unless enabled.
    WorkspaceWrite { writable_roots: Vec<PathBuf>, network: bool },
    /// No sandbox at all.
    FullAccess,
}

impl SandboxPolicy {
    /// `true` for every policy except [`SandboxPolicy::FullAccess`].
    pub fn is_sandboxed(&self) -> bool {
        !matches!(self, SandboxPolicy::FullAccess)
    }

    /// Stable kebab-case name (`read-only`, `workspace-write`, `full-access`).
    pub fn kind(&self) -> &'static str {
        match self {
            SandboxPolicy::ReadOnly => "read-only",
            SandboxPolicy::WorkspaceWrite { .. } => "workspace-write",
            SandboxPolicy::FullAccess => "full-access",
        }
    }

    /// Whether the policy allows network access.
    pub fn allows_network(&self) -> bool {
        match self {
            SandboxPolicy::ReadOnly => false,
            SandboxPolicy::WorkspaceWrite { network, .. } => *network,
            SandboxPolicy::FullAccess => true,
        }
    }

    /// The writable roots (empty unless [`SandboxPolicy::WorkspaceWrite`]).
    pub fn writable_roots(&self) -> &[PathBuf] {
        match self {
            SandboxPolicy::WorkspaceWrite { writable_roots, .. } => writable_roots,
            _ => &[],
        }
    }
}

/// Backend preference. On non-Windows platforms the Windows-specific variants behave like `Auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Backend {
    /// Platform default (restricted token on Windows, bubblewrap on Linux, Seatbelt on macOS).
    #[default]
    Auto,
    /// Windows write-restricted token + Job Object.
    RestrictedToken,
    /// Windows AppContainer (no capabilities) + Job Object.
    AppContainer,
    /// No sandbox backend: sandboxed policies fail with [`SandboxError::Unavailable`].
    None,
}

impl Backend {
    /// Config spelling: `auto`, `restricted-token`, `appcontainer`, `none`.
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::RestrictedToken => "restricted-token",
            Backend::AppContainer => "appcontainer",
            Backend::None => "none",
        }
    }

    /// Parses the config spelling (case-insensitive, `_`/`-` agnostic).
    pub fn parse(s: &str) -> Option<Backend> {
        let norm: String = s.trim().to_ascii_lowercase().chars().filter(|c| *c != '-' && *c != '_').collect();
        match norm.as_str() {
            "" | "auto" | "default" => Some(Backend::Auto),
            "restrictedtoken" | "restricted" | "token" => Some(Backend::RestrictedToken),
            "appcontainer" | "container" => Some(Backend::AppContainer),
            "none" | "off" | "disabled" => Some(Backend::None),
            _ => None,
        }
    }
}

impl FromStr for Backend {
    type Err = SandboxError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Backend::parse(s).ok_or_else(|| SandboxError::Other(format!("unknown sandbox backend `{s}`")))
    }
}

/// A command to run.
#[derive(Debug, Clone)]
pub struct ExecRequest {
    /// Program + arguments (use [`shell_argv`] to build them for a shell).
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Additions / overrides to the inherited environment.
    pub env: HashMap<String, String>,
    pub timeout: Option<Duration>,
    pub policy: SandboxPolicy,
    /// Bytes written to the child's stdin before it is closed ([`exec`]) or kept open ([`spawn`]).
    pub stdin: Option<Vec<u8>>,
    /// Cap for each of captured stdout / stderr / aggregated. When exceeded the head and the tail
    /// are kept with a marker in between. `0` means [`DEFAULT_MAX_OUTPUT_BYTES`].
    pub max_output_bytes: usize,
    pub backend: Backend,
    /// Private writable temp dir (`TEMP`/`TMP`/`TMPDIR` point here under the sandbox). Defaults to
    /// `<system temp>/odex-sandbox` when sandboxed.
    pub sandbox_temp: Option<PathBuf>,
}

impl Default for ExecRequest {
    fn default() -> Self {
        ExecRequest {
            argv: Vec::new(),
            cwd: PathBuf::from("."),
            env: HashMap::new(),
            timeout: None,
            policy: SandboxPolicy::ReadOnly,
            stdin: None,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            backend: Backend::Auto,
            sandbox_temp: None,
        }
    }
}

impl ExecRequest {
    /// A request with default settings (read-only policy, 1 MiB output cap, no timeout).
    pub fn new(argv: Vec<String>, cwd: impl Into<PathBuf>) -> Self {
        ExecRequest { argv, cwd: cwd.into(), ..Default::default() }
    }

    pub(crate) fn output_cap(&self) -> usize {
        if self.max_output_bytes == 0 {
            DEFAULT_MAX_OUTPUT_BYTES
        } else {
            self.max_output_bytes
        }
    }

    /// The private temp dir to use under the sandbox.
    pub(crate) fn effective_sandbox_temp(&self) -> PathBuf {
        self.sandbox_temp.clone().unwrap_or_else(|| std::env::temp_dir().join("odex-sandbox"))
    }
}

/// A piece of child output, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputChunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

impl OutputChunk {
    pub fn bytes(&self) -> &[u8] {
        match self {
            OutputChunk::Stdout(b) | OutputChunk::Stderr(b) => b,
        }
    }

    pub fn is_stderr(&self) -> bool {
        matches!(self, OutputChunk::Stderr(_))
    }
}

/// Result of [`exec`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    /// Exit code of the main process; `None` when it was killed (timeout / cancel) or, on Unix,
    /// when it could not be determined.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// stdout + stderr interleaved in arrival order.
    pub aggregated: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration: Duration,
    /// Whether the command ran under a sandbox backend.
    pub sandboxed: bool,
    /// Heuristic: sandboxed, non-zero exit and the output looks like an access-denied failure.
    pub sandbox_denied: bool,
    /// Any of stdout / stderr / aggregated exceeded the cap.
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The requested backend cannot be used; the engine should ask for approval instead.
    #[error("sandbox unavailable: {0}")]
    Unavailable(String),
    #[error("failed to spawn process: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Sandbox availability for `backend` under the policy named `policy_kind`
/// (`read-only`, `workspace-write`/`auto`, `full-access`/`danger-full-access`).
pub fn status(backend: Backend, policy_kind: &str) -> SandboxStatus {
    let kind = policy_kind.trim().to_ascii_lowercase().replace('_', "-");
    if matches!(kind.as_str(), "full-access" | "danger-full-access" | "fullaccess" | "dangerfullaccess") {
        return SandboxStatus {
            backend: "none".into(),
            available: true,
            network_isolated: false,
            warning: Some("Full access: commands run without a sandbox.".into()),
        };
    }
    if backend == Backend::None {
        return SandboxStatus {
            backend: "none".into(),
            available: false,
            network_isolated: false,
            warning: Some(
                "Sandbox disabled (backend = none): every command needs approval and then runs unsandboxed.".into(),
            ),
        };
    }
    platform_status(backend)
}

#[cfg(windows)]
fn platform_status(backend: Backend) -> SandboxStatus {
    windows::status(backend)
}

#[cfg(unix)]
fn platform_status(backend: Backend) -> SandboxStatus {
    unix::status(backend)
}

#[cfg(not(any(windows, unix)))]
fn platform_status(_backend: Backend) -> SandboxStatus {
    SandboxStatus {
        backend: "none".into(),
        available: false,
        network_isolated: false,
        warning: Some("No sandbox backend for this platform; commands require approval.".into()),
    }
}

fn validate(req: &ExecRequest) -> Result<(), SandboxError> {
    if req.argv.is_empty() || req.argv[0].is_empty() {
        return Err(SandboxError::Other("empty argv".into()));
    }
    if !req.cwd.is_dir() {
        return Err(SandboxError::Spawn(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("working directory does not exist: {}", req.cwd.display()),
        )));
    }
    if req.policy.is_sandboxed() && req.backend == Backend::None {
        return Err(SandboxError::Unavailable(
            "sandbox backend is disabled (`none`); approve the command to run it without a sandbox".into(),
        ));
    }
    Ok(())
}

fn launch_blocking(req: ExecRequest) -> Result<SpawnedProcess, SandboxError> {
    validate(&req)?;
    let launched = launch_platform(&req)?;
    Ok(SpawnedProcess::start(launched, req.stdin))
}

#[cfg(windows)]
fn launch_platform(req: &ExecRequest) -> Result<process::Launched, SandboxError> {
    windows::launch(req)
}

#[cfg(unix)]
fn launch_platform(req: &ExecRequest) -> Result<process::Launched, SandboxError> {
    unix::launch(req)
}

#[cfg(not(any(windows, unix)))]
fn launch_platform(req: &ExecRequest) -> Result<process::Launched, SandboxError> {
    let _ = req;
    Err(SandboxError::Unavailable("process spawning is not supported on this platform".into()))
}

/// Starts a long-running process with piped stdio (no PTY). `req.timeout` is ignored; the caller
/// owns the lifetime (dropping the [`SpawnedProcess`] kills the whole tree). `req.stdin`, if set,
/// is written first and stdin stays open.
pub async fn spawn(req: ExecRequest) -> Result<SpawnedProcess, SandboxError> {
    // ACL grants may walk a large tree the first time; keep that off the async workers.
    tokio::task::spawn_blocking(move || launch_blocking(req))
        .await
        .map_err(|e| SandboxError::Other(format!("spawn task failed: {e}")))?
}

/// Runs a command to completion, streaming output chunks to `on_output` and capturing them.
///
/// The process tree is killed on timeout, on cancellation, and shortly after the main process
/// exits if descendants are still holding the output pipes open.
pub async fn exec(
    mut req: ExecRequest,
    mut on_output: impl FnMut(OutputChunk) + Send + 'static,
    cancel: CancellationToken,
) -> Result<ExecOutput, SandboxError> {
    let start = Instant::now();
    let cap = req.output_cap();
    let timeout = req.timeout;
    let stdin = req.stdin.take();
    let sandboxed_policy = req.policy.is_sandboxed();

    if cancel.is_cancelled() {
        validate(&req)?;
        return Ok(ExecOutput {
            cancelled: true,
            duration: start.elapsed(),
            sandboxed: sandboxed_policy,
            ..Default::default()
        });
    }

    let mut child = spawn(req).await?;
    if let Some(data) = stdin {
        child.queue_stdin(data);
    }
    child.close_stdin();

    let sandboxed = child.sandboxed();
    let killer = child.killer();
    let mut out_rx = child.take_output().expect("fresh process has an output receiver");

    let mut stdout = HeadTail::new(cap);
    let mut stderr = HeadTail::new(cap);
    let mut aggregated = HeadTail::new(cap);

    let deadline = timeout.map(|t| tokio::time::Instant::from_std(start + t));
    let mut grace: Option<tokio::time::Instant> = None;
    let mut tree_killed = false;
    let mut output_open = true;
    let mut exit: Option<Option<i32>> = None;
    let mut timed_out = false;
    let mut cancelled = false;

    loop {
        if !output_open && exit.is_some() {
            break;
        }
        let interrupted = timed_out || cancelled;
        tokio::select! {
            chunk = out_rx.recv(), if output_open => match chunk {
                Some(chunk) => {
                    match &chunk {
                        OutputChunk::Stdout(b) => stdout.push(b),
                        OutputChunk::Stderr(b) => stderr.push(b),
                    }
                    aggregated.push(chunk.bytes());
                    on_output(chunk);
                }
                None => output_open = false,
            },
            code = child.wait(), if exit.is_none() => {
                exit = Some(code);
                if grace.is_none() {
                    grace = Some(tokio::time::Instant::now() + POST_EXIT_GRACE);
                }
            },
            _ = sleep_until_opt(deadline), if deadline.is_some() && !interrupted && exit.is_none() => {
                timed_out = true;
                killer.kill_tree();
                tree_killed = true;
                grace = Some(tokio::time::Instant::now() + KILL_DRAIN_GRACE);
            },
            _ = cancel.cancelled(), if !interrupted && exit.is_none() => {
                cancelled = true;
                killer.kill_tree();
                tree_killed = true;
                grace = Some(tokio::time::Instant::now() + KILL_DRAIN_GRACE);
            },
            _ = sleep_until_opt(grace), if grace.is_some() => {
                if tree_killed {
                    tracing::warn!("sandboxed process output did not close after kill; giving up");
                    break;
                }
                killer.kill_tree();
                tree_killed = true;
                grace = Some(tokio::time::Instant::now() + KILL_DRAIN_GRACE);
            },
        }
    }
    // Make sure nothing from the tree outlives the command.
    killer.kill_tree();

    let exit_code = if timed_out || cancelled { None } else { exit.flatten() };
    let (stdout_s, t1) = stdout.render();
    let (stderr_s, t2) = stderr.render();
    let (aggregated_s, t3) = aggregated.render();
    let sandbox_denied = sandboxed && !timed_out && !cancelled && looks_sandbox_denied(exit_code, &aggregated_s);
    Ok(ExecOutput {
        exit_code,
        stdout: stdout_s,
        stderr: stderr_s,
        aggregated: aggregated_s,
        timed_out,
        cancelled,
        duration: start.elapsed(),
        sandboxed,
        sandbox_denied,
        truncated: t1 || t2 || t3,
    })
}

async fn sleep_until_opt(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Writable roots that actually exist, made absolute, deduplicated.
#[cfg_attr(not(any(windows, target_os = "linux", target_os = "macos")), allow(dead_code))]
pub(crate) fn existing_roots(roots: &[PathBuf], cwd: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for root in roots {
        let abs = if root.is_absolute() { root.clone() } else { cwd.join(root) };
        let abs = std::path::absolute(&abs).unwrap_or(abs);
        if !abs.exists() {
            tracing::debug!(root = %abs.display(), "skipping non-existent writable root");
            continue;
        }
        if !out.contains(&abs) {
            out.push(abs);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_parse_roundtrip() {
        for b in [Backend::Auto, Backend::RestrictedToken, Backend::AppContainer, Backend::None] {
            assert_eq!(Backend::parse(b.as_str()), Some(b));
        }
        assert_eq!(Backend::parse("Restricted_Token"), Some(Backend::RestrictedToken));
        assert_eq!(Backend::parse("bogus"), None);
        assert!("bogus".parse::<Backend>().is_err());
    }

    #[test]
    fn policy_helpers() {
        let p = SandboxPolicy::WorkspaceWrite { writable_roots: vec![PathBuf::from("x")], network: true };
        assert!(p.is_sandboxed());
        assert!(p.allows_network());
        assert_eq!(p.kind(), "workspace-write");
        assert_eq!(p.writable_roots().len(), 1);
        assert!(!SandboxPolicy::FullAccess.is_sandboxed());
        assert!(!SandboxPolicy::ReadOnly.allows_network());
    }

    #[test]
    fn status_full_access_and_none() {
        let s = status(Backend::Auto, "full-access");
        assert!(s.available);
        assert_eq!(s.backend, "none");
        let s = status(Backend::None, "workspace-write");
        assert!(!s.available);
        assert!(s.warning.is_some());
    }

    #[tokio::test]
    async fn none_backend_refuses_sandboxed_policy() {
        let req = ExecRequest {
            argv: vec!["whatever".into()],
            cwd: std::env::temp_dir(),
            backend: Backend::None,
            ..Default::default()
        };
        let err = exec(req, |_| {}, CancellationToken::new()).await.unwrap_err();
        assert!(matches!(err, SandboxError::Unavailable(_)), "{err:?}");
    }
}
