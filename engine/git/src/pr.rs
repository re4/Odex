//! GitHub pull requests: through the `gh` CLI when it is installed, otherwise the REST API with
//! a token (the desktop's stored `github:token` secret, else `GITHUB_TOKEN` / `GH_TOKEN`). The
//! repository is derived from the `origin` remote (https, ssh and scp-like forms; GitHub
//! Enterprise hosts use `/api/v3`).
//!
//! `ODEX_GITHUB_API` overrides the REST base URL and always selects the REST path, so tests (and
//! proxies) can point the engine at another server.

use std::path::Path;
use std::time::Duration;

use odex_protocol::{PrCheck, PrListItem, PrReviewComment, PrTimelineEvent, PullRequest, ReviewComment};
use serde_json::{json, Value};

use crate::cmd::GitCommand;
use crate::diff::parse_unified_diff;
use crate::error::{GitError, Result};
use crate::Git;

const USER_AGENT: &str = concat!("odex-git/", env!("CARGO_PKG_VERSION"));

/// Characters of check log returned by [`check_log`].
pub const CHECK_LOG_CAP: usize = 12_000;

/// How to reach GitHub. Never print it: it holds the token.
#[derive(Clone, Default)]
pub struct GithubConfig {
    /// API token (explicit secret first, then the environment).
    pub token: Option<String>,
    /// REST base URL override; when set, `gh` is not used.
    pub api_base: Option<String>,
}

impl std::fmt::Debug for GithubConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubConfig")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl GithubConfig {
    /// `explicit_token` (the desktop secret) wins over `GITHUB_TOKEN` / `GH_TOKEN`;
    /// `ODEX_GITHUB_API` sets the REST base.
    pub fn resolve(explicit_token: Option<&str>) -> Self {
        let api_base = std::env::var("ODEX_GITHUB_API")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());
        GithubConfig { token: resolve_token(explicit_token), api_base }
    }

    /// REST API only, against `base` (tests).
    pub fn rest(base: &str, token: Option<&str>) -> Self {
        GithubConfig { token: token.map(str::to_string), api_base: Some(base.trim_end_matches('/').to_string()) }
    }

    async fn use_gh(&self) -> bool {
        self.api_base.is_none() && gh_available().await
    }

    /// A `gh` command that authenticates with our token when we have one.
    fn gh(&self, cwd: &Path) -> GitCommand {
        let cmd = GitCommand::gh(cwd);
        match &self.token {
            Some(t) => cmd.env("GH_TOKEN", t),
            None => cmd,
        }
    }

    fn api(&self, repo: GithubRepo) -> Result<Api> {
        let base = self.api_base.clone().unwrap_or_else(|| repo.api_base());
        Api::new(repo, base, self.token.clone())
    }
}

/// A GitHub repository identified from a remote URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRepo {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

impl GithubRepo {
    /// REST API base URL for this host.
    pub fn api_base(&self) -> String {
        if self.host.eq_ignore_ascii_case("github.com") {
            "https://api.github.com".to_string()
        } else {
            format!("https://{}/api/v3", self.host)
        }
    }

    /// `owner/repo`.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Parse `owner/repo` (and host) from a remote URL:
/// `https://github.com/o/r(.git)`, `https://user@host/o/r`, `git@github.com:o/r.git`,
/// `ssh://git@github.com[:22]/o/r.git`, `git://github.com/o/r`.
pub fn parse_remote_url(url: &str) -> Option<GithubRepo> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git") {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        let host = host.split(':').next()?;
        (host.to_string(), path.to_string())
    } else {
        // scp-like: [user@]host:owner/repo
        let (left, path) = url.split_once(':')?;
        if left.contains('/') || left.len() == 1 {
            // A local path (or a Windows drive letter), not a remote.
            return None;
        }
        let host = left.rsplit('@').next()?;
        (host.to_string(), path.trim_start_matches('/').to_string())
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/').filter(|s| !s.is_empty());
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if parts.next().is_some() || host.is_empty() {
        return None;
    }
    Some(GithubRepo { host: host.to_lowercase(), owner, repo })
}

/// The explicit token, else `GITHUB_TOKEN`, else `GH_TOKEN`.
pub fn resolve_token(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_string)
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .filter(|t| !t.trim().is_empty())
}

/// Whether the `gh` CLI is installed and runnable.
pub async fn gh_available() -> bool {
    let dir = std::env::temp_dir();
    matches!(GitCommand::gh(&dir).arg("--version").output().await, Ok(out) if out.success())
}

async fn origin_repo(cwd: &Path) -> Result<GithubRepo> {
    let git = Git::new(cwd);
    let root = git.repo_root().await?;
    let url = git.cmd_at(&root).args(["remote", "get-url", "origin"]).run_line().await?;
    parse_remote_url(&url).ok_or_else(|| GitError::Invalid(format!("origin is not a GitHub remote: {url}")))
}

