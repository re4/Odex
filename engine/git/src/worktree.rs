//! Worktrees, `.worktreeinclude` and handing work back from a worktree to the main checkout.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use odex_protocol::{HandoffResult, HandoffStrategy};

use crate::cmd::{native_path, nul_join};
use crate::error::{GitError, Result};
use crate::Git;

/// Ignored agent instructions copied into new worktrees even without `.worktreeinclude`.
const AGENTS_OVERRIDE: &str = "AGENTS.override.md";

fn check_name(kind: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('-') {
        Err(GitError::Invalid(format!("invalid {kind} {value:?}")))
    } else {
        Ok(())
    }
}

impl Git {
    /// `git worktree add -b <new_branch> <path> <base>`.
    pub async fn worktree_add(&self, path: &Path, new_branch: &str, base: &str) -> Result<()> {
        check_name("branch", new_branch)?;
        check_name("base", base)?;
        let root = self.repo_root().await?;
        self.cmd_at(&root).args(["worktree", "add", "-q", "-b", new_branch]).arg(path).arg(base).run().await?;
        Ok(())
    }

    /// Create a worktree on a new branch at the current `HEAD` and carry over every uncommitted
    /// change (staged, unstaged and untracked) as uncommitted changes in the new worktree.
    /// The source checkout is not modified.
    pub async fn worktree_add_with_changes(&self, path: &Path, new_branch: &str) -> Result<()> {
        let root = self.repo_root().await?;
        let head = self
            .rev_parse_opt(&root, "HEAD")
            .await?
            .ok_or_else(|| GitError::Invalid("cannot create a worktree: the repository has no commits yet".into()))?;
        let dirty = !self.status().await?.files.is_empty();
        let carry_ref = if dirty {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let name = format!("refs/odex/tmp/worktree-{nanos}");
            self.snapshot(&name, "odex: carry uncommitted changes into a worktree").await?;
            Some(name)
        } else {
            None
        };
        let result = async {
            self.worktree_add(path, new_branch, &head).await?;
            if let Some(name) = &carry_ref {
                Git::new(path).restore_snapshot(name).await?;
            }
            Ok(())
        }
        .await;
        if let Some(name) = &carry_ref {
            let _ = self.cmd_at(&root).args(["update-ref", "-d", name.as_str()]).output().await;
        }
        result
    }

    /// `git worktree remove [--force] <path>`.
    pub async fn worktree_remove(&self, path: &Path, force: bool) -> Result<()> {
        let root = self.repo_root().await?;
        let mut cmd = self.cmd_at(&root).args(["worktree", "remove"]);
        if force {
            cmd = cmd.args(["--force", "--force"]);
        }
        cmd.arg(path).run().await?;
        Ok(())
    }

    /// All worktrees of this repository (the main one first) with their checked-out branch.
    pub async fn worktree_list(&self) -> Result<Vec<(PathBuf, Option<String>)>> {
        let root = self.repo_root().await?;
        let out = self.cmd_at(&root).args(["worktree", "list", "--porcelain", "-z"]).run().await?;
        let mut list = Vec::new();
        let mut current: Option<(PathBuf, Option<String>)> = None;
        for token in out.stdout.split(|&b| b == 0) {
            let token = String::from_utf8_lossy(token);
            if token.is_empty() {
                if let Some(entry) = current.take() {
                    list.push(entry);
                }
                continue;
            }
            if let Some(p) = token.strip_prefix("worktree ") {
                if let Some(entry) = current.take() {
                    list.push(entry);
                }
                current = Some((native_path(p), None));
            } else if let Some(b) = token.strip_prefix("branch ") {
                if let Some(entry) = current.as_mut() {
                    entry.1 = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
                }
            }
        }
        if let Some(entry) = current.take() {
            list.push(entry);
        }
        Ok(list)
    }

