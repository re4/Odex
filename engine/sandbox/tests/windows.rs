//! Windows integration tests. ACL changes only ever touch freshly created temp dirs.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use odex_sandbox::{exec, shell_argv, spawn, status, Backend, ExecOutput, ExecRequest, OutputChunk, SandboxPolicy};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct Dirs {
    root: TempDir,
    sibling: TempDir,
    temp: TempDir,
}

impl Dirs {
    fn new() -> Dirs {
        Dirs {
            root: tempfile::Builder::new().prefix("odex-sb-root").tempdir().unwrap(),
            sibling: tempfile::Builder::new().prefix("odex-sb-sibling").tempdir().unwrap(),
            temp: tempfile::Builder::new().prefix("odex-sb-tmp").tempdir().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.root.path().to_path_buf()
    }

    fn workspace(&self) -> SandboxPolicy {
        SandboxPolicy::WorkspaceWrite { writable_roots: vec![self.root()], network: false }
    }

    fn req(&self, argv: Vec<String>, policy: SandboxPolicy) -> ExecRequest {
        ExecRequest {
            argv,
            cwd: self.root(),
            policy,
            timeout: Some(Duration::from_secs(60)),
            sandbox_temp: Some(self.temp.path().to_path_buf()),
            ..Default::default()
        }
    }
}

fn cmd(c: &str) -> Vec<String> {
    shell_argv("cmd", c)
}

fn ps(c: &str) -> Vec<String> {
    shell_argv("powershell", c)
}

async fn run(req: ExecRequest) -> ExecOutput {
    exec(req, |_| {}, CancellationToken::new()).await.expect("exec")
}

fn policies(d: &Dirs) -> Vec<SandboxPolicy> {
    vec![SandboxPolicy::ReadOnly, d.workspace(), SandboxPolicy::FullAccess]
}

#[tokio::test]
async fn echo_under_each_policy() {
    let d = Dirs::new();
    for policy in policies(&d) {
        let sandboxed = policy.is_sandboxed();
        let out = run(d.req(cmd("echo hello sandbox"), policy.clone())).await;
        assert_eq!(out.exit_code, Some(0), "{policy:?}: {out:?}");
        assert_eq!(out.stdout.trim(), "hello sandbox", "{policy:?}: {out:?}");
        assert_eq!(out.sandboxed, sandboxed);
        assert!(!out.sandbox_denied && !out.timed_out && !out.truncated);
    }
}

#[tokio::test]
async fn powershell_runs_under_each_policy() {
    let d = Dirs::new();
    std::fs::write(d.root.path().join("present.txt"), "x").unwrap();
    for policy in policies(&d) {
        let out = run(d.req(ps("Get-ChildItem -Name; Write-Output \"done\""), policy.clone())).await;
        assert_eq!(out.exit_code, Some(0), "{policy:?}: {out:?}");
        assert!(out.stdout.contains("present.txt"), "{policy:?}: {out:?}");
        assert!(out.stdout.contains("done"), "{policy:?}: {out:?}");
        assert!(out.stderr.trim().is_empty(), "{policy:?}: unexpected stderr {:?}", out.stderr);
    }
}

#[tokio::test]
async fn captures_and_interleaves_stdout_stderr() {
    let d = Dirs::new();
    let script = "[Console]::Out.WriteLine('out-1'); [Console]::Out.Flush(); Start-Sleep -Milliseconds 300; \
                  [Console]::Error.WriteLine('err-1'); [Console]::Error.Flush(); Start-Sleep -Milliseconds 300; \
                  [Console]::Out.WriteLine('out-2')";
    let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = chunks.clone();
    let out = exec(
        d.req(ps(script), d.workspace()),
        move |c: OutputChunk| sink.lock().unwrap().push(c),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(out.stdout.contains("out-1") && out.stdout.contains("out-2"), "{out:?}");
    assert!(!out.stdout.contains("err-1"));
    assert_eq!(out.stderr.trim(), "err-1");
    let a = &out.aggregated;
    let (i1, i2, i3) = (a.find("out-1").unwrap(), a.find("err-1").unwrap(), a.find("out-2").unwrap());
    assert!(i1 < i2 && i2 < i3, "aggregated out of order: {a:?}");
    let chunks = chunks.lock().unwrap();
    assert!(chunks.iter().any(|c| c.is_stderr()));
    assert!(chunks.iter().any(|c| !c.is_stderr()));
}

#[tokio::test]
async fn exit_codes_are_reported() {
    let d = Dirs::new();
    for policy in policies(&d) {
        let out = run(d.req(cmd("exit 7"), policy.clone())).await;
        assert_eq!(out.exit_code, Some(7), "{policy:?}");
        assert!(!out.sandbox_denied);
    }
    let out = run(d.req(ps("exit 3"), d.workspace())).await;
    assert_eq!(out.exit_code, Some(3));
}

#[tokio::test]
async fn stdin_is_delivered() {
    let d = Dirs::new();
    let mut req = d.req(ps("$t = [Console]::In.ReadToEnd(); Write-Output ('got:' + $t.Trim())"), d.workspace());
    req.stdin = Some(b"hello from stdin\n".to_vec());
    let out = run(req).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout.trim(), "got:hello from stdin");

    // No stdin given: the child sees EOF instead of hanging. (System32's sort: an msys `sort`
    // earlier on PATH cannot start in the sandbox while unsandboxed msys processes run.)
    let sort = format!(r"{}\System32\sort.exe", std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()));
    let out = run(d.req(vec![sort], SandboxPolicy::ReadOnly)).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
}

