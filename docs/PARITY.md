# Odex parity audit

Source of truth for what "1:1 with the upstream Codex app" means for Odex. It was built from
`docs/research/upstream-notes.md`, which covers the upstream docs, changelog and `openai/codex` app-server source as of 2026-10-04.
Where PROMPT.md and upstream disagree, PROMPT.md wins, and the row says so.

Since 2026-07-09 upstream ships the Codex surface inside a combined desktop app. The parity target is that Codex surface.

**Decisions:** **keep** means implement the upstream behavior. **adapt** means implement it, changed for self-hosted/vLLM or Odex scope. **cut** means do not implement. **stretch** means after parity.
**Milestones:** see PROMPT.md §13. M0 covers audit, skeleton and protocol; M10 covers palette, packaging and docs.

Status lives in `docs/PROGRESS.md`. A row that is "keep" is done only when it is implemented or explicitly deferred there.

## A1. Projects and threads

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Projects with 1+ folders and a primary folder | keep | PROMPT §1 | M3 |
| Edit project: add folder, make primary | keep | | M3 |
| Project menu: open in Explorer/Finder, remove from sidebar, bulk archive | keep | | M3 |
| Permanent worktree as its own project | stretch | Not in PROMPT; worktrees are per-thread | — |
| New thread in project | keep | | M3 |
| Projectless chat (`/task`) | keep | PROMPT §1 | M3 |
| Quick Chat | adapt | Upstream opens a separate chat product. Odex opens a projectless `quickChat` thread in a small window | M3 |
| Thread rename / pin / unread / archive / restore | keep | | M3 |
| Copy deep link / thread id / cwd / rollout path | keep | | M3 |
| Clear all unread; next thread needing attention | keep | | M10 |
| Activity view (unread, running, waiting) with mark-all-read | keep | PROMPT §1 | M8 |
| Search threads by title, content, branch | keep | SQLite FTS5 | M3 |
| Find in thread (next/prev) | keep | | M10 |
| Fork from latest or earlier message, into a local thread or worktree | keep | `/fork` | M3/M6 |
| Side chat `/side` (ephemeral fork) | keep | | M3 |
| Archived threads in Settings with Unarchive | keep | | M3 |
| Archive removes managed worktree after a snapshot | keep | Confirm first (PROMPT §7) | M6 |
| Pop-out thread window, always-on-top | keep | Multi-window | M10 |
| Share read-only snapshot / share links | cut | PROMPT §1 Cut | — |
| Thread handoff between hosts | stretch | PROMPT stretch (SSH) | — |
| @-mention other threads | keep | Inserts that thread's summary | M8 |
| Undo/redo last app action (archive, pin, rename) | keep | Client-side undo stack | M10 |
| Subagent activity, identicons, diff stats | keep | PROMPT §1 | M8 |
| Thread sections (user-named groups) | stretch | Not in PROMPT | — |

## A2. Run modes

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Local mode | keep | | M2 |
| Worktree mode with auto-created worktree per thread | keep | `~/.odex/worktrees/<project>/<thread>`, branch `odex/<slug>` (PROMPT §7). Upstream uses detached HEAD | M6 |
| Pick starting branch (incl. current with uncommitted changes) | keep | | M6 |
| Setup script on worktree creation (default + per-OS) | keep | Environments | M6 |
| Hand off Local ↔ Worktree (merge / cherry-pick / checkout) | keep | PROMPT §7 | M6 |
| `.worktreeinclude` copies ignored files into worktrees | keep | Cheap, useful | M6 |
| Worktree retention (keep N=15, auto-cleanup toggle, snapshot before delete) | keep | | M6 |
| Cloud mode, `/cloud`, `/cloud-environment` | cut | PROMPT Cut | — |
| SSH remote projects | stretch | PROMPT stretch | — |
| Windows native vs WSL agent | adapt | Native only. WSL is reachable as the shell (`default_shell = "wsl"`) | M2 |

