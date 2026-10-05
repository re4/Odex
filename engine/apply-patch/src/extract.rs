//! Detect `apply_patch` invocations that the model sent through a shell
//! instead of the dedicated tool, and pull the patch text out of them.

use std::path::Path;

/// A patch recovered from a shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPatch {
    /// The raw patch text (not yet parsed).
    pub patch: String,
    /// Directory from a leading `cd <dir> &&` / `Set-Location <dir>;`, if any.
    /// Relative to the command's working directory unless absolute.
    pub workdir: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// POSIX shells: backslash escapes, `'...'` literal strings.
    Posix,
    /// PowerShell: backtick escapes, `''` inside single quotes.
    PowerShell,
}

const APPLY_PATCH_NAMES: [&str; 2] = ["apply_patch", "applypatch"];

/// Detect an `apply_patch` invocation inside a shell script / command line.
///
/// Recognised forms (optionally preceded by `cd <dir> &&`, `cd <dir>;` or
/// `Set-Location <dir>;`, and optionally wrapped in `bash -lc "..."`,
/// `sh -c '...'` or `powershell -Command "..."`):
///
/// * `apply_patch "*** Begin Patch ..."` (single or double quoted argument)
/// * `apply_patch <<'EOF'` / `<<EOF` / `<<"EOF"` / `<<-EOF` heredocs
/// * PowerShell here-strings: `apply_patch @'` ... `'@` (also `@"` ... `"@`)
/// * piped forms: `@' ... '@ | apply_patch` and `cat <<'EOF' | apply_patch`
///
/// Returns `None` when the command does not invoke `apply_patch`.
pub fn extract_patch_from_command(argv_or_script: &str) -> Option<ExtractedPatch> {
    let script = argv_or_script.strip_prefix('\u{feff}').unwrap_or(argv_or_script).replace("\r\n", "\n");
    extract_script(&script, Dialect::Posix, 0)
}

/// Like [`extract_patch_from_command`] but for an already-split argv, e.g.
/// `["apply_patch", "<patch>"]`, `["bash", "-lc", "<script>"]` or
/// `["powershell", "-Command", "<script>"]`.
pub fn extract_patch_from_argv(argv: &[String]) -> Option<ExtractedPatch> {
    let program = program_name(argv.first()?);
    if APPLY_PATCH_NAMES.contains(&program.as_str()) {
        return match argv {
            [_, patch] => Some(ExtractedPatch { patch: patch.replace("\r\n", "\n"), workdir: None }),
            _ => None,
        };
    }
    match program.as_str() {
        "bash" | "sh" | "zsh" | "dash" | "ksh" => {
            let mut i = 1;
            while i < argv.len() {
                let arg = &argv[i];
                if is_posix_command_flag(arg) {
                    let script = argv.get(i + 1)?.replace("\r\n", "\n");
                    return extract_script(&script, Dialect::Posix, 1);
                }
                if !arg.starts_with('-') {
                    return None;
                }
                i += 1;
            }
            None
        }
        "powershell" | "pwsh" => {
            let mut i = 1;
            while i < argv.len() {
                let lower = argv[i].to_ascii_lowercase();
                if is_powershell_command_flag(&lower) {
                    let script = argv[i + 1..].join(" ").replace("\r\n", "\n");
                    return extract_script(&script, Dialect::PowerShell, 1);
                }
                if powershell_flag_takes_value(&lower) {
                    i += 1;
                } else if !lower.starts_with('-') {
                    return None;
                }
                i += 1;
            }
            None
        }
        "cmd" => {
            let pos = argv.iter().position(|a| a.eq_ignore_ascii_case("/c") || a.eq_ignore_ascii_case("/k"))?;
            let script = argv[pos + 1..].join(" ").replace("\r\n", "\n");
            extract_script(&script, Dialect::Posix, 1)
        }
        _ => None,
    }
}

fn program_name(program: &str) -> String {
    let base = Path::new(program).file_name().and_then(|s| s.to_str()).unwrap_or(program);
    let base = base.rsplit(['/', '\\']).next().unwrap_or(base);
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".exe").map(str::to_string).unwrap_or(lower)
}

