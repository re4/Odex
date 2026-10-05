# Upstream research notes: Codex desktop app + app-server protocol

Parity-audit reference for Odex. These notes summarize and paraphrase upstream sources. Field and method names are kept exact; nothing else is copied. Do not reuse upstream text or assets.

- Researched: 2026-10-04.
- Docs source: `learn.chatgpt.com/docs/*`. Every page has a `.md` twin (append `.md`), and `llms.txt` is the index. `developers.openai.com/codex/*` now redirects there.
- Protocol source: `openai/codex` `main` @ `4ad985e2` (2026-10-04). Files: `codex-rs/app-server-protocol/src/protocol/{common.rs,v1.rs,v2/*.rs}`, `src/rpc.rs`, `codex-rs/app-server/README.md`. The full API guide is at `learn.chatgpt.com/docs/app-server.md`.

> **Context shift (important for parity scope):** As of v26.707 (2026-07-09), the "Codex app" is part of the **ChatGPT desktop app** (macOS/Windows; Linux preview since 2026-08-11). A top-level switcher picks **Chat / Work / Codex**. Upstream copy now says "chat" instead of "thread/task" and "Scheduled" instead of "Automations". The Codex surface keeps the `codex://` URL scheme. For Odex, the relevant target is the **Codex surface**. The ChatGPT "Chat/Work", Space, dots, and Sites surfaces are out of scope unless noted.

---

## A. App features by area

### A1. Projects & chats (threads)
- Sidebar with **Projects** (local projects plus ChatGPT projects in a single view), **Recents**, pinned projects, and pinned chats. Sidebar sections are collapsible. Projects can be ordered manually.
- **Local project = 1+ folders.** Edit project → Add folder / Make primary. The primary folder is the default cwd and is used for Git ops, PR/worktree actions, and auto-discovery of `AGENTS.md`/skills/`config.toml`. Secondary folders are searchable and editable only. Remote projects support 1 folder.
- Project menu: Edit project, Archive chats (bulk), create permanent worktree, Open in Finder/Explorer/File Manager, remove from sidebar.
- **New chat** in a project. **New standalone chat** has no project (no cwd). **Quick chat** opens a plain ChatGPT chat that is not shown in the Codex sidebar.
- Chat row actions: rename, pin/unpin, archive, mark unread, copy deep link, copy session/thread ID, copy working dir, copy conversation (rollout) path.
- Unread indicators, "clear all unread", "next chat needing attention".
- **Activity view** (bell icon) lists unread, running, and waiting chats. Filters: Work/Chat/Pinned/Scheduled. Has "Mark all as read".
- **Search chats** matches title, conversation content, and Git branch names. No default shortcut. **Find in chat** has next/prev.
- **Fork** from the latest or any earlier message, into a new local chat or a worktree.
- **Side chat** (`/side`): an ephemeral fork that doesn't interrupt the main chat. It carries attachments, selected text, and review comments, and is unavailable inside a side chat or during review.
- **Archived chats** list in Settings with Unarchive. Archiving deletes the chat's managed worktree (after a snapshot).
- Chats can be popped out into a separate window with **Always on top**. Multi-window support, per-window zoom.
- **Share read-only snapshot** of a local thread (macOS, `/share`). Audience is anyone-with-link for personal accounts, or workspace/invited people. Secrets are redacted, and tool calls and commands are excluded.
- **Thread handoff** between local and remote hosts (moves a chat to a matching project on a connected host).
- @-mention other chats/tasks in the composer.
- Undo/redo of the last app action.
- Subagent activity is shown in the chat, with stable identicons per background subagent and subagent diff stats in the composer.
- Protocol-backed but UI-implied: user-defined **thread sections** (name plus icon/color appearance, ordered), and project assignment of threads.

### A2. Run modes / where a chat runs
- **Work in:** This computer (Local) / Cloud. There is a separate **Worktree** toggle for Git projects. Slash forms: `/local`, `/worktree`, `/cloud`, `/cloud-environment`.
- **Worktree:**
  - Pick the starting branch (main, a feature branch, or the current branch including uncommitted changes). The worktree is created in detached HEAD under `$CODEX_HOME/worktrees` (root configurable).
  - "Create branch here" turns the worktree into a branch.
  - **Hand off** moves a chat plus its code between Local and Worktree, and returns it to the same worktree later.
  - `.worktreeinclude` lists ignored files to copy into managed worktrees. An ignored `AGENTS.override.md` is copied automatically.
  - Permanent worktrees appear as their own project and are never auto-deleted.
- **Worktree cleanup:**
  - Keeps the most recent 15 managed worktrees by default. The limit and auto-delete toggle are configurable.
  - Never deletes worktrees tied to pinned or in-progress chats, or permanent worktrees.
  - Snapshots before deleting, and offers restore when the chat is reopened.
- **Cloud:** runs in a published, reusable cloud environment. A Create environment flow prepares and tests the setup. Cloud tasks can be continued from web/mobile.
- **Remote hosts:** SSH projects. The desktop app can also be driven from the mobile app (see A19).
- **Windows:** the agent runs natively (PowerShell plus native Windows sandbox) or in WSL2 (switch in Settings, then restart). The terminal shell is chosen independently.

### A3. Composer
- Send key: Enter, or require Cmd/Ctrl+Enter for multiline prompts (setting). Up-arrow restores the previous prompt when the composer is empty.
- **Follow-up while running:** **Steer** (inject into the current turn) or **Queue** (run next).
  - The default comes from a setting, and a modifier inverts it for one message.
  - Queued messages show above the composer and can be edited, reordered (drag), sent now, or deleted.
- Mentions:
  - `@` for files, folders, apps/plugins, skills, other chats, `@Browser`/`@Chrome`/`@Computer`/`@AppName`, and an MCP submenu.
  - `$` for skills.
  - `/` for slash commands, which work mid-draft.
- Attachments: files, images, pasted non-image files, appshots, videos (local embeds).
- Pickers in or below the composer:
  - Model (Ctrl/Cmd+Shift+M opens the model picker), reasoning effort, Fast tier (`/fast`).
  - **Permissions** mode, **IDE context** toggle, project picker, Work-in/Worktree/branch selectors, local environment selector.
- **Plan mode** (`/plan`) has plan-question prompts and notifications.
- **Goal mode** (`/goal`): a progress row above the composer with pause/resume/edit/clear and a live timer.
- **Voice** (realtime voice chat, which can delegate to and steer other threads) and **dictation** (cleanup, custom dictionary for names, paths, and symbols).
- Floating composer (v2). Warning when the selected model is downgraded or rerouted. Inline usage-limit errors with reset timing.
- Live questions from the agent (`request_user_input`) can be answered while it keeps working, and the draft is preserved.
- Context-aware **suggested prompts** (follow-ups and tasks to resume) on start or return.
- Approval cards inline in the transcript: Enter = approve, Esc = decline. Custom feedback text can be submitted with Cmd/Ctrl+Enter. MCP approval panels have "Don't ask again".

### A4. Review pane (diff)
- Shows the Git state of the repo, not only agent edits.
- Scopes: **Unstaged** (default), **Staged**, **Commit**, **Branch** (vs base), **Last turn**.
- Multi-repo project: a repo selector. "Last turn" can show **All repos**.
- Stage / unstage / revert at three levels: whole diff (Stage all / Revert all), per file, per hunk. The same file can appear in both staged and unstaged views.
- **Inline comments:** hover a line, click `+`, then type. Comments support @-mentions and skill mentions. They are collapsible and act as guidance for the next message.
- Review findings render as inline comments.
- Navigation and file actions:
  - File-tree sidebar ordered consistently with the diff, with an open-in menu.
  - Clicking a filename opens it in the chosen editor. Cmd-clicking a line opens that line.
  - Clicking the file header background expands or collapses the file.
  - Search inside the diff (Cmd/Ctrl+F seeds from the current selection). Whitespace handling. Edited-files state.
- Open the review tab with Ctrl+Shift+G (both platforms).
- `/review` → choose "against base branch" or "uncommitted changes". It reports prioritized findings without editing.
- **Review delivery:** Inline (in the current chat) or Detached (a separate review chat). Upstream docs disagree on where this setting lives (see G).
- Requires a Git repo. The app offers to `git init` otherwise.

### A5. Git & pull requests
- Commit, push (push modal with choices), and create a PR from the app. Settings cover branch-name prefix, force-push allowed, commit-message prompt, and PR-description prompt (AI-generated messages).
- PR status badges (draft/open/merged/closed) on chat rows and the PR button. The sidebar shows PR status updates.
- **Code Review (PR Chat)**, a bundled plugin that can be pinned to the sidebar:
  - Inbox: Assigned to me / Assigned to my team / Authored by me, plus Pinned PRs.
  - **Summary**: description, activity timeline, comments, checks, **Stack** (dependent PRs), and **Threads** (linked chats).
  - **Changes**: diff with comments and per-file "Mark as viewed".
  - **Review with Codex** starts a review chat. Review instructions are editable via a gear or Settings > Code Review, with optional output-format presets.
  - **Submit review**: Comment / Approve / Request changes. Posting a comment sends it immediately.
  - **Fix** on failing checks attaches the check output to the chat without sending.
  - Inspect, edit, accept, or reject proposed patches.
- GitHub is GA. GitLab merge requests are in preview.
- Protocol: PRs are linked to chats as durable **thread attachments** (`attachmentType: "pull_request"`).

### A6. Side panel / workspace tabs
- Right panel with tabs. The layout cycles between **full view**, **split view**, and **hidden tabs**. A separate toggle switches between chat and tabs.
- New tab menu: Terminal, Browser, File.
- Drag to reorder tabs. Tab widths and scroll positions stay stable.
- Chat side panel:
  - Agent **plan**, **sources** (open or download files), **generated files / artifacts**, **chat summary**, and a **Git summary**.
  - Artifact cards appear for generated-file citations.
