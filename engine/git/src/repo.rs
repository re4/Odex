//! Status, diffs, staging, commits, pushes, branches and history.

use std::path::Path;

use odex_protocol::{DiffFile, DiffStats, DiffTarget, GitBranch, GitCommitInfo, GitStatus, RepoDiff};

use crate::cmd::nul_join;
use crate::diff::{
    build_hunk_patch, hunk_has_no_context, parse_numstat_z, parse_unified_diff, untracked_file_diff,
    untracked_line_count,
};
use crate::error::{GitError, Result};
use crate::status::parse_porcelain_v2;
use crate::Git;

/// Keep individual command lines well below Windows' 32K limit.
const MAX_ARGS_CHARS: usize = 8_000;

/// What a [`DiffTarget`] compares, as `git diff` arguments.
pub(crate) struct DiffSpec {
    pub revs: Vec<String>,
    pub untracked: bool,
}

/// Base `git diff` arguments: stable prefixes, no color/external tools/textconv, rename detection.
fn diff_args(ignore_whitespace: bool, context_lines: Option<u32>) -> Vec<String> {
    let mut args: Vec<String> = [
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--submodule=short",
        "-M",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if ignore_whitespace {
        args.push("-w".into());
    }
    if let Some(n) = context_lines {
        args.push(format!("-U{n}"));
    }
    args
}

/// Split `paths` into chunks whose joined length stays under [`MAX_ARGS_CHARS`].
fn chunk_paths(paths: &[String]) -> Vec<&[String]> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut len = 0;
    for (i, p) in paths.iter().enumerate() {
        if i > start && len + p.len() + 1 > MAX_ARGS_CHARS {
            chunks.push(&paths[start..i]);
            start = i;
            len = 0;
        }
        len += p.len() + 1;
    }
    if start < paths.len() {
        chunks.push(&paths[start..]);
    }
    chunks
}

impl Git {
    /// Working tree status. Outside a repository this returns `is_repo: false` (not an error).
    pub async fn status(&self) -> Result<GitStatus> {
        let root = match self.repo_root().await {
            Ok(root) => root,
            Err(GitError::NotARepo(_)) => return Ok(not_a_repo()),
            Err(e) => return Err(e),
        };
        let status = self
            .cmd_at(&root)
            .args(["status", "--porcelain=v2", "-z", "--branch", "--show-stash", "--untracked-files=all"])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .run();
        let remote = self.cmd_at(&root).args(["remote", "get-url", "origin"]).output();
        let (status, remote) = tokio::join!(status, remote);
        let parsed = parse_porcelain_v2(&status?.stdout);
        let remote_url = remote.ok().filter(|o| o.success()).map(|o| o.stdout_line()).filter(|s| !s.is_empty());
        Ok(GitStatus {
            is_repo: true,
            repo_root: Some(root.to_string_lossy().into_owned()),
            branch: parsed.branch,
            head: parsed.head,
            upstream: parsed.upstream,
            ahead: parsed.ahead,
            behind: parsed.behind,
            files: parsed.files,
            stash_count: parsed.stash_count,
            remote_url,
        })
    }

    /// The id of the empty tree in this repository's hash algorithm.
    pub(crate) async fn empty_tree(&self, root: &Path) -> Result<String> {
        self.cmd_at(root).args(["hash-object", "-t", "tree", "--stdin"]).stdin(Vec::new()).run_line().await
    }

    /// `HEAD`, or the empty tree on an unborn branch.
    async fn head_or_empty(&self, root: &Path) -> Result<String> {
        match self.rev_parse_opt(root, "HEAD").await? {
            Some(sha) => Ok(sha),
            None => self.empty_tree(root).await,
        }
    }

