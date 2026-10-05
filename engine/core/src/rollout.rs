//! Append-only JSONL session rollouts (`~/.odex/sessions/YYYY/MM/DD/<id>.jsonl`).
//! Replaying a rollout rebuilds the thread, its turns/items and the model
//! context, so threads resume after an engine crash.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use odex_context::{ContextState, HistoryEntry};
use odex_protocol::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RolloutLine {
    Meta {
        thread: Thread,
    },
    ThreadUpdate {
        thread: Thread,
    },
    TurnStarted {
        turn: Turn,
    },
    TurnCompleted {
        turn: Turn,
    },
    Item {
        turn_id: String,
        item: ThreadItem,
    },
    /// A model-history entry appended.
    History {
        entry: HistoryEntry,
    },
    /// Full context checkpoint (after pruning/compaction); replaces prior history.
    Checkpoint {
        state: ContextState,
    },
    /// Turns from `turn_id` onward were rolled back.
    Rollback {
        turn_id: String,
    },
    Plan {
        explanation: Option<String>,
        plan: Vec<PlanStep>,
    },
    Pinned {
        original_task: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
struct Stamped<'a> {
    ts: i64,
    #[serde(flatten)]
    line: &'a RolloutLine,
}

#[derive(Debug, Clone, Deserialize)]
struct StampedOwned {
    #[serde(default)]
    #[allow(dead_code)]
    ts: i64,
    #[serde(flatten)]
    line: RolloutLine,
}

pub struct RolloutWriter {
    path: PathBuf,
    file: std::fs::File,
}

impl RolloutWriter {
    pub fn create(sessions_dir: &Path, thread_id: &str) -> std::io::Result<Self> {
        let now = chrono::Local::now();
        let dir = sessions_dir
            .join(now.format("%Y").to_string())
            .join(now.format("%m").to_string())
            .join(now.format("%d").to_string());
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{thread_id}.jsonl"));
        Self::open(&path)
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        // A crash can leave a partial last line; terminate it so the next
        // append starts on a fresh line.
        let needs_newline = (|| -> std::io::Result<bool> {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(path)?;
            if f.metadata()?.len() == 0 {
                return Ok(false);
            }
            f.seek(SeekFrom::End(-1))?;
            let mut b = [0u8; 1];
            f.read_exact(&mut b)?;
            Ok(b[0] != b'\n')
        })()
        .unwrap_or(false);
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        if needs_newline {
            file.write_all(b"\n")?;
        }
        Ok(Self { path: path.to_path_buf(), file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&mut self, line: &RolloutLine) {
        let stamped = Stamped { ts: chrono::Utc::now().timestamp_millis(), line };
        match serde_json::to_string(&stamped) {
            Ok(mut s) => {
                s.push('\n');
                if let Err(e) = self.file.write_all(s.as_bytes()) {
                    tracing::error!("rollout write failed ({}): {e}", self.path.display());
                }
            }
            Err(e) => tracing::error!("rollout serialize failed: {e}"),
        }
    }
}

/// Everything recovered from a rollout.
#[derive(Debug, Clone, Default)]
pub struct Replayed {
    pub thread: Option<Thread>,
    pub turns: Vec<Turn>,
    pub context: ContextState,
    pub plan: (Option<String>, Vec<PlanStep>),
    pub original_task: Option<String>,
}

pub fn read(path: &Path) -> std::io::Result<Replayed> {
    let f = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(f);
    let mut r = Replayed { context: ContextState::new(), ..Default::default() };
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let parsed: StampedOwned = match serde_json::from_str(&line) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("skipping corrupt rollout line in {}: {e}", path.display());
                continue;
            }
        };
        apply(&mut r, parsed.line);
    }
    // A turn left in progress (crash) is reported as interrupted.
    for t in r.turns.iter_mut() {
        if t.status == TurnStatus::InProgress {
            t.status = TurnStatus::Interrupted;
            t.error = Some(TurnError {
                message: "The engine stopped while this turn was running.".into(),
                code: Some("interrupted".into()),
            });
        }
    }
    Ok(r)
}