## A3. Composer

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Enter sends vs Ctrl+Enter for multiline (setting) | keep | | M3 |
| Up-arrow restores previous prompt | keep | | M3 |
| Steer vs Queue while running; setting + modifier invert | keep | | M3 |
| Queued messages: edit, reorder, send now, delete | keep | `thread/queue/set` | M3 |
| `@` mentions: files, folders, skills, apps (MCP), threads | keep | | M3/M7 |
| `$skill` shorthand | keep | | M7 |
| `/` menu mid-draft | keep | | M3 |
| Attach files and images, paste images and non-image files | keep | | M3 |
| Appshot attachments | keep | M9 | M9 |
| Video attachments | cut | Not useful for self-hosted VLMs | — |
| Model picker (Ctrl+Shift+M), effort picker, permission picker, Local/Worktree, branch | keep | | M3 |
| Fast tier `/fast` | cut | PROMPT Cut | — |
| IDE context toggle | stretch | PROMPT stretch `/ide-context` | — |
| Plan mode `/plan` with plan questions | keep | | M8 |
| Goal mode `/goal` with progress row, pause/resume/edit/clear, timer | keep | | M8 |
| Voice and dictation | cut | PROMPT Cut | — |
| Model downgrade warning | adapt | Warn when the endpoint serves a different model than selected, or the window shrank | M3 |
| Usage-limit errors | cut | No plans. Endpoint errors are shown instead | — |
| Context-aware suggested prompts / follow-up chips | keep | `utility` model | M8 |
| Approval cards: Enter approve, Esc decline, Ctrl+Enter custom | keep | | M4 |
| Edit and resend earlier messages | keep | Rollback + resend | M3 |
| Context meter ring with breakdown | add | PROMPT §10.8 | M5 |
| `request_user_input` live questions | adapt | Plan-mode questions are rendered in the proposed plan; no blocking tool | M8 |

## A4. Review pane

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Scopes: Unstaged, Staged, Commit, Branch vs base, Last turn | keep | `DiffTarget` | M6 |
| Uncommitted (combined) scope | keep | PROMPT §7 | M6 |
| Multi-repo selector / all repos | keep | | M6 |
| Stage / unstage / revert: all, per file, per hunk | keep | | M6 |
| Inline comments (+ on hover), collapsible, sent as guidance | keep | `reviewComments` input | M6 |
| Review findings rendered as inline comments | keep | | M6 |
| File tree with add/del counts, ordered like the diff | keep | | M6 |
| Open file in editor / at line | keep | `shell.openPath` + configured editor | M6 |
| Search in diff (Ctrl+F seeded from selection) | keep | | M6 |
| Whitespace toggle | keep | | M6 |
| Unified / split view, syntax highlighting | keep | PROMPT §7 | M6 |
| Inline vs detached review pane | keep | PROMPT §1 (detached = pop-out window) | M6 |
| `/review` (uncommitted / base branch / commit / custom) with `reviewer` model | keep | | M6 |
| Offer `git init` for non-repos | keep | | M6 |

## A5. Git and PRs

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Git summary panel | keep | | M6 |
| Commit with AI-written, editable message | keep | `utility` model, structured output | M6 |
| Push dialog: set upstream, force-with-lease, confirm | keep | | M6 |
| Branch prefix, allow force push, commit/PR prompt settings | keep | Settings → Git | M6 |
| Create PR via `gh` or REST API + token | keep | PROMPT §7 | M6 |
| PR status badges on threads | keep | | M6 |
| PR panel: summary, checks, timeline, review comments on diff lines | keep | | M6 |
| PR Chat: review an existing PR, draft inline comments, post with confirmation | keep | | M6 |
| PR inbox (assigned / authored), pinned PRs | adapt | Basic list via `gh pr list`. No team inbox | M6 |
| Submit review: comment / approve / request changes | keep | Explicit confirmation | M6 |
| "Fix" on failing checks attaches check output | keep | | M6 |
| Stacked PRs view | stretch | | — |
| GitLab MRs | cut | GitHub only | — |
| Undo snapshots per turn as hidden git refs; roll back to any turn | keep | PROMPT §7 | M6 |

