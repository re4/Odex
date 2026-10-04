//! Hook registry: trust store, listing and running hooks for an event.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::runner::{run_hook, RunError};
use crate::{event_name, is_tool_event, HookDef, HookEvent, HookInfo};

/// Combined result of running every hook bound to an event.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HookOutcome {
    /// A hook blocked the action (exit code 2 or `"decision": "block"`).
    pub blocked: bool,
    pub reason: Option<String>,
    /// Latest `modified_input` returned by a hook (replaces the tool input).
    pub modified_input: Option<Value>,
    /// Extra context for the model, in hook order.
    pub additional_context: Vec<String>,
    /// Hooks that were executed (display names).
    pub ran: Vec<String>,
    /// Hooks that were skipped because they are untrusted or changed since review.
    pub skipped_untrusted: Vec<String>,
    /// Non-blocking failures: bad exit codes, timeouts, spawn errors, bad matchers.
    pub errors: Vec<String>,
}

/// Hook definitions plus the persistent trust store (`{ "<id>": "<hash>" }`).
#[derive(Debug, Clone)]
pub struct HookRegistry {
    defs: Vec<HookDef>,
    trust_store: PathBuf,
    trusted: BTreeMap<String, String>,
}

const TRUSTED: &str = "trusted";
const UNTRUSTED: &str = "untrusted";
const CHANGED: &str = "changed";

impl HookRegistry {
    /// Create a registry; the trust store is read now (a missing or invalid
    /// file means nothing is trusted).
    pub fn new(defs: Vec<HookDef>, trust_store: PathBuf) -> Self {
        let trusted = read_trust_store(&trust_store);
        HookRegistry { defs, trust_store, trusted }
    }

    pub fn defs(&self) -> &[HookDef] {
        &self.defs
    }

    /// Re-read the trust store from disk (e.g. after another process changed it).
    pub fn reload_trust(&mut self) {
        self.trusted = read_trust_store(&self.trust_store);
    }

