//! Cached file list + fuzzy search (Ctrl+P and the agent's file finder).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use ignore::WalkState;
use odex_protocol::FileMatch;

use crate::fuzzy::{fold_str, Query, Scratch};
use crate::walk::{is_file, relative_slash_path, walker};

/// Above this many entries, scoring is spread over several threads.
const PARALLEL_THRESHOLD: usize = 8_000;

#[derive(Debug, Clone)]
struct Entry {
    root: u32,
    /// Relative to the root, forward slashes.
    path: String,
    /// `path` case-folded char by char (for the prefilter).
    folded: String,
}

/// Thread-local batch of walked entries, appended to the shared list when full or dropped.
struct Flush<'a> {
    local: Vec<Entry>,
    sink: &'a Mutex<Vec<Entry>>,
}

impl Drop for Flush<'_> {
    fn drop(&mut self) {
        if !self.local.is_empty() {
            if let Ok(mut v) = self.sink.lock() {
                v.append(&mut self.local);
            }
        }
    }
}

/// A cached list of files under a set of roots.
///
/// Respects `.gitignore` / `.ignore` / git excludes, skips `.git`, includes other hidden files.
#[derive(Debug, Clone)]
pub struct FileIndex {
    roots: Vec<PathBuf>,
    max_files: usize,
    entries: Vec<Entry>,
    truncated: bool,
}

impl FileIndex {
    /// Walk `roots` and cache up to `max_files` files (`0` = unlimited).
    pub fn build(roots: &[PathBuf], max_files: usize) -> Self {
        let mut index = FileIndex { roots: roots.to_vec(), max_files, entries: Vec::new(), truncated: false };
        index.refresh();
        index
    }

    /// Re-walk the roots, replacing the cached list.
    pub fn refresh(&mut self) {
        let cap = if self.max_files == 0 { usize::MAX } else { self.max_files };
        let count = AtomicUsize::new(0);
        let collected: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
        let mut truncated = false;
        for (root_idx, root) in self.roots.iter().enumerate() {
            if count.load(Ordering::Relaxed) >= cap {
                truncated = true;
                break;
            }
            let hit_cap = std::sync::atomic::AtomicBool::new(false);
            walker(root, true).build_parallel().run(|| {
                let root = root.clone();
                let count = &count;
                let collected = &collected;
                let hit_cap = &hit_cap;
                let mut flush = Flush { local: Vec::new(), sink: collected };
                Box::new(move |result| {
                    let entry = match result {
                        Ok(e) => e,
                        Err(_) => return WalkState::Continue,
                    };
                    if !is_file(&entry) {
                        return WalkState::Continue;
                    }
                    if count.fetch_add(1, Ordering::Relaxed) >= cap {
                        hit_cap.store(true, Ordering::Relaxed);
                        return WalkState::Quit;
                    }
                    let path = relative_slash_path(&root, entry.path());
                    let folded = fold_str(&path);
                    flush.local.push(Entry { root: root_idx as u32, path, folded });
                    if flush.local.len() >= 512 {
                        if let Ok(mut v) = flush.sink.lock() {
                            v.append(&mut flush.local);
                        }
                    }
                    WalkState::Continue
                })
            });
            if hit_cap.load(Ordering::Relaxed) {
                truncated = true;
            }
        }
        let mut entries = collected.into_inner().unwrap_or_default();
        entries.sort_by(|a, b| a.root.cmp(&b.root).then_with(|| a.path.cmp(&b.path)));
        entries.dedup_by(|a, b| a.root == b.root && a.path == b.path);
        self.entries = entries;
        self.truncated = truncated;
    }

    /// Fuzzy-search the cached files. An empty query returns the shortest paths first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<FileMatch> {
        let limit = if limit == 0 { usize::MAX } else { limit };
        let Some(query) = Query::parse(query) else {
            let mut all: Vec<&Entry> = self.entries.iter().collect();
            all.sort_by(|a, b| a.path.len().cmp(&b.path.len()).then_with(|| a.path.cmp(&b.path)));
            return all.into_iter().take(limit).map(|e| self.to_match(e, 0, Vec::new())).collect();
        };

        let score_chunk = |chunk: &[Entry], offset: usize| -> Vec<(u32, usize)> {
            let mut scratch = Scratch::default();
            let mut out = Vec::new();
            for (i, e) in chunk.iter().enumerate() {
                if !query.prefilter(&e.folded) {
                    continue;
                }
                if let Some(s) = query.score(&e.path, &mut scratch, None) {
                    out.push((s, offset + i));
                }
            }
            out
        };

