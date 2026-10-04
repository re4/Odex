//! Undo snapshots: hidden refs capturing the full working tree, and temporary-index helpers.

use std::path::{Path, PathBuf};

use crate::cmd::{native_path, nul_join};
use crate::error::{GitError, Result};
use crate::Git;

/// Identity recorded on snapshot commits (they live under hidden refs, never on branches).
const SNAPSHOT_IDENT: (&str, &str) = ("Odex", "odex@localhost");

/// A private index file that is deleted on drop.
pub(crate) struct TempIndex {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
}

fn validate_ref(name: &str) -> Result<()> {
    let ok = name.starts_with("refs/")
        && !name.ends_with('/')
        && !name.contains("..")
        && !name.chars().any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
    if ok {
        Ok(())
    } else {
        Err(GitError::Invalid(format!("invalid ref name {name:?}")))
    }
}

impl Git {
    /// Create an empty temporary index, or one seeded with a copy of the real index so that
    /// `git add -A` only re-hashes files whose stat data changed.
    pub(crate) async fn temp_index(&self, root: &Path, seed: bool) -> Result<TempIndex> {
        let dir = tempfile::Builder::new().prefix("odex-index-").tempdir()?;
        let path = dir.path().join("index");
        if seed {
            let real = self.cmd_at(root).args(["rev-parse", "--git-path", "index"]).run_line().await?;
            let real = native_path(&real);
            let real = if real.is_absolute() { real } else { root.join(real) };
            if real.is_file() {
                tokio::fs::copy(&real, &path).await?;
            }
        }
        Ok(TempIndex { _dir: dir, path })
    }

    /// Tree id of the current working tree: tracked and untracked files, minus ignored ones.
    /// Uses a temporary index, so the real index is never modified.
    pub(crate) async fn worktree_tree(&self, root: &Path) -> Result<String> {
        let index = self.temp_index(root, true).await?;
        self.cmd_at(root)
            .args(["-c", "core.safecrlf=false", "add", "-A", "--ignore-errors"])
            .env("GIT_INDEX_FILE", &index.path)
            .run()
            .await?;
        self.cmd_at(root).arg("write-tree").env("GIT_INDEX_FILE", &index.path).run_line().await
    }

    /// Record the full working tree (tracked + untracked, not ignored) as a commit under
    /// `ref_name` (e.g. `refs/odex/snapshots/<thread>/<turn>`) whose parent is `HEAD`.
    /// HEAD, the index and the stash are untouched. Returns the snapshot commit id.
    pub async fn snapshot(&self, ref_name: &str, message: &str) -> Result<String> {
        validate_ref(ref_name)?;
        let root = self.repo_root().await?;
        let tree = self.worktree_tree(&root).await?;
        let head = self.rev_parse_opt(&root, "HEAD").await?;
        let mut cmd = self.cmd_at(&root).args(["commit-tree", "--no-gpg-sign", tree.as_str()]);
        if let Some(h) = &head {
            cmd = cmd.args(["-p", h.as_str()]);
        }
        let message = if message.trim().is_empty() { "odex snapshot" } else { message };
        let sha = cmd
            .args(["-F", "-"])
            .stdin(message.as_bytes().to_vec())
            .env("GIT_AUTHOR_NAME", SNAPSHOT_IDENT.0)
            .env("GIT_AUTHOR_EMAIL", SNAPSHOT_IDENT.1)
            .env("GIT_COMMITTER_NAME", SNAPSHOT_IDENT.0)
            .env("GIT_COMMITTER_EMAIL", SNAPSHOT_IDENT.1)
            .run_line()
            .await?;
        self.cmd_at(&root).args(["update-ref", "-m", "odex snapshot", ref_name, sha.as_str()]).run().await?;
        Ok(sha)
    }