    pub(crate) async fn diff_spec(&self, root: &Path, target: &DiffTarget) -> Result<DiffSpec> {
        Ok(match target {
            DiffTarget::Uncommitted => DiffSpec { revs: vec![self.head_or_empty(root).await?], untracked: true },
            DiffTarget::Staged => DiffSpec { revs: vec!["--cached".into()], untracked: false },
            DiffTarget::Unstaged => DiffSpec { revs: vec![], untracked: true },
            DiffTarget::Base { branch } => {
                if branch.starts_with('-') {
                    return Err(GitError::Invalid(format!("invalid branch name {branch:?}")));
                }
                let base = self.cmd_at(root).args(["merge-base", branch.as_str(), "HEAD"]).run_line().await?;
                DiffSpec { revs: vec![base], untracked: true }
            }
            DiffTarget::Commit { sha } => {
                if sha.starts_with('-') {
                    return Err(GitError::Invalid(format!("invalid commit {sha:?}")));
                }
                let commit = self
                    .rev_parse_opt(root, &format!("{sha}^{{commit}}"))
                    .await?
                    .ok_or_else(|| GitError::Invalid(format!("unknown commit {sha}")))?;
                let parent = match self.rev_parse_opt(root, &format!("{commit}^1")).await? {
                    Some(p) => p,
                    None => self.empty_tree(root).await?,
                };
                DiffSpec { revs: vec![parent, commit], untracked: false }
            }
            DiffTarget::LastTurn { .. } => {
                return Err(GitError::Invalid(
                    "the lastTurn diff target is resolved by the engine (see Git::diff_from_ref)".into(),
                ))
            }
        })
    }

    /// Untracked, non-ignored files (repo-relative).
    pub(crate) async fn untracked_files(&self, root: &Path) -> Result<Vec<String>> {
        let out = self.cmd_at(root).args(["ls-files", "--others", "--exclude-standard", "-z"]).run().await?;
        Ok(out.nul_tokens())
    }

    async fn untracked_diffs(root: &Path, paths: Vec<String>) -> Result<Vec<DiffFile>> {
        let root = root.to_path_buf();
        let files = tokio::task::spawn_blocking(move || {
            paths.iter().filter_map(|p| untracked_file_diff(&root, p)).collect::<Vec<_>>()
        })
        .await
        .map_err(|e| GitError::Invalid(format!("untracked diff task failed: {e}")))?;
        Ok(files)
    }

    /// Diff for `target`; see [`DiffTarget`]. Untracked files are included (status `untracked`,
    /// every line added) for `Uncommitted`, `Unstaged` and `Base`.
    pub async fn diff(
        &self,
        target: &DiffTarget,
        ignore_whitespace: bool,
        context_lines: Option<u32>,
    ) -> Result<RepoDiff> {
        let root = self.repo_root().await?;
        let spec = self.diff_spec(&root, target).await?;
        let mut args = diff_args(ignore_whitespace, context_lines);
        args.extend(spec.revs.iter().cloned());
        args.push("--".into());
        let tracked = self.cmd_at(&root).args(&args).run();
        let untracked = async {
            if spec.untracked {
                self.untracked_files(&root).await
            } else {
                Ok(Vec::new())
            }
        };
        let (tracked, untracked) = tokio::join!(tracked, untracked);
        let mut files = parse_unified_diff(&tracked?.stdout_str());
        files.extend(Self::untracked_diffs(&root, untracked?).await?);
        Ok(make_repo_diff(&root, target.clone(), files))
    }

    /// Diff from `from_ref` (any tree-ish) to the current working tree including untracked
    /// files. Builds the working-tree tree in a temporary index; the real index is untouched.
    pub async fn diff_from_ref(&self, from_ref: &str, ignore_whitespace: bool) -> Result<RepoDiff> {
        if from_ref.starts_with('-') {
            return Err(GitError::Invalid(format!("invalid ref {from_ref:?}")));
        }
        let root = self.repo_root().await?;
        let tree = self.worktree_tree(&root).await?;
        let mut args = diff_args(ignore_whitespace, None);
        args.push(from_ref.to_string());
        args.push(tree);
        args.push("--".into());
        let out = self.cmd_at(&root).args(&args).run().await?;
        let files = parse_unified_diff(&out.stdout_str());
        Ok(make_repo_diff(&root, DiffTarget::Commit { sha: from_ref.to_string() }, files))
    }