- **File tree** (toggle) and **file search** (Cmd/Ctrl+P, also from the command menu).
- File viewer:
  - Rich previews for images, PDF, and Markdown. Markdown and code are editable in-app.
  - Annotations ("ask Codex to revise the selection"). Back navigation, recent files, remembered scroll position.
- **Artifact viewer** for docs, slides, sheets, and PDF. HTML renders as a live preview with a source toggle. Annotations work on all of these. Automatic preview can open when a task finishes.
- Image viewer: lightbox with zoom and download, **Focused** vs **Canvas** view, and per-image comments for targeted edits.
- Bottom panel (Cmd/Ctrl+J).
- Embedded MCP-app panels (inline or fullscreen) that keep their state across fullscreen changes and reloads.
- Mermaid diagrams render inline.

### A7. Integrated terminal
- A terminal per chat, scoped to the chat's project or worktree. Multiple terminal tabs per thread.
- Toggle with Ctrl+` (both platforms). Clear with Ctrl+L (Cmd+K is the command palette, not clear, except when the terminal is focused, where it clears).
- **The agent can read the current terminal output**, for example dev server status or a failed build.
- Settings: **Default terminal location** (bottom panel vs right panel) for the shortcut and env actions, and default shell on Windows (PowerShell / cmd / Git Bash / WSL).
- Word and line jump shortcuts. Copy/paste works on Windows.
- Protocol extras: background terminals list/terminate/clean, and `thread/shellCommand` (user-initiated, unsandboxed).

### A8. Project actions / local environments
- Stored in the `<project>/.codex/` folder and editable from Settings. Can be committed to share with teammates.
- **Setup scripts** run when a new worktree is created. There is a default script plus optional macOS/Windows/Linux overrides.
- **Actions** are named scripts with an icon. They appear in the top bar, run in the integrated terminal, and can have per-platform variants. Shortcut "Run environment action 1". Actions are editable.
- **Open in** menu: preferred editor (global default plus per-project override). Custom handlers are defined via `desktop.custom_file_handlers.<id>` in `config.toml`.

### A9. Permissions & approvals
- Permission modes (control below the composer):
  - **Ask for approval** (default, always available): workspace sandbox, asks before crossing the boundary.
  - **Approve for me** (= Auto-review): same sandbox, but boundary requests go to a reviewer agent.
  - **Full access**: no sandbox.
- Extra modes must first be enabled in Settings > General > Permissions. Managed policy can disable modes.
- Advanced: named **permission profiles** (beta), `config.toml` sandbox/approval keys, and **rules** (command-prefix allow/prompt/deny).
- Approval kinds:
  - Command exec: accept / accept for session / accept + execpolicy amendment / network-policy amendment / decline / cancel-turn.
  - File change: accept / accept for session / decline / cancel.
  - Extra permissions (network/filesystem subset, turn or session scope).
  - MCP elicitation (form / URL), and tool "request user input" questions.
- Auto-review shows a review item with status (in progress / approved / denied / timed out / aborted) and risk level. `/approve` retries one recent auto-review denial.
- Full-access warnings, and an extra confirmation dialog when combining Full access with the Ultra effort level.
- Hooks require an in-app **trust review** before running.
- Per-capability approvals live elsewhere: Computer Use per-app (Always allow), Browser per-site allow/block, Apple Messages per-send or always-allow per chat.

### A10. MCP
- Settings > **MCP servers**: Add server (name, STDIO command or Streamable HTTP URL). Enable/disable, Authenticate (OAuth), Restart. Recommended servers.
- `/mcp` shows connected-server status. The composer has MCP shortcuts (install keyword suggestions, Add context → MCP submenu).
- Config is shared with CLI and IDE via `config.toml` `[mcp_servers.<name>]`:
  - Stdio keys: `command`, `args`, `env`, `env_vars`, `cwd`.
  - HTTP keys: `url`, `auth`, `bearer_token_env_var`, `http_headers`, `env_http_headers`, `http_headers_helper`.
  - Common keys: `startup_timeout_sec`, `tool_timeout_sec`, `enabled`, `required`, `enabled_tools`, `disabled_tools`, `default_tools_approval_mode` (`auto|prompt|writes|approve`), `tools.<t>.approval_mode`.
- MCP Apps UI renders tool UIs inline or fullscreen. Server `instructions` are honored.

### A11. Skills & plugins
- **Skills** page (`codex://skills`): browse and manage, enable/disable. Invoke with `$name`. Enabled skills also appear in the `/` list. `$skill-creator` is a built-in skill.
- **Plugins** directory (shared with ChatGPT):
  - Tabs: OpenAI / workspace / Personal (Created by me, Shared with me), plus an **Installed** row. Search, marketplace and category filters, keyboard navigation.
  - Install with `+`, then connect/auth.
  - Plugin management moved into Settings in 26.707.
- A plugin bundles skills, MCP servers, app connectors, hooks, and browser extensions. Marketplaces can be local, repo, or Git. Plugins can be shared through marketplace sources.
- Record & Replay (macOS) records a demonstrated workflow as a reusable skill.
- Import from other agents (Claude Code, Claude Cowork, Cursor) covers instructions, settings, skills, plugins, projects, and recent work, with optional auto-sync (Settings > Import).

### A12. Automations ("Scheduled")
- **Scheduled** sidebar view works as an inbox of runs with unread indicators. Run history supports bulk mark-read and archive.
- **Standalone scheduled task**:
  - Every run starts a new chat. A task can target 1+ projects.
  - Runs in the local checkout or a background worktree (Git repos). Non-Git projects run in place.
  - Model and effort are explicit or default.
  - Schedule presets plus custom schedule. Advanced: edit the RFC 5545 `RRULE`.
  - Templates. Titles and icons in the sidebar.
  - The prompt can include `$skill` and `@` mentions.
- **Scheduled task inside a chat** ("heartbeat"): re-wakes the same chat with its context. Supports minute-based intervals plus daily/weekly.
- Can be created or updated by asking in chat, or by a skill.
- Runs unattended with default sandbox settings and `approval_policy = "never"` when policy allows, otherwise falling back to the selected mode. Requires the app running and the machine awake.
- Deep link `codex://automations` opens the create flow.

### A13. Memories
- Off by default. Enable in Settings > Personalization > Enable memories (or `[features] memories = true`).
- `/memories` per chat: whether this chat may *use* existing memories and/or *contribute* to future memories.
- Stored locally under `~/.codex/memories/`. Generated in the background after a chat has been idle. Secrets are redacted. Skipped when rate-limit headroom is low.
- Computer History (macOS, opt-in) turns app and web activity into memories and a timeline.

### A14. Notifications
- Turn-completion alerts: **never / only when the app is in the background / always**. Separate toggles for permission-request and question notifications. An option prompts for OS notification permission.
- Activity view, unread badges, tray/menu-bar presence (Windows system tray keeps the app resident), and a usage-limit display in the tray.
- Pets show a status per chat: Running / Needs input / Ready / Blocked.

### A15. Settings
See section D.

### A16. Navigation / command palette
- **Command menu** (Cmd/Ctrl+K or Cmd/Ctrl+Shift+P):
  - App commands and file search.
  - **Unread chats** section, with the most recent one preselected.
  - Theme switcher, New Quick Chat, Show/Hide pet.
- Header back/forward buttons (plus mouse back/forward), go to chat 1–9, open recent chat 1–6, previous/next chat or tab.
- **Settings search** across panels. Keyboard-shortcut editor with keystroke search and reset-all.
- Deep links (`codex://`), see C.3.

### A17. Computer Use
- Installed as a plugin. Operates GUI apps by screenshot, click, and type on macOS (Screen Recording plus Accessibility permissions) and Windows (foreground only, on the active desktop).
- Invoke with `@Computer` / `@AppName` or by asking. Per-app permission prompts with **Always allow**. Settings > Computer Use manages always-allowed apps and connected browsers.
- macOS **locked use**: an auth plug-in lets an active, trusted Computer Use turn temporarily unlock the Mac. Displays are covered during the unlock, and local input relocks.
- Windows per-app policy lives in `[computer_use.windows] always_allowed_app_ids`.
- Admins can disable it via `requirements.toml` `[features].computer_use = false`.

### A18. Built-in browser
- Browser tab (Cmd/Ctrl+T) in the side panel. Uses its own profile, separate from the user's browser. The user can sign in manually.
- Address bar searches history, falling back to a Google search. Settings > Browser has history management, clear data by time range, download location, ask-where-to-save, and profile import (where available).
- **Annotate mode** (browse ↔ comment toggle): click an element or drag an area, then comment. **Adjust** gives granular style tweaks (font, text, spacing, color) with a live preview. Comment markers stay aligned on scroll and zoom.
- **Agent browser use** (`@Browser`): opens pages, clicks, types, inspects, and screenshots.
  - Asks per website unless the site is already allowed. Allowed and blocked site lists are kept.
  - Confirms sensitive actions. Can't automate file uploads.
  - Can download and extract page assets, and has a read-only JS extraction sandbox.
- **Developer mode** (Settings > Browser): "Enable full CDP access" for profiling, console, network, DOM, and styles. Each use needs explicit approval.
- **Site tools (WebMCP)**: the agent uses tools a website exposes.
- **Browser extension** (`@Chrome`) for Chrome/Edge/Brave/Opera/Vivaldi: tab mentions, side chat in the extension, working in background tabs.

### A19. Other
- **Pets**:
  - Optional floating animated companion: built-in or custom (sprite sheet), or **Mini** controls only. Reset size, reduced animation.
  - Quick chat from the floating controls (global hotkey Option+Space on macOS, Win+Alt+P on Windows), with `@` and `$` support.
  - Follows thread status. Appshots can be sent to the pet. `/pet` wakes or tucks it.
- **Appshots**:
  - Capture the frontmost window (image plus accessible text) into a chat. Trigger by pressing both Cmd keys (macOS) or both Alt keys (Windows), or a custom hotkey.
  - Destination: Automatic (reuses a chat touched in the last 60 s, else a new chat) / Current chat / New chat.
