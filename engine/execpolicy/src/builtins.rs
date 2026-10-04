//! Built-in rules: catastrophic commands are forbidden, risky ones prompt.

use std::sync::LazyLock;

use regex::Regex;

use crate::lexer::SimpleCommand;
use crate::normalize::Cmd;
use crate::{Decision, ShellKind};

/// A built-in rule implemented as a predicate over a normalised command.
pub(crate) struct Builtin {
    pub description: &'static str,
    pub decision: Decision,
    pub justification: &'static str,
    /// Equivalent prefix length, for specificity comparisons with user rules.
    pub prefix_len: usize,
    /// Whether the rule looks beyond the prefix (like a rule with a `pattern`).
    pub refined: bool,
    pub check: fn(&Cmd) -> bool,
}

impl Builtin {
    pub(crate) fn specificity(&self) -> usize {
        self.prefix_len * 2 + usize::from(self.refined)
    }
}

// ----- argument helpers -----------------------------------------------------

/// Concatenated single-dash short flags (`-rf -v` -> "rfv"), up to `--`.
fn short_flags(args: &[String]) -> String {
    let mut out = String::new();
    for arg in args {
        if arg == "--" {
            break;
        }
        if let Some(flags) = arg.strip_prefix('-') {
            if !flags.starts_with('-') && flags.chars().all(|c| c.is_ascii_alphabetic()) {
                out.push_str(flags);
            }
        }
    }
    out
}

fn has_arg(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|a| {
        names.contains(&a.as_str()) || names.iter().any(|n| n.starts_with("--") && a.starts_with(&format!("{n}=")))
    })
}

fn has_arg_ci(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|a| names.iter().any(|n| a.eq_ignore_ascii_case(n)))
}

/// Non-option arguments (everything after `--` counts as positional).
fn positionals(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut after_dashdash = false;
    for arg in args {
        if after_dashdash {
            out.push(arg.as_str());
        } else if arg == "--" {
            after_dashdash = true;
        } else if !arg.starts_with('-') || arg == "-" {
            out.push(arg.as_str());
        }
    }
    out
}

/// PowerShell parameter `full` (e.g. `-recurse`) given, allowing unambiguous
/// abbreviations of at least `min_len` characters and `-Param:value` syntax.
pub(crate) fn ps_param(args: &[String], full: &str, min_len: usize) -> bool {
    args.iter().any(|a| {
        let lower = a.to_ascii_lowercase();
        let name = lower.split(':').next().unwrap_or("");
        name.len() >= min_len && name.starts_with('-') && full.starts_with(name)
    })
}

/// Path targets of a PowerShell item cmdlet: `-Path` / `-LiteralPath`
/// values and positional arguments (comma-separated lists are split).
fn ps_targets(args: &[String]) -> Vec<&str> {
    const PATH_PARAMS: [&str; 3] = ["-path", "-literalpath", "-pspath"];
    const VALUE_PARAMS: [&str; 6] = ["-filter", "-include", "-exclude", "-stream", "-credential", "-itemtype"];
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg.starts_with('-') && arg.len() > 1 {
            let (name, inline) = match arg.split_once(':') {
                Some((n, v)) => (n.to_ascii_lowercase(), Some(v)),
                None => (arg.to_ascii_lowercase(), None),
            };
            let is_path = name == "-lp" || (name.len() >= 2 && PATH_PARAMS.iter().any(|p| p.starts_with(&name)));
            let is_value = name.len() >= 3 && VALUE_PARAMS.iter().any(|p| p.starts_with(&name));
            if is_path {
                match inline {
                    Some(v) => out.extend(v.split(',').map(str::trim)),
                    None => {
                        if let Some(v) = args.get(i + 1) {
                            out.extend(v.split(',').map(str::trim));
                        }
                        i += 1;
                    }
                }
            } else if is_value && inline.is_none() {
                i += 1;
            }
        } else {
            out.extend(arg.split(',').map(str::trim));
        }
        i += 1;
    }
    out
}

