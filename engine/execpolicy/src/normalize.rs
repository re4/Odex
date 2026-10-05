//! Program-name normalisation, PowerShell aliases and the normalised
//! command view that rules and heuristics operate on.

use crate::lexer::Redirect;
use crate::ShellKind;

/// Last path component of a program path (handles `/` and `\`).
pub(crate) fn basename(program: &str) -> &str {
    program.rsplit(['/', '\\']).next().unwrap_or(program)
}

const STRIPPED_EXTENSIONS: [&str; 4] = [".exe", ".cmd", ".bat", ".com"];

/// Basename without `.exe` / `.cmd` / `.bat` / `.com` (original case).
pub(crate) fn normalize_program(program: &str) -> String {
    let base = basename(program.trim());
    let lower = base.to_ascii_lowercase();
    for ext in STRIPPED_EXTENSIONS {
        if lower.len() > ext.len() && lower.ends_with(ext) {
            return base[..base.len() - ext.len()].to_string();
        }
    }
    base.to_string()
}

fn has_stripped_extension(program: &str) -> bool {
    let lower = basename(program).to_ascii_lowercase();
    STRIPPED_EXTENSIONS.iter().any(|ext| lower.len() > ext.len() && lower.ends_with(ext))
}

/// Built-in PowerShell aliases (lowercase alias -> cmdlet).
///
/// `curl` / `wget` are the Windows PowerShell 5.1 aliases for
/// `Invoke-WebRequest`; `curl.exe` is left alone.
pub(crate) fn powershell_alias(alias_lc: &str) -> Option<&'static str> {
    Some(match alias_lc {
        "rm" | "del" | "ri" | "erase" | "rd" | "rmdir" => "Remove-Item",
        "ls" | "dir" | "gci" => "Get-ChildItem",
        "cat" | "gc" | "type" => "Get-Content",
        "iwr" | "curl" | "wget" => "Invoke-WebRequest",
        "irm" => "Invoke-RestMethod",
        "iex" => "Invoke-Expression",
        "mv" | "move" | "mi" => "Move-Item",
        "cp" | "copy" | "cpi" => "Copy-Item",
        "echo" | "write" => "Write-Output",
        "pwd" | "gl" => "Get-Location",
        "cd" | "sl" | "chdir" => "Set-Location",
        "pushd" => "Push-Location",
        "popd" => "Pop-Location",
        "ni" | "md" | "mkdir" => "New-Item",
        "sc" => "Set-Content",
        "ac" => "Add-Content",
        "clc" => "Clear-Content",
        "kill" | "spps" => "Stop-Process",
        "ps" | "gps" => "Get-Process",
        "sls" => "Select-String",
        "select" => "Select-Object",
        "where" | "?" => "Where-Object",
        "foreach" | "%" => "ForEach-Object",
        "sort" => "Sort-Object",
        "measure" => "Measure-Object",
        "group" => "Group-Object",
        "ft" => "Format-Table",
        "fl" => "Format-List",
        "fw" => "Format-Wide",
        "fc" => "Format-Custom",
        "gi" => "Get-Item",
        "si" => "Set-Item",
        "cli" => "Clear-Item",
        "gcm" => "Get-Command",
        "man" | "help" => "Get-Help",
        "tee" => "Tee-Object",
        "start" | "saps" => "Start-Process",
        "ren" | "rni" => "Rename-Item",
        "cls" | "clear" => "Clear-Host",
        "h" | "history" | "ghy" => "Get-History",
        "r" | "ihy" => "Invoke-History",
        "diff" | "compare" => "Compare-Object",
        "icm" => "Invoke-Command",
        "ipmo" => "Import-Module",
        "gm" => "Get-Member",
        "gv" => "Get-Variable",
        "sv" | "set" => "Set-Variable",
        "sp" => "Set-ItemProperty",
        "gp" => "Get-ItemProperty",
        "rp" => "Remove-ItemProperty",
        "ii" => "Invoke-Item",
        "rvpa" => "Resolve-Path",
        "cvpa" => "Convert-Path",
        "gal" => "Get-Alias",
        "gsv" => "Get-Service",
        "spsv" => "Stop-Service",
        "sasv" => "Start-Service",
        "epcsv" => "Export-Csv",
        "ipcsv" => "Import-Csv",
        "oh" => "Out-Host",
        "gdr" => "Get-PSDrive",
        _ => return None,
    })
}

