//! User rules: TOML files of `[[rule]]` entries and "don't ask again"
//! amendments.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Deserialize;

use crate::normalize::{git_subcommand, normalize_program};
use crate::Decision;

/// A prefix rule. `prefix[0]` is compared as a normalised program name;
/// the remaining entries must equal the following arguments.
#[derive(Debug, Clone)]
pub struct Rule {
    pub prefix: Vec<String>,
    pub decision: Decision,
    pub justification: Option<String>,
    /// Optional regex matched against the whole simple command
    /// (`argv.join(" ")`, program normalised). With an empty `prefix` the
    /// pattern alone decides.
    pub pattern: Option<Regex>,
    /// Where the rule came from (file path, `"runtime"`, ...).
    pub source: String,
}

impl Rule {
    pub fn new<I, S>(prefix: I, decision: Decision) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Rule {
            prefix: prefix.into_iter().map(Into::into).collect(),
            decision,
            justification: None,
            pattern: None,
            source: "runtime".to_string(),
        }
    }

    pub fn with_justification(mut self, justification: impl Into<String>) -> Self {
        self.justification = Some(justification.into());
        self
    }

    pub fn with_pattern(mut self, pattern: &str) -> Result<Self, regex::Error> {
        self.pattern = Some(Regex::new(pattern)?);
        Ok(self)
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Human-readable description used in [`crate::Evaluation::matched`].
    pub fn describe(&self) -> String {
        let prefix = if self.prefix.is_empty() { "*".to_string() } else { self.prefix.join(" ") };
        match &self.pattern {
            Some(pattern) => format!("{prefix} =~ /{}/ ({})", pattern.as_str(), self.source),
            None => format!("{prefix} ({})", self.source),
        }
    }

    /// Ordering key: longer prefixes are more specific, a pattern refines.
    pub(crate) fn specificity(&self) -> usize {
        self.prefix.len() * 2 + usize::from(self.pattern.is_some())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    #[serde(default)]
    rule: Vec<RuleToml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleToml {
    #[serde(default)]
    prefix: Vec<String>,
    decision: Decision,
    justification: Option<String>,
    pattern: Option<String>,
}

/// Parse a rules file. Invalid individual rules are skipped with a warning;
/// a syntax error rejects the whole file.
pub fn parse_rules_toml(text: &str, source: &str) -> (Vec<Rule>, Vec<String>) {
    let file: RulesFile = match toml::from_str(text) {
        Ok(file) => file,
        Err(e) => return (Vec::new(), vec![format!("{source}: {}", e.message())]),
    };
    let mut rules = Vec::new();
    let mut warnings = Vec::new();
    for (index, raw) in file.rule.into_iter().enumerate() {
        let n = index + 1;
        if raw.prefix.is_empty() && raw.pattern.is_none() {
            warnings.push(format!("{source}: rule #{n} has neither a prefix nor a pattern; ignored"));
            continue;
        }
        if raw.prefix.iter().any(|p| p.is_empty()) {
            warnings.push(format!("{source}: rule #{n} has an empty prefix element; ignored"));
            continue;
        }
        let pattern = match raw.pattern.as_deref().map(Regex::new).transpose() {
            Ok(pattern) => pattern,
            Err(e) => {
                warnings.push(format!("{source}: rule #{n} has an invalid pattern: {e}"));
                continue;
            }
        };
        rules.push(Rule {
            prefix: raw.prefix,
            decision: raw.decision,
            justification: raw.justification,
            pattern,
            source: source.to_string(),
        });
    }
    (rules, warnings)
}

/// `*.toml` files directly inside `dir`, sorted by name. A path that is
/// itself a `.toml` file is returned as-is. A missing directory is not an
/// error.
pub(crate) fn rule_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if dir.is_file() {
        return Ok(vec![dir.to_path_buf()]);
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("toml")))
        .collect();
    files.sort();
    Ok(files)
}

