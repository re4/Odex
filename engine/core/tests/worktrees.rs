//! Worktrees: environment selection and variables, background setup scripts, moving a local
//! thread into a worktree, retention and restore on unarchive.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::json;

use common::*;
use odex_core::api;
use odex_mock_vllm::MockReply;
use odex_protocol::*;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.email=t@odex.test", "-c", "user.name=t"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "core.autocrlf", "false"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while !f() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn thread(h: &Harness, id: &str) -> Thread {
    h.engine.thread(id).unwrap().thread()
}

async fn wait_setup(h: &Harness, id: &str) -> String {
    wait_for("setup", || thread(h, id).worktree.and_then(|w| w.setup_status).is_some_and(|s| s != "running")).await;
    thread(h, id).worktree.unwrap().setup_status.unwrap()
}

fn wt_path(h: &Harness, id: &str) -> PathBuf {
    PathBuf::from(thread(h, id).worktree.expect("worktree").path)
}

const ENVS: &str = r#"
[[environment]]
id = "a"
name = "Env A"
setup_script = "exit 3"
setup_scripts = { windows = '''Start-Sleep -Milliseconds 800; Set-Content -Path setup.txt -Value ($env:GREETING + '|' + $env:ODEX_ENVIRONMENT)''', linux = '''sleep 0.8; printf '%s|%s' "$GREETING" "$ODEX_ENVIRONMENT" > setup.txt''', macos = '''sleep 0.8; printf '%s|%s' "$GREETING" "$ODEX_ENVIRONMENT" > setup.txt''' }
env = { GREETING = "from-a" }

[[environment]]
id = "b"
name = "Env B"
setup_scripts = { windows = '''Set-Content -Path setup.txt -Value ($env:GREETING + '|' + $env:ODEX_ENVIRONMENT)''', linux = '''printf '%s|%s' "$GREETING" "$ODEX_ENVIRONMENT" > setup.txt''', macos = '''printf '%s|%s' "$GREETING" "$ODEX_ENVIRONMENT" > setup.txt''' }
env = { GREETING = "from-b" }
"#;

async fn project_with_envs(h: &Harness) -> String {
    init_repo(h.work.path());
    std::fs::create_dir_all(h.work.path().join(".odex")).unwrap();
    std::fs::write(h.work.path().join(".odex/environments.toml"), ENVS).unwrap();
    let p = api::project_add(
        &h.engine,
        ProjectAddParams {
            folders: vec![h.work.path().to_string_lossy().to_string()],
            name: None,
            primary: None,
            create: false,
        },
    )
    .await
    .unwrap();
    p.project.id
}

async fn start(h: &Harness, project: &str, env: Option<&str>) -> EResultThread {
    api::thread_start(
        &h.engine,
        ThreadStartParams {
            project_id: Some(project.into()),
            run_mode: Some(RunMode::Worktree),
            environment_id: env.map(String::from),
            permission_mode: Some(PermissionMode::FullAccess),
            ..Default::default()
        },
    )
    .await
    .map(|r| r.thread.id)
}

type EResultThread = Result<String, odex_core::EngineError>;

fn setup_out(h: &Harness, id: &str) -> String {
    std::fs::read_to_string(wt_path(h, id).join("setup.txt")).unwrap_or_default().trim().to_string()
}