- **Voice**: realtime voice conversation that can start, check, or steer work in other threads. Optional screen context (appshot).
- **Codex Micro**: a hardware keyboard integration (agent keys, command keys, insert-text keys).
- **Profile**: usage insights (lifetime and peak tokens, streaks, longest task, token activity), a profile card, display name, username, and avatar. Invite friends or coworkers.
- **Appearance/themes**: base theme, accent/background/foreground colors, UI and code fonts. Themes are shareable, and bundled third-party themes are included. Custom macOS Dock icon.
- **Remote**:
  - Pair a phone (QR, one-to-one) or another desktop to this host. Settings > Connections has Control this Mac or PC, Control other devices, and SSH.
  - Keep-awake option. Remote users can start, steer, and approve chats and review diffs.
- **IDE extension sync**: shared active chats and editor context when the app and IDE are open in the same project. IDE context toggle.
- **Usage**: rate-limit display, banked rate-limit resets, add-credits nudges.
- **Misc**:
  - In-app updates (MSIX / Windows Store / auto-update). Language override.
  - "Prevent sleep while running".
  - Onboarding with role choices. Feedback dialog (`/feedback`, Help menu) with optional logs.

---

## B. Slash commands (app composer)

These come from the upstream app reference table. Availability varies by environment and account. Enabled skills and custom prompts (`/prompts:<name>`, deprecated) also appear in the list.

| Command | What it does |
|---|---|
| `/approve` | Retry once a recent action that automatic review denied (only when auto-review is active). |
| `/cloud` | Run this chat in the cloud (when cloud execution is available). |
| `/cloud-environment` | Pick the cloud environment for this chat. |
| `/compact` | Compact (summarize) the current chat's context. |
| `/fast` | Toggle the model's catalog Fast service tier (when offered). |
| `/feedback` | Open the feedback dialog, with optional log upload. |
| `/fork` | Copy a local chat into a new local chat or a worktree. |
| `/goal` | Set a persistent goal (Goal mode). The progress row offers pause/resume/edit/clear. |
| `/ide-context` | Toggle sharing of IDE editor context. |
| `/init` | Generate an `AGENTS.md` scaffold for the project. |
| `/local` | Run the chat in the selected local project. |
| `/mcp` | Show MCP server status. |
| `/memories` | Per-chat memory use/generation controls (when Memories is enabled). |
| `/model` | Choose the model for this chat. |
| `/pet` | Wake or hide the desktop pet. |
| `/personality` | Choose a response personality (when the model supports it; deprecated in protocol). |
| `/plan` | Toggle plan mode. |
| `/project` | Choose the project for new chats. |
| `/reasoning` | Choose the reasoning effort for this chat. |
| `/review` | Start code review (uncommitted changes or against a base branch). |
| `/side` | Open a temporary side chat (ephemeral fork) without interrupting the main chat. |
| `/status` | Show chat ID, context-window usage, and rate limits. |
| `/task` | Start a chat without a project. |
| `/worktree` | Run the chat in a new Git worktree. |

Mentioned elsewhere but not in the app table:
- `/share`: share a read-only snapshot, "where slash commands are available".
- CLI-only commands for comparison: `/permissions`, `/new`, `/resume`, `/diff`, `/experimental`, `/keymap`, `/statusline`, `/title`, `/theme`, `/vim`, `/raw`, `/import`, `/debug-config`, `/voice`.

---

## C. Keyboard shortcuts & deep links

### C.1 Default shortcuts (macOS vs Windows)

Linux matches Windows, except that "Run environment action 1" uses Super+Shift+D. "(C)" marks entries that apply only to the Codex surface.

| Action | macOS | Windows |
|---|---|---|
| Command menu | ⌘⇧P / ⌘K | Ctrl+Shift+P / Ctrl+K |
| Settings | ⌘, | Ctrl+, |
| Keyboard shortcuts | ⌘/ | Ctrl+/ |
| Open folder (Codex/Work) | ⌘O | Ctrl+O |
| Back / forward | ⌘[ / ⌘] (or mouse) | Ctrl+[ / Ctrl+] (or mouse) |
| Font size + / − / reset | ⌘+ / ⌘− / ⌘0 | Ctrl+ + (or =) / Ctrl+− / Ctrl+0 |
| Toggle sidebar | ⌘B | Ctrl+B |
| Toggle bottom panel (C) | ⌘J | Ctrl+J |
| Toggle terminal (C) | ⌃` | Ctrl+` |
| Clear terminal (focused) | ⌃L / ⌘K | Ctrl+L / Ctrl+K |
| Clear all unread (C) | ⇧Esc | Shift+Esc |
| Undo / redo app action | ⌘Z / ⌘⇧Z | Ctrl+Z / Ctrl+Y or Ctrl+Shift+Z |
| Close tab/window | ⌘W | Ctrl+W / Ctrl+F4 |
| Full screen | ⌘⌃F | F11 |
| Quit | ⌘Q | Ctrl+Q |
| New chat | ⌘N / ⌘⇧O | Ctrl+N / Ctrl+Shift+O |
| New standalone chat (C) | ⌘⌥O | Ctrl+Alt+O |
| Quick chat (ChatGPT) | ⌘⌥N | Ctrl+Alt+N |
| Temporary chat (ChatGPT) | ⌘⇧N | Ctrl+Shift+N |
| Archive chat | ⌘⇧A | Ctrl+Shift+A |
| Mark unread | ⌘⇧U | Ctrl+Shift+U |
| Pin/unpin chat | ⌘⌥P | Ctrl+Alt+P |
| Rename chat | ⌘⌥R | Ctrl+Alt+R |
| Side chat (C) | ⌘⌥S | Ctrl+Alt+S |
| Search chats | unassigned | unassigned |
| Find in chat / next / prev | ⌘F / ⌘G / ⌘⇧G | Ctrl+F / Ctrl+G / Shift+F3 |
| Prev chat or tab | ⌃⇧Tab / ⌘⇧[ / ⌘⌥← | Ctrl+Shift+Tab / Ctrl+Shift+[ / Ctrl+PgUp |
| Next chat or tab | ⌃Tab / ⌘⇧] / ⌘⌥→ | Ctrl+Tab / Ctrl+Shift+] / Ctrl+PgDn |
| Next chat needing attention (C) | ⌘⌥A | Ctrl+Alt+A |
| Recent chat 1–6 | ⌘⌥1–6 | Ctrl+Alt+1–6 |
| Go to chat 1–9 | ⌘1–9 | Ctrl+1–9 |
| Model picker | ⌃⇧M | Ctrl+Shift+M |
| Project picker | ⌘⌥⇧O | Ctrl+Alt+Shift+O |
| Voice chat | ⌃⇧V | Ctrl+Shift+V |
| Dictation | ⌃⇧D | Ctrl+Shift+D |
| Restore previous prompt (empty composer) | ↑ | ↑ |
| Approve / decline open request | Enter / Esc | Enter / Esc |
| Switch Chat / Work / Codex | ⌃1 / ⌃2 / ⌃3 | Alt+1 / Alt+2 / Alt+3 |
| Toggle Activity view | ⌘⌥U | Ctrl+Alt+U |
| Run environment action 1 | ⌘⇧D | Win+Shift+D |
| Search files (C) | ⌘P | Ctrl+P |
| Toggle file tree (C) | ⌘⇧E | Ctrl+Shift+E |
| Open review tab (C) | ⌃⇧G | Ctrl+Shift+G |
| Switch chat ↔ tabs | ⌘⌥B | Ctrl+Alt+B |
| New browser tab | ⌘T | Ctrl+T |
| Cycle layout (full/split/hidden) | ⌘⇧B | Ctrl+Shift+B |
| Enter/exit full view | ⌘⇧F | Ctrl+Shift+F |
| Go to line / focus address bar | ⌘L | Ctrl+L |
| Browser back / forward | ⌘← / ⌘→ | Alt+← / Alt+→ |
| Browser reload / hard reload | ⌘R / ⌘⇧R | Ctrl+R / Ctrl+Shift+R |
| Copy browser URL | ⌘⇧C | Ctrl+Shift+C |
| Browse ↔ comment mode | ⌘. | Ctrl+. |
| Copy conversation path (C) | ⌘⌥⇧C | (not listed) |
| Copy chat deep link | ⌘⌥L | Ctrl+Alt+L |
| Copy session ID | ⌘⌥C | Ctrl+Alt+C |
| Copy working directory (C) | ⌘⇧C | Ctrl+Shift+C |
| Appshot | both ⌘ keys | both Alt keys |

Global and other bindings:
- Pet quick chat: ⌥Space (macOS) / Win+Alt+P (Windows).
- The Appshot hotkey is configured separately under Settings > Appshots.
- Custom approval feedback submits with Cmd/Ctrl+Enter.
- In IDE-style settings, Cmd/Ctrl+Shift+Enter inverts steer/queue for one message.
- All shortcuts can be rebound in Settings > Keyboard Shortcuts. Search by name or by keystroke, reset individually or all.

### C.2 Command menu contents (observed from changelog/docs)
- Commands, file search, Unread chats section, theme switcher, New Quick Chat, Hide/Show pet, and recent chats.

### C.3 Deep links (`codex://`)
- `threads/new`, `threads/new?<q>`, `new?<q>` with query params:
  - `prompt` (prefills the composer, does not auto-send; may contain `[@Name](plugin://name@marketplace)`)
  - `path` (absolute dir)
  - `originUrl` (match a workspace root by Git remote)
  - `new?` requires at least one param.
- `threads/<thread-id>`
- Settings links:
  - `settings`, `settings/browser-use`, `settings/computer-use/google-chrome`
  - `settings/connections`, `settings/connections/computer`, `settings/connections/devices`, `settings/connections/ssh`
  - `settings/connections/ssh/add?name=<ssh-config-host>`
  - Unknown subpaths go to the main Settings page.
