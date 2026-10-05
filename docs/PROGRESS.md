# Odex progress

Keep this file current. It is the resume point after a context reset.

## Status by milestone

| MS | Scope | Status |
|---|---|---|
| M0 | Parity audit, monorepo skeleton, protocol crate with TS codegen, config loading, CI | **done**. CI workflow added in M3 (see below) |
| M1 | vLLM client, tool parsing and fallbacks, resilience, Doctor, presets | **done** |
| M2 | Engine core loop, tools, rollouts, app-server JSON-RPC, `exec` | **done** |
| M3 | Electron shell, thread end to end, onboarding, Models & Endpoints | in progress |
| M4 | Permissions/approvals UI, sandboxes, exec policy, hooks trust, project trust | engine **done**; UI pending |
| M5 | Context engine tiers, recall, meter and view, soak test | engine + soak **done**; UI pending |
| M6 | Git/worktrees/review/PR/undo, terminal, actions/environments | engine **done**; UI pending |
| M7 | MCP client + settings, lazy tools, skills, plugins | engine **done**; UI pending |
| M8 | Subagents, plan, goal, automations, memories, notifications, tray | engine **done**; UI pending |
| M9 | Computer use, appshots, in-app browser, browser use | engine **done** (Notepad harness passes); UI pending |
| M10 | Palette, shortcuts, multi-window, packaging, docs | pending |

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

## Key tests

- `engine/core/tests/e2e.rs`: 14 end-to-end engine tests against the mock (tool loop, sandbox, approvals, overflow-400 recovery, plan mode, fork/rollback, recall, steer/queue, interrupt).
- `engine/core/tests/soak.rs`: a **4,096-token model, 200-step task**. Result: ≥10 compactions, no request over the window, verbatim requirements survive, and `recall` finds the compacted secret.
- `engine/computer-use/tests/notepad.rs`: GUI harness, `#[ignore]`. Run with `cargo test -p odex-computer-use -- --ignored --test-threads=1`.
- `engine/browser-bridge/tests/headless.rs`: drives headless Edge against a fixture page.

## Next

1. Desktop (M3): Electron main (engine supervisor, windows, tray), preload bridge, React renderer (sidebar, thread view, composer, onboarding, Models & Endpoints, Doctor).
2. CI workflow (`.github/workflows/ci.yml`).
3. Desktop UI for M4–M9 features, then Playwright e2e against `odex-mock-vllm`.
4. Packaging (electron-builder: NSIS + MSIX, dmg, AppImage/deb) and docs (`README`, `docs/config.md`, `docs/vllm-setup.md`).

## Known issues / gaps

- The real vLLM endpoint provided for testing (`http://192.168.50.220:8080/v1`) has been unreachable from this machine since the session started (connect timeout, ping fails). Everything so far is tested against the mock. Re-run `odex-engine --base-url http://192.168.50.220:8080/v1 doctor` when it is reachable.
- The restricted-token sandbox doesn't isolate the network (D-017). Some tools that spawn children inside the sandbox (e.g. node `child_process`) fail with EPERM; the engine offers a retry without the sandbox.
- MCP OAuth tokens are stored in a JSON file in `~/.odex`, not via `safeStorage` (D-022).
- Linux/macOS sandbox and computer-use paths are compile-checked only (no runtime test on this machine).
