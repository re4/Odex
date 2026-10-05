# Build Odex: a self-hosted Codex desktop app for vLLM

You are building **Odex**, a desktop app that clones the **OpenAI Codex desktop app** feature for feature. It keeps only the **coding, MCP and PC-control** features and runs entirely on **self-hosted models served by vLLM**. It also needs a **smart context engine**, so long threads keep working after the model's context window fills up.

Work autonomously and incrementally. Make reasonable decisions without asking, and record each one in `docs/DECISIONS.md`.

---

## 0. Ground rules

- **Parity references:**
  - The Codex app docs, now at `https://learn.chatgpt.com/docs/` (`/app`, `/features.md`, `/reference/slash-commands`, `/reference/commands`, `/reference/settings`, plus the pages on worktrees, review, automations, computer use and browser).
  - The Codex app changelog.
  - The open-source `openai/codex` repo (Apache-2.0). Study `codex-rs/app-server` and `app-server-protocol` closely. That is the engine-to-UI protocol the real app is built on, and Odex uses the same split.
- **Parity audit first:** before writing app code, create `docs/PARITY.md`. It is a table of every upstream app feature marked **keep / adapt / cut / stretch**, with a reason and the milestone that delivers it. Upstream ships weekly, so the audit, not memory, decides what "1:1" means. Where this prompt and upstream disagree, follow this prompt.
- **Licensing and branding:** write original code. If you port any Apache-2.0 code, keep its header and add a `NOTICE` entry. Don't use OpenAI, Codex or ChatGPT names or assets. The product is **Odex**, the user config directory is `~/.odex/`, and the per-project directory is `.odex/`.
- **Platform priority:** Windows 11 comes first, then macOS and Linux. Every milestone must build, run and pass tests on Windows.
- **Progress tracking:** keep `docs/PROGRESS.md` current (what's done, what's next, known issues), so any agent, including you after a context reset, can resume.

## 1. Scope

### Keep: coding

**Projects and threads**
- Projects can be one folder or several, with a primary folder.
- Chats can also exist without a project (`/task`, Quick Chat).
- Thread actions: pin, unread state, archive/restore, rename.
- Search past threads by title, content and branch name.
- An activity view of recent threads.
- Multiple windows, and a system tray icon so the app keeps running.

**Run modes**
- **Local:** the agent works in the project folder.
- **Worktree:** the agent works in an auto-created git worktree for that thread. Supports a setup script and handing changes back to a local branch.
- `/local`, `/worktree`, `/project`, `/fork` (to a new chat or worktree) and `/side` (a temporary side chat).

**Parallel work**
- Many threads run at the same time.
- Subagents get visible activity, stable identicons and diff stats.

**Composer**
- Pickers for model, reasoning effort, permission mode and Local/Worktree.
- `@` mentions for files, apps and skills; a `/` command menu.
- Paste or attach images and files.
- Queue or steer messages while a turn is running.
- Edit and resend earlier messages.
- Context-aware follow-up suggestions.

**Slash commands:** `/plan` (planning mode), `/goal` (persistent objective), `/compact`, `/review`, `/init`, `/status`, `/mcp`, `/model`, `/reasoning`, `/memories`, `/approve`, `/skills`, and any others the audit marks keep.

**Review pane**
- Git diff of the thread's changes, shown inline or detached, with a file tree.
- Stage, unstage and revert per file or per hunk; a whitespace toggle; search inside long diffs.
- Inline review comments, collapsible, sent back to the agent as feedback.
- Multi-repo review.

**Git and PRs**
- A git summary panel.
- Commit with an AI-written message; a push dialog with options.
- Create a PR, plus a PR panel: inspect a GitHub PR, review comments in diffs, an activity timeline, and PR Chat for reviewing a PR with inline feedback.

**Side panel tabs**
- The plan/task list.
- Sources: files the agent read or edited.
- Previews of generated files: code, Markdown, images, PDF.
- Workspace file tabs with inline editing of code and Markdown, annotations, and drag-to-reorder.
- An "Ask Odex" overlay on selected text.