#[tokio::test]
async fn env_is_passed_and_temp_redirected() {
    let d = Dirs::new();
    let mut req = d.req(cmd("echo [%ODEX_TEST_VALUE%] [%TEMP%] [%ODEX_SANDBOX%]"), d.workspace());
    req.env.insert("ODEX_TEST_VALUE".into(), "forty two".into());
    let out = run(req).await;
    assert_eq!(out.exit_code, Some(0));
    assert!(out.stdout.contains("[forty two]"), "{out:?}");
    let temp = d.temp.path().to_string_lossy().to_string();
    assert!(out.stdout.contains(&format!("[{temp}]")), "{out:?}");
    assert!(out.stdout.contains("[restricted-token]"), "{out:?}");

    let mut req = d.req(cmd("echo [%ODEX_TEST_VALUE%]"), SandboxPolicy::FullAccess);
    req.env.insert("ODEX_TEST_VALUE".into(), "full".into());
    assert!(run(req).await.stdout.contains("[full]"));
}

fn write_cmd(target: &Path) -> Vec<String> {
    cmd(&format!("echo sandboxed> \"{}\"", target.display()))
}

#[tokio::test]
async fn workspace_write_allows_root_and_denies_sibling() {
    let d = Dirs::new();
    let inside = d.root.path().join("inside.txt");
    let out = run(d.req(write_cmd(&inside), d.workspace())).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(std::fs::read_to_string(&inside).unwrap().trim(), "sandboxed");

    // Nested directories and existing files inside the root are writable too.
    std::fs::create_dir(d.root.path().join("sub")).unwrap();
    let nested = d.root.path().join("sub").join("n.txt");
    std::fs::write(&nested, "old").unwrap();
    let out = run(d.req(write_cmd(&nested), d.workspace())).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(std::fs::read_to_string(&nested).unwrap().trim(), "sandboxed");

    // The private temp dir is writable.
    let out = run(d.req(cmd("echo t> \"%TEMP%\\t.txt\""), d.workspace())).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(d.temp.path().join("t.txt").exists());

    // A sibling directory that was not granted is not.
    let outside = d.sibling.path().join("outside.txt");
    let out = run(d.req(write_cmd(&outside), d.workspace())).await;
    assert_ne!(out.exit_code, Some(0), "{out:?}");
    assert!(out.sandbox_denied, "{out:?}");
    assert!(!outside.exists());

    // Same via PowerShell, with the .NET exception text.
    let out = run(d.req(
        ps(&format!("Set-Content -LiteralPath '{}' -Value x -ErrorAction Stop", outside.display())),
        d.workspace(),
    ))
    .await;
    assert_ne!(out.exit_code, Some(0), "{out:?}");
    assert!(out.sandbox_denied, "{out:?}");
    assert!(!outside.exists());

    // Deleting files outside the roots is denied as well.
    let victim = d.sibling.path().join("victim.txt");
    std::fs::write(&victim, "keep").unwrap();
    let out = run(d.req(
        cmd(&format!("del /f /q \"{}\" && if exist \"{}\" exit 5", victim.display(), victim.display())),
        d.workspace(),
    ))
    .await;
    assert!(victim.exists(), "sandboxed delete removed a file outside the roots: {out:?}");

    // Full access can write anywhere.
    let out = run(d.req(write_cmd(&outside), SandboxPolicy::FullAccess)).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(outside.exists());
}

#[tokio::test]
async fn read_only_denies_writes_but_allows_reads() {
    let d = Dirs::new();
    std::fs::write(d.root.path().join("readme.txt"), "readable").unwrap();
    let out = run(d.req(cmd("type readme.txt"), SandboxPolicy::ReadOnly)).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout.trim(), "readable");

    let inside = d.root.path().join("inside.txt");
    let out = run(d.req(write_cmd(&inside), SandboxPolicy::ReadOnly)).await;
    assert_ne!(out.exit_code, Some(0), "{out:?}");
    assert!(out.sandbox_denied, "{out:?}");
    assert!(!inside.exists());

    // Writing to NUL still works (common in scripts).
    let out = run(d.req(cmd("echo x > NUL && echo ok"), SandboxPolicy::ReadOnly)).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout.trim(), "ok");
}