static WINDOWS_CRITICAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:[a-z]:|[a-z]:/users(?:/[^/]+)?|[a-z]:/(?:windows|windows/system32|program files|program files \(x86\)|programdata))$",
    )
    .expect("valid regex")
});

static POSIX_CRITICAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:/(?:bin|boot|dev|etc|home|lib|lib32|lib64|opt|proc|root|sbin|srv|sys|usr|var|users|system|library|applications|private|mnt)|/[a-z]|/mnt/[a-z]|/home/[^/]+|/users/[^/]+)$",
    )
    .expect("valid regex")
});

/// Filesystem roots, home / profile directories and core system folders,
/// in POSIX, Windows, Git-Bash and environment-variable spellings.
pub(crate) fn is_critical_path(target: &str) -> bool {
    let target = target.trim().trim_matches(|c| c == '"' || c == '\'');
    if target.is_empty() {
        return false;
    }
    let unified = target.replace('\\', "/").to_ascii_lowercase();
    let mut s = unified.as_str();
    loop {
        if let Some(rest) = s.strip_suffix("/*").or_else(|| s.strip_suffix("/.")).or_else(|| s.strip_suffix("/..")) {
            s = rest;
        } else if s.len() > 1 && s.ends_with('/') {
            s = &s[..s.len() - 1];
        } else if s == "*" || s == "/" {
            s = "";
        } else {
            break;
        }
    }
    if s.is_empty() {
        return true;
    }
    if matches!(
        s,
        "~" | "$home"
            | "${home}"
            | "$env:userprofile"
            | "${env:userprofile}"
            | "%userprofile%"
            | "$env:homedrive$env:homepath"
            | "%homedrive%%homepath%"
            | "$env:systemroot"
            | "%systemroot%"
            | "$env:windir"
            | "%windir%"
            | "$env:systemdrive"
            | "%systemdrive%"
            | "$env:programfiles"
            | "%programfiles%"
            | "${env:programfiles(x86)}"
            | "%programfiles(x86)%"
            | "$env:programdata"
            | "%programdata%"
    ) {
        return true;
    }
    WINDOWS_CRITICAL.is_match(s) || POSIX_CRITICAL.is_match(s)
}

// ----- forbid checks --------------------------------------------------------

fn rm_recursive_critical(c: &Cmd) -> bool {
    if c.raw_lc != "rm" || c.prog_lc == "remove-item" {
        return false;
    }
    let args = c.args();
    let flags = short_flags(args);
    let recursive = flags.contains('r') || flags.contains('R') || has_arg(args, &["--recursive"]);
    recursive && (has_arg(args, &["--no-preserve-root"]) || positionals(args).iter().any(|t| is_critical_path(t)))
}

fn mkfs(c: &Cmd) -> bool {
    c.raw_lc.starts_with("mkfs") || c.raw_lc == "mke2fs" || c.raw_lc == "wipefs"
}

fn is_block_device(path: &str) -> bool {
    let Some(dev) = path.strip_prefix("/dev/") else { return false };
    ["sd", "hd", "nvme", "mmcblk", "xvd", "vd", "disk", "rdisk", "md", "dm-", "mapper/", "loop"]
        .iter()
        .any(|p| dev.starts_with(p))
}

fn dd_to_device(c: &Cmd) -> bool {
    c.raw_lc == "dd" && c.args().iter().any(|a| a.strip_prefix("of=").is_some_and(is_block_device))
}

fn redirect_to_device(c: &Cmd) -> bool {
    c.redirects.iter().any(|r| r.writes_file() && is_block_device(&r.target))
}

fn format_drive(c: &Cmd) -> bool {
    static DRIVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z]:\\?$").expect("valid regex"));
    c.raw_lc == "format" && c.args().iter().any(|a| DRIVE.is_match(a))
}