## A6. Side panel

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Tabs: Review, Plan/Tasks, Sources, Files, Browser, Git/PR | keep | PROMPT §6 | M3–M9 |
| Layout cycle (full / split / hidden), chat ↔ tabs toggle | keep | | M10 |
| New tab menu: Terminal, Browser, File | keep | | M6 |
| Drag to reorder tabs | keep | | M6 |
| Sources: files read/edited, open or download | keep | | M3 |
| Generated-file previews: code, Markdown, images, PDF | keep | | M6 |
| HTML live preview with source toggle | keep | Sandboxed iframe | M6 |
| Office docs/slides/sheets viewer | cut | Non-coding | — |
| Thread summary card | keep | Uses the latest compaction summary or `utility` | M8 |
| File tree toggle (Ctrl+Shift+E) and file search (Ctrl+P) | keep | | M6/M10 |
| Workspace file tabs, inline editing (code + Markdown), annotations | keep | CodeMirror 6 | M6 |
| Back navigation, recent files, remembered scroll | keep | | M6 |
| "Ask Odex" overlay on selected text | keep | | M6 |
| Image lightbox, zoom, download | keep | | M6 |
| Image editing / Canvas | cut | Image generation/editing cut | — |
| Mermaid diagrams inline | keep | | M3 |
| MCP Apps UI panels | stretch | PROMPT stretch | — |