- `skills`
- `automations` (opens the create flow)
- Plugins:
  - `plugins/install/<name>?marketplace=<m>`
  - `plugins/<plugin-id>` (optional `hostId`, `source=manage`)
  - `plugins/<name>?marketplacePath=<abs>` (optional `mode=share`)
- `pets/install?name=&imageUrl=` (https only; optional `description`, `spriteVersionNumber=1|2`).

---

## D. Settings reference (per panel)

Upstream does not publish a single exhaustive per-option list. This table is assembled from the settings, developer-settings, feature, and changelog pages.

| Panel | Options |
|---|---|
| **General** | Require Cmd/Ctrl+Enter for multiline; **Prevent sleep while running**; **Follow-up behavior** (Steer vs Queue, with an invert shortcut); **Permissions** (enable *Auto-review / Approve for me* and *Full access* modes); **Code review → Review delivery** (Inline/Detached, also cited under Git); **Default terminal location** (bottom vs right panel); project and terminal behavior (where files open / default editor, how much command output shows in chat, where terminal tabs open); language; default view (make Codex the default); agent environment (Windows native vs WSL, needs restart); integrated terminal shell (Windows); in-app updates |
| **Profile** | Activity insights (lifetime and peak tokens, streaks, longest task, token activity); picture, display name, username; save or share a profile card; invite friend/coworker |
| **Keyboard Shortcuts** | List, rebind, reset one or all; search by command name or by pressed keystroke |
| **Notifications** | Turn completion: never / background only / always; permission-request notifications on/off; question notifications on/off; prompt for OS permission |
| **Appearance** | Base theme; accent/background/foreground colors; UI font; code font; share theme; (macOS) Dock icon variant |
| **Pets** | Choose built-in or custom pet, or Mini; show/hide; reset size; reduce animation |
| **Appshots** | Global hotkey; destination (Automatic / Current chat / New chat) |
| **Browser** (`browser-use`) | Install/enable bundled Browser plugin; set up browser extension; allowed and blocked websites; browsing history (search, reopen, delete); clear browsing data (time range, data types); profile import; download folder, reset, ask where to save; **Developer mode → Enable full CDP access** |
| **Computer Use** | Plugin install state; app access with an **Always-allowed apps** list; connected browsers (Manage); Apple Messages "always allowed to send" list; **locked use** (macOS) |
| **Personalization** | Default personality (Friendly / Pragmatic / None); custom instructions (written to personal `AGENTS.md`); **Enable memories** |
| **Suggested prompts** | Context-aware suggestions on/off |
| **Memories** | Enable; (Computer History is its own opt-in) |
| **Archived chats** | List with date and project; Unarchive |
| **Worktrees** | **Worktree root**; number of managed worktrees to keep (default 15); automatic cleanup on/off |
| **Git** | Branch name prefix/naming; allow force push; commit-message generation prompt; PR-description generation prompt; Review delivery |
| **Code Review** | Review instructions text used by "Review with Codex" |
| **MCP servers / Integrations** | Add server (STDIO or Streamable HTTP), enable/disable, Authenticate (OAuth), Restart; recommended servers |
| **Plugins / Skills** | Installed plugins management (moved into Settings in 26.707); skills enable/disable |
| **Hooks** | Discovered hooks; trust review flow |
| **Local environments** | Per-project setup scripts (default plus per-OS) and actions (name, icon, script, per-OS) |
| **Connections** | Control this Mac or PC (pair via QR); Control other devices; SSH hosts (add from `~/.ssh/config`, auto-connect); keep awake; enable Computer Use; install Chrome extension |
| **Import** | Import from Claude Code / Claude Cowork / Cursor; automatic updates (keep in sync) |
| **Workspace settings** | Workspace-level settings (added 26.401; contents not documented) |
| Agent configuration | Common controls in-app; everything else lives in `config.toml` (shared with CLI and IDE) |

`config.toml` keys that drive app behavior (non-exhaustive):
- Model and reasoning: `model`, `review_model`, `model_reasoning_effort`, `plan_mode_reasoning_effort`, `model_reasoning_summary`, `model_verbosity`, `service_tier`, `personality`.
- Permissions: `approval_policy` (`untrusted|on-request|never|{granular}`), `approvals_reviewer` (`user|auto_review`), `sandbox_mode` (`read-only|workspace-write|danger-full-access`), `[sandbox_workspace_write]`, `default_permissions` / `[permissions.<id>]`, `auto_review.*`.
- Tools: `web_search` (`cached|live|disabled`), `notify`, `[mcp_servers.*]`.
- Memories: `[features] memories`, `memories.{generate_memories,use_memories,disable_on_external_context,min_rate_limit_remaining_percent,extract_model,consolidation_model}`.
- Desktop and Computer Use: `desktop.custom_file_handlers.<id>.{label,icon,command,args,input,supports_ssh}`, `[computer_use.windows] always_allowed_app_ids`.
- Feature flags: `features.{goals,multi_agent,fast_mode,prevent_idle_sleep,hooks,apps,plugins,remote_plugin,unified_exec,shell_snapshot,network_proxy,...}`.
- Admin `requirements.toml` pins for the app: `features.{in_app_browser,in_app_chat,in_app_dictation,in_app_local_automation,in_app_updates,browser_use,browser_use_external,browser_use_full_cdp_access,computer_use,guardian_approval,plugin_sharing,...}`.

---

## E. Recent changelog (≈ Apr–Oct 2026, app-relevant)

| Date | Version | Features |
|---|---|---|
| 2026-10-01 | CLI 0.160.0 | Start sessions outside a project; queued messages survive reconnect; opt-in Guardian review context |
| 2026-09-29 | — | **GPT-6.1 Sol** becomes the default in the bundled catalog. DevDay: dots, Space, cloud environments, Codex Security Cloud, Plugin Extensions, MCP Events, Annotations Extensibility API, Astra Ultrafast |
| 2026-09-29 | CLI 0.159 | `instant_interrupt`; app-server can paginate thread items from an item anchor; prompt suggestions removed from TUI; threads archivable before the first turn |
| 2026-09-22 | — | GPT-6 Sol / Luna in Codex |
| 2026-09-14 | — | GPT-5.3-Codex-Spark removed; GPT-5.5 retires 2026-10-14 |
| 2026-09-11 | 26.908 | **Quick chat from floating Pet controls** (global hotkey; `@`/`$`; bell to follow threads); **Appshots on Windows**; open or download files from the Sources panel; Codex Micro "Insert text"; pet size reset; unfinished response comments persist across chat switches |
| 2026-09-05 | — | `codex mcp-server` removed (deprecated 08-24); use app-server, which upstream labels experimental and not for production |
| 2026-08-25 | — | Browser extension for Edge/Brave/Opera/Vivaldi; **site tools (WebMCP)** in the built-in browser; cloud browser sign-in (Work) |
| 2026-08-25 | — | Event-triggered scheduled tasks (Gmail/Slack/GitHub; web and mobile only, not desktop) |
| 2026-08-20 | — | Apple Messages plugin; **shared read-only thread snapshots** (macOS); **unified pinned threads** across desktop and iOS; Site co-editing and URL edit |
| 2026-08-19 | — | GitLab in Codex cloud (beta) |
| 2026-08-13 | — | Computer History (macOS) |
| 2026-08-11 | — | **Linux desktop preview** (.deb/.rpm); **import from Claude Code / Cowork / Cursor** with auto-sync |
| 2026-07-30 | 26.727 | Browser address-bar history search and history management; Chrome extension tab mentions and side chat; **multi-repo review**; **image editing** (Focused/Canvas, comments); **Activity view** (bell, ⌘⌥U) |
| 2026-07-29 | — | Sign in with ChatGPT (beta) for plugins |
| 2026-07-23 | 26.715 | **ChatGPT Voice** (GPT-Live) with delegation to threads; **multi-folder local projects** (primary folder) |
| 2026-07-09 | 26.707 | **Codex merged into the ChatGPT desktop app**; edit Markdown and code in-app with inline annotations; **PR Chat** (review PRs, inline feedback, accept/reject patches); plugin management moved into Settings; Full access + Ultra warning dialog; better subagent activity display |
| 2026-06-25 | — | Codex Remote GA; one-to-one QR pairing; DigitalOcean plugin (remote workspace) |
| 2026-06-18 | 26.616 | Record & Replay (macOS); bulk actions on automation runs; **thread handoff local ↔ remote host**; SSH deep links |
| 2026-06-11 | 26.609 | Rate-limit reset banking and referrals; **Browser Developer mode (CDP)**; **`/init` in app**; macOS Dock icons; Windows per-app Computer Use access; **Unread chats in command menu**; browser use 2× faster; Cmd/Ctrl+Enter submits approval feedback |
| 2026-06-09 | 26.608 | Import from Claude Code/Cowork in onboarding; **revamped plugins screen** (tabs, filters, keyboard nav); Settings search across more panels |
| 2026-06-04 | 26.602 | Profile activity insights and share cards |
| 2026-06-02 | — | Sites (preview) via the Sites plugin; Sites in the sidebar |
| 2026-06-01 | 26.601 | **Default terminal location** setting (bottom vs right panel); Amazon Bedrock provider |
| 2026-05-29 | 26.527 | **Computer Use on Windows**; remote control of Windows hosts; Profile stats; thread coordination incl. background threads; **search includes content and branch names**; subagent identicons; shortcut keypress search and reset-all |
| 2026-05-21 | 26.519 | **Appshots (macOS)**; **Goal mode GA**; locked/remote Computer Use; plugin sharing via marketplaces (Business); **browser annotation style "Adjust"**; browser asset extraction and JS sandbox |
| 2026-05-14 | — | Mobile access to a Mac host (Remote); Hooks GA; access tokens |
| 2026-05-08 | 26.506 | In-app **hook trust review**; message edits preserved across thread switches |
| 2026-05-07 | — | Codex Chrome extension |
| 2026-05-05 | 26.429 | **Dictation cleanup and dictionary**; image lightbox zoom and download |
| 2026-04-24 | 26.423 | Voice delegation tooltip; review search fixes; MCP app panels keep state |
| 2026-04-23 | — | GPT-5.5; **browser use in-app** (Browser plugin, allow/block sites); **automatic approval review** UI (status and risk) |
| 2026-04-20 | 26.417 | Local branch search; paste non-image files; **collapsible sidebar sections**; tray usage limits; **command-palette theme switcher** |
| 2026-04-16 | 26.415 | Early **in-app browser** with page comments; **Computer Use (macOS)**; **projectless chats**; **thread automations (heartbeat)**; **task side panel** (plan/sources/artifacts/summary); suggested prompts; **PR inspection in sidebar**; **artifact viewer**; Memories; SSH remotes (alpha); **multiple terminals**; menu bar / tray; **multi-window**; Intel Mac |
| 2026-04-12 | 26.410 | Command-menu file search (⌘P); rich previews in the file viewer; **terminal tabs per thread**; selected-text "Ask Codex" overlay |
| 2026-04-10 | 26.409 | PR activity timeline and PR-page comments; push modal choices; **workspace file tabs in the side panel**, drag-reorder tabs; run-action editing |
| 2026-04-09 | 26.406 | Collapsible inline review comments; **inline/detached review**; Git summary and Sources in the side panel; New Quick Chat; local video embeds |
| 2026-04-01 | 26.401 | Workspace settings; MCP approval "Don't ask again"; Windows MSIX updater and tray; `@` mentions in automation composer; subagent diff stats; artifact cards; heartbeat automations |