    /// Files changed / lines added / lines removed for `target` (untracked files count as added).
    pub async fn diff_stats(&self, target: &DiffTarget) -> Result<DiffStats> {
        let root = self.repo_root().await?;
        let spec = self.diff_spec(&root, target).await?;
        let mut args: Vec<String> = ["diff", "--numstat", "-z", "--no-color", "--no-ext-diff", "--no-textconv", "-M"]
            .map(String::from)
            .to_vec();
        args.extend(spec.revs.iter().cloned());
        args.push("--".into());
        let out = self.cmd_at(&root).args(&args).run().await?;
        let mut stats = parse_numstat_z(&out.stdout);
        if spec.untracked {
            let untracked = self.untracked_files(&root).await?;
            let root2 = root.clone();
            let lines = tokio::task::spawn_blocking(move || {
                untracked
                    .iter()
                    .filter(|p| !p.ends_with('/'))
                    .map(|p| untracked_line_count(&root2, p))
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| GitError::Invalid(format!("stats task failed: {e}")))?;
            stats.files_changed += lines.len() as u32;
            stats.additions += lines.iter().sum::<u32>();
        }
        Ok(stats)
    }

    /// Stage paths (`git add -A -- paths`); empty stages everything.
    pub async fn stage(&self, paths: &[String]) -> Result<()> {
        let root = self.repo_root().await?;
        let cmd = self.cmd_at(&root).args(["add", "-A"]);
        if paths.is_empty() {
            cmd.run().await?;
        } else {
            cmd.args(["--pathspec-from-file=-", "--pathspec-file-nul"])
                .literal_pathspecs()
                .stdin(nul_join(paths))
                .run()
                .await?;
        }
        Ok(())
    }

    /// Unstage paths (back to `HEAD`, or out of the index on an unborn branch); empty = all.
    pub async fn unstage(&self, paths: &[String]) -> Result<()> {
        let root = self.repo_root().await?;
        let has_head = self.rev_parse_opt(&root, "HEAD").await?.is_some();
        let cmd = if has_head {
            self.cmd_at(&root).args(["reset", "-q"])
        } else {
            self.cmd_at(&root).args(["rm", "--cached", "-r", "-q", "--ignore-unmatch"])
        };
        if paths.is_empty() {
            if has_head {
                cmd.run().await?;
            } else {
                cmd.arg("--").arg(".").run().await?;
            }
        } else {
            cmd.args(["--pathspec-from-file=-", "--pathspec-file-nul"])
                .literal_pathspecs()
                .stdin(nul_join(paths))
                .run()
                .await?;
        }
        Ok(())
    }

    /// Discard working-tree changes: tracked files go back to their index version (staged
    /// changes are kept), untracked files/directories under `paths` are deleted.
    /// **An empty `paths` discards every unstaged change in the repository.**
    pub async fn revert(&self, paths: &[String]) -> Result<()> {
        let root = self.repo_root().await?;
        // Tracked files whose working copy differs from the index (including deletions).
        let mut changed = self.cmd_at(&root).args(["diff", "--name-only", "-z", "--no-renames", "--"]);
        if !paths.is_empty() {
            changed = changed.args(paths).literal_pathspecs();
        }
        let changed = changed.run().await?.nul_tokens();
        if !changed.is_empty() {
            self.cmd_at(&root)
                .args(["checkout-index", "-f", "-q", "-z", "--stdin"])
                .stdin(nul_join(&changed))
                .run()
                .await?;
        }
        // Untracked (not ignored) files and directories.
        if paths.is_empty() {
            self.cmd_at(&root).args(["clean", "-f", "-d", "-q"]).run().await?;
        } else {
            for chunk in chunk_paths(paths) {
                self.cmd_at(&root)
                    .args(["clean", "-f", "-d", "-q", "--"])
                    .args(chunk)
                    .literal_pathspecs()
                    .run()
                    .await?;
            }
        }
        Ok(())
    }

    async fn apply_hunk(&self, file: &DiffFile, hunk_index: u32, extra: &[&str]) -> Result<()> {
        let root = self.repo_root().await?;
        let patch = build_hunk_patch(file, hunk_index)?;
        let mut cmd = self.cmd_at(&root).args(["apply", "--recount", "--whitespace=nowarn"]).args(extra);
        if hunk_has_no_context(file, hunk_index) {
            cmd = cmd.arg("--unidiff-zero");
        }
        cmd.arg("-").stdin(patch).run().await?;
        Ok(())
    }

    /// Stage one hunk. `file` must come from the `Unstaged` diff. An untracked file's only hunk
    /// is the whole file, so it is staged with `git add` (keeping clean filters such as autocrlf).
    pub async fn stage_hunk(&self, file: &DiffFile, hunk_index: u32) -> Result<()> {
        if file.status == "untracked" {
            ensure_hunk(file, hunk_index)?;
            return self.stage(std::slice::from_ref(&file.path)).await;
        }
        self.apply_hunk(file, hunk_index, &["--cached"]).await
    }

    /// Unstage one hunk. `file` must come from the `Staged` diff.
    pub async fn unstage_hunk(&self, file: &DiffFile, hunk_index: u32) -> Result<()> {
        self.apply_hunk(file, hunk_index, &["--cached", "-R"]).await
    }

    /// Discard one hunk from the working tree. `file` must come from the `Unstaged` diff.
    /// For an untracked file (a single whole-file hunk) this deletes the file.
    pub async fn revert_hunk(&self, file: &DiffFile, hunk_index: u32) -> Result<()> {
        if file.status == "untracked" {
            ensure_hunk(file, hunk_index)?;
            return self.revert(std::slice::from_ref(&file.path)).await;
        }
        self.apply_hunk(file, hunk_index, &["-R"]).await
    }

    /// Commit. `all` first stages everything (`git add -A`, including untracked files).
    /// Returns the new commit id and its subject line. Hooks run as usual.
    pub async fn commit(&self, message: &str, all: bool, amend: bool) -> Result<(String, String)> {
        let root = self.repo_root().await?;
        if all {
            self.cmd_at(&root).args(["add", "-A"]).run().await?;
        }
        let mut cmd = self.cmd_at(&root).arg("commit");
        if amend {
            cmd = cmd.arg("--amend");
        }
        if message.trim().is_empty() {
            if !amend {
                return Err(GitError::Invalid("commit message is empty".into()));
            }
            cmd = cmd.arg("--no-edit");
        } else {
            cmd = cmd.args(["--cleanup=strip", "-F", "-"]).stdin(message.as_bytes().to_vec());
        }
        cmd.run().await?;
        let sha = self.cmd_at(&root).args(["rev-parse", "HEAD"]).run_line().await?;
        let subject = self.cmd_at(&root).args(["log", "-1", "--format=%s", "HEAD"]).run_line().await?;
        Ok((sha, subject))
    }

    /// `git push`; returns git's combined output. With `set_upstream` and no branch, the current
    /// branch is pushed to `remote` (default `origin`).
    pub async fn push(
        &self,
        remote: Option<&str>,
        branch: Option<&str>,
        set_upstream: bool,
        force_with_lease: bool,
    ) -> Result<String> {
        let root = self.repo_root().await?;
        let mut cmd = self.cmd_at(&root).args(["push", "--porcelain"]);
        if set_upstream {
            cmd = cmd.arg("--set-upstream");
        }
        if force_with_lease {
            cmd = cmd.arg("--force-with-lease");
        }
        let branch = match branch {
            Some(b) => Some(b.to_string()),
            None if set_upstream => Some(
                self.current_branch()
                    .await?
                    .ok_or_else(|| GitError::Invalid("cannot set upstream from a detached HEAD".into()))?,
            ),
            None => None,
        };
        for value in [remote, branch.as_deref()].into_iter().flatten() {
            if value.starts_with('-') {
                return Err(GitError::Invalid(format!("invalid push argument {value:?}")));
            }
        }
        match (remote, branch.as_deref()) {
            (Some(r), Some(b)) => cmd = cmd.args([r, b]),
            (Some(r), None) => cmd = cmd.arg(r),
            (None, Some(b)) => cmd = cmd.args(["origin", b]),
            (None, None) => {}
        }
        let out = cmd.run().await?;
        let mut text = out.stdout_str();
        let err = out.stderr_str();
        if !err.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&err);
        }
        Ok(text.trim_end().to_string())
    }

    /// Local and remote-tracking branches.
    pub async fn branches(&self) -> Result<Vec<GitBranch>> {
        let root = self.repo_root().await?;
        let out = self
            .cmd_at(&root)
            .args([
                "for-each-ref",
                "--format=%(refname)%00%(refname:short)%00%(HEAD)%00%(upstream:short)",
                "refs/heads",
                "refs/remotes",
            ])
            .run()
            .await?;
        let mut branches = Vec::new();
        for line in out.stdout_str().lines() {
            let f: Vec<&str> = line.split('\0').collect();
            if f.len() < 4 {
                continue;
            }
            let remote = f[0].starts_with("refs/remotes/");
            if remote && f[0].ends_with("/HEAD") {
                continue;
            }
            branches.push(GitBranch {
                name: f[1].to_string(),
                current: f[2] == "*",
                remote,
                upstream: Some(f[3].to_string()).filter(|s| !s.is_empty()),
            });
        }
        Ok(branches)
    }

    /// Checked-out branch name; `None` when detached. Works on unborn branches.
    pub async fn current_branch(&self) -> Result<Option<String>> {
        let root = self.repo_root().await?;
        let out = self.cmd_at(&root).args(["symbolic-ref", "--short", "-q", "HEAD"]).output().await?;
        Ok(if out.success() { Some(out.stdout_line()).filter(|s| !s.is_empty()) } else { None })
    }

    /// The default branch: `origin/HEAD`, else `main`, else `master`, else the current branch.
    pub async fn default_branch(&self) -> Result<String> {
        let root = self.repo_root().await?;
        let out =
            self.cmd_at(&root).args(["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"]).output().await?;
        if out.success() {
            let name = out.stdout_line();
            if let Some(stripped) = name.strip_prefix("origin/") {
                return Ok(stripped.to_string());
            }
        }
        for candidate in ["main", "master"] {
            for prefix in ["refs/heads/", "refs/remotes/origin/"] {
                let r = format!("{prefix}{candidate}");
                if self.cmd_at(&root).args(["show-ref", "--verify", "-q", r.as_str()]).output().await?.success() {
                    return Ok(candidate.to_string());
                }
            }
        }
        Ok(self.current_branch().await?.unwrap_or_else(|| "main".to_string()))
    }

    /// Recent commits on `HEAD` (empty on an unborn branch).
    pub async fn log(&self, limit: u32) -> Result<Vec<GitCommitInfo>> {
        let root = self.repo_root().await?;
        if self.rev_parse_opt(&root, "HEAD").await?.is_none() {
            return Ok(Vec::new());
        }
        let n = format!("-n{}", limit.max(1));
        let out = self
            .cmd_at(&root)
            .args(["log", n.as_str(), "-z", "--format=%H%x1f%h%x1f%s%x1f%an%x1f%ct", "HEAD", "--"])
            .run()
            .await?;
        Ok(out
            .nul_tokens()
            .iter()
            .filter_map(|rec| {
                let f: Vec<&str> = rec.trim_start_matches('\n').split('\x1f').collect();
                (f.len() == 5).then(|| GitCommitInfo {
                    sha: f[0].to_string(),
                    short_sha: f[1].to_string(),
                    subject: f[2].to_string(),
                    author: f[3].to_string(),
                    timestamp: f[4].trim().parse::<i64>().unwrap_or(0) * 1000,
                })
            })
            .collect())
    }

    /// Paths with unresolved merge conflicts.
    pub async fn conflicted_files(&self) -> Result<Vec<String>> {
        let root = self.repo_root().await?;
        let out = self.cmd_at(&root).args(["diff", "--name-only", "-z", "--diff-filter=U"]).run().await?;
        let mut files = out.nul_tokens();
        files.dedup();
        Ok(files)
    }
}

