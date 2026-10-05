//! SQLite index (`~/.odex/odex.sqlite`): threads, full-text search, recall,
//! projects, usage stats. Rollouts (JSONL) remain the source of truth for
//! conversation content; this index can be rebuilt from them.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

use odex_protocol::*;

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
CREATE TABLE IF NOT EXISTS threads (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    json TEXT NOT NULL,
    title TEXT,
    preview TEXT,
    cwd TEXT,
    branch TEXT,
    kind TEXT,
    archived INTEGER NOT NULL DEFAULT 0,
    pinned INTEGER NOT NULL DEFAULT 0,
    unread INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    rollout TEXT
);
CREATE INDEX IF NOT EXISTS threads_updated ON threads(updated_at DESC);
CREATE VIRTUAL TABLE IF NOT EXISTS thread_fts USING fts5(thread_id UNINDEXED, field UNINDEXED, text, tokenize = 'unicode61');
CREATE VIRTUAL TABLE IF NOT EXISTS recall_fts USING fts5(thread_id UNINDEXED, entry_id UNINDEXED, turn_index UNINDEXED, label UNINDEXED, text, tokenize = 'unicode61');
CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, json TEXT NOT NULL, position INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS usage (
    day TEXT NOT NULL, model TEXT NOT NULL, requests INTEGER NOT NULL DEFAULT 0,
    input INTEGER NOT NULL DEFAULT 0, cached INTEGER NOT NULL DEFAULT 0, output INTEGER NOT NULL DEFAULT 0,
    reasoning INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, model)
);
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
"#;

#[derive(Debug, Clone)]
pub struct RecallHit {
    pub entry_id: String,
    pub turn_index: u32,
    pub label: String,
    pub snippet: String,
    pub text: String,
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> anyhow::Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    pub fn conn(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }

    // ------------------------------------------------------------ threads

