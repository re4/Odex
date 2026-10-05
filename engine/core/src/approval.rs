//! Approval decisions, user approval requests, and automatic review.

use std::path::{Path, PathBuf};

use serde_json::json;
use tokio_util::sync::CancellationToken;

use odex_execpolicy::{Decision, Evaluation};
use odex_llm::types::{ChatMessage, ChatRequest, StructuredOutput};
use odex_protocol::*;

use crate::engine::Engine;
use crate::thread::{new_id, Denial, PendingApproval, ThreadRt};

/// What to do with a command.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecPlan {
    /// Run in the sandbox (or unsandboxed when `sandbox` is false).
    Run {
        sandbox: bool,
    },
    /// Ask the user; `reason` is a short code, `unsandboxed_if_approved`
    /// says whether approval lifts the sandbox.
    Ask {
        reason: String,
        unsandboxed_if_approved: bool,
    },
    Refuse(String),
}

pub struct ExecCtx<'a> {
    pub mode: PermissionMode,
    pub eval: &'a Evaluation,
    pub escalated: bool,
    pub network_allowed: bool,
    pub session_allows: bool,
    pub sandbox_available: bool,
    pub plan_mode: bool,
    /// Refines `Auto` (PROMPT §5); read-only and full access ignore it.
    pub policy: ApprovalPolicy,
}

/// Map permission mode + exec policy to a plan (PROMPT §5).
pub fn plan_exec(c: &ExecCtx) -> ExecPlan {
    if c.eval.decision == Some(Decision::Forbid) {
        let why = c.eval.justification.clone().unwrap_or_else(|| "it matches a forbidden command rule".into());
        return ExecPlan::Refuse(format!(
            "This command is forbidden by policy: {why}. Ask the user to run it themselves if it is really needed."
        ));
    }
    if c.plan_mode && !c.eval.known_safe {
        return ExecPlan::Refuse(
            "Planning mode is read-only: only read-only commands may run. Describe this step in your plan instead."
                .into(),
        );
    }
    match c.mode {
        PermissionMode::FullAccess => ExecPlan::Run { sandbox: false },
        PermissionMode::ReadOnly => {
            if c.eval.known_safe {
                if c.sandbox_available {
                    ExecPlan::Run { sandbox: true }
                } else {
                    ExecPlan::Run { sandbox: false }
                }
            } else if c.session_allows {
                ExecPlan::Run { sandbox: false }
            } else {
                ExecPlan::Ask { reason: "readOnly".into(), unsandboxed_if_approved: true }
            }
        }
        PermissionMode::Auto => {
            if c.session_allows {
                return ExecPlan::Run {
                    sandbox: !c.escalated && c.sandbox_available && (!c.eval.network_likely || c.network_allowed),
                };
            }
            match c.policy {
                ApprovalPolicy::Never => {
                    // never ask: anything that would need approval goes back to the model
                    if c.eval.decision == Some(Decision::Prompt) {
                        return ExecPlan::Refuse(
                            "This command needs approval under the command rules, but approvals are turned off \
                             (approval_policy = \"never\"). Use another approach or ask the user to run it."
                                .into(),
                        );
                    }
                    if c.escalated {
                        return ExecPlan::Refuse(
                            "Running outside the sandbox needs approval, but approvals are turned off \
                             (approval_policy = \"never\"). Run it without escalation or ask the user to run it."
                                .into(),
                        );
                    }
                    if !c.sandbox_available {
                        return ExecPlan::Refuse(
                            "The sandbox is unavailable and approvals are turned off (approval_policy = \"never\"), \
                             so commands can't run. Ask the user to fix the sandbox or change the approval policy."
                                .into(),
                        );
                    }
                    return ExecPlan::Run { sandbox: true };
                }
                ApprovalPolicy::OnFailure => {
                    // try everything in the sandbox first; a sandbox denial asks to retry outside it
                    if c.eval.decision == Some(Decision::Prompt) {
                        return ExecPlan::Ask { reason: "policy".into(), unsandboxed_if_approved: c.escalated };
                    }
                    if !c.sandbox_available {
                        return ExecPlan::Ask { reason: "sandboxUnavailable".into(), unsandboxed_if_approved: true };
                    }
                    return ExecPlan::Run { sandbox: true };
                }
                ApprovalPolicy::Untrusted => {
                    if !c.eval.known_safe && c.eval.decision != Some(Decision::Allow) {
                        let outside =
                            c.escalated || (c.eval.network_likely && !c.network_allowed) || !c.sandbox_available;
                        return ExecPlan::Ask { reason: "untrusted".into(), unsandboxed_if_approved: outside };
                    }
                }
                ApprovalPolicy::OnRequest => {}
            }
            if c.eval.decision == Some(Decision::Prompt) {
                return ExecPlan::Ask { reason: "policy".into(), unsandboxed_if_approved: c.escalated };
            }
            if c.escalated {
                return ExecPlan::Ask { reason: "escalation".into(), unsandboxed_if_approved: true };
            }
            if c.eval.network_likely && !c.network_allowed {
                return ExecPlan::Ask { reason: "network".into(), unsandboxed_if_approved: true };
            }
            if !c.sandbox_available {
                return ExecPlan::Ask { reason: "sandboxUnavailable".into(), unsandboxed_if_approved: true };
            }
            if c.eval.decision == Some(Decision::Allow) {
                return ExecPlan::Run { sandbox: true };
            }
            ExecPlan::Run { sandbox: true }
        }
    }
}

