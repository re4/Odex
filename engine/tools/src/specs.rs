//! Tool schemas shown to the model. Order and serialization are
//! deterministic so the request prefix stays byte-stable (prefix caching).

use serde_json::{json, Value};

use odex_llm::types::ToolSpec;
use odex_protocol::ToolProfile;

fn spec(name: &str, description: &str, parameters: Value) -> ToolSpec {
    ToolSpec { name: name.into(), description: description.into(), parameters }
}

pub fn shell(windows: bool) -> ToolSpec {
    let shell_name = if windows { "PowerShell" } else { "bash" };
    spec(
        "shell",
        &format!(
            "Run a {shell_name} command and return its output. Use for builds, tests, git and other CLI tools. \
             Prefer read_file/grep/glob/list_dir for reading and edit_file/write_file/apply_patch for editing. \
             Commands run in a sandbox that allows writes only inside the workspace and may block network access; \
             if a command needs more (network, writing elsewhere), set escalated=true with a one-sentence justification."
        ),
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": format!("The {shell_name} command line to run.")},
                "workdir": {"type": "string", "description": "Working directory (default: the thread's cwd)."},
                "timeout_ms": {"type": "integer", "description": "Timeout in milliseconds (default 120000)."},
                "escalated": {"type": "boolean", "description": "Run outside the sandbox (requires user approval)."},
                "justification": {"type": "string", "description": "Why escalation is needed (shown to the user)."}
            },
            "required": ["command"]
        }),
    )
}

pub fn exec_command() -> ToolSpec {
    spec(
        "exec_command",
        "Start a long-running or interactive process (dev server, REPL, watcher) in a terminal session. \
         Returns the first output and a session_id; use write_stdin to send input or read more output.",
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "workdir": {"type": "string"},
                "yield_ms": {"type": "integer", "description": "How long to wait for initial output (default 2000)."}
            },
            "required": ["command"]
        }),
    )
}

pub fn write_stdin() -> ToolSpec {
    spec(
        "write_stdin",
        "Send input to an exec_command session and return new output. Send empty chars to just poll output. Set kill=true to stop it.",
        json!({
            "type": "object",
            "properties": {
                "session_id": {"type": "string"},
                "chars": {"type": "string", "description": "Text to send; include \\n to press Enter."},
                "yield_ms": {"type": "integer", "description": "How long to wait for output (default 1000)."},
                "kill": {"type": "boolean"}
            },
            "required": ["session_id"]
        }),
    )
}

pub fn apply_patch() -> ToolSpec {
    spec(
        "apply_patch",
        "Edit files with a patch. Format:\n*** Begin Patch\n*** Add File: path\n+new line\n*** Update File: path\n@@ optional context line\n context\n-old\n+new\n*** Delete File: path\n*** End Patch\n\
         Include 3 lines of unchanged context around each change. Paths are relative to the working directory.",
        json!({
            "type": "object",
            "properties": {"patch": {"type": "string", "description": "The full patch text."}},
            "required": ["patch"]
        }),
    )
}

pub fn update_plan() -> ToolSpec {
    spec(
        "update_plan",
        "Create or update the task plan shown to the user. Use for multi-step work; keep exactly one step in_progress.",
        json!({
            "type": "object",
            "properties": {
                "explanation": {"type": "string"},
                "plan": {"type": "array", "items": {"type": "object", "properties": {
                    "step": {"type": "string"},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                }, "required": ["step", "status"]}}
            },
            "required": ["plan"]
        }),
    )
}

pub fn view_image() -> ToolSpec {
    spec(
        "view_image",
        "Look at an image file from the workspace (screenshots, diagrams, UI output).",
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    )
}

pub fn read_file() -> ToolSpec {
    spec(
        "read_file",
        "Read a text file with line numbers. Large files are paged: pass offset (1-based line) and limit to read more.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "description": "First line to read (1-based)."},
                "limit": {"type": "integer", "description": "Maximum lines to read."}
            },
            "required": ["path"]
        }),
    )
}

