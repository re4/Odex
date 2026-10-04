//! Rule evaluation over parsed commands.

use std::fs;
use std::path::PathBuf;

use crate::builtins::{self, BUILTINS};
use crate::heuristics;
use crate::lexer::{parse_commands, Redirect, SimpleCommand};
use crate::normalize::{is_cmdlet_name, normalize_program, powershell_alias, Cmd};
use crate::rules::{parse_rules_toml, rule_files, Rule};
use crate::{Decision, ShellKind};

/// How deep `bash -c "..."`-style nesting is followed.
const MAX_NESTING: u8 = 3;
/// How many wrapper layers (`sudo env nice ...`) are peeled.
const MAX_WRAPPERS: usize = 4;

/// Result of evaluating a command against the policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evaluation {
    /// Most restrictive decision of any matched rule; `None` when no rule
    /// matched (the caller applies its permission-mode defaults).
    pub decision: Option<Decision>,
    /// Every matched rule: (description, decision).
    pub matched: Vec<(String, Decision)>,
    /// The simple commands, or `None` when the command uses constructs that
    /// cannot be analysed statically (subshells, `$(...)`, backticks, ...).
    pub commands: Option<Vec<Vec<String>>>,
    /// Every simple command is a known read-only command, nothing is
    /// redirected into a file, and no rule asked to prompt or forbid.
    pub known_safe: bool,
    /// Something probably modifies files (redirection, rm/mv/cp, Set-Content, installs, ...).
    pub writes_likely: bool,
    /// Something probably uses the network (curl, package installs, git fetch/push, ssh, ...).
    pub network_likely: bool,
    /// Justification of the rule that produced `decision`.
    pub justification: Option<String>,
}

/// A set of approval rules plus (optionally) the built-in dangerous-command rules.
///
/// Within one simple command, any matching `forbid` rule wins; otherwise the
/// most specific rule wins (longer prefix, then a `pattern`), and between
/// equally specific rules the one added later wins (so user files override
/// built-ins and later rule directories override earlier ones). Across the
/// simple commands of a script the most restrictive decision wins.
#[derive(Debug, Clone)]
pub struct Policy {
    rules: Vec<Rule>,
    builtins: bool,
}

impl Default for Policy {
    /// Same as [`Policy::with_defaults`].
    fn default() -> Self {
        Policy::with_defaults()
    }
}

impl Policy {
    /// No rules at all (not even the built-ins).
    pub fn empty() -> Self {
        Policy { rules: Vec::new(), builtins: false }
    }

    /// Only the built-in forbid / prompt rules.
    pub fn with_defaults() -> Self {
        Policy { rules: Vec::new(), builtins: true }
    }

    /// Built-ins plus every `*.toml` file in each directory (files sorted by
    /// name; a path to a single `.toml` file also works). Missing
    /// directories are skipped silently; unreadable or invalid files and
    /// rules produce warnings.
    pub fn load(dirs: &[PathBuf]) -> (Self, Vec<String>) {
        let mut policy = Policy::with_defaults();
        let mut warnings = Vec::new();
        for dir in dirs {
            match rule_files(dir) {
                Ok(files) => {
                    for file in files {
                        match fs::read_to_string(&file) {
                            Ok(text) => {
                                let (rules, file_warnings) = parse_rules_toml(&text, &file.display().to_string());
                                policy.rules.extend(rules);
                                warnings.extend(file_warnings);
                            }
                            Err(e) => warnings.push(format!("{}: {e}", file.display())),
                        }
                    }
                }
                Err(warning) => warnings.push(warning),
            }
        }
        (policy, warnings)
    }

    /// Add a rule; it takes precedence over earlier rules of equal specificity.
    pub fn add_rule(&mut self, rule: Rule) {
        self.rules.push(rule);
    }

    /// User rules in precedence order (built-ins are not listed).
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Evaluate a command line written for `shell`.
    pub fn evaluate(&self, command: &str, shell: ShellKind) -> Evaluation {
        self.evaluate_script(command, shell, 0)
    }

    /// Evaluate an argv. Shell wrappers (`bash -lc "<script>"`,
    /// `powershell -Command ...`, `cmd /c ...`) are unwrapped and their
    /// script is evaluated.
    pub fn evaluate_argv(&self, argv: &[String]) -> Evaluation {
        let command = SimpleCommand { argv: argv.to_vec(), ..Default::default() };
        let mut acc = Acc::new(true);
        self.eval_commands(std::slice::from_ref(&command), None, 0, &mut acc);
        acc.finish()
    }