## A7. Terminal

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Terminal tabs per thread, scoped to cwd/worktree | keep | node-pty + xterm.js | M6 |
| Multiple terminals at once | keep | | M6 |
| Ctrl+` toggle, Ctrl+J bottom panel, Ctrl+L clear | keep | | M6 |
| Default terminal location (bottom / right) | keep | | M6 |
| Default shell on Windows (PowerShell / cmd / Git Bash / WSL) | keep | | M6 |
| Agent reads the integrated terminal | keep | `read_terminal` tool → `terminal/read` server request | M6 |
| Background terminals (agent PTY sessions) list/terminate | keep | `exec/sessions`, `exec/kill` | M2/M6 |
| User shell command in thread (unsandboxed) | keep | `!cmd` in composer | M6 |

## A8. Project actions and environments

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Actions (name, icon, command, per-OS) in the top bar, run in terminal | keep | `.odex/actions.toml` | M6 |
| Edit actions in-app | keep | | M6 |
| Run environment action 1 shortcut | keep | | M10 |
| Environments with setup scripts (default + per-OS) | keep | `.odex/environments.toml` | M6 |
| Open in editor menu, per-project override | keep | | M6 |
| Auto-open dev-server URL in the in-app browser | keep | PROMPT §9 | M9 |

## A9. Permissions and approvals

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Modes: read-only / auto / full access | adapt | PROMPT §5 names. Upstream: Ask / Approve-for-me / Full | M4 |
| Full-access warning dialog | keep | | M4 |
| Automatic review (reviewer model verdict allow / deny / ask) | keep | Toggle in Permissions; maps upstream "Approve for me" | M4 |
| `/approve` overrides one denial | keep | | M4 |
| Approval kinds: exec, patch, MCP, network, computer use, browser, download | keep | | M4/M7/M9 |
| Accept for session ("don't ask again" by prefix/tool) | keep | | M4 |
| Accept + execpolicy amendment (persist rule) | keep | "Always allow this prefix" writes `~/.odex/rules/` | M4 |
| Cancel turn from approval | keep | `abort` decision | M4 |
| Auto-review status item (risk, rationale) | keep | Notice item + approval card badge | M4 |
| Exec policy rules (allow / prompt / forbid prefixes) | keep | | M4 |
| Hooks with in-app trust review | keep | | M4 |
| Project trust prompt on new folders | keep | | M4 |
| Sandbox: Windows restricted token / AppContainer, Linux bwrap/Landlock, macOS Seatbelt | keep | PROMPT §5. Windows, Linux bubblewrap and macOS Seatbelt are implemented. **Deferred:** the Landlock fallback for Linux hosts without bwrap (bwrap is the documented requirement; without it the engine falls back to approval-required, never silently unsandboxed) | M4 |
| Named permission profiles | stretch | Config `[profiles]` covers most of it | — |
| Managed `requirements.toml` admin pins | cut | Enterprise | — |

## A10. MCP

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Settings UI: add / edit / enable / disable / restart, live status, logs | keep | | M7 |
| stdio + Streamable HTTP, bearer token, headers | keep | | M7 |
| OAuth for HTTP servers | keep | PKCE + dynamic registration | M7 |
| `enabled_tools` / `disabled_tools`, per-tool approval, "don't ask again" | keep | | M7 |
| Startup / tool timeouts | keep | | M7 |
| `/mcp` status | keep | | M7 |
| Resources and prompts (@-mentions) | keep | | M7 |
| Elicitation (form) | keep | `elicitation/request` | M7 |
| Server `instructions` honored | keep | Appended to the system prompt | M7 |
| Schema sanitizing for chat templates | add | PROMPT §11 | M7 |
| Lazy tool loading via `search_tools` | add | PROMPT §10.7 | M7 |
| Recommended servers list | adapt | Static list of popular local servers (filesystem, playwright, search) | M7 |
| MCP Apps UI | stretch | | — |

## A11. Skills and plugins

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Skills manager: browse, create, edit, enable/disable, import | keep | | M7 |
| `@`/`$` skill invocation; skills in the `/` menu | keep | | M7 |
| Built-in skill-creator skill | keep | Ships as a bundled skill | M7 |
| Local plugins (folder or git URL) with trust review | keep | PROMPT §11 | M7 |
| Plugin marketplace, sharing, workspace plugins | cut | PROMPT Cut | — |
| Record & Replay | stretch | PROMPT stretch | — |
| Import from other agents (CLAUDE.md, Cursor rules, MCP configs) | stretch | PROMPT stretch | — |

## A12. Automations

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Standalone scheduled task: new thread per run, project, local or worktree | keep | | M8 |
| Cron or friendly picker schedule | keep | PROMPT §8 (upstream uses RRULE; Odex uses cron + presets) | M8 |
| Model, effort and permission mode per automation | keep | | M8 |
| Thread automations (heartbeat) that keep context | keep | | M8 |
| Run history review queue: unread, bulk mark read / archive | keep | | M8 |
| Templates, titles | keep | | M8 |
| Create automations by asking in chat | stretch | | — |
| Event-triggered tasks (Gmail / Slack) | cut | Cloud / non-coding | — |
| Runs while the app is in the tray | keep | Engine scheduler | M8 |

## A13. Memories

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Opt-in memories stored locally (global + per project) | keep | `~/.odex/memories/` | M8 |
| `utility` model proposes; user approves / edits / deletes | adapt | PROMPT §8 requires approval (upstream is automatic) | M8 |
| `/memories` per-thread use / generate | keep | | M8 |
| Capped injection budget | keep | | M8 |
| Secret redaction in memories | keep | | M8 |
| Computer History | cut | PROMPT Cut | — |

## A14. Notifications

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Turn complete: never / background only / always | keep | | M8 |
| Approval-needed notifications | keep | | M8 |
| Keep computer awake while running | keep | `powerSaveBlocker` | M8 |
| Tray: running / needs-approval counts, quick actions, Quit | keep | | M8 |
| Usage-limit display in tray | adapt | Local token usage instead | M8 |
| Pets | cut | PROMPT Cut | — |

## A15. Settings panels

| Panel | Decision | Reason / notes | MS |
|---|---|---|---|
| General (send key, prevent sleep, follow-up behavior, permissions, review delivery, terminal location, editor, shell) | keep | | M3+ |
| Appearance (theme, accent, bg/fg, UI/code fonts, density) | keep | | M10 |
| Keyboard Shortcuts (search by keypress, rebind, reset one/all) | keep | | M10 |
| Notifications | keep | | M8 |
| Personalization (custom instructions, enable memories) | adapt | Custom instructions only. Personality cut (PROMPT) | M8 |
| Computer Use (enable, always-allowed apps) | keep | | M9 |
| Browser (allowed/blocked sites, history, clear data, downloads, developer mode) | keep | | M9 |
| MCP | keep | | M7 |
| Skills & Plugins | keep | | M7 |
| Hooks | keep | | M4 |
| Memories | keep | | M8 |
| Archived Threads | keep | | M3 |
| Worktrees (root, keep count, auto-cleanup) | keep | | M6 |
| Git (branch prefix, force push, commit/PR prompts) | keep | | M6 |
| Code Review (review instructions) | keep | | M6 |
| Local environments | keep | | M6 |
| Models & Endpoints | add | PROMPT §3.5 | M3 |
| Context | add | PROMPT §10.8 | M5 |
| Usage (local token stats) | add | PROMPT §1 Add | M8 |
| Settings search across panels | keep | | M10 |
| Profile, Pets, Appshots hotkey destination, Connections, Import, Workspace | cut / adapt | Profile, Pets, Connections, Workspace cut. Appshot hotkey kept under Computer Use. Import is stretch | — |
| Suggested prompts toggle | keep | Under General | M8 |

## A16. Navigation

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Command palette (Ctrl+K / Ctrl+Shift+P) with commands, threads, projects, unread section, theme switcher | keep | | M10 |
| File search Ctrl+P | keep | | M10 |
| Settings Ctrl+, ; shortcuts Ctrl+/ | keep | | M10 |
| Back / forward, go to thread 1–9, recent thread 1–6, prev/next thread | keep | | M10 |
| Deep links `odex://threads/new?prompt=&path=`, `odex://threads/<id>`, `odex://settings/<panel>`, `odex://skills`, `odex://automations` | keep | Odex scheme | M10 |
| Font size zoom (Ctrl +/−/0) | keep | | M10 |
| Switch Chat/Work/Codex surfaces | cut | Single surface | — |