Older baseline (Feb–Mar 2026), useful for parity:
- App launch (2026-02-02).
- Queued-message drag reorder and model-downgrade warning (26.217).
- PR status badges and worktree-retention setting (26.227).
- Handoff Local ↔ Worktree and worktree auto-cleanup toggle (26.303).
- Windows app with native sandbox and WSL option (26.304).
- Agent reads the integrated terminal (26.311).
- Themes; revamped automations with local/worktree, model/effort, templates (26.312).
- Header back/forward; Open in Finder/Explorer (26.313).
- Fork from an earlier message; slash commands for model/reasoning; plan-mode question notifications (26.317).
- Skills in the `@` menu; Cmd+F seeded from selection (26.318).
- Floating composer v2; terminal word/line jumps (26.320).
- Thread search; archive all threads in a project; VS Code settings sync (26.323).
- Skills/plugins pages redesigned; per-window zoom; automation titles and icons (26.324).
- Plugins (2026-03-25).

---

## F. App-server protocol (`codex app-server`)

### F.1 Wire format & transport
- JSON-RPC 2.0 *shape* without the `"jsonrpc"` field.
  - Request: `{id, method, params?, trace?}`, where `trace` is optional W3C trace context.
  - Response: `{id, result}`. Error: `{id, error:{code, message, data?}}`. Notification: `{method, params?}`.
  - `RequestId` = string | integer.
- Server notifications may carry `emittedAtMs` (flattened envelope field; optional for older servers).
- Transports (`--listen`):
  - `stdio://`: default, newline-delimited JSON.
  - `ws://IP:PORT`: experimental, one message per text frame. Serves `GET /readyz` and `/healthz`, and rejects requests that carry an `Origin` header. Auth options are `--ws-auth capability-token|signed-bearer-token`.
  - `unix://[PATH]`: WebSocket over a Unix socket.
  - `off`.
- Errors seen:
  - `-32600`: invalid request, also used for policy rejections and live-worker archive/delete.
  - `-32601`: method not found, also unsupported paginated creation.
  - `-32602`: invalid params.
  - `-32001`: server overloaded (WS ingress queue full; retry with backoff).
  - Text errors: `Not initialized`, `Already initialized`, `<descriptor> requires experimentalApi capability`.
- Schema generation: `codex app-server generate-ts --out DIR` and `generate-json-schema --out DIR` (version-specific).
- Request serialization classes (server side): per-thread (`thread_id`), global (`config`, `projects`, `thread-sections`, `memory`), and concurrent (fs, list/read paging).

### F.2 Initialize handshake
1. Client → `initialize`:
   - `params.clientInfo` = `{name, title?, version}`. `name` matters for compliance logs.
   - `params.capabilities?` = `{experimentalApi: bool, optOutNotificationMethods?: string[] (exact names), requestAttestation: bool, mcpServerOpenaiFormElicitation: bool (legacy), explicitGatewayOauth: bool, extensions?: {<ext>: json}}`.
2. Response → `{userAgent, codexHome (abs path), platformFamily ("unix"|"windows"), platformOs ("macos"|"linux"|"windows")}`.
3. Client → notification `initialized` (the **only** client notification).
4. Any other request before this fails. A second `initialize` errors.

Experimental methods and fields are rejected unless `experimentalApi: true`. Gating is per method and per field (`#[experimental("method.field")]`).

### F.3 Core object shapes

All JSON fields are camelCase. Tagged unions use `type` unless noted.

**Thread**:
- Identity: `id` (UUIDv7), `sessionId` (root of fork tree), `forkedFromId?`, `parentThreadId?` (subagents).
- Content and storage: `preview` (≈ first user msg), `ephemeral`, `historyMode` (`legacy|paginated`).
- Organization: `section?` (`{id,name,appearance?{icon?,color?}}`), `sectionEnteredAt?`, `projectId` (nullable).
- Model: `modelProvider`, `model?`, `reasoningEffort?`.
- Timestamps (seconds): `createdAt`, `updatedAt`, `recencyAt?`.
- Runtime and origin: `status` (ThreadStatus), `path?` (unstable), `cwd`, `cliVersion`, `originator?`, `source` (SessionSource: `cli|vscode|exec|appServer|custom|subAgent|unknown`), `threadSource?` (a plain string converted by core; logical values are user / subagent / guardian review / feature(name) / memory consolidation, and the exact wire strings were not verified).
- Metadata: `agentNickname?`, `agentRole?`, `gitInfo?` (`{sha?,branch?,originUrl?}`), `name?` (user title).
- Turns: `turns` (populated only by resume, fork, and read with includeTurns).
- Experimental: `environments?`, `extra?`, `canAcceptDirectInput?`, `daybreakEnabled?`.

**ThreadStatus**: `{type:"notLoaded"} | {type:"idle"} | {type:"systemError"} | {type:"active", activeFlags:["waitingOnApproval"|"waitingOnUserInput"]}`.

**Turn**:
- `id` (UUIDv7), `items: ThreadItem[]`, `itemsView` (`notLoaded|summary|full`).
- `status` (`completed|interrupted|failed|inProgress`), `error?: TurnError`.
- `startedAt?`, `completedAt?` (seconds), `durationMs?`.

**TurnError**: `{message, codexErrorInfo?, additionalDetails?, misalignment?{errorType?, detailedExplanation?, steer?{message}}}`.

**CodexErrorInfo** (camelCase):
- `contextWindowExceeded`, `sessionBudgetExceeded`, `usageLimitExceeded`, `rateLimitExceeded`, `flexUnavailable`, `serverOverloaded`, `cyberPolicy`, `misalignmentPolicyViolation`, `tooManyDenials`.
- With `{httpStatusCode?}`: `httpConnectionFailed`, `responseStreamConnectionFailed`, `responseStreamDisconnected`, `responseTooManyFailedAttempts`.
- `internalServerError`, `unauthorized`, `badRequest`, `threadRollbackFailed`, `sandboxError`.
- `activeTurnNotSteerable{turnKind: review|compact}`, and `other` (untagged string).

**ThreadItem** (`type` discriminator, camelCase):

| type | key fields |
|---|---|
| `userMessage` | `id, clientId?, content: UserInput[]` |
| `hookPrompt` | `id, fragments[{text, hookRunId}]` |
| `agentMessage` | `id, text, phase?` (`commentary`/`final_answer`), `memoryCitation?{entries[{path,lineStart,lineEnd,note}],threadIds}`, `delivery?` (`async`), `questions?[{title, options?}]` |
| `functionCallOutput` | `id, name, namespace?, output` |
| `plan` | `id, text` (final item is authoritative vs deltas) |
| `reasoning` | `id, summary: string[], content: string[]` |
| `commandExecution` | `id, pluginId?, scriptPath?, command, cwd, processId?, source` (`agent|userShell|unifiedExecStartup|unifiedExecInteraction`), `status` (`inProgress|completed|failed|declined`), `commandActions[]` (`read{command,name,path}`/`listFiles{command,path?}`/`search{command,query?,path?}`/`unknown{command}`), `aggregatedOutput?, exitCode?, durationMs?` |
| `fileChange` | `id, changes[{path, kind:{type:add\|delete\|update, movePath?}, diff}], status` (`inProgress\|completed\|failed\|declined`) |
| `mcpToolCall` | `id, server, tool, status` (`inProgress\|completed\|failed`), `arguments, appContext?{connectorId,linkId?,resourceUri?,appName?,actionName?}, mcpAppResourceUri?` (legacy), `mcpAppUi?{resourceUri, preferredModelDisplayMode: inline\|fullscreen}, pluginId?, readOnlyHint?, result?{content[], structuredContent?, _meta?}, error?{message}, durationMs?` |
| `dynamicToolCall` | `id, namespace?, tool, arguments, status, contentItems?, success?, durationMs?` |
| `collabAgentToolCall` | `id, tool` (`spawnAgent\|sendInput\|resumeAgent\|wait\|closeAgent\|sendMessage\|followupTask\|interruptAgent\|listAgents`), `status` (`inProgress\|completed\|failed\|interrupted`), `senderThreadId, receiverThreadIds[], prompt?, model?, reasoningEffort?, agentsStates{<threadId>:{status, message?}}` |
| `subAgentActivity` | `id, kind` (`started\|interacted\|interrupted\|completed`), `agentThreadId, agentPath` |
| `webSearch` | `id, query, action?` (`search{query?,queries?}`/`openPage{url?}`/`findInPage{url?,pattern?}`/`other`), `results?` |
| `imageView` | `id, path` |
| `sleep` | `id, durationMs` |
| `imageGeneration` | `id, status, revisedPrompt?, result, transparentBackground?, failure?{type:usageLimitExceeded,limitId,resetsAt?}, savedPath?` |
| `enteredReviewMode` / `exitedReviewMode` | `id, review` (exited carries the final review text) |
| `contextCompaction` | `id` |

