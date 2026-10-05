//! Hooks: user, trusted-project and plugin command hooks, gated by trust review.

use std::path::Path;

use serde_json::Value;

use odex_hooks::{defs_from_toml, HookOutcome, HookRegistry};
use odex_protocol::*;

use crate::engine::Engine;
use crate::thread::ThreadRt;

pub fn registry(engine: &Engine, root: Option<&Path>) -> HookRegistry {
    let user = engine.user_settings();
    let mut defs = defs_from_toml(&user.hooks, "user", engine.home.root());
    if let Some(r) = root {
        if engine.is_trusted(r) {
            if let Ok((proj, _)) = odex_config::load_file(&odex_config::project_dir(r).join("config.toml")) {
                if let Some(h) = &proj.hooks {
                    defs.extend(defs_from_toml(h, &format!("project:{}", r.display()), r));
                }
            }
        }
    }
    for (plugin_id, dir, hooks) in crate::plugins::hooks(engine) {
        defs.extend(defs_from_toml(&hooks, &format!("plugin:{plugin_id}"), &dir));
    }
    HookRegistry::new(defs, engine.home.trusted_hooks_path())
}

/// Run hooks for an event in a thread's context. Untrusted hooks are skipped
/// and surfaced for review.
pub async fn run(
    engine: &Engine,
    rt: &ThreadRt,
    event: HookEvent,
    mut payload: Value,
    tool: Option<&str>,
) -> HookOutcome {
    let t = rt.thread();
    let root = engine.thread_root(&t);
    let reg = registry(engine, Some(&root));
    if reg.defs().is_empty() {
        return HookOutcome::default();
    }
    if let Value::Object(m) = &mut payload {
        m.insert("thread_id".into(), Value::String(t.id.clone()));
        m.insert("cwd".into(), Value::String(t.cwd.clone()));
        m.insert("permission_mode".into(), Value::String(t.permission_mode.as_str().into()));
    }
    let out = reg.run(event, payload, tool).await;
    if !out.skipped_untrusted.is_empty() {
        engine
            .emitter()
            .raw(notification::HOOKS_REVIEW_REQUIRED, &HooksReviewNotification { hooks: reg.needs_review() });
    }
    for e in &out.errors {
        tracing::warn!("hook error: {e}");
    }
    out
}