pub fn list_dir() -> ToolSpec {
    spec(
        "list_dir",
        "List a directory as a tree (respects .gitignore; skips node_modules, target, .git).",
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "depth": {"type": "integer", "description": "Depth (default 2)."}}
        }),
    )
}

pub fn grep() -> ToolSpec {
    spec(
        "grep",
        "Search file contents with a regular expression (ripgrep semantics, respects .gitignore).",
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "File or directory (default: cwd)."},
                "glob": {"type": "string", "description": "Only files matching this glob, e.g. \"*.rs\"."},
                "case_insensitive": {"type": "boolean"},
                "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"]},
                "context": {"type": "integer", "description": "Lines of context around matches."},
                "max_results": {"type": "integer"}
            },
            "required": ["pattern"]
        }),
    )
}

pub fn glob() -> ToolSpec {
    spec(
        "glob",
        "Find files by glob pattern (e.g. \"src/**/*.ts\"), newest first.",
        json!({
            "type": "object",
            "properties": {"pattern": {"type": "string"}, "path": {"type": "string"}},
            "required": ["pattern"]
        }),
    )
}

pub fn edit_file() -> ToolSpec {
    spec(
        "edit_file",
        "Replace exact text in a file. old_string must match exactly once (include enough context) unless replace_all is true. \
         An empty old_string creates the file with new_string.",
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean"}
            },
            "required": ["path", "old_string", "new_string"]
        }),
    )
}

pub fn write_file() -> ToolSpec {
    spec(
        "write_file",
        "Create or overwrite a file with the given content.",
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "content": {"type": "string"}},
            "required": ["path", "content"]
        }),
    )
}

pub fn read_output() -> ToolSpec {
    spec(
        "read_output",
        "Read a stored full tool output by its ref (e.g. \"ref:out_17\") when a result was truncated or pruned.",
        json!({
            "type": "object",
            "properties": {
                "ref": {"type": "string"},
                "offset": {"type": "integer", "description": "First line (1-based)."},
                "limit": {"type": "integer"}
            },
            "required": ["ref"]
        }),
    )
}

pub fn recall() -> ToolSpec {
    spec(
        "recall",
        "Search this thread's earlier history (including parts removed by context compaction) and return exact snippets.",
        json!({
            "type": "object",
            "properties": {"query": {"type": "string"}, "limit": {"type": "integer"}},
            "required": ["query"]
        }),
    )
}

pub fn read_terminal() -> ToolSpec {
    spec(
        "read_terminal",
        "Read recent output from the user's integrated terminal for this thread (e.g. a dev server log or a failed build).",
        json!({
            "type": "object",
            "properties": {"terminal_id": {"type": "string"}, "lines": {"type": "integer"}}
        }),
    )
}

pub fn spawn_agent() -> ToolSpec {
    spec(
        "spawn_agent",
        "Start a subagent on a self-contained task (exploration, research, an isolated change). It runs in parallel; \
         only its final report comes back to you. Use read_only mode for investigation. Returns an agent id.",
        json!({
            "type": "object",
            "properties": {
                "task": {"type": "string", "description": "Complete instructions; the subagent can't see this conversation."},
                "mode": {"type": "string", "enum": ["read_only", "write"]},
                "worktree": {"type": "boolean", "description": "Run in a separate git worktree (write mode)."}
            },
            "required": ["task"]
        }),
    )
}

/// `generate_image`: parameters follow the ComfyUI workflow's placeholders
/// (`None` when the workflow couldn't be read).
pub fn generate_image(placeholders: Option<&[String]>) -> ToolSpec {
    generation(
        "generate_image",
        "Generate an image with the user's ComfyUI workflow and save it in the workspace \
         (default generated/<prompt words>.png). Write a detailed visual prompt: subject, style, composition, lighting.",
        placeholders,
        true,
    )
}