    pub fn upsert_thread(&self, t: &Thread, rollout: Option<&str>) -> anyhow::Result<()> {
        let json = serde_json::to_string(t)?;
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO threads (id, project_id, json, title, preview, cwd, branch, kind, archived, pinned, unread, created_at, updated_at, rollout)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET project_id=excluded.project_id, json=excluded.json, title=excluded.title,
               preview=excluded.preview, cwd=excluded.cwd, branch=excluded.branch, kind=excluded.kind,
               archived=excluded.archived, pinned=excluded.pinned, unread=excluded.unread, updated_at=excluded.updated_at,
               rollout=COALESCE(excluded.rollout, threads.rollout)",
            params![
                t.id,
                t.project_id,
                json,
                t.name,
                t.preview,
                t.cwd,
                t.branch,
                serde_json::to_value(t.kind)?.as_str().unwrap_or("normal"),
                t.archived as i32,
                t.pinned as i32,
                t.unread as i32,
                t.created_at,
                t.updated_at,
                rollout
            ],
        )?;
        // title / branch search rows are replaced on every upsert
        c.execute("DELETE FROM thread_fts WHERE thread_id = ?1 AND field IN ('title','branch')", params![t.id])?;
        if let Some(n) = &t.name {
            c.execute("INSERT INTO thread_fts (thread_id, field, text) VALUES (?1, 'title', ?2)", params![t.id, n])?;
        }
        if let Some(b) = &t.branch {
            c.execute("INSERT INTO thread_fts (thread_id, field, text) VALUES (?1, 'branch', ?2)", params![t.id, b])?;
        }
        Ok(())
    }

    pub fn get_thread(&self, id: &str) -> anyhow::Result<Option<(Thread, Option<String>)>> {
        let c = self.conn.lock().unwrap();
        let row: Option<(String, Option<String>)> = c
            .query_row("SELECT json, rollout FROM threads WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        Ok(match row {
            Some((j, r)) => Some((serde_json::from_str(&j)?, r)),
            None => None,
        })
    }

    pub fn list_threads(&self, p: &ThreadListParams) -> anyhow::Result<Vec<Thread>> {
        let c = self.conn.lock().unwrap();
        let archived = p.archived.unwrap_or(false) as i32;
        let limit = p.limit.unwrap_or(500).min(5000) as i64;
        let offset: i64 = p.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
        let mut stmt = c.prepare(
            "SELECT json FROM threads WHERE archived = ?1 AND (?2 IS NULL OR project_id = ?2)
             ORDER BY pinned DESC, updated_at DESC LIMIT ?3 OFFSET ?4",
        )?;
        let rows = stmt.query_map(params![archived, p.project_id, limit, offset], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for j in rows.flatten() {
            if let Ok(t) = serde_json::from_str::<Thread>(&j) {
                if let Some(kinds) = &p.kinds {
                    if !kinds.contains(&t.kind) {
                        continue;
                    }
                } else if matches!(t.kind, ThreadKind::Subagent | ThreadKind::Side) || t.ephemeral {
                    continue;
                }
                out.push(t);
            }
        }
        Ok(out)
    }

    pub fn delete_thread(&self, id: &str) -> anyhow::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute("DELETE FROM threads WHERE id = ?1", params![id])?;
        c.execute("DELETE FROM thread_fts WHERE thread_id = ?1", params![id])?;
        c.execute("DELETE FROM recall_fts WHERE thread_id = ?1", params![id])?;
        Ok(())
    }

    pub fn index_content(&self, thread_id: &str, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let c = self.conn.lock().unwrap();
        let _ = c.execute(
            "INSERT INTO thread_fts (thread_id, field, text) VALUES (?1, 'content', ?2)",
            params![thread_id, text],
        );
    }

    pub fn search_threads(&self, query: &str, limit: u32) -> anyhow::Result<Vec<SearchHit>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(vec![]);
        }
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT f.thread_id, f.field, snippet(thread_fts, 2, '[', ']', '…', 12), t.title, t.updated_at
             FROM thread_fts f JOIN threads t ON t.id = f.thread_id
             WHERE thread_fts MATCH ?1 AND t.archived = 0
             ORDER BY bm25(thread_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, (limit * 4) as i64], |r| {
            Ok(SearchHit {
                thread_id: r.get(0)?,
                field: r.get(1)?,
                snippet: r.get(2)?,
                title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                updated_at: r.get(4)?,
            })
        })?;
        let mut out: Vec<SearchHit> = Vec::new();
        for h in rows.flatten() {
            if out.iter().any(|x| x.thread_id == h.thread_id) {
                continue; // best hit per thread
            }
            out.push(h);
            if out.len() >= limit as usize {
                break;
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------- recall

    pub fn index_recall(&self, thread_id: &str, entry_id: &str, turn_index: u32, label: &str, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let c = self.conn.lock().unwrap();
        let _ = c.execute(
            "INSERT INTO recall_fts (thread_id, entry_id, turn_index, label, text) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![thread_id, entry_id, turn_index as i64, label, text],
        );
    }

    /// BM25 search over a thread's full history.
    pub fn recall(&self, thread_id: &str, query: &str, limit: usize) -> anyhow::Result<Vec<RecallHit>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(vec![]);
        }
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT entry_id, turn_index, label, snippet(recall_fts, 4, '»', '«', '…', 40), text FROM recall_fts
             WHERE recall_fts MATCH ?1 AND thread_id = ?2 ORDER BY bm25(recall_fts) LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![q, thread_id, limit as i64], |r| {
            Ok(RecallHit {
                entry_id: r.get(0)?,
                turn_index: r.get::<_, i64>(1)? as u32,
                label: r.get(2)?,
                snippet: r.get(3)?,
                text: r.get(4)?,
            })
        })?;
        Ok(rows.flatten().collect())
    }

    pub fn copy_recall(&self, from: &str, to: &str) {
        let c = self.conn.lock().unwrap();
        let _ = c.execute(
            "INSERT INTO recall_fts (thread_id, entry_id, turn_index, label, text) SELECT ?2, entry_id, turn_index, label, text FROM recall_fts WHERE thread_id = ?1",
            params![from, to],
        );
    }

    // ----------------------------------------------------------- projects

    pub fn upsert_project(&self, p: &Project) -> anyhow::Result<()> {
        let c = self.conn.lock().unwrap();
        c.execute(
            "INSERT INTO projects (id, json, position) VALUES (?1, ?2, (SELECT COALESCE(MAX(position),0)+1 FROM projects))
             ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![p.id, serde_json::to_string(p)?],
        )?;
        Ok(())
    }

    pub fn projects(&self) -> anyhow::Result<Vec<Project>> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare("SELECT json FROM projects ORDER BY position")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.flatten().filter_map(|j| serde_json::from_str(&j).ok()).collect())
    }

    pub fn delete_project(&self, id: &str) -> anyhow::Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(())
    }

    // -------------------------------------------------------------- usage

    pub fn record_usage(&self, model: &str, u: &TokenUsage) {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let c = self.conn.lock().unwrap();
        let _ = c.execute(
            "INSERT INTO usage (day, model, requests, input, cached, output, reasoning) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6)
             ON CONFLICT(day, model) DO UPDATE SET requests = requests + 1, input = input + ?3, cached = cached + ?4,
               output = output + ?5, reasoning = reasoning + ?6",
            params![day, model, u.input_tokens as i64, u.cached_input_tokens as i64, u.output_tokens as i64, u.reasoning_tokens as i64],
        );
    }

    pub fn usage(&self, since: Option<&str>) -> anyhow::Result<UsageStats> {
        let c = self.conn.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT day, model, requests, input, cached, output, reasoning FROM usage WHERE (?1 IS NULL OR day >= ?1) ORDER BY day DESC, model",
        )?;
        let rows = stmt.query_map(params![since], |r| {
            let input = r.get::<_, i64>(3)? as u64;
            let output = r.get::<_, i64>(5)? as u64;
            Ok(UsageRow {
                date: r.get(0)?,
                model: r.get(1)?,
                requests: r.get::<_, i64>(2)? as u32,
                usage: TokenUsage {
                    input_tokens: input,
                    cached_input_tokens: r.get::<_, i64>(4)? as u64,
                    output_tokens: output,
                    reasoning_tokens: r.get::<_, i64>(6)? as u64,
                    total_tokens: input + output,
                },
            })
        })?;
        let rows: Vec<UsageRow> = rows.flatten().collect();
        let mut totals = TokenUsage::default();
        let mut by_model = std::collections::BTreeMap::new();
        for r in &rows {
            totals.add(&r.usage);
            by_model.entry(r.model.clone()).or_insert_with(TokenUsage::default).add(&r.usage);
        }
        Ok(UsageStats { rows, totals, by_model })
    }

    // ----------------------------------------------------------------- kv

    pub fn kv_get(&self, key: &str) -> Option<String> {
        let c = self.conn.lock().unwrap();
        c.query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn kv_set(&self, key: &str, value: &str) {
        let c = self.conn.lock().unwrap();
        let _ = c.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }
}

