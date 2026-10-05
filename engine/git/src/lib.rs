//! Git integration for Odex.
//!
//! Everything shells out to the `git` CLI rather than linking libgit2, so user configuration,
//! hooks, credential helpers, filters (LFS, autocrlf) and `.gitattributes` behave exactly as they
//! do in the user's terminal. Commands run non-interactively (`GIT_TERMINAL_PROMPT=0`, no editor,
//! no pager, no console window on Windows) and use NUL-separated porcelain formats.
//!
//! Paths in inputs and outputs (status, diffs, stage/unstage/revert) are relative to the
//! repository root with forward slashes, matching git's porcelain output.
//!
//! Timestamps (`GitCommitInfo::timestamp`, PR timeline/comment times) are Unix milliseconds.

mod cmd;
mod diff;
mod error;
pub mod pr;
mod repo;
mod snapshot;
mod status;
mod worktree;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::OnceCell;

pub use diff::{build_hunk_patch, parse_unified_diff};
pub use error::{GitError, Result};
pub use worktree::MovedChanges;

use cmd::{native_path, GitCommand};

/// A git working tree (or any directory inside one).
#[derive(Debug, Clone)]
pub struct Git {
    cwd: PathBuf,
    root: Arc<OnceCell<PathBuf>>,
}

impl Git {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Git { cwd: cwd.into(), root: Arc::new(OnceCell::new()) }
    }

    /// The directory this handle was created for.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn cmd(&self) -> GitCommand {
        GitCommand::new(&self.cwd)
    }

    pub(crate) fn cmd_at(&self, dir: &Path) -> GitCommand {
        GitCommand::new(dir)
    }

    /// Whether `cwd` is inside a git working tree.
    pub async fn is_repo(&self) -> bool {
        if !self.cwd.is_dir() {
            return false;
        }
        match self.cmd().args(["rev-parse", "--is-inside-work-tree"]).output().await {
            Ok(out) => out.success() && out.stdout_line() == "true",
            Err(_) => false,
        }
    }

    /// Top-level directory of the working tree (cached after the first success).
    pub async fn repo_root(&self) -> Result<PathBuf> {
        if !self.cwd.is_dir() {
            return Err(GitError::NotARepo(self.cwd.clone()));
        }
        self.root
            .get_or_try_init(|| async {
                let line = self.cmd().args(["rev-parse", "--show-toplevel"]).run_line().await?;
                if line.is_empty() {
                    return Err(GitError::NotARepo(self.cwd.clone()));
                }
                Ok(native_path(&line))
            })
            .await
            .cloned()
    }

    /// `git init` in `cwd` (created if missing).
    pub async fn init(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.cwd).await?;
        self.cmd().args(["init", "-q"]).run().await?;
        Ok(())
    }

    /// Commit id of `HEAD`, or `None` on an unborn branch.
    pub async fn head_sha(&self) -> Result<Option<String>> {
        let root = self.repo_root().await?;
        self.rev_parse_opt(&root, "HEAD").await
    }

    /// `git rev-parse --verify -q <rev>^{commit}`-style lookup that maps "unknown" to `None`.
    pub(crate) async fn rev_parse_opt(&self, root: &Path, rev: &str) -> Result<Option<String>> {
        let out = self.cmd_at(root).args(["rev-parse", "--verify", "-q", rev]).output().await?;
        if out.success() {
            Ok(Some(out.stdout_line()))
        } else if out.stderr_str().contains("not a git repository") {
            Err(GitError::NotARepo(self.cwd.clone()))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A throwaway repository configured only through its own `.git/config`.
    pub struct TestRepo {
        _dir: tempfile::TempDir,
        pub path: PathBuf,
    }

    impl TestRepo {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("repo");
            std::fs::create_dir_all(&path).unwrap();
            let repo = TestRepo { _dir: dir, path };
            repo.git(&["init", "-q", "-b", "main"]);
            repo.configure(&repo.path);
            repo
        }

        /// Apply the hermetic test config to a repository (or worktree) at `dir`.
        pub fn configure(&self, dir: &Path) {
            for (k, v) in [
                ("user.name", "Odex Test"),
                ("user.email", "test@odex.invalid"),
                ("core.autocrlf", "false"),
                ("core.safecrlf", "false"),
                ("commit.gpgsign", "false"),
                ("tag.gpgsign", "false"),
                ("core.hooksPath", ".no-hooks"),
                ("merge.conflictstyle", "merge"),
            ] {
                run(dir, &["config", k, v]);
            }
        }

        pub fn git(&self, args: &[&str]) -> String {
            run(&self.path, args)
        }

        pub fn write(&self, rel: &str, content: &str) {
            let p = self.path.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }

        pub fn read(&self, rel: &str) -> String {
            std::fs::read_to_string(self.path.join(rel)).unwrap()
        }

        pub fn exists(&self, rel: &str) -> bool {
            self.path.join(rel).exists()
        }

        pub fn commit_all(&self, msg: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "-q", "-m", msg]);
        }

        /// A sibling path inside the same temp dir (for worktrees).
        pub fn sibling(&self, name: &str) -> PathBuf {
            self.path.parent().unwrap().join(name)
        }
    }

    pub fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "core.quotepath=false"])
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}