/// Unix milliseconds from an RFC 3339 timestamp (0 when missing/invalid).
fn millis(v: &Value) -> i64 {
    v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis()).unwrap_or(0)
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn opt_s(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn number_from_url(url: &str) -> Option<u32> {
    let idx = url.rfind("/pull/")?;
    url[idx + 6..].split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// The Actions job / check-run id in a check URL:
/// `…/actions/runs/<run>/job/<id>` or `…/runs/<id>` (a check-run page).
pub fn job_id_from_url(url: &str) -> Option<String> {
    let digits = |rest: &str| -> Option<String> {
        let d: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        (!d.is_empty()).then_some(d)
    };
    if let Some(i) = url.rfind("/job/") {
        return digits(&url[i + 5..]);
    }
    let i = url.rfind("/runs/")?;
    if url[..i].ends_with("/actions") {
        return None; // a workflow run, not a single job
    }
    digits(&url[i + 6..])
}

/// `comment` / `approve` / `requestChanges` (any case, `_` or `-` separated) → the REST event.
pub fn normalize_event(event: &str) -> Option<&'static str> {
    let e: String = event.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
    match e.as_str() {
        "" | "COMMENT" => Some("COMMENT"),
        "APPROVE" => Some("APPROVE"),
        "REQUESTCHANGES" => Some("REQUEST_CHANGES"),
        _ => None,
    }
}

/// Combined check state (`failure` > `pending` > `success`; none without checks) and the
/// number of failed checks.
pub fn summarize_checks(checks: &[PrCheck]) -> (Option<String>, u32) {
    let failed = checks.iter().filter(|c| c.state == "failure").count() as u32;
    let state = if failed > 0 {
        Some("failure")
    } else if checks.iter().any(|c| c.state == "pending") {
        Some("pending")
    } else if checks.iter().any(|c| c.state == "success") {
        Some("success")
    } else {
        None
    };
    (state.map(str::to_string), failed)
}

/// Drop the `2024-05-01T10:00:00.1234567Z ` prefix GitHub Actions puts on log lines.
fn strip_timestamp(line: &str) -> &str {
    let b = line.as_bytes();
    if b.len() > 20 && b[..4].iter().all(u8::is_ascii_digit) && b[4] == b'-' && b[10] == b'T' {
        if let Some(i) = b[..b.len().min(40)].windows(2).position(|w| w == b"Z ") {
            return line.get(i + 2..).unwrap_or(line);
        }
    }
    line
}

/// Keep what helps fix a failure within `cap` characters: `##[error]` lines from anywhere, then
/// the tail of the log (failures are reported last). Returns the text and whether it was cut.
pub fn tail_log(raw: &str, cap: usize) -> (String, bool) {
    let lines: Vec<&str> = raw.lines().map(strip_timestamp).filter(|l| !l.trim().is_empty()).collect();
    let total: usize = lines.iter().map(|l| l.len() + 1).sum();
    if total <= cap {
        return (lines.join("\n"), false);
    }
    let mut tail: Vec<&str> = Vec::new();
    let mut used = 0;
    let budget = cap * 3 / 4;
    for l in lines.iter().rev() {
        if used + l.len() + 1 > budget {
            break;
        }
        used += l.len() + 1;
        tail.push(l);
    }
    tail.reverse();
    let cut = lines.len() - tail.len();
    let mut errors: Vec<&str> = Vec::new();
    let mut err_used = 0;
    for l in &lines[..cut] {
        if l.contains("##[error]") && !errors.contains(l) && err_used + l.len() < cap - budget {
            err_used += l.len() + 1;
            errors.push(l);
        }
    }
    let mut out = String::new();
    if !errors.is_empty() {
        out.push_str("Errors reported earlier in the log:\n");
        out.push_str(&errors.join("\n"));
        out.push_str("\n\n");
    }
    out.push_str(&format!("… ({cut} earlier lines omitted)\n"));
    out.push_str(&tail.join("\n"));
    (out, true)
}

/// Map a check-run status/conclusion or a commit status state to
/// `pending | success | failure | neutral | skipped`.
pub fn check_state(status: Option<&str>, conclusion: Option<&str>) -> &'static str {
    let status = status.map(str::to_ascii_lowercase);
    if let Some(st) = status.as_deref() {
        if matches!(st, "queued" | "in_progress" | "waiting" | "requested" | "pending" | "expected") {
            return "pending";
        }
        if matches!(st, "success" | "failure" | "error") && conclusion.is_none() {
            // Commit status API: the state is the outcome.
            return if st == "success" { "success" } else { "failure" };
        }
    }
    match conclusion.map(str::to_ascii_lowercase).as_deref() {
        Some("success") => "success",
        Some("failure" | "timed_out" | "cancelled" | "action_required" | "startup_failure" | "error") => "failure",
        Some("skipped") => "skipped",
        Some("neutral" | "stale") => "neutral",
        Some(_) => "neutral",
        None => "pending",
    }
}

/// The JSON body for `POST /repos/{o}/{r}/pulls/{n}/reviews`.
///
/// Line comments become review comments (`side` `old` → `LEFT`, otherwise `RIGHT`; an
/// `end_line` turns into a multi-line range). Comments without a line are folded into the
/// review body, since the reviews API only accepts line comments.
pub fn review_payload(comments: &[ReviewComment], body: Option<&str>, event: &str) -> Result<Value> {
    let event = normalize_event(event).ok_or_else(|| GitError::Invalid(format!("invalid review event {event:?}")))?;
    let mut text = body.unwrap_or_default().trim().to_string();
    let mut inline = Vec::new();
    for c in comments {
        let side = if c.side.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("old") || s.eq_ignore_ascii_case("left"))
        {
            "LEFT"
        } else {
            "RIGHT"
        };
        match c.line {
            Some(line) => {
                let mut obj = json!({ "path": c.path, "body": c.body, "side": side });
                match c.end_line {
                    Some(end) if end > line => {
                        obj["start_line"] = json!(line);
                        obj["start_side"] = json!(side);
                        obj["line"] = json!(end);
                    }
                    _ => obj["line"] = json!(line),
                }
                inline.push(obj);
            }
            None => {
                if !text.is_empty() {
                    text.push_str("\n\n");
                }
                text.push_str(&format!("**{}**: {}", c.path, c.body));
            }
        }
    }
    let mut payload = json!({ "event": event, "comments": inline });
    if !text.is_empty() {
        payload["body"] = json!(text);
    }
    Ok(payload)
}

/// Build a [`PullRequest`] from REST API responses.
#[allow(clippy::too_many_arguments)]
pub fn pr_from_rest(
    pr: &Value,
    diff: &str,
    issue_comments: &Value,
    reviews: &Value,
    review_comments: &Value,
    commits: &Value,
    check_runs: &Value,
    statuses: &Value,
) -> PullRequest {
    let merged = pr["merged"].as_bool().unwrap_or(false) || !pr["merged_at"].is_null();
    let state = if merged {
        "merged"
    } else if pr["state"].as_str() == Some("closed") {
        "closed"
    } else if pr["draft"].as_bool().unwrap_or(false) {
        "draft"
    } else {
        "open"
    };
    let author = s(&pr["user"]["login"]);
    let mut timeline = vec![PrTimelineEvent {
        kind: "opened".into(),
        author: Some(author.clone()),
        body: None,
        at: millis(&pr["created_at"]),
    }];
    for c in issue_comments.as_array().into_iter().flatten() {
        timeline.push(PrTimelineEvent {
            kind: "commented".into(),
            author: opt_s(&c["user"]["login"]),
            body: opt_s(&c["body"]),
            at: millis(&c["created_at"]),
        });
    }
    for r in reviews.as_array().into_iter().flatten() {
        if r["state"].as_str() == Some("PENDING") {
            continue;
        }
        timeline.push(PrTimelineEvent {
            kind: "reviewed".into(),
            author: opt_s(&r["user"]["login"]),
            body: opt_s(&r["body"]).or_else(|| opt_s(&r["state"]).map(|s| s.to_lowercase())),
            at: millis(&r["submitted_at"]),
        });
    }
    for c in commits.as_array().into_iter().flatten() {
        timeline.push(PrTimelineEvent {
            kind: "committed".into(),
            author: opt_s(&c["author"]["login"]).or_else(|| opt_s(&c["commit"]["author"]["name"])),
            body: c["commit"]["message"].as_str().and_then(|m| m.lines().next()).map(str::to_string),
            at: millis(&c["commit"]["committer"]["date"]),
        });
    }
    if merged {
        timeline.push(PrTimelineEvent {
            kind: "merged".into(),
            author: opt_s(&pr["merged_by"]["login"]),
            body: None,
            at: millis(&pr["merged_at"]),
        });
    } else if state == "closed" {
        timeline.push(PrTimelineEvent {
            kind: "closed".into(),
            author: None,
            body: None,
            at: millis(&pr["closed_at"]),
        });
    }
    timeline.sort_by_key(|e| e.at);

    let mut checks = Vec::new();
    for run in check_runs["check_runs"].as_array().into_iter().flatten() {
        checks.push(PrCheck {
            name: s(&run["name"]),
            state: check_state(run["status"].as_str(), run["conclusion"].as_str()).into(),
            url: opt_s(&run["details_url"]).or_else(|| opt_s(&run["html_url"])),
            id: run["id"].as_u64().map(|n| n.to_string()),
        });
    }
    for st in statuses["statuses"].as_array().into_iter().flatten() {
        checks.push(PrCheck {
            name: s(&st["context"]),
            state: check_state(st["state"].as_str(), None).into(),
            url: opt_s(&st["target_url"]),
            id: None,
        });
    }

    let review_comments = review_comments
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| PrReviewComment {
            id: c["id"].as_u64().map(|n| n.to_string()).unwrap_or_else(|| s(&c["node_id"])),
            author: s(&c["user"]["login"]),
            body: s(&c["body"]),
            path: s(&c["path"]),
            line: c["line"].as_u64().or_else(|| c["original_line"].as_u64()).map(|n| n as u32),
            side: opt_s(&c["side"]),
            at: millis(&c["created_at"]),
            in_reply_to: c["in_reply_to_id"].as_u64().map(|n| n.to_string()),
        })
        .collect();

    PullRequest {
        number: pr["number"].as_u64().unwrap_or(0) as u32,
        title: s(&pr["title"]),
        body: s(&pr["body"]),
        state: state.into(),
        url: s(&pr["html_url"]),
        author,
        head: s(&pr["head"]["ref"]),
        base: s(&pr["base"]["ref"]),
        additions: pr["additions"].as_u64().unwrap_or(0) as u32,
        deletions: pr["deletions"].as_u64().unwrap_or(0) as u32,
        checks,
        timeline,
        review_comments,
        files: parse_unified_diff(diff),
    }
}

