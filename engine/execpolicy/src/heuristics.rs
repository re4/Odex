//! Known-safe (read-only) commands and write / network heuristics.

use crate::builtins::ps_param;
use crate::normalize::{git_subcommand, Cmd};

fn has(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|a| names.contains(&a.as_str()))
}

fn has_prefix(args: &[String], prefixes: &[&str]) -> bool {
    args.iter().any(|a| prefixes.iter().any(|p| a.starts_with(p)))
}

fn positional_count(args: &[String]) -> usize {
    args.iter().filter(|a| !a.starts_with('-')).count()
}

/// Help / version probes: `<prog> --version`, `-V`, `--help`, `/?`.
fn is_version_or_help(args: &[String]) -> bool {
    args.len() == 1 && matches!(args[0].as_str(), "--version" | "-V" | "--help" | "-version" | "-help" | "/?")
}

/// Whether a normalised command is a known read-only command.
pub(crate) fn is_safe(cmd: &Cmd) -> bool {
    let args = cmd.args();
    if is_version_or_help(args) {
        return true;
    }
    let name = cmd.prog_lc.as_str();
    match name {
        "ls" | "dir" | "pwd" | "cat" | "type" | "head" | "tail" | "wc" | "echo" | "printf" | "grep" | "egrep"
        | "fgrep" | "which" | "where" | "whoami" | "stat" | "du" | "df" | "uname" | "cut" | "diff" | "cmp" | "nl"
        | "true" | "false" | "basename" | "dirname" | "realpath" | "readlink" | "id" | "groups" | "tac" | "rev"
        | "seq" | "column" | "md5sum" | "sha1sum" | "sha256sum" | "sha512sum" | "b2sum" | "cksum" | "test" | "["
        | "printenv" | "arch" | "nproc" | "uptime" | "locale" | "tty" | "findstr" | "ver" | "vol" | "cd" | "pushd"
        | "popd" | "chdir" | "tr" | "fold" | "paste" | "join" | "comm" | "od" | "hexdump" | "strings" | "jq" | "ps"
        | "pgrep" | "free" | "who" | "w" | "wslpath" | "cygpath" => true,
        "file" => !has_prefix(args, &["-C"]),
        "tree" => !has(args, &["-o"]),
        "rg" => !args.iter().any(|a| a == "--pre" || a.starts_with("--pre=")),
        "find" => !has_prefix(
            args,
            &["-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls"],
        ),
        "sort" => !args.iter().any(|a| a == "-o" || a.starts_with("--output") || (a.starts_with("-o") && a.len() > 2)),
        "date" => {
            !has_prefix(args, &["-s", "--set"])
                && args.iter().filter(|a| !a.starts_with('-')).all(|a| a.starts_with('+'))
        }
        "env" => args.is_empty(),
        "uniq" | "xxd" => positional_count(args) <= 1,
        "hostname" => args.iter().all(|a| a.starts_with('-')),
        "git" => git_is_safe(args),
        // PowerShell (canonical names, plus common aliases when not in a PowerShell context).
        "get-childitem" | "get-content" | "get-item" | "get-itemproperty" | "get-location" | "select-string"
        | "test-path" | "get-command" | "get-process" | "get-service" | "get-date" | "get-help" | "get-member"
        | "get-alias" | "get-filehash" | "get-acl" | "get-history" | "get-psdrive" | "get-culture" | "get-host"
        | "get-variable" | "get-module" | "get-uptime" | "get-computerinfo" | "get-psprovider" | "get-timezone"
        | "get-unique" | "measure-object" | "select-object" | "where-object" | "sort-object" | "group-object"
        | "compare-object" | "resolve-path" | "split-path" | "join-path" | "convert-path" | "write-output"
        | "write-host" | "out-string" | "out-host" | "out-null" | "convertto-json" | "convertfrom-json"
        | "convertto-csv" | "convertfrom-csv" | "test-json" | "set-location" | "push-location" | "pop-location"
        | "format-table" | "format-list" | "format-wide" | "format-custom" | "format-hex" | "gci" | "gc" | "gi"
        | "gl" | "sls" | "gcm" | "gps" | "gal" | "gm" | "gv" | "rvpa" | "measure" | "select" | "ft" | "fl" | "fw"
        | "gsv" | "ghy" => true,
        "tee-object" => ps_param(args, "-variable", 2) && !ps_param(args, "-filepath", 2),
        _ => false,
    }
}

