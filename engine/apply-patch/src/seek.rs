//! Locating chunk context inside a file, with progressively fuzzier matching.

use std::borrow::Cow;

use crate::error::ClosestMatch;

/// Matching strictness, tried in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fuzz {
    Exact,
    /// Ignore trailing whitespace.
    TrimEnd,
    /// Ignore leading and trailing whitespace.
    Trim,
    /// Trim, normalise Unicode punctuation (smart quotes, dashes, odd spaces)
    /// to ASCII and collapse internal whitespace runs.
    Normalized,
}

pub(crate) const LEVELS: [Fuzz; 4] = [Fuzz::Exact, Fuzz::TrimEnd, Fuzz::Trim, Fuzz::Normalized];

/// Map typographic punctuation to ASCII, drop zero-width characters and
/// collapse whitespace runs to a single space. The result is trimmed.
pub(crate) fn normalize_punctuation(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        let mapped: &str = match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' | '\u{2035}' => "'",
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' | '\u{2036}' | '\u{00AB}' | '\u{00BB}' => {
                "\""
            }
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' | '\u{2212}' | '\u{FE58}'
            | '\u{FE63}' | '\u{FF0D}' => "-",
            '\u{2026}' => "...",
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' => "",
            c if c.is_whitespace() => {
                pending_space = true;
                continue;
            }
            _ => {
                if pending_space && !out.is_empty() {
                    out.push(' ');
                }
                pending_space = false;
                out.push(c);
                continue;
            }
        };
        if !mapped.is_empty() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push_str(mapped);
        }
    }
    out
}

pub(crate) fn key(line: &str, fuzz: Fuzz) -> Cow<'_, str> {
    match fuzz {
        Fuzz::Exact => Cow::Borrowed(line),
        Fuzz::TrimEnd => Cow::Borrowed(line.trim_end()),
        Fuzz::Trim => Cow::Borrowed(line.trim()),
        Fuzz::Normalized => Cow::Owned(normalize_punctuation(line)),
    }
}

fn matches_at(lines: &[String], pattern_keys: &[Cow<'_, str>], pos: usize, fuzz: Fuzz) -> bool {
    pattern_keys.iter().enumerate().all(|(j, pk)| key(&lines[pos + j], fuzz) == *pk)
}

/// Find `pattern` in `lines` at or after `start`.
///
/// Every fuzz level is tried in order; within a level the first position
/// wins. When `eof` is set, a match ending exactly at the end of the file is
/// preferred (at any level) over earlier matches.
pub(crate) fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(if eof { lines.len() } else { start.min(lines.len()) });
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let last_start = lines.len() - pattern.len();
    if start > last_start {
        return None;
    }
    let keys_for = |fuzz: Fuzz| pattern.iter().map(|p| key(p, fuzz)).collect::<Vec<_>>();
    if eof {
        for fuzz in LEVELS {
            if matches_at(lines, &keys_for(fuzz), last_start, fuzz) {
                return Some(last_start);
            }
        }
    }
    for fuzz in LEVELS {
        let keys = keys_for(fuzz);
        if let Some(pos) = (start..=last_start).find(|&pos| matches_at(lines, &keys, pos, fuzz)) {
            return Some(pos);
        }
    }
    None
}

/// Find the line a `@@ header` refers to, at or after `start`.
/// Tries exact equality, trimmed equality, substring, then normalised substring.
pub(crate) fn seek_header(lines: &[String], header: &str, start: usize) -> Option<usize> {
    let wanted = header.trim();
    if wanted.is_empty() {
        return None;
    }
    let range = start.min(lines.len())..lines.len();
    let find = |pred: &dyn Fn(&str) -> bool| range.clone().find(|&i| pred(&lines[i]));
    find(&|l| l == header).or_else(|| find(&|l| l.trim() == wanted)).or_else(|| find(&|l| l.contains(wanted))).or_else(
        || {
            let wanted = normalize_punctuation(wanted);
            find(&|l| normalize_punctuation(l).contains(&wanted))
        },
    )
}

