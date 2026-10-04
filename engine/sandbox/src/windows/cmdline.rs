//! Program resolution, command-line quoting and environment blocks.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Appends `arg` quoted per the MSVCRT / `CommandLineToArgvW` rules.
pub(crate) fn quote_arg(arg: &str, out: &mut String) {
    let needs_quotes = arg.is_empty() || arg.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '"'));
    if !needs_quotes {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // Escape the backslashes preceding a quote, then the quote itself.
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote must be doubled.
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

/// argv[0] is parsed without backslash escapes: quote it only when needed.
fn quote_program(program: &str, out: &mut String) {
    if program.contains([' ', '\t']) || program.is_empty() {
        out.push('"');
        out.push_str(program);
        out.push('"');
    } else {
        out.push_str(program);
    }
}

fn file_name_lower(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Builds the command line for `program` (resolved path) with `args` (argv[1..]).
///
/// `cmd.exe`: everything after `/c` (or `/k`) is passed verbatim (joined with spaces); when `/s`
/// precedes it, the command is wrapped in one pair of quotes, which cmd strips again.
pub(crate) fn build_command_line(program: &Path, args: &[String]) -> String {
    let mut line = String::new();
    quote_program(&program.to_string_lossy(), &mut line);
    if file_name_lower(program) == "cmd.exe" {
        if let Some(pos) = args.iter().position(|a| a.eq_ignore_ascii_case("/c") || a.eq_ignore_ascii_case("/k")) {
            let strip_quotes = args[..pos].iter().any(|a| a.eq_ignore_ascii_case("/s"));
            for a in &args[..=pos] {
                line.push(' ');
                quote_arg(a, &mut line);
            }
            let rest = args[pos + 1..].join(" ");
            if !rest.is_empty() {
                line.push(' ');
                if strip_quotes {
                    line.push('"');
                    line.push_str(&rest);
                    line.push('"');
                } else {
                    line.push_str(&rest);
                }
            }
            return line;
        }
    }
    for a in args {
        line.push(' ');
        quote_arg(a, &mut line);
    }
    line
}

/// Batch files must be run through `cmd.exe /d /s /c "<script> args"`.
pub(crate) fn is_batch(program: &Path) -> bool {
    let name = file_name_lower(program);
    name.ends_with(".bat") || name.ends_with(".cmd")
}

/// Command line that runs a batch file through `cmd`.
pub(crate) fn batch_command_line(cmd_exe: &Path, script: &Path, args: &[String]) -> String {
    let mut inner = String::new();
    quote_arg(&script.to_string_lossy(), &mut inner);
    for a in args {
        inner.push(' ');
        quote_arg(a, &mut inner);
    }
    let mut line = String::new();
    quote_program(&cmd_exe.to_string_lossy(), &mut line);
    line.push_str(" /d /s /c \"");
    line.push_str(&inner);
    line.push('"');
    line
}

/// Finds `program` like a shell would, using the child's PATH / PATHEXT. Names containing a path
/// separator are resolved relative to `cwd`; bare names are searched on PATH only (the working
/// directory is deliberately not searched).
pub(crate) fn resolve_program(
    program: &str,
    cwd: &Path,
    path_var: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    let exts: Vec<String> = pathext
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .map(|e| e.trim().to_string())
        .filter(|e| e.starts_with('.') && e.len() > 1)
        .collect();
    let try_base = |base: PathBuf| -> Option<PathBuf> {
        if base.extension().is_some() && base.is_file() {
            return Some(base);
        }
        for ext in &exts {
            let mut s: OsString = base.clone().into_os_string();
            s.push(ext);
            let candidate = PathBuf::from(s);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    };
    let p = Path::new(program);
    if p.is_absolute() || program.contains(['\\', '/']) {
        return try_base(cwd.join(p));
    }
    let path_var = path_var?;
    std::env::split_paths(path_var).filter(|d| !d.as_os_str().is_empty()).find_map(|dir| try_base(dir.join(p)))
}

/// Case-insensitive environment map preserving the original key spelling.
#[derive(Default)]
pub(crate) struct EnvMap {
    vars: BTreeMap<String, (OsString, OsString)>,
}

impl EnvMap {
    pub(crate) fn from_current() -> Self {
        let mut map = EnvMap::default();
        for (k, v) in std::env::vars_os() {
            map.set(k, v);
        }
        map
    }

    pub(crate) fn set(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        let key = key.into();
        let norm = key.to_string_lossy().to_uppercase();
        self.vars.insert(norm, (key, value.into()));
    }

    pub(crate) fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.get(&key.to_uppercase()).map(|(_, v)| v.as_os_str())
    }

    /// `KEY=VALUE\0...\0\0` in UTF-16, sorted case-insensitively.
    pub(crate) fn to_block(&self) -> Vec<u16> {
        let mut block = Vec::new();
        for (key, value) in self.vars.values() {
            if key.is_empty() {
                continue;
            }
            block.extend(key.encode_wide());
            block.push('=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(a: &str) -> String {
        let mut s = String::new();
        quote_arg(a, &mut s);
        s
    }

    #[test]
    fn msvcrt_quoting() {
        assert_eq!(q("simple"), "simple");
        assert_eq!(q(""), "\"\"");
        assert_eq!(q("a b"), "\"a b\"");
        assert_eq!(q("a\"b"), "\"a\\\"b\"");
        assert_eq!(q("C:\\path\\"), "C:\\path\\");
        assert_eq!(q("C:\\my path\\"), "\"C:\\my path\\\\\"");
        assert_eq!(q("a\\\\\"b"), "\"a\\\\\\\\\\\"b\"");
        assert_eq!(q("x\\y z"), "\"x\\y z\"");
    }

    #[test]
    fn cmd_command_is_verbatim() {
        let line = build_command_line(
            Path::new("C:\\Windows\\System32\\cmd.exe"),
            &["/d".into(), "/s".into(), "/c".into(), "echo \"a b\" & dir".into()],
        );
        assert_eq!(line, "C:\\Windows\\System32\\cmd.exe /d /s /c \"echo \"a b\" & dir\"");
        let line = build_command_line(Path::new("C:\\x\\cmd.exe"), &["/c".into(), "echo".into(), "hi".into()]);
        assert_eq!(line, "C:\\x\\cmd.exe /c echo hi");
    }

    #[test]
    fn generic_command_line() {
        let line = build_command_line(Path::new("C:\\Program Files\\x.exe"), &["a b".into(), "c".into()]);
        assert_eq!(line, "\"C:\\Program Files\\x.exe\" \"a b\" c");
    }

    #[test]
    fn batch_line() {
        let line = batch_command_line(Path::new("C:\\W\\cmd.exe"), Path::new("C:\\a b\\x.cmd"), &["1".into()]);
        assert_eq!(line, "C:\\W\\cmd.exe /d /s /c \"\"C:\\a b\\x.cmd\" 1\"");
    }

    #[test]
    fn resolves_cmd_on_path() {
        let path = std::env::var_os("PATH");
        let found = resolve_program("cmd", Path::new("."), path.as_deref(), None).expect("cmd.exe on PATH");
        assert!(found.to_string_lossy().to_ascii_lowercase().ends_with("cmd.exe"));
        assert!(resolve_program("definitely-not-a-program-xyz", Path::new("."), path.as_deref(), None).is_none());
    }

    #[test]
    fn env_map_case_insensitive() {
        let mut env = EnvMap::default();
        env.set("Path", "a");
        env.set("PATH", "b");
        assert_eq!(env.get("path"), Some(OsStr::new("b")));
        let block = env.to_block();
        assert_eq!(String::from_utf16_lossy(&block), "PATH=b\0\0");
    }
}