/// Read-only git invocations.
fn git_is_safe(args: &[String]) -> bool {
    // Global options that could run arbitrary programs (`-c core.pager=...`) are not safe.
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        match args[i].as_str() {
            "-C" | "--git-dir" | "--work-tree" => i += 2,
            "--no-pager" | "-P" | "--no-optional-locks" | "--literal-pathspecs" | "--no-replace-objects" | "--bare" => {
                i += 1
            }
            a if a.starts_with("--git-dir=") || a.starts_with("--work-tree=") => i += 1,
            "--version" | "--help" => return args.len() == i + 1,
            _ => return false,
        }
    }
    let Some((sub, rest)) = git_subcommand(&args[i.min(args.len())..]) else { return false };
    let writes_output = rest.iter().any(|a| a == "--output" || a.starts_with("--output="));
    match sub {
        "status" | "show" | "rev-parse" | "ls-files" | "blame" | "describe" | "shortlog" | "ls-tree" | "cat-file"
        | "rev-list" | "show-ref" | "merge-base" | "name-rev" | "count-objects" | "whatchanged" | "version"
        | "help" | "grep" | "annotate" | "check-ignore" | "check-attr" | "var" | "for-each-ref" | "show-branch"
        | "cherry" | "range-diff" | "log" | "diff" => !writes_output,
        "branch" => list_only(rest, &BRANCH_LIST_FLAGS, &BRANCH_VALUE_FLAGS, &["-l", "--list"]),
        "tag" => list_only(rest, &TAG_LIST_FLAGS, &TAG_VALUE_FLAGS, &["-l", "--list"]),
        "remote" => match rest.first().map(String::as_str) {
            None => true,
            Some("-v") | Some("--verbose") => rest.len() == 1,
            Some("get-url") => true,
            _ => false,
        },
        "config" => git_config_is_read(rest),
        "stash" => matches!(rest.first().map(String::as_str), Some("list") | Some("show")),
        "reflog" => !matches!(rest.first().map(String::as_str), Some("expire") | Some("delete")),
        "worktree" => rest.first().is_some_and(|s| s == "list"),
        "submodule" => matches!(rest.first().map(String::as_str), None | Some("status") | Some("summary")),
        "notes" => matches!(rest.first().map(String::as_str), None | Some("list") | Some("show")),
        _ => false,
    }
}

const BRANCH_LIST_FLAGS: [&str; 20] = [
    "-a",
    "--all",
    "-r",
    "--remotes",
    "-v",
    "-vv",
    "--verbose",
    "--show-current",
    "--no-color",
    "--color",
    "-l",
    "--list",
    "-i",
    "--ignore-case",
    "--omit-empty",
    "--column",
    "--no-column",
    "--merged",
    "--no-merged",
    "--contains",
];
const BRANCH_VALUE_FLAGS: [&str; 7] =
    ["--merged", "--no-merged", "--contains", "--no-contains", "--points-at", "--sort", "--format"];
const TAG_LIST_FLAGS: [&str; 13] = [
    "-l",
    "--list",
    "-n",
    "--contains",
    "--no-contains",
    "--merged",
    "--no-merged",
    "--points-at",
    "--column",
    "--no-column",
    "-i",
    "--ignore-case",
    "--color",
];
const TAG_VALUE_FLAGS: [&str; 7] =
    ["--contains", "--no-contains", "--merged", "--no-merged", "--points-at", "--sort", "--format"];