#[tokio::test]
async fn timeout_kills_the_process_tree() {
    let d = Dirs::new();
    let marker = d.root.path().join("marker.txt");
    // cmd -> powershell grandchild that would write the marker after 3 seconds.
    let inner = format!(
        "powershell.exe -NoProfile -NonInteractive -Command \"Start-Sleep 3; Set-Content -LiteralPath '{}' -Value x\"",
        marker.display()
    );
    for policy in [d.workspace(), SandboxPolicy::FullAccess] {
        let mut req = d.req(cmd(&inner), policy.clone());
        req.timeout = Some(Duration::from_millis(1000));
        let started = Instant::now();
        let out = run(req).await;
        assert!(out.timed_out, "{policy:?}: {out:?}");
        assert_eq!(out.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(6), "took {:?}", started.elapsed());
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!marker.exists(), "{policy:?}: grandchild survived the timeout");
    }

    let mut req = d.req(ps("Start-Sleep 30"), d.workspace());
    req.timeout = Some(Duration::from_secs(1));
    let started = Instant::now();
    let out = run(req).await;
    assert!(out.timed_out && out.exit_code.is_none(), "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(6));
}

#[tokio::test]
async fn cancellation_kills_the_process() {
    let d = Dirs::new();
    let token = CancellationToken::new();
    let t2 = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(700)).await;
        t2.cancel();
    });
    let started = Instant::now();
    let out = exec(d.req(ps("Start-Sleep 30"), d.workspace()), |_| {}, token).await.unwrap();
    assert!(out.cancelled, "{out:?}");
    assert!(!out.timed_out);
    assert_eq!(out.exit_code, None);
    assert!(started.elapsed() < Duration::from_secs(6));
}

#[tokio::test]
async fn output_is_truncated_head_and_tail() {
    let d = Dirs::new();
    let mut req = d.req(ps("[Console]::Out.Write('HEAD' + ('x' * 200000) + 'TAIL')"), SandboxPolicy::FullAccess);
    req.max_output_bytes = 1000;
    let out = run(req).await;
    assert_eq!(out.exit_code, Some(0), "{:?}", out.stderr);
    assert!(out.truncated);
    assert!(out.stdout.starts_with("HEAD"), "{}", &out.stdout[..50]);
    assert!(out.stdout.trim_end().ends_with("TAIL"));
    assert!(out.stdout.contains("bytes truncated"));
    assert!(out.stdout.len() < 1200);
    assert!(out.aggregated.len() < 1200);
}

#[tokio::test]
async fn spawn_interactive_process() {
    let d = Dirs::new();
    let script = "while (($l = [Console]::In.ReadLine()) -ne $null) { [Console]::Out.WriteLine('echo:' + $l); [Console]::Out.Flush() }";
    for policy in [d.workspace(), SandboxPolicy::FullAccess] {
        let mut proc = spawn(d.req(ps(script), policy.clone())).await.expect("spawn");
        assert!(proc.pid() > 0);
        assert_eq!(proc.sandboxed(), policy.is_sandboxed());
        proc.write_stdin(b"first line\n").await.unwrap();
        let mut seen = String::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !seen.contains("echo:first line") {
            let chunk = tokio::time::timeout_at(deadline.into(), proc.recv_output()).await.expect("output in time");
            seen.push_str(&String::from_utf8_lossy(chunk.expect("open").bytes()));
        }
        proc.write_stdin(b"second\n").await.unwrap();
        while !seen.contains("echo:second") {
            let chunk = tokio::time::timeout_at(deadline.into(), proc.recv_output()).await.expect("output in time");
            seen.push_str(&String::from_utf8_lossy(chunk.expect("open").bytes()));
        }
        assert!(proc.try_wait().is_none());
        proc.kill();
        let code = tokio::time::timeout(Duration::from_secs(5), proc.wait()).await.expect("exit after kill");
        assert_eq!(code, Some(1));
    }

    // close_stdin -> EOF -> the loop ends on its own.
    let mut proc = spawn(d.req(ps(script), d.workspace())).await.unwrap();
    proc.write_stdin(b"bye\n").await.unwrap();
    proc.close_stdin();
    let code = tokio::time::timeout(Duration::from_secs(20), proc.wait()).await.expect("exit after EOF");
    assert_eq!(code, Some(0));
    let mut rx = proc.take_output().unwrap();
    let mut all = Vec::new();
    while let Some(c) = rx.recv().await {
        all.extend_from_slice(c.bytes());
    }
    assert!(String::from_utf8_lossy(&all).contains("echo:bye"));
}