/// Turn free text into a safe FTS5 query: quoted terms OR'ed, prefix match on the last.
pub fn fts_query(q: &str) -> String {
    let terms: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.' && c != '/')
        .filter(|t| !t.is_empty())
        .map(|t| t.replace('"', ""))
        .filter(|t| !t.is_empty())
        .take(12)
        .collect();
    if terms.is_empty() {
        return String::new();
    }
    let n = terms.len();
    terms
        .into_iter()
        .enumerate()
        .map(|(i, t)| if i == n - 1 { format!("\"{t}\"*") } else { format!("\"{t}\"") })
        .collect::<Vec<_>>()
        .join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str, name: &str) -> Thread {
        Thread {
            id: id.into(),
            name: Some(name.into()),
            kind: ThreadKind::Normal,
            project_id: None,
            cwd: "/x".into(),
            run_mode: RunMode::Local,
            worktree: None,
            branch: Some("odex/feature-login".into()),
            model: None,
            effort: None,
            permission_mode: PermissionMode::Auto,
            created_at: 1,
            updated_at: 2,
            archived: false,
            pinned: false,
            unread: false,
            status: ThreadStatus::Idle,
            preview: String::new(),
            parent_thread_id: None,
            goal: None,
            memories_enabled: false,
            ephemeral: false,
            diff_stats: None,
            usage: TokenUsage::default(),
            last_error: None,
        }
    }

    #[test]
    fn threads_search_and_recall() {
        let s = Store::in_memory().unwrap();
        s.upsert_thread(&thread("a", "Fix the parser"), Some("/r/a.jsonl")).unwrap();
        s.upsert_thread(&thread("b", "Write docs"), None).unwrap();
        s.index_content("b", "we discussed the tokenizer edge cases");
        assert_eq!(s.list_threads(&ThreadListParams::default()).unwrap().len(), 2);
        let hits = s.search_threads("parser", 10).unwrap();
        assert_eq!(hits[0].thread_id, "a");
        assert_eq!(hits[0].field, "title");
        let hits = s.search_threads("tokeniz", 10).unwrap();
        assert_eq!(hits[0].thread_id, "b");
        let hits = s.search_threads("feature-login", 10).unwrap();
        assert!(hits.iter().any(|h| h.field == "branch"));

        s.index_recall("a", "e1", 0, "tool shell", "the secret code is 7319 and the port is 8080");
        s.index_recall("a", "e2", 1, "user", "unrelated text");
        let r = s.recall("a", "secret code", 5).unwrap();
        assert_eq!(r[0].entry_id, "e1");
        assert!(r[0].snippet.contains("secret"));
        assert!(s.recall("b", "secret", 5).unwrap().is_empty());
    }

    #[test]
    fn usage_rollup() {
        let s = Store::in_memory().unwrap();
        let u = TokenUsage { input_tokens: 100, output_tokens: 10, total_tokens: 110, ..Default::default() };
        s.record_usage("m1", &u);
        s.record_usage("m1", &u);
        s.record_usage("m2", &u);
        let st = s.usage(None).unwrap();
        assert_eq!(st.totals.input_tokens, 300);
        assert_eq!(st.by_model["m1"].output_tokens, 20);
        assert_eq!(st.rows.iter().find(|r| r.model == "m1").unwrap().requests, 2);
    }

    #[test]
    fn fts_query_escapes() {
        assert_eq!(fts_query("hello \"world\""), "\"hello\" OR \"world\"*");
        assert_eq!(fts_query("  "), "");
    }
}