        let mut scored: Vec<(u32, usize)> = if self.entries.len() >= PARALLEL_THRESHOLD {
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 16);
            let chunk_size = self.entries.len().div_ceil(threads);
            std::thread::scope(|s| {
                let handles: Vec<_> = self
                    .entries
                    .chunks(chunk_size)
                    .enumerate()
                    .map(|(ci, chunk)| {
                        let score_chunk = &score_chunk;
                        s.spawn(move || score_chunk(chunk, ci * chunk_size))
                    })
                    .collect();
                handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
            })
        } else {
            score_chunk(&self.entries, 0)
        };

        scored.sort_unstable_by(|&(sa, ia), &(sb, ib)| {
            let (a, b) = (&self.entries[ia], &self.entries[ib]);
            sb.cmp(&sa)
                .then_with(|| a.path.len().cmp(&b.path.len()))
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.root.cmp(&b.root))
        });
        scored.truncate(limit);

        let mut scratch = Scratch::default();
        scored
            .into_iter()
            .map(|(score, i)| {
                let e = &self.entries[i];
                let mut indices = Vec::new();
                query.score(&e.path, &mut scratch, Some(&mut indices));
                self.to_match(e, score, indices)
            })
            .collect()
    }

    /// Number of cached files.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no files are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `max_files` cut the walk short.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The roots this index covers.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Iterate cached files as `(root, relative path)`.
    pub fn files(&self) -> impl Iterator<Item = (&PathBuf, &str)> {
        self.entries.iter().map(|e| (&self.roots[e.root as usize], e.path.as_str()))
    }

    fn to_match(&self, e: &Entry, score: u32, indices: Vec<u32>) -> FileMatch {
        FileMatch {
            path: e.path.clone(),
            root: self.roots[e.root as usize].to_string_lossy().into_owned(),
            score,
            indices,
        }
    }
}

/// Build a one-off index of `roots` and search it.
pub fn fuzzy_search(roots: &[PathBuf], query: &str, limit: usize) -> Vec<FileMatch> {
    FileIndex::build(roots, 0).search(query, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(root: &std::path::Path, rel: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn respects_gitignore_and_skips_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "src/main.rs");
        touch(root, "src/domain/remains.rs");
        touch(root, ".github/workflows/ci.yml");
        touch(root, "target/debug/main.rs");
        touch(root, ".git/HEAD");
        touch(root, "notes.log");
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();

        let index = FileIndex::build(&[root.to_path_buf()], 0);
        let files: Vec<&str> = index.files().map(|(_, p)| p).collect();
        assert!(files.contains(&"src/main.rs"));
        assert!(files.contains(&".github/workflows/ci.yml"), "hidden files are included: {files:?}");
        assert!(files.contains(&".gitignore"));
        assert!(!files.iter().any(|f| f.starts_with("target/")), "{files:?}");
        assert!(!files.iter().any(|f| f.starts_with(".git/")), "{files:?}");
        assert!(!files.contains(&"notes.log"));
        assert_eq!(index.len(), 4);
        assert!(!index.is_truncated());

        let results = index.search("mainrs", 10);
        assert_eq!(results[0].path, "src/main.rs");
        assert_eq!(results[0].root, root.to_string_lossy());
        assert_eq!(results[1].path, "src/domain/remains.rs");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn empty_query_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in ["a/b/c/deep.txt", "top.txt", "mid/file.txt"] {
            touch(root, name);
        }
        let index = FileIndex::build(&[root.to_path_buf()], 0);
        let all = index.search("", 0);
        assert_eq!(
            all.iter().map(|m| m.path.as_str()).collect::<Vec<_>>(),
            ["top.txt", "mid/file.txt", "a/b/c/deep.txt"]
        );
        assert_eq!(index.search("txt", 2).len(), 2);

        let capped = FileIndex::build(&[root.to_path_buf()], 2);
        assert_eq!(capped.len(), 2);
        assert!(capped.is_truncated());
    }

    #[test]
    fn shorter_paths_win_ties_and_refresh_sees_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "x/lib.rs");
        touch(root, "xx/lib.rs");
        let mut index = FileIndex::build(&[root.to_path_buf()], 0);
        let r = index.search("lib", 10);
        assert_eq!(r[0].path, "x/lib.rs");
        touch(root, "lib.rs");
        index.refresh();
        assert_eq!(index.len(), 3);
        assert_eq!(index.search("lib", 10)[0].path, "lib.rs");
    }

    #[test]
    fn multiple_roots_and_parallel_scoring() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for i in 0..PARALLEL_THRESHOLD / 2 + 10 {
            touch(a.path(), &format!("d{}/f{i}.txt", i % 50));
            touch(b.path(), &format!("e{}/g{i}.md", i % 50));
        }
        touch(b.path(), "special/needle.rs");
        let index = FileIndex::build(&[a.path().to_path_buf(), b.path().to_path_buf()], 0);
        assert!(index.len() >= PARALLEL_THRESHOLD);
        let r = index.search("needle", 5);
        assert_eq!(r[0].path, "special/needle.rs");
        assert_eq!(r[0].root, b.path().to_string_lossy());
        let via_fn = fuzzy_search(&[b.path().to_path_buf()], "needle", 1);
        assert_eq!(via_fn[0].path, "special/needle.rs");
    }
}
