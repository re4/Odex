use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, Instant};

use odex_hooks::{HookDef, HookEvent, HookRegistry, DEFAULT_TIMEOUT};
use serde_json::json;
use tempfile::TempDir;

/// Platform shell snippets (PowerShell on Windows, sh elsewhere).
#[cfg(windows)]
mod snippet {
    pub fn print(text: &str) -> String {
        format!("Write-Output '{}'", text.replace('\'', "''"))
    }
    pub fn fail(code: i32, stderr: &str) -> String {
        format!("[Console]::Error.WriteLine('{stderr}'); exit {code}")
    }
    pub fn sleep(secs: u32) -> String {
        format!("Start-Sleep -Seconds {secs}")
    }
    pub fn touch(file: &str) -> String {
        format!("Set-Content -Path '{file}' -Value x")
    }
    /// Print `seen:` followed by the raw stdin payload.
    pub fn echo_stdin() -> String {
        "Write-Output ('seen:' + [Console]::In.ReadToEnd().Trim())".to_string()
    }
    pub fn print_env() -> String {
        "Write-Output ('event=' + $env:ODEX_HOOK_EVENT)".to_string()
    }
    pub fn run_script(path: &str) -> String {
        format!("& ./{path}")
    }
}

#[cfg(not(windows))]
mod snippet {
    pub fn print(text: &str) -> String {
        format!("printf '%s\\n' '{}'", text.replace('\'', "'\\''"))
    }
    pub fn fail(code: i32, stderr: &str) -> String {
        format!("echo '{stderr}' >&2; exit {code}")
    }
    pub fn sleep(secs: u32) -> String {
        format!("sleep {secs}")
    }
    pub fn touch(file: &str) -> String {
        format!("echo x > '{file}'")
    }
    pub fn echo_stdin() -> String {
        "printf 'seen:%s\\n' \"$(cat)\"".to_string()
    }
    pub fn print_env() -> String {
        "echo \"event=$ODEX_HOOK_EVENT\"".to_string()
    }
    pub fn run_script(path: &str) -> String {
        format!("sh ./{path}")
    }
}

fn def(event: HookEvent, command: String, name: &str, cwd: &Path) -> HookDef {
    HookDef {
        event,
        command,
        matcher: None,
        timeout: DEFAULT_TIMEOUT,
        name: Some(name.to_string()),
        source: "user".to_string(),
        cwd: cwd.to_path_buf(),
    }
}

fn trusted_registry(defs: Vec<HookDef>, dir: &Path) -> HookRegistry {
    let mut registry = HookRegistry::new(defs, dir.join("trusted_hooks.json"));
    for info in registry.list() {
        registry.set_trust(&info.id, &info.hash, true).unwrap();
    }
    registry
}

#[tokio::test]
async fn untrusted_hooks_are_skipped() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let registry = HookRegistry::new(
        vec![def(HookEvent::SessionStart, snippet::touch("ran.txt"), "toucher", cwd)],
        cwd.join("trust.json"),
    );
    let outcome = registry.run(HookEvent::SessionStart, json!({}), None).await;
    assert_eq!(outcome.skipped_untrusted, vec!["toucher"]);
    assert!(outcome.ran.is_empty());
    assert!(!cwd.join("ran.txt").exists());
}

#[tokio::test]
async fn trusted_hook_runs_in_cwd_and_returns_context() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let registry = trusted_registry(
        vec![
            def(HookEvent::UserPromptSubmit, snippet::touch("ran.txt"), "toucher", cwd),
            def(HookEvent::UserPromptSubmit, snippet::print("remember the tests"), "context", cwd),
            def(HookEvent::UserPromptSubmit, snippet::print_env(), "env", cwd),
            def(HookEvent::Stop, snippet::print("other event"), "stop", cwd),
        ],
        cwd,
    );
    let outcome = registry.run(HookEvent::UserPromptSubmit, json!({"prompt": "hi"}), None).await;
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert_eq!(outcome.ran, vec!["toucher", "context", "env"]);
    assert!(cwd.join("ran.txt").exists());
    assert_eq!(outcome.additional_context, vec!["remember the tests", "event=userPromptSubmit"]);
    assert!(!outcome.blocked);
}

#[tokio::test]
async fn exit_code_two_blocks_and_stops_the_chain() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let registry = trusted_registry(
        vec![
            def(HookEvent::PreToolUse, snippet::fail(2, "rm is not allowed here"), "guard", cwd),
            def(HookEvent::PreToolUse, snippet::touch("after.txt"), "after", cwd),
        ],
        cwd,
    );
    let outcome = registry.run(HookEvent::PreToolUse, json!({"tool_input": {}}), Some("shell")).await;
    assert!(outcome.blocked);
    assert_eq!(outcome.reason.as_deref(), Some("rm is not allowed here"));
    assert_eq!(outcome.ran, vec!["guard"]);
    assert!(!cwd.join("after.txt").exists());
}

