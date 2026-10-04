//! GitHub pull requests: through the `gh` CLI when it is installed, otherwise the REST API with
//! a token (explicit, or `GITHUB_TOKEN` / `GH_TOKEN`). The repository is derived from the
//! `origin` remote (https, ssh and scp-like forms; GitHub Enterprise hosts use `/api/v3`).

use std::path::Path;
use std::time::Duration;

use odex_protocol::{PrCheck, PrReviewComment, PrTimelineEvent, PullRequest, ReviewComment};
use serde_json::{json, Value};

use crate::cmd::GitCommand;
use crate::diff::parse_unified_diff;
use crate::error::{GitError, Result};
use crate::Git;

const USER_AGENT: &str = concat!("odex-git/", env!("CARGO_PKG_VERSION"));

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
    let event = event.trim().to_ascii_uppercase();
    if !matches!(event.as_str(), "COMMENT" | "APPROVE" | "REQUEST_CHANGES") {
        return Err(GitError::Invalid(format!("invalid review event {event:?}")));
    }
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
        });
    }
    for st in statuses["statuses"].as_array().into_iter().flatten() {
        checks.push(PrCheck {
            name: s(&st["context"]),
            state: check_state(st["state"].as_str(), None).into(),
            url: opt_s(&st["target_url"]),
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
                }
            } else {
                PrCheck {
                    name: s(&c["name"]),
                    state: check_state(c["status"].as_str(), c["conclusion"].as_str().filter(|s| !s.is_empty())).into(),
                    url: opt_s(&c["detailsUrl"]),
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
    fn new(repo: GithubRepo, token: Option<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| GitError::Network(e.to_string()))?;
        Ok(Api { client, base: repo.api_base(), repo, token })
    }

    fn require_token(&self) -> Result<()> {
        if self.token.is_some() {
            Ok(())
        } else {
            Err(GitError::Invalid(
                "a GitHub token is required (set GITHUB_TOKEN or GH_TOKEN, or install the gh CLI)".into(),
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
        let resp = req.send().await.map_err(|e| GitError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| GitError::Network(e.to_string()))?;
        if status.is_success() {
            Ok(text)
        } else {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    let mut msg = v["message"].as_str()?.to_string();
                    if let Some(errors) = v["errors"].as_array() {
                        for e in errors {
                            if let Some(m) = e["message"].as_str() {
                                msg.push_str(&format!("; {m}"));
                            }
                        }
                    }
                    Some(msg)
                })
                .unwrap_or(text);
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
}

async fn gh_json(cwd: &Path, args: &[&str]) -> Result<Value> {
    let out = GitCommand::gh(cwd).args(args).run().await?;
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
    token: Option<&str>,
) -> Result<(String, Option<u32>)> {
    if gh_available().await {
        let mut args = vec!["pr", "create", "--title", title, "--body-file", "-"];
        if let Some(b) = base {
            args.extend(["--base", b]);
        }
        if draft {
            args.push("--draft");
        }
        let out = GitCommand::gh(cwd).args(&args).stdin(body.as_bytes().to_vec()).run().await?;
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
    let api = Api::new(repo, resolve_token(token))?;
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
pub async fn view(cwd: &Path, number: Option<u32>, token: Option<&str>) -> Result<Option<PullRequest>> {
    if gh_available().await {
        const FIELDS: &str = "number,title,body,state,url,author,headRefName,baseRefName,additions,deletions,isDraft,\
statusCheckRollup,comments,reviews,commits,createdAt,mergedAt,closedAt,mergedBy";
        let num = number.map(|n| n.to_string());
        let mut args = vec!["pr", "view"];
        if let Some(n) = &num {
            args.push(n);
        }
        args.extend(["--json", FIELDS]);
        let out = GitCommand::gh(cwd).args(&args).output().await?;
        if !out.success() {
            let err = out.stderr_str();
            if err.contains("no pull requests found") || err.contains("Could not resolve to a PullRequest") {
                return Ok(None);
            }
            return Err(crate::cmd::command_error("gh", args.join(" "), cwd, &out));
        }
        let view: Value = serde_json::from_slice(&out.stdout).map_err(|e| GitError::Parse(e.to_string()))?;
        let n = view["number"].as_u64().unwrap_or(0).to_string();
        let diff = GitCommand::gh(cwd)
            .args(["pr", "diff", n.as_str(), "--color=never"])
            .run()
            .await
            .map(|o| o.stdout_str())
            .unwrap_or_default();
        let comments_path = format!("repos/{{owner}}/{{repo}}/pulls/{n}/comments?per_page=100");
        let comments = gh_json(cwd, &["api", comments_path.as_str()]).await.unwrap_or(Value::Null);
        return Ok(Some(pr_from_gh(&view, &diff, &comments)));
    }

    let repo = origin_repo(cwd).await?;
    let api = Api::new(repo.clone(), resolve_token(token))?;
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

/// Open pull requests: `(number, title, state)`.
pub async fn list(cwd: &Path, token: Option<&str>) -> Result<Vec<(u32, String, String)>> {
    if gh_available().await {
        let v = gh_json(cwd, &["pr", "list", "--json", "number,title,state,isDraft", "--limit", "100"]).await?;
        return Ok(v
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                let state = if p["isDraft"].as_bool().unwrap_or(false) {
                    "draft".to_string()
                } else {
                    s(&p["state"]).to_lowercase()
                };
                (p["number"].as_u64().unwrap_or(0) as u32, s(&p["title"]), state)
            })
            .collect());
    }
    let api = Api::new(origin_repo(cwd).await?, resolve_token(token))?;
    let v = api.get("/pulls?state=open&per_page=100").await?;
    Ok(v.as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let state = if p["draft"].as_bool().unwrap_or(false) { "draft".to_string() } else { s(&p["state"]) };
            (p["number"].as_u64().unwrap_or(0) as u32, s(&p["title"]), state)
        })
        .collect())
}

/// Submit a review (`COMMENT`, `APPROVE` or `REQUEST_CHANGES`) with inline comments.
/// Returns the review's URL.
pub async fn submit_review(
    cwd: &Path,
    number: u32,
    comments: &[ReviewComment],
    body: Option<&str>,
    event: &str,
    token: Option<&str>,
) -> Result<String> {
    let payload = review_payload(comments, body, event)?;
    if gh_available().await {
        let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}/reviews");
        let out = GitCommand::gh(cwd)
            .args(["api", "--method", "POST", path.as_str(), "--input", "-"])
            .stdin(serde_json::to_vec(&payload).map_err(|e| GitError::Parse(e.to_string()))?)
            .run()
            .await?;
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| GitError::Parse(e.to_string()))?;
        return Ok(opt_s(&v["html_url"]).unwrap_or_else(|| v["id"].to_string()));
    }
    let api = Api::new(origin_repo(cwd).await?, resolve_token(token))?;
    api.require_token()?;
    let v = api.post(&format!("/pulls/{number}/reviews"), &payload).await?;
    Ok(opt_s(&v["html_url"]).unwrap_or_else(|| v["id"].to_string()))
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
        let api = Api::new(r, Some("t".into())).unwrap();
        assert_eq!(api.url("/pulls/1"), "https://api.github.com/repos/odex-app/odex/pulls/1");
        assert!(Api::new(GithubRepo { host: "github.com".into(), owner: "o".into(), repo: "r".into() }, None)
            .unwrap()
            .require_token()
            .is_err());
    }
}