**Terminal**
- Integrated terminal tabs per thread (Ctrl+`), several terminals at once, and a setting for the default terminal location.

**Project actions**
- User-defined run buttons per project (dev server, tests, lint), editable.
- Local environment definitions with worktree setup scripts.

**Permissions**
- Permission modes: read-only / auto / full access, with a full-access warning dialog.
- Approval cards: Ctrl+Enter for custom approve, "don't ask again" handling.
- Automatic review mode: a reviewer model judges escalation requests, and `/approve` overrides one denial.
- Hooks, with an in-app trust review.

**MCP:** a settings UI to add, edit, enable or disable servers, with live status and OAuth. Per-tool approvals and "don't ask again".

**Skills and plugins**
- A skills manager to create, import, enable and `@`-mention skills.
- **Local** plugins only: bundles of skills, MCP servers and hooks installed from a folder or git URL. No marketplace.

**Automations**
- Scheduled tasks and thread automations (heartbeat wake-ups that keep the thread's context).
- A run history review queue with bulk "mark read" and "archive".
- Automations honor the permission mode.

**Memories**
- Local and opt-in: preferences, conventions and stack facts, managed via `/memories` and the Memories settings panel.

**Notifications**
- Alerts for turn complete and approval needed, plus an option to keep the computer awake while threads run.

**Settings panels**
- General, Appearance (theme, accent, fonts, density), Keyboard Shortcuts (search by keypress, rebind, reset), Notifications, Personalization (custom instructions), Computer Use, Browser, MCP, Skills & Plugins, Hooks, Memories, Archived Threads.
- New panels: **Models & Endpoints** and **Context**.
- Search across all panels.

**Navigation:** a command palette (Ctrl+K), file search (Ctrl+P), settings (Ctrl+,), an "unread" section in the palette, and `odex://` deep links.

**Project instructions:** AGENTS.md discovery and generation via `/init`.

### Keep: PC control
- **Computer use** on Windows (first), then macOS and Linux, with **per-app access controls**. It runs in the **background** where possible, so several agents can work without taking over the user's mouse.
- **Appshots:** capture a window's state into the composer for visual debugging.
- **In-app browser:** local dev-server previews and public pages; **browser use** by the agent over CDP; page-level comments and annotations the user sends to the agent; developer mode with raw CDP access; browsing history management.

### Cut
- ChatGPT sign-in, plans, usage limits, rate-limit banners and referrals.
- Cloud mode (`/cloud`, `/cloud-environment`) and Codex Remote mobile control.
- Sites, Dots, Space, Pets, Codex Micro, voice and dictation, image generation and editing.
- OpenAI-hosted web search. Users can add search through MCP.
- Browser extensions (Chrome, Edge and others), Chronicle / Computer History, the plugin marketplace and workspace sharing, share links, profile and activity insight cards.
- `/fast` and `/personality` (custom instructions cover the latter), and non-coding plugins.
- All telemetry, and anything that calls `*.openai.com` or `*.chatgpt.com`.

### Stretch (after parity)
- SSH remote projects and thread handoff between hosts.
- Importing setup from other agents (CLAUDE.md, Cursor rules, their MCP configs).
- MCP Apps UI panels.
- An IDE context bridge (`/ide-context`).
- Record & Replay: turn a demonstrated desktop workflow into a skill.

### Add
- The vLLM model layer (§3), Models & Endpoints settings, and **Doctor** (a health check for vLLM endpoints, §3.5).
- The smart context engine (§10) with a context meter.
- Local token-usage stats in place of plan limits.

## 2. Architecture and stack

The engine/UI split mirrors the real app.

**Engine (Rust):** a headless agent engine in a Cargo workspace. It ships one binary, `odex-engine`:
- `odex-engine app-server` speaks **JSON-RPC 2.0 over stdio**, modeled on Codex's app-server v2 protocol:
  - Methods: `thread/start|resume|fork|list|archive|rollback`, `turn/start|interrupt|steer`, and others the audit identifies.
  - Notifications: `thread/*`, `turn/*` and `item/started|delta|completed`.
  - Server-to-client requests: approvals and elicitations.
- `odex-engine exec` is headless and used by tests, CI and debugging.
- It runs many threads concurrently. Each endpoint has a `max_concurrent_requests` queue; vLLM's continuous batching makes parallel agents cheap.

**Desktop (Electron + React + TypeScript + Vite):**
- **Main process:**
  - Spawns and supervises the engine: restarts it on crash and re-attaches threads.
  - Owns windows, tray, notifications, deep links, global hotkeys and the single-instance lock.
  - Runs user terminals with `node-pty`.
  - Hosts the in-app browser (`WebContentsView` + `webContents.debugger` for CDP).
  - Stores secrets with `safeStorage`.
- **Renderer:**
  - React, with a small store such as Zustand.
  - Virtualized thread and message lists.
  - Monaco for diffs and editing (or CodeMirror 6; pick one, log why).
  - `xterm.js` for terminals.
  - Markdown with syntax highlighting.
  - Light/dark/system themes.

**Shared protocol types** are generated from Rust into TypeScript (`ts-rs`, or JSON Schema → TS). There is one source of truth, and the protocol is versioned.

**Storage:**
- In `~/.odex/`: `config.toml` (with `[profiles]`), append-only JSONL session rollouts, and a SQLite index for threads, search, automations, memories and usage stats.
- In each trusted project, `.odex/` holds project config, actions, environments, skills and rules.

**Packaging:** `electron-builder` produces Windows NSIS and MSIX installers (handling long paths), a macOS dmg, and Linux AppImage and deb. Self-update from GitHub Releases is optional.

**Why this stack:**
- Electron gives Chromium on every OS, so CDP browser use is easy, and node-pty and xterm.js are mature. It also matches the real app.
- Rust gives the engine native sandboxing, ConPTY, UI Automation and speed.

```
odex/
  engine/                 # Cargo workspace
    protocol/  core/  llm/  context/  tools/  apply-patch/  sandbox/  execpolicy/
    hooks/  mcp-client/  computer-use/  browser-bridge/  git/  file-search/
    automations/  memories/  config/  app-server/  exec/
  desktop/
    main/  preload/  renderer/  shared-types/   # generated from engine/protocol
  presets/models.toml
  docs/  PARITY.md  DECISIONS.md  PROGRESS.md  config.md  vllm-setup.md
```

## 3. Model layer (vLLM)

### 3.1 Endpoints and models
- **Endpoints** (`[model_providers.<id>]`) set:
  - `base_url`, defaulting to `http://localhost:8000/v1`
  - an optional API key, stored via `safeStorage`
  - headers and query params
  - `wire_api = "chat" | "responses"`, defaulting to `chat`
  - retries and timeouts
- Several endpoints can be active at once, for example the coder model on one GPU box and the vision model on another.
- **Auto-discovery:**
  - `GET /v1/models` gives the model ids and `max_model_len`. That value becomes the context window unless overridden.
  - `GET /health` and `/version` report status.
- **Model roles** (each falls back to `main`):
  - `main`: the agent.
  - `compactor`: context summaries.
  - `reviewer`: automatic review and `/review`.
  - `vision`: screenshots, used when `main` can't see images.
  - `utility`: titles, commit messages and follow-up suggestions.
  - `embedding`: optional, for recall and memory search.
- **Per-model settings:**
  - Sampling: `temperature`, `top_p`, `top_k`, `min_p`, `repetition_penalty`, `presence_penalty`, `max_output_tokens`.
  - Capability flags: tools, vision, parallel tools, reasoning.
  - `chat_template_kwargs` and `extra_body` (passed through verbatim).
  - Reasoning-effort mapping for `/reasoning`, for example `reasoning_effort` or `{"enable_thinking": false}`.
  - `tool_profile` (§4).
  - `reasoning_history = drop | current_turn | all`.
  - `coordinate_space` (§9).
- **Presets:** ship `presets/models.toml` with known-good settings for popular self-hosted coding models (Qwen3-Coder, GLM-4.x, gpt-oss, DeepSeek-V3.x, Devstral, Kimi-K2 and similar). Each preset includes the exact `vllm serve` flags it needs (`--enable-auto-tool-choice --tool-call-parser … --reasoning-parser … --max-model-len … --enable-prefix-caching`). Check parser names against current vLLM docs.

### 3.2 Requests and streaming
- **Request:** `POST /v1/chat/completions` with:
  - `stream: true`
  - `stream_options.include_usage`
  - `tools`, `tool_choice: "auto"`, and `parallel_tool_calls` when supported
- **Output budget:** `max_tokens = min(max_output_tokens, window − prompt_tokens − margin)`.
- **Structured outputs:** use them (json_schema, or vLLM guided decoding depending on version) for compaction summaries, commit messages, the review format and automatic-review verdicts.
- **Prefix-cache friendliness:** keep the request prefix byte-stable. That means a fixed system prompt, deterministic tool order and serialization, AGENTS.md placed once near the top, and volatile data (time, git status) placed late.
- **Parsing:**
  - Assemble streamed tool calls by `index`.
  - Read reasoning from both `reasoning_content` and `reasoning`.
  - If the server didn't parse thinking, strip `<think>…</think>` on the client.
- **Fallback tool-call parser:** when the content contains a tool call instead of `tool_calls`, parse it on the client. Formats to handle: Hermes `<tool_call>`, Qwen3-Coder XML, Llama-3 JSON, Mistral `[TOOL_CALLS]`, DeepSeek special tokens, and pythonic calls.
- **Argument hygiene:**
  - Repair JSON (trailing commas, raw newlines, truncation).
  - Validate against the tool's schema, and return a precise error so the model retries. Cap retries at 3, then warn.
- **Loop breaker:** break repetition loops (the same call with the same args repeatedly, or degenerate repeated text) with a nudge.

### 3.3 Resilience
- Retry 429, 5xx, resets and mid-stream drops with backoff and jitter. The thread shows "reconnecting…".
- **Context overflow:** vLLM returns HTTP 400 "maximum context length is X… requested Y". Parse it, run the context engine's emergency path (§10.5), and retry transparently.
- An idle-stream timeout leads to a retry, never a hang.
- If the engine process crashes, threads resume from their rollouts.

### 3.4 Token counting
- Ground truth: `usage` on every response.
- Estimates between responses, in order of preference:
  1. vLLM `POST /tokenize`
  2. the model's HF `tokenizer.json` via the `tokenizers` crate
  3. a calibrated chars-per-token ratio

### 3.5 Doctor and Models & Endpoints settings
- **Settings UI:**
  - Add an endpoint, test the connection, and list discovered models with their context window size.
  - Show capability badges, assign roles, and edit sampling presets.
- **Doctor** probes each endpoint and shows a pass/fail table:
  - streaming, a native tool call, streamed and parallel tool calls
  - reasoning parsing, vision (tiny test image), `/tokenize`
  - prefix-cache speed-up (time two identical prompts), structured output
- Doctor suggests the missing `vllm serve` flags with a copy button and caches results in `~/.odex/models_cache.json`.
- **First-run onboarding:**
  1. Auto-detect `localhost:8000`.
  2. Pick models for each role.
  3. Run Doctor.
  4. Choose a default permission mode.
  5. Open or create the first project.

## 4. Agent tools (engine)

**Core (Codex parity):**
- `shell`: arguments are `command`, `workdir`, `timeout_ms`, and `escalated` + `justification`. The default shell is PowerShell on Windows.
- **PTY sessions** (`exec_command` + `write_stdin`): for dev servers and REPLs, using ConPTY on Windows. Sessions can be listed and killed from the UI.
- `apply_patch`: the Codex patch format, with fuzzy context matching, CRLF and BOM preservation, and clear errors. Also intercept `apply_patch` sent through `shell`.
- `update_plan`: steps marked pending, in_progress or completed. Shown in the side panel.
- `view_image`.

**Extended (on by default, because open models do better with them):**
- `read_file(path, offset, limit)`, `list_dir`, `grep` (ripgrep semantics), `glob`
- `edit_file(path, old, new, replace_all)`, `write_file`
- `read_output(ref)`, to page truncated outputs
- `recall(query)`, to search pre-compaction history

**Agent tools:**
- `spawn_agent(task, mode: read_only|write, worktree?)` and `wait_agents` run subagents. The UI shows their activity, identicons and diff stats. The parent only receives each subagent's final report.

**Plan mode (`/plan`):** read-only exploration that ends in a structured plan. The user approves or edits the plan, then execution starts.

**Goal mode (`/goal <objective>`):** a persistent objective the agent keeps pursuing across turns and compactions until it is done, blocked, or a time or token budget runs out. The UI shows a timer and progress.

**Tool profiles**, set per model:
- `codex`: core only.
- `extended`: core + extended (the default).
- `minimal`: shell, edit, read and plan, for small models.

Tool-schema tokens are measured and shown in the context breakdown.

**Base system prompt:** write an original prompt tuned for open models, kept under about 2.5k tokens. It covers:
- a short preamble before tool calls
- when to plan
- prefer dedicated tools over shell for reading and editing
- test after changes
- never revert the user's changes
- a concise final message with file references

Then append environment context and AGENTS.md (global `~/.odex/AGENTS.md`, then root down to cwd; `AGENTS.override.md` wins; capped by `project_doc_max_bytes`).

## 5. Permissions, sandbox, hooks

**Permission modes:**
- **Read-only:** no writes, approval for everything else.
- **Auto:** workspace-write sandbox, asks before escalating or using the network.
- **Full access:** shows a warning dialog.

These map to Codex's approval policies (`untrusted | on-failure | on-request | never`) and sandbox modes (`read-only | workspace-write | danger-full-access`). Network is off in workspace-write unless enabled.

**Approval cards** are shown inline in the thread and as OS notifications.
- They show the command, cwd and justification. Patch requests show a diff; MCP requests show the tool and args.
- Choices:
  - **Approve**
  - **Approve, and don't ask again this session** for this command prefix or tool
  - **Deny with feedback**
  - **Custom approve** (Ctrl+Enter)

**Automatic review:** an optional mode where the `reviewer` model judges each escalation against the user's goal and a risk rubric. It returns a structured verdict of allow, deny or ask-user. `/approve` overrides one denial.

**Sandbox backends:**
- **Windows:** AppContainer or restricted token + Job Object, with ACL grants on writable roots and no network capability.
- **Linux:** Landlock + seccomp, or bubblewrap.
- **macOS:** Seatbelt.
- If a backend is unavailable, fall back to approval-required, with a visible warning. Never silently run unsandboxed.

**Exec policy:** rules files (`~/.odex/rules/`, `.odex/rules/`) map command prefixes to allow, prompt or forbid. Dangerous patterns ship forbidden by default.

**Hooks:**
- Events: session start, user prompt submit, pre/post tool use, stop, and notification.
- Hooks are command hooks that receive a JSON payload and can block or modify.
- New or changed hooks need an in-app **trust review** before they run.

**Project trust:** opening a new folder asks whether to trust it. Untrusted projects ignore `.odex/` config, hooks and actions.

## 6. App UI

**Main window:** a left sidebar, the center thread, a right side panel and a bottom terminal panel. Every region can be resized and collapsed, and its state persists per window.

**Sidebar:**
- New thread, plus Quick Chat.
- Projects, collapsible, each listing its threads.
- Thread rows show state: running spinner, needs approval, unread dot, error.
- Pinned threads, the Activity view, Automations (with an unread run count) and Search.
- Archived threads, reached from settings.

**Thread:**
- Streaming Markdown messages.
- Collapsible reasoning.
- Command cells: command, exit code, duration, collapsible output.
- Patch cells with mini-diffs.
- MCP, browser and computer-use cells (with screenshot thumbnails).
- Subagent cards.
- Plan updates and approval cards.
- Compaction notices, for example "context compacted 118k → 24k".
- Follow-up suggestion chips.
- Scroll position is preserved per thread, as are unfinished edits and comments.

**Composer:**
- Multiline input, with a setting for whether Enter sends or adds a newline.
- Mention and slash menus; attachment chips.
- Chips for mode (Local/Worktree), model, reasoning effort and permission mode.
- A **context meter**: a ring showing % of the window used. Hover shows the breakdown; click opens the Context view (§10.8).
- A stop button; messages typed while running are queued or used to steer.

**Side panel tabs:** Review, Plan/Tasks, Sources, Files (preview and edit), Browser, Git/PR.

**Terminal panel:** tabs per thread, starting in the thread's cwd or worktree.

**Command palette (Ctrl+K):**
- Commands, threads, projects and the unread section.
- A theme switcher.
- File search with Ctrl+P.

**Keyboard shortcuts:** match the upstream defaults from the commands reference. All are rebindable in settings, with search by keypress.

**Tray:** shows running and needs-approval counts, quick actions and Quit. The app keeps running in the background for automations.

**Accessibility:** full keyboard navigation, screen-reader labels, and respect for reduced motion.

## 7. Git, worktrees, review, PRs

**Worktrees:**
- Created under `~/.odex/worktrees/<project>/<thread>` on branch `odex/<slug>`.
- Run the project's setup script when created.
- "Hand off" merges, cherry-picks or checks out the worktree branch into the local checkout, and handles conflicts.
- Clean up when a thread is archived, after confirming.

**Review pane:**
- Covers the thread's working-tree diff against its base: uncommitted changes, the branch versus base, or a specific commit.
- The file tree shows add/del counts.
- Diffs are syntax-highlighted, in unified or split view.
- Stage, unstage and revert per hunk or file; a whitespace toggle; search.
- **Inline comments** are batched and sent back to the agent as structured feedback.
- Multiple repos are shown together.

**`/review`:** a dedicated review turn using the `reviewer` model. Output is prioritized findings with file:line anchors, rendered as review comments.

**Commit, push and PR:**
- Commit with an AI-written message, editable before committing.
- The push dialog offers set upstream or force-with-lease, with a confirm step.
- Create a PR through the `gh` CLI if it's present, otherwise the GitHub REST API with a token in `safeStorage`.
- **PR panel:** status, checks, a timeline, and review comments placed on diff lines.
- **PR Chat:** review an existing PR and draft inline comments. Posting them requires explicit confirmation.
- Undo snapshots each turn as a hidden git ref, so the user can roll a thread back to any earlier turn.

## 8. Automations and memories

**Automations:**
- Each automation has a schedule (cron or a friendly picker), a target project or thread, a prompt, a model and a permission mode, which it honors.
- **Thread automations** wake an existing thread on a schedule and keep its context, with smart context applied.
- Runs execute headlessly in the engine while the app is in the tray.
- Results land in a **review queue** with unread state and bulk "mark read" or "archive".
- Failures show the reason.

**Memories:**
- Opt-in, stored in `~/.odex/memories/` (global and per project).
- The `utility` model proposes memories after threads end. The user approves, edits or deletes them in Settings → Memories.
- Approved memories are injected within a capped token budget, so they never crowd the context.
- `/memories` toggles use and generation per thread.

## 9. Computer use and browser use

**Computer use:** off until enabled in Settings → Computer Use. Apps must be granted per app.

**Tools:**
- `screenshot(screen|monitor|window|region)`:
  - Captures with Windows.Graphics.Capture or PrintWindow, so **background windows** can be captured without bringing them to the front.
  - Downscaled to the model's image limit, with scale factors returned.
  - Per-monitor DPI aware (Windows `PER_MONITOR_AWARE_V2`), with correct multi-monitor coordinates.
- `ui_tree(window, depth, filter)`:
  - The UI Automation tree (AT-SPI on Linux, AX on macOS).
  - Element ids, role, name, value, bounds and state.
  - Token-budgeted, so text-only models can act.
- `ui_action(element, invoke|focus|set_value|toggle|expand|select|scroll_into_view)`: prefer these **non-intrusive** UIA patterns, which don't move the user's cursor.
- `mouse(...)` and `keyboard(...)`: real input via SendInput, used only when needed. It shows a **takeover overlay**, and coordinates map from screenshot space to physical pixels.
- `window(list|focus|move|resize|minimize|maximize|close|launch)`, `clipboard(get|set)`, `wait(ms)`.

**Model routing:**
- If `main` has vision, screenshots go to it directly.
- If not, the `vision` model describes the screen and locates elements, and returns that to `main`.
- `coordinate_space = pixels | normalized_1000 | normalized_1` matches the grounding style of models like Qwen-VL and UI-TARS.

**Safety (non-negotiable):**
- Per-app allowlist, and per-action approval by default.
- A global **kill switch** hotkey (default Ctrl+Alt+Esc) stops all computer and browser actions at once.
- A visible indicator shows whenever the agent controls input.
- Every action is logged with before/after screenshots in the thread.
- Never type into password fields (UIA `IsPassword`) or touch UAC or credential prompts. Ask the user to do those steps.

**Appshots:** capture the focused window or a chosen window, with its screenshot and UI tree, into the composer as context.

**In-app browser (side panel tab):**
- Tabs, an address bar, back/forward, devtools, and auto-open of the dev server from project actions.
- **Browser use tools** over CDP:
  - `browser_navigate`
  - `browser_snapshot`: an accessibility/DOM snapshot with element refs
  - `browser_click`, `browser_type`, `browser_select`, `browser_scroll`
  - `browser_screenshot`, `browser_eval` (allowed in developer mode)
  - `browser_console`, `browser_network`
- The user can **comment on page elements or regions**. Comments carry selector, bounds and a screenshot, and are sent to the agent.
- Per-site access permissions; downloads need approval; history can be managed in settings.
- Never enter credentials or payment details; ask the user.

**Context hygiene:** only the latest one or two screenshots stay as images. Older ones become text stubs holding the action and a description.

## 10. Smart context engine

**Goal:** a thread, goal or automation can run indefinitely, even on a 32k model, without losing the task, the user's requirements or the working state, and without the user doing anything.

### 10.1 Budget
- `budget = window − reserved_output − margin`. Defaults: reserve `min(max_output_tokens, 25% of window)` and keep a 3% margin.
- Measure exactly from `usage` and estimate deltas (§3.4).
- Check before **every model call inside a turn**, not just between user messages.

### 10.2 Tier 0: prevention (always on)
- **Cap tool outputs:** head plus tail, scaled to the window, with a marker such as `[… 2,431 lines omitted — full output saved as ref:out_17; use read_output]`. Full outputs are kept on disk.
- **Page file reads.**
- **Cap images.**

### 10.3 Tier 1: pruning (no LLM, at `prune_at`, default 0.70)
Apply in order until under target:
1. Turn tool outputs older than K turns into one-line stubs: tool, args, exit code, size and ref.
2. Drop superseded file reads.
3. Collapse duplicates and repeated failures.
4. Turn old screenshots into stubs.
5. Drop old reasoning.

Prune in large batches so vLLM's prefix cache isn't invalidated on every step.

### 10.4 Tier 2: compaction (LLM, at `compact_at`, default 0.85, or `/compact [focus]`)
- **Summarize** everything except pinned items and the most recent `keep_recent_tokens`, which defaults to about 20% of the window, ends on whole turns, and never splits a tool call from its result.
- **Use the `compactor` model** with structured output. The handoff summary has these sections:
  - The user's goal and every explicit requirement, **quoted verbatim**.
  - Decisions made, with their reasons.
  - The plan, with step status.
  - Files changed: purpose and current state.
  - Codebase facts learned.
  - Commands and tests run, with their latest results.
  - Open errors.
  - Next steps.
  - Important refs.
- **If the history is too large** for one call, run a rolling map-reduce.
- **Rebuilt context**, in this order:
  1. system prompt, tools and AGENTS.md (unchanged, so the cached prefix survives)
  2. `[Context summary #n]`
  3. pinned items:
     - the original task, verbatim
     - the active `/goal`
     - the current plan
     - `.odex/NOTES.md`, the agent's working notes, size-capped
     - approved memories
  4. a **working-set snapshot**: `git status --short`, `git diff --stat`, files touched, and optionally the 1–3 hottest files within budget
  5. the recent turns, verbatim
- **Target** at most 50% of the window afterwards. If still over, shrink the recent turns, then truncate oversized items. Allow at most 2 attempts per step, then use Tier 3.
- **Mid-turn:** compaction mid-turn is seamless, and the agent **continues automatically**. Subagents and automations compact on their own.
- **Fallback:** if the compactor fails, build a deterministic extractive summary: user messages, the plan, files touched and the last errors. A thread never stops because of context.
- **Logging:** each compaction is a checkpoint in the rollout and an `item` in the thread UI.

### 10.5 Tier 3: emergency
On an overflow 400, or when estimation was wrong:
1. Prune aggressively.
2. Compact.
3. Hard-trim the oldest non-pinned items.
4. Retry.

The user's newest message is never dropped.

### 10.6 Recall
Nothing is deleted.
- `recall(query)` searches pre-compaction history with BM25, or the embedding model if one is configured, and returns exact snippets with refs.
- `read_output(ref)` pages stored outputs.
- The system prompt tells the model to use both when the summary lacks a detail.

### 10.7 Other context-aware behavior
- **Model switches:** switching to a smaller-window model, resuming a large thread, or forking a thread re-budgets and compacts on load if needed.
- **MCP tool budget:** if tool schemas exceed 15% of the window (configurable), switch to lazy loading through `search_tools(query)`.
- **Subagents** keep exploration out of the parent's context.

### 10.8 UI
- The composer context meter (§6).
- **Context view** (from `/status`, `/context` or a meter click) shows a token breakdown by system, tools, AGENTS.md, memories, summary, history, tool outputs and images, plus the compaction history.
- The **Context settings** panel holds the thresholds, keep-recent size, compactor model and output caps.

## 11. MCP, skills, plugins

**MCP client:**
- Server definitions in `[mcp_servers.<name>]`:
  - stdio servers: `command`, `args`, `env`, `cwd`
  - HTTP servers: `url` + bearer token or OAuth
  - timeouts, `enabled`, `enabled_tools` and `disabled_tools`
- Everything can be edited in Settings → MCP, with live status, logs and restart.
- Servers start in parallel. A failing server shows a warning and never blocks a thread.
- Tool names are `mcp__<server>__<tool>`, sanitized to at most 64 characters (hash suffix on collision).
- Expose resources and prompts.
- **Schema sanitizing** for vLLM chat templates: inline `$ref` and simplify `anyOf`; keep the original schema for validation.
- Calls go through the permission system, with custom approval panels and "don't ask again".
- `/mcp` shows status.

**Skills:**
- `~/.odex/skills/<name>/SKILL.md` and `.odex/skills/`.
- Only the front-matter `name` and `description` go in the prompt; the body loads on use.
- The skills manager lets users create, edit, enable/disable and import skills.

**Plugins:** local bundles with a manifest, containing skills, MCP servers, hooks and project actions. They install from a folder or git URL, after a trust review.

## 12. Testing and acceptance

- **Mock vLLM server** (axum): replays SSE fixtures covering content, reasoning, native and fallback tool calls, malformed args, disconnects, 429/503 errors and overflow 400s. It enforces a configurable `max_model_len`.
- **Engine unit tests:** patch apply, exec policy, sanitizers, budgeting, each context tier, worktree lifecycle and the automation scheduler.
- **Context soak test:** a 4,096-token mock model runs a 200-step task that forces at least 10 compactions. The test asserts that the task completes, verbatim requirements survive, no request overflows, and `recall` finds a pre-compaction detail.
- **App end-to-end:** Playwright's Electron support, run against the mock server. It covers onboarding, starting a thread, approval flows, review pane stage/revert/comment, worktree create and hand-off, terminal, an automation run, `/compact`, and settings persistence. Add visual snapshots of the main screens in light and dark themes.
- **Computer use:** drives a Notepad harness on Windows: screenshot, `ui_tree` find, type, verify. The kill switch is tested. Browser use drives a local fixture page.
- **Opt-in real-vLLM smoke suite:** set `ODEX_E2E_BASE_URL` to run Doctor, a fix-a-failing-test task, an MCP round-trip and a compaction.
- **CI:** GitHub Actions on Windows, macOS and Linux, running `cargo fmt`/`clippy -D warnings`, `tsc --noEmit`, ESLint and all tests, and building installers.

**Done means:**
- Every "keep" row in PARITY.md is implemented or explicitly deferred.
- Doctor passes against a stock vLLM.
- Three parallel worktree threads on a 32k model each run more than 1 hour without context errors.
- Everything passes on Windows.

## 13. Milestones (in order; update PROGRESS.md and commit after each)

1. **M0:** parity audit, monorepo skeleton, protocol crate with TS codegen, config loading, CI.
2. **M1:** vLLM client (streaming, tool parsing and fallbacks, resilience), Doctor, model presets.
3. **M2:** engine core loop, tools, rollouts, `app-server` JSON-RPC, `exec` mode.
4. **M3:** Electron shell: window, sidebar, thread view, composer, onboarding, Models & Endpoints settings. A thread works end to end.
5. **M4:** permissions, approval cards, sandboxes (Windows first), exec policy, hooks with trust review, project trust.
6. **M5:** smart context engine, all tiers, plus recall, the context meter and view, and the soak test.
7. **M6:** git: worktrees, review pane, commit/push/PR, PR panel, undo; the terminal panel; project actions and environments.
8. **M7:** MCP client and settings, lazy tool loading, skills, local plugins.
9. **M8:** subagents, plan mode, goal mode, automations with review queue, memories, notifications, tray.
10. **M9:** computer use with safety, appshots, the in-app browser and browser use.
11. **M10:** command palette, shortcuts, multi-window, search, appearance, accessibility, packaging and installers, docs (`README`, `docs/config.md`, `docs/vllm-setup.md` with tested `vllm serve` commands), then stretch goals.

Start with M0 now.
