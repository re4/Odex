//! SQLite persistence for automations and their runs.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Local};
use odex_protocol::{Automation, AutomationRun};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::schedule::parse_schedule;

/// Errors returned by [`AutomationStore`].
#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    InvalidSchedule(String),
    Invalid(String),
    NotFound(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Sql(e) => write!(f, "database error: {e}"),
            StoreError::Json(e) => write!(f, "corrupt automation record: {e}"),
            StoreError::InvalidSchedule(e) => write!(f, "invalid schedule: {e}"),
            StoreError::Invalid(e) => write!(f, "{e}"),
            StoreError::NotFound(id) => write!(f, "not found: {id}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Sql(e) => Some(e),
            StoreError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        StoreError::Sql(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        StoreError::Json(e)
    }
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

/// Run statuses accepted by [`AutomationStore::finish_run`].
pub const FINAL_RUN_STATUSES: &[&str] = &["completed", "failed", "skipped"];

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS automations (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL DEFAULT '',
    enabled     INTEGER NOT NULL DEFAULT 1,
    next_run_at INTEGER,
    created_at  INTEGER NOT NULL DEFAULT 0,
    data        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS automations_due_idx ON automations(enabled, next_run_at);
CREATE TABLE IF NOT EXISTS automation_runs (
    id              TEXT PRIMARY KEY NOT NULL,
    automation_id   TEXT NOT NULL,
    automation_name TEXT NOT NULL DEFAULT '',
    thread_id       TEXT,
    status          TEXT NOT NULL,
    started_at      INTEGER NOT NULL,
    finished_at     INTEGER,
    summary         TEXT,
    error           TEXT,
    unread          INTEGER NOT NULL DEFAULT 0,
    archived        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS automation_runs_automation_idx ON automation_runs(automation_id, started_at);
CREATE INDEX IF NOT EXISTS automation_runs_inbox_idx ON automation_runs(archived, unread, started_at);
CREATE INDEX IF NOT EXISTS automation_runs_status_idx ON automation_runs(status);
";

const RUN_COLUMNS: &str =
    "id, automation_id, automation_name, thread_id, status, started_at, finished_at, summary, error, unread, archived";

/// Automations (stored as JSON plus indexed columns) and their run history.
#[derive(Clone)]
pub struct AutomationStore {
    conn: Arc<Mutex<Connection>>,
}

impl AutomationStore {
    /// Wrap a shared connection, creating the tables if missing. Idempotent.
    pub fn new(conn: Arc<Mutex<Connection>>) -> rusqlite::Result<Self> {
        {
            let guard = conn.lock().unwrap_or_else(|e| e.into_inner());
            guard.execute_batch(SCHEMA)?;
        }
        Ok(Self { conn })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // -- automations --------------------------------------------------------

    /// All automations in creation order.
    pub fn list(&self) -> Vec<Automation> {
        let conn = self.lock();
        query_automations(&conn, "SELECT data FROM automations ORDER BY created_at, rowid", [])
    }

    pub fn get(&self, id: &str) -> Option<Automation> {
        let conn = self.lock();
        get_automation(&conn, id).ok().flatten()
    }

    /// Insert or update. Assigns `id`/`created_at` when empty, keeps the
    /// server-owned fields (`created_at`, `last_run_at`) of an existing record
    /// and computes `next_run_at`.
    pub fn upsert(&self, a: Automation) -> StoreResult<Automation> {
        self.upsert_at(a, Local::now())
    }

    /// [`upsert`](Self::upsert) with an explicit clock.
    pub fn upsert_at(&self, mut a: Automation, now: DateTime<Local>) -> StoreResult<Automation> {
        let schedule = parse_schedule(&a.schedule).map_err(StoreError::InvalidSchedule)?;
        a.schedule = a.schedule.trim().to_string();
        a.name = a.name.trim().to_string();
        if a.name.is_empty() {
            a.name = "Untitled automation".to_string();
        }
        match a.target.as_str() {
            "project" => {}
            "thread" if a.thread_id.as_deref().is_some_and(|t| !t.is_empty()) => {}
            "thread" => return Err(StoreError::Invalid("a `thread` automation needs a thread_id".into())),
            other => return Err(StoreError::Invalid(format!("unknown target `{other}` (expected project or thread)"))),
        }

        let conn = self.lock();
        let existing = if a.id.is_empty() { None } else { get_automation(&conn, &a.id)? };
        if a.id.is_empty() {
            a.id = uuid::Uuid::now_v7().to_string();
        }
        let computed = || if a.enabled { schedule.next_after(now).map(|t| t.timestamp_millis()) } else { None };
        let next = match &existing {
            Some(old) => {
                let keep = old.schedule == a.schedule && old.enabled && a.enabled && old.next_run_at.is_some();
                if keep {
                    old.next_run_at
                } else {
                    computed()
                }
            }
            None => computed(),
        };
        match existing {
            Some(old) => {
                a.created_at = old.created_at;
                a.last_run_at = old.last_run_at;
            }
            None if a.created_at <= 0 => a.created_at = now.timestamp_millis(),
            None => {}
        }
        a.next_run_at = next;
        save_automation(&conn, &a)?;
        Ok(a)
    }

    /// Delete an automation. Its run history is kept (runs carry the name).
    pub fn delete(&self, id: &str) -> StoreResult<()> {
        let conn = self.lock();
        let n = conn.execute("DELETE FROM automations WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// Enabled automations whose `next_run_at` is at or before `now`, oldest first.
    pub fn due(&self, now: DateTime<Local>) -> Vec<Automation> {
        let conn = self.lock();
        query_automations(
            &conn,
            "SELECT data FROM automations WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 \
             ORDER BY next_run_at, rowid",
            [now.timestamp_millis()],
        )
    }

    /// Record that a run started: sets `last_run_at` and moves `next_run_at`
    /// to the first occurrence after `now` (missed runs are skipped, not replayed).
    pub fn mark_started(&self, id: &str, now: DateTime<Local>) -> StoreResult<()> {
        let conn = self.lock();
        let mut a = get_automation(&conn, id)?.ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        a.last_run_at = Some(now.timestamp_millis());
        a.next_run_at = if a.enabled {
            parse_schedule(&a.schedule).ok().and_then(|s| s.next_after(now)).map(|t| t.timestamp_millis())
        } else {
            None
        };
        save_automation(&conn, &a)
    }

    // -- runs ---------------------------------------------------------------

    /// Start a run record (`status = running`, read, not archived).
    pub fn create_run(&self, a: &Automation, thread_id: Option<String>) -> StoreResult<AutomationRun> {
        let run = AutomationRun {
            id: uuid::Uuid::now_v7().to_string(),
            automation_id: a.id.clone(),
            automation_name: a.name.clone(),
            thread_id,
            status: "running".to_string(),
            started_at: Local::now().timestamp_millis(),
            finished_at: None,
            summary: None,
            error: None,
            unread: false,
            archived: false,
        };
        let conn = self.lock();
        conn.execute(
            &format!(
                "INSERT INTO automation_runs ({RUN_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
            ),
            params![
                run.id,
                run.automation_id,
                run.automation_name,
                run.thread_id,
                run.status,
                run.started_at,
                run.finished_at,
                run.summary,
                run.error,
                run.unread,
                run.archived
            ],
        )?;
        Ok(run)
    }

    /// Finish a run with `completed`, `failed` or `skipped`; the run becomes unread.
    pub fn finish_run(
        &self,
        run_id: &str,
        status: &str,
        summary: Option<String>,
        error: Option<String>,
    ) -> StoreResult<AutomationRun> {
        if !FINAL_RUN_STATUSES.contains(&status) {
            return Err(StoreError::Invalid(format!(
                "invalid run status `{status}` (expected one of {})",
                FINAL_RUN_STATUSES.join(", ")
            )));
        }
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE automation_runs SET status = ?2, summary = ?3, error = ?4, finished_at = ?5, unread = 1 \
             WHERE id = ?1",
            params![run_id, status, summary, error, Local::now().timestamp_millis()],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound(run_id.to_string()));
        }
        get_run(&conn, run_id)?.ok_or_else(|| StoreError::NotFound(run_id.to_string()))
    }

    pub fn get_run(&self, run_id: &str) -> Option<AutomationRun> {
        let conn = self.lock();
        get_run(&conn, run_id).ok().flatten()
    }

    /// Runs, newest first. `limit == 0` means no limit.
    pub fn runs(
        &self,
        automation_id: Option<&str>,
        unread_only: bool,
        include_archived: bool,
        limit: usize,
    ) -> Vec<AutomationRun> {
        let mut sql = format!("SELECT {RUN_COLUMNS} FROM automation_runs WHERE 1 = 1");
        let mut args: Vec<String> = Vec::new();
        if let Some(id) = automation_id {
            args.push(id.to_string());
            sql.push_str(&format!(" AND automation_id = ?{}", args.len()));
        }
        if unread_only {
            sql.push_str(" AND unread = 1");
        }
        if !include_archived {
            sql.push_str(" AND archived = 0");
        }
        sql.push_str(" ORDER BY started_at DESC, rowid DESC");
        if limit > 0 {
            sql.push_str(&format!(" LIMIT {limit}"));
        }
        let conn = self.lock();
        query_runs(&conn, &sql, params_from_iter(args.iter()))
    }

    /// Unread, unarchived runs.
    pub fn unread_count(&self) -> u32 {
        let conn = self.lock();
        conn.query_row("SELECT COUNT(*) FROM automation_runs WHERE unread = 1 AND archived = 0", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n.max(0) as u32)
        .unwrap_or(0)
    }

    pub fn mark_read(&self, ids: &[String]) -> StoreResult<()> {
        self.update_runs("UPDATE automation_runs SET unread = 0 WHERE id = ?1", ids)
    }

    /// Archive runs (archiving also marks them read).
    pub fn archive_runs(&self, ids: &[String]) -> StoreResult<()> {
        self.update_runs("UPDATE automation_runs SET archived = 1, unread = 0 WHERE id = ?1", ids)
    }

    fn update_runs(&self, sql: &str, ids: &[String]) -> StoreResult<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(sql)?;
            for id in ids {
                stmt.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Runs still marked `running`, oldest first.
    pub fn running_runs(&self) -> Vec<AutomationRun> {
        let conn = self.lock();
        query_runs(
            &conn,
            &format!("SELECT {RUN_COLUMNS} FROM automation_runs WHERE status = 'running' ORDER BY started_at, rowid"),
            [],
        )
    }

    /// Mark every `running` run as failed (call on engine start: those runs
    /// died with the previous process). Returns how many were changed.
    pub fn fail_stale_running(&self, reason: &str) -> StoreResult<usize> {
        let conn = self.lock();
        Ok(conn.execute(
            "UPDATE automation_runs SET status = 'failed', error = ?1, finished_at = ?2, unread = 1 \
             WHERE status = 'running'",
            params![reason, Local::now().timestamp_millis()],
        )?)
    }
}

fn get_automation(conn: &Connection, id: &str) -> StoreResult<Option<Automation>> {
    let data: Option<String> =
        conn.query_row("SELECT data FROM automations WHERE id = ?1", [id], |r| r.get(0)).optional()?;
    match data {
        Some(d) => Ok(Some(serde_json::from_str(&d)?)),
        None => Ok(None),
    }
}

fn save_automation(conn: &Connection, a: &Automation) -> StoreResult<()> {
    let data = serde_json::to_string(a)?;
    conn.execute(
        "INSERT INTO automations (id, name, enabled, next_run_at, created_at, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, enabled = excluded.enabled, \
         next_run_at = excluded.next_run_at, created_at = excluded.created_at, data = excluded.data",
        params![a.id, a.name, a.enabled, a.next_run_at, a.created_at, data],
    )?;
    Ok(())
}

fn query_automations<P: rusqlite::Params>(conn: &Connection, sql: &str, p: P) -> Vec<Automation> {
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(p, |r| r.get::<_, String>(0)) else {
        return Vec::new();
    };
    // Rows that fail to decode (e.g. written by a newer version) are skipped.
    rows.filter_map(|r| r.ok()).filter_map(|d| serde_json::from_str(&d).ok()).collect()
}

fn run_from_row(r: &Row<'_>) -> rusqlite::Result<AutomationRun> {
    Ok(AutomationRun {
        id: r.get(0)?,
        automation_id: r.get(1)?,
        automation_name: r.get(2)?,
        thread_id: r.get(3)?,
        status: r.get(4)?,
        started_at: r.get(5)?,
        finished_at: r.get(6)?,
        summary: r.get(7)?,
        error: r.get(8)?,
        unread: r.get(9)?,
        archived: r.get(10)?,
    })
}

fn get_run(conn: &Connection, id: &str) -> StoreResult<Option<AutomationRun>> {
    Ok(conn
        .query_row(&format!("SELECT {RUN_COLUMNS} FROM automation_runs WHERE id = ?1"), [id], run_from_row)
        .optional()?)
}

fn query_runs<P: rusqlite::Params>(conn: &Connection, sql: &str, p: P) -> Vec<AutomationRun> {
    let Ok(mut stmt) = conn.prepare(sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(p, run_from_row) else {
        return Vec::new();
    };
    rows.filter_map(|r| r.ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use odex_protocol::{PermissionMode, RunMode};

    fn store() -> AutomationStore {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        AutomationStore::new(conn).unwrap()
    }

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().unwrap()
    }

    fn automation(name: &str, schedule: &str) -> Automation {
        Automation {
            id: String::new(),
            name: name.to_string(),
            schedule: schedule.to_string(),
            target: "project".to_string(),
            project_id: Some("p1".into()),
            thread_id: None,
            cwd: Some("C:/work/app".into()),
            prompt: "Summarize new issues".into(),
            model: None,
            effort: None,
            permission_mode: PermissionMode::ReadOnly,
            run_mode: RunMode::Local,
            enabled: true,
            created_at: 0,
            last_run_at: None,
            next_run_at: None,
        }
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let a = AutomationStore::new(conn.clone()).unwrap();
        a.upsert(automation("x", "@daily")).unwrap();
        let b = AutomationStore::new(conn).unwrap();
        assert_eq!(b.list().len(), 1);
    }

    #[test]
    fn crud_round_trip() {
        let s = store();
        let now = local(2026, 3, 10, 8, 7);
        let a = s.upsert_at(automation("Triage", "every 15m"), now).unwrap();
        assert!(!a.id.is_empty());
        assert_eq!(a.created_at, now.timestamp_millis());
        assert_eq!(a.next_run_at, Some(local(2026, 3, 10, 8, 15).timestamp_millis()));
        assert_eq!(s.get(&a.id), Some(a.clone()));

        let b = s.upsert_at(automation("Nightly", "daily 02:00"), now).unwrap();
        assert_eq!(s.list().iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["Triage", "Nightly"]);

        // Update: server-owned fields survive, schedule change recomputes next.
        let mut edit = a.clone();
        edit.name = "Triage v2".into();
        edit.created_at = 1;
        edit.last_run_at = Some(42);
        edit.schedule = "hourly".into();
        let later = local(2026, 3, 10, 9, 30);
        let edited = s.upsert_at(edit, later).unwrap();
        assert_eq!(edited.created_at, a.created_at);
        assert_eq!(edited.last_run_at, None);
        assert_eq!(edited.next_run_at, Some(local(2026, 3, 10, 10, 0).timestamp_millis()));

        // Same schedule -> pending next_run_at is kept even if it is in the past.
        let mut rename = edited.clone();
        rename.name = "Triage v3".into();
        let kept = s.upsert_at(rename, local(2026, 3, 10, 11, 0)).unwrap();
        assert_eq!(kept.next_run_at, edited.next_run_at);

        // Disable clears next_run_at.
        let mut off = kept.clone();
        off.enabled = false;
        assert_eq!(s.upsert_at(off, later).unwrap().next_run_at, None);

        s.delete(&b.id).unwrap();
        assert!(s.get(&b.id).is_none());
        assert!(matches!(s.delete(&b.id), Err(StoreError::NotFound(_))));
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn upsert_validates() {
        let s = store();
        assert!(matches!(s.upsert(automation("bad", "every blue moon")), Err(StoreError::InvalidSchedule(_))));
        let mut t = automation("thread", "@hourly");
        t.target = "thread".into();
        assert!(matches!(s.upsert(t.clone()), Err(StoreError::Invalid(_))));
        t.thread_id = Some("th_1".into());
        assert!(s.upsert(t).is_ok());
        let mut u = automation("", "@hourly");
        u.target = "nowhere".into();
        assert!(s.upsert(u).is_err());
        let named = s.upsert(automation("  ", "@hourly")).unwrap();
        assert_eq!(named.name, "Untitled automation");
    }

    #[test]
    fn due_and_mark_started_skip_missed_runs() {
        let s = store();
        let t0 = local(2026, 3, 10, 8, 0);
        let a = s.upsert_at(automation("Every 15", "*/15 * * * *"), t0).unwrap();
        let mut off = automation("Off", "*/15 * * * *");
        off.enabled = false;
        s.upsert_at(off, t0).unwrap();

        assert!(s.due(local(2026, 3, 10, 8, 14)).is_empty());
        let due = s.due(local(2026, 3, 10, 8, 15));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, a.id);

        // Engine was asleep for hours: one run, next is after "now".
        let late = local(2026, 3, 10, 13, 2);
        assert_eq!(s.due(late).len(), 1);
        s.mark_started(&a.id, late).unwrap();
        let after = s.get(&a.id).unwrap();
        assert_eq!(after.last_run_at, Some(late.timestamp_millis()));
        assert_eq!(after.next_run_at, Some(local(2026, 3, 10, 13, 15).timestamp_millis()));
        assert!(s.due(late).is_empty());
        assert!(matches!(s.mark_started("nope", late), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn runs_lifecycle_filters_and_counts() {
        let s = store();
        let a = s.upsert(automation("A", "@hourly")).unwrap();
        let b = s.upsert(automation("B", "@daily")).unwrap();

        let r1 = s.create_run(&a, Some("th1".into())).unwrap();
        let r2 = s.create_run(&a, None).unwrap();
        let r3 = s.create_run(&b, None).unwrap();
        assert_eq!(r1.status, "running");
        assert!(!r1.unread);
        assert_eq!(s.running_runs().len(), 3);
        assert_eq!(s.unread_count(), 0);

        let done = s.finish_run(&r1.id, "completed", Some("3 new issues".into()), None).unwrap();
        assert_eq!(done.status, "completed");
        assert!(done.unread);
        assert!(done.finished_at.is_some());
        assert_eq!(done.summary.as_deref(), Some("3 new issues"));
        assert_eq!(done.thread_id.as_deref(), Some("th1"));
        s.finish_run(&r3.id, "failed", None, Some("model offline".into())).unwrap();
        assert!(matches!(s.finish_run(&r2.id, "bogus", None, None), Err(StoreError::Invalid(_))));
        assert!(matches!(s.finish_run("missing", "completed", None, None), Err(StoreError::NotFound(_))));

        assert_eq!(s.unread_count(), 2);
        // Newest first.
        let all = s.runs(None, false, false, 0);
        assert_eq!(
            all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            [r3.id.as_str(), r2.id.as_str(), r1.id.as_str()]
        );
        assert_eq!(s.runs(Some(&a.id), false, false, 0).len(), 2);
        assert_eq!(s.runs(None, true, false, 0).len(), 2);
        assert_eq!(s.runs(None, false, false, 1).len(), 1);
        assert_eq!(s.runs(Some(&b.id), true, false, 10)[0].error.as_deref(), Some("model offline"));

        s.mark_read(std::slice::from_ref(&r1.id)).unwrap();
        assert_eq!(s.unread_count(), 1);
        assert!(!s.get_run(&r1.id).unwrap().unread);

        s.archive_runs(std::slice::from_ref(&r3.id)).unwrap();
        assert_eq!(s.unread_count(), 0);
        assert_eq!(s.runs(None, false, false, 0).len(), 2);
        let with_archived = s.runs(None, false, true, 0);
        assert_eq!(with_archived.len(), 3);
        assert!(with_archived.iter().any(|r| r.id == r3.id && r.archived));

        // Deleting the automation keeps its history.
        s.delete(&a.id).unwrap();
        assert_eq!(s.runs(Some(&a.id), false, true, 0).len(), 2);
    }

    #[test]
    fn stale_running_runs_fail_on_restart() {
        let s = store();
        let a = s.upsert(automation("A", "@hourly")).unwrap();
        let r1 = s.create_run(&a, None).unwrap();
        let r2 = s.create_run(&a, None).unwrap();
        s.finish_run(&r2.id, "completed", None, None).unwrap();
        assert_eq!(s.fail_stale_running("engine restarted").unwrap(), 1);
        let r1 = s.get_run(&r1.id).unwrap();
        assert_eq!(r1.status, "failed");
        assert_eq!(r1.error.as_deref(), Some("engine restarted"));
        assert!(r1.unread);
        assert!(s.running_runs().is_empty());
        assert_eq!(s.fail_stale_running("again").unwrap(), 0);
    }
}
