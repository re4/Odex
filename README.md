# Odex

**A desktop coding agent for self-hosted models served by vLLM.**

Odex is a desktop app for agentic coding, MCP tools and PC control. It runs entirely on models you host, such as Qwen3-Coder, GLM, gpt-oss, DeepSeek, Devstral or Kimi-K2, behind [vLLM](https://docs.vllm.ai) or any server that speaks the Chat Completions API. Its context engine keeps long threads working after the model's context window fills up. Nothing leaves your machines: no accounts, no cloud service and no telemetry.

Odex has two parts. A headless Rust engine (`odex-engine`) runs the agent: threads, tools, sandboxing, context management and storage. An Electron + React desktop app talks to it over JSON-RPC on stdio. The same engine also runs headless with `odex-engine exec` for scripts and CI.

> **Status:** pre-release. Windows 11 comes first, then macOS and Linux. See [`docs/PROGRESS.md`](docs/PROGRESS.md) for what's done and what's next.

## Features

**Coding agent**
- Projects with one or more folders, plus chats without a project. Many threads run in parallel.
- **Local** and **Worktree** run modes. Worktree mode gives each thread its own git worktree, with setup scripts and hand-off back to your branch.
- Composer with model, reasoning-effort and permission pickers, `@` mentions, a `/` command menu, attachments, and queueing or steering while a turn runs.
- Slash commands: `/plan`, `/goal`, `/compact`, `/review`, `/init`, `/status`, `/mcp`, `/model`, `/reasoning`, `/memories`, `/approve`, `/skills`, `/fork`, `/side`, and more.
- Subagents with live activity, identicons and diff stats.
- A review pane with stage, unstage and revert per file or hunk, plus inline comments sent back to the agent. Commit with an AI-written message, push, open PRs, and use the PR panel and PR Chat.
- Per-turn undo snapshots, an integrated terminal, project run actions, a command palette, file search and `odex://` deep links.

**Built for self-hosted models**
- Several endpoints at once. Model discovery through `/v1/models`, with the context window read from `max_model_len`.
- Model roles: `main`, `compactor`, `reviewer`, `vision`, `utility`, each falling back to `main`. Discovered models you don't want can be removed from the list (and restored).
- Image and 3D generation through ComfyUI: pick API-format workflows for the Image generation and 3D generation roles, and the agent gets `generate_image` / `generate_3d`, saving results into the workspace. See [`[comfyui]`](docs/config.md#comfyui).
- [Presets](presets/models.toml) for popular coding models, each with the exact `vllm serve` flags it needs.
- **Doctor:** checks an endpoint for streaming, native and parallel tool calls, reasoning parsing, vision, `/tokenize`, prefix caching and structured output, then suggests the missing server flags.
- A client-side fallback parser for tool calls that arrive as text (Hermes, Qwen3-Coder XML, GLM, Mistral, DeepSeek, Kimi, Llama JSON, pythonic), JSON argument repair, retries with backoff, and transparent recovery from context-overflow errors.

**Smart context engine**
- Tiered context management: output caps, pruning without an LLM call, structured LLM compaction with the user's requirements quoted verbatim, and an emergency path. A thread never stops because its context is full.
- `recall` and `read_output` let the agent get back details from before a compaction.
- A context meter and a context view with a token breakdown and the compaction history.

**Safety**
- Permission modes: read-only, auto (workspace-write sandbox) and full access.
- Approval cards, including "don't ask again" and custom approve.
- Sandboxes: a restricted token or AppContainer with a Job Object on Windows, bubblewrap on Linux, Seatbelt on macOS.
- Exec-policy rules, hooks with a trust review, project trust, and optional automatic review by a reviewer model.

**MCP, skills and automation**
- MCP client for stdio and HTTP servers with OAuth. Tools load lazily when their schemas would crowd the context.
- Skills and local plugins.
- Scheduled and thread automations with a review queue.
- Opt-in local memories.
- Notifications and a tray icon.
- In-app updates from GitHub Releases: new versions download in the background and Odex asks before restarting to install them (Settings → About; can be turned off).

**PC control**
- Computer use, Windows first: background screenshots, UI Automation trees and actions, per-app allowlists, a kill switch, and before/after screenshots logged in the thread.
- Appshots: capture a window's screenshot and UI tree into the composer.
- In-app browser with agent browser use over CDP, plus page comments you can send to the agent.

## Screenshots

_Screenshots of the main window, review pane, Doctor and the context view will go here._

<!--
![Thread view](docs/images/thread-light.png)
![Review pane](docs/images/review-dark.png)
![Doctor](docs/images/doctor.png)
-->

## Quick start

### 1. Serve a model with vLLM

vLLM runs on Linux with a GPU. On Windows, use WSL2 or Docker Desktop with GPU support, or a Linux box on your network.

```bash
pip install -U vllm
vllm serve Qwen/Qwen3-Coder-30B-A3B-Instruct \
  --enable-auto-tool-choice --tool-call-parser qwen3_coder \
  --max-model-len 131072 --enable-prefix-caching
```

[`docs/vllm-setup.md`](docs/vllm-setup.md) has the command for every preset (GLM, gpt-oss, DeepSeek, Devstral, Kimi-K2, Llama, ...), multi-GPU setups, API keys and remote endpoints, and troubleshooting.

### 2. Build the engine

You need a current stable Rust toolchain ([rustup](https://rustup.rs)). On Windows, that means the MSVC toolchain with the Visual Studio C++ build tools.

```bash
cd engine
cargo build --release -p odex-engine
```

### 3. Run the desktop app

You need Node.js 20.19 or later (24 LTS recommended).

```bash
cd desktop
npm ci
npm run dev
```

In development, the app uses the newest `odex-engine` build in `engine/target/{debug,release}`. Set `ODEX_ENGINE_PATH` to use a different binary. On first run, onboarding detects `localhost:8000`, lets you pick models for each role, runs Doctor, and asks for a default permission mode and a first project.

### 4. Check the endpoint (optional)

```bash
engine/target/release/odex-engine --base-url http://localhost:8000/v1 doctor
```

## CLI

`odex-engine` is the engine the desktop app runs. You can also use it directly.

```text
odex-engine [--home <dir>] [--profile <name>] [--base-url <url>] <command>

  app-server   Serve the JSON-RPC protocol on stdio (what the desktop app runs)
  exec         Run one task headlessly
  doctor       Check vLLM endpoints and models
  generate-ts  Write the TypeScript protocol bindings
```

- `--home` overrides `~/.odex` (also `ODEX_HOME`).
- `--profile` selects a `[profiles.<name>]` from the config.
- `--base-url` sets the endpoint to use when no `[model_providers]` are configured (also `ODEX_BASE_URL`).
- Logs go to stderr. Set the level with `ODEX_LOG`, for example `ODEX_LOG=debug`.

**`exec`** runs one task to completion and prints the final message. The exit code is 0 when the turn completed.

```bash
odex-engine exec "Fix the failing test in tests/test_parser.py" -C ~/code/app --auto-approve
odex-engine exec - < task.md                       # read the task from stdin
odex-engine exec "Summarize the architecture" --permission-mode read-only --plan
odex-engine exec "..." -m local:Qwen/Qwen3-Coder-30B-A3B-Instruct --effort none --json
```

| Flag | Meaning |
|---|---|
| `-C, --cwd <dir>` | Working directory (default: current). |
| `-m, --model <key>` | A `[models]` key, `provider:model`, or a served model id. |
| `--permission-mode` | `read-only`, `auto` (default) or `full-access`. |
| `--effort` | `none`, `minimal`, `low`, `medium`, `high` or `xhigh`. |
| `--auto-approve` | Approve every approval request. Without it, they are denied. |
| `--plan` | Planning mode: read-only, ends with a plan. |
| `--json` | Print every event as a JSON line on stdout. |
| `--resume <thread-id>` | Continue an earlier thread. |
| `--output-last-message <file>` | Also write the final message to a file. |
| `--timeout <secs>` | Abort after this long (default 3600). |

**`doctor`** checks every configured endpoint, or one model with `--model <key>`. Add `--quick` to skip the prefix-cache timing and `--json` for machine-readable output. [`docs/vllm-setup.md#doctor`](docs/vllm-setup.md#doctor) explains each check.

**`app-server`** speaks JSON-RPC 2.0, newline-delimited, on stdin and stdout. The protocol types live in `engine/protocol` and are generated into `desktop/shared-types/src/generated` (`npm run gen:types`). Pass `--no-scheduler` to disable automations.

## Configuration

Settings live in `~/.odex/config.toml`, with optional profiles and a per-project `.odex/config.toml` for trusted folders. Most settings are also editable in the app. [`docs/config.md`](docs/config.md) documents every key. A minimal config:

```toml
model = "coder"

[model_providers.gpu]
base_url = "http://10.0.0.5:8000/v1"
api_key_env = "VLLM_API_KEY"

[models.coder]
provider = "gpu"
model = "Qwen/Qwen3-Coder-30B-A3B-Instruct"
```

## Repository layout

```text
engine/                 Rust workspace; builds the odex-engine binary
  protocol/             Engine <-> UI protocol types (source of truth; TS is generated from here)
  config/               ~/.odex, config layering, profiles, comment-preserving edits, presets
  llm/                  vLLM client: streaming, tool-call fallbacks, retries, discovery, Doctor
  context/              Smart context engine: budget, pruning, compaction, emergency trim, recall
  tools/                Agent tool schemas and host-independent tool logic
  apply-patch/          The apply_patch edit format
  execpolicy/           Shell command classification and approval rules
  sandbox/              Windows restricted token / AppContainer, Linux bubblewrap, macOS Seatbelt
  hooks/                Command hooks with trust review
  git/  file-search/    Git (via the git CLI), worktrees, review; fast file search
  mcp-client/           MCP stdio + HTTP client, OAuth, schema sanitizing
  computer-use/         Screenshots, UI Automation, input (Windows first)
  browser-bridge/       Browser use over CDP
  comfyui/              ComfyUI client for image and 3D generation workflows
  automations/  memories/
  core/                 The engine: threads, turns, approvals, tools, subagents, storage, rollouts
  app-server/  exec/  cli/
  mock-vllm/            Mock vLLM server for tests (SSE fixtures, faults, max_model_len enforcement); also a mock ComfyUI
desktop/                Electron + React + TypeScript (electron-vite)
  main/  preload/       Main process (engine supervisor, windows, tray, terminals, browser) and bridge
  renderer/             React UI
  shared-types/         Generated protocol types plus desktop-only types
  e2e/                  Playwright Electron tests against the mock vLLM
  scripts/              prepare-engine.mjs (stages the engine binary for packaging)
  electron-builder.yml  Installer configuration
presets/models.toml     Model presets with their vllm serve flags (compiled into the engine)
docs/                   PARITY, DECISIONS, PROGRESS, config, vllm-setup, research notes
.github/workflows/      CI
```

## Testing

```bash
# Engine: format, lint, unit and end-to-end tests against the mock vLLM (includes the context soak test)
cd engine
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Desktop: typecheck, lint, then Playwright e2e (needs the engine and mock binaries)
cargo build -p odex-engine -p odex-mock-vllm      # in engine/
cd desktop && npm run typecheck && npm run lint && npm run build && npx playwright test
```

Opt-in suites cover the Windows computer-use harness (Notepad) and a real vLLM server. [`docs/PROGRESS.md#testing`](docs/PROGRESS.md#testing) lists every suite and how to run it.

## Packaging

```bash
cd engine && cargo build --release -p odex-engine
cd ../desktop
npm run dist:win      # NSIS installer + MSIX  -> desktop/release/
npm run dist:mac      # dmg
npm run dist:linux    # AppImage + deb
```

Each `dist*` script stages the release engine into `desktop/build/bin` (`scripts/prepare-engine.mjs`), builds the app and runs electron-builder with [`desktop/electron-builder.yml`](desktop/electron-builder.yml). Builds are unsigned unless you provide signing credentials through the standard electron-builder environment variables. The MSIX identity fields in that file are placeholders until the app has a Partner Center listing. Tagged commits (`v*`) build installers in CI and attach them to a draft GitHub release.

### Releases and updates

Installed copies update themselves from the [GitHub releases page](https://github.com/re4/Odex/releases) with [electron-updater](https://www.electron.build/auto-update) ([`desktop/main/updater.ts`](desktop/main/updater.ts)). The app checks the latest release at startup and every 4 hours, downloads a newer version in the background, verifies its SHA-512, and then asks to restart and install it. Settings → About shows the status, has **Check for updates**, and can turn automatic updates off (then a check only offers the download).

For that to work, a release must carry the update metadata next to the installers:

| Platform | Upload |
|---|---|
| Windows | `Odex-Setup-<version>-x64.exe`, its `.blockmap` and `latest.yml` |
| macOS | the `.zip` (and `.dmg`), their `.blockmap` files and `latest-mac.yml` (updates need a signed build) |
| Linux | the `.AppImage` / `.deb` and `latest-linux.yml` |

The CI release job uploads all of these. When publishing by hand, upload the files from `desktop/release/`, publish the release (drafts and pre-releases are ignored), and tag it `v<version>` to match `package.json`. The MSIX package is updated by Windows, not by Odex. `ODEX_UPDATE_URL` points the updater at another feed (any folder served over HTTP with `latest.yml` and the installers), for mirrors and tests. Updater logs go to `~/.odex/logs/updater.log`.

## Documentation

- [`docs/vllm-setup.md`](docs/vllm-setup.md): serving models, flags, Doctor, troubleshooting
- [`docs/config.md`](docs/config.md): every configuration key
- [`docs/PARITY.md`](docs/PARITY.md): feature audit (keep / adapt / cut / stretch)
- [`docs/DECISIONS.md`](docs/DECISIONS.md): design decisions and their reasons
- [`docs/PROGRESS.md`](docs/PROGRESS.md): status, test suites, known issues

## License

Apache License 2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