/// `git branch` / `git tag` in listing mode only. Positional arguments are
/// allowed only as list patterns or as values of filter options.
fn list_only(rest: &[String], list_flags: &[&str], value_flags: &[&str], list_mode_flags: &[&str]) -> bool {
    let list_mode = rest.iter().any(|a| list_mode_flags.contains(&a.as_str()));
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        if arg.starts_with('-') {
            let (name, inline_value) = match arg.split_once('=') {
                Some((name, _)) => (name, true),
                None => (arg, false),
            };
            let is_count = name.starts_with("-n") && name[2..].chars().all(|c| c.is_ascii_digit());
            if value_flags.contains(&name) {
                if !inline_value && rest.get(i + 1).is_some_and(|next| !next.starts_with('-')) {
                    i += 1;
                }
            } else if !(list_flags.contains(&name) || is_count) {
                return false;
            }
        } else if !list_mode {
            // A bare name creates a branch / tag.
            return false;
        }
        i += 1;
    }
    true
}

fn git_config_is_read(rest: &[String]) -> bool {
    let scope = [
        "--global",
        "--local",
        "--system",
        "--worktree",
        "--show-origin",
        "--show-scope",
        "--null",
        "-z",
        "--name-only",
        "--includes",
    ];
    let remaining: Vec<&str> = rest.iter().map(String::as_str).filter(|a| !scope.contains(a)).collect();
    match remaining.first() {
        Some(&("--get" | "--get-all" | "--get-regexp" | "--get-urlmatch")) => true,
        Some(&("--list" | "-l")) => remaining.len() == 1,
        Some(first) if !first.starts_with('-') => remaining.len() == 1,
        _ => false,
    }
}

/// Whether the command (ignoring redirections) is likely to modify files.
pub(crate) fn writes_likely(cmd: &Cmd) -> bool {
    let args = cmd.args();
    let sub = cmd.subcommand();
    match cmd.prog_lc.as_str() {
        "rm" | "rmdir" | "mv" | "cp" | "del" | "erase" | "rd" | "move" | "copy" | "xcopy" | "robocopy" | "ren"
        | "rename" | "mkdir" | "md" | "touch" | "ln" | "chmod" | "chown" | "chgrp" | "truncate" | "shred" | "dd"
        | "install" | "mklink" | "unlink" | "rsync" | "scp" | "patch" | "apply_patch" | "applypatch" | "split"
        | "csplit" | "7z" | "zip" | "gzip" | "gunzip" | "bzip2" | "xz" | "unxz" | "zstd" | "mkfs" | "attrib"
        | "icacls" | "takeown" => true,
        "tar" => {
            args.first().is_some_and(|a| a.trim_start_matches('-').chars().any(|c| matches!(c, 'x' | 'c' | 'r' | 'u')))
                || has(args, &["--extract", "--create", "--append", "--update"])
        }
        "unzip" => !has(args, &["-l", "-t", "-v", "-Z"]),
        "tee" => positional_count(args) > 0,
        "sed" => args.iter().any(|a| {
            a == "--in-place"
                || a.starts_with("--in-place=")
                || (a.starts_with('-') && !a.starts_with("--") && a.contains('i'))
        }),
        "perl" => args.iter().any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('i')),
        "git" => git_writes(args),
        "remove-item"
        | "move-item"
        | "copy-item"
        | "rename-item"
        | "new-item"
        | "set-content"
        | "add-content"
        | "clear-content"
        | "out-file"
        | "set-item"
        | "set-itemproperty"
        | "new-itemproperty"
        | "remove-itemproperty"
        | "rename-itemproperty"
        | "clear-item"
        | "expand-archive"
        | "compress-archive"
        | "export-csv"
        | "export-clixml"
        | "set-acl" => true,
        "tee-object" => !ps_param(args, "-variable", 2),
        "invoke-webrequest" | "invoke-restmethod" => ps_param(args, "-outfile", 2),
        "curl" => args.iter().any(|a| {
            a == "-o"
                || a == "-O"
                || a.starts_with("--output")
                || a == "--remote-name"
                || a == "--remote-name-all"
                || (a.starts_with('-') && !a.starts_with("--") && (a.contains('o') || a.contains('O')))
        }),
        "wget" => {
            let to_stdout = args
                .iter()
                .any(|a| matches!(a.as_str(), "-O-" | "-qO-" | "--output-document=-" | "--spider"))
                || args.windows(2).any(|w| matches!(w[0].as_str(), "-O" | "-qO" | "--output-document") && w[1] == "-");
            !to_stdout
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            (cmd.prog_lc == "yarn" && args.is_empty())
                || matches!(
                    sub,
                    Some(
                        "install"
                            | "i"
                            | "add"
                            | "remove"
                            | "rm"
                            | "uninstall"
                            | "un"
                            | "update"
                            | "up"
                            | "upgrade"
                            | "ci"
                            | "init"
                            | "create"
                            | "link"
                            | "prune"
                            | "dedupe"
                            | "version"
                            | "pack"
                    )
                )
        }
        "pip" | "pip3" => matches!(sub, Some("install" | "uninstall" | "download" | "wheel")),
        "python" | "python3" | "py" => {
            args.first().is_some_and(|a| a == "-m")
                && match args.get(1).map(String::as_str) {
                    Some("pip") => {
                        matches!(args.get(2).map(String::as_str), Some("install" | "uninstall" | "download"))
                    }
                    Some("venv" | "ensurepip" | "black") => true,
                    _ => false,
                }
        }
        "uv" => {
            matches!(sub, Some("add" | "remove" | "sync" | "lock" | "venv" | "init"))
                || (sub == Some("pip") && has(args, &["install", "uninstall"]))
        }
        "cargo" => match sub {
            Some(
                "install" | "uninstall" | "add" | "remove" | "rm" | "update" | "new" | "init" | "fix" | "vendor"
                | "generate-lockfile",
            ) => true,
            Some("fmt") => !has(args, &["--check"]),
            Some("clippy") => has(args, &["--fix"]),
            _ => false,
        },
        "go" => matches!(sub, Some("get" | "install" | "mod" | "generate" | "fmt")),
        "gofmt" => has_prefix(args, &["-w"]),
        "rustfmt" => !has(args, &["--check"]),
        "prettier" => has(args, &["--write", "-w"]),
        "eslint" => has(args, &["--fix"]),
        "black" => !has(args, &["--check", "--diff"]),
        "ruff" => has(args, &["--fix"]) || (sub == Some("format") && !has(args, &["--check", "--diff"])),
        "dotnet" => matches!(sub, Some("new" | "add" | "remove" | "restore" | "publish" | "tool")),
        "winget" | "choco" | "scoop" | "brew" | "apt" | "apt-get" | "dnf" | "yum" | "pacman" | "snap" => {
            matches!(sub, Some("install" | "uninstall" | "remove" | "upgrade" | "update" | "-S" | "-R" | "-Syu"))
                || has_prefix(args, &["-S", "-R"])
        }
        _ => false,
    }
}

