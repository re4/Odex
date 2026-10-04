//! Spawning `git` (and `gh`) with a hardened, non-interactive environment.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncWriteExt;

use crate::error::{GitError, Result};

/// Environment variables inherited from a parent git process (e.g. when Odex is started from a
/// hook) that would redirect our commands to another repository or index.
const SCRUBBED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Captured result of a finished process.
#[derive(Debug, Clone)]
pub(crate) struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }

    /// stdout without the trailing newline.
    pub fn stdout_line(&self) -> String {
        self.stdout_str().trim_end_matches(['\n', '\r']).to_string()
    }

    /// stdout split on NUL, empty tokens removed.
    pub fn nul_tokens(&self) -> Vec<String> {
        self.stdout
            .split(|&b| b == 0)
            .filter(|t| !t.is_empty())
            .map(|t| String::from_utf8_lossy(t).into_owned())
            .collect()
    }
}

/// Builder for one `git` invocation.
#[derive(Debug, Clone)]
pub(crate) struct GitCommand {
    program: &'static str,
    cwd: PathBuf,
    args: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    stdin: Option<Vec<u8>>,
}

impl GitCommand {
    pub fn new(cwd: &Path) -> Self {
        GitCommand {
            program: "git",
            cwd: cwd.to_path_buf(),
            args: vec!["-c".into(), "core.quotepath=false".into(), "-c".into(), "color.ui=false".into()],
            envs: Vec::new(),
            stdin: None,
        }
    }

    /// A `gh` invocation with the same environment hardening.
    pub fn gh(cwd: &Path) -> Self {
        GitCommand { program: "gh", cwd: cwd.to_path_buf(), args: Vec::new(), envs: Vec::new(), stdin: None }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(args.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs.push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Treat pathspecs literally (no glob magic) — for user-supplied file paths.
    pub fn literal_pathspecs(self) -> Self {
        self.env("GIT_LITERAL_PATHSPECS", "1")
    }

    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    fn describe(&self) -> String {
        let skip = if self.program == "git" { 4 } else { 0 };
        self.args.iter().skip(skip).map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }

    /// Run and capture output regardless of the exit status.
    pub async fn output(self) -> Result<Output> {
        let mut cmd = tokio::process::Command::new(self.program);
        cmd.current_dir(&self.cwd).args(&self.args).kill_on_drop(true);
        for key in SCRUBBED_ENV {
            cmd.env_remove(key);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("PAGER", "cat")
            .env("GIT_EDITOR", ":")
            .env("GIT_SEQUENCE_EDITOR", ":")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1");
        // Stable, untranslated messages. On Windows the C locale can upset Git for Windows'
        // bundled MSYS tools, so only the message language is pinned there.
        #[cfg(not(windows))]
        cmd.env("LC_ALL", "C");
        #[cfg(windows)]
        cmd.env("LANGUAGE", "en");
        for (k, v) in &self.envs {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.stdin(if self.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                if self.program == "git" {
                    GitError::GitNotFound
                } else {
                    GitError::Invalid(format!("`{}` is not installed", self.program))
                }
            } else {
                GitError::Io(e)
            }
        })?;
        let writer = match (self.stdin, child.stdin.take()) {
            (Some(data), Some(mut pipe)) => Some(tokio::spawn(async move {
                // A broken pipe just means the process did not need all of its input.
                let _ = pipe.write_all(&data).await;
                let _ = pipe.shutdown().await;
            })),
            _ => None,
        };
        let out = child.wait_with_output().await?;
        if let Some(w) = writer {
            let _ = w.await;
        }
        Ok(Output { code: out.status.code(), stdout: out.stdout, stderr: out.stderr })
    }

    /// Run and fail on a non-zero exit status.
    pub async fn run(self) -> Result<Output> {
        let program = self.program.to_string();
        let desc = self.describe();
        let cwd = self.cwd.clone();
        let out = self.output().await?;
        if out.success() {
            Ok(out)
        } else {
            Err(command_error(&program, desc, &cwd, &out))
        }
    }

    /// Run and return stdout without its trailing newline.
    pub async fn run_line(self) -> Result<String> {
        Ok(self.run().await?.stdout_line())
    }
}

pub(crate) fn command_error(program: &str, args: String, cwd: &Path, out: &Output) -> GitError {
    let stderr = out.stderr_str();
    if program == "git" && stderr.contains("not a git repository") {
        return GitError::NotARepo(cwd.to_path_buf());
    }
    GitError::Command { program: program.to_string(), args, code: out.code, stderr, stdout: out.stdout_str() }
}

/// Convert a path printed by git (forward slashes on every platform) to a native `PathBuf`.
pub(crate) fn native_path(s: &str) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(s.replace('/', "\\"))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(s)
    }
}

/// NUL-join paths for `--pathspec-from-file=- --pathspec-file-nul` / `--stdin -z`.
pub(crate) fn nul_join<S: AsRef<str>>(items: &[S]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in items {
        out.extend_from_slice(item.as_ref().as_bytes());
        out.push(0);
    }
    out
}