/// Build a [`PullRequest`] from `gh pr view --json …`, `gh pr diff` and the review comments
/// (`gh api repos/{owner}/{repo}/pulls/{n}/comments`, REST shape).
pub fn pr_from_gh(view: &Value, diff: &str, review_comments: &Value) -> PullRequest {
    let raw_state = view["state"].as_str().unwrap_or("OPEN").to_ascii_uppercase();
    let state = match raw_state.as_str() {
        "MERGED" => "merged",
        "CLOSED" => "closed",
        _ if view["isDraft"].as_bool().unwrap_or(false) => "draft",
        _ => "open",
    };
    let author = s(&view["author"]["login"]);
    let mut timeline = vec![PrTimelineEvent {
        kind: "opened".into(),
        author: Some(author.clone()),
        body: None,
        at: millis(&view["createdAt"]),
    }];
    for c in view["comments"].as_array().into_iter().flatten() {
        timeline.push(PrTimelineEvent {
            kind: "commented".into(),
            author: opt_s(&c["author"]["login"]),
            body: opt_s(&c["body"]),
            at: millis(&c["createdAt"]),
        });
    }
    for r in view["reviews"].as_array().into_iter().flatten() {
        timeline.push(PrTimelineEvent {
            kind: "reviewed".into(),
            author: opt_s(&r["author"]["login"]),
            body: opt_s(&r["body"]).or_else(|| opt_s(&r["state"]).map(|s| s.to_lowercase())),
            at: millis(&r["submittedAt"]),
        });
    }
    for c in view["commits"].as_array().into_iter().flatten() {
        let author = c["authors"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|a| opt_s(&a["login"]).or_else(|| opt_s(&a["name"])));
        timeline.push(PrTimelineEvent {
            kind: "committed".into(),
            author,
            body: opt_s(&c["messageHeadline"]),
            at: millis(&c["committedDate"]),
        });
    }
    match state {
        "merged" => timeline.push(PrTimelineEvent {
            kind: "merged".into(),
            author: opt_s(&view["mergedBy"]["login"]),
            body: None,
            at: millis(&view["mergedAt"]),
        }),
        "closed" => timeline.push(PrTimelineEvent {
            kind: "closed".into(),
            author: None,
            body: None,
            at: millis(&view["closedAt"]),
        }),
        _ => {}
    }
    timeline.sort_by_key(|e| e.at);

    let checks = view["statusCheckRollup"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            if c["__typename"].as_str() == Some("StatusContext") {
                PrCheck {
                    name: s(&c["context"]),
                    state: check_state(c["state"].as_str(), None).into(),
                    url: opt_s(&c["targetUrl"]),
                    id: None,
                }
            } else {
                let url = opt_s(&c["detailsUrl"]);
                PrCheck {
                    name: s(&c["name"]),
                    state: check_state(c["status"].as_str(), c["conclusion"].as_str().filter(|s| !s.is_empty())).into(),
                    id: url.as_deref().and_then(job_id_from_url),
                    url,
                }
            }
        })
        .collect();

    let empty = Value::Null;
    let mut pr = pr_from_rest(&json!({}), diff, &empty, &empty, review_comments, &empty, &empty, &empty);
    pr.number = view["number"].as_u64().unwrap_or(0) as u32;
    pr.title = s(&view["title"]);
    pr.body = s(&view["body"]);
    pr.state = state.into();
    pr.url = s(&view["url"]);
    pr.author = author;
    pr.head = s(&view["headRefName"]);
    pr.base = s(&view["baseRefName"]);
    pr.additions = view["additions"].as_u64().unwrap_or(0) as u32;
    pr.deletions = view["deletions"].as_u64().unwrap_or(0) as u32;
    pr.checks = checks;
    pr.timeline = timeline;
    pr
}

/// Minimal GitHub REST client.
struct Api {
    client: reqwest::Client,
    base: String,
    repo: GithubRepo,
    token: Option<String>,
}