**UserInput** (`type`):
- `text{text, textElements[{byteRange{start,end}, placeholder?}]}`
- `image{url | fileId, detail?}`, `localImage{path, detail?}`
- `audio{url}`, `localAudio{path}`
- `skill{name, path}`, `mention{name, path}`

**Enums**:
- `AskForApproval` (kebab-case): `untrusted|on-request|never|{granular:{sandboxApproval,rules,skillApproval,requestPermissions,mcpElicitations}}` (granular is experimental).
- `ApprovalsReviewer`: `user|auto_review`.
- `SandboxMode`: `read-only|workspace-write|danger-full-access`.
- `SandboxPolicy` (`type`): `dangerFullAccess | readOnly{networkAccess} | externalSandbox{networkAccess: restricted|enabled} | workspaceWrite{writableRoots[], networkAccess, excludeTmpdirEnvVar, excludeSlashTmp}`.
- `ReasoningEffort`: `none|minimal|low|medium|high|xhigh|max|ultra|persistent|<custom>`.
- `ReasoningSummary`: `auto|concise|detailed|none`.
- `Personality`: `none|friendly|pragmatic` (deprecated).
- `CollaborationMode`: `{mode: plan|default, settings{model, reasoning_effort?, developer_instructions?}}`.
- Service tier: `fast|flex` (wire value "priority" for fast; `"default"` = standard).

### F.4 Client → server requests

*(X) = experimental (requires `experimentalApi`).*

**Thread lifecycle**
- `thread/start`:
  - Params: `model?, modelProvider?, serviceTier?, cwd?, approvalPolicy?, approvalsReviewer?, sandbox?` (SandboxMode), `config?` (overrides map), `serviceName?, baseInstructions?, developerInstructions?, personality?, ephemeral?, sessionStartSource?` (`startup|clear`), `threadSource?`.
  - Experimental params: `permissions` (profile id), `runtimeWorkspaceRoots`, `historyMode`, `projectId`, `daybreakEnabled`, `environments[{environmentId,cwd,runtimeWorkspaceRoots?}]`, `dynamicTools`, `selectedCapabilityRoots`, `allowProviderModelFallback`, `experimentalRawEvents`.
  - Response: `{thread, model, modelProvider, serviceTier?, disabledPluginIds[], cwd, runtimeWorkspaceRoots(X), instructionSources[], approvalPolicy, approvalsReviewer, sandbox (SandboxPolicy), activePermissionProfile?(X){id,extends?}, reasoningEffort?, multiAgentMode(X, deprecated)}`.
  - Emits `thread/started` and auto-subscribes.
- `thread/resume`:
  - Params: `threadId`, the same overrides as start, `excludeTurns?`, and (X) `history`, `path`, `initialTurnsPage{limit?,sortDirection?,itemsView?}`.
  - Response: like start plus `collaborationMode?, initialTurnsPage?(X), turnsBackwardsCursor?, itemsBackwardsCursor?`.
- `thread/fork`:
  - Params: `threadId, lastTurnId?` (inclusive), `ephemeral?, threadSource?, excludeTurns?`, overrides, and (X) `beforeTurnId`, `path`, `deferGoalContinuation`.
  - Response: like start. The new thread carries `forkedFromId`. A non-ephemeral fork copies thread attachments.
- `thread/read`: `{threadId, includeTurns?}` → `{thread}`.
- `thread/list`:
  - Params: `{cursor?, limit?, sortKey?` (snake_case on the wire: `created_at|updated_at|recency_at|section_position`), `sortDirection?` (`asc|desc`), `modelProviders?, sourceKinds?, originators?, archived?, sectionId?` (null = unsectioned), `projectId?(X), cwd? (string|string[]), useStateDbOnly?, searchTerm?, parentThreadId?(X), ancestorThreadId?(X)}`.
  - Response: `{data: Thread[], nextCursor?, backwardsCursor?}`.
- `thread/search` (X): `{searchTerm, cursor?, limit?, sortKey?, sortDirection?, sourceKinds?, archived?}` → `{data[{thread, snippet}], nextCursor?, backwardsCursor?}`.
- `thread/searchOccurrences` (X): `{threadId, searchTerm, cursor?, limit?}` → `{data[{turnId,itemId,snippet,snippetMatchRange{start,end} (UTF-16),turnCursor}], nextCursor?}`.
- `thread/loaded/list`: `{cursor?, limit?}` → `{data: threadId[], nextCursor?}`.
- `thread/turns/list`: `{threadId, cursor?, limit?, sortDirection? (default desc), itemsView?}` → `{data: Turn[], nextCursor?, backwardsCursor?}`.
- `thread/items/list`: `{threadId, turnId?, cursor?` (opaque string, or `{type:"item", itemId}` anchor that needs `turnId`), `limit?, sortDirection? (default asc)}` → `{data[{turnId, item, startedAtMs?, completedAtMs?}], nextCursor?, backwardsCursor?}`.
- `thread/timeline/list` (X): `{threadId, cursor?, limit?}` → `{data[item|realtime|turnStarted|turnCompleted entries with position], nextCursor?, activeRealtimeSessionAtPageStart?}`.
- `thread/archive`, `thread/unarchive` (→ `{thread}`), `thread/delete` (permanent, cascades to spawned descendants): all `{threadId}`.
- `thread/unsubscribe` → `{status: notLoaded|notSubscribed|unsubscribed}`. If it was the last subscriber, the server unloads the thread after a grace period and emits `thread/closed`.
- `thread/name/set` `{threadId, name}` emits `thread/name/updated`.
- `thread/metadata/update`: `{threadId, gitInfo?{sha?,branch?,originUrl?}` (each field: omit = keep, null = clear), `projectId?(X), daybreakEnabled?(X)}` → `{thread}`.
- `thread/settings/update` (X): `{threadId, disabledPluginIds?, cwd?, approvalPolicy?, approvalsReviewer?, sandboxPolicy?, permissions?, model?, serviceTier?, effort?, summary?, collaborationMode?, personality?}` emits `thread/settings/updated{threadId, threadSettings}`.
- `thread/compact/start` `{threadId}` → `{}` (progress via turn/item events).
- `thread/revert` `{threadId, beforeTurnId}` → `{thread (turns empty), turnsBackwardsCursor?, itemsBackwardsCursor?}`. Emits `thread/reverted`. **Replaces the removed `thread/rollback`.**
- `thread/inject_items` `{threadId, items: ResponseItem-json[]}` → `{}`.
- `thread/shellCommand` `{threadId, command, timeoutMs?}` → `{}`. Runs a user-initiated, **unsandboxed** shell command and streams as a `commandExecution` item with `source:"userShell"`.
- `thread/approveGuardianDeniedAction` `{threadId, event}` → `{}`.
- `thread/backgroundTerminals/{list,terminate,clean}` (X):
  - list `{threadId,cursor?,limit?}` → `{data[{itemId,processId,command,cwd,osPid?,cpuPercent?,rssKb?}], nextCursor?}`.
  - terminate `{threadId, processId}` → `{terminated}`.
- `thread/increment_elicitation` / `thread/decrement_elicitation` (X) `{threadId}` → `{count, paused}`.
- `thread/memoryMode/set` (X) `{threadId, mode: enabled|disabled}`. `memory/status` (X), `memory/reset` (X).
- `thread/prediction/request` (X) `{threadId, sourceTurnId}` emits `thread/prediction/updated`.
- `rollout/compress` (X).

**Goals**
- `thread/goal/set` `{threadId, origin?: user|automatic, objective?, status?, tokenBudget?}` → `{goal}`.
- `thread/goal/get` → `{goal?}`. `thread/goal/clear` `{threadId, origin?}` → `{cleared}`.
- `ThreadGoal`: `{threadId, objective, status: active|paused|blocked|usageLimited|budgetLimited|complete, tokenBudget?, tokensUsed, timeUsedSeconds, createdAt, updatedAt}`.

**Queue** (X), server-side follow-up queue:
- `thread/queue/add` `{threadId, input, clientUserMessageId}` → `{queuedSubmission{id,input,clientUserMessageId}}`.
- `thread/queue/list`, `thread/queue/update` `{queuedSubmissionId, input}`, `thread/queue/delete`, `thread/queue/reorder` `{queuedSubmissionIds[]}`.
- `thread/queue/start` `{queuedSubmissionId?}` → `{turn}`.

**Attachments**
- `thread/attachment/add` `{threadId, attachmentType, identityKey, payload}` → `{outcome, attachment{id,attachmentType,identityKey,payload,createdAt}}`.
- `thread/attachment/list` `{threadId, cursor?, limit?}` (max 100 per thread).
- `thread/attachmentOwner/list` `{attachmentType, identityKey, archived?}` → `{data[{threadId, archived}], nextCursor?}`.
- `thread/attachment/remove` → `{}`.

**Sections**
- `threadSection/list` `{cursor?,limit?}`, `threadSection/create` `{name, appearance?{icon?,color?}}`, `threadSection/update` `{sectionId, name, appearance?}`, `threadSection/delete`.
- `thread/section/move` `{threadId, sectionId|null, beforeThreadId?}`.

**Projects** (X)
- `project/list` `{cursor?,limit?,sortKey?: position|recencyAt, sortDirection?}`, `project/read` `{projectId}`.
- `project/create` `{name, roots[{path}], metadata?, idempotencyKey}`.
- `project/import` (adds `threads?`), `project/update` `{projectId, name?, roots?, metadata?}`, `project/move` `{projectId, beforeProjectId?}`, `project/delete`.
- `Project`: `{id, name, roots[{path}], metadata{}, position, createdAt, updatedAt, recencyAt?}`.