    fn trust_of(&self, def: &HookDef, hash: &str) -> &'static str {
        match self.trusted.get(&def.id()) {
            Some(stored) if stored == hash => TRUSTED,
            Some(_) => CHANGED,
            None => UNTRUSTED,
        }
    }

    /// Every hook with its trust state: `trusted`, `untrusted` or `changed`.
    pub fn list(&self) -> Vec<HookInfo> {
        self.defs
            .iter()
            .map(|def| {
                let hash = def.hash();
                let trust = self.trust_of(def, &hash);
                def.info(hash, trust)
            })
            .collect()
    }

    /// Hooks that need the user's review (untrusted or changed).
    pub fn needs_review(&self) -> Vec<HookInfo> {
        self.list().into_iter().filter(|info| info.trust != TRUSTED).collect()
    }

    /// Trust or untrust a hook. Trusting requires `hash` to equal the hook's
    /// current definition hash (what the user reviewed must be what runs);
    /// otherwise `InvalidInput` is returned. Unknown ids give `NotFound` when
    /// trusting. The store is re-read before writing so concurrent changes by
    /// other registries are kept, and written atomically.
    pub fn set_trust(&mut self, id: &str, hash: &str, trusted: bool) -> io::Result<()> {
        if trusted {
            let def = self
                .defs
                .iter()
                .find(|def| def.id() == id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("unknown hook id '{id}'")))?;
            let current = def.hash();
            if current != hash {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("hook '{id}' changed since it was reviewed (hash {hash} != current {current})"),
                ));
            }
        }
        let mut store = read_trust_store(&self.trust_store);
        if trusted {
            store.insert(id.to_string(), hash.to_string());
        } else {
            store.remove(id);
        }
        write_trust_store(&self.trust_store, &store)?;
        self.trusted = store;
        Ok(())
    }

    /// Run the trusted hooks bound to `event`, sequentially in definition
    /// order. A block stops the chain; `modified_input` from one hook becomes
    /// the `tool_input` of the next hook's payload.
    pub async fn run(&self, event: HookEvent, payload: Value, tool_name: Option<&str>) -> HookOutcome {
        let mut outcome = HookOutcome::default();
        let mut payload = into_object(payload);
        payload.insert("hook_event".to_string(), Value::String(event_name(event).to_string()));

        for def in self.defs.iter().filter(|def| def.event == event) {
            let label = def.display_name();
            if is_tool_event(event) {
                match matcher_matches(def.matcher.as_deref(), tool_name) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(e) => {
                        outcome.errors.push(format!("hook '{label}': invalid matcher: {e}"));
                        continue;
                    }
                }
            }
            if self.trust_of(def, &def.hash()) != TRUSTED {
                outcome.skipped_untrusted.push(label);
                continue;
            }

            let input = serde_json::to_vec(&Value::Object(payload.clone())).unwrap_or_default();
            let id = def.id();
            let env = [("ODEX_HOOK_EVENT", event_name(event)), ("ODEX_HOOK_ID", id.as_str())];
            let result = run_hook(&def.command, &def.cwd, &input, def.timeout, &env).await;
            let run = match result {
                Ok(run) => run,
                Err(RunError::Spawn(e)) => {
                    outcome.errors.push(format!("hook '{label}' could not be started: {e}"));
                    continue;
                }
                Err(RunError::Wait(e)) => {
                    outcome.ran.push(label.clone());
                    outcome.errors.push(format!("hook '{label}' failed: {e}"));
                    continue;
                }
                Err(RunError::Timeout) => {
                    outcome.ran.push(label.clone());
                    outcome.errors.push(format!(
                        "hook '{label}' timed out after {:.1}s and was stopped",
                        def.timeout.as_secs_f64()
                    ));
                    continue;
                }
            };
            outcome.ran.push(label.clone());

            match run.status.code() {
                Some(2) => {
                    let reason = first_non_empty(&[&run.stderr, &run.stdout])
                        .unwrap_or_else(|| format!("blocked by hook '{label}'"));
                    outcome.blocked = true;
                    outcome.reason = Some(reason);
                    break;
                }
                Some(0) => {
                    if apply_response(&run.stdout, &label, &mut outcome, &mut payload) {
                        break;
                    }
                }
                code => {
                    let code = code.map_or_else(|| "a signal".to_string(), |c| format!("code {c}"));
                    let detail =
                        first_non_empty(&[&run.stderr, &run.stdout]).map(|d| format!(": {d}")).unwrap_or_default();
                    outcome.errors.push(format!("hook '{label}' exited with {code}{detail}"));
                }
            }
        }
        outcome
    }
}

/// Handle a successful hook's stdout. Returns `true` when the hook blocked.
fn apply_response(stdout: &str, label: &str, outcome: &mut HookOutcome, payload: &mut Map<String, Value>) -> bool {
    let text = stdout.trim();
    if text.is_empty() {
        return false;
    }
    let Ok(Value::Object(response)) = serde_json::from_str::<Value>(text) else {
        outcome.additional_context.push(text.to_string());
        return false;
    };
    match response.get("additional_context") {
        Some(Value::String(context)) if !context.trim().is_empty() => {
            outcome.additional_context.push(context.trim().to_string());
        }
        Some(Value::Array(items)) => {
            outcome.additional_context.extend(
                items.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
            );
        }
        _ => {}
    }
    if let Some(modified) = response.get("modified_input").filter(|v| !v.is_null()) {
        outcome.modified_input = Some(modified.clone());
        payload.insert("tool_input".to_string(), modified.clone());
    }
    let blocked = response.get("decision").and_then(Value::as_str).is_some_and(|d| d.eq_ignore_ascii_case("block"));
    if blocked {
        outcome.blocked = true;
        outcome.reason = Some(
            response
                .get("reason")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map_or_else(|| format!("blocked by hook '{label}'"), str::to_string),
        );
    }
    blocked
}

fn first_non_empty(candidates: &[&str]) -> Option<String> {
    candidates.iter().map(|s| s.trim()).find(|s| !s.is_empty()).map(str::to_string)
}