    /// Copy ignored files selected by `repo_root/.worktreeinclude` (gitignore syntax) into
    /// `worktree`, plus an ignored root `AGENTS.override.md`. Only files git ignores are copied
    /// (tracked files are already in the worktree). Returns the copied repo-relative paths
    /// (directories end without a slash and are copied recursively).
    pub async fn copy_worktree_include(&self, repo_root: &Path, worktree: &Path) -> Result<Vec<PathBuf>> {
        let include = repo_root.join(".worktreeinclude");
        let mut selected: Vec<String> = Vec::new();
        if include.is_file() {
            // Untracked paths matching the include patterns (directories collapsed)...
            let out = self
                .cmd_at(repo_root)
                .args(["ls-files", "--others", "--ignored", "--directory", "--no-empty-directory", "-z"])
                .arg(format!("--exclude-from={}", include.display()))
                .run()
                .await?;
            let candidates = out.nul_tokens();
            // ...that the repository itself ignores.
            let ignored = self.check_ignored(repo_root, &candidates).await?;
            selected.extend(candidates.into_iter().filter(|c| ignored.contains(c)));
        }
        if repo_root.join(AGENTS_OVERRIDE).is_file()
            && !selected.iter().any(|s| s == AGENTS_OVERRIDE)
            && self.check_ignored(repo_root, &[AGENTS_OVERRIDE.to_string()]).await?.iter().any(|s| s == AGENTS_OVERRIDE)
        {
            selected.push(AGENTS_OVERRIDE.to_string());
        }
        let src_root = repo_root.to_path_buf();
        let dst_root = worktree.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut copied = Vec::new();
            for rel in selected {
                let rel_path = native_path(rel.trim_end_matches('/'));
                let src = src_root.join(&rel_path);
                let dst = dst_root.join(&rel_path);
                copy_recursive(&src, &dst)?;
                copied.push(rel_path);
            }
            Ok::<_, GitError>(copied)
        })
        .await
        .map_err(|e| GitError::Invalid(format!("copy task failed: {e}")))?
    }

    /// Which of `paths` git ignores (`git check-ignore`).
    async fn check_ignored(&self, root: &Path, paths: &[String]) -> Result<Vec<String>> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let out = self.cmd_at(root).args(["check-ignore", "-z", "--stdin"]).stdin(nul_join(paths)).output().await?;
        match out.code {
            Some(0) => Ok(out.nul_tokens()),
            Some(1) => Ok(Vec::new()),
            _ => Err(crate::cmd::command_error("git", "check-ignore -z --stdin".into(), root, &out)),
        }
    }

    /// Bring the work of `worktree` (whose branch is checked out there) into this checkout.
    ///
    /// If the worktree has uncommitted changes they are committed first with `commit_message`
    /// (an error if it is `None`). `target_branch` defaults to this checkout's current branch;
    /// for `Merge`/`Squash`/`CherryPick` it is checked out first when needed (this checkout must
    /// then be clean). Conflicts produce `ok: false` with the conflicted files, leaving the
    /// repository mid-operation so the user can resolve or abort.
    pub async fn handoff(
        &self,
        worktree: &Path,
        strategy: HandoffStrategy,
        target_branch: Option<&str>,
        commit_message: Option<&str>,
    ) -> Result<HandoffResult> {
        let root = self.repo_root().await?;
        let wt = Git::new(worktree);
        let wt_branch = wt
            .current_branch()
            .await?
            .ok_or_else(|| GitError::Invalid("the worktree is not on a branch (detached HEAD)".into()))?;

        if !wt.status().await?.files.is_empty() {
            match commit_message {
                Some(msg) if !msg.trim().is_empty() => {
                    wt.commit(msg, true, false).await?;
                }
                _ => {
                    return Err(GitError::Invalid(
                        "the worktree has uncommitted changes; provide a commit message to commit them first".into(),
                    ))
                }
            }
        }

        let current = self.current_branch().await?;
        let target = match target_branch {
            Some(t) => t.to_string(),
            None => current.clone().ok_or_else(|| {
                GitError::Invalid("this checkout is on a detached HEAD; choose a target branch".into())
            })?,
        };
        check_name("branch", &target)?;
        let local_dirty = self.status().await?.files.iter().any(|f| !f.untracked);
        let ok = |message: String| HandoffResult { ok: true, strategy, message, conflicts: Vec::new() };

        if strategy == HandoffStrategy::Checkout {
            if local_dirty {
                return Err(GitError::Invalid(
                    "this checkout has uncommitted changes; commit or stash them first".into(),
                ));
            }
            wt.cmd().args(["checkout", "-q", "--detach"]).run().await?;
            self.cmd_at(&root).args(["checkout", "-q", wt_branch.as_str()]).run().await?;
            return Ok(ok(format!(
                "Checked out {wt_branch} here; the worktree at {} is now detached.",
                worktree.display()
            )));
        }

        if wt_branch == target {
            return Err(GitError::Invalid(format!("the worktree branch and the target are both {target}")));
        }
        if current.as_deref() != Some(target.as_str()) {
            if local_dirty {
                return Err(GitError::Invalid(format!(
                    "this checkout has uncommitted changes; commit or stash them before switching to {target}"
                )));
            }
            self.cmd_at(&root).args(["checkout", "-q", target.as_str()]).run().await?;
        }

        let (cmd, abort_hint) = match strategy {
            HandoffStrategy::Merge => (
                self.cmd_at(&root).args(["merge", "--no-ff", "--no-edit", wt_branch.as_str()]),
                "resolve the conflicts and commit, or run `git merge --abort`",
            ),
            HandoffStrategy::Squash => (
                self.cmd_at(&root).args(["merge", "--squash", wt_branch.as_str()]),
                "resolve the conflicts and commit, or run `git reset --merge` to abort",
            ),
            HandoffStrategy::CherryPick => {
                let base =
                    self.cmd_at(&root).args(["merge-base", target.as_str(), wt_branch.as_str()]).run_line().await?;
                let range = format!("{base}..{wt_branch}");
                let commits = self
                    .cmd_at(&root)
                    .args(["rev-list", "--reverse", "--no-merges", range.as_str()])
                    .run()
                    .await?
                    .stdout_str()
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if commits.is_empty() {
                    return Ok(ok(format!("Nothing to cherry-pick: {wt_branch} has no new commits.")));
                }
                (
                    self.cmd_at(&root).args(["cherry-pick", "--allow-empty"]).args(&commits),
                    "resolve the conflicts and run `git cherry-pick --continue`, or `git cherry-pick --abort`",
                )
            }
            HandoffStrategy::Checkout => unreachable!("handled above"),
        };
        let out = cmd.output().await?;
        if out.success() {
            let message = match strategy {
                HandoffStrategy::Merge => format!("Merged {wt_branch} into {target}."),
                HandoffStrategy::Squash => {
                    format!("Squashed {wt_branch} onto {target}; the changes are staged and ready to commit.")
                }
                _ => format!("Cherry-picked the commits of {wt_branch} onto {target}."),
            };
            return Ok(ok(message));
        }
        let conflicts = self.conflicted_files().await?;
        if conflicts.is_empty() {
            return Err(crate::cmd::command_error("git", format!("{strategy:?} handoff"), &root, &out));
        }
        Ok(HandoffResult {
            ok: false,
            strategy,
            message: format!(
                "{} conflicted file(s) while bringing {wt_branch} into {target}: {abort_hint}.",
                conflicts.len()
            ),
            conflicts,
        })
    }
}

