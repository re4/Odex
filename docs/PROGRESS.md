# Odex progress

Keep this file current. It is the resume point after a context reset.

## Status by milestone

| MS | Scope | Status |
|---|---|---|
| M0 | Parity audit, monorepo skeleton, protocol crate with TS codegen, config loading, CI | **done**. CI workflow added in M3 (see below) |
| M1 | vLLM client, tool parsing and fallbacks, resilience, Doctor, presets | **done** |
| M2 | Engine core loop, tools, rollouts, app-server JSON-RPC, `exec` | **done** |
| M3 | Electron shell, thread end to end, onboarding, Models & Endpoints | **done** (onboarding, thread, composer, approvals e2e-tested) |
| M4 | Permissions/approvals UI, sandboxes, exec policy, hooks trust, project trust | **done** (approval cards, `approval_policy`, project `.odex/rules`, Permissions settings, hooks review) |
| M5 | Context engine tiers, recall, meter and view, soak test | **done** (meter ring, Context view, Context settings, `/compact` e2e) |
| M6 | Git/worktrees/review/PR/undo, terminal, actions/environments | **done** (review pane, Git/PR panel, hand-off, terminal, project actions/environments editor) |
| M7 | MCP client + settings, lazy tools, skills, plugins | **done** (MCP, Skills, Plugins, Hooks panels) |
| M8 | Subagents, plan, goal, automations, memories, notifications, tray | **done** (plan approval, goal row, Automations + Activity, Memories) |
| M9 | Computer use, appshots, in-app browser, browser use | **done** (Browser panel, browser + computer-use settings, agent browser e2e) |
| M10 | Palette, shortcuts, multi-window, packaging, docs | **done** (palette, rebindable shortcuts, find, undo, pop-out windows, deep links, packaging, docs). Stretch goals not started |

## Engine crates

All built and tested on Windows 11. `cargo test --workspace`, `cargo clippy --workspace --all-targets -D warnings` and `cargo fmt --check` all pass.

- `protocol`: wire types, method registry, TS codegen (`odex-engine generate-ts`).
- `config`: `~/.odex`, layering, profiles, comment-preserving edits, presets (`presets/models.toml`, verified against vLLM 0.30).
- `llm`: streaming client, retries, overflow parsing, fallback tool parsers, JSON repair and validation, registry, Doctor, loop guard.
- `mock-vllm`: axum mock with SSE fixtures, faults, `max_model_len` enforcement, JSON rule policies and `/__mock/*` control endpoints.
- `context`: budget, Tier 1 pruning, Tier 2 compaction (map-reduce, verbatim requirements, extractive fallback), Tier 3 emergency trim, template normalization.
- `tools`: tool schemas (codex/extended/minimal/compact), output capping and refs, edit/write ops.
- `apply-patch`, `execpolicy`, `sandbox`, `hooks`, `file-search`, `git`, `mcp-client`, `computer-use`, `browser-bridge`, `automations`, `memories`: see each crate's docs.
- `core`: the engine (threads, turns, approvals, tools, extensions, subagents, background jobs, store, rollouts).
- `app-server`: JSON-RPC stdio server. `exec`: headless runs. `cli`: the `odex-engine` binary.

## Testing

Every suite below runs on Windows 11. CI (`.github/workflows/ci.yml`) runs the default suites on Windows, Ubuntu and macOS. The opt-in suites (Notepad, real vLLM) are local only.