    fn evaluate_script(&self, command: &str, shell: ShellKind, depth: u8) -> Evaluation {
        let (commands, analyzable) = match parse_commands(command, shell, false) {
            Ok(commands) => (commands, true),
            Err(_) => (parse_commands(command, shell, true).unwrap_or_default(), false),
        };
        let mut acc = Acc::new(analyzable);
        self.eval_commands(&commands, Some(shell), depth, &mut acc);
        if self.builtins {
            for (description, decision, justification) in builtins::raw_checks(command, analyzable) {
                acc.add_match(description.to_string(), decision);
                acc.record(decision, Some(justification.to_string()));
            }
        }
        acc.finish()
    }

    fn eval_commands(&self, commands: &[SimpleCommand], shell: Option<ShellKind>, depth: u8, acc: &mut Acc) {
        for command in commands {
            if command.expression {
                if command.redirects.iter().any(Redirect::writes_file) {
                    acc.writes = true;
                    acc.all_safe = false;
                }
            } else {
                self.eval_simple(command, shell, depth, acc);
            }
            self.eval_commands(&command.nested, shell, depth, acc);
        }
        if self.builtins && builtins::pipeline_executes_download(commands, shell, |c| self.variants(c, shell)) {
            let (description, decision, justification) = builtins::pipe_to_shell_entry();
            acc.add_match(description.to_string(), decision);
            acc.record(decision, Some(justification.to_string()));
        }
    }

    fn eval_simple(&self, command: &SimpleCommand, shell: Option<ShellKind>, depth: u8, acc: &mut Acc) {
        let file_redirect = command.redirects.iter().any(Redirect::writes_file);
        if file_redirect {
            acc.writes = true;
            acc.all_safe = false;
        }
        let Some(cmd) = Cmd::new(&command.argv, &command.redirects, shell) else { return };
        acc.any_command = true;
        let variants = self.variants(&cmd, shell);
        let mut replaced_by_inner = false;
        for (index, variant) in variants.iter().enumerate() {
            if let Some((decision, justification)) = self.decide(variant, acc) {
                acc.record(decision, justification);
            }
            acc.writes |= heuristics::writes_likely(variant);
            acc.network |= heuristics::network_likely(variant);
            if depth >= MAX_NESTING {
                continue;
            }
            if let Some((script, inner_shell)) = nested_shell(variant) {
                let inner = self.evaluate_script(&script, inner_shell, depth + 1);
                if index == 0 {
                    replaced_by_inner = true;
                    match &inner.commands {
                        Some(list) => acc.commands.extend(list.iter().cloned()),
                        None => acc.analyzable = false,
                    }
                    acc.all_safe &= inner.known_safe;
                } else if inner.commands.is_none() {
                    acc.analyzable = false;
                }
                acc.merge(inner);
            }
        }
        if !replaced_by_inner {
            acc.commands.push(command.argv.clone());
            acc.all_safe &= heuristics::is_safe(&variants[0]);
        }
    }

    /// The command itself plus the commands it wraps (`sudo X`, `env A=1 X`, ...).
    fn variants(&self, cmd: &Cmd, shell: Option<ShellKind>) -> Vec<Cmd> {
        let mut out = vec![cmd.clone()];
        while out.len() <= MAX_WRAPPERS {
            match unwrap_wrapper(out.last().expect("non-empty"), shell) {
                Some(inner) => out.push(inner),
                None => break,
            }
        }
        out
    }

    /// Decision for one command variant (see [`Policy`] for precedence).
    fn decide(&self, cmd: &Cmd, acc: &mut Acc) -> Option<(Decision, Option<String>)> {
        // (specificity, order, decision, justification)
        let mut candidates: Vec<(usize, usize, Decision, Option<String>)> = Vec::new();
        if self.builtins {
            for (order, builtin) in BUILTINS.iter().enumerate() {
                if (builtin.check)(cmd) {
                    acc.add_match(format!("{} (builtin)", builtin.description), builtin.decision);
                    candidates.push((
                        builtin.specificity(),
                        order,
                        builtin.decision,
                        Some(builtin.justification.to_string()),
                    ));
                }
            }
        }
        for (index, rule) in self.rules.iter().enumerate() {
            if rule_matches(rule, cmd) {
                acc.add_match(rule.describe(), rule.decision);
                candidates.push((
                    rule.specificity(),
                    BUILTINS.len() + index,
                    rule.decision,
                    rule.justification.clone(),
                ));
            }
        }
        if let Some(forbid) = candidates.iter().find(|c| c.2 == Decision::Forbid) {
            return Some((Decision::Forbid, forbid.3.clone()));
        }
        candidates.into_iter().max_by_key(|c| (c.0, c.1)).map(|c| (c.2, c.3))
    }
}

