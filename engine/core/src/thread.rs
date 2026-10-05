//! In-memory runtime state of a loaded thread.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use odex_context::ContextState;
use odex_llm::loopguard::LoopGuard;
use odex_protocol::*;
use odex_tools::output::OutputStore;

use crate::rollout::{RolloutLine, RolloutWriter};

pub struct RunningTurn {
    pub turn_id: String,
    pub cancel: CancellationToken,
    pub steer: mpsc::UnboundedSender<Vec<UserInput>>,
    pub mode: TurnMode,
}

pub struct PendingApproval {
    pub params: ApprovalRequestParams,
    pub reply: Option<oneshot::Sender<ApprovalDecision>>,
}

/// "Don't ask again this session" memory.
#[derive(Debug, Default, Clone)]
pub struct SessionAllow {
    pub command_prefixes: Vec<Vec<String>>,
    pub mcp_tools: HashSet<String>,
    pub apps: HashSet<String>,
    pub sites: HashSet<String>,
    pub patch_outside_workspace: bool,
}

impl SessionAllow {
    pub fn allows_command(&self, argv_list: &[Vec<String>]) -> bool {
        !argv_list.is_empty()
            && argv_list.iter().all(|argv| {
                self.command_prefixes.iter().any(|p| {
                    !p.is_empty()
                        && argv.len() >= p.len()
                        && argv[..p.len()].iter().zip(p).all(|(a, b)| a.eq_ignore_ascii_case(b))
                })
            })
    }
}

/// An auto-review denial that `/approve` can override once.
#[derive(Debug, Clone)]
pub struct Denial {
    pub signature: String,
    pub reason: String,
}

pub struct ThreadRt {
    pub id: String,
    pub meta: Mutex<Thread>,
    pub ctx: tokio::sync::Mutex<ContextState>,
    pub turns: Mutex<Vec<Turn>>,
    pub running: Mutex<Option<RunningTurn>>,
    pub queue: Mutex<Vec<Vec<UserInput>>>,
    pub pending_approvals: Mutex<HashMap<String, PendingApproval>>,
    pub session_allow: Mutex<SessionAllow>,
    rollout: Mutex<RolloutWriter>,
    pub rollout_path: PathBuf,
    pub plan: Mutex<(Option<String>, Vec<PlanStep>)>,
    pub sources: Mutex<BTreeMap<String, SourceEntry>>,
    pub followups: Mutex<Vec<String>>,
    pub outputs: OutputStore,
    pub loop_guard: Mutex<LoopGuard>,
    pub denials: Mutex<Vec<Denial>>,
    pub override_next_denial: AtomicBool,
    pub original_task: Mutex<Option<String>>,
    /// Validation failures per tool in the current turn (capped retries).
    pub arg_failures: Mutex<HashMap<String, u32>>,
    /// MCP tools activated through `search_tools` (lazy loading).
    pub active_mcp_tools: Mutex<HashSet<String>>,
    pub turn_counter: AtomicU32,
    /// Subagent ids spawned by this thread.
    pub children: Mutex<Vec<String>>,
    pub stop_hook_streak: AtomicU32,
}

impl ThreadRt {
    pub fn new(
        thread: Thread,
        rollout: RolloutWriter,
        outputs_dir: PathBuf,
        ctx: ContextState,
        turns: Vec<Turn>,
    ) -> Self {
        let rollout_path = rollout.path().to_path_buf();
        let n = turns.len() as u32;
        Self {
            id: thread.id.clone(),
            meta: Mutex::new(thread),
            ctx: tokio::sync::Mutex::new(ctx),
            turns: Mutex::new(turns),
            running: Mutex::new(None),
            queue: Mutex::new(Vec::new()),
            pending_approvals: Mutex::new(HashMap::new()),
            session_allow: Mutex::new(SessionAllow::default()),
            rollout: Mutex::new(rollout),
            rollout_path,
            plan: Mutex::new((None, vec![])),
            sources: Mutex::new(BTreeMap::new()),
            followups: Mutex::new(vec![]),
            outputs: OutputStore::new(outputs_dir),
            loop_guard: Mutex::new(LoopGuard::new()),
            denials: Mutex::new(vec![]),
            override_next_denial: AtomicBool::new(false),
            original_task: Mutex::new(None),
            arg_failures: Mutex::new(HashMap::new()),
            active_mcp_tools: Mutex::new(HashSet::new()),
            turn_counter: AtomicU32::new(n),
            children: Mutex::new(vec![]),
            stop_hook_streak: AtomicU32::new(0),
        }
    }

