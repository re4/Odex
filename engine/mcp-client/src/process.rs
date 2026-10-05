//! Child-process helpers: command resolution (Windows `npx` → `npx.cmd`) and
//! killing the whole process tree.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Resolve `command` the way a shell would. On Windows, `Command::new("npx")`
/// only finds `npx.exe`; Node/Python launchers are usually `.cmd`/`.bat`
/// shims, so search `PATH` with `PATHEXT`-style extensions ourselves.
/// `path_var` is the child's `PATH` (if the server config overrides it).
pub(crate) fn resolve_command(command: &str, path_var: Option<&OsStr>) -> PathBuf {
    if cfg!(windows) {
        resolve_windows(command, path_var)
    } else {
        PathBuf::from(command)
    }
}

fn windows_extensions() -> Vec<String> {
    let mut exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_default()
        .split(';')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| e.starts_with('.') && e.len() > 1)
        .collect();
    for e in [".exe", ".cmd", ".bat", ".com"] {
        if !exts.iter().any(|x| x == e) {
            exts.push(e.to_string());
        }
    }
    // Only things `CreateProcess` (or Rust's batch-file support) can launch.
    exts.retain(|e| matches!(e.as_str(), ".exe" | ".cmd" | ".bat" | ".com"));
    exts
}

fn resolve_windows(command: &str, path_var: Option<&OsStr>) -> PathBuf {
    let p = Path::new(command);
    let has_ext = p.extension().is_some();
    let has_dir = command.contains('/') || command.contains('\\');
    let candidates = |base: &Path| -> Vec<PathBuf> {
        let mut v = Vec::new();
        if has_ext {
            v.push(base.to_path_buf());
        }
        for e in windows_extensions() {
            let mut s: OsString = base.as_os_str().to_owned();
            s.push(&e);
            v.push(PathBuf::from(s));
        }
        v
    };
    if has_dir {
        return candidates(p).into_iter().find(|c| c.is_file()).unwrap_or_else(|| p.to_path_buf());
    }
    let path_value: OsString = match path_var {
        Some(v) => v.to_owned(),
        None => std::env::var_os("PATH").unwrap_or_default(),
    };
    for dir in std::env::split_paths(&path_value) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if let Some(found) = candidates(&dir.join(command)).into_iter().find(|c| c.is_file()) {
            return found;
        }
    }
    p.to_path_buf()
}

/// The child's process tree: a kill-on-close job object on Windows (so `npx`
/// shims don't leave orphaned `node.exe` children behind), the process group
/// on Unix.
pub(crate) struct ProcessTree {
    #[cfg(windows)]
    job: Option<JobObject>,
    #[cfg(unix)]
    pid: Option<u32>,
}

impl ProcessTree {
    pub(crate) fn attach(child: &tokio::process::Child) -> ProcessTree {
        #[cfg(windows)]
        {
            ProcessTree { job: JobObject::for_child(child) }
        }
        #[cfg(unix)]
        {
            ProcessTree { pid: child.id() }
        }
        #[cfg(not(any(windows, unix)))]
        {
            let _ = child;
            ProcessTree {}
        }
    }

    /// Ask the tree to exit (SIGTERM on Unix; nothing on Windows).
    pub(crate) fn terminate_gracefully(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: kill(2) with a negative pid targets the process group we created.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
        }
    }

    /// Kill every process in the tree.
    pub(crate) fn kill(&self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.terminate();
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // SAFETY: see above.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
struct JobObject(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: a job handle is a kernel object handle usable from any thread.
#[cfg(windows)]
unsafe impl Send for JobObject {}
// SAFETY: see above; we only call thread-safe Win32 APIs on it.
#[cfg(windows)]
unsafe impl Sync for JobObject {}

#[cfg(windows)]
impl JobObject {
    fn for_child(child: &tokio::process::Child) -> Option<JobObject> {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let process = child.raw_handle()?;
        // SAFETY: plain Win32 calls with valid arguments; the handle is owned by `JobObject`.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let job = JobObject(job);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                return None;
            }
            if AssignProcessToJobObject(job.0, process as windows_sys::Win32::Foundation::HANDLE) == 0 {
                return None;
            }
            Some(job)
        }
    }

    fn terminate(&self) {
        // SAFETY: valid job handle owned by self.
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for JobObject {
    fn drop(&mut self) {
        // SAFETY: we own the handle; closing it kills the remaining processes.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn resolves_cmd_shims_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fakenpx.cmd"), "@echo off\r\n").unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        let resolved = resolve_command("fakenpx", Some(&path));
        assert_eq!(resolved, dir.path().join("fakenpx.cmd"));
        // explicit extension / unknown commands pass through
        assert_eq!(resolve_command("does-not-exist-xyz", Some(&path)), PathBuf::from("does-not-exist-xyz"));
        // system PATH
        let cmd = resolve_command("cmd", None);
        assert!(cmd.to_string_lossy().to_ascii_lowercase().ends_with("cmd.exe"), "{cmd:?}");
        // a path without extension
        let full = dir.path().join("fakenpx");
        assert_eq!(resolve_command(full.to_str().unwrap(), None), dir.path().join("fakenpx.cmd"));
    }

    #[cfg(not(windows))]
    #[test]
    fn passthrough_elsewhere() {
        assert_eq!(resolve_command("npx", None), PathBuf::from("npx"));
    }

    /// Killing the tree must also kill grandchildren: they hold the stdout
    /// pipe, so EOF only arrives once every process in the tree is gone.
    #[cfg(any(windows, unix))]
    #[tokio::test]
    async fn kill_takes_down_grandchildren() {
        use tokio::io::AsyncReadExt;
        #[cfg(windows)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("cmd");
            c.args(["/c", "ping -n 60 127.0.0.1"]);
            c
        };
        #[cfg(unix)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("sh");
            c.args(["-c", "sleep 60; echo done"]);
            c.process_group(0);
            c
        };
        cmd.stdout(std::process::Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().unwrap();
        let tree = ProcessTree::attach(&child);
        let mut out = child.stdout.take().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        tree.kill();
        let mut buf = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(10), out.read_to_end(&mut buf))
            .await
            .expect("a grandchild survived and kept the pipe open")
            .unwrap();
        child.wait().await.unwrap();
    }
}
