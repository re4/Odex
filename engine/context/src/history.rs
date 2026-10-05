//! Model-facing conversation history.

use serde::{Deserialize, Serialize};

use odex_llm::types::{ChatMessage, ContentPart, Role};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A user message (start of a turn).
    User,
    /// A user message injected mid-turn (steering).
    Steer,
    /// An assistant step (text and/or tool calls).
    Assistant,
    /// The result of one tool call.
    ToolResult,
    /// Engine-injected guidance (loop breaker, validation retry hints, hook context).
    Nudge,
    /// Images attached after a tool result (screenshots, view_image).
    Image,
}

/// Metadata about a tool result, used for pruning and stubs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolMeta {
    pub tool: String,
    pub call_id: String,
    /// Short human/LLM-readable argument summary (`path=src/a.rs`).
    pub args_summary: String,
    pub exit_code: Option<i32>,
    pub success: bool,
    /// `ref:out_N` of the full output on disk.
    pub output_ref: Option<String>,
    /// Size of the full output.
    pub full_chars: usize,
    pub lines: usize,
    /// Path this tool read (file reads; used to detect superseded reads).
    pub file_read: Option<String>,
    /// Paths this tool modified.
    pub file_writes: Vec<String>,
    /// Hash of (tool, args, output) for duplicate collapsing.
    pub fingerprint: u64,
    /// Hash of (tool, args) for repeated-failure collapsing.
    pub call_fingerprint: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub turn_id: String,
    /// Ordinal of the turn this entry belongs to (0-based).
    pub turn_index: u32,
    pub kind: EntryKind,
    pub msg: ChatMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolMeta>,
    /// Estimated tokens (refreshed when content changes).
    pub tokens: u32,
    /// Never pruned or summarized away.
    #[serde(default)]
    pub pinned: bool,
    /// Content replaced by a one-line stub.
    #[serde(default)]
    pub stubbed: bool,
}

impl HistoryEntry {
    pub fn new(turn_id: &str, turn_index: u32, kind: EntryKind, msg: ChatMessage) -> Self {
        Self {
            id: format!("e_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
            turn_id: turn_id.to_string(),
            turn_index,
            kind,
            msg,
            tool: None,
            tokens: 0,
            pinned: false,
            stubbed: false,
        }
    }

    pub fn with_tool(mut self, meta: ToolMeta) -> Self {
        self.tool = Some(meta);
        self
    }

    pub fn is_user(&self) -> bool {
        matches!(self.kind, EntryKind::User | EntryKind::Steer)
    }

    pub fn image_count(&self) -> usize {
        self.msg.image_count()
    }

    pub fn text(&self) -> String {
        self.msg.text_content()
    }

    /// Replace the text content (images dropped), keeping tool linkage.
    pub fn set_text(&mut self, text: String) {
        self.msg.content = vec![ContentPart::Text { text }];
    }
}

/// Indices of atomic groups: a group starts at a user/steer/nudge/assistant
/// entry; tool results and images attach to the preceding assistant group.
/// Splitting history only at group starts never separates a tool call from
/// its result.
pub fn group_starts(entries: &[HistoryEntry]) -> Vec<usize> {
    let mut starts = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let attaches = matches!(e.kind, EntryKind::ToolResult | EntryKind::Image)
            && i > 0
            && entries[..i].iter().rev().any(|p| p.kind == EntryKind::Assistant);
        if !attaches {
            starts.push(i);
        }
    }
    starts
}