const POWERSHELL_VERBS: &[&str] = &[
    "add",
    "approve",
    "assert",
    "backup",
    "block",
    "checkpoint",
    "clear",
    "close",
    "compare",
    "complete",
    "compress",
    "confirm",
    "connect",
    "convert",
    "convertfrom",
    "convertto",
    "copy",
    "debug",
    "deny",
    "disable",
    "disconnect",
    "dismount",
    "edit",
    "enable",
    "enter",
    "exit",
    "expand",
    "export",
    "find",
    "foreach",
    "format",
    "get",
    "grant",
    "group",
    "hide",
    "import",
    "initialize",
    "install",
    "invoke",
    "join",
    "limit",
    "lock",
    "measure",
    "merge",
    "mount",
    "move",
    "new",
    "open",
    "optimize",
    "out",
    "ping",
    "pop",
    "protect",
    "publish",
    "push",
    "read",
    "receive",
    "redo",
    "register",
    "remove",
    "rename",
    "repair",
    "request",
    "reset",
    "resize",
    "resolve",
    "restart",
    "restore",
    "resume",
    "revoke",
    "save",
    "search",
    "select",
    "send",
    "set",
    "show",
    "skip",
    "sort",
    "split",
    "start",
    "step",
    "stop",
    "submit",
    "suspend",
    "switch",
    "sync",
    "tee",
    "test",
    "trace",
    "unblock",
    "undo",
    "uninstall",
    "unlock",
    "unprotect",
    "unpublish",
    "unregister",
    "update",
    "use",
    "wait",
    "watch",
    "where",
    "write",
];

/// `Verb-Noun` with an approved PowerShell verb (case-insensitive).
pub(crate) fn is_cmdlet_name(name: &str) -> bool {
    let Some((verb, noun)) = name.split_once('-') else { return false };
    !noun.is_empty()
        && noun.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && POWERSHELL_VERBS.contains(&verb.to_ascii_lowercase().as_str())
}

/// A simple command with its program normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cmd {
    /// `argv[0]` is the canonical program: basename without extension, with
    /// PowerShell aliases resolved in a PowerShell context.
    pub argv: Vec<String>,
    /// Basename without extension, before alias resolution.
    pub raw: String,
    pub prog_lc: String,
    pub raw_lc: String,
    /// Parsed in a PowerShell context (aliases applied, parameters case-insensitive).
    pub powershell: bool,
    pub redirects: Vec<Redirect>,
}

impl Cmd {
    pub(crate) fn new(argv: &[String], redirects: &[Redirect], shell: Option<ShellKind>) -> Option<Cmd> {
        let first = argv.first()?;
        let raw = normalize_program(first);
        let powershell = shell == Some(ShellKind::PowerShell);
        let raw_lc = raw.to_ascii_lowercase();
        let canonical = if powershell && !has_stripped_extension(first) {
            powershell_alias(&raw_lc).map(str::to_string).unwrap_or_else(|| raw.clone())
        } else {
            raw.clone()
        };
        let mut normalized = Vec::with_capacity(argv.len());
        normalized.push(canonical.clone());
        normalized.extend(argv[1..].iter().cloned());
        Some(Cmd {
            argv: normalized,
            prog_lc: canonical.to_ascii_lowercase(),
            raw,
            raw_lc,
            powershell,
            redirects: redirects.to_vec(),
        })
    }