fn is_posix_command_flag(arg: &str) -> bool {
    arg.len() > 1
        && arg.starts_with('-')
        && !arg.starts_with("--")
        && arg[1..].chars().all(|c| c.is_ascii_alphabetic())
        && arg.contains('c')
}

fn is_powershell_command_flag(lower: &str) -> bool {
    matches!(lower, "-command" | "-c" | "/c" | "-com" | "-comm" | "-comma" | "-comman" | "/command")
}

fn powershell_flag_takes_value(lower: &str) -> bool {
    matches!(
        lower,
        "-executionpolicy"
            | "-ep"
            | "-ex"
            | "-workingdirectory"
            | "-wd"
            | "-windowstyle"
            | "-inputformat"
            | "-outputformat"
    )
}

fn extract_script(script: &str, dialect: Dialect, depth: u8) -> Option<ExtractedPatch> {
    if depth > 4 {
        return None;
    }
    let script = script.trim();
    if let Some((inner, inner_dialect)) = unwrap_shell_invocation(script) {
        return extract_script(&inner, inner_dialect, depth + 1);
    }

    let (workdir, rest) = split_leading_cds(script, dialect);
    let rest = rest.trim_start();

    if let Some(patch) = piped_into_apply_patch(rest) {
        return Some(ExtractedPatch { patch, workdir });
    }

    let after = strip_apply_patch_token(rest)?;
    let patch = parse_patch_argument(after, dialect)?;
    Some(ExtractedPatch { patch, workdir })
}

/// `bash -lc "<script>"`, `sh -c '<script>'`, `powershell -NoProfile -Command "<script>"`.
fn unwrap_shell_invocation(script: &str) -> Option<(String, Dialect)> {
    let (program, mut rest) = parse_word(script, Dialect::Posix)?;
    match program_name(&program).as_str() {
        "bash" | "sh" | "zsh" | "dash" | "ksh" => loop {
            let (word, after) = parse_word(rest, Dialect::Posix)?;
            if is_posix_command_flag(&word) {
                let (inner, _) = parse_word(after, Dialect::Posix)?;
                return Some((inner, Dialect::Posix));
            }
            if !word.starts_with('-') {
                return None;
            }
            rest = after;
        },
        "powershell" | "pwsh" => loop {
            let (word, after) = parse_word(rest, Dialect::Posix)?;
            let lower = word.to_ascii_lowercase();
            if is_powershell_command_flag(&lower) {
                let after = after.trim_start();
                // A single quoted argument holds the whole script; otherwise
                // everything after -Command is the script.
                if after.starts_with('"') || after.starts_with('\'') {
                    if let Some((inner, tail)) = parse_word(after, Dialect::Posix) {
                        if tail.trim().is_empty() {
                            return Some((inner, Dialect::PowerShell));
                        }
                    }
                }
                return Some((after.to_string(), Dialect::PowerShell));
            }
            if powershell_flag_takes_value(&lower) {
                let (_, after_value) = parse_word(after, Dialect::Posix)?;
                rest = after_value;
            } else if lower.starts_with('-') {
                rest = after;
            } else {
                return None;
            }
        },
        _ => None,
    }
}

/// Peel any number of leading `cd <dir> &&` / `Set-Location <dir>;` prefixes.
fn split_leading_cds(script: &str, dialect: Dialect) -> (Option<String>, &str) {
    let mut workdir: Option<String> = None;
    let mut rest = script;
    while let Some((dir, after)) = split_leading_cd(rest, dialect) {
        workdir = Some(match workdir {
            Some(prev) if !is_absolute_like(&dir) => format!("{}/{}", prev.trim_end_matches(['/', '\\']), dir),
            _ => dir,
        });
        rest = after;
    }
    (workdir, rest)
}

