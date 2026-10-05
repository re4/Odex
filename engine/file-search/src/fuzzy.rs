//! Fuzzy subsequence scoring over relative paths.
//!
//! The scorer finds the best alignment of the query characters as a subsequence of the path with
//! a small dynamic program (Smith-Waterman style, in the spirit of fzf's v2 algorithm):
//!
//! * every matched character earns [`SCORE_MATCH`] plus a positional bonus: path-segment starts
//!   (start of string or after `/`), word starts after `_ - . space`, camelCase humps and
//!   letter→digit transitions;
//! * the first query character's bonus is multiplied, so queries prefer to start on a boundary;
//! * runs of consecutive matches keep the bonus of the run's first character (at least
//!   [`BONUS_CONSECUTIVE`]);
//! * gaps cost [`GAP_START`] plus [`GAP_EXTEND`] per extra skipped character, capped at
//!   [`GAP_MAX`] so long paths are not punished without bound;
//! * characters matched inside the basename earn [`BONUS_BASENAME`].
//!
//! Matching is case-insensitive unless the query contains an uppercase character (smart case).
//! Whitespace separates independent terms which must all match; their scores are summed.

const SCORE_MATCH: i32 = 16;
const GAP_START: i32 = -3;
const GAP_EXTEND: i32 = -1;
const GAP_MAX: i32 = -12;
const BONUS_SEGMENT: i32 = 10;
const BONUS_DELIMITER: i32 = 8;
const BONUS_CAMEL: i32 = 7;
const BONUS_CONSECUTIVE: i32 = 5;
const FIRST_CHAR_MULTIPLIER: i32 = 2;
const BONUS_BASENAME: i32 = 6;
/// Distance at which a gap reaches [`GAP_MAX`].
const GAP_CAP_DISTANCE: usize = 10;
const NEG: i32 = i32::MIN / 4;

/// Score `path` against `query`. Returns the score and the char indices (into `path`) of the
/// matched characters, or `None` when the query does not match.
pub fn fuzzy_match(query: &str, path: &str) -> Option<(u32, Vec<u32>)> {
    let query = Query::parse(query)?;
    let mut scratch = Scratch::default();
    let mut indices = Vec::new();
    let score = query.score(path, &mut scratch, Some(&mut indices))?;
    Some((score, indices))
}

/// Fold a character for case-insensitive comparison, keeping a 1:1 char mapping.
pub(crate) fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Folded copy of `s` with exactly one char per input char.
pub(crate) fn fold_str(s: &str) -> String {
    s.chars().map(fold).collect()
}

#[derive(Debug, Clone)]
pub(crate) struct Query {
    /// Terms, already folded unless `case_sensitive`.
    terms: Vec<Vec<char>>,
    /// Folded terms, used for the cheap prefilter.
    folded_terms: Vec<Vec<char>>,
    case_sensitive: bool,
}

impl Query {
    /// Parse a user query. Returns `None` for an empty (whitespace-only) query.
    pub(crate) fn parse(raw: &str) -> Option<Query> {
        let case_sensitive = raw.chars().any(char::is_uppercase);
        let mut terms = Vec::new();
        let mut folded_terms = Vec::new();
        for term in raw.split_whitespace() {
            let chars: Vec<char> = term.chars().map(|c| if c == '\\' { '/' } else { c }).collect();
            folded_terms.push(chars.iter().copied().map(fold).collect());
            terms.push(if case_sensitive { chars } else { chars.into_iter().map(fold).collect() });
        }
        if terms.is_empty() {
            None
        } else {
            Some(Query { terms, folded_terms, case_sensitive })
        }
    }

