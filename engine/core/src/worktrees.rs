//! Thread worktrees and project environments.
//!
//! - `.odex/environments.toml` / `.odex/actions.toml`: load and save (per-OS entries survive an
//!   in-app save).
//! - Which environment a thread uses ([`thread_environment`]) and its variables, which are added
//!   to the setup script, the agent's commands and `!cmd` commands.
//! - Worktree creation; the environment's setup script runs in the background
//!   ([`start_setup`]) and turns wait for it ([`wait_for_setup`]).
//! - Moving a local thread and its uncommitted changes into a worktree ([`from_local`]).
//! - Retention: `[worktrees] keep` / `auto_cleanup` ([`prune`]), and restoring a removed
//!   worktree when its thread is unarchived ([`restore_worktree`]).

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use once_cell::sync::Lazy;
use tokio_util::sync::CancellationToken;

use odex_git::Git;
use odex_protocol::*;

use crate::engine::{EResult, Engine, EngineError};
use crate::thread::ThreadRt;

/// Default `[worktrees] keep`.
pub const DEFAULT_KEEP: u32 = 15;
/// Setup scripts are stopped after this long.
const SETUP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::new(jsonrpc::error_codes::INVALID_PARAMS, msg)
}

fn git_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::new(jsonrpc::error_codes::GIT_ERROR, e.to_string())
}

// ================================================================ project files

/// `[[environment]]` entries of `<root>/.odex/environments.toml`. `setup_script` is the default
/// script and `setup_scripts` the per-OS ones, as written in the file.
pub fn load_environments(root: &Path) -> Vec<Environment> {
    #[derive(serde::Deserialize)]
    struct E {
        id: String,
        name: Option<String>,
        setup_script: Option<String>,
        #[serde(default)]
        setup_scripts: Option<PerOs>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    }
    #[derive(serde::Deserialize, Default)]
    struct F {
        #[serde(default)]
        environment: Vec<E>,
    }
    let path = odex_config::project_dir(root).join("environments.toml");
    let Ok(text) = std::fs::read_to_string(&path) else { return vec![] };
    match toml::from_str::<F>(&text) {
        Ok(f) => f
            .environment
            .into_iter()
            .map(|e| Environment {
                name: e.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| e.id.clone()),
                setup_script: e.setup_script,
                setup_scripts: e.setup_scripts.filter(|s| !s.is_empty()),
                id: e.id,
                env: e.env,
            })
            .collect(),
        Err(err) => {
            tracing::warn!("{}: {err}", path.display());
            vec![]
        }
    }
}

/// Write `<root>/.odex/environments.toml`. An environment sent without `setup_scripts` (an
/// older client) keeps the per-OS scripts the file already has for its id.
pub fn save_environments(root: &Path, envs: &[Environment]) -> std::io::Result<()> {
    #[derive(serde::Serialize)]
    struct E<'a> {
        id: &'a str,
        name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        setup_script: Option<&'a str>,
        // tables last
        #[serde(skip_serializing_if = "Option::is_none")]
        setup_scripts: Option<PerOs>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        env: &'a BTreeMap<String, String>,
    }
    #[derive(serde::Serialize)]
    struct F<'a> {
        environment: Vec<E<'a>>,
    }
    let previous = load_environments(root);
    let f = F {
        environment: envs
            .iter()
            .map(|e| E {
                id: &e.id,
                name: &e.name,
                setup_script: e.setup_script.as_deref().filter(|s| !s.trim().is_empty()),
                setup_scripts: match &e.setup_scripts {
                    Some(s) => Some(s.clone()),
                    None => previous.iter().find(|p| p.id == e.id).and_then(|p| p.setup_scripts.clone()),
                }
                .map(trim_per_os)
                .filter(|s| !s.is_empty()),
                env: &e.env,
            })
            .collect(),
    };
    std::fs::create_dir_all(odex_config::project_dir(root))?;
    let text = toml::to_string_pretty(&f).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(odex_config::project_dir(root).join("environments.toml"), text)
}

