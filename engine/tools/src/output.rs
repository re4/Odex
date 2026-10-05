//! Tier 0 output capping and the on-disk store of full outputs (`ref:out_N`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Head+tail truncation result.
#[derive(Debug, Clone, PartialEq)]
pub struct Capped {
    pub text: String,
    pub truncated: bool,
    pub omitted_lines: usize,
    pub total_lines: usize,
}

/// Keep the head (60%) and tail (40%) of `text` within `max_chars`, cutting at
/// line boundaries, with a marker naming the ref for the full output.
pub fn cap(text: &str, max_chars: usize, out_ref: Option<&str>) -> Capped {
    let total_lines = text.lines().count();
    if text.chars().count() <= max_chars {
        return Capped { text: text.to_string(), truncated: false, omitted_lines: 0, total_lines };
    }
    let lines: Vec<&str> = text.lines().collect();
    let head_budget = max_chars * 6 / 10;
    let tail_budget = max_chars - head_budget;
    let mut head = Vec::new();
    let mut used = 0;
    for l in &lines {
        let n = l.chars().count() + 1;
        if used + n > head_budget {
            break;
        }
        used += n;
        head.push(*l);
    }
    let mut tail = Vec::new();
    used = 0;
    for l in lines.iter().rev() {
        let n = l.chars().count() + 1;
        if used + n > tail_budget || head.len() + tail.len() >= lines.len() {
            break;
        }
        used += n;
        tail.push(*l);
    }
    tail.reverse();
    // A single enormous line: fall back to char slicing.
    if head.is_empty() && tail.is_empty() {
        let h: String = text.chars().take(head_budget).collect();
        let t: String = text.chars().rev().take(tail_budget).collect::<Vec<_>>().into_iter().rev().collect();
        let marker = marker(0, text.chars().count() - head_budget - tail_budget, out_ref);
        return Capped { text: format!("{h}\n{marker}\n{t}"), truncated: true, omitted_lines: 0, total_lines };
    }
    let omitted = lines.len() - head.len() - tail.len();
    let marker = marker(omitted, 0, out_ref);
    Capped {
        text: format!("{}\n{marker}\n{}", head.join("\n"), tail.join("\n")),
        truncated: true,
        omitted_lines: omitted,
        total_lines,
    }
}

fn marker(lines: usize, chars: usize, out_ref: Option<&str>) -> String {
    let what = if lines > 0 { format!("{} lines", fmt_num(lines)) } else { format!("{} chars", fmt_num(chars)) };
    match out_ref {
        Some(r) => format!("[… {what} omitted — full output saved as {r}; use read_output(\"{r}\") …]"),
        None => format!("[… {what} omitted …]"),
    }
}

pub fn fmt_num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Inline output cap scaled to the context window (tokens), unless configured.
pub fn cap_tokens_for_window(window: u32, configured: u32) -> u32 {
    if configured > 0 {
        return configured;
    }
    ((window as f64) * 0.08).clamp(400.0, 12_000.0) as u32
}

/// Stores full outputs on disk per thread; refs are `ref:out_N`.
pub struct OutputStore {
    dir: PathBuf,
    next: AtomicU64,
}

impl OutputStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        // continue numbering after existing files (resume)
        let mut max = 0u64;
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(num) =
                    n.strip_prefix("out_").and_then(|r| r.strip_suffix(".txt")).and_then(|x| x.parse::<u64>().ok())
                {
                    max = max.max(num);
                }
            }
        }
        Self { dir, next: AtomicU64::new(max + 1) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn save(&self, text: &str) -> std::io::Result<String> {
        std::fs::create_dir_all(&self.dir)?;
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        std::fs::write(self.dir.join(format!("out_{n}.txt")), text)?;
        Ok(format!("ref:out_{n}"))
    }

    pub fn path_of(&self, r: &str) -> Option<PathBuf> {
        let id = r.trim().trim_start_matches("ref:");
        if !id.starts_with("out_") || id.contains(['/', '\\', '.']) {
            return None;
        }
        Some(self.dir.join(format!("{id}.txt")))
    }

    /// Page through a stored output (1-based line offset).
    pub fn read(&self, r: &str, offset: usize, limit: usize) -> Result<String, String> {
        let p = self.path_of(r).ok_or_else(|| format!("invalid ref `{r}` (expected ref:out_N)"))?;
        let text = std::fs::read_to_string(&p).map_err(|_| format!("no stored output for `{r}`"))?;
        let lines: Vec<&str> = text.lines().collect();
        let start = offset.max(1) - 1;
        if start >= lines.len() && !lines.is_empty() {
            return Err(format!("offset {offset} is past the end ({} lines)", lines.len()));
        }
        let end = (start + limit.max(1)).min(lines.len());
        let mut out = String::new();
        for (i, l) in lines[start..end].iter().enumerate() {
            out.push_str(&format!("{:>6}\t{l}\n", start + i + 1));
        }
        if end < lines.len() {
            out.push_str(&format!("[… {} more lines; continue with offset={} …]\n", lines.len() - end, end + 1));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_head_and_tail() {
        let text: String = (1..=1000).map(|i| format!("line {i}\n")).collect();
        let c = cap(&text, 500, Some("ref:out_3"));
        assert!(c.truncated);
        assert!(c.text.starts_with("line 1\n"));
        assert!(c.text.trim_end().ends_with("line 1000"));
        assert!(c.text.contains("lines omitted — full output saved as ref:out_3"));
        assert!(c.text.len() < 700);
        assert_eq!(c.total_lines, 1000);
    }

    #[test]
    fn short_untouched() {
        assert!(!cap("hi", 10, None).truncated);
    }

    #[test]
    fn huge_single_line() {
        let c = cap(&"x".repeat(5000), 100, None);
        assert!(c.truncated);
        assert!(c.text.contains("chars omitted"));
    }

    #[test]
    fn store_roundtrip_and_paging() {
        let d = tempfile::tempdir().unwrap();
        let s = OutputStore::new(d.path());
        let r = s.save(&(1..=50).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n")).unwrap();
        assert_eq!(r, "ref:out_1");
        let page = s.read(&r, 10, 5).unwrap();
        assert!(page.contains("    10\tl10"));
        assert!(page.contains("continue with offset=15"));
        assert!(s.read("ref:../x", 1, 1).is_err());
        let s2 = OutputStore::new(d.path());
        assert_eq!(s2.save("x").unwrap(), "ref:out_2");
    }

    #[test]
    fn numbers() {
        assert_eq!(fmt_num(2431), "2,431");
        assert_eq!(fmt_num(12), "12");
    }
}
