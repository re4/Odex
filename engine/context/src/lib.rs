//! Odex smart context engine (PROMPT §10).
//!
//! - **Budget**: `window − reserved_output − margin`, checked before every
//!   model call inside a turn.
//! - **Tier 0** (prevention, done by the tool layer + [`ContextState::push`]):
//!   capped tool outputs with refs, paged reads, only the newest images kept.
//! - **Tier 1** [`ContextState::prune`]: stub old tool outputs, superseded
//!   reads, duplicates/repeated failures, old screenshots, old reasoning —
//!   in large batches to keep the prefix cache warm.
//! - **Tier 2** [`compact`]: structured LLM summary (map-reduce if needed)
//!   that keeps verbatim requirements; rebuilt context = system, summary,
//!   pinned items, working set, recent turns. Extractive fallback on failure.
//! - **Tier 3** [`ContextState::emergency_trim`]: on overflow, prune hard,
//!   compact, then hard-trim oldest non-pinned items. The newest user message
//!   is never dropped.

pub mod history;
pub mod summary;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use odex_config::ContextSettings;
use odex_llm::tokens::{TokenEstimator, IMAGE_TOKENS};
use odex_llm::types::{ChatMessage, ContentPart, ToolSpec};
use odex_protocol::{CompactionRecord, CompactionTrigger, ContextBreakdown, ReasoningHistory};

pub use history::{group_starts, normalize_for_template, EntryKind, HistoryEntry, ToolMeta};
pub use summary::{PlanItem, SummaryData};

/// Window budget for one model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    pub window: u32,
    pub reserved_output: u32,
    pub margin: u32,
}

impl Budget {
    pub fn new(window: u32, max_output_tokens: u32, s: &ContextSettings) -> Self {
        let reserved = max_output_tokens.min((window as f64 * s.reserve_output_ratio) as u32).max(64.min(window / 4));
        let margin = ((window as f64 * s.margin_ratio).ceil() as u32).max(16);
        Self { window, reserved_output: reserved, margin }
    }

    /// Tokens available for the prompt.
    pub fn budget(&self) -> u32 {
        self.window.saturating_sub(self.reserved_output + self.margin)
    }

    /// `max_tokens = min(max_output, window − prompt − margin)`.
    pub fn max_tokens_for(&self, prompt_tokens: u32, max_output_tokens: u32) -> u32 {
        max_output_tokens.min(self.window.saturating_sub(prompt_tokens + self.margin))
    }
}

/// The pieces of the system prompt, kept separate for the breakdown.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemParts {
    /// Fixed base prompt (byte-stable).
    pub base: String,
    /// Environment, custom instructions, skills list, MCP instructions.
    pub extra: String,
    pub agents_md: String,
    pub memories: String,
}

impl SystemParts {
    pub fn render(&self) -> String {
        let mut s = self.base.clone();
        for part in [&self.agents_md, &self.extra, &self.memories] {
            if !part.trim().is_empty() {
                s.push_str("\n\n");
                s.push_str(part.trim_end());
            }
        }
        s
    }
}

/// Items that survive every compaction.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pinned {
    /// The thread's original task, verbatim.
    pub original_task: Option<String>,
    /// Active `/goal`.
    pub goal: Option<String>,
    pub plan: Vec<PlanItem>,
    /// `.odex/NOTES.md`, size-capped.
    pub notes: Option<String>,
    /// git status / diff stat / files touched snapshot.
    pub working_set: Option<String>,
}