    /// Cheap necessary condition: every term is a (case-insensitive) subsequence of `folded_path`.
    pub(crate) fn prefilter(&self, folded_path: &str) -> bool {
        self.folded_terms.iter().all(|term| {
            let mut it = term.iter().peekable();
            for c in folded_path.chars() {
                match it.peek() {
                    Some(&&want) if want == c => {
                        it.next();
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            it.peek().is_none()
        })
    }

    /// Full score; fills `indices` (sorted, deduplicated) when provided.
    pub(crate) fn score(&self, path: &str, scratch: &mut Scratch, mut indices: Option<&mut Vec<u32>>) -> Option<u32> {
        scratch.load(path, self.case_sensitive);
        if let Some(out) = indices.as_deref_mut() {
            out.clear();
        }
        let mut total: i64 = 0;
        for term in &self.terms {
            let s = scratch.score_term(term, indices.as_deref_mut())?;
            total += i64::from(s);
        }
        if let Some(out) = indices {
            out.sort_unstable();
            out.dedup();
        }
        Some(total.clamp(1, i64::from(u32::MAX)) as u32)
    }
}

/// Reusable buffers for scoring (one per thread).
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    chars: Vec<char>,
    cmp: Vec<char>,
    bonus: Vec<i32>,
    basename_start: usize,
    h: Vec<i32>,
    run_bonus: Vec<i32>,
    prev: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Separator,
    Delimiter,
    Lower,
    Upper,
    Digit,
    Other,
}

fn class_of(c: char) -> Class {
    match c {
        '/' | '\\' => Class::Separator,
        '_' | '-' | '.' | ' ' | ',' | ':' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '@' | '+' | '#' => {
            Class::Delimiter
        }
        c if c.is_lowercase() => Class::Lower,
        c if c.is_uppercase() => Class::Upper,
        c if c.is_numeric() => Class::Digit,
        _ => Class::Other,
    }
}

fn position_bonus(prev: Option<Class>, cur: Class) -> i32 {
    match (prev, cur) {
        (_, Class::Separator) => 0,
        (None, _) | (Some(Class::Separator), _) => BONUS_SEGMENT,
        (Some(Class::Delimiter), Class::Delimiter) => 0,
        (Some(Class::Delimiter), _) => BONUS_DELIMITER,
        (Some(Class::Lower), Class::Upper) => BONUS_CAMEL,
        (Some(Class::Lower | Class::Upper | Class::Other), Class::Digit) => BONUS_CAMEL,
        _ => 0,
    }
}

impl Scratch {
    fn load(&mut self, path: &str, case_sensitive: bool) {
        self.chars.clear();
        self.chars.extend(path.chars());
        self.cmp.clear();
        if case_sensitive {
            self.cmp.extend(self.chars.iter().map(|&c| if c == '\\' { '/' } else { c }));
        } else {
            self.cmp.extend(self.chars.iter().map(|&c| if c == '\\' { '/' } else { fold(c) }));
        }
        self.bonus.clear();
        let mut prev = None;
        for &c in &self.chars {
            let cls = class_of(c);
            self.bonus.push(position_bonus(prev, cls));
            prev = Some(cls);
        }
        self.basename_start = self.chars.iter().rposition(|&c| c == '/' || c == '\\').map(|i| i + 1).unwrap_or(0);
    }

    fn basename_bonus(&self, j: usize) -> i32 {
        if j >= self.basename_start {
            BONUS_BASENAME
        } else {
            0
        }
    }

    /// Best alignment of `term` within the loaded path.
    fn score_term(&mut self, term: &[char], indices: Option<&mut Vec<u32>>) -> Option<i32> {
        let n = self.cmp.len();
        let m = term.len();
        if m == 0 {
            return Some(0);
        }
        if m > n {
            return None;
        }
        // Greedy forward scan: is it a subsequence at all, and where can it start?
        let mut qi = 0;
        let mut lo = 0;
        for (j, &c) in self.cmp.iter().enumerate() {
            if c == term[qi] {
                if qi == 0 {
                    lo = j;
                }
                qi += 1;
                if qi == m {
                    break;
                }
            }
        }
        if qi < m {
            return None;
        }
        let hi = self.cmp.iter().rposition(|&c| c == term[m - 1])?;
        let width = hi + 1 - lo;

        self.h.clear();
        self.h.resize(m * width, NEG);
        self.run_bonus.clear();
        self.run_bonus.resize(m * width, 0);
        self.prev.clear();
        self.prev.resize(m * width, u32::MAX);

        for jj in 0..width {
            let j = lo + jj;
            if self.cmp[j] == term[0] {
                let b = self.bonus[j];
                self.h[jj] = SCORE_MATCH + b * FIRST_CHAR_MULTIPLIER + self.basename_bonus(j);
                self.run_bonus[jj] = b;
            }
        }

        for (i, &want) in term.iter().enumerate().skip(1) {
            let row = i * width;
            let prow = row - width;
            let mut far_best = NEG;
            let mut far_pos = u32::MAX;
            for jj in i..width {
                // Positions far enough back that the gap penalty is capped.
                if jj > GAP_CAP_DISTANCE {
                    let k = jj - GAP_CAP_DISTANCE - 1;
                    if self.h[prow + k] > far_best {
                        far_best = self.h[prow + k];
                        far_pos = k as u32;
                    }
                }
                let j = lo + jj;
                if self.cmp[j] != want {
                    continue;
                }
                let b = self.bonus[j];
                let mut best = NEG;
                let mut best_prev = u32::MAX;
                let mut best_rb = b;

                // Consecutive with the previous query character.
                let diag = self.h[prow + jj - 1];
                if diag > NEG {
                    let rb = self.run_bonus[prow + jj - 1];
                    best = diag + SCORE_MATCH + b.max(rb).max(BONUS_CONSECUTIVE);
                    best_prev = (jj - 1) as u32;
                    best_rb = if b >= BONUS_DELIMITER { b } else { rb };
                }
                // Gapped, near positions (exact penalty).
                if jj >= 2 {
                    let kmin = jj.saturating_sub(GAP_CAP_DISTANCE);
                    for k in kmin..=jj - 2 {
                        let v = self.h[prow + k];
                        if v <= NEG {
                            continue;
                        }
                        let d = (jj - 1 - k) as i32;
                        let s = v + GAP_START + GAP_EXTEND * (d - 1) + SCORE_MATCH + b;
                        if s > best {
                            best = s;
                            best_prev = k as u32;
                            best_rb = b;
                        }
                    }
                }
                // Gapped, far positions (capped penalty).
                if far_best > NEG {
                    let s = far_best + GAP_MAX + SCORE_MATCH + b;
                    if s > best {
                        best = s;
                        best_prev = far_pos;
                        best_rb = b;
                    }
                }
                if best > NEG {
                    self.h[row + jj] = best + self.basename_bonus(j);
                    self.prev[row + jj] = best_prev;
                    self.run_bonus[row + jj] = best_rb;
                }
            }
        }

        let last = (m - 1) * width;
        let (best_jj, best) =
            (0..width).map(|jj| (jj, self.h[last + jj])).max_by_key(|&(jj, s)| (s, std::cmp::Reverse(jj)))?;
        if best <= NEG {
            return None;
        }
        if let Some(out) = indices {
            let mut jj = best_jj;
            for i in (0..m).rev() {
                out.push((lo + jj) as u32);
                if i > 0 {
                    jj = self.prev[i * width + jj] as usize;
                }
            }
        }
        Some(best)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(q: &str, p: &str) -> u32 {
        fuzzy_match(q, p).map(|(s, _)| s).unwrap_or(0)
    }

    #[test]
    fn subsequence_required() {
        assert!(fuzzy_match("xyz", "src/main.rs").is_none());
        assert!(fuzzy_match("smr", "src/main.rs").is_some());
        assert!(fuzzy_match("   ", "src/main.rs").is_none());
    }

    #[test]
    fn indices_point_at_matched_chars() {
        let (_, idx) = fuzzy_match("mainrs", "src/main.rs").unwrap();
        assert_eq!(idx, vec![4, 5, 6, 7, 9, 10]);
        let path: Vec<char> = "src/main.rs".chars().collect();
        let picked: String = idx.iter().map(|&i| path[i as usize]).collect();
        assert_eq!(picked, "mainrs");
    }

    #[test]
    fn ranking_sanity() {
        assert!(score("mainrs", "src/main.rs") > score("mainrs", "src/domain/remains.rs"));
        // basename beats directory
        assert!(score("foo", "src/foo.rs") > score("foo", "foo/bar/baz.rs"));
        // segment start beats mid-word
        assert!(score("cfg", "src/cfg/mod.rs") > score("cfg", "src/xcfgx/mod.rs"));
        // camelCase humps
        assert!(score("fb", "src/FooBar.ts") > score("fb", "src/fabric.ts"));
        // consecutive beats scattered
        assert!(score("test", "tests/x.rs") > score("test", "t/e/s/t.rs"));
    }

    #[test]
    fn smart_case() {
        assert!(fuzzy_match("readme", "README.md").is_some());
        assert!(fuzzy_match("README", "readme.md").is_none());
        assert!(fuzzy_match("ReadMe", "src/ReadMe.md").is_some());
    }

    #[test]
    fn multiple_terms_and_backslashes() {
        let (_, idx) = fuzzy_match("src main", "src/main.rs").unwrap();
        assert_eq!(idx, vec![0, 1, 2, 4, 5, 6, 7]);
        assert!(fuzzy_match("src\\main", "src/main.rs").is_some());
        assert!(fuzzy_match("src zzz", "src/main.rs").is_none());
    }

    #[test]
    fn unicode_paths_keep_char_indices() {
        let (_, idx) = fuzzy_match("éa", "docs/Éclair/a.md").unwrap();
        let chars: Vec<char> = "docs/Éclair/a.md".chars().collect();
        assert_eq!(chars[idx[0] as usize], 'É');
        assert_eq!(chars[idx[1] as usize], 'a');
    }

    #[test]
    fn long_gaps_still_positive() {
        let long = format!("a{}b", "x".repeat(500));
        assert!(score("ab", &long) >= 1);
    }
}