fn git_writes(args: &[String]) -> bool {
    let Some((sub, rest)) = git_subcommand(args) else { return false };
    match sub {
        "add" | "commit" | "checkout" | "switch" | "reset" | "merge" | "rebase" | "pull" | "fetch" | "cherry-pick"
        | "revert" | "apply" | "am" | "clean" | "rm" | "mv" | "restore" | "init" | "clone" | "gc" | "prune"
        | "update-ref" | "update-index" | "filter-branch" | "replace" | "worktree" | "submodule" => {
            !(sub == "worktree" && rest.first().is_some_and(|s| s == "list"))
                && !(sub == "submodule"
                    && matches!(rest.first().map(String::as_str), None | Some("status" | "summary")))
        }
        "stash" => !matches!(rest.first().map(String::as_str), Some("list" | "show")),
        "branch" => !list_only(rest, &BRANCH_LIST_FLAGS, &BRANCH_VALUE_FLAGS, &["-l", "--list"]),
        "tag" => !list_only(rest, &TAG_LIST_FLAGS, &TAG_VALUE_FLAGS, &["-l", "--list"]),
        "config" => !git_config_is_read(rest),
        "notes" => matches!(rest.first().map(String::as_str), Some("add" | "append" | "edit" | "remove" | "copy")),
        _ => false,
    }
}

