//! Git-backed helpers used by turns: undo snapshots, working-set snapshot,
//! diff stats, review results.

use std::path::Path;

use odex_git::Git;
use odex_protocol::*;

use crate::engine::Engine;
use crate::thread::{new_id, ThreadRt};

pub fn snapshot_ref(thread_id: &str, turn_id: &str) -> String {
    format!("refs/odex/snapshots/{thread_id}/{turn_id}")
}

/// Hidden git ref capturing the working tree before a turn (for rollback).
pub async fn snapshot_turn(cwd: &str, thread_id: &str, turn_id: &str) {
    let g = Git::new(cwd);
    if !g.is_repo().await {
        return;
    }
    if let Err(e) = g.snapshot(&snapshot_ref(thread_id, turn_id), &format!("odex snapshot before turn {turn_id}")).await
    {
        tracing::debug!("snapshot failed: {e}");
    }
}

pub async fn diff_stats(cwd: &str) -> Option<DiffStats> {
    let g = Git::new(cwd);
    if !g.is_repo().await {
        return None;
    }
    g.diff_stats(&DiffTarget::Uncommitted).await.ok()
}

/// `git status --short`, `git diff --stat` and files touched, for the pinned block.
pub async fn working_set(cwd: &str, touched: &[String]) -> Option<String> {
    let g = Git::new(cwd);
    let mut s = String::new();
    if g.is_repo().await {
        if let Ok(st) = g.status().await {
            s.push_str(&format!("Branch: {}\n", st.branch.clone().unwrap_or_else(|| "(detached)".into())));
            if st.files.is_empty() {
                s.push_str("git status: clean\n");
            } else {
                s.push_str("git status --short:\n");
                for f in st.files.iter().take(60) {
                    s.push_str(&format!("{} {}\n", f.code, f.path));
                }
                if st.files.len() > 60 {
                    s.push_str(&format!("… {} more\n", st.files.len() - 60));
                }
            }
        }
        if let Ok(d) = g.diff(&DiffTarget::Uncommitted, false, Some(0)).await {
            if !d.files.is_empty() {
                s.push_str("git diff --stat:\n");
                for f in d.files.iter().take(60) {
                    s.push_str(&format!("{} | +{} -{}\n", f.path, f.additions, f.deletions));
                }
            }
        }
    }
    if !touched.is_empty() {
        s.push_str(&format!(
            "Files touched in this thread: {}\n",
            touched.iter().take(80).cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Parse a review turn's final JSON into a Review item (with a repair pass).
pub async fn emit_review(engine: &Engine, rt: &ThreadRt, turn_id: &str, text: &str) {
    let parsed = odex_llm::repair::parse_lenient(text).ok().map(|(v, _)| v).filter(|v| v.get("findings").is_some());
    let item = match parsed {
        Some(v) => {
            let findings: Vec<ReviewFinding> = v["findings"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|f| {
                            Some(ReviewFinding {
                                title: f.get("title")?.as_str()?.to_string(),
                                body: f.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string(),
                                priority: f.get("priority").and_then(|p| p.as_u64()).unwrap_or(2) as u32,
                                confidence: f.get("confidence").and_then(|c| c.as_f64()),
                                path: f.get("path").and_then(|p| p.as_str()).map(String::from),
                                line_start: f.get("line_start").and_then(|l| l.as_u64()).map(|l| l as u32),
                                line_end: f.get("line_end").and_then(|l| l.as_u64()).map(|l| l as u32),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut findings = findings;
            findings.sort_by_key(|f| f.priority);
            ThreadItem::Review {
                id: new_id("item"),
                summary: v.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                findings,
                overall_correctness: v.get("overall_correctness").and_then(|s| s.as_str()).map(String::from),
            }
        }
        None => ThreadItem::Review {
            id: new_id("item"),
            summary: text.to_string(),
            findings: vec![],
            overall_correctness: None,
        },
    };
    crate::turn::complete_item(engine, rt, turn_id, item);
}

/// Text of the diff a review turn looks at.
pub async fn review_diff_text(
    cwd: &str,
    target: &DiffTarget,
    thread_id: &str,
    last_turn: Option<&str>,
) -> anyhow::Result<String> {
    let g = Git::new(cwd);
    let d = match target {
        DiffTarget::LastTurn { .. } => match last_turn {
            Some(t) => g.diff_from_ref(&snapshot_ref(thread_id, t), false).await?,
            None => g.diff(&DiffTarget::Uncommitted, false, None).await?,
        },
        other => g.diff(other, false, None).await?,
    };
    let mut s = String::new();
    for f in &d.files {
        s.push_str(&f.header);
        if !s.ends_with('\n') {
            s.push('\n');
        }
        for h in &f.hunks {
            s.push_str(&h.header);
            s.push('\n');
            for l in &h.lines {
                let p = match l.kind {
                    DiffLineKind::Add => '+',
                    DiffLineKind::Del => '-',
                    DiffLineKind::Context => ' ',
                    DiffLineKind::Meta => '\\',
                };
                s.push(p);
                s.push_str(&l.text);
                s.push('\n');
            }
        }
    }
    Ok(s)
}

pub fn is_git_dir(p: &Path) -> bool {
    p.join(".git").exists()
}