fn apply(r: &mut Replayed, line: RolloutLine) {
    match line {
        RolloutLine::Meta { thread } | RolloutLine::ThreadUpdate { thread } => r.thread = Some(thread),
        RolloutLine::TurnStarted { turn } => r.turns.push(turn),
        RolloutLine::TurnCompleted { turn } => {
            if let Some(t) = r.turns.iter_mut().find(|t| t.id == turn.id) {
                let items = std::mem::take(&mut t.items);
                *t = turn;
                t.items = items;
            } else {
                r.turns.push(turn);
            }
        }
        RolloutLine::Item { turn_id, item } => {
            if let Some(t) = r.turns.iter_mut().find(|t| t.id == turn_id) {
                if let Some(existing) = t.items.iter_mut().find(|i| i.id() == item.id()) {
                    *existing = item;
                } else {
                    t.items.push(item);
                }
            }
        }
        RolloutLine::History { entry } => {
            r.context.entries.push(entry);
            r.context.last_exact = None;
        }
        RolloutLine::Checkpoint { state } => r.context = state,
        RolloutLine::Rollback { turn_id } => {
            if let Some(pos) = r.turns.iter().position(|t| t.id == turn_id) {
                let dropped: Vec<String> = r.turns[pos..].iter().map(|t| t.id.clone()).collect();
                r.turns.truncate(pos);
                r.context.entries.retain(|e| !dropped.contains(&e.turn_id));
                r.context.last_exact = None;
            }
        }
        RolloutLine::Plan { explanation, plan } => r.plan = (explanation, plan),
        RolloutLine::Pinned { original_task } => r.original_task = original_task,
    }
}

/// Find a rollout by thread id under the sessions dir (fallback when the index lost it).
pub fn find(sessions_dir: &Path, thread_id: &str) -> Option<PathBuf> {
    let name = format!("{thread_id}.jsonl");
    let mut stack = vec![sessions_dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().map(|n| n == name.as_str()).unwrap_or(false) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use odex_context::EntryKind;
    use odex_llm::types::ChatMessage;

    #[test]
    fn replay_roundtrip_with_rollback_and_corruption() {
        let d = tempfile::tempdir().unwrap();
        let mut w = RolloutWriter::create(d.path(), "th1").unwrap();
        let turn = |id: &str| Turn {
            id: id.into(),
            thread_id: "th1".into(),
            status: TurnStatus::InProgress,
            mode: TurnMode::Default,
            started_at: 1,
            completed_at: None,
            error: None,
            usage: TokenUsage::default(),
            items: vec![],
        };
        w.append(&RolloutLine::TurnStarted { turn: turn("t1") });
        w.append(&RolloutLine::Item {
            turn_id: "t1".into(),
            item: ThreadItem::AgentMessage { id: "i1".into(), text: "hi".into() },
        });
        w.append(&RolloutLine::History { entry: HistoryEntry::new("t1", 0, EntryKind::User, ChatMessage::user("q1")) });
        let mut done = turn("t1");
        done.status = TurnStatus::Completed;
        w.append(&RolloutLine::TurnCompleted { turn: done });
        w.append(&RolloutLine::TurnStarted { turn: turn("t2") });
        w.append(&RolloutLine::History { entry: HistoryEntry::new("t2", 1, EntryKind::User, ChatMessage::user("q2")) });
        let path = w.path().to_path_buf();
        drop(w);
        // corrupt line in the middle of an append
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"type\":\"item\",\"tu").unwrap();
        let r = read(&path).unwrap();
        assert_eq!(r.turns.len(), 2);
        assert_eq!(r.turns[0].items.len(), 1);
        assert_eq!(r.turns[1].status, TurnStatus::Interrupted);
        assert_eq!(r.context.entries.len(), 2);

        let mut w = RolloutWriter::open(&path).unwrap();
        w.append(&RolloutLine::Rollback { turn_id: "t2".into() });
        drop(w);
        let r = read(&path).unwrap();
        assert_eq!(r.turns.len(), 1);
        assert_eq!(r.context.entries.len(), 1);
        assert_eq!(find(d.path(), "th1").unwrap(), path);
    }
}