/// The matcher must match the whole tool name; none / `""` / `"*"` match all.
fn matcher_matches(matcher: Option<&str>, tool_name: Option<&str>) -> Result<bool, regex::Error> {
    let matcher = match matcher.map(str::trim) {
        None | Some("") | Some("*") => return Ok(true),
        Some(m) => m,
    };
    let Some(tool) = tool_name else { return Ok(false) };
    Ok(Regex::new(&format!("^(?:{matcher})$"))?.is_match(tool))
}

fn into_object(payload: Value) -> Map<String, Value> {
    match payload {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => {
            let mut map = Map::new();
            map.insert("payload".to_string(), other);
            map
        }
    }
}

fn read_trust_store(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = fs::read_to_string(path) else { return BTreeMap::new() };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => {
            map.into_iter().filter_map(|(id, hash)| hash.as_str().map(|h| (id, h.to_string()))).collect()
        }
        _ => BTreeMap::new(),
    }
}

/// Write via a temporary sibling file and rename, so readers never see a
/// partial file.
fn write_trust_store(path: &Path, store: &BTreeMap<String, String>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(store).map_err(io::Error::other)?;
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "trust".into());
    let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    fs::write(&tmp, json.as_bytes())?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_semantics() {
        assert!(matcher_matches(None, Some("shell")).unwrap());
        assert!(matcher_matches(Some("*"), None).unwrap());
        assert!(matcher_matches(Some("shell|apply_patch"), Some("apply_patch")).unwrap());
        assert!(!matcher_matches(Some("shell"), Some("shell_command")).unwrap());
        assert!(matcher_matches(Some("mcp__.*"), Some("mcp__github__search")).unwrap());
        assert!(!matcher_matches(Some("shell"), None).unwrap());
        assert!(matcher_matches(Some("("), Some("x")).is_err());
    }

    #[test]
    fn response_parsing() {
        let mut outcome = HookOutcome::default();
        let mut payload = Map::new();
        let blocked = apply_response(
            r#"{"decision":"allow","modified_input":{"cmd":"ls"},"additional_context":["a"," ",""]}"#,
            "h",
            &mut outcome,
            &mut payload,
        );
        assert!(!blocked);
        assert_eq!(outcome.modified_input, Some(serde_json::json!({"cmd": "ls"})));
        assert_eq!(payload.get("tool_input"), Some(&serde_json::json!({"cmd": "ls"})));
        assert_eq!(outcome.additional_context, vec!["a"]);

        let blocked = apply_response(r#"{"decision":"block"}"#, "h", &mut outcome, &mut payload);
        assert!(blocked);
        assert_eq!(outcome.reason.as_deref(), Some("blocked by hook 'h'"));

        let mut outcome = HookOutcome::default();
        assert!(!apply_response("  plain text \n", "h", &mut outcome, &mut payload));
        assert!(!apply_response("[1,2]", "h", &mut outcome, &mut payload));
        assert!(!apply_response("", "h", &mut outcome, &mut payload));
        assert_eq!(outcome.additional_context, vec!["plain text", "[1,2]"]);
    }

    #[test]
    fn payload_normalisation() {
        assert_eq!(into_object(Value::Null), Map::new());
        assert_eq!(into_object(serde_json::json!(3)).get("payload"), Some(&serde_json::json!(3)));
        assert_eq!(into_object(serde_json::json!({"a": 1})).get("a"), Some(&serde_json::json!(1)));
    }

    #[test]
    fn trust_store_round_trip_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("trusted_hooks.json");
        assert!(read_trust_store(&path).is_empty());
        let mut store = BTreeMap::new();
        store.insert("abc".to_string(), "123".to_string());
        write_trust_store(&path, &store).unwrap();
        assert_eq!(read_trust_store(&path), store);
        fs::write(&path, "not json").unwrap();
        assert!(read_trust_store(&path).is_empty());
        fs::write(&path, r#"{"a": "h", "b": 5}"#).unwrap();
        assert_eq!(read_trust_store(&path).len(), 1);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1, "no temp files left");
    }
}
