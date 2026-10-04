//! Linux (bubblewrap) and macOS (Seatbelt) backends.
//!
//! Children run in their own process group (`setpgid(0, 0)`); killing sends `SIGKILL` to the
//! whole group.
//!
//! * **Linux:** `bwrap --ro-bind / / --dev /dev --proc /proc --tmpfs /tmp [--bind root root]...
//!   [--unshare-net] --die-with-parent --new-session --chdir <cwd> -- <argv>`. Availability is
//!   probed once by running a trivial command under bwrap (unprivileged user namespaces may be
//!   disabled even when the binary exists).
//! * **macOS:** `/usr/bin/sandbox-exec -p <profile> <argv>` with a profile that allows everything
//!   except file writes outside the roots / temp dirs and (unless allowed) network access.

use std::ffi::OsString;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::existing_roots;
use crate::process::{KillTree, Launched};
use crate::{Backend, ExecRequest, SandboxError, SandboxStatus};

struct GroupKiller {
    pgid: libc::pid_t,
}

impl KillTree for GroupKiller {
    fn kill_tree(&self) {
        if self.pgid > 0 {
            // SAFETY: plain syscall; ESRCH when the group is already gone is fine.
            unsafe {
                libc::killpg(self.pgid, libc::SIGKILL);
            }
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(target_os = "linux")]
mod backend {
    use super::*;
    use std::sync::OnceLock;

    pub(super) const NAME: &str = "bubblewrap";

    fn probe() -> &'static Result<PathBuf, String> {
        static PROBE: OnceLock<Result<PathBuf, String>> = OnceLock::new();
        PROBE.get_or_init(|| {
            let bwrap = find_in_path("bwrap").ok_or_else(|| "bubblewrap (bwrap) is not installed".to_string())?;
            let out = Command::new(&bwrap)
                .args(["--ro-bind", "/", "/", "--dev", "/dev", "--unshare-net", "--die-with-parent", "--", "true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|e| format!("could not run bwrap: {e}"))?;
            if out.status.success() {
                Ok(bwrap)
            } else {
                Err(format!("bwrap is not usable here: {}", String::from_utf8_lossy(&out.stderr).trim()))
            }
        })
    }

    pub(super) fn status() -> SandboxStatus {
        match probe() {
            Ok(_) => SandboxStatus { backend: NAME.into(), available: true, network_isolated: true, warning: None },
            Err(e) => SandboxStatus {
                backend: NAME.into(),
                available: false,
                network_isolated: false,
                warning: Some(format!("{e}; sandboxed commands require approval.")),
            },
        }
    }

    pub(super) fn wrap(req: &ExecRequest, cwd: &Path, temp: Option<&Path>) -> Result<Vec<OsString>, SandboxError> {
        let bwrap = probe().as_ref().map_err(|e| SandboxError::Unavailable(e.clone()))?;
        let mut a: Vec<OsString> = vec![bwrap.into()];
        for s in ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"] {
            a.push(s.into());
        }
        // The tmpfs hides /tmp; keep a working directory below it visible (read-only).
        if cwd.starts_with("/tmp") {
            a.extend([OsString::from("--ro-bind"), cwd.into(), cwd.into()]);
        }
        for root in existing_roots(req.policy.writable_roots(), cwd) {
            let root = canonical(&root);
            a.extend([OsString::from("--bind"), root.clone().into_os_string(), root.into_os_string()]);
        }
        if let Some(t) = temp {
            a.extend([OsString::from("--bind"), t.into(), t.into()]);
        }
        if !req.policy.allows_network() {
            a.push("--unshare-net".into());
        }
        for s in ["--die-with-parent", "--new-session", "--chdir"] {
            a.push(s.into());
        }
        a.push(cwd.into());
        a.push("--".into());
        a.extend(req.argv.iter().map(OsString::from));
        Ok(a)
    }
}

#[cfg(target_os = "macos")]
mod backend {
    use super::*;

    pub(super) const NAME: &str = "seatbelt";
    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    pub(super) fn status() -> SandboxStatus {
        if Path::new(SANDBOX_EXEC).is_file() {
            SandboxStatus { backend: NAME.into(), available: true, network_isolated: true, warning: None }
        } else {
            SandboxStatus {
                backend: NAME.into(),
                available: false,
                network_isolated: false,
                warning: Some("sandbox-exec not found; sandboxed commands require approval.".into()),
            }
        }
    }

    /// Escapes a path for an SBPL string literal.
    fn sbpl_string(p: &Path) -> String {
        let s = p.to_string_lossy();
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    }

    pub(super) fn profile(roots: &[PathBuf], temp: Option<&Path>, network: bool) -> String {
        let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*)\n(allow file-write*\n");
        p.push_str("  (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\")\n");
        p.push_str("  (regex #\"^/dev/tty\") (regex #\"^/dev/fd/\")\n");
        p.push_str("  (subpath \"/private/tmp\") (subpath \"/private/var/folders\")\n");
        for root in roots {
            p.push_str(&format!("  (subpath {})\n", sbpl_string(root)));
        }
        if let Some(t) = temp {
            p.push_str(&format!("  (subpath {})\n", sbpl_string(t)));
        }
        p.push_str(")\n");
        if !network {
            p.push_str("(deny network*)\n");
        }
        p
    }

    pub(super) fn wrap(req: &ExecRequest, cwd: &Path, temp: Option<&Path>) -> Result<Vec<OsString>, SandboxError> {
        if !Path::new(SANDBOX_EXEC).is_file() {
            return Err(SandboxError::Unavailable("sandbox-exec not found".into()));
        }
        let roots: Vec<PathBuf> =
            existing_roots(req.policy.writable_roots(), cwd).iter().map(|r| canonical(r)).collect();
        let temp = temp.map(canonical);
        let profile = profile(&roots, temp.as_deref(), req.policy.allows_network());
        let mut a: Vec<OsString> = vec![SANDBOX_EXEC.into(), "-p".into(), profile.into()];
        a.extend(req.argv.iter().map(OsString::from));
        Ok(a)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod backend {
    use super::*;

    pub(super) const NAME: &str = "none";

    pub(super) fn status() -> SandboxStatus {
        SandboxStatus {
            backend: NAME.into(),
            available: false,
            network_isolated: false,
            warning: Some("No sandbox backend for this platform; commands require approval.".into()),
        }
    }

    pub(super) fn wrap(_req: &ExecRequest, _cwd: &Path, _temp: Option<&Path>) -> Result<Vec<OsString>, SandboxError> {
        Err(SandboxError::Unavailable("no sandbox backend for this platform".into()))
    }
}

pub(crate) fn status(_backend: Backend) -> SandboxStatus {
    backend::status()
}

pub(crate) fn launch(req: &ExecRequest) -> Result<Launched, SandboxError> {
    let sandboxed = req.policy.is_sandboxed();
    let cwd = canonical(&req.cwd);
    let mut temp: Option<PathBuf> = None;
    let argv: Vec<OsString> = if sandboxed {
        if req.backend == Backend::None {
            return Err(SandboxError::Unavailable("sandbox backend is disabled (`none`)".into()));
        }
        let t = req.effective_sandbox_temp();
        if std::fs::create_dir_all(&t).is_ok() {
            temp = Some(canonical(&t));
        }
        backend::wrap(req, &cwd, temp.as_deref())?
    } else {
        req.argv.iter().map(OsString::from).collect()
    };

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(&cwd).envs(&req.env);
    if sandboxed {
        if let Some(t) = &temp {
            cmd.env("TMPDIR", t);
        }
        cmd.env("ODEX_SANDBOX", backend::NAME);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(SandboxError::Spawn)?;
    let pid = child.id();
    let stdin = child.stdin.take().ok_or_else(|| SandboxError::Other("missing stdin pipe".into()))?;
    let stdout = child.stdout.take().ok_or_else(|| SandboxError::Other("missing stdout pipe".into()))?;
    let stderr = child.stderr.take().ok_or_else(|| SandboxError::Other("missing stderr pipe".into()))?;
    Ok(Launched {
        pid,
        sandboxed,
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: Box::new(stderr),
        waiter: Box::new(move || match child.wait() {
            Ok(status) => status.code().or_else(|| status.signal().map(|s| 128 + s)),
            Err(_) => None,
        }),
        killer: Arc::new(GroupKiller { pgid: pid as libc::pid_t }),
    })
}