    pub fn thread(&self) -> Thread {
        self.meta.lock().unwrap().clone()
    }

    pub fn log(&self, line: RolloutLine) {
        self.rollout.lock().unwrap().append(&line);
    }

    pub fn rollout_path(&self) -> Option<std::path::PathBuf> {
        Some(self.rollout.lock().unwrap().path().to_path_buf())
    }

    pub fn update<F: FnOnce(&mut Thread)>(&self, f: F) -> Thread {
        let mut m = self.meta.lock().unwrap();
        f(&mut m);
        m.updated_at = chrono::Utc::now().timestamp_millis();
        let t = m.clone();
        drop(m);
        self.log(RolloutLine::ThreadUpdate { thread: t.clone() });
        t
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    pub fn next_turn_index(&self) -> u32 {
        self.turn_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Record a completed or updated item in the in-memory turn list.
    pub fn upsert_item(&self, turn_id: &str, item: &ThreadItem) {
        let mut turns = self.turns.lock().unwrap();
        if let Some(t) = turns.iter_mut().find(|t| t.id == turn_id) {
            if let Some(existing) = t.items.iter_mut().find(|i| i.id() == item.id()) {
                *existing = item.clone();
            } else {
                t.items.push(item.clone());
            }
        }
    }

    pub fn touch_source(&self, path: &str, read: bool, edited: bool) -> Vec<SourceEntry> {
        let mut s = self.sources.lock().unwrap();
        let e = s.entry(path.to_string()).or_insert(SourceEntry {
            path: path.to_string(),
            read: false,
            edited: false,
            last_touched: 0,
        });
        e.read |= read;
        e.edited |= edited;
        e.last_touched = chrono::Utc::now().timestamp_millis();
        let mut v: Vec<SourceEntry> = s.values().cloned().collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.last_touched));
        v
    }

    pub fn files_touched(&self) -> Vec<String> {
        self.sources.lock().unwrap().values().filter(|s| s.edited).map(|s| s.path.clone()).collect()
    }
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Stable nickname for subagents from their id.
pub fn nickname(seed: &str) -> String {
    const ADJ: &[&str] = &[
        "Amber", "Brisk", "Cobalt", "Dapper", "Ember", "Frosty", "Gentle", "Hazel", "Indigo", "Jolly", "Keen", "Lunar",
        "Mossy", "Nimble", "Olive", "Plucky", "Quiet", "Rusty", "Sunny", "Tidy", "Umber", "Vivid", "Witty", "Zesty",
    ];
    const NOUN: &[&str] = &[
        "Otter", "Falcon", "Badger", "Heron", "Lynx", "Marten", "Newt", "Osprey", "Puffin", "Quokka", "Raven", "Stoat",
        "Tapir", "Vole", "Wren", "Yak", "Ibex", "Gecko", "Finch", "Egret",
    ];
    let h = seed.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211));
    format!("{} {}", ADJ[(h % ADJ.len() as u64) as usize], NOUN[((h >> 16) % NOUN.len() as u64) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_allow_prefixes() {
        let mut s = SessionAllow::default();
        s.command_prefixes.push(vec!["git".into(), "push".into()]);
        assert!(s.allows_command(&[vec!["git".into(), "push".into(), "origin".into()]]));
        assert!(!s.allows_command(&[vec!["git".into(), "pull".into()]]));
        assert!(!s.allows_command(&[]));
    }

    #[test]
    fn nicknames_stable() {
        assert_eq!(nickname("abc"), nickname("abc"));
    }
}