| Suite | Where | Run | In CI |
|---|---|---|---|
| Engine format and lint | `engine/` | `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` | yes |
| Engine unit and integration tests | each crate's `src/` and `tests/` | `cargo test --workspace` (in `engine/`) | yes |
| Engine end to end | `engine/core/tests/e2e.rs` | `cargo test -p odex-core --test e2e` | yes |
| Context soak | `engine/core/tests/soak.rs` | `cargo test -p odex-core --test soak -- --nocapture` (~5 s in debug) | yes, own step |
| Parallel endurance | `engine/core/tests/soak_parallel.rs` | `cargo test -p odex-core --test soak_parallel -- --nocapture` (~30 s) | yes |
| Headless browser use | `engine/browser-bridge/tests/headless.rs` | part of `cargo test --workspace` | yes |
| Desktop typecheck and lint | `desktop/` | `npm run typecheck` and `npm run lint` | yes |
| Desktop e2e (Playwright + Electron) | `desktop/e2e/*.spec.ts` | `npm run build && npx playwright test` | yes (`xvfb-run` on Linux) |
| Notepad computer-use harness | `engine/computer-use/tests/notepad.rs` | `cargo test -p odex-computer-use --test notepad -- --ignored --test-threads=1` | no |
| Real-vLLM smoke | `engine/core/tests/real_vllm.rs` | `ODEX_E2E_BASE_URL=http://host:8000/v1 cargo test -p odex-core --test real_vllm -- --nocapture --test-threads=1` | no (skips) |

**Engine tests against the mock.** `odex-mock-vllm` (`engine/mock-vllm`) replays scripted replies: content, reasoning, native and fallback tool calls, malformed arguments, disconnects, 429/503 and overflow 400s. It enforces `max_model_len`. Tests use the in-process `MockServer`. The Playwright suite uses the binary with JSON rule files; the rule format is documented at the top of `engine/mock-vllm/src/rules.rs`.
- `e2e.rs` covers the tool loop, sandboxed shell, approvals, overflow-400 recovery, plan mode, fork and rollback, recall, steer and queue, and interrupt.
- `soak.rs` runs a **4,096-token model through a 200-step task**. It asserts at least 10 compactions, no request over the window, verbatim requirements surviving, and `recall` finding the compacted secret.
- `soak_parallel.rs` runs three worktree threads on a 32k model in parallel. Each must finish without context errors and stay isolated from the others.

**Desktop e2e.** Build the engine and mock first: `cargo build -p odex-engine -p odex-mock-vllm` in `engine/`. The harness (`desktop/e2e/harness.ts`) starts the mock, launches the built app with an isolated `ODEX_HOME`, and drives it. Set `ODEX_OUT=out-<name>` to build and test from a separate output directory (for parallel runs). Visual baselines live in `desktop/e2e/__screenshots__/<platform>/`. Refresh them with `npx playwright test --update-snapshots`. In CI, a platform without baselines records them instead of failing, and uploads them as the `visual-baselines-<os>` artifact so they can be committed. A failed run uploads `playwright-report/` and `test-results/` (traces).

**Notepad computer-use harness.** It needs an interactive Windows desktop, briefly takes focus, and opens and closes its own Notepad and password-box windows without saving. It's gated by `#[ignore]`, so `cargo test --workspace` and CI skip it. Run it from `engine/`: `cargo test -p odex-computer-use --test notepad -- --ignored --test-threads=1`. It covers background screenshots, `ui_tree` search, typing through UI Automation, password-field refusal and the clipboard.

**Real-vLLM smoke.** It is opt-in: every test prints "skipped" and passes unless `ODEX_E2E_BASE_URL` is set. Optional variables:
- `ODEX_E2E_MODEL` (default: the first served model)
- `ODEX_E2E_API_KEY`
- `ODEX_E2E_TIMEOUT_SECS` (per turn, default 600)
- `ODEX_E2E_MCP_SERVER`

The tests:
1. `discovery_and_doctor`: `/v1/models` reports the model with `max_model_len`, and a quick Doctor run has no `fail` for connect, streaming or toolCall.
2. `agent_writes_file_and_runs_command`: a real agent turn in a temp dir creates a file with a unique token and prints it with a shell command.
3. `agent_fixes_failing_node_test`: fixes `sum.js` so `node test.js` passes, without touching the test. Skipped without `node`.
4. `mcp_round_trip`: calls `calc.add` on `odex-mcp-test-server`. Build it first with `cargo build -p odex-mcp-client --bins`; the test is skipped otherwise.
5. `small_window_compacts_without_overflow`: overrides the window to 4,096 tokens and sends log chunks until at least one compaction happens, then two more turns. It checks there are no overflow errors and the context stays within the window.

