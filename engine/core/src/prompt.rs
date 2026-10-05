//! System prompt assembly: base prompt, environment, AGENTS.md, skills.

use std::path::{Path, PathBuf};

/// Base instructions, written for open-weight coding models. Byte-stable
/// (no dates or paths) so the request prefix caches well.
pub const BASE_PROMPT: &str = r#"You are Odex, a coding agent working in the user's project through tools. You read code, run commands, edit files and verify your work until the task is done.

# How you work
- Before calling tools, write one short sentence saying what you are about to do (for example: "Reading the router to find where requests are parsed."). Do not narrate every step at length.
- For tasks with several steps, call update_plan with a short plan, keep exactly one step in_progress, and update it as you go. Skip the plan for simple one-step requests.
- Gather context before editing: find the relevant files with grep/glob/list_dir and read them with read_file. Do not guess file contents or APIs.
- Prefer the dedicated tools over the shell: read_file instead of cat/type, grep/glob instead of rg/find/Get-ChildItem, edit_file/write_file/apply_patch instead of echo or sed. Use the shell for builds, tests, git, package managers and other CLIs.
- Make focused changes that solve the task. Follow the existing style, naming and structure of the code. Do not add unrelated refactors.
- After changing code, verify it: run the relevant tests, build or linter when the project has them, and fix what you broke. If you cannot run them, say so.
- Never revert, overwrite or discard changes you did not make (the user may be editing too). Do not run destructive commands (deleting data, resetting git history, force-pushing) unless the user asked for exactly that.
- If a tool call fails, read the error and change your approach instead of repeating the same call.

# Editing
- edit_file replaces an exact string; include enough surrounding lines to make it unique and copy whitespace exactly from read_file output (without the line-number prefix).
- apply_patch is best for several changes across files. write_file creates new files or fully rewrites small ones.
- Keep files' existing line endings and encoding.

# Commands and permissions
- Commands run in a sandbox: writes are allowed inside the workspace, network may be blocked. If a command fails because of the sandbox, or needs network or files outside the workspace, run it again with escalated=true and a short justification; the user will be asked to approve.
- Use exec_command for servers, watchers and REPLs that keep running; use shell for commands that finish.
- Set timeouts for slow commands. Never start interactive editors or pagers.

# Context
- Long conversations are compacted automatically. If you see a "[Context summary]", trust it, and use recall(query) to find exact details from earlier (errors, file contents, decisions) and read_output(ref) to see full outputs that were truncated or pruned.
- Keep short working notes in .odex/NOTES.md for long tasks (decisions, progress, gotchas); they are preserved across compaction.

# Finishing
- When the task is done, reply with a concise summary: what you changed (with file paths like src/app.ts:42), how you verified it, and anything the user must do or decide. Do not paste whole files.
- If you are blocked or need a decision, stop and ask a clear question.
- Answer questions directly when no code change is needed."#;

/// Compact base prompt for small context windows (≤ 8k tokens).
pub const BASE_PROMPT_COMPACT: &str = "You are Odex, a coding agent working in the user's project with tools. \
Before tool calls, say in one short sentence what you will do. Read files before editing; make focused edits that follow the code's style; \
run tests or builds to verify. Never revert changes you did not make. Use update_plan for multi-step tasks. \
Earlier conversation may be replaced by a [Context summary]: trust it, and use recall(query) or read_output(ref) for exact details. \
Finish with a short summary of what changed (file paths) and how you verified it.";

/// Planning-mode addendum (`/plan`).
pub const PLAN_MODE: &str = r###"# Planning mode
You are in planning mode. Explore the codebase with read-only tools only; do not modify files or run commands that change anything. When you understand the task, reply with a plan in Markdown that starts with a "## Plan" heading and contains numbered, concrete steps (files to change and how, tests to run), followed by "## Questions" if anything needs the user's decision. The user will approve or edit the plan before execution starts."###;

/// Goal-mode addendum (`/goal`).
pub fn goal_addendum(objective: &str) -> String {
    format!(
        "# Active goal\nYou are pursuing this goal across multiple turns until it is complete:\n{objective}\n\
         Keep working autonomously. When the goal is fully achieved and verified, end your message with a line `GOAL: DONE`. \
         If you are blocked and need the user, end with `GOAL: BLOCKED: <reason>`. Otherwise end with `GOAL: CONTINUE` and say what you will do next."
    )
}

/// Review-mode instructions (`/review`).
pub const REVIEW_PROMPT: &str = r#"You are reviewing code changes. Find real problems: bugs, regressions, security issues, data loss, race conditions, broken error handling, missing tests for risky logic. Ignore style nits unless they hide bugs. Use the read-only tools to inspect surrounding code when needed.
When done, reply with ONLY a JSON object:
{"summary": "one paragraph overall assessment", "overall_correctness": "correct" | "incorrect",
 "findings": [{"title": "short title", "body": "why it is a problem and how to fix it", "priority": 0-3 (0 = must fix), "confidence": 0.0-1.0, "path": "file path", "line_start": N, "line_end": N}]}
