//! Projects, git, review, PRs, file search, exec sessions.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAction {
    pub id: String,
    pub name: String,
    pub command: String,
    /// Relative to the primary folder (or absolute).
    pub cwd: Option<String>,
    /// Icon hint: `play`, `test`, `lint`, `build`, `server`.
    pub icon: Option<String>,
    /// URL to open in the in-app browser once running (dev servers).
    pub open_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub id: String,
    pub name: String,
    /// Script run when a worktree is created (PowerShell on Windows, sh elsewhere).
    pub setup_script: Option<String>,
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub folders: Vec<String>,
    /// Index into `folders`.
    pub primary: u32,
    pub trusted: bool,
    pub actions: Vec<ProjectAction>,
    pub environments: Vec<Environment>,
    pub default_environment: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub last_opened_at: i64,
    pub collapsed: bool,
    pub is_git: bool,
}

impl Project {
    pub fn primary_folder(&self) -> &str {
        self.folders
            .get(self.primary as usize)
            .or_else(|| self.folders.first())
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitFileStatus {
    pub path: String,
    pub orig_path: Option<String>,
    /// Two-letter porcelain code, e.g. ` M`, `A `, `??`.
    pub code: String,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub conflicted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub is_repo: bool,
    pub repo_root: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<GitFileStatus>,
    pub stash_count: u32,
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DiffTarget {
    /// Working tree + index vs HEAD (includes untracked files).
    Uncommitted,
    /// Index vs HEAD.
    Staged,
    /// Working tree vs index.
    Unstaged,
    /// Current branch vs merge-base with `branch`.
    Base { branch: String },
    /// A single commit.
    Commit { sha: String },
    /// Changes made by a thread's most recent turn (vs its undo snapshot).
    #[serde(rename_all = "camelCase")]
    LastTurn { thread_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum DiffLineKind {
    Context,
    Add,
    Del,
    /// "\ No newline at end of file"
    Meta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
    /// Stable id within the file: index in the hunk list.
    pub index: u32,
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub path: String,
    pub old_path: Option<String>,
    /// `added`, `deleted`, `modified`, `renamed`, `untracked`, `binary`.
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
    /// Header lines (`diff --git`, `index`, `---`, `+++`) needed to rebuild a patch.
    pub header: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoDiff {
    pub repo_root: String,
    pub target: DiffTarget,
    pub files: Vec<DiffFile>,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitInfo {
    pub sha: String,
    pub short_sha: String,
    pub subject: String,
    pub author: String,
    #[ts(type = "number")]
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GitBranch {
    pub name: String,
    pub current: bool,
    pub remote: bool,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum HandoffStrategy {
    Merge,
    CherryPick,
    Checkout,
    Squash,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HandoffResult {
    pub ok: bool,
    pub strategy: HandoffStrategy,
    pub message: String,
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrCheck {
    pub name: String,
    /// `pending`, `success`, `failure`, `neutral`, `skipped`.
    pub state: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrTimelineEvent {
    /// `commented`, `reviewed`, `committed`, `merged`, `closed`, `opened`, ...
    pub kind: String,
    pub author: Option<String>,
    pub body: Option<String>,
    #[ts(type = "number")]
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PrReviewComment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub path: String,
    pub line: Option<u32>,
    pub side: Option<String>,
    #[ts(type = "number")]
    pub at: i64,
    pub in_reply_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u32,
    pub title: String,
    pub body: String,
    /// `open`, `closed`, `merged`, `draft`.
    pub state: String,
    pub url: String,
    pub author: String,
    pub head: String,
    pub base: String,
    pub additions: u32,
    pub deletions: u32,
    pub checks: Vec<PrCheck>,
    pub timeline: Vec<PrTimelineEvent>,
    pub review_comments: Vec<PrReviewComment>,
    pub files: Vec<DiffFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileMatch {
    pub path: String,
    pub root: String,
    pub score: u32,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ExecSessionInfo {
    pub id: String,
    pub thread_id: Option<String>,
    pub command: String,
    pub cwd: String,
    pub pid: Option<u32>,
    pub running: bool,
    pub exit_code: Option<i32>,
    #[ts(type = "number")]
    pub started_at: i64,
}