/// Copy a file, symlink or directory tree.
fn copy_recursive(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            let target = std::fs::read_link(src)?;
            let _ = std::fs::remove_file(dst);
            std::os::unix::fs::symlink(target, dst)?;
            return Ok(());
        }
        #[cfg(not(unix))]
        {
            // Creating symlinks needs privileges on Windows: copy file targets, skip directories.
            if src.is_file() {
                std::fs::copy(src, dst)?;
            }
            return Ok(());
        }
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;

    fn same_path(a: &Path, b: &Path) -> bool {
        std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
    }

    #[tokio::test]
    async fn worktree_add_list_remove() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("a.txt", "a\n");
        repo.commit_all("init");
        let wt_path = repo.sibling("wt-one");
        git.worktree_add(&wt_path, "odex/one", "main").await.unwrap();
        assert!(wt_path.join("a.txt").exists());
        let list = git.worktree_list().await.unwrap();
        assert_eq!(list.len(), 2);
        assert!(same_path(&list[0].0, &repo.path));
        assert_eq!(list[0].1.as_deref(), Some("main"));
        assert!(same_path(&list[1].0, &wt_path));
        assert_eq!(list[1].1.as_deref(), Some("odex/one"));

        std::fs::write(wt_path.join("dirty.txt"), "x").unwrap();
        assert!(git.worktree_remove(&wt_path, false).await.is_err());
        git.worktree_remove(&wt_path, true).await.unwrap();
        assert!(!wt_path.exists());
        assert_eq!(git.worktree_list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn worktree_with_uncommitted_changes() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("a.txt", "a\n");
        repo.write("gone.txt", "g\n");
        repo.commit_all("init");
        repo.write("a.txt", "changed\n");
        repo.write("new/untracked.txt", "u\n");
        std::fs::remove_file(repo.path.join("gone.txt")).unwrap();
        let before = repo.git(&["status", "--porcelain"]);

        let wt_path = repo.sibling("wt-carry");
        git.worktree_add_with_changes(&wt_path, "odex/carry").await.unwrap();
        assert_eq!(std::fs::read_to_string(wt_path.join("a.txt")).unwrap(), "changed\n");
        assert_eq!(std::fs::read_to_string(wt_path.join("new/untracked.txt")).unwrap(), "u\n");
        assert!(!wt_path.join("gone.txt").exists());
        let wt = Git::new(&wt_path);
        assert_eq!(wt.head_sha().await.unwrap(), git.head_sha().await.unwrap(), "branch starts at HEAD");
        assert_eq!(repo.git(&["status", "--porcelain"]), before, "source checkout untouched");
        assert!(git.list_refs("refs/odex/tmp").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn copies_worktree_include_files() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write(".gitignore", ".env\nnode_modules/\n*.log\nAGENTS.override.md\nsecret.txt\n");
        repo.write(".worktreeinclude", ".env\nnode_modules/\nconfig/local.json\n");
        repo.write("config/local.json", "{}\n");
        repo.commit_all("init");
        repo.write(".env", "TOKEN=1\n");
        repo.write("node_modules/pkg/index.js", "module.exports = 1;\n");
        repo.write("debug.log", "not included\n");
        repo.write("secret.txt", "not included\n");
        repo.write("AGENTS.override.md", "local rules\n");
        repo.write("config/local.json", "{\"tracked\": true}\n");

        let wt_path = repo.sibling("wt-include");
        git.worktree_add(&wt_path, "odex/include", "main").await.unwrap();
        let mut copied = git.copy_worktree_include(&repo.path, &wt_path).await.unwrap();
        copied.sort();
        let copied: Vec<String> = copied.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(copied, [".env", "AGENTS.override.md", "node_modules"]);
        assert_eq!(std::fs::read_to_string(wt_path.join(".env")).unwrap(), "TOKEN=1\n");
        assert!(wt_path.join("node_modules/pkg/index.js").exists());
        assert!(!wt_path.join("debug.log").exists());
        assert!(!wt_path.join("secret.txt").exists());
        // Tracked files come from the branch, not from the source working tree.
        assert_eq!(std::fs::read_to_string(wt_path.join("config/local.json")).unwrap(), "{}\n");
    }

    async fn setup_handoff(repo: &TestRepo, name: &str) -> (Git, PathBuf) {
        let git = Git::new(&repo.path);
        repo.write("shared.txt", "line1\nline2\nline3\n");
        repo.commit_all("init");
        let wt_path = repo.sibling(name);
        git.worktree_add(&wt_path, &format!("odex/{name}"), "main").await.unwrap();
        repo.configure(&wt_path);
        (git, wt_path)
    }

    #[tokio::test]
    async fn handoff_merge_commits_dirty_worktree() {
        let repo = TestRepo::new();
        let (git, wt_path) = setup_handoff(&repo, "merge").await;
        std::fs::write(wt_path.join("feature.txt"), "feature\n").unwrap();
        assert!(git.handoff(&wt_path, HandoffStrategy::Merge, None, None).await.is_err());
        let res = git.handoff(&wt_path, HandoffStrategy::Merge, None, Some("Add feature")).await.unwrap();
        assert!(res.ok, "{}", res.message);
        assert!(repo.exists("feature.txt"));
        let parents = repo.git(&["rev-list", "--parents", "-n1", "HEAD"]);
        assert_eq!(parents.split_whitespace().count(), 3, "--no-ff creates a merge commit");
    }

    #[tokio::test]
    async fn handoff_merge_conflict_reports_files() {
        let repo = TestRepo::new();
        let (git, wt_path) = setup_handoff(&repo, "conflict").await;
        std::fs::write(wt_path.join("shared.txt"), "line1\nworktree\nline3\n").unwrap();
        let wt = Git::new(&wt_path);
        wt.commit("worktree edit", true, false).await.unwrap();
        repo.write("shared.txt", "line1\nlocal\nline3\n");
        repo.commit_all("local edit");
        let res = git.handoff(&wt_path, HandoffStrategy::Merge, Some("main"), None).await.unwrap();
        assert!(!res.ok);
        assert_eq!(res.conflicts, ["shared.txt"]);
        assert!(res.message.contains("git merge --abort"));
        assert!(repo.path.join(".git/MERGE_HEAD").exists(), "repository left mid-merge");
    }

    #[tokio::test]
    async fn handoff_squash_cherry_pick_and_checkout() {
        let repo = TestRepo::new();
        let (git, wt_path) = setup_handoff(&repo, "multi").await;
        let wt = Git::new(&wt_path);
        std::fs::write(wt_path.join("one.txt"), "1\n").unwrap();
        wt.commit("one", true, false).await.unwrap();
        std::fs::write(wt_path.join("two.txt"), "2\n").unwrap();
        wt.commit("two", true, false).await.unwrap();

        let res = git.handoff(&wt_path, HandoffStrategy::Squash, None, None).await.unwrap();
        assert!(res.ok);
        let staged = repo.git(&["diff", "--cached", "--name-only"]);
        assert_eq!(staged.lines().collect::<Vec<_>>(), ["one.txt", "two.txt"]);
        repo.git(&["reset", "-q", "--hard"]);

        let res = git.handoff(&wt_path, HandoffStrategy::CherryPick, None, None).await.unwrap();
        assert!(res.ok, "{}", res.message);
        let subjects = repo.git(&["log", "--format=%s", "-n2"]);
        assert_eq!(subjects.lines().collect::<Vec<_>>(), ["two", "one"]);

        let res = git.handoff(&wt_path, HandoffStrategy::Checkout, None, None).await.unwrap();
        assert!(res.ok);
        assert_eq!(git.current_branch().await.unwrap().as_deref(), Some("odex/multi"));
        assert_eq!(wt.current_branch().await.unwrap(), None);
    }
}