**Turns**
- `turn/start`:
  - Params: `{threadId, input: UserInput[], clientUserMessageId?, turnTrigger?, toolOutput?{name,namespace?,output}, disabledPluginIds?, cwd?, approvalPolicy?, approvalsReviewer?, sandboxPolicy?, model?, serviceTier?, serviceTierForTurn?, effort?, summary?, personality?, outputSchema?}`.
  - Experimental params: `permissions`, `runtimeWorkspaceRoots`, `environments`, `additionalContext{<key>:{value, kind: untrusted|application}}`, `responsesapiClientMetadata`, `collaborationMode`, `cyberAccessProgram`, `multiAgentMode` (ignored).
  - Overrides persist for subsequent turns. Response → `{turn}`.
- `turn/steer` `{threadId, expectedTurnId, input, clientUserMessageId?, additionalContext?(X)}` → `{turnId}`. Fails with `activeTurnNotSteerable` during review or compact turns.
- `turn/interrupt` `{threadId, turnId}` → `{}`. The turn ends `interrupted`.
- `turn/settings/update` (X) `{threadId, turnId, approvalsReviewer?, model?, effort?, summary?, serviceTier?}` → `{status: applied|targetUnavailable}`.

**Review**
- `review/start` `{threadId, target, delivery?: inline|detached}` (detached is deprecated) → `{turn, reviewThreadId}`.
- `target` (`type`): `uncommittedChanges | baseBranch{branch} | commit{sha, title?} | custom{instructions}`.
- Streams the `enteredReviewMode` and `exitedReviewMode` items.

**Realtime voice** (X)
- `thread/realtime/{start,appendAudio,appendText,appendSpeech,stop,listVoices}`.

**Models / config / features**
- `model/list` `{cursor?, limit?, includeHidden?}` → `{data: Model[], nextCursor?}`.
  - `Model{id, model, displayName, description, hidden, isDefault, supportedReasoningEfforts[{reasoningEffort, description}], defaultReasoningEffort, inputModalities, serviceTiers[{id,name,description}], defaultServiceTier?, upgrade?, upgradeInfo?, availabilityNux?, modelSpecialty?, multiAgentVersion?, availableAccessPrograms?, supportsPersonality(false)}`.
- `modelProvider/capabilities/read`, `collaborationMode/list` (X) → `{data[{name, mode?, model?, reasoning_effort?}]}`.
- `experimentalFeature/list`, `experimentalFeature/enablement/set`, `permissionProfile/list`.
- `config/read` `{includeLayers?, cwd?}` → `{config, origins{key: layerMeta}, layers?}`.
  - Config keys include `model, reviewModel, approvalPolicy, approvalsReviewer, sandboxMode, sandboxWorkspaceWrite, webSearch, tools, instructions, developerInstructions, modelReasoningEffort, modelReasoningSummary, modelVerbosity, serviceTier, apps, browserUse, computerUse, desktop{}` plus flattened extras.
- `config/value/write` `{keyPath, value, mergeStrategy: replace|upsert, filePath?, expectedVersion?}` → `{status: ok|okOverridden, version, filePath, overriddenMetadata?}`.
- `config/batchWrite` `{edits[{keyPath,value,mergeStrategy}], filePath?, expectedVersion?, reloadUserConfig?}`.
- `configRequirements/read`.
- `externalAgentConfig/{detect,import,import/recordHistory,import/readHistories}`.

**Skills / plugins / apps / hooks**
- `skills/list` `{cwds[], forceReload, perCwdExtraUserRoots?}` → `{data[{cwd, skills[SkillMetadata{name,description,shortDescription?,interface?,dependencies?,path,scope,enabled,pluginId?}], errors[]}]}`.
- `skills/extraRoots/set`, `skills/config/write` (enable/disable by path), `hooks/list`.
- `marketplace/{add,remove,upgrade}`.
- `plugin/{list,search(X),installed,reconcile,read,install,uninstall}`, `plugin/skill/read`, `plugin/share/{save,updateTargets,list,checkout,delete}`. Upstream marks the plugin list/read/install/uninstall methods as under development.
- `app/{list,read,installed}` (connectors).

**MCP**
- `mcpServerStatus/list` `{cursor?, limit?, detail?: full|toolsAndAuthOnly, threadId?, serverName?}` → servers `{name, runtimeStatus?, pluginId?, httpOrigin?, serverInfo?, serverCapabilities?, tools{}, resources..., auth...}`.
- `mcpServer/oauth/login` `{name, threadId?, clientRegistration?, scopes?, timeoutSecs?}` → `{authorizationUrl, loginId}`. Emits `mcpServer/oauthLogin/completed`.
- `config/mcpServer/reload`.
- `mcpServer/resource/read`.
- `mcpServer/tool/call` `{threadId, server, tool, arguments?, _meta?}`.
- `mcpServer/event/stream/{start,stop}` (X).

**Exec / process / fs / search**
- `command/exec` `{command: argv[], processId?, tty, streamStdin, streamStdoutStderr, outputBytesCap?, disableOutputCap, disableTimeout, timeoutMs?, cwd?, env?{K: string|null}, size?{rows,cols}, sandboxPolicy?, permissionProfile?}` → `{exitCode, stdout, stderr}`. Runs sandboxed.
- `command/exec/{write,resize,terminate}` (by `processId`). Output streams via `command/exec/outputDelta` (base64).
- `process/{spawn,writeStdin,kill,resizePty}` (X): unsandboxed, keyed by `processHandle`. Emits `process/outputDelta` and `process/exited`.
- `fs/readFile` `{path}` → `{dataBase64}`; `fs/writeFile` `{path, dataBase64}`; `fs/createDirectory` `{path, recursive?}`; `fs/getMetadata` → `{isDirectory,isFile,isSymlink,createdAtMs,modifiedAtMs}`; `fs/readDirectory` → `{entries[{fileName,isDirectory,isFile}]}`; `fs/remove` `{path,recursive?,force?}`; `fs/copy` `{sourcePath,destinationPath,recursive}`; `fs/watch` `{watchId,path}`; `fs/unwatch`. Emits `fs/changed{watchId, changedPaths}`. All paths absolute.
- `fuzzyFileSearch` `{query, roots[], cancellationToken?}` → `{files[{root,path,match_type,file_name,score,indices?}]}` (note: snake_case fields).
- (X) `fuzzyFileSearch/sessionStart{sessionId,roots}` / `sessionUpdate{sessionId,query}` / `sessionStop`, with notifications `fuzzyFileSearch/sessionUpdated{sessionId,query,files}` and `sessionCompleted`.

**Account / auth / misc**
- `account/read` → `{account?: {type:"apiKey"}|{type:"chatgpt",...}, requiresOpenaiAuth, workspaceRouting?(X)}`.
- `account/login/start`: `{type:"apiKey",apiKey} | {type:"chatgpt",...} | {type:"chatgptDeviceCode"} | {type:"chatgptAuthTokens",accessToken,...}`. Then `account/login/cancel` and `account/logout`.
- `account/rateLimits/read` → `{rateLimits{limitId?,limitName?,primary?{usedPercent,windowDurationMins?,resetsAt?},secondary?,credits?...}, rateLimitsByLimitId?, rateLimitResetCredits?...}`.
- `account/rateLimitResetCredit/consume`, `account/usage/read`, `account/workspaceMessages/read`, `account/sendAddCreditsNudgeEmail`.
- `account/gatewayOAuth/{read,login,cancel}`, `account/bedrock/{discover,setup,checkGovCloudRequirements}` (X).
- `feedback/upload` `{classification, reason?, threadId?, includeLogs, extraLogFiles?, tags?}`.
- `windowsSandbox/setupStart` `{mode: elevated|unelevated}` emits `windowsSandbox/setupCompleted`. `windowsSandbox/readiness`.
- `environment/{add,info,status}` (X).
- `remoteControl/{enable,disable,status/read,pairing/start,pairing/status,client/list,client/revoke}` (X).
- `userVerification/{status,enroll,delete,verify,cancel}` (X).
- `server/diagnostics` (X).
- Deprecated v1 methods: `getConversationSummary`, `gitDiffToRemote` `{cwd}` → `{sha, diff}`, `getAuthStatus`.

### F.5 Server → client requests (client must respond)

| Method | Params (key) | Response |
|---|---|---|
| `item/commandExecution/requestApproval` | `threadId, turnId, itemId, startedAtMs, kind (command\|writeStdin), approvalId?, environmentId?, reason?, networkApprovalContext?{host,protocol}, command?, cwd?, commandActions?, proposedExecpolicyAmendment?, proposedNetworkPolicyAmendments?, additionalPermissions?(X), availableDecisions?(X)` | `{decision: "accept"\|"acceptForSession"\|{acceptWithExecpolicyAmendment:{execpolicy_amendment}}\|{applyNetworkPolicyAmendment:{network_policy_amendment}}\|"decline"\|"cancel"}` |
| `item/fileChange/requestApproval` | `threadId, turnId, itemId, startedAtMs, reason?, grantRoot?` | `{decision: accept\|acceptForSession\|decline\|cancel}` |
| `item/permissions/requestApproval` | `threadId, turnId, itemId, environmentId?, startedAtMs, cwd, reason?, permissions{network?, fileSystem?}` | `{permissions (granted subset), scope: turn\|session, strictAutoReview?}` |
| `item/tool/requestUserInput` | `threadId, turnId, itemId, questions[{id, header, question, isOther, isSecret, options?[{label, description}]}], isBlocking, autoResolutionMs? (deprecated)` | `{answers: {<questionId>: {answers: string[]}}}` |
| `mcpServer/elicitation/request` | `threadId, turnId?, serverName`, plus flattened `mode`: `form{message, requestedSchema, _meta?}` / `url{message, url, elicitationId}` / `openai/form` / `openaiForm` / `openai/userVerification`(X) `{title, description, challenge}` | `{action: accept\|decline\|cancel, content?, _meta?}` |
| `item/tool/call` (dynamic tools, X) | `threadId, turnId, callId, namespace?, tool, arguments` | `{contentItems[], success}` |
| `account/chatgptAuthTokens/refresh` | — | refreshed tokens |
| `attestation/generate` | (opt-in via `requestAttestation`) | `{token}` |
| `currentTime/read` (X) | — | client clock |
| legacy `applyPatchApproval`, `execCommandApproval` | v1 only | `{decision}` |