/// Accumulates results across the simple commands of a script.
struct Acc {
    decision: Option<Decision>,
    justification: Option<String>,
    matched: Vec<(String, Decision)>,
    commands: Vec<Vec<String>>,
    analyzable: bool,
    all_safe: bool,
    any_command: bool,
    writes: bool,
    network: bool,
}

impl Acc {
    fn new(analyzable: bool) -> Self {
        Acc {
            decision: None,
            justification: None,
            matched: Vec::new(),
            commands: Vec::new(),
            analyzable,
            all_safe: true,
            any_command: false,
            writes: false,
            network: false,
        }
    }

    fn record(&mut self, decision: Decision, justification: Option<String>) {
        match self.decision {
            Some(current) if current > decision => {}
            Some(current) if current == decision => {
                if self.justification.is_none() {
                    self.justification = justification;
                }
            }
            _ => {
                self.decision = Some(decision);
                self.justification = justification;
            }
        }
    }

    fn add_match(&mut self, description: String, decision: Decision) {
        if !self.matched.iter().any(|(d, dec)| *d == description && *dec == decision) {
            self.matched.push((description, decision));
        }
    }

    fn merge(&mut self, inner: Evaluation) {
        if let Some(decision) = inner.decision {
            self.record(decision, inner.justification);
        }
        for (description, decision) in inner.matched {
            self.add_match(description, decision);
        }
        self.writes |= inner.writes_likely;
        self.network |= inner.network_likely;
    }

    fn finish(self) -> Evaluation {
        let known_safe = self.analyzable
            && self.any_command
            && self.all_safe
            && !matches!(self.decision, Some(Decision::Prompt | Decision::Forbid));
        Evaluation {
            decision: self.decision,
            matched: self.matched,
            commands: self.analyzable.then_some(self.commands),
            known_safe,
            writes_likely: self.writes,
            network_likely: self.network,
            justification: self.justification,
        }
    }
}

fn rule_matches(rule: &Rule, cmd: &Cmd) -> bool {
    if rule.prefix.is_empty() && rule.pattern.is_none() {
        return false;
    }
    if !prefix_matches(&rule.prefix, cmd) {
        return false;
    }
    match &rule.pattern {
        None => true,
        Some(pattern) => {
            if pattern.is_match(&cmd.argv.join(" ")) {
                return true;
            }
            cmd.raw != cmd.argv[0] && {
                let mut raw = vec![cmd.raw.clone()];
                raw.extend(cmd.args().iter().cloned());
                pattern.is_match(&raw.join(" "))
            }
        }
    }
}

fn prefix_matches(prefix: &[String], cmd: &Cmd) -> bool {
    let Some(program) = prefix.first() else { return true };
    if cmd.argv.len() < prefix.len() {
        return false;
    }
    let wanted = normalize_program(program);
    let case_insensitive = cfg!(windows) || cmd.powershell || is_cmdlet_name(&wanted) || is_cmdlet_name(&cmd.argv[0]);
    let eq = |a: &str, b: &str| if case_insensitive { a.eq_ignore_ascii_case(b) } else { a == b };
    let program_matches = eq(&wanted, &cmd.argv[0])
        || eq(&wanted, &cmd.raw)
        || (cmd.powershell
            && powershell_alias(&wanted.to_ascii_lowercase())
                .is_some_and(|alias| alias.eq_ignore_ascii_case(&cmd.argv[0])));
    if !program_matches {
        return false;
    }
    let params_ci = cmd.params_case_insensitive();
    prefix[1..].iter().zip(&cmd.argv[1..]).all(|(want, got)| {
        if params_ci && want.starts_with('-') {
            want.eq_ignore_ascii_case(got)
        } else {
            want == got
        }
    })
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Skip leading options (and the values of `value_options`), honouring `--`.
fn skip_options<'a>(args: &'a [String], value_options: &[&str]) -> &'a [String] {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            return &args[i + 1..];
        }
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        i += if value_options.contains(&arg) { 2 } else { 1 };
    }
    &args[i.min(args.len())..]
}