/// Make a message list safe for strict chat templates:
/// - first non-system message is a user message,
/// - consecutive user messages are merged,
/// - every assistant tool call has a tool result (synthesized if missing),
/// - orphan tool results (no preceding call) become user text,
/// - images are only on user messages.
pub fn normalize_for_template(msgs: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(msgs.len());
    let mut open_calls: Vec<(String, String)> = Vec::new(); // (id, name)
    for m in msgs {
        match m.role {
            Role::Tool => {
                let id = m.tool_call_id.clone().unwrap_or_default();
                if let Some(pos) = open_calls.iter().position(|(cid, _)| *cid == id) {
                    open_calls.remove(pos);
                    out.push(m);
                } else {
                    close_open(&mut out, &mut open_calls);
                    let mut u = ChatMessage::user(format!("[tool result]\n{}", m.text_content()));
                    u.content.extend(m.content.iter().filter(|p| matches!(p, ContentPart::ImageUrl { .. })).cloned());
                    push_user(&mut out, u);
                }
            }
            Role::Assistant => {
                close_open(&mut out, &mut open_calls);
                if out.iter().all(|x| x.role == Role::System) {
                    out.push(ChatMessage::user("(continuing)"));
                }
                open_calls = m.tool_calls.iter().map(|c| (c.id.clone(), c.name.clone())).collect();
                if let Some(prev) = out.last_mut() {
                    // merge consecutive assistant texts without tool calls
                    if prev.role == Role::Assistant && prev.tool_calls.is_empty() && m.tool_calls.is_empty() {
                        let t = format!("{}\n\n{}", prev.text_content(), m.text_content());
                        prev.content = vec![ContentPart::Text { text: t.trim().to_string() }];
                        continue;
                    }
                }
                out.push(m);
            }
            Role::User => {
                close_open(&mut out, &mut open_calls);
                push_user(&mut out, m);
            }
            Role::System => {
                if out.is_empty() || out.iter().all(|x| x.role == Role::System) {
                    if let Some(prev) = out.last_mut() {
                        let t = format!("{}\n\n{}", prev.text_content(), m.text_content());
                        prev.content = vec![ContentPart::Text { text: t }];
                    } else {
                        out.push(m);
                    }
                } else {
                    // late system messages become user notes (many templates reject them)
                    close_open(&mut out, &mut open_calls);
                    push_user(&mut out, ChatMessage::user(format!("[system note]\n{}", m.text_content())));
                }
            }
        }
    }
    close_open(&mut out, &mut open_calls);
    if out.iter().all(|x| x.role == Role::System) {
        out.push(ChatMessage::user("(continue)"));
    }
    out
}

fn close_open(out: &mut Vec<ChatMessage>, open: &mut Vec<(String, String)>) {
    for (id, name) in open.drain(..) {
        out.push(ChatMessage::tool_result(id, name, "[no result recorded]"));
    }
}

fn push_user(out: &mut Vec<ChatMessage>, m: ChatMessage) {
    if let Some(prev) = out.last_mut() {
        if prev.role == Role::User {
            let mut parts = prev.content.clone();
            parts.push(ContentPart::Text { text: "\n\n".into() });
            parts.extend(m.content);
            // collapse adjacent text parts
            let mut merged: Vec<ContentPart> = Vec::new();
            for p in parts {
                match (merged.last_mut(), p) {
                    (Some(ContentPart::Text { text: a }), ContentPart::Text { text: b }) => a.push_str(&b),
                    (_, p) => merged.push(p),
                }
            }
            prev.content = merged;
            return;
        }
    }
    out.push(m);
}

#[cfg(test)]
mod tests {
    use super::*;
    use odex_llm::types::ToolCall;

    fn call(id: &str) -> ToolCall {
        ToolCall { id: id.into(), name: "t".into(), arguments: "{}".into() }
    }

    #[test]
    fn normalizes_structure() {
        let msgs = vec![
            ChatMessage::system("sys"),
            ChatMessage::assistant("hi"),
            ChatMessage::user("a"),
            ChatMessage::user("b"),
            ChatMessage::assistant_tool_calls(None, vec![call("1"), call("2")]),
            ChatMessage::tool_result("1", "t", "r1"),
            ChatMessage::tool_result("zz", "t", "orphan"),
            ChatMessage::system("late"),
        ];
        let out = normalize_for_template(msgs);
        let roles: Vec<Role> = out.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                Role::System,
                Role::User,
                Role::Assistant,
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::Tool,
                Role::User
            ]
        );
        assert!(out[3].text_content().contains('a') && out[3].text_content().contains('b'));
        assert_eq!(out[6].tool_call_id.as_deref(), Some("2"));
        assert!(out[7].text_content().contains("orphan") && out[7].text_content().contains("late"));
    }

    #[test]
    fn groups_keep_tool_results_with_calls() {
        let mk = |k| HistoryEntry::new("t", 0, k, ChatMessage::user("x"));
        let es = vec![
            mk(EntryKind::User),
            mk(EntryKind::Assistant),
            mk(EntryKind::ToolResult),
            mk(EntryKind::ToolResult),
            mk(EntryKind::Assistant),
            mk(EntryKind::User),
        ];
        assert_eq!(group_starts(&es), vec![0, 1, 4, 5]);
    }
}