fn ensure_hunk(file: &DiffFile, hunk_index: u32) -> Result<()> {
    if file.hunks.iter().any(|h| h.index == hunk_index) || (file.hunks.is_empty() && hunk_index == 0) {
        Ok(())
    } else {
        Err(GitError::Invalid(format!("{} has no hunk {hunk_index}", file.path)))
    }
}

fn not_a_repo() -> GitStatus {
    GitStatus {
        is_repo: false,
        repo_root: None,
        branch: None,
        head: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        files: Vec::new(),
        stash_count: 0,
        remote_url: None,
    }
}

fn make_repo_diff(root: &Path, target: DiffTarget, mut files: Vec<DiffFile>) -> RepoDiff {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let additions = files.iter().map(|f| f.additions).sum();
    let deletions = files.iter().map(|f| f.deletions).sum();
    RepoDiff { repo_root: root.to_string_lossy().into_owned(), target, files, additions, deletions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;
    use odex_protocol::DiffLineKind;

    #[test]
    fn futures_are_send() {
        fn is_send<T: Send>(_: &T) {}
        let g = Git::new(".");
        is_send(&g.status());
        is_send(&g.diff(&DiffTarget::Uncommitted, false, None));
        is_send(&g.diff_from_ref("HEAD", false));
        is_send(&g.revert(&[]));
        is_send(&g.push(None, None, false, false));
    }

    fn lines(n: usize, tag: &str) -> String {
        (1..=n).map(|i| format!("{tag}{i}\n")).collect()
    }

    #[tokio::test]
    async fn status_outside_repo_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let st = Git::new(dir.path()).status().await.unwrap();
        assert!(!st.is_repo);
        assert!(!Git::new(dir.path()).is_repo().await);
        assert!(matches!(Git::new(dir.path()).repo_root().await, Err(GitError::NotARepo(_))));
    }

    #[tokio::test]
    async fn status_reports_rename_untracked_and_conflict() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        let st = git.status().await.unwrap();
        assert!(st.is_repo);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!(st.head, None);

        repo.write("a.txt", &lines(20, "a"));
        repo.write("conflict.txt", "base\n");
        repo.commit_all("init");
        repo.git(&["mv", "a.txt", "b.txt"]);
        repo.write("new dir/u.txt", "u\n");
        repo.git(&["remote", "add", "origin", "https://github.com/odex-app/odex.git"]);
        let st = git.status().await.unwrap();
        assert_eq!(st.remote_url.as_deref(), Some("https://github.com/odex-app/odex.git"));
        assert!(st.head.is_some());
        let renamed = st.files.iter().find(|f| f.path == "b.txt").unwrap();
        assert_eq!(renamed.orig_path.as_deref(), Some("a.txt"));
        assert_eq!(renamed.code, "R ");
        assert!(renamed.staged);
        let untracked = st.files.iter().find(|f| f.path == "new dir/u.txt").unwrap();
        assert!(untracked.untracked);

        repo.git(&["commit", "-q", "-m", "rename"]);
        repo.git(&["checkout", "-q", "-b", "other"]);
        repo.write("conflict.txt", "other\n");
        repo.commit_all("other");
        repo.git(&["checkout", "-q", "main"]);
        repo.write("conflict.txt", "main\n");
        repo.git(&["commit", "-q", "-am", "main"]);
        let merge =
            std::process::Command::new("git").current_dir(&repo.path).args(["merge", "other"]).output().unwrap();
        assert!(!merge.status.success());
        let st = git.status().await.unwrap();
        let c = st.files.iter().find(|f| f.path == "conflict.txt").unwrap();
        assert!(c.conflicted);
        assert_eq!(c.code, "UU");
        assert_eq!(git.conflicted_files().await.unwrap(), vec!["conflict.txt".to_string()]);
    }

    #[tokio::test]
    async fn diff_targets_including_untracked_and_whitespace() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("src/lib.rs", "fn a() {}\nfn b() {}\n");
        repo.write("ws.txt", "x = 1\n");
        repo.commit_all("init");
        let first = git.head_sha().await.unwrap().unwrap();

        repo.write("src/lib.rs", "fn a() {}\nfn b() { 1 }\n");
        repo.git(&["add", "src/lib.rs"]);
        repo.write("ws.txt", "x  =  1\n");
        repo.write("notes/todo.md", "one\ntwo");

        let d = git.diff(&DiffTarget::Uncommitted, false, None).await.unwrap();
        let paths: Vec<&str> = d.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["notes/todo.md", "src/lib.rs", "ws.txt"]);
        let todo = &d.files[0];
        assert_eq!(todo.status, "untracked");
        assert_eq!(todo.additions, 2);
        assert_eq!(todo.hunks[0].lines.last().unwrap().kind, DiffLineKind::Meta);
        assert_eq!(d.additions, 4);

        let staged = git.diff(&DiffTarget::Staged, false, None).await.unwrap();
        assert_eq!(staged.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["src/lib.rs"]);

        let unstaged = git.diff(&DiffTarget::Unstaged, false, None).await.unwrap();
        assert_eq!(unstaged.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["notes/todo.md", "ws.txt"]);
        let no_ws = git.diff(&DiffTarget::Unstaged, true, None).await.unwrap();
        assert_eq!(no_ws.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["notes/todo.md"]);

        repo.git(&["commit", "-q", "-m", "second"]);
        let commit = git.diff(&DiffTarget::Commit { sha: "HEAD".into() }, false, Some(0)).await.unwrap();
        assert_eq!(commit.files.len(), 1);
        assert_eq!((commit.additions, commit.deletions), (1, 1));
        let root_commit = git.diff(&DiffTarget::Commit { sha: first.clone() }, false, None).await.unwrap();
        assert_eq!(root_commit.files.len(), 2);
        assert!(root_commit.files.iter().all(|f| f.status == "added"));

        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.write("feature.txt", "f\n");
        repo.git(&["add", "feature.txt"]);
        repo.git(&["commit", "-q", "-m", "feature"]);
        let base = git.diff(&DiffTarget::Base { branch: "main".into() }, false, None).await.unwrap();
        let paths: Vec<&str> = base.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["feature.txt", "notes/todo.md", "ws.txt"]);

        assert!(git.diff(&DiffTarget::LastTurn { thread_id: "t".into() }, false, None).await.is_err());

        let stats = git.diff_stats(&DiffTarget::Uncommitted).await.unwrap();
        assert_eq!(stats, DiffStats { files_changed: 2, additions: 3, deletions: 1 });
        let stats = git.diff_stats(&DiffTarget::Base { branch: "main".into() }).await.unwrap();
        assert_eq!(stats.files_changed, 3);
    }

    #[tokio::test]
    async fn unborn_repo_diff_stage_unstage() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("a.txt", "a\n");
        assert_eq!(git.diff(&DiffTarget::Uncommitted, false, None).await.unwrap().files.len(), 1);
        git.stage(&[]).await.unwrap();
        let staged = git.diff(&DiffTarget::Staged, false, None).await.unwrap();
        assert_eq!(staged.files[0].status, "added");
        assert_eq!(git.diff(&DiffTarget::Uncommitted, false, None).await.unwrap().files[0].status, "added");
        git.unstage(&["a.txt".into()]).await.unwrap();
        assert!(git.diff(&DiffTarget::Staged, false, None).await.unwrap().files.is_empty());
        assert!(git.log(5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn stage_unstage_revert_paths() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("a.txt", "a\n");
        repo.write("[id]/page.tsx", "p\n");
        repo.write(".gitignore", "*.log\n");
        repo.commit_all("init");
        repo.write("a.txt", "a2\n");
        repo.write("[id]/page.tsx", "p2\n");
        repo.write("u/new.txt", "n\n");
        repo.write("stray.txt", "s\n");
        repo.write("keep.log", "ignored\n");

        git.stage(&["[id]/page.tsx".into()]).await.unwrap();
        let st = git.status().await.unwrap();
        let page = st.files.iter().find(|f| f.path == "[id]/page.tsx").unwrap();
        assert!(page.staged && !page.unstaged);
        git.unstage(&["[id]/page.tsx".into()]).await.unwrap();
        let st = git.status().await.unwrap();
        assert!(!st.files.iter().find(|f| f.path == "[id]/page.tsx").unwrap().staged);

        git.stage(&["a.txt".into()]).await.unwrap();
        repo.write("a.txt", "a3\n");
        git.revert(&["a.txt".into(), "u".into()]).await.unwrap();
        assert_eq!(repo.read("a.txt"), "a2\n", "revert restores the index version");
        assert!(!repo.exists("u"));
        assert_eq!(repo.read("[id]/page.tsx"), "p2\n");

        git.revert(&[]).await.unwrap();
        assert_eq!(repo.read("[id]/page.tsx"), "p\n");
        assert!(!repo.exists("stray.txt"));
        assert!(repo.exists("keep.log"), "ignored files survive");
        assert_eq!(repo.read("a.txt"), "a2\n");
    }

    #[tokio::test]
    async fn hunk_stage_unstage_revert() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("f.txt", &lines(30, "line"));
        repo.commit_all("init");
        let modified = lines(30, "line").replace("line2\n", "line2 changed\n").replace("line28\n", "line28 changed\n");
        repo.write("f.txt", &modified);

        let unstaged = git.diff(&DiffTarget::Unstaged, false, None).await.unwrap();
        let file = &unstaged.files[0];
        assert_eq!(file.hunks.len(), 2);

        git.stage_hunk(file, 1).await.unwrap();
        let staged = git.diff(&DiffTarget::Staged, false, None).await.unwrap();
        assert_eq!(staged.files[0].hunks.len(), 1);
        assert!(staged.files[0].hunks[0].lines.iter().any(|l| l.text == "line28 changed"));
        let unstaged = git.diff(&DiffTarget::Unstaged, false, None).await.unwrap();
        assert_eq!(unstaged.files[0].hunks.len(), 1);
        assert!(unstaged.files[0].hunks[0].lines.iter().any(|l| l.text == "line2 changed"));

        git.unstage_hunk(&staged.files[0], 0).await.unwrap();
        assert!(git.diff(&DiffTarget::Staged, false, None).await.unwrap().files.is_empty());
        assert_eq!(repo.read("f.txt"), modified, "unstaging never touches the working tree");

        let unstaged = git.diff(&DiffTarget::Unstaged, false, None).await.unwrap();
        git.revert_hunk(&unstaged.files[0], 0).await.unwrap();
        let expected = lines(30, "line").replace("line28\n", "line28 changed\n");
        assert_eq!(repo.read("f.txt"), expected);

        // Staging the single hunk of an untracked file adds it to the index.
        repo.write("new.txt", "hello\n");
        let unstaged = git.diff(&DiffTarget::Unstaged, false, None).await.unwrap();
        let new = unstaged.files.iter().find(|f| f.path == "new.txt").unwrap();
        git.stage_hunk(new, 0).await.unwrap();
        let staged = git.diff(&DiffTarget::Staged, false, None).await.unwrap();
        assert_eq!(staged.files[0].path, "new.txt");
        assert_eq!(staged.files[0].status, "added");

        // Reverting it deletes an untracked file.
        repo.write("scratch.txt", "tmp\n");
        let unstaged = git.diff(&DiffTarget::Unstaged, false, None).await.unwrap();
        let scratch = unstaged.files.iter().find(|f| f.path == "scratch.txt").unwrap();
        assert!(git.revert_hunk(scratch, 3).await.is_err());
        git.revert_hunk(scratch, 0).await.unwrap();
        assert!(!repo.exists("scratch.txt"));
    }

    #[tokio::test]
    async fn commit_log_branches() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("a.txt", "a\n");
        assert!(git.commit("", true, false).await.is_err());
        let (sha, subject) = git.commit("First commit\n\nWith a body", true, false).await.unwrap();
        assert_eq!(subject, "First commit");
        assert_eq!(git.head_sha().await.unwrap().as_deref(), Some(sha.as_str()));
        repo.write("b.txt", "b\n");
        let (sha2, _) = git.commit("Amended", true, true).await.unwrap();
        assert_ne!(sha, sha2);
        let log = git.log(10).await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].subject, "Amended");
        assert_eq!(log[0].author, "Odex Test");
        assert!(log[0].timestamp > 1_600_000_000_000);
        assert!(log[0].short_sha.len() >= 7);

        repo.git(&["branch", "feature"]);
        let branches = git.branches().await.unwrap();
        assert!(branches.iter().any(|b| b.name == "main" && b.current && !b.remote));
        assert!(branches.iter().any(|b| b.name == "feature" && !b.current));
        assert_eq!(git.current_branch().await.unwrap().as_deref(), Some("main"));
        assert_eq!(git.default_branch().await.unwrap(), "main");
        repo.git(&["checkout", "-q", "--detach"]);
        assert_eq!(git.current_branch().await.unwrap(), None);
    }

    #[tokio::test]
    async fn push_to_local_bare_remote() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        let remote = repo.sibling("remote.git");
        crate::testutil::run(repo.path.parent().unwrap(), &["init", "-q", "--bare", remote.to_str().unwrap()]);
        repo.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
        repo.write("a.txt", "a\n");
        repo.commit_all("init");
        let out = git.push(None, None, true, false).await.unwrap();
        assert!(out.contains("refs/heads/main"), "{out}");
        let st = git.status().await.unwrap();
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        let branches = git.branches().await.unwrap();
        assert!(branches.iter().any(|b| b.remote && b.name == "origin/main"));
        assert!(branches.iter().any(|b| b.name == "main" && b.upstream.as_deref() == Some("origin/main")));
    }
}