/// Whether the command is likely to use the network.
pub(crate) fn network_likely(cmd: &Cmd) -> bool {
    let args = cmd.args();
    let sub = cmd.subcommand();
    if args.iter().any(|a| {
        let lower = a.to_ascii_lowercase();
        ["http://", "https://", "ftp://", "ssh://", "git@", "git://"].iter().any(|p| lower.starts_with(p))
    }) {
        return true;
    }
    match cmd.prog_lc.as_str() {
        "curl" | "wget" | "invoke-webrequest" | "invoke-restmethod" | "start-bitstransfer" | "ssh" | "scp" | "sftp"
        | "ftp" | "telnet" | "nc" | "ncat" | "netcat" | "ping" | "test-connection" | "test-netconnection"
        | "nslookup" | "dig" | "host" | "resolve-dnsname" | "tracert" | "traceroute" | "gh" | "aws" | "az"
        | "gcloud" | "kubectl" | "helm" | "npx" | "pnpx" | "bunx" | "send-mailmessage" | "aria2c" | "http" | "xh" => {
            true
        }
        "rsync" => positionals_have_remote(args),
        "npm" | "pnpm" | "yarn" | "bun" => {
            (cmd.prog_lc == "yarn" && args.is_empty())
                || matches!(
                    sub,
                    Some(
                        "install"
                            | "i"
                            | "add"
                            | "ci"
                            | "update"
                            | "up"
                            | "upgrade"
                            | "publish"
                            | "unpublish"
                            | "view"
                            | "info"
                            | "outdated"
                            | "audit"
                            | "login"
                            | "whoami"
                            | "search"
                            | "create"
                            | "dlx"
                            | "exec"
                            | "x"
                            | "init"
                    )
                )
        }
        "pip" | "pip3" => matches!(sub, Some("install" | "download" | "search" | "index")),
        "python" | "python3" | "py" => {
            args.first().is_some_and(|a| a == "-m")
                && args.get(1).is_some_and(|m| m == "pip")
                && matches!(args.get(2).map(String::as_str), Some("install" | "download"))
        }
        "uv" | "uvx" => {
            cmd.prog_lc == "uvx"
                || matches!(sub, Some("add" | "sync" | "lock" | "tool" | "python" | "run"))
                || (sub == Some("pip") && has(args, &["install"]))
        }
        "cargo" => matches!(
            sub,
            Some(
                "install"
                    | "add"
                    | "update"
                    | "fetch"
                    | "publish"
                    | "search"
                    | "login"
                    | "yank"
                    | "owner"
                    | "vendor"
                    | "generate-lockfile"
            )
        ),
        "git" => match git_subcommand(args) {
            Some(("clone" | "fetch" | "pull" | "push" | "ls-remote", _)) => true,
            Some(("remote", rest)) => rest.first().is_some_and(|s| s == "update" || s == "show"),
            Some(("submodule", rest)) => rest.first().is_some_and(|s| s == "update" || s == "sync"),
            _ => false,
        },
        "docker" | "podman" => matches!(sub, Some("pull" | "push" | "login" | "search")),
        "go" => matches!(sub, Some("get" | "install" | "mod")),
        "gem" => matches!(sub, Some("install" | "push" | "update")),
        "bundle" => matches!(sub, Some("install" | "update")),
        "composer" => matches!(sub, Some("install" | "require" | "update")),
        "dotnet" => matches!(sub, Some("restore" | "add" | "tool")) || (sub == Some("nuget") && has(args, &["push"])),
        "nuget" => matches!(sub, Some("install" | "restore" | "push")),
        "brew" | "apt" | "apt-get" | "dnf" | "yum" | "pacman" | "winget" | "choco" | "scoop" | "snap" => {
            matches!(sub, Some("install" | "update" | "upgrade" | "search" | "-S" | "-Syu"))
                || has_prefix(args, &["-S"])
        }
        "conda" | "mamba" => matches!(sub, Some("install" | "create" | "update")),
        "rustup" => matches!(sub, Some("update" | "install" | "toolchain" | "target" | "component")),
        "deno" => matches!(sub, Some("install" | "cache" | "add")),
        "terraform" => matches!(sub, Some("init" | "plan" | "apply" | "destroy")),
        "ollama" => matches!(sub, Some("pull" | "push")),
        _ => false,
    }
}

fn positionals_have_remote(args: &[String]) -> bool {
    args.iter().filter(|a| !a.starts_with('-')).any(|a| match a.find(':') {
        Some(1) => false, // Windows drive letter
        Some(_) => true,
        None => false,
    })
}