/// The position where the most (non-blank) pattern lines match, compared
/// leniently. Used to point the model at the region it probably meant.
pub(crate) fn closest_match(lines: &[String], pattern: &[String]) -> Option<ClosestMatch> {
    if lines.is_empty() || pattern.is_empty() {
        return None;
    }
    let pattern_keys: Vec<String> = pattern.iter().map(|p| normalize_punctuation(p)).collect();
    let line_keys: Vec<String> = lines.iter().map(|l| normalize_punctuation(l)).collect();
    let mut best: Option<(usize, usize)> = None; // (matched, position)
    for pos in 0..lines.len() {
        let matched = pattern_keys
            .iter()
            .enumerate()
            .filter(|(j, pk)| !pk.is_empty() && line_keys.get(pos + j).is_some_and(|lk| lk == *pk))
            .count();
        if matched > best.map_or(0, |(m, _)| m) {
            best = Some((matched, pos));
        }
    }
    let (matched, pos) = best?;
    let end = (pos + pattern.len()).min(lines.len());
    Some(ClosestMatch { line: pos + 1, matched, snippet: lines[pos..end].to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_match_and_start_offset() {
        let lines = v(&["a", "b", "c", "a", "b"]);
        assert_eq!(seek_sequence(&lines, &v(&["a", "b"]), 0, false), Some(0));
        assert_eq!(seek_sequence(&lines, &v(&["a", "b"]), 1, false), Some(3));
        assert_eq!(seek_sequence(&lines, &v(&["a", "b"]), 4, false), None);
        assert_eq!(seek_sequence(&lines, &v(&["x"]), 0, false), None);
        assert_eq!(seek_sequence(&lines, &v(&["a", "b", "c", "a", "b", "c"]), 0, false), None);
    }

    #[test]
    fn eof_prefers_last_occurrence() {
        let lines = v(&["a", "b", "c", "a", "b"]);
        assert_eq!(seek_sequence(&lines, &v(&["a", "b"]), 0, true), Some(3));
        // Not at the end: falls back to a normal search.
        assert_eq!(seek_sequence(&lines, &v(&["b", "c"]), 0, true), Some(1));
        assert_eq!(seek_sequence(&lines, &[], 0, true), Some(5));
        assert_eq!(seek_sequence(&lines, &[], 2, false), Some(2));
    }

    #[test]
    fn fuzz_levels() {
        let lines = v(&["fn main() {   ", "\tlet x = \u{201C}hi\u{201D};", "    y \u{2014} z\u{00A0}w", "}"]);
        // trailing whitespace
        assert_eq!(seek_sequence(&lines, &v(&["fn main() {"]), 0, false), Some(0));
        // leading + trailing whitespace
        assert_eq!(seek_sequence(&lines, &v(&["y \u{2014} z\u{00A0}w"]), 0, false), Some(2));
        // punctuation normalisation
        assert_eq!(seek_sequence(&lines, &v(&["let x = \"hi\";"]), 0, false), Some(1));
        assert_eq!(seek_sequence(&lines, &v(&["y - z w"]), 0, false), Some(2));
    }

    #[test]
    fn stricter_level_wins_over_earlier_fuzzy_match() {
        let lines = v(&["  foo", "foo"]);
        assert_eq!(seek_sequence(&lines, &v(&["foo"]), 0, false), Some(1));
    }

    #[test]
    fn normalisation() {
        assert_eq!(normalize_punctuation("  \u{2018}a\u{2019}  \u{2013}\tb\u{2026} "), "'a' - b...");
        assert_eq!(normalize_punctuation("x\u{200B}y"), "xy");
        assert_eq!(normalize_punctuation(""), "");
    }

    #[test]
    fn header_seek() {
        let lines = v(&["class A:", "    def greet(self):", "        pass", "def greet():", "    pass"]);
        assert_eq!(seek_header(&lines, "def greet():", 0), Some(3));
        assert_eq!(seek_header(&lines, "def greet(self):", 0), Some(1));
        assert_eq!(seek_header(&lines, "greet", 2), Some(3));
        assert_eq!(seek_header(&lines, "nope", 0), None);
        assert_eq!(seek_header(&lines, "  ", 0), None);
    }

    #[test]
    fn closest_match_reports_best_region() {
        let lines = v(&["one", "two", "three", "four", "five"]);
        let m = closest_match(&lines, &v(&["three", "FOUR", "five"])).unwrap();
        assert_eq!(m.line, 3);
        assert_eq!(m.matched, 2);
        assert_eq!(m.snippet, v(&["three", "four", "five"]));
        assert!(closest_match(&lines, &v(&["zzz"])).is_none());
        // Blank pattern lines do not count as matches.
        assert!(closest_match(&v(&["", "x"]), &v(&["", "y"])).is_none());
    }
}