After any of these is answered or cleared, the server emits `serverRequest/resolved{threadId, requestId}`.

### F.6 Server → client notifications

- **Thread**:
  - Lifecycle: `thread/started{thread}`, `thread/status/changed{threadId,status}`, `thread/archived`, `thread/unarchived`, `thread/deleted`, `thread/closed`, `thread/reverted` (each `{threadId}`).
  - Metadata: `thread/name/updated{threadId, threadName?}`, `thread/settings/updated`(X), `thread/tokenUsage/updated{threadId, turnId, tokenUsage{total,last:{totalTokens,inputTokens,cachedInputTokens,cacheWriteInputTokens,outputTokens,reasoningOutputTokens}, modelContextWindow?}}`.
  - Goals: `thread/goal/updated{threadId, turnId?, goal}`, `thread/goal/cleared`.
  - Other: `thread/queue/changed`(X), `thread/attachment/updated{threadId, attachmentType, identityKey, attachmentId, operation}`, `thread/prediction/updated`(X), `thread/project/updated`(X), `project/changed{projectId, changeType}`(X), `thread/environment/connected|disconnected`(X), `thread/compacted` (deprecated; use the `contextCompaction` item).
- **Turn**: `turn/started{threadId, turn}`, `turn/completed{threadId, turn}`, `turn/diff/updated{threadId, turnId, diff}` (aggregated unified diff), `turn/plan/updated{threadId, turnId, explanation?, plan[{step, status: pending|inProgress|completed}]}`, `turn/moderationMetadata`(X).
- **Items**:
  - Lifecycle: `item/started{item, threadId, turnId, startedAtMs}`, `item/completed{item, threadId, turnId, completedAtMs}` (authoritative).
  - Deltas: `item/agentMessage/delta{threadId,turnId,itemId,delta}`, `item/plan/delta`, `item/reasoning/summaryTextDelta{...,summaryIndex}`, `item/reasoning/summaryPartAdded{...,summaryIndex}`, `item/reasoning/textDelta{...,contentIndex}`, `item/commandExecution/outputDelta`, `item/commandExecution/terminalInteraction{...,processId,stdin}`, `item/fileChange/patchUpdated{...,changes}`, `item/fileChange/outputDelta` (deprecated, no longer emitted), `item/mcpToolCall/progress`.
  - Auto-review: `item/autoApprovalReview/started{threadId,turnId,startedAtMs,reviewId,targetItemId?,review{status,riskLevel?,userAuthorization?,rationale?},action}` and `.../completed` (adds `completedAtMs`, `decisionSource`), `autoApprovalReview/strictReviewRequired`(X).
  - Raw: `rawResponseItem/completed`, `rawResponse/completed` (internal).
- **Hooks**: `hook/started{threadId, turnId?, run}`, `hook/completed`.
- **Model**: `model/rerouted{threadId,turnId,fromModel,toModel,reason}`, `model/verification`, `model/safetyBuffering/updated`, `modelProvider/authRecoveryStarted|Completed`.
- **Diagnostics**: `error{error: TurnError, willRetry, threadId, turnId}`, `warning{threadId?, message}`, `guardianWarning{threadId, message}`, `deprecationNotice`, `configWarning{summary, details?, path?, range?}`.
- **MCP**: `mcpServer/startupStatus/updated{threadId?, name, status, error?, failureReason?}`, `mcpServer/oauthLogin/completed`, `mcpServer/event/stream/notification`(X).
- **Account**: `account/updated`, `account/login/completed`, `account/rateLimits/updated`, `account/gatewayOAuth/changed`, `app/list/updated`, `remoteControl/status/changed`.
- **Skills / config / fs / exec**: `skills/changed`, `externalAgentConfig/import/progress|completed`, `fs/changed`, `command/exec/outputDelta`, `process/outputDelta`(X), `process/exited`(X), `fuzzyFileSearch/sessionUpdated|sessionCompleted`(X).
- **Realtime** (X): `thread/realtime/{started,itemAdded,item/started,item/transcript/delta,item/completed,transcript/delta,transcript/done,outputAudio/delta,sdp,error,closed}`.
- **Windows**: `windows/worldWritableWarning`, `windowsSandbox/setupCompleted{mode, success, error}`.

### F.7 Typical flows
- **Turn**: `turn/start` → `turn/started` → for each item, `item/started` → deltas → `item/completed` → `turn/diff/updated` / `turn/plan/updated` / `thread/tokenUsage/updated` → `turn/completed`.
- **Command approval**: `item/started` (commandExecution, inProgress) → `item/commandExecution/requestApproval` → client decision → `serverRequest/resolved` → `item/completed` (completed|failed|declined).
- **Review**: `review/start` → `turn/started` → `item/*` with `enteredReviewMode` → ... → `exitedReviewMode` (final text) → `turn/completed`.

### F.8 Versioning & deprecations
- Two API generations live in one enum. v2 is the current set above. Deprecated v1 methods (`getConversationSummary`, `gitDiffToRemote`, `getAuthStatus`) and legacy approval requests are still present.
- No numeric protocol version is exchanged. Compatibility relies on `experimentalApi` gating, optional fields, and the per-build generated TS/JSON schema. Clients probe features (for example, gateway OAuth support is confirmed by a successful `account/gatewayOAuth/read`).
- **Removed**: `thread/rollback` (use `thread/revert{beforeTurnId}`). Historical rollback events still replay. The `codex mcp-server` command is also removed.
- **Deprecated**:
  - `personality` and `multiAgentMode` (ignored; use `effort:"ultra"` for proactive multi-agent behavior).
  - Detached review delivery, the `thread/compacted` notification, `item/fileChange/outputDelta`, and `mcpAppResourceUri`.
  - `autoResolutionMs` (use `isBlocking`) and full-history hydration on resume/read (use `excludeTurns` plus `thread/turns/list` / `thread/items/list`).
- **History**: `historyMode: paginated` creation is not supported yet (-32601).
- Upstream labels app-server as **experimental and not supported for production**. Reasoning efforts include new `max|ultra|persistent` values.

---

## G. Uncertainties, gaps, failed fetches

### Fetch results
- `learn.chatgpt.com/docs/codex/<page>` returns **404**. Real paths are `learn.chatgpt.com/docs/<slug>`, for example `/docs/app`, `/docs/reference/slash-commands`, `/docs/environments/git-worktrees`, `/docs/automations`, `/docs/computer-use`, `/docs/browser`.
- `/docs/features.md` works. `/docs/features` (HTML) was not needed.
- `developers.openai.com/codex/*` redirects to learn.chatgpt.com.
- Changelog: `/docs/changelog.md` is **404**. The HTML at `/docs/changelog` was parsed instead (some formatting lost; iOS and CLI entries filtered out by heading).
- `raw.githubusercontent.com/.../protocol/v2.rs` is **404**. v2 is now split into `protocol/v2/*.rs` (about 40 modules: thread, turn, item, review, config, mcp, plugin, etc.).
- `codex-rs/app-server/README.md` is no longer an API overview. It is a set of feature notes (attachments, rollback removal, gateway OAuth, user verification, item anchors). The full API guide is `learn.chatgpt.com/docs/app-server.md`.

### Discrepancies (docs vs source on `main`)
- **Rollback**: the docs still list `thread/rollback{threadId,numTurns}` as deprecated, but source and the README say it was **removed** in favor of `thread/revert`.
- **isPinned**: the docs mention `thread/list` filter `isPinned` and `thread/metadata/update` `isPinned`, but neither exists in v2 source on `main`, which uses `sectionId`/`projectId`. Pinning may be client-side or desktop-private, or the docs may be stale.
- **Sandbox example casing**: the docs example `"sandbox": "workspaceWrite"` conflicts with source `SandboxMode`, which is kebab-case (`workspace-write`). The camelCase form is only for `SandboxPolicy.type` (`workspaceWrite`).
- **Collab item name**: the docs' item table says `collabToolCall` with `receiverThreadId`/`newThreadId`/`agentStatus`, while source has `collabAgentToolCall` with `receiverThreadIds[]` and `agentsStates{}`.
- **Request-user-input method name**: the docs call the method `tool/requestUserInput`, but the actual wire method is `item/tool/requestUserInput`.
- **Experimental status**: the docs call `thread/turns/list` and `thread/items/list` experimental, but source has no `#[experimental]` on them.
- **Review delivery location**: one page says Settings > Git; another says Settings > General > Code review.

### Gaps
- The settings page is high-level. Exact control names and defaults for several panels (Project/terminal behavior, Workspace settings, Git, Worktrees) are inferred. Verify against a running app if pixel or option parity is required.
- No upstream reference lists command-menu entries exhaustively. The list in C.2 is assembled from changelog mentions.
- `/share` is described in one doc but is absent from the app slash-command table.
- The `Personality` slash command and settings remain in the app docs, but the protocol marks personality deprecated (`supportsPersonality:false` for all models).
- Model names (GPT-6.x Sol/Luna/Astra, GPT-5.5 retirement) reflect upstream docs at research time. Odex should source models dynamically via `model/list`.
- Not inspected:
  - `codex-rs/app-server` server implementation, so runtime behaviors like grace periods and ordering come from the docs.
  - The `account.rs` and `plugin.rs` response details beyond the key fields.
  - Realtime and voice payloads.
- Scratch copies of all fetched sources are in the session scratchpad and are not part of the repo.
