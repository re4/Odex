//! Odex automations: schedule parsing (cron + friendly phrases) and the
//! SQLite-backed store for automations and their runs.
//!
//! The engine's scheduler loop is expected to:
//! 1. call [`AutomationStore::fail_stale_running`] once at startup,
//! 2. periodically call [`AutomationStore::due`] and, for each due automation,
//!    [`AutomationStore::mark_started`] + [`AutomationStore::create_run`],
//! 3. call [`AutomationStore::finish_run`] when the turn ends.

mod schedule;
mod store;

pub use schedule::{parse_schedule, validate, Schedule};
pub use store::{AutomationStore, StoreError, StoreResult, FINAL_RUN_STATUSES};