impl Pinned {
    /// Render the pinned block. Before any compaction only the goal is shown
    /// (the original task and plan are still in the history verbatim).
    pub fn render(&self, compacted: bool) -> Option<String> {
        let mut s = String::new();
        if compacted {
            if let Some(t) = self.original_task.as_ref().filter(|t| !t.trim().is_empty()) {
                s.push_str("### Original task (verbatim)\n");
                s.push_str(t.trim());
                s.push_str("\n\n");
            }
        }
        if let Some(g) = self.goal.as_ref().filter(|g| !g.trim().is_empty()) {
            s.push_str("### Active goal (keep pursuing until done or blocked)\n");
            s.push_str(g.trim());
            s.push_str("\n\n");
        }
        if compacted {
            if !self.plan.is_empty() {
                s.push_str("### Current plan\n");
                for p in &self.plan {
                    let mark = match p.status.as_str() {
                        "completed" => "[x]",
                        "in_progress" => "[~]",
                        _ => "[ ]",
                    };
                    s.push_str(&format!("- {mark} {}\n", p.step));
                }
                s.push('\n');
            }
            if let Some(n) = self.notes.as_ref().filter(|n| !n.trim().is_empty()) {
                s.push_str("### Working notes (.odex/NOTES.md)\n");
                s.push_str(n.trim());
                s.push_str("\n\n");
            }
            if let Some(w) = self.working_set.as_ref().filter(|w| !w.trim().is_empty()) {
                s.push_str("### Working set\n");
                s.push_str(w.trim());
                s.push('\n');
            }
        }
        if s.is_empty() {
            None
        } else {
            Some(format!("[Pinned context]\n{s}"))
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StoredSummary {
    pub number: u32,
    pub data: SummaryData,
    pub llm: bool,
}

/// The mutable context of one thread.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextState {
    pub entries: Vec<HistoryEntry>,
    pub summary: Option<StoredSummary>,
    pub compactions: Vec<CompactionRecord>,
    pub prunes: u32,
    /// exact / estimated prompt tokens (EMA), applied to estimates.
    #[serde(default = "one")]
    pub correction: f64,
    /// Exact prompt tokens of the last request, if no entries were added since.
    #[serde(default)]
    pub last_exact: Option<u32>,
    /// Verbatim user requirements carried across compactions.
    #[serde(default)]
    pub requirements: Vec<String>,
}

fn one() -> f64 {
    1.0
}

/// Inputs needed to build or measure a prompt.
pub struct PromptInputs<'a> {
    pub system: &'a SystemParts,
    pub tools: &'a [ToolSpec],
    pub pinned: &'a Pinned,
    pub estimator: &'a TokenEstimator,
    pub reasoning_history: ReasoningHistory,
    pub current_turn: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PruneReport {
    pub before: u32,
    pub after: u32,
    pub stubbed: usize,
    pub reasoning_dropped: usize,
}

const MAX_REQUIREMENT_CHARS: usize = 2000;

impl ContextState {
    pub fn new() -> Self {
        Self { correction: 1.0, ..Default::default() }
    }

    pub fn compacted(&self) -> bool {
        self.summary.is_some()
    }

    /// Append an entry, estimating its tokens. Enforces image hygiene: only the
    /// newest `max_images` images stay as images; older ones become stubs.
    pub fn push(&mut self, mut e: HistoryEntry, est: &TokenEstimator, max_images: u32) {
        if e.is_user() && e.kind == EntryKind::User {
            let t = e.text();
            let t = t.trim();
            if !t.is_empty() {
                let r: String = t.chars().take(MAX_REQUIREMENT_CHARS).collect();
                if !self.requirements.contains(&r) {
                    self.requirements.push(r);
                    // keep the list bounded: first task + latest messages
                    if self.requirements.len() > 24 {
                        self.requirements.remove(1);
                    }
                }
            }
        }
        e.tokens = est.message(&e.msg);
        let adds_images = e.image_count() > 0;
        self.entries.push(e);
        self.last_exact = None;
        if adds_images {
            self.enforce_image_limit(max_images as usize, est);
        }
    }

    fn enforce_image_limit(&mut self, max_images: usize, est: &TokenEstimator) {
        let mut seen = 0usize;
        for e in self.entries.iter_mut().rev() {
            let n = e.image_count();
            if n == 0 {
                continue;
            }
            if seen + n <= max_images {
                seen += n;
                continue;
            }
            let text = e.text();
            let stub = format!("{}\n[image removed to save context; it showed the result of the step above]", text.trim());
            e.msg.content = vec![ContentPart::Text { text: stub.trim().to_string() }];
            e.tokens = est.message(&e.msg);
        }
    }

    /// Record an exact prompt token count for a request built from the current state.
    pub fn observe_usage(&mut self, exact_prompt_tokens: u32, estimated_raw: u32) {
        if estimated_raw > 50 && exact_prompt_tokens > 50 {
            let ratio = (exact_prompt_tokens as f64 / estimated_raw as f64).clamp(0.4, 3.0);
            self.correction = self.correction * 0.5 + ratio * 0.5;
        }
        self.last_exact = Some(exact_prompt_tokens);
    }

    /// Raw (uncorrected) breakdown.
    pub fn raw_breakdown(&self, p: &PromptInputs) -> ContextBreakdown {
        let est = p.estimator;
        let mut b = ContextBreakdown {
            system: est.text(&p.system.base) + est.text(&p.system.extra) + 8,
            tools: est.tools(p.tools),
            agents_md: est.text(&p.system.agents_md),
            memories: est.text(&p.system.memories),
            ..Default::default()
        };
        if let Some(s) = &self.summary {
            b.summary = est.text(&s.data.render(s.number));
        }
        if let Some(pin) = p.pinned.render(self.compacted()) {
            b.pinned = est.text(&pin);
        }
        for e in &self.entries {
            let images = e.image_count() as u32;
            let text_tokens = e.tokens.saturating_sub(images * IMAGE_TOKENS);
            b.images += images * IMAGE_TOKENS;
            match e.kind {
                EntryKind::ToolResult => b.tool_outputs += text_tokens,
                _ => b.history += text_tokens,
            }
            if e.kind == EntryKind::Assistant && !self.keeps_reasoning(e, p) {
                // reasoning not sent → don't count it
                if let Some(r) = &e.msg.reasoning {
                    b.history = b.history.saturating_sub(est.text(r));
                }
            }
        }
        b
    }

    fn keeps_reasoning(&self, e: &HistoryEntry, p: &PromptInputs) -> bool {
        match p.reasoning_history {
            ReasoningHistory::Drop => false,
            ReasoningHistory::All => true,
            ReasoningHistory::CurrentTurn => e.turn_index == p.current_turn,
        }
    }

    /// Corrected breakdown (scaled so the total matches calibrated estimates).
    pub fn breakdown(&self, p: &PromptInputs) -> ContextBreakdown {
        let raw = self.raw_breakdown(p);
        let c = self.correction;
        let scale = |x: u32| (x as f64 * c).round() as u32;
        ContextBreakdown {
            system: scale(raw.system),
            tools: scale(raw.tools),
            agents_md: scale(raw.agents_md),
            memories: scale(raw.memories),
            summary: scale(raw.summary),
            pinned: scale(raw.pinned),
            history: scale(raw.history),
            tool_outputs: scale(raw.tool_outputs),
            images: raw.images,
        }
    }

    /// Estimated prompt tokens of the next request.
    pub fn estimate(&self, p: &PromptInputs) -> u32 {
        if let Some(x) = self.last_exact {
            return x;
        }
        self.breakdown(p).total()
    }

    pub fn raw_estimate(&self, p: &PromptInputs) -> u32 {
        self.raw_breakdown(p).total()
    }

    /// Build the request messages (template-normalized).
    pub fn build_messages(&self, p: &PromptInputs) -> Vec<ChatMessage> {
        let mut msgs = vec![ChatMessage::system(p.system.render())];
        let mut lead = String::new();
        if let Some(s) = &self.summary {
            lead.push_str(&s.data.render(s.number));
        }
        if let Some(pin) = p.pinned.render(self.compacted()) {
            if !lead.is_empty() {
                lead.push_str("\n\n");
            }
            lead.push_str(&pin);
        }
        if !lead.is_empty() {
            msgs.push(ChatMessage::user(lead));
        }
        for e in &self.entries {
            let mut m = e.msg.clone();
            if m.reasoning.is_some() && !self.keeps_reasoning(e, p) {
                m.reasoning = None;
            }
            msgs.push(m);
        }
        normalize_for_template(msgs)
    }

    fn current_group_start(&self) -> usize {
        group_starts(&self.entries).last().copied().unwrap_or(0)
    }

    fn stub(e: &mut HistoryEntry, est: &TokenEstimator, why: &str) -> bool {
        if e.stubbed || e.pinned || e.kind != EntryKind::ToolResult {
            return false;
        }
        let Some(t) = &e.tool else { return false };
        let status = match (t.success, t.exit_code) {
            (_, Some(c)) => format!("exit {c}"),
            (true, None) => "ok".into(),
            (false, None) => "failed".into(),
        };
        let size = if t.lines > 1 { format!("{} lines", t.lines) } else { format!("{} chars", t.full_chars) };
        let ref_part = match &t.output_ref {
            Some(r) => format!("; full output: {r} (use read_output(\"{r}\"))"),
            None => "; use recall(...) to search it".into(),
        };
        let stub = format!("[{}({}) → {status}, {size}; {why}{ref_part}]", t.tool, t.args_summary);
        e.set_text(stub);
        e.stubbed = true;
        e.tokens = est.message(&e.msg);
        true
    }

    /// Tier 1 pruning. Applies steps in order until the estimate is ≤ `target`.
    /// `aggressive` (Tier 3) stubs every tool output outside the current step
    /// and drops all reasoning and older images.
    pub fn prune(&mut self, p: &PromptInputs, s: &ContextSettings, target: u32, aggressive: bool) -> PruneReport {
        let est = p.estimator;
        let before = self.estimate(p);
        let mut rep = PruneReport { before, ..Default::default() };
        let protect_from = self.current_group_start();
        let over = |st: &ContextState| st.estimate(p) > target;
        self.last_exact = None;

        // 1. old tool outputs → stubs (oldest first)
        let k = if aggressive { 0 } else { s.stub_after_turns };
        let cutoff_turn = p.current_turn.saturating_sub(k);
        for i in 0..protect_from {
            if !over(self) {
                break;
            }
            let e = &mut self.entries[i];
            if e.turn_index < cutoff_turn || (aggressive && i < protect_from) {
                if Self::stub(e, est, "output pruned to save context") {
                    rep.stubbed += 1;
                }
            }
        }
        // 2. superseded file reads (read again later, or written after)
        if over(self) {
            for i in 0..protect_from {
                let Some(path) = self.entries[i].tool.as_ref().and_then(|t| t.file_read.clone()) else { continue };
                let superseded = self.entries[i + 1..].iter().any(|later| {
                    later.tool.as_ref().map(|t| t.file_read.as_deref() == Some(&path) || t.file_writes.contains(&path)).unwrap_or(false)
                });
                if superseded && Self::stub(&mut self.entries[i], est, "superseded by a later read/edit of this file") {
                    rep.stubbed += 1;
                }
            }
        }
        // 3. duplicates and repeated failures
        if over(self) {
            let n = self.entries.len();
            for i in 0..protect_from.min(n) {
                let Some(t) = self.entries[i].tool.clone() else { continue };
                let later_same = self.entries[i + 1..].iter().any(|l| l.tool.as_ref().map(|lt| lt.fingerprint == t.fingerprint).unwrap_or(false));
                let later_fail = !t.success
                    && self.entries[i + 1..]
                        .iter()
                        .any(|l| l.tool.as_ref().map(|lt| lt.call_fingerprint == t.call_fingerprint).unwrap_or(false));
                if later_same && Self::stub(&mut self.entries[i], est, "duplicate of a later identical result") {
                    rep.stubbed += 1;
                } else if later_fail && Self::stub(&mut self.entries[i], est, "earlier attempt of a repeated call") {
                    rep.stubbed += 1;
                }
            }
        }
        // 4. old screenshots → stubs
        if over(self) || aggressive {
            let keep = if aggressive { 1 } else { s.max_images.min(1) as usize };
            self.enforce_image_limit(keep, est);
        }
        // 5. old reasoning
        if over(self) || aggressive {
            for e in self.entries.iter_mut() {
                if e.kind == EntryKind::Assistant && e.msg.reasoning.is_some() && (aggressive || e.turn_index < p.current_turn) {
                    e.msg.reasoning = None;
                    e.tokens = est.message(&e.msg);
                    rep.reasoning_dropped += 1;
                }
            }
        }
        // 6. (aggressive) any remaining older tool output
        if aggressive && over(self) {
            for i in 0..protect_from {
                if Self::stub(&mut self.entries[i], est, "output pruned (emergency)") {
                    rep.stubbed += 1;
                }
            }
        }
        if rep.stubbed > 0 || rep.reasoning_dropped > 0 {
            self.prunes += 1;
        }
        rep.after = self.estimate(p);
        rep
    }

    /// Choose the split: entries before it get summarized. Keeps about
    /// `keep_recent` tokens of whole groups, preferring a turn boundary.
    /// Returns `None` when there is nothing worth summarizing.
    pub fn plan_compaction(&self, keep_recent: u32) -> Option<usize> {
        let starts = group_starts(&self.entries);
        if starts.len() < 2 {
            return None;
        }
        let mut acc = 0u32;
        let mut split = self.entries.len();
        for (gi, &start) in starts.iter().enumerate().rev() {
            let end = starts.get(gi + 1).copied().unwrap_or(self.entries.len());
            let t: u32 = self.entries[start..end].iter().map(|e| e.tokens).sum();
            if acc + t > keep_recent && split < self.entries.len() {
                break;
            }
            acc += t;
            split = start;
            if acc > keep_recent {
                break;
            }
        }
        // Prefer starting the kept region at a user turn boundary.
        if let Some(u) = (split..self.entries.len()).find(|&i| self.entries[i].kind == EntryKind::User && starts.contains(&i)) {
            if u < self.entries.len() {
                split = u;
            }
        }
        // Always keep at least the last group.
        let last_group = *starts.last().unwrap();
        split = split.min(last_group);
        let summarizable = self.entries[..split].iter().filter(|e| !e.pinned).count();
        if summarizable == 0 {
            None
        } else {
            Some(split)
        }
    }

    /// Hard-trim oldest non-pinned groups until ≤ target (Tier 3, last resort).
    /// The newest user message and the last group are never dropped.
    pub fn emergency_trim(&mut self, p: &PromptInputs, target: u32) -> usize {
        let mut dropped = 0;
        self.last_exact = None;
        loop {
            if self.estimate(p) <= target {
                break;
            }
            let newest_user = self.entries.iter().rposition(|e| e.kind == EntryKind::User);
            let starts = group_starts(&self.entries);
            if starts.len() <= 1 {
                break;
            }
            let last_group = *starts.last().unwrap();
            // find the oldest droppable group
            let mut victim = None;
            for (gi, &s) in starts.iter().enumerate() {
                let e = starts.get(gi + 1).copied().unwrap_or(self.entries.len());
                if s >= last_group {
                    break;
                }
                if Some(s) == newest_user || self.entries[s..e].iter().any(|x| x.pinned) {
                    continue;
                }
                victim = Some((s, e));
                break;
            }
            let Some((s, e)) = victim else { break };
            self.entries.drain(s..e);
            dropped += e - s;
        }
        if dropped > 0 {
            // leave a marker so the model knows history was cut
            let note = HistoryEntry::new(
                "trim",
                0,
                EntryKind::Nudge,
                ChatMessage::user("[Older conversation was trimmed to fit the context window. Use recall(query) to search it.]"),
            );
            let mut note = note;
            note.tokens = p.estimator.message(&note.msg);
            self.entries.insert(0, note);
        }
        // Last resort: clip oversized items that remain.
        if self.estimate(p) > target {
            self.truncate_oversized(p, target);
        }
        dropped
    }

    /// Clip the largest non-user items to fit.
    pub fn truncate_oversized(&mut self, p: &PromptInputs, target: u32) {
        let est = p.estimator;
        for _ in 0..8 {
            if self.estimate(p) <= target {
                return;
            }
            let Some((idx, _)) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| !e.pinned)
                .max_by_key(|(_, e)| e.tokens)
            else {
                return;
            };
            let e = &mut self.entries[idx];
            let text = e.text();
            let keep_chars = (text.chars().count() / 3).max(200);
            let clipped = summary::clip_middle(&text, keep_chars);
            if clipped.len() >= text.len() {
                return;
            }
            e.set_text(clipped);
            e.tokens = est.message(&e.msg);
            self.last_exact = None;
        }
    }
}

/// The compactor model, abstracted for testing.
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Max prompt tokens per summarize call.
    fn input_budget(&self) -> u32;
    fn count(&self, text: &str) -> u32;
    /// Return the model's raw JSON text for `schema`.
    async fn summarize(&self, system: &str, user: &str, schema: &Value) -> anyhow::Result<String>;
}