#[tokio::test]
async fn status_sanity() {
    let s = status(Backend::Auto, "workspace-write");
    assert_eq!(s.backend, "restricted-token");
    assert!(s.available, "{s:?}");
    assert!(!s.network_isolated);
    assert!(s.warning.as_deref().unwrap_or("").contains("Network"));

    let s = status(Backend::RestrictedToken, "read-only");
    assert!(s.available);

    let s = status(Backend::AppContainer, "workspace-write");
    assert_eq!(s.backend, "appcontainer");
    assert!(s.available, "{s:?}");
    assert!(s.network_isolated);
    assert!(s.warning.is_some());

    let s = status(Backend::Auto, "full-access");
    assert!(s.available && !s.network_isolated);
    let s = status(Backend::None, "read-only");
    assert!(!s.available);
}

#[tokio::test]
async fn appcontainer_backend() {
    let d = Dirs::new();
    let policy = d.workspace();
    let mk = |argv| {
        let mut r = d.req(argv, policy.clone());
        r.backend = Backend::AppContainer;
        r
    };
    let out = run(mk(cmd("echo hello appcontainer"))).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout.trim(), "hello appcontainer");
    assert!(out.sandboxed);

    let inside = d.root.path().join("inside.txt");
    let out = run(mk(write_cmd(&inside))).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(inside.exists());

    let outside = d.sibling.path().join("outside.txt");
    let out = run(mk(write_cmd(&outside))).await;
    assert_ne!(out.exit_code, Some(0), "{out:?}");
    assert!(out.sandbox_denied, "{out:?}");
    assert!(!outside.exists());

    // AppContainers cannot traverse absolute paths through the user profile (the documented read
    // limitation), so directory enumeration is blocked, but the inherited working-directory handle
    // still allows relative single-file reads of granted files.
    let out = run(mk(cmd("type inside.txt"))).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert!(out.stdout.contains("sandboxed"), "{out:?}");

    // Network is isolated: loopback connections are refused.
    let net = run(mk(ps("try { (New-Object Net.Sockets.TcpClient).Connect('127.0.0.1', 80); 'connected' }                          catch { 'blocked' }"))).await;
    assert!(net.stdout.contains("blocked") || net.exit_code != Some(0), "network not isolated: {net:?}");
}

#[tokio::test]
async fn batch_files_and_missing_programs() {
    let d = Dirs::new();
    let script = d.root.path().join("hello script.cmd");
    std::fs::write(&script, "@echo off\r\necho batch:%1\r\n").unwrap();
    let out = run(d.req(vec![script.to_string_lossy().into_owned(), "arg1".into()], d.workspace())).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout.trim(), "batch:arg1");

    let err =
        exec(d.req(vec!["definitely-not-a-real-program-odex".into()], d.workspace()), |_| {}, CancellationToken::new())
            .await
            .unwrap_err();
    assert!(matches!(err, odex_sandbox::SandboxError::Spawn(_)), "{err:?}");
}

#[tokio::test]
async fn argv_quoting_roundtrip() {
    let d = Dirs::new();
    // .NET parses its command line with the MSVCRT rules; echo what it saw after the marker.
    let script = d.root.path().join("args.ps1");
    std::fs::write(
        &script,
        "$a = [Environment]::GetCommandLineArgs(); $i = [Array]::IndexOf($a, '--marker')\r\n\
         for ($j = $i + 1; $j -lt $a.Length; $j++) { [Console]::Out.WriteLine('[' + $a[$j] + ']') }\r\n",
    )
    .unwrap();
    let args =
        ["plain", "with space", "quote\"inside", "trailing\\", "back\\\\slash \"mix\"", "", "tab\there", "C:\\dir\\"];
    let mut argv: Vec<String> =
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    argv.push(script.to_string_lossy().into_owned());
    argv.push("--marker".into());
    argv.extend(args.iter().map(|s| s.to_string()));
    let out = run(d.req(argv, d.workspace())).await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    let expected: Vec<String> = args.iter().map(|a| format!("[{a}]")).collect();
    let got: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(got, expected, "{out:?}");

    let out = run(d.req(
        vec!["cmd.exe".into(), "/d".into(), "/s".into(), "/c".into(), "echo a \"b c\" & echo second".into()],
        SandboxPolicy::ReadOnly,
    ))
    .await;
    assert_eq!(out.stdout.lines().map(str::trim).collect::<Vec<_>>(), vec!["a \"b c\"", "second"]);
}

#[tokio::test]
async fn oem_output_is_decoded() {
    let d = Dirs::new();
    // cmd writes to pipes in the OEM code page, which is not UTF-8 for non-ASCII text.
    let out = run(d.req(cmd("echo caf\u{e9}"), SandboxPolicy::ReadOnly)).await;
    assert_eq!(out.exit_code, Some(0));
    assert_eq!(out.stdout.trim(), "caf\u{e9}", "{:?}", out.stdout);
}