    /// Make the working tree match the snapshot at `ref_name`: changed files are rewritten from
    /// the snapshot, files that exist now (tracked or untracked, not ignored) but not in the
    /// snapshot are deleted. Only differing paths are touched; HEAD and the index are left alone
    /// (the index's stat cache is refreshed).
    pub async fn restore_snapshot(&self, ref_name: &str) -> Result<()> {
        validate_ref(ref_name)?;
        let root = self.repo_root().await?;
        let snap_tree = self
            .rev_parse_opt(&root, &format!("{ref_name}^{{tree}}"))
            .await?
            .ok_or_else(|| GitError::Invalid(format!("snapshot {ref_name} not found")))?;
        let current = self.worktree_tree(&root).await?;
        if current == snap_tree {
            return Ok(());
        }
        let out = self
            .cmd_at(&root)
            .args(["diff-tree", "-r", "-z", "--no-renames", "--name-status", current.as_str(), snap_tree.as_str()])
            .run()
            .await?;
        let tokens = out.nul_tokens();
        let mut delete = Vec::new();
        let mut write = Vec::new();
        for pair in tokens.chunks(2) {
            if let [status, path] = pair {
                match status.chars().next() {
                    Some('D') => delete.push(path.clone()),
                    Some(_) => write.push(path.clone()),
                    None => {}
                }
            }
        }

        let root2 = root.clone();
        tokio::task::spawn_blocking(move || remove_paths(&root2, &delete))
            .await
            .map_err(|e| GitError::Invalid(format!("restore task failed: {e}")))??;

        if !write.is_empty() {
            let index = self.temp_index(&root, false).await?;
            self.cmd_at(&root).args(["read-tree", snap_tree.as_str()]).env("GIT_INDEX_FILE", &index.path).run().await?;
            self.cmd_at(&root)
                .args(["checkout-index", "-f", "-q", "-z", "--stdin"])
                .env("GIT_INDEX_FILE", &index.path)
                .stdin(nul_join(&write))
                .run()
                .await?;
        }
        // Best effort: keep `git status` fast. Content of the index is unchanged.
        let _ = self.cmd_at(&root).args(["update-index", "-q", "--refresh"]).output().await;
        Ok(())
    }

    /// Full names of refs under `prefix` (matched per path component, like `git for-each-ref`).
    pub async fn list_refs(&self, prefix: &str) -> Result<Vec<String>> {
        let root = self.repo_root().await?;
        let prefix = prefix.trim_end_matches('/');
        if !prefix.starts_with("refs/") {
            return Err(GitError::Invalid(format!("ref prefix must start with refs/: {prefix:?}")));
        }
        let out = self.cmd_at(&root).args(["for-each-ref", "--format=%(refname)", prefix]).run().await?;
        Ok(out.stdout_str().lines().map(str::to_string).filter(|s| !s.is_empty()).collect())
    }

    /// Delete every ref under `prefix`. Refuses broad prefixes such as `refs/heads`.
    pub async fn delete_refs(&self, prefix: &str) -> Result<()> {
        let trimmed = prefix.trim_end_matches('/');
        if matches!(trimmed, "refs" | "refs/heads" | "refs/tags" | "refs/remotes" | "refs/notes" | "refs/stash") {
            return Err(GitError::Invalid(format!("refusing to delete everything under {prefix:?}")));
        }
        let refs = self.list_refs(trimmed).await?;
        if refs.is_empty() {
            return Ok(());
        }
        let root = self.repo_root().await?;
        let script: String = refs.iter().map(|r| format!("delete {r}\n")).collect();
        self.cmd_at(&root).args(["update-ref", "--stdin"]).stdin(script.into_bytes()).run().await?;
        Ok(())
    }
}

