//! `odex-hooks`: command hooks (user, project and plugin) with trust review.
//!
//! A hook is a shell command bound to a [`HookEvent`]. Hooks only run after
//! the user has reviewed and trusted them; the trust record stores the
//! definition hash, which covers the command, the matcher and the contents
//! of any script file the command refers to, so editing a hook script
//! re-requires review.
//!
//! Protocol for hook processes:
//! * stdin: the event payload as JSON plus `"hook_event": "<event>"`
//!   (protocol spelling, e.g. `"preToolUse"`); stdin is then closed.
//! * exit 2: block; stderr (or stdout) is the reason.
//! * exit 0: stdout may be JSON
//!   `{"decision": "block"|"allow", "reason": "...", "modified_input": {...}, "additional_context": "..."}`;
//!   other non-empty stdout becomes additional context.
//! * any other exit code, a timeout or a spawn failure is reported as an
//!   error and does not block.

mod hash;
mod registry;
mod runner;

use std::path::{Path, PathBuf};
use std::time::Duration;

use odex_protocol::config_types::{HookToml, HooksToml};
pub use odex_protocol::ext::{HookEvent, HookInfo};

pub use hash::{definition_hash, hook_id, referenced_files};
pub use registry::{HookOutcome, HookRegistry};

/// Default per-hook timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// One hook definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDef {
    pub event: HookEvent,
    pub command: String,
    /// Regex on the tool name (tool events only). It must match the whole
    /// name; `None`, `""` and `"*"` match every tool.
    pub matcher: Option<String>,
    pub timeout: Duration,
    pub name: Option<String>,
    /// `user`, `project:<path>` or `plugin:<id>`.
    pub source: String,
    /// Working directory the command runs in (and script paths resolve against).
    pub cwd: PathBuf,
}

impl HookDef {
    /// Stable id: short hash of event, source, command and matcher.
    pub fn id(&self) -> String {
        hook_id(self.event, &self.source, &self.command, self.matcher.as_deref())
    }

    /// Definition hash, including referenced script files' contents.
    pub fn hash(&self) -> String {
        definition_hash(self.event, &self.command, self.matcher.as_deref(), &self.cwd)
    }

    /// Name for messages: the configured name, else the command.
    pub fn display_name(&self) -> String {
        match &self.name {
            Some(name) if !name.trim().is_empty() => name.clone(),
            _ => self.command.clone(),
        }
    }

    pub(crate) fn info(&self, hash: String, trust: &str) -> HookInfo {
        HookInfo {
            id: self.id(),
            event: self.event,
            name: self.name.clone(),
            command: self.command.clone(),
            matcher: self.matcher.clone(),
            source: self.source.clone(),
            hash,
            trust: trust.to_string(),
        }
    }
}

/// The protocol (serde) spelling of an event, used for `hook_event`.
pub fn event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart => "sessionStart",
        HookEvent::UserPromptSubmit => "userPromptSubmit",
        HookEvent::PreToolUse => "preToolUse",
        HookEvent::PostToolUse => "postToolUse",
        HookEvent::Stop => "stop",
        HookEvent::Notification => "notification",
    }
}

/// Whether `event` is a tool event (where matchers apply).
pub fn is_tool_event(event: HookEvent) -> bool {
    matches!(event, HookEvent::PreToolUse | HookEvent::PostToolUse)
}

/// Convert a `[hooks]` config table into definitions (in event order, then
/// file order). Entries with an empty command are skipped; a missing or zero
/// `timeout_ms` means [`DEFAULT_TIMEOUT`].
pub fn defs_from_toml(h: &HooksToml, source: &str, cwd: &Path) -> Vec<HookDef> {
    let groups: [(HookEvent, &Vec<HookToml>); 6] = [
        (HookEvent::SessionStart, &h.session_start),
        (HookEvent::UserPromptSubmit, &h.user_prompt_submit),
        (HookEvent::PreToolUse, &h.pre_tool_use),
        (HookEvent::PostToolUse, &h.post_tool_use),
        (HookEvent::Stop, &h.stop),
        (HookEvent::Notification, &h.notification),
    ];
    let mut out = Vec::new();
    for (event, hooks) in groups {
        for hook in hooks {
            if hook.command.trim().is_empty() {
                continue;
            }
            out.push(HookDef {
                event,
                command: hook.command.clone(),
                matcher: hook.matcher.clone().filter(|m| !m.trim().is_empty()),
                timeout: match hook.timeout_ms {
                    Some(ms) if ms > 0 => Duration::from_millis(u64::from(ms)),
                    _ => DEFAULT_TIMEOUT,
                },
                name: hook.name.clone(),
                source: source.to_string(),
                cwd: cwd.to_path_buf(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_match_protocol_serialization() {
        for event in [
            HookEvent::SessionStart,
            HookEvent::UserPromptSubmit,
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::Notification,
        ] {
            assert_eq!(serde_json::to_value(event).unwrap(), serde_json::Value::String(event_name(event).into()));
        }
        assert!(is_tool_event(HookEvent::PreToolUse));
        assert!(!is_tool_event(HookEvent::Stop));
    }

    #[test]
    fn toml_conversion() {
        let hooks = HooksToml {
            pre_tool_use: vec![
                HookToml {
                    command: "check.sh".into(),
                    matcher: Some("shell".into()),
                    timeout_ms: Some(1500),
                    name: Some("check".into()),
                },
                HookToml { command: "  ".into(), ..Default::default() },
            ],
            stop: vec![HookToml {
                command: "notify".into(),
                matcher: Some(String::new()),
                timeout_ms: Some(0),
                name: None,
            }],
            ..Default::default()
        };
        let defs = defs_from_toml(&hooks, "project:/repo", Path::new("/repo"));
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].event, HookEvent::PreToolUse);
        assert_eq!(defs[0].timeout, Duration::from_millis(1500));
        assert_eq!(defs[0].matcher.as_deref(), Some("shell"));
        assert_eq!(defs[0].display_name(), "check");
        assert_eq!(defs[0].source, "project:/repo");
        assert_eq!(defs[1].event, HookEvent::Stop);
        assert_eq!(defs[1].timeout, DEFAULT_TIMEOUT);
        assert_eq!(defs[1].matcher, None);
        assert_eq!(defs[1].display_name(), "notify");
    }
}