/// Append a `[[rule]]` with `prefix` and `decision` to a rules file,
/// creating the file (and its parent directories) when needed.
pub fn append_allow_rule(file: &Path, prefix: &[String], decision: Decision) -> io::Result<()> {
    if prefix.is_empty() || prefix.iter().any(|p| p.is_empty()) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "rule prefix must be non-empty"));
    }
    if let Some(parent) = file.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let existing = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut block = String::new();
    if !existing.is_empty() {
        if !existing.ends_with('\n') {
            block.push('\n');
        }
        block.push('\n');
    }
    let prefix_value = toml::Value::Array(prefix.iter().cloned().map(toml::Value::String).collect());
    let _ = writeln!(block, "[[rule]]");
    let _ = writeln!(block, "prefix = {prefix_value}");
    let _ = writeln!(block, "decision = \"{}\"", decision.as_str());
    let mut handle = OpenOptions::new().create(true).append(true).open(file)?;
    handle.write_all(block.as_bytes())?;
    handle.flush()
}

/// Tools whose first non-option argument is a subcommand worth remembering.
const SUBCOMMAND_TOOLS: &[&str] = &[
    "git",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "docker",
    "podman",
    "kubectl",
    "go",
    "dotnet",
    "pip",
    "pip3",
    "uv",
    "poetry",
    "rustup",
    "brew",
    "apt",
    "apt-get",
    "winget",
    "choco",
    "scoop",
    "terraform",
    "deno",
    "conda",
    "mvn",
    "gradle",
    "systemctl",
    "helm",
    "make",
    "just",
    "nx",
    "turbo",
    "gh",
    "az",
    "aws",
    "gcloud",
    "composer",
    "bundle",
    "gem",
    "mix",
    "flutter",
    "dart",
    "swift",
    "zig",
    "cmake",
    "ollama",
    "rake",
    "npx",
    "pnpx",
    "bunx",
    "uvx",
];

/// Tools with a second command level (`gh pr create`, `docker compose up`).
const TWO_LEVEL: &[&str] = &["gh", "az", "aws", "gcloud"];
const DOCKER_GROUPS: &[&str] = &["compose", "container", "image", "volume", "network", "system", "buildx", "builder"];
/// Subcommands followed by a script / binary name (`npm run test`).
const RUNNER_SUBCOMMANDS: &[&str] = &["run", "exec", "x", "dlx", "run-script"];
const GIT_GROUPS: &[&str] = &["stash", "remote", "submodule", "worktree", "notes", "lfs"];

const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "py",
    "node",
    "deno",
    "ruby",
    "perl",
    "php",
    "bash",
    "sh",
    "zsh",
    "pwsh",
    "powershell",
    "rscript",
    "lua",
    "tsx",
    "ts-node",
];

/// Options of interpreters that take a value (so the value is not the script).
const INTERPRETER_VALUE_OPTIONS: &[&str] =
    &["-X", "-W", "-Q", "--require", "-r", "--import", "--loader", "-ExecutionPolicy"];