/// Delete repo-relative `paths` and prune directories left empty (never `root` itself).
fn remove_paths(root: &Path, paths: &[String]) -> std::io::Result<()> {
    for rel in paths {
        let full = root.join(native_path(rel));
        match std::fs::symlink_metadata(&full) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => continue,
            Ok(_) => {
                if let Err(e) = std::fs::remove_file(&full) {
                    // Read-only files (common on Windows) need their attribute cleared first.
                    if e.kind() == std::io::ErrorKind::PermissionDenied {
                        let mut perms = std::fs::metadata(&full)?.permissions();
                        #[allow(clippy::permissions_set_readonly_false)]
                        perms.set_readonly(false);
                        std::fs::set_permissions(&full, perms)?;
                        std::fs::remove_file(&full)?;
                    } else if e.kind() != std::io::ErrorKind::NotFound {
                        return Err(e);
                    }
                }
            }
            Err(_) => continue,
        }
        let mut dir = full.parent();
        while let Some(d) = dir {
            if d == root || !d.starts_with(root) {
                break;
            }
            if std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestRepo;
    use odex_protocol::DiffTarget;
    use std::collections::BTreeMap;

    /// Every non-.git file under the repo with its content.
    fn tree_state(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let p = e.path();
                if e.file_name() == ".git" {
                    continue;
                }
                if p.is_dir() {
                    walk(base, &p, out);
                } else {
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    #[tokio::test]
    async fn snapshot_restore_roundtrip() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("keep.txt", "keep\n");
        repo.write("mod.txt", "original\n");
        repo.write("del.txt", "to delete\n");
        repo.write("dir/nested.txt", "nested\n");
        repo.write(".gitignore", "*.log\n");
        repo.commit_all("init");
        // Pre-turn state: a staged change, an unstaged change, an untracked file, an ignored file.
        repo.write("mod.txt", "staged version\n");
        repo.git(&["add", "mod.txt"]);
        repo.write("mod.txt", "working version\n");
        repo.write("untracked/u.txt", "untracked\n");
        repo.write("debug.log", "ignored\n");

        let head_before = repo.git(&["rev-parse", "HEAD"]);
        let index_before = repo.git(&["ls-files", "-s"]);
        let stash_before = repo.git(&["stash", "list"]);
        let state_before = tree_state(&repo.path);

        let sha = git.snapshot("refs/odex/snapshots/t1/1", "turn 1").await.unwrap();
        assert_eq!(repo.git(&["rev-parse", "refs/odex/snapshots/t1/1"]).trim(), sha);
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head_before);
        assert_eq!(repo.git(&["ls-files", "-s"]), index_before);
        // The snapshot's parent is HEAD and it contains the untracked file but not the ignored one.
        assert_eq!(repo.git(&["rev-parse", &format!("{sha}^")]), head_before);
        let files = repo.git(&["ls-tree", "-r", "--name-only", &sha]);
        assert!(files.contains("untracked/u.txt"));
        assert!(!files.contains("debug.log"));

        // The "turn": modify, add, delete, replace a dir with a file, touch ignored files.
        repo.write("mod.txt", "agent edit\n");
        repo.write("added/new.txt", "new\n");
        repo.write("keep.txt", "keep\n");
        std::fs::remove_file(repo.path.join("del.txt")).unwrap();
        std::fs::remove_dir_all(repo.path.join("dir")).unwrap();
        repo.write("dir", "now a file\n");
        std::fs::remove_dir_all(repo.path.join("untracked")).unwrap();
        repo.write("debug.log", "changed ignored\n");

        let d = git.diff_from_ref(&sha, false).await.unwrap();
        let changed: Vec<&str> = d.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(changed, ["added/new.txt", "del.txt", "dir", "dir/nested.txt", "mod.txt", "untracked/u.txt"]);
        let new = d.files.iter().find(|f| f.path == "added/new.txt").unwrap();
        assert_eq!(new.status, "added");

        git.restore_snapshot("refs/odex/snapshots/t1/1").await.unwrap();
        let mut expected = state_before.clone();
        expected.insert("debug.log".into(), b"changed ignored\n".to_vec());
        assert_eq!(tree_state(&repo.path), expected);
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), head_before);
        assert_eq!(repo.git(&["ls-files", "-s"]), index_before);
        assert_eq!(repo.git(&["stash", "list"]), stash_before);
        // Staged vs unstaged split is preserved because the index was never touched.
        let staged = git.diff(&DiffTarget::Staged, false, None).await.unwrap();
        assert!(staged.files[0].hunks[0].lines.iter().any(|l| l.text == "staged version"));

        // Restoring again is a no-op.
        git.restore_snapshot("refs/odex/snapshots/t1/1").await.unwrap();
        assert_eq!(tree_state(&repo.path), expected);
    }

    #[tokio::test]
    async fn snapshot_on_unborn_branch_and_ref_management() {
        let repo = TestRepo::new();
        let git = Git::new(&repo.path);
        repo.write("a.txt", "a\n");
        let sha = git.snapshot("refs/odex/snapshots/t2/1", "first").await.unwrap();
        assert!(repo.git(&["cat-file", "-p", &sha]).lines().all(|l| !l.starts_with("parent ")));
        repo.write("b.txt", "b\n");
        git.snapshot("refs/odex/snapshots/t2/2", "second").await.unwrap();
        git.snapshot("refs/odex/snapshots/t3/1", "other thread").await.unwrap();
        git.restore_snapshot("refs/odex/snapshots/t2/1").await.unwrap();
        assert!(!repo.exists("b.txt"));
        assert!(repo.exists("a.txt"));

        assert_eq!(
            git.list_refs("refs/odex/snapshots/t2").await.unwrap(),
            ["refs/odex/snapshots/t2/1", "refs/odex/snapshots/t2/2"]
        );
        git.delete_refs("refs/odex/snapshots/t2/").await.unwrap();
        assert!(git.list_refs("refs/odex/snapshots/t2").await.unwrap().is_empty());
        assert_eq!(git.list_refs("refs/odex").await.unwrap(), ["refs/odex/snapshots/t3/1"]);
        assert!(git.delete_refs("refs/heads").await.is_err());
        assert!(git.snapshot("not-a-ref", "x").await.is_err());
        assert!(git.restore_snapshot("refs/odex/missing").await.is_err());
    }
}