/// `[[action]]` entries of `<root>/.odex/actions.toml`.
pub fn load_actions(root: &Path) -> Vec<ProjectAction> {
    #[derive(serde::Deserialize, Default)]
    struct F {
        #[serde(default)]
        action: Vec<ProjectAction>,
    }
    let path = odex_config::project_dir(root).join("actions.toml");
    let Ok(text) = std::fs::read_to_string(&path) else { return vec![] };
    match toml::from_str::<F>(&text) {
        Ok(f) => f
            .action
            .into_iter()
            .map(|mut a| {
                a.commands = a.commands.filter(|c| !c.is_empty());
                a
            })
            .collect(),
        Err(err) => {
            tracing::warn!("{}: {err}", path.display());
            vec![]
        }
    }
}

/// Write `<root>/.odex/actions.toml`. An action sent without `commands` keeps the per-OS
/// commands the file already has for its id.
pub fn save_actions(root: &Path, actions: &[ProjectAction]) -> std::io::Result<()> {
    #[derive(serde::Serialize)]
    struct F<'a> {
        action: &'a [ProjectAction],
    }
    let previous = load_actions(root);
    let actions: Vec<ProjectAction> = actions
        .iter()
        .map(|a| {
            let mut a = a.clone();
            a.commands = match a.commands.take() {
                Some(c) => Some(c),
                None => previous.iter().find(|p| p.id == a.id).and_then(|p| p.commands.clone()),
            }
            .map(trim_per_os)
            .filter(|c| !c.is_empty());
            a.cwd = a.cwd.filter(|c| !c.trim().is_empty());
            a.open_url = a.open_url.filter(|u| !u.trim().is_empty());
            a
        })
        .collect();
    std::fs::create_dir_all(odex_config::project_dir(root))?;
    let text = toml::to_string_pretty(&F { action: &actions }).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(odex_config::project_dir(root).join("actions.toml"), text)
}

fn trim_per_os(p: PerOs) -> PerOs {
    let keep = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    PerOs { windows: keep(p.windows), macos: keep(p.macos), linux: keep(p.linux) }
}

// ================================================================ environments

/// Folder whose `.odex/` defines a thread's environments: the project's primary folder, else
/// the worktree's main checkout, else the thread's cwd.
pub fn env_root(engine: &Engine, t: &Thread) -> PathBuf {
    if let Some(pid) = &t.project_id {
        if let Ok(p) = engine.project(pid) {
            return PathBuf::from(p.primary_folder());
        }
    }
    if let Some(wt) = &t.worktree {
        return PathBuf::from(&wt.repo_root);
    }
    PathBuf::from(&t.cwd)
}

/// Pick an environment: an explicit id (`""` = none), else the project default, else the first.
/// An explicit id that no longer exists falls back to the default.
pub fn pick_environment(envs: &[Environment], wanted: Option<&str>, default: Option<&str>) -> Option<Environment> {
    let fallback = || {
        default
            .filter(|d| !d.is_empty())
            .and_then(|d| envs.iter().find(|e| e.id == d))
            .or_else(|| envs.first())
            .cloned()
    };
    match wanted {
        Some("") => None,
        Some(id) => envs.iter().find(|e| e.id == id).cloned().or_else(fallback),
        None => fallback(),
    }
}

/// The environment a thread uses (trusted projects only).
pub fn thread_environment(engine: &Engine, t: &Thread) -> Option<Environment> {
    let root = env_root(engine, t);
    if !engine.is_trusted(&root) {
        return None;
    }
    let envs = load_environments(&root);
    if envs.is_empty() {
        return None;
    }
    let default = t.project_id.as_deref().and_then(|pid| engine.project(pid).ok()).and_then(|p| p.default_environment);
    pick_environment(&envs, t.environment_id.as_deref(), default.as_deref())
}

/// Variables of the thread's environment, added to its setup script and commands.
pub fn thread_env_vars(engine: &Engine, t: &Thread) -> HashMap<String, String> {
    thread_environment(engine, t).map(|e| e.env.into_iter().collect()).unwrap_or_default()
}