## A17. Computer use

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Computer use on Windows | keep | PROMPT §9. Background-capable via UIA patterns + WGC/PrintWindow (upstream is foreground-only) | M9 |
| macOS / Linux computer use | keep | **Deferred** to after parity: needs AX (macOS) and AT-SPI (Linux) backends that can't be developed or tested on the Windows build machine. The crate compiles everywhere and returns a clear "unsupported on this platform" error; Windows is complete | M9 |
| Per-app access (always allow) | keep | | M9 |
| Kill switch hotkey, takeover indicator, action log with screenshots | add | PROMPT §9 | M9 |
| Never type into password fields or touch UAC | keep | | M9 |
| `@Computer` / `@AppName` mentions | keep | | M9 |
| Locked use (macOS unlock) | cut | Security-sensitive, out of scope | — |
| Apple Messages | cut | Non-coding | — |

## A18. Browser

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| In-app browser tab (tabs, address bar, back/forward, devtools) | keep | `WebContentsView` | M9 |
| Own profile, separate from the user's | keep | `persist:odex-browser` partition | M9 |
| History search and management, clear data | keep | | M9 |
| Annotate mode: element or area comments sent to agent | keep | | M9 |
| Agent browser use over CDP (navigate, snapshot, click, type, select, scroll, screenshot, console, network) | keep | | M9 |
| Per-site allow/block, downloads need approval | keep | | M9 |
| Developer mode: full CDP access / `browser_eval` | keep | | M9 |
| Never enter credentials or payments | keep | | M9 |
| "Adjust" style tweaks | stretch | | — |
| Site tools (WebMCP) | stretch | | — |
| Browser extensions (Chrome/Edge/...) | cut | PROMPT Cut | — |

## A19. Other

| Feature | Decision | Reason / notes | MS |
|---|---|---|---|
| Appshots (capture window + accessible text) with a hotkey | keep | PROMPT §9 | M9 |
| Multi-window, per-window state | keep | | M10 |
| In-app updates | adapt | **Deferred** until releases are published: self-update needs a signed release feed (electron-updater + GitHub Releases) and code-signing certificates. Installers are built by CI on tags | M10 |
| `/feedback` with log upload | cut | No telemetry. "Open logs folder" instead | — |
| Codex Micro, Voice, Pets, Sites, Dots, Space, Remote control | cut | PROMPT Cut | — |
| Profile / insight cards / referrals / rate limits / sign-in | cut | PROMPT Cut | — |
| OpenAI-hosted web search | cut | PROMPT Cut (add search via MCP) | — |
| Image generation | cut | PROMPT Cut | — |
| Telemetry, any `*.openai.com` / `*.chatgpt.com` call | cut | PROMPT Cut. A CI grep enforces it | M0 |

## B. Slash commands

