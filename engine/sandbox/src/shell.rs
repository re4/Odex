//! Building argv for running a command string through a shell.

use std::path::Path;

/// Prefix for PowerShell commands: no progress bars (they corrupt captured output) and UTF-8
/// console output so the captured bytes decode cleanly.
pub(crate) const POWERSHELL_PREFIX: &str =
    "$ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.Encoding]::UTF8; ";

/// argv that runs `command` through `shell`.
///
/// * `powershell` / `pwsh` → `<exe> -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command <prefix+command>`
/// * `cmd` → `cmd.exe /d /s /c <command>` (the command string is passed verbatim)
/// * `bash` / `zsh` / `sh` (and other POSIX shells) → `<shell> -lc <command>`
/// * `gitbash` → Git for Windows `bash.exe -lc <command>` (falls back to `bash.exe`)
/// * `wsl` → `wsl.exe -e bash -lc <command>`
///
/// A full path to a known shell keeps the path; unknown shells get `<shell> -c <command>`.
pub fn shell_argv(shell: &str, command: &str) -> Vec<String> {
    let trimmed = shell.trim();
    let has_dir = trimmed.contains('/') || trimmed.contains('\\');
    let name = Path::new(trimmed).file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let stem = name.strip_suffix(".exe").unwrap_or(&name).to_string();
    let exe = |default: &str| if has_dir { trimmed.to_string() } else { default.to_string() };
    match stem.as_str() {
        "powershell" | "pwsh" => {
            let program = exe(if stem == "pwsh" { "pwsh.exe" } else { "powershell.exe" });
            vec![
                program,
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-Command".into(),
                format!("{POWERSHELL_PREFIX}{command}"),
            ]
        }
        "cmd" => vec![exe("cmd.exe"), "/d".into(), "/s".into(), "/c".into(), command.to_string()],
        "gitbash" | "git-bash" => vec![find_git_bash(), "-lc".into(), command.to_string()],
        "wsl" => vec![exe("wsl.exe"), "-e".into(), "bash".into(), "-lc".into(), command.to_string()],
        "bash" | "zsh" | "sh" | "dash" | "ksh" | "ash" => vec![trimmed.to_string(), "-lc".into(), command.to_string()],
        _ => vec![trimmed.to_string(), "-c".into(), command.to_string()],
    }
}

/// Locates Git for Windows' `bin\bash.exe`.
#[cfg(windows)]
fn find_git_bash() -> String {
    use std::path::PathBuf;

    let mut candidates: Vec<PathBuf> = Vec::new();
    // Derive from git.exe on PATH: <root>\cmd\git.exe, <root>\bin\git.exe, <root>\mingw64\bin\git.exe.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.join("git.exe").is_file() {
                let mut root = dir.clone();
                for _ in 0..3 {
                    candidates.push(root.join("bin").join("bash.exe"));
                    if !root.pop() {
                        break;
                    }
                }
            }
        }
    }
    for var in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(var) {
            candidates.push(PathBuf::from(dir).join("Git").join("bin").join("bash.exe"));
        }
    }
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(dir).join("Programs").join("Git").join("bin").join("bash.exe"));
    }
    candidates
        .into_iter()
        .find(|c| c.is_file())
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bash.exe".to_string())
}

#[cfg(not(windows))]
fn find_git_bash() -> String {
    "bash".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_argv() {
        let argv = shell_argv("powershell", "Get-ChildItem");
        assert_eq!(argv[0], "powershell.exe");
        assert_eq!(argv[6], "-Command");
        assert!(argv[7].starts_with("$ProgressPreference='SilentlyContinue';"));
        assert!(argv[7].ends_with("Get-ChildItem"));
        assert_eq!(shell_argv("pwsh", "x")[0], "pwsh.exe");
        assert_eq!(shell_argv("PowerShell.exe", "x")[0], "powershell.exe");
    }

    #[test]
    fn cmd_and_posix_argv() {
        assert_eq!(shell_argv("cmd", "echo hi"), vec!["cmd.exe", "/d", "/s", "/c", "echo hi"]);
        assert_eq!(shell_argv("bash", "ls"), vec!["bash", "-lc", "ls"]);
        assert_eq!(shell_argv("/bin/zsh", "ls"), vec!["/bin/zsh", "-lc", "ls"]);
        assert_eq!(shell_argv("wsl", "ls"), vec!["wsl.exe", "-e", "bash", "-lc", "ls"]);
        let gb = shell_argv("gitbash", "ls");
        assert!(gb[0].to_ascii_lowercase().ends_with("bash.exe") || gb[0] == "bash");
        assert_eq!(&gb[1..], &["-lc", "ls"]);
    }
}