fn disk_tools(c: &Cmd) -> bool {
    c.is(&["diskpart", "bcdedit"])
}

fn cipher_wipe(c: &Cmd) -> bool {
    c.raw_lc == "cipher" && c.args().iter().any(|a| a.to_ascii_lowercase().starts_with("/w"))
}

fn remove_item_critical(c: &Cmd) -> bool {
    if c.prog_lc != "remove-item" {
        return false;
    }
    let args = c.args();
    let targets = ps_targets(args);
    let registry = targets.iter().any(|t| {
        let lower = t.to_ascii_lowercase();
        lower.starts_with("hklm:") || lower.starts_with("registry::hkey_local_machine")
    });
    registry || (ps_param(args, "-recurse", 2) && targets.iter().any(|t| is_critical_path(t)))
}

fn cmd_delete_critical(c: &Cmd) -> bool {
    if !matches!(c.raw_lc.as_str(), "del" | "erase" | "rd" | "rmdir") {
        return false;
    }
    let args = c.args();
    has_arg_ci(args, &["/s"]) && args.iter().filter(|a| !a.starts_with('/') || a.len() > 2).any(|a| is_critical_path(a))
}

fn recursive_perms_critical(c: &Cmd) -> bool {
    if !matches!(c.raw_lc.as_str(), "chmod" | "chown" | "chgrp") {
        return false;
    }
    let args = c.args();
    let recursive = short_flags(args).contains('R') || has_arg(args, &["--recursive"]);
    recursive && positionals(args).iter().any(|t| is_critical_path(t))
}

fn reg_delete_hklm(c: &Cmd) -> bool {
    let args = c.args();
    c.raw_lc == "reg"
        && args.first().is_some_and(|a| a.eq_ignore_ascii_case("delete"))
        && args.get(1).is_some_and(|k| {
            let upper = k.to_ascii_uppercase();
            upper.starts_with("HKLM") || upper.starts_with("HKEY_LOCAL_MACHINE")
        })
}

fn disk_cmdlets(c: &Cmd) -> bool {
    matches!(c.prog_lc.as_str(), "format-volume" | "clear-disk" | "initialize-disk" | "remove-partition")
}

fn shadow_copy_delete(c: &Cmd) -> bool {
    let args = c.args();
    let first = |n: &str| args.first().is_some_and(|a| a.eq_ignore_ascii_case(n));
    (c.raw_lc == "vssadmin" && first("delete") && args.get(1).is_some_and(|a| a.eq_ignore_ascii_case("shadows")))
        || (c.raw_lc == "wmic" && has_arg_ci(args, &["shadowcopy"]) && has_arg_ci(args, &["delete"]))
        || (c.raw_lc == "wbadmin" && first("delete"))
}

// ----- prompt checks --------------------------------------------------------

fn power(c: &Cmd) -> bool {
    let sub = c.args().first().map(String::as_str);
    c.is(&["shutdown", "reboot", "halt", "poweroff", "stop-computer", "restart-computer"])
        || (c.raw_lc == "init" && matches!(sub, Some("0") | Some("6")))
        || (c.raw_lc == "systemctl" && matches!(sub, Some("poweroff" | "reboot" | "halt" | "suspend" | "hibernate")))
}

fn git_push_force(c: &Cmd) -> bool {
    let Some(("push", rest)) = c.git() else { return false };
    let flags = short_flags(rest);
    flags.contains('f')
        || flags.contains('d')
        || has_arg(rest, &["--force", "--force-with-lease", "--force-if-includes", "--mirror", "--delete", "--prune"])
        || positionals(rest).iter().skip(1).any(|p| p.starts_with('+') || (p.starts_with(':') && p.len() > 1))
}

fn git_reset_hard(c: &Cmd) -> bool {
    matches!(c.git(), Some(("reset", rest)) if has_arg(rest, &["--hard"]))
}