    pub(crate) fn args(&self) -> &[String] {
        &self.argv[1..]
    }

    /// Whether `-Param` style arguments compare case-insensitively.
    pub(crate) fn params_case_insensitive(&self) -> bool {
        self.powershell || is_cmdlet_name(&self.argv[0])
    }

    /// Program is one of `names` (lowercase), by canonical or raw name.
    pub(crate) fn is(&self, names: &[&str]) -> bool {
        names.contains(&self.prog_lc.as_str()) || names.contains(&self.raw_lc.as_str())
    }

    /// The `git` subcommand and its arguments, skipping global options.
    pub(crate) fn git(&self) -> Option<(&str, &[String])> {
        if self.prog_lc != "git" {
            return None;
        }
        git_subcommand(self.args())
    }

    /// First argument that is not an option.
    pub(crate) fn subcommand(&self) -> Option<&str> {
        self.args().iter().find(|a| !a.starts_with('-')).map(String::as_str)
    }
}

/// Skip git's global options (`-C dir`, `-c k=v`, `--no-pager`, ...).
pub(crate) fn git_subcommand(args: &[String]) -> Option<(&str, &[String])> {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => i += 2,
            a if a.starts_with('-') => i += 1,
            a => return Some((a, &args[i + 1..])),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn program_normalisation() {
        assert_eq!(normalize_program("C:\\Program Files\\Git\\cmd\\git.exe"), "git");
        assert_eq!(normalize_program("/usr/bin/python3"), "python3");
        assert_eq!(normalize_program("npm.CMD"), "npm");
        assert_eq!(normalize_program("build.bat"), "build");
        assert_eq!(normalize_program(".exe"), ".exe");
        assert_eq!(normalize_program("script.ps1"), "script.ps1");
    }

    #[test]
    fn alias_resolution_only_in_powershell() {
        let ps = Cmd::new(&s(&["del", "x"]), &[], Some(ShellKind::PowerShell)).unwrap();
        assert_eq!(ps.argv[0], "Remove-Item");
        assert_eq!(ps.prog_lc, "remove-item");
        assert_eq!(ps.raw_lc, "del");
        let cmd = Cmd::new(&s(&["del", "x"]), &[], Some(ShellKind::Cmd)).unwrap();
        assert_eq!(cmd.argv[0], "del");
        let exe = Cmd::new(&s(&["curl.exe", "-s", "x"]), &[], Some(ShellKind::PowerShell)).unwrap();
        assert_eq!(exe.prog_lc, "curl");
        let alias = Cmd::new(&s(&["curl", "x"]), &[], Some(ShellKind::PowerShell)).unwrap();
        assert_eq!(alias.prog_lc, "invoke-webrequest");
        for (alias, target) in [
            ("ls", "Get-ChildItem"),
            ("gci", "Get-ChildItem"),
            ("cat", "Get-Content"),
            ("type", "Get-Content"),
            ("ri", "Remove-Item"),
            ("iwr", "Invoke-WebRequest"),
            ("irm", "Invoke-RestMethod"),
            ("iex", "Invoke-Expression"),
        ] {
            assert_eq!(powershell_alias(alias), Some(target));
        }
    }

    #[test]
    fn cmdlet_detection() {
        assert!(is_cmdlet_name("Get-ChildItem"));
        assert!(is_cmdlet_name("remove-item"));
        assert!(!is_cmdlet_name("git-lfs"));
        assert!(!is_cmdlet_name("clang-format"));
        assert!(!is_cmdlet_name("Get-"));
        assert!(!is_cmdlet_name("ls"));
    }

    #[test]
    fn git_subcommand_skips_globals() {
        assert_eq!(git_subcommand(&s(&["-C", "repo", "--no-pager", "log", "-1"])), Some(("log", &s(&["-1"])[..])));
        assert_eq!(git_subcommand(&s(&["--version"])), None);
    }
}
