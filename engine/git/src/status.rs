//! `git status --porcelain=v2 -z --branch --show-stash` parsing.

use odex_protocol::GitFileStatus;

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct ParsedStatus {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub stash_count: u32,
    pub files: Vec<GitFileStatus>,
}

fn code_of(xy: &str) -> String {
    xy.chars().take(2).map(|c| if c == '.' { ' ' } else { c }).collect()
}

fn entry(xy: &str, path: &str, orig: Option<String>) -> GitFileStatus {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or('.');
    let y = chars.next().unwrap_or('.');
    GitFileStatus {
        path: path.to_string(),
        orig_path: orig,
        code: code_of(xy),
        staged: x != '.',
        unstaged: y != '.',
        untracked: false,
        conflicted: false,
    }
}

pub(crate) fn parse_porcelain_v2(out: &[u8]) -> ParsedStatus {
    let tokens: Vec<String> =
        out.split(|&b| b == 0).filter(|t| !t.is_empty()).map(|t| String::from_utf8_lossy(t).into_owned()).collect();
    let mut st = ParsedStatus::default();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        i += 1;
        if let Some(header) = t.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" if value != "(initial)" => st.head = Some(value.to_string()),
                "branch.head" if value != "(detached)" => st.branch = Some(value.to_string()),
                "branch.upstream" => st.upstream = Some(value.to_string()),
                "branch.ab" => {
                    for part in value.split_whitespace() {
                        if let Some(n) = part.strip_prefix('+') {
                            st.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            st.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                "stash" => st.stash_count = value.trim().parse().unwrap_or(0),
                _ => {}
            }
            continue;
        }
        match t.as_bytes().first() {
            Some(b'1') => {
                // 1 XY sub mH mI mW hH hI path
                let f: Vec<&str> = t.splitn(9, ' ').collect();
                if f.len() == 9 {
                    st.files.push(entry(f[1], f[8], None));
                }
            }
            Some(b'2') => {
                // 2 XY sub mH mI mW hH hI Xscore path, then the original path as its own token
                let f: Vec<&str> = t.splitn(10, ' ').collect();
                let orig = tokens.get(i).cloned();
                i += 1;
                if f.len() == 10 {
                    st.files.push(entry(f[1], f[9], orig));
                }
            }
            Some(b'u') => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let f: Vec<&str> = t.splitn(11, ' ').collect();
                if f.len() == 11 {
                    let mut e = entry(f[1], f[10], None);
                    e.staged = false;
                    e.unstaged = false;
                    e.conflicted = true;
                    st.files.push(e);
                }
            }
            Some(b'?') => {
                if let Some(path) = t.strip_prefix("? ") {
                    st.files.push(GitFileStatus {
                        path: path.to_string(),
                        orig_path: None,
                        code: "??".into(),
                        staged: false,
                        unstaged: false,
                        untracked: true,
                        conflicted: false,
                    });
                }
            }
            _ => {}
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_headers_and_entries() {
        let raw = [
            "# branch.oid 0123456789abcdef0123456789abcdef01234567",
            "# branch.head feature/x",
            "# branch.upstream origin/feature/x",
            "# branch.ab +2 -1",
            "# stash 3",
            "1 .M N... 100644 100644 100644 aaaa bbbb src/main.rs",
            "1 A. N... 000000 100644 100644 0000 cccc new file.txt",
            "2 R. N... 100644 100644 100644 dddd dddd R100 renamed.rs",
            "old name.rs",
            "u UU N... 100644 100644 100644 100644 e1 e2 e3 conflict.txt",
            "? untracked dir/file.txt",
            "! ignored.log",
        ]
        .join("\0");
        let st = parse_porcelain_v2(raw.as_bytes());
        assert_eq!(st.head.as_deref(), Some("0123456789abcdef0123456789abcdef01234567"));
        assert_eq!(st.branch.as_deref(), Some("feature/x"));
        assert_eq!(st.upstream.as_deref(), Some("origin/feature/x"));
        assert_eq!((st.ahead, st.behind, st.stash_count), (2, 1, 3));
        assert_eq!(st.files.len(), 5);
        assert_eq!(st.files[0].code, " M");
        assert!(st.files[0].unstaged && !st.files[0].staged);
        assert_eq!(st.files[1].path, "new file.txt");
        assert!(st.files[1].staged);
        assert_eq!(st.files[2].path, "renamed.rs");
        assert_eq!(st.files[2].orig_path.as_deref(), Some("old name.rs"));
        assert_eq!(st.files[2].code, "R ");
        assert!(st.files[3].conflicted);
        assert_eq!(st.files[3].code, "UU");
        assert!(st.files[4].untracked);
        assert_eq!(st.files[4].path, "untracked dir/file.txt");
    }

    #[test]
    fn initial_and_detached() {
        let st = parse_porcelain_v2(b"# branch.oid (initial)\0# branch.head main\0");
        assert_eq!((st.head, st.branch.as_deref()), (None, Some("main")));
        let st = parse_porcelain_v2(b"# branch.oid abc\0# branch.head (detached)\0");
        assert_eq!((st.head.as_deref(), st.branch), (Some("abc"), None));
    }
}
