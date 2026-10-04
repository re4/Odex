//! `odex-execpolicy`: classify shell commands for approvals.
//!
//! * [`split_commands`] splits a command line (bash/sh/zsh, PowerShell or
//!   cmd syntax) into simple commands, or returns `None` when it uses
//!   constructs that cannot be analysed statically.
//! * [`Policy`] holds prefix rules (built-in dangerous patterns plus user
//!   `*.toml` rule files) and produces an [`Evaluation`]: decision, matched
//!   rules, read-only / write / network heuristics.
//! * [`approval_prefix`] and [`append_allow_rule`] implement "don't ask
//!   again" amendments.
//!
//! Rules file format:
//!
//! ```toml
//! [[rule]]
//! prefix = ["git", "push"]
//! decision = "prompt"   # allow | prompt | forbid
//! justification = "Publishing commits"
//! # pattern = "regex matched against the full simple command"
//! ```

mod builtins;
mod heuristics;
mod lexer;
mod normalize;
mod policy;
mod rules;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use policy::{Evaluation, Policy};
pub use rules::{append_allow_rule, approval_prefix, parse_rules_toml, Rule};

/// Outcome of a rule. Ordered from least to most restrictive, so the most
/// restrictive of several decisions is their `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    #[serde(alias = "ask")]
    Prompt,
    #[serde(alias = "deny")]
    Forbid,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Prompt => "prompt",
            Decision::Forbid => "forbid",
        }
    }
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Decision {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(Decision::Allow),
            "prompt" | "ask" => Ok(Decision::Prompt),
            "forbid" | "deny" => Ok(Decision::Forbid),
            other => Err(format!("unknown decision '{other}' (expected allow, prompt or forbid)")),
        }
    }
}

/// The shell a command line is written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShellKind {
    PowerShell,
    Cmd,
    Bash,
    Sh,
    Zsh,
}

impl ShellKind {
    /// From a shell name or path: `powershell`/`pwsh` -> PowerShell, `cmd`
    /// -> Cmd, `bash` -> Bash, `zsh` -> Zsh; `sh`, `dash`, `ksh` and anything
    /// unknown -> Sh (POSIX parsing).
    pub fn from_name(s: &str) -> Self {
        let name = normalize::normalize_program(s).to_ascii_lowercase();
        match name.as_str() {
            "powershell" | "pwsh" | "powershell_ise" | "pwsh-preview" => ShellKind::PowerShell,
            "cmd" => ShellKind::Cmd,
            "bash" | "git-bash" => ShellKind::Bash,
            "zsh" => ShellKind::Zsh,
            _ => ShellKind::Sh,
        }
    }

    /// PowerShell on Windows, Bash elsewhere.
    pub fn platform_default() -> Self {
        if cfg!(windows) {
            ShellKind::PowerShell
        } else {
            ShellKind::Bash
        }
    }

    pub fn is_posix(self) -> bool {
        matches!(self, ShellKind::Bash | ShellKind::Sh | ShellKind::Zsh)
    }
}

/// Split a command line into simple commands (argv lists).
///
/// Splits on `&&`, `||`, `;`, `|`, `&` and newlines; handles quoting and
/// escapes per shell; drops comments, redirections and (POSIX) leading
/// `NAME=value` assignments; analyses PowerShell `{ ... }` script blocks
/// (their commands follow the enclosing command) and skips PowerShell
/// expression segments (`$x`, literals). Returns `None` for constructs that
/// cannot be analysed: subshells, `$(...)`, backticks (POSIX), process
/// substitution, PowerShell `(...)`, `$(...)`, `@(...)`, cmd `( ... )`.
pub fn split_commands(command: &str, shell: ShellKind) -> Option<Vec<Vec<String>>> {
    let commands = lexer::parse_commands(command, shell, false).ok()?;
    let mut out = Vec::new();
    lexer::flatten_argvs(&commands, &mut out);
    Some(out)
}

/// Whether one simple command is a known read-only command (`ls`, `cat`,
/// `rg`, `git status`, `Get-ChildItem`, `<prog> --version`, ...). Options
/// that write files or run other programs (`find -delete`, `sort -o`,
/// `git branch -D`, `rg --pre`) disqualify it.
pub fn is_known_safe(argv: &[String]) -> bool {
    normalize::Cmd::new(argv, &[], None).is_some_and(|cmd| heuristics::is_safe(&cmd))
}