/// The command run by a wrapper such as `sudo`, `env`, `nice`, `timeout`, `xargs`.
fn unwrap_wrapper(cmd: &Cmd, shell: Option<ShellKind>) -> Option<Cmd> {
    if cmd.powershell && cmd.prog_lc != cmd.raw_lc {
        // An alias such as `kill` resolved to a cmdlet; not a wrapper.
        return None;
    }
    let args = cmd.args();
    let rest: &[String] = match cmd.raw_lc.as_str() {
        "sudo" | "doas" | "gsudo" => skip_options(
            args,
            &[
                "-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U", "--user", "--group", "--host", "--prompt",
                "--chdir",
            ],
        ),
        "env" => {
            let rest = skip_options(args, &["-u", "-C", "-S", "--unset", "--chdir"]);
            let assignments = rest.iter().take_while(|a| is_assignment(a)).count();
            &rest[assignments..]
        }
        "nice" => skip_options(args, &["-n", "--adjustment"]),
        "time" => skip_options(args, &["-o", "-f", "--output", "--format"]),
        "nohup" | "builtin" | "exec" => skip_options(args, &["-a"]),
        "command" => {
            if args.iter().any(|a| a == "-v" || a == "-V") {
                return None;
            }
            skip_options(args, &[])
        }
        "timeout" => {
            let rest = skip_options(args, &["-s", "-k", "--signal", "--kill-after"]);
            rest.get(1..).unwrap_or(&[])
        }
        "xargs" => skip_options(
            args,
            &["-n", "-I", "-L", "-P", "-d", "-E", "-s", "-a", "--max-args", "--max-procs", "--delimiter", "--arg-file"],
        ),
        "watch" => skip_options(args, &["-n", "--interval"]),
        "call" => args,
        _ => return None,
    };
    Cmd::new(rest, &[], shell)
}

/// `bash -c <script>`, `powershell -Command <script>`, `cmd /c <script>`.
fn nested_shell(cmd: &Cmd) -> Option<(String, ShellKind)> {
    let args = cmd.args();
    match cmd.raw_lc.as_str() {
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "ash" => {
            let pos = args.iter().position(|a| {
                a.len() > 1
                    && a.starts_with('-')
                    && !a.starts_with("--")
                    && a[1..].chars().all(|c| c.is_ascii_alphabetic())
                    && a.contains('c')
            })?;
            if args[..pos].iter().any(|a| !a.starts_with('-')) {
                return None;
            }
            Some((args.get(pos + 1)?.clone(), ShellKind::from_name(&cmd.raw_lc)))
        }
        "powershell" | "pwsh" => {
            let mut i = 0;
            while i < args.len() {
                let lower = args[i].to_ascii_lowercase();
                if lower == "-c" || lower == "/c" || (lower.len() >= 4 && "-command".starts_with(&lower)) {
                    return Some((args[i + 1..].join(" "), ShellKind::PowerShell));
                }
                if lower == "-f"
                    || (lower.len() >= 3 && "-file".starts_with(&lower))
                    || lower.starts_with("-e") && "-encodedcommand".starts_with(&lower)
                {
                    return None;
                }
                if matches!(
                    lower.as_str(),
                    "-executionpolicy"
                        | "-ep"
                        | "-ex"
                        | "-workingdirectory"
                        | "-wd"
                        | "-windowstyle"
                        | "-w"
                        | "-inputformat"
                        | "-outputformat"
                        | "-if"
                        | "-of"
                        | "-configurationname"
                        | "-settingsfile"
                ) {
                    i += 2;
                    continue;
                }
                if lower.starts_with('-') {
                    i += 1;
                    continue;
                }
                // Windows PowerShell treats the first positional as -Command; pwsh as -File.
                return (cmd.raw_lc == "powershell").then(|| (args[i..].join(" "), ShellKind::PowerShell));
            }
            None
        }
        "cmd" => {
            let pos = args.iter().position(|a| a.eq_ignore_ascii_case("/c") || a.eq_ignore_ascii_case("/k"))?;
            Some((args[pos + 1..].join(" "), ShellKind::Cmd))
        }
        _ => None,
    }
}