/// Check an environment id given to `thread/start` / `worktree/fromLocal` against the project.
pub fn validate_environment_id(engine: &Engine, root: &Path, id: Option<&str>) -> EResult<()> {
    let Some(id) = id.filter(|i| !i.is_empty()) else { return Ok(()) };
    if !engine.is_trusted(root) {
        return Err(bad("the project is not trusted, so its environments are not available"));
    }
    if !load_environments(root).iter().any(|e| e.id == id) {
        return Err(bad(format!("unknown environment `{id}` (see .odex/environments.toml)")));
    }
    Ok(())
}

// ================================================================ worktrees

/// How a new worktree starts.
pub enum Carry<'a> {
    /// On a new branch from `base` (default: the current branch).
    Base(Option<&'a str>),
    /// From `HEAD`, with a copy of the checkout's uncommitted changes.
    CurrentWithChanges,
    /// From `HEAD`, moving the checkout's uncommitted changes (stashed locally unless `keep_local`).
    Move { keep_local: bool },
}

/// Create the thread's worktree, switch the thread to it and start the environment's setup
/// script in the background.
pub async fn create_worktree(
    engine: &Engine,
    rt: &Arc<ThreadRt>,
    carry: Carry<'_>,
) -> EResult<Option<odex_git::MovedChanges>> {
    let t = rt.thread();
    let root = engine.thread_root(&t);
    let g = Git::new(&root);
    if !g.is_repo().await {
        return Err(bad(format!("{} is not a git repository; worktree mode needs git", root.display())));
    }
    let repo_root = g.repo_root().await.map_err(git_err)?;
    let s = engine.thread_settings(&t);
    let project_slug = crate::api::slug(
        &repo_root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "project".into()),
    );
    let short = &t.id[t.id.len().saturating_sub(8)..];
    let mut path = s.worktrees_dir.join(&project_slug).join(short);
    let prefix = crate::api::branch_prefix(s.raw.git.as_ref().and_then(|g| g.branch_prefix.as_deref()));
    let stem = if let Some(n) = &t.name { format!("{}-{short}", crate::api::slug(n)) } else { short.to_string() };
    let mut branch = format!("{prefix}{stem}");
    // a thread that had a worktree before keeps its old branch; pick a fresh name
    let mut n = 2;
    while g.branch_exists(&branch).await.unwrap_or(false) || path.exists() {
        branch = format!("{prefix}{stem}-{n}");
        path = s.worktrees_dir.join(&project_slug).join(format!("{short}-{n}"));
        n += 1;
    }
    let current = g.current_branch().await.ok().flatten();
    let mut moved = None;
    let base_branch = match carry {
        Carry::Base(base) => {
            let base_branch = base.map(String::from).or(current.clone()).unwrap_or_else(|| "HEAD".into());
            g.worktree_add(&path, &branch, &base_branch).await.map_err(git_err)?;
            base_branch
        }
        Carry::CurrentWithChanges => {
            g.worktree_add_with_changes(&path, &branch).await.map_err(git_err)?;
            current.clone().unwrap_or_else(|| "HEAD".into())
        }
        Carry::Move { keep_local } => {
            moved = Some(g.worktree_move_changes(&path, &branch, keep_local).await.map_err(git_err)?);
            current.clone().unwrap_or_else(|| "HEAD".into())
        }
    };
    let _ = g.copy_worktree_include(&repo_root, &path).await;
    let base_commit = Git::new(&path).head_sha().await.ok().flatten();
    // keep the cwd's relative position inside the repo
    let rel = PathBuf::from(&t.cwd).strip_prefix(&repo_root).map(|r| r.to_path_buf()).unwrap_or_default();
    let cwd = path.join(rel);
    let wt = WorktreeInfo {
        path: path.to_string_lossy().to_string(),
        branch: branch.clone(),
        base_branch: Some(base_branch),
        base_commit,
        repo_root: repo_root.to_string_lossy().to_string(),
        setup_status: None,
        setup_log: None,
    };
    engine.update_thread(rt, |t| {
        t.run_mode = RunMode::Worktree;
        t.worktree = Some(wt.clone());
        t.branch = Some(branch.clone());
        t.cwd = cwd.to_string_lossy().to_string();
    });
    if let Err(e) = start_setup(engine, rt) {
        tracing::warn!("setup script not started: {}", e.message);
    }
    spawn_prune(engine);
    Ok(moved)
}