impl Api {
    fn new(repo: GithubRepo, base: String, token: Option<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| GitError::Network(e.to_string()))?;
        Ok(Api { client, base, repo, token })
    }

    fn require_token(&self) -> Result<()> {
        if self.token.is_some() {
            Ok(())
        } else {
            Err(GitError::Invalid(
                "a GitHub token is required (add one in Settings → Git, set GITHUB_TOKEN or GH_TOKEN, or install the gh CLI)"
                    .into(),
            ))
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/repos/{}/{}{}", self.base, self.repo.owner, self.repo.repo, path)
    }

    fn request(&self, method: reqwest::Method, path: &str, accept: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .request(method, self.url(path))
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        req
    }

    async fn send(req: reqwest::RequestBuilder) -> Result<String> {
        // `without_url`: errors must not echo request details.
        let resp = req.send().await.map_err(|e| GitError::Network(e.without_url().to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| GitError::Network(e.without_url().to_string()))?;
        if status.is_success() {
            Ok(text)
        } else {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    let mut msg = v["message"].as_str()?.to_string();
                    if let Some(errors) = v["errors"].as_array() {
                        for e in errors {
                            if let Some(m) = e["message"].as_str().or_else(|| e.as_str()) {
                                msg.push_str(&format!("; {m}"));
                            }
                        }
                    }
                    Some(msg)
                })
                .unwrap_or_else(|| text.chars().take(300).collect());
            Err(GitError::Http { status: status.as_u16(), message })
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let text = Self::send(self.request(reqwest::Method::GET, path, "application/vnd.github+json")).await?;
        serde_json::from_str(&text).map_err(|e| GitError::Parse(e.to_string()))
    }

    async fn get_or_null(&self, path: String) -> Value {
        self.get(&path).await.unwrap_or(Value::Null)
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let req = self.request(reqwest::Method::POST, path, "application/vnd.github+json").json(body);
        let text = Self::send(req).await?;
        serde_json::from_str(&text).map_err(|e| GitError::Parse(e.to_string()))
    }

    async fn diff(&self, number: u32) -> String {
        Self::send(self.request(reqwest::Method::GET, &format!("/pulls/{number}"), "application/vnd.github.diff"))
            .await
            .unwrap_or_default()
    }

    /// Plain-text job log (GitHub redirects to a short-lived download URL).
    async fn job_log(&self, job_id: &str) -> Result<String> {
        let path = format!("/actions/jobs/{job_id}/logs");
        Self::send(self.request(reqwest::Method::GET, &path, "application/vnd.github+json")).await
    }
}

async fn gh_json(gh: GitCommand, args: &[&str]) -> Result<Value> {
    let out = gh.args(args).run().await?;
    serde_json::from_slice(&out.stdout).map_err(|e| GitError::Parse(format!("gh output: {e}")))
}

/// Open a pull request for the current branch (which must already be pushed).
/// Returns the PR URL and number.
pub async fn create(
    cwd: &Path,
    title: &str,
    body: &str,
    base: Option<&str>,
    draft: bool,
    cfg: &GithubConfig,
) -> Result<(String, Option<u32>)> {
    if cfg.use_gh().await {
        let mut args = vec!["pr", "create", "--title", title, "--body-file", "-"];
        if let Some(b) = base {
            args.extend(["--base", b]);
        }
        if draft {
            args.push("--draft");
        }
        let out = cfg.gh(cwd).args(&args).stdin(body.as_bytes().to_vec()).run().await?;
        let url = out.stdout_str().lines().rev().find(|l| l.contains("/pull/")).unwrap_or_default().trim().to_string();
        let number = number_from_url(&url);
        return Ok((url, number));
    }
    let repo = origin_repo(cwd).await?;
    let git = Git::new(cwd);
    let head = git
        .current_branch()
        .await?
        .ok_or_else(|| GitError::Invalid("detached HEAD: no branch to open a PR from".into()))?;
    let api = cfg.api(repo)?;
    api.require_token()?;
    let base = match base {
        Some(b) => b.to_string(),
        None => match api.get("").await.ok().and_then(|v| opt_s(&v["default_branch"])) {
            Some(b) => b,
            None => git.default_branch().await?,
        },
    };
    let created = api
        .post("/pulls", &json!({ "title": title, "body": body, "head": head, "base": base, "draft": draft }))
        .await?;
    Ok((s(&created["html_url"]), created["number"].as_u64().map(|n| n as u32)))
}