/// Is `path` inside one of the writable roots (and not inside `.git`)?
pub fn within_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let norm = |p: &Path| odex_config::normalize_path(p);
    // canonicalize the existing parent for new files
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => break,
        }
    }
    let rest = path.strip_prefix(&probe).map(|r| r.to_path_buf()).unwrap_or_default();
    let full = PathBuf::from(norm(&probe)).join(rest);
    let sep = if cfg!(windows) { '\\' } else { '/' };
    let mut full_s = full.to_string_lossy().to_string();
    if cfg!(windows) {
        full_s = full_s.replace('/', "\\").to_lowercase();
    }
    if full_s.contains(&format!("{sep}.git{sep}")) || full_s.ends_with(&format!("{sep}.git")) {
        return false;
    }
    roots.iter().any(|r| {
        let rs = norm(r);
        full_s == rs || full_s.starts_with(&format!("{rs}{sep}"))
    })
}

/// Ask the user (or the automatic reviewer) to approve something.
pub async fn request(
    engine: &Engine,
    rt: &ThreadRt,
    turn_id: &str,
    item_id: Option<String>,
    kind: ApprovalKind,
    cancel: &CancellationToken,
) -> ApprovalDecision {
    let t = rt.thread();
    let settings = engine.thread_settings(&t);
    let signature = approval_signature(&kind);

    // `/approve` override of a previous automatic-review denial
    if rt.override_next_denial.swap(false, std::sync::atomic::Ordering::SeqCst) {
        let mut d = rt.denials.lock().unwrap();
        if let Some(pos) = d.iter().position(|x| x.signature == signature) {
            d.remove(pos);
            return ApprovalDecision::Approve;
        }
        if !d.is_empty() {
            d.pop();
            return ApprovalDecision::Approve;
        }
    }

    let mut verdict: Option<AutoReviewVerdict> = None;
    if settings.auto_review {
        let v = auto_review(engine, rt, &kind, settings.auto_review_rubric.as_deref()).await;
        match v.decision.as_str() {
            "allow" => {
                crate::turn::notice(
                    engine,
                    rt,
                    turn_id,
                    NoticeLevel::Info,
                    format!("Automatic review approved ({} risk): {}", v.risk, v.reason),
                    Some("autoReview"),
                );
                return ApprovalDecision::Approve;
            }
            "deny" => {
                rt.denials.lock().unwrap().push(Denial { signature, reason: v.reason.clone() });
                crate::turn::notice(
                    engine,
                    rt,
                    turn_id,
                    NoticeLevel::Warning,
                    format!("Automatic review denied ({} risk): {}. Use /approve to allow it once.", v.risk, v.reason),
                    Some("autoReviewDenied"),
                );
                return ApprovalDecision::Deny {
                    feedback: Some(format!("Automatic review denied this action: {}", v.reason)),
                };
            }
            _ => verdict = Some(v),
        }
    }

    let approval_id = new_id("appr");
    let params = ApprovalRequestParams {
        approval_id: approval_id.clone(),
        thread_id: rt.id.clone(),
        turn_id: turn_id.to_string(),
        item_id,
        approval: kind,
        auto_review: verdict,
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    rt.pending_approvals
        .lock()
        .unwrap()
        .insert(approval_id.clone(), PendingApproval { params: params.clone(), reply: Some(tx) });
    engine.set_status(rt, ThreadStatus::WaitingApproval);
    let em = engine.emitter();
    let req_fut = em.request(server_request::APPROVAL_REQUEST, &params);
    let decision = tokio::select! {
        r = req_fut => match r.and_then(|v| Ok(serde_json::from_value::<ApprovalResponse>(v)?)) {
            Ok(resp) => resp.decision,
            Err(e) => {
                tracing::warn!("approval request failed: {e:#}");
                ApprovalDecision::Deny { feedback: Some(format!("Approval could not be obtained ({e}); the action was not run.")) }
            }
        },
        r = rx => r.unwrap_or(ApprovalDecision::Abort),
        _ = cancel.cancelled() => ApprovalDecision::Abort,
    };
    rt.pending_approvals.lock().unwrap().remove(&approval_id);
    em.approval_resolved(&rt.id, &approval_id);
    if rt.is_running() {
        engine.set_status(rt, ThreadStatus::Running);
    }
    decision
}

fn approval_signature(k: &ApprovalKind) -> String {
    match k {
        ApprovalKind::Exec { command, .. } => format!("exec:{command}"),
        ApprovalKind::Patch { changes, .. } => {
            format!("patch:{}", changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>().join(","))
        }
        ApprovalKind::Mcp { server, tool, .. } => format!("mcp:{server}:{tool}"),
        ApprovalKind::ComputerUse { app, action, .. } => format!("cu:{app}:{action}"),
        ApprovalKind::Browser { site, action, .. } => format!("browser:{site}:{action}"),
        ApprovalKind::Download { url, .. } => format!("download:{url}"),
        ApprovalKind::Hook { hook_id, .. } => format!("hook:{hook_id}"),
    }
}

const REVIEW_RUBRIC: &str = "You are a security reviewer for a coding agent. Decide whether the agent's requested action is safe to perform \
without asking the user, given the user's goal. Risk rubric:\n\
- low: read-only, reversible, local changes inside the project, standard dev commands (build, test, install from a lockfile).\n\
- medium: network access to well-known package registries or docs, writes outside the project to caches/tool dirs.\n\
- high: deleting or overwriting data outside the project, credentials/secrets, publishing/pushing, system configuration, \
downloading and executing scripts, anything unrelated to the user's goal.\n\
Decide `allow` for low risk that clearly serves the goal, `deny` for high risk or actions unrelated to the goal, and `askUser` when unsure. Respond in JSON.";

/// Ask the reviewer model for a verdict.
pub async fn auto_review(
    engine: &Engine,
    rt: &ThreadRt,
    kind: &ApprovalKind,
    rubric: Option<&str>,
) -> AutoReviewVerdict {
    let t = rt.thread();
    let ask =
        |reason: &str| AutoReviewVerdict { decision: "askUser".into(), risk: "unknown".into(), reason: reason.into() };
    let Some(h) = engine.role_model(ModelRole::Reviewer, Some(&t)) else { return ask("no reviewer model configured") };
    let goal = rt.original_task.lock().unwrap().clone().unwrap_or_default();
    let action = serde_json::to_string_pretty(kind).unwrap_or_default();
    let mut system = REVIEW_RUBRIC.to_string();
    if let Some(r) = rubric {
        system.push_str("\nAdditional rules from the user:\n");
        system.push_str(r);
    }
    let schema = json!({
        "type": "object",
        "properties": {
            "decision": {"type": "string", "enum": ["allow", "deny", "askUser"]},
            "risk": {"type": "string", "enum": ["low", "medium", "high"]},
            "reason": {"type": "string"}
        },
        "required": ["decision", "risk", "reason"]
    });
    let req = ChatRequest {
        messages: vec![
            ChatMessage::system(system),
            ChatMessage::user(format!(
                "User's goal:\n{}\n\nWorking directory: {}\nPermission mode: {}\n\nRequested action:\n{action}",
                goal.chars().take(3000).collect::<String>(),
                t.cwd,
                t.permission_mode.as_str()
            )),
        ],
        max_tokens: Some(400),
        structured: Some(StructuredOutput { name: "approval_verdict".into(), schema }),
        effort: Some(ReasoningEffort::None),
        ..Default::default()
    };
    match h.client.chat(&h.model, &req, &CancellationToken::new()).await {
        Ok(r) => match odex_llm::repair::parse_lenient(&r.content) {
            Ok((v, _)) => {
                let decision = v.get("decision").and_then(|x| x.as_str()).unwrap_or("askUser").to_string();
                let decision =
                    if ["allow", "deny", "askUser"].contains(&decision.as_str()) { decision } else { "askUser".into() };
                AutoReviewVerdict {
                    decision,
                    risk: v.get("risk").and_then(|x| x.as_str()).unwrap_or("medium").into(),
                    reason: v.get("reason").and_then(|x| x.as_str()).unwrap_or("").into(),
                }
            }
            Err(_) => ask("reviewer returned an unreadable verdict"),
        },
        Err(e) => ask(&format!("reviewer unavailable: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(decision: Option<Decision>, safe: bool, net: bool) -> Evaluation {
        Evaluation {
            decision,
            matched: vec![],
            commands: Some(vec![vec!["x".into()]]),
            known_safe: safe,
            writes_likely: false,
            network_likely: net,
            justification: None,
        }
    }

    fn ctx<'a>(mode: PermissionMode, e: &'a Evaluation) -> ExecCtx<'a> {
        ExecCtx {
            mode,
            eval: e,
            escalated: false,
            network_allowed: false,
            session_allows: false,
            sandbox_available: true,
            plan_mode: false,
            policy: ApprovalPolicy::OnRequest,
        }
    }

    #[test]
    fn modes() {
        let safe = eval(None, true, false);
        let unsafe_ = eval(None, false, false);
        let net = eval(None, false, true);
        let forbid = eval(Some(Decision::Forbid), false, false);
        let prompt = eval(Some(Decision::Prompt), false, false);
        assert_eq!(plan_exec(&ctx(PermissionMode::ReadOnly, &safe)), ExecPlan::Run { sandbox: true });
        assert!(matches!(plan_exec(&ctx(PermissionMode::ReadOnly, &unsafe_)), ExecPlan::Ask { .. }));
        assert_eq!(plan_exec(&ctx(PermissionMode::Auto, &unsafe_)), ExecPlan::Run { sandbox: true });
        assert!(
            matches!(plan_exec(&ctx(PermissionMode::Auto, &net)), ExecPlan::Ask { ref reason, .. } if reason == "network")
        );
        assert!(
            matches!(plan_exec(&ctx(PermissionMode::Auto, &prompt)), ExecPlan::Ask { ref reason, .. } if reason == "policy")
        );
        assert!(matches!(plan_exec(&ctx(PermissionMode::FullAccess, &forbid)), ExecPlan::Refuse(_)));
        assert_eq!(plan_exec(&ctx(PermissionMode::FullAccess, &net)), ExecPlan::Run { sandbox: false });
        let mut c = ctx(PermissionMode::Auto, &unsafe_);
        c.escalated = true;
        assert!(matches!(plan_exec(&c), ExecPlan::Ask { ref reason, .. } if reason == "escalation"));
        c.session_allows = true;
        assert_eq!(plan_exec(&c), ExecPlan::Run { sandbox: false });
        let mut c = ctx(PermissionMode::Auto, &unsafe_);
        c.sandbox_available = false;
        assert!(matches!(plan_exec(&c), ExecPlan::Ask { ref reason, .. } if reason == "sandboxUnavailable"));
    }

    #[test]
    fn approval_policies_refine_auto() {
        let safe = eval(None, true, false);
        let unsafe_ = eval(None, false, false);
        let net = eval(None, false, true);
        let prompt = eval(Some(Decision::Prompt), false, false);
        let with = |e, p: ApprovalPolicy| {
            let mut c = ctx(PermissionMode::Auto, e);
            c.policy = p;
            plan_exec(&c)
        };
        // untrusted: only known-safe commands run without asking
        assert_eq!(with(&safe, ApprovalPolicy::Untrusted), ExecPlan::Run { sandbox: true });
        assert!(
            matches!(with(&unsafe_, ApprovalPolicy::Untrusted), ExecPlan::Ask { ref reason, unsandboxed_if_approved: false } if reason == "untrusted")
        );
        assert!(matches!(with(&net, ApprovalPolicy::Untrusted), ExecPlan::Ask { unsandboxed_if_approved: true, .. }));
        // on-failure: network/escalation run sandboxed first
        assert_eq!(with(&net, ApprovalPolicy::OnFailure), ExecPlan::Run { sandbox: true });
        let mut c = ctx(PermissionMode::Auto, &unsafe_);
        c.policy = ApprovalPolicy::OnFailure;
        c.escalated = true;
        assert_eq!(plan_exec(&c), ExecPlan::Run { sandbox: true });
        assert!(matches!(with(&prompt, ApprovalPolicy::OnFailure), ExecPlan::Ask { .. }));
        // never: no asks at all
        assert_eq!(with(&net, ApprovalPolicy::Never), ExecPlan::Run { sandbox: true });
        assert!(matches!(with(&prompt, ApprovalPolicy::Never), ExecPlan::Refuse(_)));
        c.policy = ApprovalPolicy::Never;
        assert!(matches!(plan_exec(&c), ExecPlan::Refuse(_)));
        // read-only and full access are unaffected
        let mut c = ctx(PermissionMode::FullAccess, &unsafe_);
        c.policy = ApprovalPolicy::Untrusted;
        assert_eq!(plan_exec(&c), ExecPlan::Run { sandbox: false });
    }

    #[test]
    fn roots() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let roots = vec![root.clone()];
        assert!(within_roots(&root.join("src/new.rs"), &roots));
        assert!(within_roots(&root.join("a/b/c.txt"), &roots));
        assert!(!within_roots(&d.path().join("other.txt"), &roots));
        assert!(!within_roots(&root.join(".git/config"), &roots));
    }
}