/// Setup scripts running now, by thread id.
static SETUPS: Lazy<Mutex<HashMap<String, CancellationToken>>> = Lazy::new(Default::default);

/// Run the thread's environment setup script in its worktree, in the background. Returns
/// whether a script started (false when the environment has none for this OS).
pub fn start_setup(engine: &Engine, rt: &Arc<ThreadRt>) -> EResult<bool> {
    let t = rt.thread();
    let wt = t.worktree.clone().ok_or_else(|| bad("this thread does not run in a worktree"))?;
    if !Path::new(&wt.path).is_dir() {
        return Err(bad(format!("the worktree folder {} is missing", wt.path)));
    }
    let Some(env) = thread_environment(engine, &t) else { return Ok(false) };
    let Some(script) = env.effective_setup_script().map(String::from) else { return Ok(false) };
    let cancel = CancellationToken::new();
    {
        let mut running = SETUPS.lock().unwrap();
        if running.contains_key(&t.id) {
            return Err(bad("the setup script is already running"));
        }
        running.insert(t.id.clone(), cancel.clone());
    }
    let logs = engine.home.logs_dir();
    let _ = std::fs::create_dir_all(&logs);
    let log = logs.join(format!("setup-{}.log", t.id));
    let log_str = log.to_string_lossy().to_string();
    engine.update_thread(rt, |t| {
        if let Some(w) = &mut t.worktree {
            w.setup_status = Some("running".into());
            w.setup_log = Some(log_str.clone());
        }
    });
    let shell = engine.thread_settings(&t).default_shell;
    let e2 = engine.clone();
    let rt2 = rt.clone();
    tokio::spawn(async move {
        let status = run_setup(&shell, &script, &wt, &env, &log, &cancel).await;
        SETUPS.lock().unwrap().remove(&rt2.id);
        e2.update_thread(&rt2, |t| {
            if let Some(w) = t.worktree.as_mut().filter(|w| w.path == wt.path) {
                w.setup_status = Some(status);
            }
        });
    });
    Ok(true)
}

async fn run_setup(
    shell: &str,
    script: &str,
    wt: &WorktreeInfo,
    env: &Environment,
    log: &Path,
    cancel: &CancellationToken,
) -> String {
    let file = std::fs::File::create(log).ok().map(|f| Arc::new(Mutex::new(f)));
    if let Some(f) = &file {
        let _ = writeln!(f.lock().unwrap(), "# environment {} ({}) in {}\n$ {script}\n", env.name, env.id, wt.path);
    }
    let mut req = odex_sandbox::ExecRequest::new(odex_sandbox::shell_argv(shell, script), PathBuf::from(&wt.path));
    req.policy = odex_sandbox::SandboxPolicy::FullAccess;
    req.timeout = Some(SETUP_TIMEOUT);
    req.env.extend(env.env.clone());
    req.env.insert("ODEX_WORKTREE_PATH".into(), wt.path.clone());
    req.env.insert("ODEX_SOURCE_TREE_PATH".into(), wt.repo_root.clone());
    req.env.insert("ODEX_ENVIRONMENT".into(), env.id.clone());
    let sink = file.clone();
    let out = odex_sandbox::exec(
        req,
        move |c| {
            if let Some(f) = &sink {
                let _ = f.lock().unwrap().write_all(c.bytes());
            }
        },
        cancel.clone(),
    )
    .await;
    let status = match out {
        Ok(o) if o.exit_code == Some(0) && !o.timed_out => "ok".to_string(),
        _ if cancel.is_cancelled() => "failed: cancelled".to_string(),
        Ok(o) if o.timed_out => format!("failed: timed out after {} minutes", SETUP_TIMEOUT.as_secs() / 60),
        Ok(o) => {
            let tail: String =
                o.aggregated.trim_end().chars().rev().take(500).collect::<String>().chars().rev().collect();
            format!("failed (exit {}): {tail}", o.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()))
        }
        Err(e) => format!("failed: {e}"),
    };
    if let Some(f) = &file {
        let _ = writeln!(f.lock().unwrap(), "\n# {status}");
    }
    status
}