#[tokio::test]
async fn environment_selection_variables_and_background_setup() {
    let h = harness(32768, "").await;
    let project = project_with_envs(&h).await;

    // no default set: the first environment (per-OS script wins over the default `exit 3`)
    let t1 = start(&h, &project, None).await.unwrap();
    assert_eq!(
        thread(&h, &t1).worktree.unwrap().setup_status.as_deref(),
        Some("running"),
        "setup runs in the background"
    );
    // a turn started now waits for the setup script, and the agent's shell sees the variables
    let cmd = if cfg!(windows) {
        "Get-Content setup.txt; Write-Output \"var=$env:GREETING\""
    } else {
        "cat setup.txt; echo; echo var=$GREETING"
    };
    h.server.push_all([MockReply::tool("shell", json!({ "command": cmd })), MockReply::text("ok")]);
    let turn = h.run(&t1, "show the setup output").await;
    assert_eq!(turn.status, TurnStatus::Completed);
    let out = h
        .sink
        .items()
        .into_iter()
        .find_map(|i| match i {
            ThreadItem::CommandExecution { output, .. } if output.contains("var=") => Some(output),
            _ => None,
        })
        .expect("command item");
    assert!(out.contains("from-a|a"), "setup finished before the command: {out}");
    assert!(out.contains("var=from-a"), "environment variables reach agent commands: {out}");
    assert_eq!(wait_setup(&h, &t1).await, "ok");
    let log = thread(&h, &t1).worktree.unwrap().setup_log.expect("setup log");
    assert!(std::fs::read_to_string(log).unwrap().contains("# ok"));

    // the project default
    api::project_update(
        &h.engine,
        ProjectUpdateParams { id: project.clone(), default_environment: Some("b".into()), ..Default::default() },
    )
    .unwrap();
    let t2 = start(&h, &project, None).await.unwrap();
    assert_eq!(wait_setup(&h, &t2).await, "ok");
    assert_eq!(setup_out(&h, &t2), "from-b|b");

    // an explicit environment wins over the default
    let t3 = start(&h, &project, Some("a")).await.unwrap();
    assert_eq!(thread(&h, &t3).environment_id.as_deref(), Some("a"));
    assert_eq!(wait_setup(&h, &t3).await, "ok");
    assert_eq!(setup_out(&h, &t3), "from-a|a");

    // "" = no environment: no setup
    let t4 = start(&h, &project, Some("")).await.unwrap();
    assert_eq!(thread(&h, &t4).worktree.unwrap().setup_status, None);

    // unknown ids are refused
    assert!(start(&h, &project, Some("nope")).await.is_err());

    // rerun setup
    std::fs::remove_file(wt_path(&h, &t2).join("setup.txt")).unwrap();
    api::worktree_setup(&h.engine, ThreadIdParams { thread_id: t2.clone() }).await.unwrap();
    assert_eq!(wait_setup(&h, &t2).await, "ok");
    assert_eq!(setup_out(&h, &t2), "from-b|b");
}

#[tokio::test]
async fn failed_setup_is_reported() {
    let h = harness(32768, "").await;
    init_repo(h.work.path());
    std::fs::create_dir_all(h.work.path().join(".odex")).unwrap();
    std::fs::write(
        h.work.path().join(".odex/environments.toml"),
        "[[environment]]\nid = \"x\"\nsetup_script = \"echo broken-setup; exit 7\"\n",
    )
    .unwrap();
    let p = api::project_add(
        &h.engine,
        ProjectAddParams {
            folders: vec![h.work.path().to_string_lossy().to_string()],
            name: None,
            primary: None,
            create: false,
        },
    )
    .await
    .unwrap();
    let t = start(&h, &p.project.id, None).await.unwrap();
    let status = wait_setup(&h, &t).await;
    assert!(status.starts_with("failed (exit 7)"), "{status}");
    assert!(status.contains("broken-setup"), "{status}");
}