#[tokio::test]
async fn json_decision_block() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let registry = trusted_registry(
        vec![def(
            HookEvent::PreToolUse,
            snippet::print(r#"{"decision":"block","reason":"policy says no","additional_context":"see docs"}"#),
            "json",
            cwd,
        )],
        cwd,
    );
    let outcome = registry.run(HookEvent::PreToolUse, json!({}), Some("apply_patch")).await;
    assert!(outcome.blocked, "{outcome:?}");
    assert_eq!(outcome.reason.as_deref(), Some("policy says no"));
    assert_eq!(outcome.additional_context, vec!["see docs"]);
}

#[tokio::test]
async fn modified_input_feeds_the_next_hook() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let registry = trusted_registry(
        vec![
            def(
                HookEvent::PreToolUse,
                snippet::print(r#"{"decision":"allow","modified_input":{"x":1}}"#),
                "rewrite",
                cwd,
            ),
            def(HookEvent::PreToolUse, snippet::echo_stdin(), "observer", cwd),
        ],
        cwd,
    );
    let outcome = registry.run(HookEvent::PreToolUse, json!({"tool_input": {"x": 0}}), Some("shell")).await;
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert_eq!(outcome.modified_input, Some(json!({"x": 1})));
    assert_eq!(outcome.ran, vec!["rewrite", "observer"]);
    let seen = &outcome.additional_context[0];
    assert!(seen.starts_with("seen:"), "{seen}");
    assert!(seen.contains(r#""tool_input":{"x":1}"#), "{seen}");
    assert!(seen.contains(r#""hook_event":"preToolUse""#), "{seen}");
}

#[tokio::test]
async fn timeouts_kill_the_hook_without_blocking() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let mut slow = def(HookEvent::Stop, snippet::sleep(10), "slow", cwd);
    slow.timeout = Duration::from_secs(1);
    let registry = trusted_registry(vec![slow, def(HookEvent::Stop, snippet::print("next"), "next", cwd)], cwd);
    let started = Instant::now();
    let outcome = registry.run(HookEvent::Stop, json!({}), None).await;
    assert!(started.elapsed() < Duration::from_secs(8), "took {:?}", started.elapsed());
    assert!(!outcome.blocked);
    assert_eq!(outcome.errors.len(), 1, "{:?}", outcome.errors);
    assert!(outcome.errors[0].contains("'slow' timed out after 1.0s"), "{:?}", outcome.errors);
    assert_eq!(outcome.ran, vec!["slow", "next"]);
    assert_eq!(outcome.additional_context, vec!["next"]);
}

#[tokio::test]
async fn other_exit_codes_are_errors_not_blocks() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let registry = trusted_registry(
        vec![
            def(HookEvent::PostToolUse, snippet::fail(3, "linter crashed"), "lint", cwd),
            def(HookEvent::PostToolUse, snippet::print("still runs"), "after", cwd),
        ],
        cwd,
    );
    let outcome = registry.run(HookEvent::PostToolUse, json!({}), Some("shell")).await;
    assert!(!outcome.blocked);
    assert_eq!(outcome.errors, vec!["hook 'lint' exited with code 3: linter crashed"]);
    assert_eq!(outcome.additional_context, vec!["still runs"]);
}

#[tokio::test]
async fn matchers_filter_tool_events() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let mut shell_only = def(HookEvent::PreToolUse, snippet::print("shell hook"), "shell-only", cwd);
    shell_only.matcher = Some("shell|exec_command".to_string());
    let mut patch_only = def(HookEvent::PreToolUse, snippet::print("patch hook"), "patch-only", cwd);
    patch_only.matcher = Some("apply_patch".to_string());
    let all = def(HookEvent::PreToolUse, snippet::print("all tools"), "all", cwd);
    let mut bad = def(HookEvent::PreToolUse, snippet::print("never"), "bad", cwd);
    bad.matcher = Some("(".to_string());
    let mut stop = def(HookEvent::Stop, snippet::print("stop ran"), "stop", cwd);
    stop.matcher = Some("nothing-matches-this".to_string());
    let registry = trusted_registry(vec![shell_only, patch_only, all, bad, stop], cwd);

    let outcome = registry.run(HookEvent::PreToolUse, json!({}), Some("shell")).await;
    assert_eq!(outcome.ran, vec!["shell-only", "all"]);
    assert_eq!(outcome.additional_context, vec!["shell hook", "all tools"]);
    assert_eq!(outcome.errors.len(), 1);
    assert!(outcome.errors[0].contains("invalid matcher"));

    let outcome = registry.run(HookEvent::PreToolUse, json!({}), Some("apply_patch")).await;
    assert_eq!(outcome.ran, vec!["patch-only", "all"]);

    // Matchers only apply to tool events.
    let outcome = registry.run(HookEvent::Stop, json!({}), None).await;
    assert_eq!(outcome.ran, vec!["stop"]);
}

#[tokio::test]
async fn editing_a_script_requires_review_again() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let script = if cfg!(windows) { "hook.ps1" } else { "hook.sh" };
    let body = if cfg!(windows) { "Write-Output 'v1'" } else { "echo v1" };
    fs::write(cwd.join(script), body).unwrap();
    let mut registry = HookRegistry::new(
        vec![def(HookEvent::Stop, snippet::run_script(script), "script", cwd)],
        cwd.join("store").join("trusted_hooks.json"),
    );

    let info = registry.list().remove(0);
    assert_eq!(info.trust, "untrusted");
    assert_eq!(registry.needs_review().len(), 1);

    // Wrong hash and unknown id are rejected.
    let err = registry.set_trust(&info.id, "deadbeef", true).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput);
    let err = registry.set_trust("nope", &info.hash, true).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);

    registry.set_trust(&info.id, &info.hash, true).unwrap();
    assert_eq!(registry.list()[0].trust, "trusted");
    assert!(registry.needs_review().is_empty());
    let outcome = registry.run(HookEvent::Stop, json!({}), None).await;
    assert_eq!(outcome.additional_context, vec!["v1"], "{outcome:?}");

    // The store persists `{ id: hash }`.
    let stored: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(cwd.join("store").join("trusted_hooks.json")).unwrap()).unwrap();
    assert_eq!(stored, json!({ info.id.clone(): info.hash.clone() }));

    // Editing the script changes the hash: the hook is skipped until re-reviewed.
    let edited = if cfg!(windows) { "Write-Output 'v2'" } else { "echo v2" };
    fs::write(cwd.join(script), edited).unwrap();
    let changed = registry.list().remove(0);
    assert_eq!(changed.trust, "changed");
    assert_ne!(changed.hash, info.hash);
    assert_eq!(changed.id, info.id);
    assert_eq!(registry.needs_review().len(), 1);
    let outcome = registry.run(HookEvent::Stop, json!({}), None).await;
    assert_eq!(outcome.skipped_untrusted, vec!["script"]);
    assert!(outcome.ran.is_empty());

    registry.set_trust(&changed.id, &changed.hash, true).unwrap();
    let outcome = registry.run(HookEvent::Stop, json!({}), None).await;
    assert_eq!(outcome.additional_context, vec!["v2"]);

    // Untrusting removes the record.
    registry.set_trust(&changed.id, "", false).unwrap();
    assert_eq!(registry.list()[0].trust, "untrusted");
}