fn is_absolute_like(dir: &str) -> bool {
    let b = dir.as_bytes();
    dir.starts_with('/')
        || dir.starts_with('\\')
        || dir.starts_with('~')
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

fn split_leading_cd(script: &str, dialect: Dialect) -> Option<(String, &str)> {
    let (word, mut rest) = parse_word(script, dialect)?;
    let lower = word.to_ascii_lowercase();
    if !matches!(lower.as_str(), "cd" | "chdir" | "pushd" | "set-location" | "sl" | "push-location") {
        return None;
    }
    let (mut dir, mut after) = parse_word(rest, dialect)?;
    if dir.eq_ignore_ascii_case("-path") || dir.eq_ignore_ascii_case("-literalpath") {
        rest = after;
        (dir, after) = parse_word(rest, dialect)?;
    }
    if dir.eq_ignore_ascii_case("/d") {
        // cmd's `cd /d C:\dir`
        rest = after;
        (dir, after) = parse_word(rest, dialect)?;
    }
    let after = after.trim_start_matches([' ', '\t']);
    let after = after
        .strip_prefix("&&")
        .or_else(|| after.strip_prefix(';'))
        .or_else(|| after.strip_prefix('\n'))
        .or_else(|| after.strip_prefix('&'))?;
    Some((dir, after))
}

/// Text following the `apply_patch` program token, if the script starts with it.
fn strip_apply_patch_token(script: &str) -> Option<&str> {
    for name in APPLY_PATCH_NAMES {
        let candidates = [name.to_string(), format!("./{name}"), format!(".\\{name}"), format!("{name}.exe")];
        for candidate in &candidates {
            if let Some(rest) = script.strip_prefix(candidate.as_str()) {
                match rest.chars().next() {
                    None => return Some(rest),
                    Some(c) if c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '@' => {
                        return Some(rest);
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

fn parse_patch_argument(after: &str, dialect: Dialect) -> Option<String> {
    let mut arg = after.trim_start_matches([' ', '\t']);
    if let Some(rest) = arg.strip_prefix("--") {
        if rest.starts_with([' ', '\t']) {
            arg = rest.trim_start_matches([' ', '\t']);
        }
    }
    if arg.starts_with("<<") && !arg.starts_with("<<<") {
        return parse_heredoc(arg);
    }
    if let Some(body) = arg.strip_prefix("<<<") {
        let (word, _) = parse_word(body.trim_start(), dialect)?;
        return Some(word);
    }
    if arg.starts_with("@'") || arg.starts_with("@\"") {
        return parse_here_string(arg).map(|(body, _)| body);
    }
    if let Some(body) = arg.strip_prefix("$'") {
        return parse_ansi_c_quoted(body).map(|(s, _)| s);
    }
    if arg.starts_with('\'') || arg.starts_with('"') {
        let dialect = if dialect == Dialect::Posix && looks_like_powershell_double_quoted(arg) {
            Dialect::PowerShell
        } else {
            dialect
        };
        return parse_word(arg, dialect).map(|(word, _)| word);
    }
    None
}

/// Backtick escapes (`` `" ``) only make sense in PowerShell.
fn looks_like_powershell_double_quoted(arg: &str) -> bool {
    arg.starts_with('"') && arg.contains("`\"")
}

/// `@' ... '@ | apply_patch` or `cat <<'EOF' | apply_patch`.
fn piped_into_apply_patch(script: &str) -> Option<String> {
    if script.starts_with("@'") || script.starts_with("@\"") {
        let (body, tail) = parse_here_string(script)?;
        let tail = tail.trim_start().strip_prefix('|')?.trim_start();
        strip_apply_patch_token(tail)?;
        return Some(body);
    }
    let first_line = script.lines().next()?;
    let (word, rest) = parse_word(first_line, Dialect::Posix)?;
    if word == "cat" {
        let rest = rest.trim_start();
        if rest.starts_with("<<") {
            let pipe = first_line.find('|')?;
            strip_apply_patch_token(first_line[pipe + 1..].trim_start())?;
            let offset = first_line.len() - rest.len();
            return parse_heredoc(&script[offset..]);
        }
    }
    None
}

/// Parse `<<[-]['"]DELIM['"] ... DELIM` starting at `<<`.
fn parse_heredoc(text: &str) -> Option<String> {
    let mut rest = text.strip_prefix("<<")?;
    let strip_tabs = rest.starts_with('-');
    if strip_tabs {
        rest = &rest[1..];
    }
    rest = rest.trim_start_matches([' ', '\t']);
    let (delimiter, _) = parse_word(rest, Dialect::Posix)?;
    if delimiter.is_empty() {
        return None;
    }
    let newline = rest.find('\n')?;
    let body = &rest[newline + 1..];
    let mut out: Vec<&str> = Vec::new();
    for line in body.split('\n') {
        let candidate = if strip_tabs { line.trim_start_matches('\t') } else { line };
        if candidate.trim_end() == delimiter {
            return Some(out.join("\n"));
        }
        out.push(candidate);
    }
    // Missing terminator: be lenient and take everything.
    Some(out.join("\n").trim_end().to_string())
}

/// Parse a PowerShell here-string starting at `@'` / `@"`. Returns the body
/// and the text following the terminator.
fn parse_here_string(text: &str) -> Option<(String, &str)> {
    let quote = text.chars().nth(1)?;
    let rest = &text[2..];
    let newline = rest.find('\n')?;
    if !rest[..newline].trim().is_empty() {
        return None;
    }
    let terminator = format!("{quote}@");
    let body_start = newline + 1;
    let mut offset = body_start;
    let mut out: Vec<&str> = Vec::new();
    for line in rest[body_start..].split('\n') {
        if line.trim_start().starts_with(&terminator) {
            let term_pos = offset + (line.len() - line.trim_start().len()) + terminator.len();
            return Some((out.join("\n"), &rest[term_pos..]));
        }
        out.push(line);
        offset += line.len() + 1;
    }
    Some((out.join("\n").trim_end().to_string(), ""))
}

/// Bash `$'...'` strings.
fn parse_ansi_c_quoted(body: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\'' => return Some((out, &body[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

fn is_word_terminator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')')
}

/// Parse one shell word (concatenation of quoted / unquoted segments) after
/// skipping leading spaces and tabs. Returns `None` for an unterminated quote
/// or when no word is present.
fn parse_word(text: &str, dialect: Dialect) -> Option<(String, &str)> {
    let text = text.trim_start_matches([' ', '\t']);
    let mut out = String::new();
    let mut any = false;
    let mut chars = text.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        if is_word_terminator(c) {
            return if any { Some((out, &text[i..])) } else { None };
        }
        any = true;
        chars.next();
        match c {
            '\'' => loop {
                let (_, c) = chars.next()?;
                if c == '\'' {
                    // `''` inside single quotes: PowerShell's escaped quote (and
                    // a no-op concatenation in POSIX shells, which nobody writes
                    // on purpose).
                    if chars.peek().map(|&(_, n)| n) == Some('\'') {
                        chars.next();
                        out.push('\'');
                        continue;
                    }
                    break;
                }
                out.push(c);
            },
            '"' => loop {
                let (_, c) = chars.next()?;
                match (c, dialect) {
                    ('"', Dialect::PowerShell) if chars.peek().map(|&(_, n)| n) == Some('"') => {
                        chars.next();
                        out.push('"');
                    }
                    ('"', _) => break,
                    ('\\', Dialect::Posix) => {
                        let (_, next) = chars.next()?;
                        match next {
                            '$' | '`' | '"' | '\\' => out.push(next),
                            '\n' => {}
                            other => {
                                out.push('\\');
                                out.push(other);
                            }
                        }
                    }
                    ('`', Dialect::PowerShell) => {
                        let (_, next) = chars.next()?;
                        out.push(powershell_escape(next));
                    }
                    (other, _) => out.push(other),
                }
            },
            '\\' if dialect == Dialect::Posix => match chars.peek().map(|&(_, n)| n) {
                // Only treat the backslash as an escape before shell-special
                // characters so unquoted Windows paths (`C:\dir`) survive.
                Some(next) if is_posix_escapable(next) => {
                    chars.next();
                    if next != '\n' {
                        out.push(next);
                    }
                }
                _ => out.push('\\'),
            },
            '`' if dialect == Dialect::PowerShell => {
                if let Some((_, next)) = chars.next() {
                    out.push(powershell_escape(next));
                }
            }
            other => out.push(other),
        }
    }
    if any {
        Some((out, ""))
    } else {
        None
    }
}

fn is_posix_escapable(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '\\' | '\''
                | '"'
                | '$'
                | '`'
                | ';'
                | '&'
                | '|'
                | '<'
                | '>'
                | '('
                | ')'
                | '*'
                | '?'
                | '['
                | ']'
                | '#'
                | '~'
                | '!'
                | '{'
                | '}'
        )
}

fn powershell_escape(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const PATCH: &str = "*** Begin Patch\n*** Add File: hello.txt\n+it's \"quoted\" $HOME `tick`\n*** End Patch";

    fn ex(patch: &str, workdir: Option<&str>) -> Option<ExtractedPatch> {
        Some(ExtractedPatch { patch: patch.to_string(), workdir: workdir.map(str::to_string) })
    }

    #[test]
    fn quoted_heredoc() {
        let script = format!("apply_patch <<'EOF'\n{PATCH}\nEOF\n");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, None));
    }

    #[test]
    fn heredoc_variants() {
        for opener in ["<<EOF", "<< EOF", "<<\"EOF\"", "<<'PATCH'", "<<-EOF"] {
            let delim = if opener.contains("PATCH") { "PATCH" } else { "EOF" };
            let script = format!("apply_patch {opener}\n{PATCH}\n{delim}");
            assert_eq!(extract_patch_from_command(&script), ex(PATCH, None), "{opener}");
        }
        // <<- strips leading tabs from body and terminator.
        let script = "apply_patch <<-EOF\n\t*** Begin Patch\n\t*** Delete File: a\n\t*** End Patch\n\tEOF";
        assert_eq!(extract_patch_from_command(script), ex("*** Begin Patch\n*** Delete File: a\n*** End Patch", None));
        // Missing terminator is tolerated.
        let script = format!("apply_patch <<'EOF'\n{PATCH}\n");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, None));
        // Trailing commands after the terminator are ignored.
        let script = format!("apply_patch <<'EOF'\n{PATCH}\nEOF\necho done");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, None));
    }

    #[test]
    fn quoted_argument() {
        let script = "apply_patch '*** Begin Patch\n*** Delete File: a\n*** End Patch'";
        assert_eq!(extract_patch_from_command(script), ex("*** Begin Patch\n*** Delete File: a\n*** End Patch", None));
        let script = "apply_patch \"*** Begin Patch\n*** Add File: a\n+say \\\"hi\\\" \\$x\n*** End Patch\"";
        assert_eq!(
            extract_patch_from_command(script),
            ex("*** Begin Patch\n*** Add File: a\n+say \"hi\" $x\n*** End Patch", None)
        );
        // bash '\'' idiom
        let script = "apply_patch '*** Begin Patch\n*** Add File: a\n+it'\\''s\n*** End Patch'";
        assert_eq!(
            extract_patch_from_command(script),
            ex("*** Begin Patch\n*** Add File: a\n+it's\n*** End Patch", None)
        );
        // PowerShell '' escape
        let script = "apply_patch '*** Begin Patch\n*** Add File: a\n+it''s\n*** End Patch'";
        assert_eq!(
            extract_patch_from_command(script),
            ex("*** Begin Patch\n*** Add File: a\n+it's\n*** End Patch", None)
        );
    }

    #[test]
    fn bash_lc_wrapper() {
        let script =
            "bash -lc \"apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: a\n+echo \\$HOME\n*** End Patch\nEOF\n\"";
        assert_eq!(
            extract_patch_from_command(script),
            ex("*** Begin Patch\n*** Add File: a\n+echo $HOME\n*** End Patch", None)
        );
        let script = format!(
            "/bin/sh -c 'cd sub/dir && apply_patch <<\"EOF\"\n{}\nEOF'",
            "*** Begin Patch\n*** Delete File: x\n*** End Patch"
        );
        assert_eq!(
            extract_patch_from_command(&script),
            ex("*** Begin Patch\n*** Delete File: x\n*** End Patch", Some("sub/dir"))
        );
    }

    #[test]
    fn powershell_here_strings() {
        let script = format!("apply_patch @'\n{PATCH}\n'@");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, None));
        let script = format!("apply_patch @\"\n{PATCH}\n\"@");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, None));
        let script = format!("@'\n{PATCH}\n'@ | apply_patch");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, None));
        let script = format!("Set-Location 'C:\\work\\repo'; apply_patch @'\n{PATCH}\n'@");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, Some("C:\\work\\repo")));
        let script = format!(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -Command \"apply_patch @'\n{}\n'@\"",
            "*** Begin Patch\n*** Delete File: x\n*** End Patch"
        );
        assert_eq!(extract_patch_from_command(&script), ex("*** Begin Patch\n*** Delete File: x\n*** End Patch", None));
    }

    #[test]
    fn cd_prefixes() {
        let script = format!("cd my-dir && apply_patch <<EOF\n{PATCH}\nEOF");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, Some("my-dir")));
        let script = format!("cd \"dir with space\"; apply_patch <<EOF\n{PATCH}\nEOF");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, Some("dir with space")));
        let script = format!("cd a && cd b && apply_patch <<EOF\n{PATCH}\nEOF");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, Some("a/b")));
        let script = format!("cd a && cd /abs && apply_patch <<EOF\n{PATCH}\nEOF");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, Some("/abs")));
        let script = format!("cd /d C:\\x && apply_patch <<EOF\n{PATCH}\nEOF");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, Some("C:\\x")));
    }

    #[test]
    fn cat_pipe_and_crlf() {
        let script = format!("cat <<'EOF' | apply_patch\n{PATCH}\nEOF");
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, None));
        let script = format!("apply_patch <<'EOF'\r\n{}\r\nEOF\r\n", PATCH.replace('\n', "\r\n"));
        assert_eq!(extract_patch_from_command(&script), ex(PATCH, None));
    }

    #[test]
    fn non_apply_patch_commands() {
        for script in [
            "echo hi",
            "git apply foo.patch",
            "apply_patches <<EOF\nx\nEOF",
            "bash -lc 'ls -la'",
            "cd x && ls",
            "apply_patch",
            "",
        ] {
            assert_eq!(extract_patch_from_command(script), None, "{script}");
        }
    }

    #[test]
    fn argv_forms() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(extract_patch_from_argv(&argv(&["apply_patch", PATCH])), ex(PATCH, None));
        assert_eq!(extract_patch_from_argv(&argv(&["C:\\bin\\applypatch.exe", PATCH])), ex(PATCH, None));
        assert_eq!(extract_patch_from_argv(&argv(&["apply_patch"])), None);
        let script = format!("apply_patch <<'EOF'\n{PATCH}\nEOF");
        assert_eq!(extract_patch_from_argv(&argv(&["bash", "-lc", &script])), ex(PATCH, None));
        assert_eq!(extract_patch_from_argv(&argv(&["/usr/bin/zsh", "-l", "-c", &script])), ex(PATCH, None));
        let ps = format!("apply_patch @'\n{PATCH}\n'@");
        assert_eq!(extract_patch_from_argv(&argv(&["pwsh", "-NoProfile", "-Command", &ps])), ex(PATCH, None));
        assert_eq!(extract_patch_from_argv(&argv(&["bash", "-lc", "ls"])), None);
        assert_eq!(extract_patch_from_argv(&argv(&["git", "status"])), None);
        assert_eq!(extract_patch_from_argv(&argv(&["cmd", "/c", &script])), ex(PATCH, None));
    }

    #[test]
    fn word_parsing() {
        assert_eq!(parse_word("  foo bar", Dialect::Posix), Some(("foo".into(), " bar")));
        assert_eq!(parse_word("'a b'c\"d\"&&x", Dialect::Posix), Some(("a bcd".into(), "&&x")));
        assert_eq!(parse_word("\"a`\"b\"", Dialect::PowerShell), Some(("a\"b".into(), "")));
        assert_eq!(parse_word("\"a\"\"b\"", Dialect::PowerShell), Some(("a\"b".into(), "")));
        assert_eq!(parse_word("'unterminated", Dialect::Posix), None);
        assert_eq!(parse_word("   ", Dialect::Posix), None);
        assert_eq!(parse_ansi_c_quoted("a\\nb' rest"), Some(("a\nb".into(), " rest")));
    }
}