| Command | Decision | Notes | MS |
|---|---|---|---|
| `/approve` | keep | | M4 |
| `/cloud`, `/cloud-environment` | cut | | — |
| `/compact [focus]` | keep | | M5 |
| `/fast` | cut | | — |
| `/feedback` | cut | | — |
| `/fork` | keep | New thread or worktree | M3 |
| `/goal <objective>` | keep | | M8 |
| `/ide-context` | stretch | | — |
| `/init` | keep | | M3 |
| `/local` | keep | | M3 |
| `/mcp` | keep | | M7 |
| `/memories` | keep | | M8 |
| `/model` | keep | | M3 |
| `/pet` | cut | | — |
| `/personality` | cut | Use custom instructions | — |
| `/plan` | keep | | M8 |
| `/project` | keep | | M3 |
| `/reasoning` | keep | | M3 |
| `/review` | keep | | M6 |
| `/side` | keep | | M3 |
| `/status` | keep | Thread id, model, endpoint, context usage, token usage (no rate limits) | M3/M5 |
| `/task` | keep | | M3 |
| `/worktree` | keep | | M6 |
| `/share` | cut | | — |
| `/skills` | add | PROMPT §1 | M7 |
| `/context` | add | Opens the Context view (PROMPT §10.8) | M5 |
| `/doctor` | add | Runs Doctor for the current model | M3 |
| `/new` | add | Same as the New thread button | M3 |
| `/permissions` | add | Opens the permission picker | M4 |
| `/clear` | add | Starts a fresh thread in the same project | M3 |

## C. App-server protocol

Odex uses the same engine/UI split over JSON-RPC 2.0 on stdio. Odex keeps the `"jsonrpc":"2.0"` field (upstream omits it). Odex also exchanges an explicit `protocolVersion` in `initialize` instead of experimental gating.

| Upstream | Decision | Odex method |
|---|---|---|
| `initialize` + `initialized` | adapt | `initialize` (returns version, home, sandbox status, onboarding flag) |
| `thread/start|resume|fork|read|list|archive|unarchive|delete` | keep | same names |
| `thread/rollback` (removed upstream) / `thread/revert` | adapt | `thread/rollback{turnId, restoreFiles}` per PROMPT. `thread/revert` is an alias |
| `thread/search` | keep | FTS5 over title, content and branch |
| `thread/name/set`, `thread/metadata/update`, `thread/settings/update` | adapt | merged into `thread/update` |
| `thread/compact/start` | adapt | `thread/compact{focus}` |
| `thread/goal/set|get|clear` | keep | `thread/goal/set|clear`, goal included in `Thread` |
| `thread/queue/*` | adapt | `thread/queue/set` (edit, reorder, delete in one call) |
| `thread/shellCommand` | keep | `thread/shellCommand` |
| `thread/backgroundTerminals/*` | adapt | `exec/sessions`, `exec/kill` |
| `thread/approveGuardianDeniedAction` | adapt | `thread/approveOverride` (`/approve`) |
| `turn/start|steer|interrupt` | keep | same |
| `review/start` | keep | same; targets add `lastTurn` |
| `model/list` | adapt | lists vLLM-discovered and configured models with roles and context windows |
| `config/read`, `config/value/write`, `config/batchWrite` | adapt | `config/read`, `config/write{edits[]}` |
| `mcpServerStatus/list`, `mcpServer/oauth/login`, `config/mcpServer/reload`, `mcpServer/resource/read` | adapt | `mcp/list|upsert|remove|restart|logs|login|logout|readResource` |
| `skills/list`, `skills/config/write`, `hooks/list`, `plugin/*` | adapt | `skills/*`, `plugins/*` (local only), `hooks/list|trust` |
| `fuzzyFileSearch` | adapt | `fs/search` |
| `command/exec`, `process/*`, `fs/*` | adapt | Desktop main does file I/O directly. Engine exposes `exec/*` for agent sessions |
| `account/*`, `feedback/upload`, `remoteControl/*`, realtime, attestation | cut | |
| `windowsSandbox/setupStart|readiness` | adapt | `sandbox/status` |
| Notifications `thread/started`, `thread/status/changed`, `turn/*`, `item/started|completed` | adapt | `thread/updated` carries status. Deltas unified as `item/delta` with a typed `delta` |
| `turn/diff/updated`, `turn/plan/updated`, `thread/tokenUsage/updated` | keep | |
| `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `mcpServer/elicitation/request` | adapt | one `approval/request` with a typed `kind`, plus `elicitation/request` |
| `serverRequest/resolved` | adapt | `approval/resolved` |
| — | add | `provider/*`, `doctor/run`, `preset/list`, `thread/context`, `thread/context/updated`, `git/*`, `worktree/*`, `pr/*`, `automation/*`, `memory/*`, `usage/stats`, `computerUse/*`, `appshot/capture`, `browser/execute` (server→client), `terminal/read` (server→client), `secrets/store` |