#[derive(Debug, Clone)]
pub struct CompactionOutcome {
    pub record: CompactionRecord,
    pub attempts: u32,
    pub llm: bool,
    pub summarized_entries: usize,
}

/// Tier 2 compaction. `target` is the post-compaction token goal.
#[allow(clippy::too_many_arguments)]
pub async fn compact(
    state: &mut ContextState,
    summarizer: Option<&dyn Summarizer>,
    p: &PromptInputs<'_>,
    s: &ContextSettings,
    window: u32,
    trigger: CompactionTrigger,
    focus: Option<&str>,
    turn_id: Option<String>,
) -> Option<CompactionOutcome> {
    let before = state.estimate(p);
    let target = (window as f64 * s.target_after_compact) as u32;
    let mut keep_recent = (window as f64 * s.keep_recent_ratio) as u32;
    let mut attempts = 0;
    let mut any_llm = false;
    let mut summarized_total = 0;
    while attempts < 2 {
        attempts += 1;
        let Some(split) = state.plan_compaction(keep_recent) else { break };
        let (summarized, kept): (Vec<HistoryEntry>, Vec<HistoryEntry>) = {
            let mut sum = Vec::new();
            let mut kept_pinned = Vec::new();
            for e in state.entries.drain(..split) {
                if e.pinned {
                    kept_pinned.push(e);
                } else {
                    sum.push(e);
                }
            }
            let mut kept = kept_pinned;
            kept.extend(state.entries.drain(..));
            (sum, kept)
        };
        summarized_total += summarized.len();
        let previous = state.summary.clone();
        let plan = p.pinned.plan.clone();
        let (data, llm) = match summarizer {
            Some(sm) => match summarize_entries(sm, previous.as_ref().map(|x| &x.data), &summarized, focus, &state.requirements).await {
                Ok(mut d) => {
                    d.ensure_requirements(&state.requirements);
                    if let Some(prev) = &previous {
                        d.ensure_requirements(&prev.data.goal_and_requirements);
                    }
                    (d, true)
                }
                Err(e) => {
                    tracing::warn!("compactor failed, using extractive summary: {e:#}");
                    (summary::extractive(&summarized, previous.as_ref().map(|x| &x.data), &plan), false)
                }
            },
            None => (summary::extractive(&summarized, previous.as_ref().map(|x| &x.data), &plan), false),
        };
        let mut data = data;
        data.ensure_requirements(&state.requirements);
        any_llm |= llm;
        let number = previous.as_ref().map(|x| x.number + 1).unwrap_or(1);
        state.summary = Some(StoredSummary { number, data, llm });
        state.entries = kept;
        state.last_exact = None;
        if state.estimate(p) <= target {
            break;
        }
        keep_recent /= 2;
    }
    if attempts == 0 || summarized_total == 0 {
        return None;
    }
    if state.estimate(p) > target {
        // shrink oversized kept items before giving up to Tier 3
        state.truncate_oversized(p, target);
    }
    let after = state.estimate(p);
    let record = CompactionRecord {
        summary_number: state.summary.as_ref().map(|x| x.number).unwrap_or(0),
        at: chrono::Utc::now().timestamp_millis(),
        tokens_before: before,
        tokens_after: after,
        trigger,
        llm: any_llm,
        turn_id,
    };
    state.compactions.push(record.clone());
    Some(CompactionOutcome { record, attempts, llm: any_llm, summarized_entries: summarized_total })
}