#[tokio::test]
async fn move_local_thread_into_worktree() {
    let h = harness(32768, "").await;
    init_repo(h.work.path());
    std::fs::write(h.work.path().join("README.md"), "changed locally\n").unwrap();
    std::fs::write(h.work.path().join("notes.txt"), "new file\n").unwrap();
    let tid = h.thread(PermissionMode::Auto).await;
    assert!(thread(&h, &tid).worktree.is_none());

    let r =
        api::worktree_from_local(&h.engine, WorktreeFromLocalParams { thread_id: tid.clone(), ..Default::default() })
            .await
            .unwrap();
    assert_eq!(r.moved_files, 2);
    assert_eq!(r.thread.run_mode, RunMode::Worktree);
    let wt = PathBuf::from(&r.thread.worktree.as_ref().unwrap().path);
    assert!(r.thread.cwd.starts_with(&r.thread.worktree.as_ref().unwrap().path));
    assert_eq!(std::fs::read_to_string(wt.join("README.md")).unwrap(), "changed locally\n");
    assert_eq!(std::fs::read_to_string(wt.join("notes.txt")).unwrap(), "new file\n");
    // the local checkout is clean; the stash keeps a copy
    assert_eq!(git(h.work.path(), &["status", "--porcelain"]).trim(), "");
    assert!(git(h.work.path(), &["stash", "list"]).contains(r.stash.as_deref().unwrap()));
    // a worktree thread cannot be moved again
    assert!(api::worktree_from_local(&h.engine, WorktreeFromLocalParams { thread_id: tid, ..Default::default() })
        .await
        .is_err());
}

#[tokio::test]
async fn retention_prunes_archived_worktrees_and_unarchive_restores() {
    let h = harness(32768, "[worktrees]\nkeep = 1\n").await;
    init_repo(h.work.path());
    let mut ids = Vec::new();
    for _ in 0..3 {
        let t = api::thread_start(
            &h.engine,
            ThreadStartParams {
                cwd: Some(h.work.path().to_string_lossy().to_string()),
                run_mode: Some(RunMode::Worktree),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        ids.push(t.thread.id);
        // distinct updated_at for a stable "oldest first"
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let paths: Vec<PathBuf> = ids.iter().map(|id| wt_path(&h, id)).collect();
    assert!(paths.iter().all(|p| p.is_dir()), "active worktrees are never pruned");
    std::fs::write(paths[0].join("wip.txt"), "unsaved work\n").unwrap();

    api::thread_archive(&h.engine, ThreadArchiveParams { thread_id: ids[0].clone(), remove_worktree: false })
        .await
        .unwrap();
    wait_for("first prune", || !paths[0].exists()).await;
    let refs = git(h.work.path(), &["for-each-ref", "--format=%(refname)", "refs/odex/archived"]);
    assert!(refs.contains(&format!("refs/odex/archived/{}", ids[0])), "{refs}");

    api::thread_archive(&h.engine, ThreadArchiveParams { thread_id: ids[1].clone(), remove_worktree: false })
        .await
        .unwrap();
    wait_for("second prune", || !paths[1].exists()).await;
    assert!(paths[2].is_dir());
    let list = api::worktree_list(&h.engine).await.unwrap();
    assert_eq!(list.thread_ids, vec![ids[2].clone()]);

    // manual prune with nothing to do
    assert!(api::worktree_prune(&h.engine).await.unwrap().removed.is_empty());

    // unarchive brings the worktree back with its uncommitted work
    let t = api::thread_unarchive(&h.engine, ThreadIdParams { thread_id: ids[0].clone() }).await.unwrap();
    assert_eq!(t.thread.run_mode, RunMode::Worktree);
    assert!(paths[0].is_dir());
    assert_eq!(std::fs::read_to_string(paths[0].join("wip.txt")).unwrap(), "unsaved work\n");
}

#[tokio::test]
async fn auto_cleanup_off_keeps_worktrees() {
    let h = harness(32768, "[worktrees]\nkeep = 0\nauto_cleanup = false\n").await;
    init_repo(h.work.path());
    let t = api::thread_start(
        &h.engine,
        ThreadStartParams {
            cwd: Some(h.work.path().to_string_lossy().to_string()),
            run_mode: Some(RunMode::Worktree),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let id = t.thread.id;
    let path = wt_path(&h, &id);
    api::thread_archive(&h.engine, ThreadArchiveParams { thread_id: id.clone(), remove_worktree: false })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(path.is_dir(), "auto cleanup is off");
    // an explicit prune still applies `keep`
    assert_eq!(api::worktree_prune(&h.engine).await.unwrap().removed, vec![id]);
    assert!(!path.exists());
}