/// `generate_3d`: like [`generate_image`], for a 3D-model workflow.
pub fn generate_3d(placeholders: Option<&[String]>) -> ToolSpec {
    generation(
        "generate_3d",
        "Generate a 3D model (usually .glb) with the user's ComfyUI workflow and save it in the workspace \
         (default generated/<name>.glb). Image-to-3D workflows take `image`, a workspace image path; \
         you can make one with generate_image first.",
        placeholders,
        false,
    )
}

fn generation(name: &str, description: &str, placeholders: Option<&[String]>, image_tool: bool) -> ToolSpec {
    let known = |p: &str| placeholders.is_some_and(|ph| ph.iter().any(|x| x == p));
    let unknown = placeholders.is_none();
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    if image_tool || unknown || known("prompt") {
        props.insert("prompt".into(), json!({"type": "string", "description": "What to generate."}));
        if image_tool || known("prompt") {
            required.push("prompt");
        }
    }
    if known("negative_prompt") || unknown {
        props.insert("negative_prompt".into(), json!({"type": "string", "description": "What to avoid."}));
    }
    for dim in ["width", "height"] {
        if known(dim) || (unknown && image_tool) {
            props.insert(dim.into(), json!({"type": "integer", "description": "Pixels (default 1024)."}));
        }
    }
    if known("image") || (unknown && !image_tool) {
        props.insert("image".into(), json!({"type": "string", "description": "Workspace path of the input image."}));
        if known("image") {
            required.push("image");
        }
    }
    props.insert(
        "seed".into(),
        json!({"type": "integer", "description": "Fixed seed for a repeatable result; random when omitted."}),
    );
    props.insert(
        "path".into(),
        json!({"type": "string", "description": "Where to save in the workspace, e.g. assets/hero; the extension follows the output."}),
    );
    spec(name, description, json!({"type": "object", "properties": props, "required": required}))
}

pub fn wait_agents() -> ToolSpec {
    spec(
        "wait_agents",
        "Wait for subagents to finish and return their final reports.",
        json!({
            "type": "object",
            "properties": {
                "ids": {"type": "array", "items": {"type": "string"}, "description": "Agent ids (default: all running)."},
                "timeout_ms": {"type": "integer"}
            }
        }),
    )
}

pub fn search_tools() -> ToolSpec {
    spec(
        "search_tools",
        "Find additional MCP tools by keyword. Matching tools become callable on your next step.",
        json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}),
    )
}

/// Windows at or below this size get the compact prompt and toolset.
pub const SMALL_WINDOW: u32 = 8192;