/// Fetch a pull request with checks, timeline, review comments and parsed file diffs.
/// `number: None` looks up the PR for the current branch; `Ok(None)` when there is none.
pub async fn view(cwd: &Path, number: Option<u32>, cfg: &GithubConfig) -> Result<Option<PullRequest>> {
    if cfg.use_gh().await {
        const FIELDS: &str = "number,title,body,state,url,author,headRefName,baseRefName,additions,deletions,isDraft,\
statusCheckRollup,comments,reviews,commits,createdAt,mergedAt,closedAt,mergedBy";
        let num = number.map(|n| n.to_string());
        let mut args = vec!["pr", "view"];
        if let Some(n) = &num {
            args.push(n);
        }
        args.extend(["--json", FIELDS]);
        let out = cfg.gh(cwd).args(&args).output().await?;
        if !out.success() {
            let err = out.stderr_str();
            if err.contains("no pull requests found") || err.contains("Could not resolve to a PullRequest") {
                return Ok(None);
            }
            return Err(crate::cmd::command_error("gh", args.join(" "), cwd, &out));
        }
        let view: Value = serde_json::from_slice(&out.stdout).map_err(|e| GitError::Parse(e.to_string()))?;
        let n = view["number"].as_u64().unwrap_or(0).to_string();
        let diff = cfg
            .gh(cwd)
            .args(["pr", "diff", n.as_str(), "--color=never"])
            .run()
            .await
            .map(|o| o.stdout_str())
            .unwrap_or_default();
        let comments_path = format!("repos/{{owner}}/{{repo}}/pulls/{n}/comments?per_page=100");
        let comments = gh_json(cfg.gh(cwd), &["api", comments_path.as_str()]).await.unwrap_or(Value::Null);
        return Ok(Some(pr_from_gh(&view, &diff, &comments)));
    }

    let repo = origin_repo(cwd).await?;
    let api = cfg.api(repo.clone())?;
    let number = match number {
        Some(n) => n,
        None => {
            let Some(branch) = Git::new(cwd).current_branch().await? else { return Ok(None) };
            let list = api.get(&format!("/pulls?state=all&per_page=1&head={}:{}", repo.owner, branch)).await?;
            match list.as_array().and_then(|a| a.first()).and_then(|p| p["number"].as_u64()) {
                Some(n) => n as u32,
                None => return Ok(None),
            }
        }
    };
    let pr = match api.get(&format!("/pulls/{number}")).await {
        Ok(v) => v,
        Err(GitError::Http { status: 404, .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    let sha = s(&pr["head"]["sha"]);
    let (diff, issue_comments, reviews, review_comments, commits, runs, statuses) = tokio::join!(
        api.diff(number),
        api.get_or_null(format!("/issues/{number}/comments?per_page=100")),
        api.get_or_null(format!("/pulls/{number}/reviews?per_page=100")),
        api.get_or_null(format!("/pulls/{number}/comments?per_page=100")),
        api.get_or_null(format!("/pulls/{number}/commits?per_page=100")),
        api.get_or_null(format!("/commits/{sha}/check-runs?per_page=100")),
        api.get_or_null(format!("/commits/{sha}/status")),
    );
    Ok(Some(pr_from_rest(&pr, &diff, &issue_comments, &reviews, &review_comments, &commits, &runs, &statuses)))
}

fn list_item_from_rest(p: &Value) -> PrListItem {
    let state = if p["merged_at"].is_string() {
        "merged".to_string()
    } else if p["draft"].as_bool().unwrap_or(false) {
        "draft".to_string()
    } else {
        s(&p["state"]).to_lowercase()
    };
    PrListItem {
        number: p["number"].as_u64().unwrap_or(0) as u32,
        title: s(&p["title"]),
        state,
        author: s(&p["user"]["login"]),
        head: s(&p["head"]["ref"]),
        base: s(&p["base"]["ref"]),
        url: s(&p["html_url"]),
        updated_at: millis(&p["updated_at"]),
    }
}

fn list_item_from_gh(p: &Value) -> PrListItem {
    let state =
        if p["isDraft"].as_bool().unwrap_or(false) { "draft".to_string() } else { s(&p["state"]).to_lowercase() };
    PrListItem {
        number: p["number"].as_u64().unwrap_or(0) as u32,
        title: s(&p["title"]),
        state,
        author: s(&p["author"]["login"]),
        head: s(&p["headRefName"]),
        base: s(&p["baseRefName"]),
        url: s(&p["url"]),
        updated_at: millis(&p["updatedAt"]),
    }
}

/// Open pull requests of the `origin` repository, most recently updated first.
pub async fn list(cwd: &Path, cfg: &GithubConfig) -> Result<Vec<PrListItem>> {
    const GH_FIELDS: &str = "number,title,state,isDraft,author,headRefName,baseRefName,url,updatedAt";
    let mut items: Vec<PrListItem> = if cfg.use_gh().await {
        let v = gh_json(cfg.gh(cwd), &["pr", "list", "--json", GH_FIELDS, "--limit", "50"]).await?;
        v.as_array().into_iter().flatten().map(list_item_from_gh).collect()
    } else {
        let api = cfg.api(origin_repo(cwd).await?)?;
        let v = api.get("/pulls?state=open&sort=updated&direction=desc&per_page=50").await?;
        v.as_array().into_iter().flatten().map(list_item_from_rest).collect()
    };
    items.sort_by_key(|p| std::cmp::Reverse(p.updated_at));
    Ok(items)
}

/// Submit a review (`COMMENT`, `APPROVE` or `REQUEST_CHANGES`) with inline comments.
/// Returns the review's URL.
pub async fn submit_review(
    cwd: &Path,
    number: u32,
    comments: &[ReviewComment],
    body: Option<&str>,
    event: &str,
    cfg: &GithubConfig,
) -> Result<String> {
    let payload = review_payload(comments, body, event)?;
    if cfg.use_gh().await {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews");
        let out = cfg
            .gh(cwd)
            .args(["api", "--method", "POST", path.as_str(), "--input", "-"])
            .stdin(serde_json::to_vec(&payload).map_err(|e| GitError::Parse(e.to_string()))?)
            .run()
            .await?;
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| GitError::Parse(e.to_string()))?;
        return Ok(opt_s(&v["html_url"]).unwrap_or_else(|| v["id"].to_string()));
    }
    let api = cfg.api(origin_repo(cwd).await?)?;
    api.require_token()?;
    let v = api.post(&format!("/pulls/{number}/reviews"), &payload).await?;
    Ok(opt_s(&v["html_url"]).unwrap_or_else(|| v["id"].to_string()))
}

/// Text of a check run's `output` (title, summary, details).
fn check_output_text(run: &Value) -> String {
    let out = &run["output"];
    [opt_s(&out["title"]), opt_s(&out["summary"]), opt_s(&out["text"])]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The output and failed log of one check, capped at [`CHECK_LOG_CAP`] characters.
/// `check_id` is the check-run (Actions job) id; an Actions job `url` may stand in for it.
/// Returns the text and whether the log was cut.
pub async fn check_log(
    cwd: &Path,
    check_id: Option<&str>,
    url: Option<&str>,
    cfg: &GithubConfig,
) -> Result<(String, bool)> {
    let id = check_id
        .map(str::to_string)
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .or_else(|| url.and_then(job_id_from_url))
        .ok_or_else(|| GitError::Invalid("this check has no log Odex can fetch (not a GitHub check run)".into()))?;
    let (summary, log) = if cfg.use_gh().await {
        let path = format!("repos/{{owner}}/{{repo}}/check-runs/{id}");
        let summary =
            gh_json(cfg.gh(cwd), &["api", path.as_str()]).await.map(|v| check_output_text(&v)).unwrap_or_default();
        let job = ["run", "view", "--job", id.as_str()];
        let mut log = cfg.gh(cwd).args(job).arg("--log-failed").run().await.map(|o| o.stdout_str());
        if log.as_ref().map_or(true, |l| l.trim().is_empty()) {
            log = cfg.gh(cwd).args(job).arg("--log").run().await.map(|o| o.stdout_str());
        }
        (summary, log)
    } else {
        let api = cfg.api(origin_repo(cwd).await?)?;
        let run_path = format!("/check-runs/{id}");
        let (run, log) = tokio::join!(api.get(&run_path), api.job_log(&id));
        (run.map(|v| check_output_text(&v)).unwrap_or_default(), log)
    };
    let summary = summary.trim();
    let summary_cut = summary.chars().count() > CHECK_LOG_CAP / 3;
    let summary: String = if summary_cut {
        summary.chars().take(CHECK_LOG_CAP / 3).chain("…".chars()).collect()
    } else {
        summary.to_string()
    };
    let (log_text, log_cut) = match &log {
        Ok(l) if !l.trim().is_empty() => tail_log(l, CHECK_LOG_CAP - summary.len()),
        _ => (String::new(), false),
    };
    if summary.is_empty() && log_text.is_empty() {
        return Err(match log {
            Err(e) => e,
            Ok(_) => GitError::Invalid("the check has no output or log".into()),
        });
    }
    let text = [summary, log_text].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n");
    Ok((text, summary_cut || log_cut))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn repo(host: &str, owner: &str, name: &str) -> Option<GithubRepo> {
        Some(GithubRepo { host: host.into(), owner: owner.into(), repo: name.into() })
    }

    #[test]
    fn remote_url_forms() {
        let expected = repo("github.com", "odex-app", "odex");
        for url in [
            "https://github.com/odex-app/odex.git",
            "https://github.com/odex-app/odex",
            "https://github.com/odex-app/odex/",
            "http://github.com/odex-app/odex.git",
            "https://user:pass@github.com/odex-app/odex.git",
            "git@github.com:odex-app/odex.git",
            "github.com:odex-app/odex",
            "ssh://git@github.com/odex-app/odex.git",
            "ssh://git@github.com:22/odex-app/odex.git",
            "git://github.com/odex-app/odex.git",
            "  https://GitHub.com/odex-app/odex.git\n",
        ] {
            assert_eq!(parse_remote_url(url), expected, "{url}");
        }
        assert_eq!(parse_remote_url("git@ghe.corp.example:team/svc.git"), repo("ghe.corp.example", "team", "svc"));
        assert_eq!(repo("ghe.corp.example", "t", "s").unwrap().api_base(), "https://ghe.corp.example/api/v3");
        assert_eq!(expected.unwrap().api_base(), "https://api.github.com");
        assert_eq!(parse_remote_url("C:/Users/me/repo.git"), None);
        assert_eq!(parse_remote_url("/srv/git/repo.git"), None);
        assert_eq!(parse_remote_url("file:///srv/git/o/r.git"), None);
        assert_eq!(parse_remote_url("https://gitlab.com/group/sub/repo.git"), None);
    }

    #[test]
    fn review_payload_shapes() {
        let comments = vec![
            ReviewComment {
                path: "src/a.rs".into(),
                line: Some(10),
                end_line: None,
                side: Some("new".into()),
                body: "nit".into(),
                snippet: None,
            },
            ReviewComment {
                path: "src/b.rs".into(),
                line: Some(3),
                end_line: Some(6),
                side: Some("old".into()),
                body: "range".into(),
                snippet: None,
            },
            ReviewComment {
                path: "README.md".into(),
                line: None,
                end_line: None,
                side: None,
                body: "whole file".into(),
                snippet: None,
            },
        ];
        let p = review_payload(&comments, Some("Looks good"), "request_changes").unwrap();
        assert_eq!(
            p,
            json!({
                "event": "REQUEST_CHANGES",
                "body": "Looks good\n\n**README.md**: whole file",
                "comments": [
                    { "path": "src/a.rs", "body": "nit", "side": "RIGHT", "line": 10 },
                    { "path": "src/b.rs", "body": "range", "side": "LEFT", "start_line": 3, "start_side": "LEFT", "line": 6 }
                ]
            })
        );
        let p = review_payload(&[], None, "APPROVE").unwrap();
        assert_eq!(p, json!({ "event": "APPROVE", "comments": [] }));
        assert!(review_payload(&[], None, "MERGE").is_err());
    }

    #[test]
    fn check_states() {
        assert_eq!(check_state(Some("in_progress"), None), "pending");
        assert_eq!(check_state(Some("completed"), Some("success")), "success");
        assert_eq!(check_state(Some("completed"), Some("timed_out")), "failure");
        assert_eq!(check_state(Some("completed"), Some("skipped")), "skipped");
        assert_eq!(check_state(Some("COMPLETED"), Some("NEUTRAL")), "neutral");
        assert_eq!(check_state(Some("error"), None), "failure");
        assert_eq!(check_state(Some("pending"), None), "pending");
        assert_eq!(check_state(Some("success"), None), "success");
    }

    const DIFF: &str = "diff --git a/src/lib.rs b/src/lib.rs\nindex 1..2 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n a\n+b\n";

    #[test]
    fn rest_assembly() {
        let pr = json!({
            "number": 42, "title": "Add b", "body": "Body", "state": "open", "draft": false, "merged": false,
            "html_url": "https://github.com/o/r/pull/42", "user": {"login": "alice"},
            "head": {"ref": "feature", "sha": "abc"}, "base": {"ref": "main"},
            "additions": 1, "deletions": 0, "created_at": "2024-05-01T10:00:00Z"
        });
        let issue_comments = json!([{ "user": {"login": "bob"}, "body": "hi", "created_at": "2024-05-01T11:00:00Z" }]);
        let reviews = json!([
            { "user": {"login": "carol"}, "body": "", "state": "APPROVED", "submitted_at": "2024-05-01T12:00:00Z" },
            { "user": {"login": "dave"}, "body": "draft", "state": "PENDING", "submitted_at": null }
        ]);
        let review_comments = json!([{
            "id": 7, "user": {"login": "carol"}, "body": "why?", "path": "src/lib.rs", "line": 2, "side": "RIGHT",
            "created_at": "2024-05-01T12:00:00Z", "in_reply_to_id": 5
        }]);
        let commits = json!([{ "author": {"login": "alice"}, "commit": { "message": "Add b\n\nbody", "committer": {"date": "2024-05-01T09:00:00Z"} } }]);
        let runs = json!({ "check_runs": [
            { "name": "ci", "status": "completed", "conclusion": "success", "details_url": "https://ci/1" },
            { "name": "lint", "status": "in_progress", "conclusion": null }
        ]});
        let statuses = json!({ "statuses": [{ "context": "deploy", "state": "error", "target_url": "https://d" }] });
        let out = pr_from_rest(&pr, DIFF, &issue_comments, &reviews, &review_comments, &commits, &runs, &statuses);
        assert_eq!(out.number, 42);
        assert_eq!(out.state, "open");
        assert_eq!((out.head.as_str(), out.base.as_str(), out.author.as_str()), ("feature", "main", "alice"));
        let kinds: Vec<&str> = out.timeline.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["committed", "opened", "commented", "reviewed"]);
        assert_eq!(out.timeline[0].body.as_deref(), Some("Add b"));
        assert_eq!(out.timeline[3].body.as_deref(), Some("approved"));
        assert_eq!(out.timeline[1].at, 1_714_557_600_000);
        let states: Vec<&str> = out.checks.iter().map(|c| c.state.as_str()).collect();
        assert_eq!(states, ["success", "pending", "failure"]);
        assert_eq!(out.review_comments[0].id, "7");
        assert_eq!(out.review_comments[0].in_reply_to.as_deref(), Some("5"));
        assert_eq!(out.review_comments[0].line, Some(2));
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].additions, 1);

        let merged = pr_from_rest(
            &json!({"state": "closed", "merged_at": "2024-05-02T00:00:00Z", "merged_by": {"login": "x"}}),
            "",
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &Value::Null,
        );
        assert_eq!(merged.state, "merged");
        assert_eq!(merged.timeline.last().unwrap().kind, "merged");
        let draft = pr_from_rest(
            &json!({"state": "open", "draft": true}),
            "",
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &Value::Null,
        );
        assert_eq!(draft.state, "draft");
    }

    #[test]
    fn gh_assembly() {
        let view = json!({
            "number": 9, "title": "T", "body": "B", "state": "OPEN", "isDraft": true, "url": "https://github.com/o/r/pull/9",
            "author": {"login": "alice"}, "headRefName": "f", "baseRefName": "main", "additions": 3, "deletions": 1,
            "createdAt": "2024-01-01T00:00:00Z",
            "comments": [{"author": {"login": "bob"}, "body": "c", "createdAt": "2024-01-02T00:00:00Z"}],
            "reviews": [{"author": {"login": "carol"}, "body": "", "state": "CHANGES_REQUESTED", "submittedAt": "2024-01-03T00:00:00Z"}],
            "commits": [{"messageHeadline": "init", "committedDate": "2023-12-31T00:00:00Z", "authors": [{"login": "alice", "name": "Alice"}]}],
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://ci"},
                {"__typename": "StatusContext", "context": "ext", "state": "PENDING", "targetUrl": null}
            ]
        });
        let pr = pr_from_gh(&view, DIFF, &json!([]));
        assert_eq!((pr.number, pr.state.as_str(), pr.head.as_str()), (9, "draft", "f"));
        assert_eq!(pr.checks.iter().map(|c| c.state.as_str()).collect::<Vec<_>>(), ["failure", "pending"]);
        assert_eq!(
            pr.timeline.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            ["committed", "opened", "commented", "reviewed"]
        );
        assert_eq!(pr.timeline[3].body.as_deref(), Some("changes_requested"));
        assert_eq!(pr.files[0].path, "src/lib.rs");
        assert_eq!(number_from_url("https://github.com/o/r/pull/123"), Some(123));
        assert_eq!(number_from_url("https://github.com/o/r/pull/123/files"), Some(123));
    }

    #[tokio::test]
    async fn origin_parsing_from_repo() {
        let repo = crate::testutil::TestRepo::new();
        repo.git(&["remote", "add", "origin", "git@github.com:odex-app/odex.git"]);
        let r = origin_repo(&repo.path).await.unwrap();
        assert_eq!(r.slug(), "odex-app/odex");
        let cfg = GithubConfig { token: Some("t".into()), api_base: None };
        let api = cfg.api(r).unwrap();
        assert_eq!(api.url("/pulls/1"), "https://api.github.com/repos/odex-app/odex/pulls/1");
        let o = GithubRepo { host: "github.com".into(), owner: "o".into(), repo: "r".into() };
        assert!(GithubConfig::default().api(o.clone()).unwrap().require_token().is_err());
        let fake = GithubConfig::rest("http://127.0.0.1:9/", None);
        assert_eq!(fake.api(o).unwrap().url("/pulls"), "http://127.0.0.1:9/repos/o/r/pulls");
        assert!(!format!("{:?}", GithubConfig::rest("x", Some("s3cret"))).contains("s3cret"));
    }

    #[test]
    fn helpers() {
        assert_eq!(job_id_from_url("https://github.com/o/r/actions/runs/11/job/22").as_deref(), Some("22"));
        assert_eq!(job_id_from_url("https://github.com/o/r/runs/33?check_suite_focus=true").as_deref(), Some("33"));
        assert_eq!(job_id_from_url("https://github.com/o/r/actions/runs/11"), None);
        assert_eq!(job_id_from_url("https://ci.example/build/5"), None);
        assert_eq!(normalize_event("requestChanges"), Some("REQUEST_CHANGES"));
        assert_eq!(normalize_event("request-changes"), Some("REQUEST_CHANGES"));
        assert_eq!(normalize_event("Approve"), Some("APPROVE"));
        assert_eq!(normalize_event(""), Some("COMMENT"));
        assert_eq!(normalize_event("merge"), None);
        let check = |state: &str| PrCheck { name: "c".into(), state: state.into(), url: None, id: None };
        assert_eq!(summarize_checks(&[]), (None, 0));
        assert_eq!(summarize_checks(&[check("success"), check("pending")]), (Some("pending".into()), 0));
        assert_eq!(
            summarize_checks(&[check("failure"), check("failure"), check("success")]),
            (Some("failure".into()), 2)
        );
        assert_eq!(summarize_checks(&[check("success"), check("skipped")]), (Some("success".into()), 0));

        let (short, cut) = tail_log("2024-05-01T10:00:00.1234567Z hello\n\nworld\n", 100);
        assert_eq!((short.as_str(), cut), ("hello\nworld", false));
        let mut long = String::from("2024-05-01T10:00:00.0000000Z ##[error]early failure\n");
        for i in 0..2000 {
            long.push_str(&format!("2024-05-01T10:00:01.0000000Z line {i}\n"));
        }
        long.push_str("2024-05-01T10:00:02.0000000Z ##[error]Process completed with exit code 1.\n");
        let (t, cut) = tail_log(&long, 1000);
        assert!(cut);
        assert!(t.chars().count() <= 1100, "{}", t.len());
        assert!(t.contains("##[error]early failure"));
        assert!(t.ends_with("##[error]Process completed with exit code 1."));
        assert!(!t.contains("2024-05-01T"));
    }

    /// A tiny fake of the GitHub REST API: records requests, answers the PR endpoints.
    mod fake {
        use std::sync::{Arc, Mutex};

        use axum::body::Body;
        use axum::extract::{Request, State};
        use axum::http::{header, StatusCode};
        use axum::response::Response;
        use axum::Router;
        use serde_json::{json, Value};

        #[derive(Debug, Clone)]
        pub struct Hit {
            pub method: String,
            pub path: String,
            pub query: String,
            pub auth: Option<String>,
            pub body: Value,
        }

        #[derive(Clone)]
        pub struct Fake {
            pub base: String,
            pub hits: Arc<Mutex<Vec<Hit>>>,
        }

        pub const DIFF: &str = "diff --git a/src/app.rs b/src/app.rs\nindex 1..2 100644\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1,2 +1,3 @@\n fn main() {\n+    run();\n }\n";

        fn json(v: Value) -> Response {
            Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(v.to_string()))
                .unwrap()
        }

        fn status(code: StatusCode, v: Value) -> Response {
            let mut r = json(v);
            *r.status_mut() = code;
            r
        }

        pub fn pr_json(n: u64) -> Value {
            json!({
                "number": n, "title": "Add run()", "body": "Calls run.", "state": "open", "draft": false, "merged": false,
                "html_url": format!("https://github.com/o/r/pull/{n}"), "user": {"login": "alice"},
                "head": {"ref": "feature", "sha": "abc123"}, "base": {"ref": "main"},
                "additions": 1, "deletions": 0, "created_at": "2024-05-01T10:00:00Z", "updated_at": "2024-05-02T10:00:00Z"
            })
        }

        async fn handle(State(f): State<Fake>, req: Request) -> Response {
            let method = req.method().to_string();
            let path = req.uri().path().to_string();
            let query = req.uri().query().unwrap_or("").to_string();
            let accept = req.headers().get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            let auth = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(str::to_string);
            let bytes = axum::body::to_bytes(req.into_body(), usize::MAX).await.unwrap_or_default();
            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            f.hits.lock().unwrap().push(Hit {
                method: method.clone(),
                path: path.clone(),
                query: query.clone(),
                auth,
                body,
            });
            if path == "/blob/55" {
                let mut log = String::new();
                for i in 0..3000 {
                    log.push_str(&format!("2024-05-01T10:00:00.0000000Z step output {i}\n"));
                }
                log.push_str("2024-05-01T10:00:01.0000000Z ##[error]test failed: expected 2, got 3\n");
                return Response::new(Body::from(log));
            }
            let Some(rest) = path.strip_prefix("/repos/o/r") else {
                return status(StatusCode::NOT_FOUND, json!({"message": "Not Found"}));
            };
            match (method.as_str(), rest) {
                ("GET", "/pulls") if query.contains("head=o:feature") => json(json!([pr_json(7)])),
                ("GET", "/pulls") if query.contains("head=") => json(json!([])),
                ("GET", "/pulls") => json(json!([
                    { "number": 3, "title": "Older", "state": "open", "draft": true, "user": {"login": "bob"},
                      "head": {"ref": "old"}, "base": {"ref": "main"}, "html_url": "https://github.com/o/r/pull/3",
                      "updated_at": "2024-04-01T00:00:00Z", "merged_at": null },
                    pr_json(7),
                ])),
                ("POST", "/pulls") => json(json!({ "number": 9, "html_url": "https://github.com/o/r/pull/9" })),
                ("GET", "/pulls/7") if accept.contains("diff") => Response::new(Body::from(DIFF)),
                ("GET", "/pulls/7") => json(pr_json(7)),
                ("GET", "/pulls/404") => status(StatusCode::NOT_FOUND, json!({"message": "Not Found"})),
                ("GET", "/pulls/7/comments") => json(json!([{
                    "id": 70, "user": {"login": "carol"}, "body": "Why run here?", "path": "src/app.rs", "line": 2,
                    "side": "RIGHT", "created_at": "2024-05-01T12:00:00Z"
                }])),
                ("GET", "/commits/abc123/check-runs") => json(json!({ "check_runs": [
                    { "id": 55, "name": "test", "status": "completed", "conclusion": "failure",
                      "details_url": "https://github.com/o/r/actions/runs/5/job/55" },
                    { "id": 56, "name": "lint", "status": "completed", "conclusion": "success" }
                ]})),
                ("GET", "/check-runs/55") => {
                    json(json!({ "id": 55, "output": { "title": "1 test failed", "summary": "math::add failed" } }))
                }
                ("GET", "/actions/jobs/55/logs") => Response::builder()
                    .status(StatusCode::FOUND)
                    .header(header::LOCATION, format!("{}/blob/55", f.base))
                    .body(Body::empty())
                    .unwrap(),
                ("POST", "/pulls/7/reviews") => {
                    json(json!({ "id": 700, "html_url": "https://github.com/o/r/pull/7#pullrequestreview-700" }))
                }
                ("GET", p) if p.starts_with("/issues/") || p.starts_with("/pulls/7/") || p.starts_with("/commits/") => {
                    json(json!([]))
                }
                _ => status(StatusCode::NOT_FOUND, json!({"message": "Not Found"})),
            }
        }

        pub async fn start() -> Fake {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let fake = Fake { base, hits: Arc::new(Mutex::new(Vec::new())) };
            let app = Router::new().fallback(handle).with_state(fake.clone());
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            fake
        }
    }

    fn github_repo() -> crate::testutil::TestRepo {
        let repo = crate::testutil::TestRepo::new();
        repo.write("a.txt", "a\n");
        repo.commit_all("init");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.git(&["remote", "add", "origin", "https://github.com/o/r.git"]);
        repo
    }

    #[tokio::test]
    async fn rest_flows_against_fake_github() {
        let gh = fake::start().await;
        let repo = github_repo();
        let cfg = GithubConfig::rest(&gh.base, Some("t0k"));

        // inbox: most recently updated first, draft state mapped
        let prs = list(&repo.path, &cfg).await.unwrap();
        assert_eq!(prs.iter().map(|p| (p.number, p.state.as_str())).collect::<Vec<_>>(), [(7, "open"), (3, "draft")]);
        assert_eq!((prs[0].author.as_str(), prs[0].head.as_str(), prs[0].base.as_str()), ("alice", "feature", "main"));
        assert_eq!(prs[0].url, "https://github.com/o/r/pull/7");

        // the PR for the current branch, with files, review comments and check ids
        let pr = view(&repo.path, None, &cfg).await.unwrap().expect("pr for branch");
        assert_eq!(pr.number, 7);
        assert_eq!(pr.files.len(), 1);
        assert_eq!(pr.files[0].path, "src/app.rs");
        assert_eq!(pr.files[0].additions, 1);
        assert_eq!(pr.review_comments.len(), 1);
        assert_eq!((pr.review_comments[0].path.as_str(), pr.review_comments[0].line), ("src/app.rs", Some(2)));
        assert_eq!(pr.checks[0].id.as_deref(), Some("55"));
        assert_eq!(summarize_checks(&pr.checks), (Some("failure".into()), 1));
        assert!(view(&repo.path, Some(404), &cfg).await.unwrap().is_none());

        // submit a review with an event and inline comments
        let comments = vec![ReviewComment {
            path: "src/app.rs".into(),
            line: Some(2),
            end_line: None,
            side: Some("new".into()),
            body: "Handle the error from run().".into(),
            snippet: None,
        }];
        let url = submit_review(&repo.path, 7, &comments, Some("Needs work"), "requestChanges", &cfg).await.unwrap();
        assert!(url.ends_with("#pullrequestreview-700"));
        let hit = gh.hits.lock().unwrap().iter().find(|h| h.method == "POST" && h.path.ends_with("/reviews")).cloned();
        let hit = hit.expect("review posted");
        assert_eq!(hit.auth.as_deref(), Some("Bearer t0k"));
        assert_eq!(hit.body["event"], "REQUEST_CHANGES");
        assert_eq!(hit.body["body"], "Needs work");
        assert_eq!(
            hit.body["comments"],
            json!([{ "path": "src/app.rs", "body": "Handle the error from run().", "side": "RIGHT", "line": 2 }])
        );

        // a failing check's output and log tail, capped
        let (text, truncated) = check_log(&repo.path, Some("55"), None, &cfg).await.unwrap();
        assert!(truncated);
        assert!(text.starts_with("1 test failed\n\nmath::add failed"), "{text}");
        assert!(text.ends_with("##[error]test failed: expected 2, got 3"), "{text}");
        assert!(text.chars().count() <= CHECK_LOG_CAP + 200);
        // the job id can come from the details URL
        let (by_url, _) =
            check_log(&repo.path, None, Some("https://github.com/o/r/actions/runs/5/job/55"), &cfg).await.unwrap();
        assert_eq!(by_url, text);
        assert!(check_log(&repo.path, None, Some("https://ci.example/1"), &cfg).await.is_err());

        // create a PR from the current branch
        let (url, number) = create(&repo.path, "T", "B", Some("main"), true, &cfg).await.unwrap();
        assert_eq!((url.as_str(), number), ("https://github.com/o/r/pull/9", Some(9)));
        let hits = gh.hits.lock().unwrap().clone();
        let created = hits.iter().find(|h| h.method == "POST" && h.path == "/repos/o/r/pulls").unwrap();
        assert_eq!(
            created.body,
            json!({ "title": "T", "body": "B", "head": "feature", "base": "main", "draft": true })
        );
        assert!(hits.iter().filter(|h| h.path.starts_with("/repos/")).all(|h| h.auth.as_deref() == Some("Bearer t0k")));
        assert!(hits.iter().any(|h| h.path == "/repos/o/r/pulls" && h.query.contains("state=open")));

        // posting needs a token
        let anon = GithubConfig::rest(&gh.base, None);
        let err = submit_review(&repo.path, 7, &[], Some("x"), "comment", &anon).await.unwrap_err();
        assert!(err.to_string().contains("token is required"), "{err}");
    }
}