fn git_clean_force(c: &Cmd) -> bool {
    let Some(("clean", rest)) = c.git() else { return false };
    let flags = short_flags(rest);
    let force = flags.contains('f') || has_arg(rest, &["--force"]);
    let dry_run = flags.contains('n') || has_arg(rest, &["--dry-run"]);
    force && !dry_run
}

fn git_discard_changes(c: &Cmd) -> bool {
    match c.git() {
        Some(("checkout", rest)) => {
            has_arg(rest, &["-f", "--force"]) || positionals(rest).iter().any(|p| matches!(*p, "." | ":/" | "*"))
        }
        Some(("restore", rest)) => {
            let staged_only = has_arg(rest, &["--staged", "-S"]) && !has_arg(rest, &["--worktree", "-W"]);
            !staged_only && positionals(rest).iter().any(|p| matches!(*p, "." | ":/" | "*"))
        }
        Some(("stash", rest)) => rest.first().is_some_and(|s| s == "clear"),
        _ => false,
    }
}

fn git_branch_force_delete(c: &Cmd) -> bool {
    let Some(("branch", rest)) = c.git() else { return false };
    let flags = short_flags(rest);
    flags.contains('D')
        || ((flags.contains('d') || has_arg(rest, &["--delete"]))
            && (flags.contains('f') || has_arg(rest, &["--force"])))
}

fn elevation(c: &Cmd) -> bool {
    if c.is(&["sudo", "doas", "su", "pkexec", "runas", "gsudo"]) {
        return true;
    }
    if c.prog_lc == "start-process" {
        let args = c.args();
        return args.windows(2).any(|w| {
            w[0].to_ascii_lowercase().starts_with("-verb")
                && w[1].trim_matches(['\'', '"']).eq_ignore_ascii_case("runas")
        }) || args
            .iter()
            .any(|a| a.to_ascii_lowercase().starts_with("-verb:") && a.to_ascii_lowercase().ends_with("runas"));
    }
    false
}

fn execution_policy(c: &Cmd) -> bool {
    c.prog_lc == "set-executionpolicy"
}

fn publish(c: &Cmd) -> bool {
    let sub = c.subcommand();
    (c.is(&["npm", "pnpm", "yarn", "bun"]) && matches!(sub, Some("publish" | "unpublish")))
        || (c.raw_lc == "cargo" && matches!(sub, Some("publish" | "yank")))
        || (c.raw_lc == "twine" && sub == Some("upload"))
        || (c.raw_lc == "gem" && sub == Some("push"))
        || (c.is(&["poetry", "uv", "hatch", "flit"]) && sub == Some("publish"))
        || (c.raw_lc == "dotnet" && sub == Some("nuget") && has_arg(c.args(), &["push"]))
}

fn container_prune(c: &Cmd) -> bool {
    c.is(&["docker", "podman"]) && c.args().iter().any(|a| a == "prune")
}

fn kill_all(c: &Cmd) -> bool {
    if c.raw_lc != "kill" || c.powershell {
        return false;
    }
    let args = c.args();
    args.iter().skip(1).any(|a| a == "-1") || args.iter().skip_while(|a| *a != "--").skip(1).any(|a| a == "-1")
}

fn taskkill_force(c: &Cmd) -> bool {
    c.raw_lc == "taskkill" && has_arg_ci(c.args(), &["/f"])
}

fn stop_process_force(c: &Cmd) -> bool {
    c.prog_lc == "stop-process" && ps_param(c.args(), "-force", 2)
}

fn invoke_expression(c: &Cmd) -> bool {
    c.prog_lc == "invoke-expression"
}

fn encoded_powershell(c: &Cmd) -> bool {
    c.is(&["powershell", "pwsh"])
        && c.args().iter().any(|a| {
            let lower = a.to_ascii_lowercase();
            lower.len() >= 2
                && (lower == "-e" || lower == "-ec" || ("-encodedcommand".starts_with(&lower) && lower.len() >= 4))
        })
}