/// Short-description toolset for small context windows (≤ [`SMALL_WINDOW`]):
/// shell, read, edit, write, plan, plus recall/read_output for the context engine.
pub fn compact(windows: bool) -> Vec<ToolSpec> {
    let shell_name = if windows { "PowerShell" } else { "bash" };
    vec![
        spec(
            "shell",
            &format!("Run a {shell_name} command (sandboxed; set escalated=true to ask for more access)."),
            json!({"type": "object", "properties": {
                "command": {"type": "string"}, "workdir": {"type": "string"}, "timeout_ms": {"type": "integer"},
                "escalated": {"type": "boolean"}, "justification": {"type": "string"}
            }, "required": ["command"]}),
        ),
        spec(
            "read_file",
            "Read a file (numbered lines; use offset/limit for more).",
            json!({"type": "object", "properties": {"path": {"type": "string"}, "offset": {"type": "integer"}, "limit": {"type": "integer"}}, "required": ["path"]}),
        ),
        spec(
            "edit_file",
            "Replace exact text old_string with new_string (empty old_string creates the file).",
            json!({"type": "object", "properties": {"path": {"type": "string"}, "old_string": {"type": "string"}, "new_string": {"type": "string"}, "replace_all": {"type": "boolean"}}, "required": ["path", "old_string", "new_string"]}),
        ),
        spec(
            "write_file",
            "Create or overwrite a file.",
            json!({"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}}, "required": ["path", "content"]}),
        ),
        spec(
            "update_plan",
            "Set the task plan (steps with status pending/in_progress/completed).",
            json!({"type": "object", "properties": {"plan": {"type": "array", "items": {"type": "object", "properties": {
                "step": {"type": "string"}, "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}}, "required": ["step", "status"]}}}, "required": ["plan"]}),
        ),
        spec(
            "recall",
            "Search earlier (compacted) history for exact details.",
            json!({"type": "object", "properties": {"query": {"type": "string"}}, "required": ["query"]}),
        ),
        spec(
            "read_output",
            "Read a stored full output by ref (ref:out_N).",
            json!({"type": "object", "properties": {"ref": {"type": "string"}, "offset": {"type": "integer"}, "limit": {"type": "integer"}}, "required": ["ref"]}),
        ),
    ]
}

/// Built-in tools for a profile, in a fixed order.
pub fn builtin(profile: ToolProfile, windows: bool) -> Vec<ToolSpec> {
    match profile {
        ToolProfile::Codex => {
            vec![shell(windows), exec_command(), write_stdin(), apply_patch(), update_plan(), view_image()]
        }
        ToolProfile::Minimal => vec![shell(windows), read_file(), edit_file(), update_plan()],
        ToolProfile::Extended => vec![
            shell(windows),
            exec_command(),
            write_stdin(),
            apply_patch(),
            update_plan(),
            view_image(),
            read_file(),
            list_dir(),
            grep(),
            glob(),
            edit_file(),
            write_file(),
            read_output(),
            recall(),
        ],
    }
}

/// Tools that only read (safe in read-only mode, parallelizable).
pub fn is_read_only(name: &str) -> bool {
    matches!(
        name,
        "read_file"
            | "list_dir"
            | "grep"
            | "glob"
            | "read_output"
            | "recall"
            | "view_image"
            | "update_plan"
            | "read_terminal"
            | "wait_agents"
            | "search_tools"
            | "browser_snapshot"
            | "browser_screenshot"
            | "browser_console"
            | "browser_network"
            | "screenshot"
            | "ui_tree"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles() {
        let names = |p| builtin(p, true).into_iter().map(|t| t.name).collect::<Vec<_>>();
        assert_eq!(names(ToolProfile::Minimal), vec!["shell", "read_file", "edit_file", "update_plan"]);
        assert_eq!(names(ToolProfile::Codex).len(), 6);
        assert_eq!(names(ToolProfile::Extended).len(), 14);
        // deterministic serialization
        let a = serde_json::to_string(
            &builtin(ToolProfile::Extended, true).iter().map(|t| t.to_wire()).collect::<Vec<_>>(),
        )
        .unwrap();
        let b = serde_json::to_string(
            &builtin(ToolProfile::Extended, true).iter().map(|t| t.to_wire()).collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn generation_params_follow_placeholders() {
        let props = |t: ToolSpec| {
            let p = t.parameters["properties"].as_object().unwrap().keys().cloned().collect::<Vec<_>>();
            (p, t.parameters["required"].clone())
        };
        let ph = vec!["image".to_string(), "seed".to_string()];
        assert_eq!(
            props(generate_3d(Some(&ph))),
            (vec!["image".into(), "seed".into(), "path".into()], json!(["image"]))
        );
        let ph = vec!["prompt".to_string(), "width".to_string()];
        assert_eq!(
            props(generate_image(Some(&ph))),
            (vec!["prompt".into(), "width".into(), "seed".into(), "path".into()], json!(["prompt"]))
        );
        let (p, req) = props(generate_image(None));
        assert_eq!(p, vec!["prompt", "negative_prompt", "width", "height", "seed", "path"]);
        assert_eq!(req, json!(["prompt"]));
    }
}