#[tokio::test]
async fn registries_sharing_a_store_keep_each_others_entries() {
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path();
    let store = cwd.join("trusted_hooks.json");
    let mut a = HookRegistry::new(vec![def(HookEvent::Stop, snippet::print("a"), "a", cwd)], store.clone());
    let mut b = HookRegistry::new(vec![def(HookEvent::Stop, snippet::print("b"), "b", cwd)], store.clone());
    let info_a = a.list().remove(0);
    let info_b = b.list().remove(0);
    a.set_trust(&info_a.id, &info_a.hash, true).unwrap();
    b.set_trust(&info_b.id, &info_b.hash, true).unwrap();
    let stored: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(&store).unwrap()).unwrap();
    assert_eq!(stored.len(), 2);
    a.reload_trust();
    assert_eq!(a.list()[0].trust, "trusted");
    // A fresh registry sees persisted trust.
    let fresh = HookRegistry::new(vec![def(HookEvent::Stop, snippet::print("b"), "b", cwd)], store);
    assert_eq!(fresh.list()[0].trust, "trusted");
}

#[tokio::test]
async fn spawn_failures_are_reported() {
    let tmp = TempDir::new().unwrap();
    let missing_cwd = tmp.path().join("does-not-exist");
    let registry =
        trusted_registry(vec![def(HookEvent::Stop, snippet::print("x"), "nowhere", &missing_cwd)], tmp.path());
    let outcome = registry.run(HookEvent::Stop, json!({}), None).await;
    assert!(outcome.ran.is_empty());
    assert_eq!(outcome.errors.len(), 1);
    assert!(outcome.errors[0].contains("could not be started"), "{:?}", outcome.errors);
}