fn destructive_infra(c: &Cmd) -> bool {
    let sub = c.subcommand();
    (c.raw_lc == "terraform" && sub == Some("destroy"))
        || (c.raw_lc == "kubectl" && sub == Some("delete"))
        || (c.raw_lc == "gh" && c.args().len() >= 2 && c.args()[1] == "delete")
        || (c.raw_lc == "crontab" && has_arg(c.args(), &["-r"]))
}

fn registry_edit(c: &Cmd) -> bool {
    c.raw_lc == "reg"
        && c.args().first().is_some_and(|a| {
            matches!(a.to_ascii_lowercase().as_str(), "delete" | "add" | "import" | "restore" | "load" | "unload")
        })
}

macro_rules! builtin {
    ($description:expr, $decision:ident, $justification:expr, $prefix_len:expr, $refined:expr, $check:expr) => {
        Builtin {
            description: $description,
            decision: Decision::$decision,
            justification: $justification,
            prefix_len: $prefix_len,
            refined: $refined,
            check: $check,
        }
    };
}

pub(crate) static BUILTINS: &[Builtin] = &[
    // Forbidden: irreversible damage to the machine.
    builtin!(
        "rm -r on /, ~ or a system directory",
        Forbid,
        "Recursively deleting a filesystem root, home directory or system directory is never allowed",
        1,
        true,
        rm_recursive_critical
    ),
    builtin!("mkfs / wipefs", Forbid, "Formatting or wiping a filesystem destroys all data on it", 1, false, mkfs),
    builtin!(
        "dd of=<block device>",
        Forbid,
        "Writing raw data to a disk device destroys its contents",
        1,
        true,
        dd_to_device
    ),
    builtin!(
        "redirect into a block device",
        Forbid,
        "Writing raw data to a disk device destroys its contents",
        1,
        true,
        redirect_to_device
    ),
    builtin!("format <drive>:", Forbid, "Formatting a drive destroys all data on it", 1, true, format_drive),
    builtin!(
        "diskpart / bcdedit",
        Forbid,
        "Disk partitioning and boot configuration tools can make the machine unbootable",
        1,
        false,
        disk_tools
    ),
    builtin!("cipher /w", Forbid, "cipher /w wipes free space on a volume", 1, true, cipher_wipe),
    builtin!(
        "Remove-Item -Recurse on a drive root, profile or HKLM",
        Forbid,
        "Recursively deleting a drive root, the user profile, a system folder or HKLM is never allowed",
        1,
        true,
        remove_item_critical
    ),
    builtin!(
        "del/rd /s on a drive root or profile",
        Forbid,
        "Recursively deleting a drive root or the user profile is never allowed",
        1,
        true,
        cmd_delete_critical
    ),
    builtin!(
        "chmod/chown -R on / or a system directory",
        Forbid,
        "Recursively changing ownership or permissions of system directories breaks the machine",
        1,
        true,
        recursive_perms_critical
    ),
    builtin!(
        "reg delete HKLM",
        Forbid,
        "Deleting machine-wide registry keys can break Windows",
        2,
        true,
        reg_delete_hklm
    ),
    builtin!(
        "Format-Volume / Clear-Disk",
        Forbid,
        "Formatting or clearing a disk destroys all data on it",
        1,
        false,
        disk_cmdlets
    ),
    builtin!(
        "delete shadow copies / backups",
        Forbid,
        "Deleting shadow copies or backups removes recovery points",
        2,
        true,
        shadow_copy_delete
    ),
    // Prompt: risky but sometimes legitimate.
    builtin!("shutdown / reboot", Prompt, "Shuts down or restarts the computer", 1, false, power),
    builtin!(
        "git push --force / remote delete",
        Prompt,
        "Force-pushing or deleting remote refs can destroy published history",
        2,
        true,
        git_push_force
    ),
    builtin!("git reset --hard", Prompt, "Discards uncommitted changes", 2, true, git_reset_hard),
    builtin!("git clean -f", Prompt, "Permanently deletes untracked files", 2, true, git_clean_force),
    builtin!(
        "git checkout/restore . / stash clear",
        Prompt,
        "Discards uncommitted changes",
        2,
        true,
        git_discard_changes
    ),
    builtin!(
        "git branch -D",
        Prompt,
        "Force-deletes a branch, possibly losing unmerged commits",
        2,
        true,
        git_branch_force_delete
    ),
    builtin!("sudo / runas", Prompt, "Runs a command with elevated privileges", 1, false, elevation),
    builtin!(
        "Set-ExecutionPolicy",
        Prompt,
        "Changes the PowerShell script execution policy",
        1,
        false,
        execution_policy
    ),
    builtin!("package publish", Prompt, "Publishes a package to a public registry", 2, false, publish),
    builtin!(
        "docker/podman prune",
        Prompt,
        "Permanently deletes containers, images or volumes",
        2,
        false,
        container_prune
    ),
    builtin!("kill -1", Prompt, "Sends a signal to every process the user owns", 1, true, kill_all),
    builtin!("taskkill /f", Prompt, "Force-terminates processes", 1, true, taskkill_force),
    builtin!("Stop-Process -Force", Prompt, "Force-terminates processes", 1, true, stop_process_force),
    builtin!("Invoke-Expression", Prompt, "Executes a string as code", 1, false, invoke_expression),
    builtin!(
        "powershell -EncodedCommand",
        Prompt,
        "Runs an encoded (opaque) PowerShell script",
        1,
        true,
        encoded_powershell
    ),
    builtin!(
        "terraform destroy / kubectl delete / gh ... delete",
        Prompt,
        "Deletes remote infrastructure or resources",
        2,
        false,
        destructive_infra
    ),
    builtin!("reg add/delete", Prompt, "Modifies the Windows registry", 2, false, registry_edit),
];