/// Stop a thread's running setup script (archive, worktree removal).
pub fn cancel_setup(thread_id: &str) {
    if let Some(c) = SETUPS.lock().unwrap().remove(thread_id) {
        c.cancel();
    }
}

/// Wait while the thread's setup script runs, so the agent starts in a ready worktree. A
/// `running` status left over from a previous engine process is marked failed.
pub async fn wait_for_setup(engine: &Engine, rt: &ThreadRt, cancel: &CancellationToken) {
    loop {
        let running = rt.thread().worktree.as_ref().and_then(|w| w.setup_status.clone()).as_deref() == Some("running");
        if !running {
            return;
        }
        if !SETUPS.lock().unwrap().contains_key(&rt.id) {
            engine.update_thread(rt, |t| {
                if let Some(w) = &mut t.worktree {
                    w.setup_status = Some("failed: interrupted (Odex stopped while the setup script ran)".into());
                }
            });
            return;
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

/// `worktree/fromLocal`.
pub async fn from_local(engine: &Engine, p: WorktreeFromLocalParams) -> EResult<WorktreeFromLocalResponse> {
    let rt = engine.thread(&p.thread_id)?;
    if rt.is_running() {
        return Err(EngineError::new(
            jsonrpc::error_codes::THREAD_BUSY,
            "stop the running turn before moving the thread",
        ));
    }
    let t = rt.thread();
    if t.worktree.is_some() {
        return Err(bad("this thread already runs in a worktree"));
    }
    if let Some(id) = &p.environment_id {
        validate_environment_id(engine, &env_root(engine, &t), Some(id))?;
        engine.update_thread(&rt, |t| t.environment_id = Some(id.clone()));
    }
    let moved = create_worktree(engine, &rt, Carry::Move { keep_local: p.keep_local }).await?;
    let thread = rt.thread();
    let wt = thread.worktree.clone().expect("worktree just created");
    let (files, stash, stash_error) = moved.map(|m| (m.files, m.stash, m.stash_error)).unwrap_or_default();
    let mut message = if files == 0 {
        format!("The thread now runs in a worktree on {} (there were no uncommitted changes to move).", wt.branch)
    } else if p.keep_local {
        format!("Copied {files} changed file(s) into a worktree on {}; the local checkout still has them.", wt.branch)
    } else {
        format!(
            "Moved {files} changed file(s) into a worktree on {}. The local checkout is clean; a copy is kept in git stash (\"{}\").",
            wt.branch,
            stash.clone().unwrap_or_default()
        )
    };
    if let Some(e) = stash_error {
        message = format!(
            "Copied {files} changed file(s) into a worktree on {}, but the local checkout could not be cleaned ({e}); it still has them.",
            wt.branch
        );
    }
    Ok(WorktreeFromLocalResponse { thread, moved_files: files as u32, stash, message })
}

// ================================================================ retention

static PRUNING: Lazy<tokio::sync::Mutex<()>> = Lazy::new(Default::default);

/// `[worktrees]` settings: (keep, auto_cleanup).
pub fn retention(engine: &Engine) -> (usize, bool) {
    let w = engine.user_settings().raw.worktrees.unwrap_or_default();
    (w.keep.unwrap_or(DEFAULT_KEEP) as usize, w.auto_cleanup.unwrap_or(true))
}

/// Apply the retention policy in the background when `auto_cleanup` is on.
pub fn spawn_prune(engine: &Engine) {
    let (keep, auto) = retention(engine);
    if !auto {
        return;
    }
    let e2 = engine.clone();
    tokio::spawn(async move {
        let removed = prune(&e2, keep).await;
        if !removed.is_empty() {
            tracing::info!("worktree retention removed {} worktree(s)", removed.len());
        }
    });
}

/// Keep at most `keep` thread worktrees on disk: remove the oldest worktrees of archived
/// threads (least recently updated first) after snapshotting them to
/// `refs/odex/archived/<thread>`. Worktrees of active threads are never removed. Returns the
/// ids of the threads whose worktree was removed (their thread keeps the worktree info, so
/// unarchiving restores it).
pub async fn prune(engine: &Engine, keep: usize) -> Vec<String> {
    let _guard = PRUNING.lock().await;
    let mut live: Vec<Thread> = Vec::new();
    for archived in [false, true] {
        let params = ThreadListParams { archived: Some(archived), limit: Some(5000), ..Default::default() };
        if let Ok(list) = engine.store.list_threads(&params) {
            for t in list {
                let t = engine.loaded(&t.id).map(|rt| rt.thread()).unwrap_or(t);
                if t.worktree.as_ref().is_some_and(|w| Path::new(&w.path).is_dir()) {
                    live.push(t);
                }
            }
        }
    }
    if live.len() <= keep {
        return vec![];
    }
    let mut candidates: Vec<&Thread> =
        live.iter().filter(|t| t.archived && t.status != ThreadStatus::Running).collect();
    candidates.sort_by_key(|t| t.updated_at);
    let mut removed = Vec::new();
    for t in candidates.into_iter().take(live.len() - keep) {
        let wt = t.worktree.as_ref().expect("filtered on worktree");
        cancel_setup(&t.id);
        let snap = Git::new(&wt.path).snapshot(&archived_ref(&t.id), "odex: worktree removed by retention").await;
        if let Err(e) = snap {
            // without a snapshot uncommitted work would be lost: keep this one
            tracing::warn!("retention: not removing {} (snapshot failed: {e})", wt.path);
            continue;
        }
        match Git::new(&wt.repo_root).worktree_remove(Path::new(&wt.path), true).await {
            Ok(()) => removed.push(t.id.clone()),
            Err(e) => tracing::warn!("retention: could not remove {}: {e}", wt.path),
        }
    }
    removed
}

/// Hidden ref holding the last state of a removed worktree.
pub fn archived_ref(thread_id: &str) -> String {
    format!("refs/odex/archived/{thread_id}")
}

/// Re-create a thread's removed worktree (archive or retention removed it) from its branch and
/// restore the uncommitted work from its snapshot. When that is impossible the thread falls back
/// to the local checkout. Returns a note for the user, if any.
pub async fn restore_worktree(engine: &Engine, rt: &Arc<ThreadRt>) -> Option<String> {
    let t = rt.thread();
    let wt = t.worktree.clone()?;
    if Path::new(&wt.path).exists() {
        return None;
    }
    let g = Git::new(&wt.repo_root);
    if !g.is_repo().await {
        return Some(format!("the checkout {} is gone; the worktree was not restored", wt.repo_root));
    }
    match g.worktree_add_existing(Path::new(&wt.path), &wt.branch).await {
        Ok(()) => {
            let wg = Git::new(&wt.path);
            let snap = archived_ref(&t.id);
            if wg.list_refs("refs/odex/archived").await.map(|r| r.contains(&snap)).unwrap_or(false) {
                if let Err(e) = wg.restore_snapshot(&snap).await {
                    return Some(format!("worktree restored, but its uncommitted changes were not: {e}"));
                }
            }
            let _ = g.copy_worktree_include(Path::new(&wt.repo_root), Path::new(&wt.path)).await;
            spawn_prune(engine);
            None
        }
        Err(e) => {
            let note = format!(
                "could not restore the worktree on {} ({e}); the thread continues in the local checkout",
                wt.branch
            );
            engine.update_thread(rt, |t| {
                t.worktree = None;
                t.run_mode = RunMode::Local;
                t.cwd = wt.repo_root.clone();
                t.last_error = Some(note.clone());
            });
            Some(note)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: &str) -> Environment {
        Environment { id: id.into(), name: id.into(), setup_script: None, setup_scripts: None, env: Default::default() }
    }

    #[test]
    fn picks_explicit_then_default_then_first() {
        let envs = vec![env("a"), env("b"), env("c")];
        let id = |e: Option<Environment>| e.map(|e| e.id);
        assert_eq!(id(pick_environment(&envs, Some("b"), Some("c"))), Some("b".into()));
        assert_eq!(id(pick_environment(&envs, None, Some("c"))), Some("c".into()));
        assert_eq!(id(pick_environment(&envs, None, None)), Some("a".into()));
        assert_eq!(id(pick_environment(&envs, None, Some(""))), Some("a".into()));
        assert_eq!(id(pick_environment(&envs, Some(""), Some("c"))), None, "explicit none");
        assert_eq!(id(pick_environment(&envs, Some("gone"), Some("c"))), Some("c".into()));
        assert_eq!(id(pick_environment(&[], None, None)), None);
    }

    #[test]
    fn per_os_scripts_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".odex")).unwrap();
        std::fs::write(
            dir.path().join(".odex/environments.toml"),
            "[[environment]]\nid = \"deps\"\nname = \"Deps\"\nsetup_script = \"make\"\nsetup_scripts = { windows = \"nmake\", linux = \"make linux\" }\nenv = { A = \"1\" }\n",
        )
        .unwrap();
        let mut envs = load_environments(dir.path());
        assert_eq!(envs[0].setup_scripts.as_ref().unwrap().windows.as_deref(), Some("nmake"));
        // an older client that does not know setup_scripts
        envs[0].setup_scripts = None;
        envs[0].name = "Dependencies".into();
        save_environments(dir.path(), &envs).unwrap();
        let again = load_environments(dir.path());
        assert_eq!(again[0].name, "Dependencies");
        assert_eq!(again[0].setup_script.as_deref(), Some("make"));
        let s = again[0].setup_scripts.clone().unwrap();
        assert_eq!(
            (s.windows.as_deref(), s.macos.as_deref(), s.linux.as_deref()),
            (Some("nmake"), None, Some("make linux"))
        );
        assert_eq!(again[0].env.get("A").map(String::as_str), Some("1"));
        let expected = if cfg!(windows) {
            "nmake"
        } else if cfg!(target_os = "macos") {
            "make"
        } else {
            "make linux"
        };
        assert_eq!(again[0].effective_setup_script(), Some(expected));
        // explicit edit: clear windows, set macos
        let mut envs = again;
        envs[0].setup_scripts =
            Some(PerOs { windows: Some(" ".into()), macos: Some("brew bundle".into()), linux: None });
        save_environments(dir.path(), &envs).unwrap();
        let s = load_environments(dir.path())[0].setup_scripts.clone().unwrap();
        assert_eq!((s.windows, s.macos.as_deref(), s.linux), (None, Some("brew bundle"), None));
    }

    #[test]
    fn per_os_action_commands_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let a = ProjectAction {
            id: "dev".into(),
            name: "Dev".into(),
            command: "npm run dev".into(),
            cwd: Some("".into()),
            icon: Some("server".into()),
            open_url: None,
            commands: Some(PerOs { windows: Some("npm.cmd run dev".into()), ..Default::default() }),
        };
        save_actions(dir.path(), std::slice::from_ref(&a)).unwrap();
        let text = std::fs::read_to_string(dir.path().join(".odex/actions.toml")).unwrap();
        assert!(text.contains("windows = \"npm.cmd run dev\""), "{text}");
        let back = load_actions(dir.path());
        assert_eq!(back[0].commands.as_ref().unwrap().windows.as_deref(), Some("npm.cmd run dev"));
        assert_eq!(back[0].cwd, None);
        // saved again without commands (older client): kept
        let mut older = back[0].clone();
        older.commands = None;
        save_actions(dir.path(), &[older]).unwrap();
        assert!(load_actions(dir.path())[0].commands.is_some());
    }
}
