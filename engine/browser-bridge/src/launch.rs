//! Locating and launching a headless Chromium-based browser.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

/// Environment variable that overrides browser discovery.
pub const BROWSER_ENV: &str = "ODEX_BROWSER";

/// Find a Chromium-based browser: `$ODEX_BROWSER`, `$CHROME_PATH`, then the
/// usual install locations of Edge, Chrome, Chromium and Brave for the
/// current platform, then `PATH`.
pub fn find_browser() -> Option<PathBuf> {
    for var in [BROWSER_ENV, "CHROME_PATH"] {
        if let Some(p) = std::env::var_os(var).map(PathBuf::from) {
            if p.is_file() {
                return Some(p);
            }
        }
    }
    candidates().into_iter().find(|p| p.is_file())
}

#[cfg(windows)]
fn candidates() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles(x86)", "ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(v) = std::env::var_os(var) {
            roots.push(PathBuf::from(v));
        }
    }
    roots.push(PathBuf::from(r"C:\Program Files (x86)"));
    roots.push(PathBuf::from(r"C:\Program Files"));
    let rel = [
        r"Microsoft\Edge\Application\msedge.exe",
        r"Google\Chrome\Application\chrome.exe",
        r"Chromium\Application\chrome.exe",
        r"BraveSoftware\Brave-Browser\Application\brave.exe",
        r"Microsoft\Edge Beta\Application\msedge.exe",
        r"Microsoft\Edge Dev\Application\msedge.exe",
        r"Google\Chrome SxS\Application\chrome.exe",
    ];
    let mut out = Vec::new();
    for r in rel {
        for root in &roots {
            out.push(root.join(r));
        }
    }
    out.extend(on_path(&["msedge.exe", "chrome.exe", "chromium.exe", "brave.exe"]));
    out
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<PathBuf> {
    let apps = [
        "Google Chrome.app/Contents/MacOS/Google Chrome",
        "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "Chromium.app/Contents/MacOS/Chromium",
        "Brave Browser.app/Contents/MacOS/Brave Browser",
        "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
    ];
    let mut out = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for app in apps {
        out.push(Path::new("/Applications").join(app));
        if let Some(h) = &home {
            out.push(h.join("Applications").join(app));
        }
    }
    out.extend(on_path(&["google-chrome", "chromium", "microsoft-edge"]));
    out
}

#[cfg(all(unix, not(target_os = "macos")))]
fn candidates() -> Vec<PathBuf> {
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "microsoft-edge-stable",
        "brave-browser",
    ];
    let mut out = on_path(&names);
    for dir in ["/usr/bin", "/usr/local/bin", "/snap/bin", "/opt/google/chrome", "/opt/microsoft/msedge"] {
        for n in names {
            out.push(Path::new(dir).join(n));
        }
    }
    out.push(PathBuf::from("/opt/google/chrome/chrome"));
    out.push(PathBuf::from("/opt/microsoft/msedge/msedge"));
    out
}

#[cfg(not(any(windows, unix)))]
fn candidates() -> Vec<PathBuf> {
    Vec::new()
}

fn on_path(names: &[&str]) -> Vec<PathBuf> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path).flat_map(|dir| names.iter().map(move |n| dir.join(n))).collect()
}

/// Launch a headless browser with its own profile in `user_data_dir` and a
/// random DevTools port. Returns the child (killed on drop) and the HTTP
/// DevTools endpoint (`http://127.0.0.1:<port>`).
pub async fn launch_headless_browser(user_data_dir: &Path) -> Result<(tokio::process::Child, String)> {
    let exe = find_browser().ok_or_else(|| {
        anyhow!("no Chromium-based browser found (install Edge or Chrome, or set {BROWSER_ENV} to its executable)")
    })?;
    std::fs::create_dir_all(user_data_dir)
        .with_context(|| format!("creating browser profile dir {}", user_data_dir.display()))?;
    let port_file = user_data_dir.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&port_file);

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("--headless=new")
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", user_data_dir.display()))
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-sync",
            "--disable-features=Translate,MediaRouter,OptimizationHints",
            "--disable-component-update",
            "--mute-audio",
            "--window-size=1280,800",
            "about:blank",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().with_context(|| format!("launching {}", exe.display()))?;

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            if let Some(port) = text.lines().next().and_then(|l| l.trim().parse::<u16>().ok()) {
                return Ok((child, format!("http://127.0.0.1:{port}")));
            }
        }
        if let Some(status) = child.try_wait()? {
            bail!("{} exited during startup ({status})", exe.display());
        }
        if Instant::now() > deadline {
            let _ = child.kill().await;
            bail!("{} did not report a DevTools port within 30s", exe.display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