/// The prefix that "don't ask again" should remember for `argv`.
///
/// * subcommand tools keep the subcommand: `git push origin main` -> `git push`,
///   `npm run test` -> `npm run test`, `docker compose up -d` -> `docker compose up`
/// * interpreters keep the script: `python script.py --x` -> `python script.py`,
///   `python -m pytest -q` -> `python -m pytest`; inline code (`-c`) is kept whole
/// * anything else keeps only the program.
///
/// The program name is normalised (directory and `.exe`/`.cmd`/`.bat` removed,
/// lowercased on Windows).
pub fn approval_prefix(argv: &[String]) -> Vec<String> {
    let Some(first) = argv.first() else { return Vec::new() };
    let mut program = normalize_program(first);
    if cfg!(windows) {
        program = program.to_ascii_lowercase();
    }
    let lookup = program.to_ascii_lowercase();
    let args = &argv[1..];
    let mut out = vec![program];

    if lookup == "git" {
        if let Some((sub, rest)) = git_subcommand(args) {
            out.push(sub.to_string());
            if GIT_GROUPS.contains(&sub) {
                if let Some(next) = rest.iter().find(|a| !a.starts_with('-')) {
                    out.push(next.clone());
                }
            }
        }
        return out;
    }

    if SUBCOMMAND_TOOLS.contains(&lookup.as_str()) {
        let mut positional = args.iter().filter(|a| !a.starts_with('-'));
        if let Some(sub) = positional.next() {
            out.push(sub.clone());
            // `cargo run` arguments are passed to the binary, not a script name.
            let needs_next = (RUNNER_SUBCOMMANDS.contains(&sub.as_str()) && lookup != "cargo")
                || TWO_LEVEL.contains(&lookup.as_str())
                || (matches!(lookup.as_str(), "docker" | "podman") && DOCKER_GROUPS.contains(&sub.as_str()));
            if needs_next {
                if let Some(next) = positional.next() {
                    out.push(next.clone());
                }
            }
        }
        return out;
    }

    if INTERPRETERS.contains(&lookup.as_str()) {
        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            match arg {
                "-m" => {
                    out.push(arg.to_string());
                    if let Some(module) = args.get(i + 1) {
                        out.push(module.clone());
                    }
                    return out;
                }
                "-c" | "-e" | "--eval" | "-Command" | "-command" | "-EncodedCommand" => {
                    // Inline code: remember the exact invocation.
                    out.extend(args[i..].iter().cloned());
                    return out;
                }
                "-File" | "-file" => {
                    if let Some(script) = args.get(i + 1) {
                        out.push(arg.to_string());
                        out.push(script.clone());
                    }
                    return out;
                }
                a if INTERPRETER_VALUE_OPTIONS.contains(&a) => i += 2,
                a if a.starts_with('-') => i += 1,
                _ => {
                    out.push(arg.to_string());
                    return out;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn approval_prefixes() {
        let cases: &[(&[&str], &[&str])] = &[
            (&["git", "push", "origin", "main"], &["git", "push"]),
            (&["git", "-C", "repo", "stash", "pop"], &["git", "stash", "pop"]),
            (&["npm", "run", "test", "--", "--watch"], &["npm", "run", "test"]),
            (&["cargo", "test", "-p", "x"], &["cargo", "test"]),
            (&["cargo", "run", "--release"], &["cargo", "run"]),
            (&["python", "script.py", "--flag"], &["python", "script.py"]),
            (&["python", "-u", "script.py"], &["python", "script.py"]),
            (&["python", "-m", "pytest", "-q"], &["python", "-m", "pytest"]),
            (&["python", "-c", "print(1)"], &["python", "-c", "print(1)"]),
            (&["docker", "compose", "up", "-d"], &["docker", "compose", "up"]),
            (&["gh", "pr", "create", "--fill"], &["gh", "pr", "create"]),
            (&["make", "test"], &["make", "test"]),
            (&["rm", "-rf", "build"], &["rm"]),
            (&["pwsh", "-NoProfile", "-File", "build.ps1"], &["pwsh", "-File", "build.ps1"]),
            (&["ls"], &["ls"]),
        ];
        for (argv, expected) in cases {
            assert_eq!(approval_prefix(&s(argv)), s(expected), "{argv:?}");
        }
        assert!(approval_prefix(&[]).is_empty());
        let normalized = approval_prefix(&s(&["C:\\Tools\\Node.EXE", "app.js"]));
        if cfg!(windows) {
            assert_eq!(normalized, s(&["node", "app.js"]));
        } else {
            assert_eq!(normalized, s(&["Node", "app.js"]));
        }
        assert_eq!(approval_prefix(&s(&["/usr/bin/git", "status"])), s(&["git", "status"]));
    }

    #[test]
    fn parse_rules_with_warnings() {
        let text = r#"
[[rule]]
prefix = ["git", "push"]
decision = "prompt"
justification = "Publishing commits"

[[rule]]
decision = "allow"

[[rule]]
prefix = ["x"]
decision = "forbid"
pattern = "("

[[rule]]
pattern = "^npm (ci|install)$"
decision = "allow"
"#;
        let (rules, warnings) = parse_rules_toml(text, "test.toml");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].prefix, s(&["git", "push"]));
        assert_eq!(rules[0].justification.as_deref(), Some("Publishing commits"));
        assert_eq!(rules[0].describe(), "git push (test.toml)");
        assert_eq!(rules[1].describe(), "* =~ /^npm (ci|install)$/ (test.toml)");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        let (rules, warnings) = parse_rules_toml("[[rule]]\nprefix = 3", "bad.toml");
        assert!(rules.is_empty());
        assert!(warnings[0].starts_with("bad.toml: "), "{warnings:?}");
        let (_, warnings) = parse_rules_toml("[[rule]]\nprefix=[\"a\"]\ndecision=\"maybe\"", "bad.toml");
        assert_eq!(warnings.len(), 1);
    }
}