/// Summarize entries, using rolling map-reduce when they don't fit one call.
async fn summarize_entries(
    sm: &dyn Summarizer,
    previous: Option<&SummaryData>,
    entries: &[HistoryEntry],
    focus: Option<&str>,
    requirements: &[String],
) -> anyhow::Result<SummaryData> {
    let budget = sm.input_budget().max(512);
    let overhead = sm.count(summary::COMPACTOR_SYSTEM) + 300;
    let mut running: Option<SummaryData> = previous.cloned();
    let schema = SummaryData::json_schema();
    // greedy chunking by rendered size
    let mut chunks: Vec<&[HistoryEntry]> = Vec::new();
    let mut start = 0;
    let mut acc = 0u32;
    let per_item_chars = ((budget as usize) * 2).clamp(800, 6000);
    for (i, e) in entries.iter().enumerate() {
        let t = sm.count(&summary::render_transcript(std::slice::from_ref(e), per_item_chars));
        let prev_t = running.as_ref().map(|r| sm.count(&serde_json::to_string(r).unwrap_or_default())).unwrap_or(0);
        let req_t: u32 = requirements.iter().map(|r| sm.count(r)).sum();
        if acc + t + overhead + prev_t + req_t > budget && i > start {
            chunks.push(&entries[start..i]);
            start = i;
            acc = 0;
        }
        acc += t;
    }
    if start < entries.len() {
        chunks.push(&entries[start..]);
    }
    for chunk in chunks {
        let transcript = summary::render_transcript(chunk, per_item_chars);
        let prev_json = running.as_ref().map(|r| serde_json::to_string_pretty(r).unwrap_or_default());
        let user = summary::compactor_user_prompt(prev_json.as_deref(), &transcript, focus, requirements);
        let raw = sm.summarize(summary::COMPACTOR_SYSTEM, &user, &schema).await?;
        let parsed = SummaryData::parse(&raw).ok_or_else(|| anyhow::anyhow!("compactor returned unusable JSON"))?;
        running = Some(parsed);
    }
    running.ok_or_else(|| anyhow::anyhow!("nothing summarized"))
}

#[cfg(test)]
mod tests;