// ----- whole-command checks -------------------------------------------------

static FORK_BOMB: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\s*\(\s*\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;?\s*:").expect("valid regex"));

static DOWNLOAD_PIPE_SHELL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:curl|wget|iwr|irm|invoke-webrequest|invoke-restmethod)\b[^|\n]*\|\s*(?:sudo\s+)?(?:sh|bash|zsh|dash|ksh|python[0-9.]*|perl|ruby|node|iex|invoke-expression|powershell|pwsh)\b",
    )
    .expect("valid regex")
});

static SHELL_OF_DOWNLOAD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:sh|bash|zsh|python[0-9.]*)\b[^\n]*(?:<\(|\$\()\s*(?:curl|wget)\b").expect("valid regex")
});

static INVOKE_EXPRESSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:^|[\s;|&(=])(?:iex|invoke-expression)(?:$|[\s;|&(])").expect("valid regex"));

pub(crate) const FORK_BOMB_DESC: &str = "fork bomb (builtin)";
pub(crate) const PIPE_TO_SHELL_DESC: &str = "download piped into a shell (builtin)";
const PIPE_TO_SHELL_WHY: &str = "Downloads code from the network and executes it";

/// Checks over the raw command text, used for every command.
pub(crate) fn raw_checks(command: &str, analyzable: bool) -> Vec<(&'static str, Decision, &'static str)> {
    let mut out = Vec::new();
    if FORK_BOMB.is_match(command) {
        out.push((FORK_BOMB_DESC, Decision::Forbid, "A fork bomb exhausts system resources"));
    }
    if !analyzable {
        if DOWNLOAD_PIPE_SHELL.is_match(command) || SHELL_OF_DOWNLOAD.is_match(command) {
            out.push((PIPE_TO_SHELL_DESC, Decision::Prompt, PIPE_TO_SHELL_WHY));
        }
        if INVOKE_EXPRESSION.is_match(command) {
            out.push(("Invoke-Expression (builtin)", Decision::Prompt, "Executes a string as code"));
        }
    }
    out
}

fn is_downloader(c: &Cmd) -> bool {
    c.is(&["curl", "wget", "invoke-webrequest", "invoke-restmethod", "fetch", "http", "xh", "aria2c"])
}

/// An interpreter that would execute code read from stdin.
fn executes_stdin(c: &Cmd) -> bool {
    let args = c.args();
    if c.prog_lc == "invoke-expression" {
        return true;
    }
    if c.is(&["sh", "bash", "zsh", "dash", "ksh", "ash", "fish"]) {
        return args.iter().all(|a| a.starts_with('-') && a != "-c") || has_arg(args, &["-s", "-"]);
    }
    if c.is(&["python", "python3", "py", "perl", "ruby", "node", "php", "deno", "bun"]) {
        let code_flags = ["-c", "-m", "-e", "-E", "-p", "-r"];
        return args.iter().all(|a| a.starts_with('-') && !code_flags.contains(&a.as_str())) || has_arg(args, &["-"]);
    }
    if c.is(&["powershell", "pwsh", "cmd"]) {
        return args.iter().all(|a| a.starts_with('-') || a.starts_with('/')) || has_arg(args, &["-"]);
    }
    false
}

/// `curl ... | sh` style pipelines among parsed commands.
pub(crate) fn pipeline_executes_download(
    commands: &[SimpleCommand],
    shell: Option<ShellKind>,
    unwrap: impl Fn(&Cmd) -> Vec<Cmd>,
) -> bool {
    let mut downloaded = false;
    for command in commands {
        if command.op_before != Some(crate::lexer::Op::Pipe) {
            downloaded = false;
        }
        let Some(cmd) = Cmd::new(&command.argv, &command.redirects, shell) else { continue };
        let variants = unwrap(&cmd);
        if downloaded && variants.iter().any(executes_stdin) {
            return true;
        }
        if variants.iter().any(is_downloader) {
            downloaded = true;
        }
    }
    false
}

pub(crate) fn pipe_to_shell_entry() -> (&'static str, Decision, &'static str) {
    (PIPE_TO_SHELL_DESC, Decision::Prompt, PIPE_TO_SHELL_WHY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_paths() {
        for p in [
            "/",
            "/*",
            "//",
            "/.",
            "~",
            "~/",
            "~/*",
            "$HOME",
            "${HOME}",
            "$HOME/",
            "C:\\",
            "C:/",
            "c:",
            "C:\\*",
            "\\",
            "$env:USERPROFILE",
            "%USERPROFILE%",
            "C:\\Users\\alice",
            "C:\\Users\\alice\\",
            "C:\\Users",
            "C:\\Windows",
            "C:\\Program Files",
            "/usr",
            "/etc/",
            "/home/bob",
            "/Users/carol",
            "/c",
            "/c/",
            "/mnt/c",
        ] {
            assert!(is_critical_path(p), "{p}");
        }
        for p in [
            "",
            "build",
            "./build",
            "~/projects",
            "$HOME/projects/x",
            "C:\\Users\\alice\\code\\app",
            "/home/bob/src",
            "/tmp/x",
            "C:\\temp",
            "node_modules",
            ".",
        ] {
            assert!(!is_critical_path(p), "{p}");
        }
    }

    #[test]
    fn ps_param_abbreviations() {
        let args: Vec<String> = ["-Rec", "-Force:$true"].iter().map(|s| s.to_string()).collect();
        assert!(ps_param(&args, "-recurse", 2));
        assert!(ps_param(&args, "-force", 2));
        assert!(!ps_param(&args, "-confirm", 2));
    }

    #[test]
    fn ps_target_extraction() {
        let args: Vec<String> = ["-Path", "a,b", "-Filter", "*.txt", "c", "-LiteralPath:d", "-Recurse"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(ps_targets(&args), vec!["a", "b", "c", "d"]);
    }
}
