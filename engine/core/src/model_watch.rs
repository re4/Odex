//! Model downgrade detection: warn when the endpoint stops serving a thread's
//! model, the default model changes under it, or the served context window
//! shrinks below what the thread was using.

use std::sync::Arc;

use odex_llm::ModelHandle;
use odex_protocol::*;

use crate::engine::Engine;
use crate::thread::{now_ms, ThreadRt};

/// Lowercased family token of a served model id (`Qwen/Qwen3-32B-AWQ` → `qwen3`).
fn family(id: &str) -> String {
    let base = id.rsplit('/').next().unwrap_or(id).to_lowercase();
    base.split(['-', '_', '.', ':']).find(|s| !s.is_empty()).unwrap_or(&base).to_string()
}

/// What an endpoint serves instead of `wanted`: a same-family chat model if any.
fn alternative(wanted: &str, served: &[DiscoveredModel]) -> Option<(String, bool)> {
    let chat: Vec<&DiscoveredModel> = served.iter().filter(|m| !m.id.to_lowercase().contains("embed")).collect();
    let fam = family(wanted);
    if let Some(m) = chat.iter().find(|m| family(&m.id) == fam) {
        return Some((m.id.clone(), true));
    }
    chat.first().map(|m| (m.id.clone(), false))
}

fn provider_name(engine: &Engine, id: &str) -> String {
    engine.registry.client(id).map(|c| c.provider.name.clone()).unwrap_or_else(|| id.to_string())
}

fn fmt_tokens(n: u32) -> String {
    if n >= 1024 && n.is_multiple_of(1024) {
        format!("{}K", n / 1024)
    } else if n >= 1000 {
        format!("{:.1}K", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Current warning for a thread (no side effects).
pub fn compute(engine: &Engine, t: &Thread, handle: Option<&ModelHandle>) -> Option<ModelWarning> {
    let reg = &engine.registry;
    let warn = |code: &str, message: String, served: Option<String>, prev: Option<u32>, now: Option<u32>| {
        Some(ModelWarning {
            code: code.into(),
            message,
            served_model: served,
            previous_window: prev,
            window: now,
            at: now_ms(),
        })
    };
    // nothing is known before the first endpoint probe
    if reg.providers().iter().all(|p| p.health == EndpointHealth::Unknown) {
        return None;
    }
    let h = match handle {
        Some(h) => h.clone(),
        None => engine.main_model(t).ok()?,
    };
    // The thread's model resolves nowhere, so the default model stands in.
    if let Some(k) = t.model.as_deref() {
        if reg.resolve(k).is_none() {
            return warn(
                "notServed",
                format!("{k} is not served by any endpoint; this thread falls back to {}.", h.model.model_id),
                Some(h.model.model_id.clone()),
                t.last_model.as_ref().map(|l| l.context_window),
                Some(h.context_window),
            );
        }
    }
    // The model is not listed by its (reachable) endpoint. Without a listing
    // (unreachable endpoint) the window is a guess, so compare nothing.
    let served = reg.served_models(&h.model.provider_id)?;
    if !served.iter().any(|m| m.id == h.model.model_id) {
        let alt = alternative(&h.model.model_id, &served);
        let tail = match &alt {
            Some((id, true)) => format!("; it now serves {id} (same family)"),
            Some((id, false)) => format!("; it now serves {id}"),
            None => String::new(),
        };
        return warn(
            "notServed",
            format!(
                "{} is not served by {} anymore{tail}. Switch the thread's model to keep working.",
                h.model.model_id,
                provider_name(engine, &h.model.provider_id)
            ),
            alt.map(|a| a.0),
            t.last_model.as_ref().map(|l| l.context_window),
            None,
        );
    }
    let last = t.last_model.as_ref()?;
    if t.model.is_none() && last.key != h.model.key && last.model_id != h.model.model_id {
        let smaller = h.context_window < last.context_window;
        return warn(
            "modelChanged",
            format!(
                "This thread ran on {} ({} context); the default model is now {} ({} context){}.",
                last.model_id,
                fmt_tokens(last.context_window),
                h.model.model_id,
                fmt_tokens(h.context_window),
                if smaller { ", so older context may be compacted" } else { "" }
            ),
            Some(h.model.model_id.clone()),
            Some(last.context_window),
            Some(h.context_window),
        );
    }
    if last.model_id == h.model.model_id && h.context_window < last.context_window {
        return warn(
            "windowShrank",
            format!(
                "The context window of {} shrank from {} to {} tokens (the endpoint's max_model_len changed); older context will be compacted to fit.",
                h.model.model_id,
                fmt_tokens(last.context_window),
                fmt_tokens(h.context_window)
            ),
            None,
            Some(last.context_window),
            Some(h.context_window),
        );
    }
    None
}

fn same(a: &Option<ModelWarning>, b: &Option<ModelWarning>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.code == b.code && a.message == b.message,
        _ => false,
    }
}

/// Re-evaluate one thread's warning (after a registry refresh or when opened).
pub fn check(engine: &Engine, rt: &ThreadRt) {
    let t = rt.thread();
    if t.archived {
        return;
    }
    let w = compute(engine, &t, None);
    if !same(&w, &t.model_warning) {
        engine.update_thread(rt, |t| t.model_warning = w);
    }
}

/// Re-evaluate every loaded thread (call after `registry.refresh()`).
pub fn check_all(engine: &Engine) {
    let threads: Vec<Arc<ThreadRt>> = engine.threads.lock().unwrap().values().cloned().collect();
    for rt in threads {
        check(engine, &rt);
    }
}

/// At turn start: warn in the transcript when the model changed under the
/// thread, then remember what this turn runs on.
pub fn on_turn_start(engine: &Engine, rt: &ThreadRt, turn_id: &str, handle: Option<&ModelHandle>) {
    let t = rt.thread();
    let w = compute(engine, &t, handle);
    if let Some(w) = &w {
        crate::turn::notice(engine, rt, turn_id, NoticeLevel::Warning, w.message.clone(), Some("modelWarning"));
    }
    let used = handle.map(|h| ModelUse {
        key: h.model.key.clone(),
        provider_id: h.model.provider_id.clone(),
        model_id: h.model.model_id.clone(),
        context_window: h.context_window,
    });
    // the transcript notice records a one-time change; only a missing model keeps the banner
    let w = w.filter(|w| w.code == "notServed");
    if !same(&w, &t.model_warning) || (used.is_some() && used != t.last_model) {
        engine.update_thread(rt, |t| {
            t.model_warning = w;
            if used.is_some() {
                t.last_model = used;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dm(id: &str) -> DiscoveredModel {
        DiscoveredModel { id: id.into(), max_model_len: None, owned_by: None, root: None }
    }

    #[test]
    fn families() {
        assert_eq!(family("Qwen/Qwen3-32B-AWQ"), "qwen3");
        assert_eq!(family("mock-coder"), "mock");
        let served = vec![dm("text-embed-small"), dm("llama-3-8b"), dm("qwen3-8b")];
        assert_eq!(alternative("Qwen/Qwen3-32B", &served), Some(("qwen3-8b".into(), true)));
        assert_eq!(alternative("mistral-7b", &served), Some(("llama-3-8b".into(), false)));
        assert_eq!(alternative("x", &[dm("bge-embed")]), None);
        assert_eq!(fmt_tokens(32768), "32K");
        assert_eq!(fmt_tokens(8000), "8.0K");
    }
}