The suite has been verified against `odex-mock-vllm` with a rules file. It has **not yet run against a real server**: `http://192.168.50.220:8080/v1` was still unreachable (curl timed out after 5 s; the suite fails fast with "not reachable").

**exec smoke.** `odex-engine --home <empty dir> --base-url http://host:8000/v1 exec "Create hello.txt containing 'hello from odex', then print it with a shell command." --auto-approve` (see `docs/vllm-setup.md`).

**Packaging.**
- `npm run dist:win` (in `desktop/`, after `cargo build --release -p odex-engine`) produces `desktop/release/Odex-Setup-<version>-x64.exe` (NSIS) and `Odex-<version>-x64.msix`.
- To check a package, launch `release/win-unpacked/Odex.exe` with a temporary `ODEX_HOME`: the engine should start from `resources/bin` and a terminal should open (node-pty unpacked from the asar).

## Desktop e2e coverage

`desktop/e2e/` (70 tests, all passing on Windows): onboarding, thread end to end, approvals (exec escalation), plan mode, `/compact`, terminal, `!cmd`, edit and resend, find, project actions, undo, deep links, review pane (stage/revert/comment, hunks, big diffs, hand-off), files and editor, browser panel and agent browser use, MCP/skills/plugins/hooks, all settings panels with persistence, automations and Activity, and visual snapshots of home and thread in light and dark.

## Next

1. Run the real-vLLM smoke suite and Doctor once a server is reachable, then mark the `vllm serve` commands in `docs/vllm-setup.md` as tested.
2. Push to GitHub so CI runs for the first time (Linux/macOS sandbox, Electron and visual baselines have only run here on Windows), then commit the Linux and macOS baselines from the CI artifacts.
3. Replace the packaging placeholders (MSIX identity, deb maintainer) and add signing secrets.
4. Stretch goals (PROMPT §1): SSH remote projects, `/ide-context`, thread sections, permanent worktree projects.

## Known issues / gaps

- The real vLLM endpoint provided for testing (`http://192.168.50.220:8080/v1`) has been unreachable from this machine since the session started (connect timeout, ping fails). The machine is on the same LAN (192.168.50.40), but a full-tunnel VPN interface ("desktop", routes 0.0.0.0/1 + 128.0.0.0/1) captures the traffic, and ARP for the host is incomplete. Everything so far is tested against the mock. Re-run `odex-engine --base-url http://192.168.50.220:8080/v1 doctor` when it is reachable.
- The restricted-token sandbox doesn't isolate the network (D-017). Some tools that spawn children inside the sandbox (e.g. node `child_process`) fail with EPERM; the engine offers a retry without the sandbox.
- MCP OAuth tokens are stored in a JSON file in `~/.odex`, not via `safeStorage` (D-022).
- Linux/macOS sandbox and computer-use paths are compile-checked only (no runtime test on this machine). CI is the first place they run: the Linux job installs bubblewrap and lifts Ubuntu's unprivileged-userns restriction for bwrap, headless Chrome and Electron.
- Fixed: the extractive fallback summary could grow the context (it copied every user message, up to 4,000 chars each). It is now bounded (D-036) and covered by a unit test.
- PR flows (create/view/comment) aren't e2e-tested: they need network and the `gh` CLI. OAuth sign-in for MCP HTTP servers isn't e2e-tested either (no OAuth test server).
- `[notifications]` in config.toml is unused (the desktop keeps those settings in `desktop.json`), and so are `wire_api = "responses"`, `tool_call_format` and `tokenizer_path`. All are documented as reserved in `docs/config.md`.
- MSIX on this machine (Windows 11 build 26300): electron-builder's downloaded `makeappx.exe` (both the default winCodeSign 2.6.0 bundle and the 26100 kits bundle) fails to start ("side-by-side configuration is incorrect", reported as `spawn UNKNOWN`). Building with `ELECTRON_BUILDER_WINDOWS_KITS_PATH="C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64"` works, and CI sets this automatically when an SDK is installed. NSIS isn't affected.