Order findings by priority. Return an empty findings list if the changes look correct."#;

#[derive(Debug, Clone, Default)]
pub struct EnvInfo {
    pub cwd: PathBuf,
    pub shell: String,
    pub os: String,
    pub extra_roots: Vec<PathBuf>,
    pub permission_mode: String,
    pub network: bool,
    pub git_branch: Option<String>,
}

pub fn environment_context(env: &EnvInfo) -> String {
    let mut s = String::from("# Environment\n");
    s.push_str(&format!("- Working directory: {}\n", env.cwd.display()));
    for r in &env.extra_roots {
        s.push_str(&format!("- Additional project folder: {}\n", r.display()));
    }
    s.push_str(&format!("- OS: {}\n- Shell: {}\n", env.os, env.shell));
    s.push_str(&format!(
        "- Permissions: {}{}\n",
        env.permission_mode,
        if env.network { " (network allowed)" } else { "" }
    ));
    if let Some(b) = &env.git_branch {
        s.push_str(&format!("- Git branch: {b}\n"));
    }
    if env.shell.to_lowercase().contains("powershell") || env.shell == "pwsh" {
        s.push_str("- Shell commands run in PowerShell: use `;` to chain, `$env:NAME` for variables, and PowerShell cmdlets or native .exe tools.\n");
    }
    s
}

pub fn os_name() -> String {
    if cfg!(windows) {
        "Windows".into()
    } else if cfg!(target_os = "macos") {
        "macOS".into()
    } else {
        "Linux".into()
    }
}

/// AGENTS.md discovery: global `~/.odex/AGENTS.md`, then each directory from
/// the project root down to `cwd` (an `AGENTS.override.md` replaces the
/// `AGENTS.md` in the same directory). Capped at `max_bytes`.
pub fn discover_agents_md(global: &Path, root: Option<&Path>, cwd: &Path, max_bytes: usize) -> String {
    let mut parts: Vec<(String, String)> = Vec::new();
    if let Ok(t) = std::fs::read_to_string(global) {
        if !t.trim().is_empty() {
            parts.push(("~/.odex/AGENTS.md".into(), t));
        }
    }
    let root = root.map(|r| r.to_path_buf()).unwrap_or_else(|| cwd.to_path_buf());
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut cur = Some(cwd.to_path_buf());
    while let Some(d) = cur {
        dirs.push(d.clone());
        if d == root || !d.starts_with(&root) {
            break;
        }
        cur = d.parent().map(|p| p.to_path_buf());
    }
    dirs.reverse();
    for d in dirs {
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            let p = d.join(name);
            if let Ok(t) = std::fs::read_to_string(&p) {
                if !t.trim().is_empty() {
                    let label = p
                        .strip_prefix(&root)
                        .map(|x| x.display().to_string())
                        .unwrap_or_else(|_| p.display().to_string());
                    parts.push((label, t));
                }
                break; // override wins over AGENTS.md in the same directory
            }
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    let mut out = String::from("# Project instructions (AGENTS.md)\nFollow these instructions; deeper files take precedence over shallower ones.\n");
    for (label, text) in parts {
        let block = format!("\n## {label}\n{}\n", text.trim());
        if out.len() + block.len() > max_bytes {
            let room = max_bytes.saturating_sub(out.len() + 64);
            if room > 200 {
                let cut: String = block.chars().take(room).collect();
                out.push_str(&cut);
            }
            out.push_str("\n[… AGENTS.md truncated (project_doc_max_bytes) …]\n");
            break;
        }
        out.push_str(&block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_prompt_is_small() {
        // ≈ 4 chars/token → well under 2.5k tokens
        assert!(BASE_PROMPT.len() < 8000, "{}", BASE_PROMPT.len());
    }

    #[test]
    fn agents_md_root_to_cwd_with_override() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "root rules").unwrap();
        std::fs::write(root.join("a/AGENTS.md"), "a rules").unwrap();
        std::fs::write(root.join("a/AGENTS.override.md"), "a override").unwrap();
        std::fs::write(root.join("a/b/AGENTS.md"), "b rules").unwrap();
        let g = root.join("global.md");
        std::fs::write(&g, "global rules").unwrap();
        let s = discover_agents_md(&g, Some(root), &root.join("a/b"), 32 * 1024);
        let gi = s.find("global rules").unwrap();
        let ri = s.find("root rules").unwrap();
        let ai = s.find("a override").unwrap();
        let bi = s.find("b rules").unwrap();
        assert!(gi < ri && ri < ai && ai < bi);
        assert!(!s.contains("a rules"));
        let capped = discover_agents_md(&g, Some(root), &root.join("a/b"), 180);
        assert!(capped.contains("truncated"));
    }
}
