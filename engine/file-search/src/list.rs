//! Indented directory tree for the agent's `list_dir` tool.

use std::fs;
use std::path::Path;

use crate::Error;

/// Directories that are shown but never descended into.
const SKIPPED_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", ".venv", "__pycache__"];

struct Item {
    name: String,
    is_dir: bool,
    is_symlink: bool,
}

/// Render `path` as an indented tree, `depth` levels deep (`0` is treated as `1`), showing at most
/// `limit` entries (`0` = unlimited).
///
/// Directories come first, then files, each sorted by name. Heavy directories (`.git`,
/// `node_modules`, `target`, `dist`, `.venv`, `__pycache__`) are listed as `name/ (skipped)`.
/// When the limit is reached, each open level ends with `… N more`.
pub fn list_dir(path: &Path, depth: usize, limit: usize) -> Result<String, Error> {
    let meta = fs::metadata(path).map_err(|e| Error::io(path, e))?;
    if !meta.is_dir() {
        return Err(Error::InvalidArgument(format!("not a directory: {}", path.display())));
    }
    let mut out = String::new();
    let shown = path.to_string_lossy();
    out.push_str(shown.trim_end_matches(['/', '\\']));
    out.push(std::path::MAIN_SEPARATOR);
    out.push('\n');
    let mut state = State { remaining: if limit == 0 { usize::MAX } else { limit }, stopped: false };
    walk(path, 1, depth.max(1), &mut state, &mut out)?;
    Ok(out)
}

struct State {
    remaining: usize,
    stopped: bool,
}

fn read_items(dir: &Path) -> std::io::Result<Vec<Item>> {
    let mut items = Vec::new();
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = entry.file_type()?;
        let is_symlink = ft.is_symlink();
        let is_dir = if is_symlink { entry.path().is_dir() } else { ft.is_dir() };
        items.push(Item { name, is_dir, is_symlink });
    }
    items.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(items)
}

fn walk(dir: &Path, level: usize, max_depth: usize, state: &mut State, out: &mut String) -> Result<(), Error> {
    let items = match read_items(dir) {
        Ok(items) => items,
        Err(e) if level == 1 => return Err(Error::io(dir, e)),
        Err(_) => {
            out.push_str(&"  ".repeat(level));
            out.push_str("(unreadable)\n");
            return Ok(());
        }
    };
    let indent = "  ".repeat(level);
    let total = items.len();
    for (i, item) in items.into_iter().enumerate() {
        if state.stopped || state.remaining == 0 {
            state.stopped = true;
            out.push_str(&format!("{indent}… {} more\n", total - i));
            return Ok(());
        }
        state.remaining -= 1;
        if item.is_dir {
            if SKIPPED_DIRS.contains(&item.name.as_str()) {
                out.push_str(&format!("{indent}{}/ (skipped)\n", item.name));
            } else if item.is_symlink {
                out.push_str(&format!("{indent}{}/ (symlink)\n", item.name));
            } else {
                out.push_str(&format!("{indent}{}/\n", item.name));
                if level < max_depth {
                    walk(&dir.join(&item.name), level + 1, max_depth, state, out)?;
                }
            }
        } else if item.is_symlink {
            out.push_str(&format!("{indent}{} (symlink)\n", item.name));
        } else {
            out.push_str(&format!("{indent}{}\n", item.name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for d in ["src/util", "node_modules/pkg", "target/debug", ".git", "docs"] {
            fs::create_dir_all(r.join(d)).unwrap();
        }
        for f in ["src/main.rs", "src/util/mod.rs", "node_modules/pkg/index.js", "README.md", "Cargo.toml", ".env"] {
            fs::write(r.join(f), "x").unwrap();
        }
        dir
    }

    fn body(text: &str) -> String {
        text.lines().skip(1).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn tree_with_skips_and_ordering() {
        let dir = fixture();
        let text = list_dir(dir.path(), 2, 0).unwrap();
        assert!(text.lines().next().unwrap().ends_with(std::path::MAIN_SEPARATOR));
        assert_eq!(
            body(&text),
            [
                "  .git/ (skipped)",
                "  docs/",
                "  node_modules/ (skipped)",
                "  src/",
                "    util/",
                "    main.rs",
                "  target/ (skipped)",
                "  .env",
                "  Cargo.toml",
                "  README.md",
            ]
            .join("\n")
        );

        let deeper = list_dir(dir.path(), 3, 0).unwrap();
        assert!(deeper.contains("      mod.rs"));
        let shallow = list_dir(dir.path(), 0, 0).unwrap();
        assert!(!shallow.contains("main.rs"));
    }

    #[test]
    fn limit_reports_remaining() {
        let dir = fixture();
        let text = list_dir(dir.path(), 2, 5).unwrap();
        assert_eq!(
            body(&text),
            [
                "  .git/ (skipped)",
                "  docs/",
                "  node_modules/ (skipped)",
                "  src/",
                "    util/",
                "    … 1 more",
                "  … 4 more"
            ]
            .join("\n")
        );
        assert!(matches!(list_dir(&dir.path().join("README.md"), 1, 0), Err(Error::InvalidArgument(_))));
        assert!(matches!(list_dir(&dir.path().join("missing"), 1, 0), Err(Error::NotFound(_))));
    }
}
