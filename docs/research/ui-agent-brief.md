# Brief for desktop UI work (shared by parallel agents)

Repo: `C:\Users\Mirin\Desktop\Odex`. Odex is a self-hosted coding-agent desktop app (feature clone of a
well-known coding desktop app, limited to coding, MCP and PC control) running on vLLM. Read `PROMPT.md`
(spec) and the relevant rows of `docs/PARITY.md` for the features in your area. Never use OpenAI / Codex /
ChatGPT names or assets in code, UI text or docs; paths are `~/.odex/` and `.odex/`.

## Stack
- `engine/` (Rust): `odex-engine app-server` speaks JSON-RPC over stdio. Every method and type is listed in
  `desktop/shared-types/src/generated/methods.ts` (ClientRequestMap = renderer→engine calls,
  ServerNotificationMap, ServerRequestMap) with types in the same folder. Handlers: `engine/core/src/api.rs`.
- `desktop/` (Electron 44 + React 19 + TS 5.9 + Vite 7 via electron-vite 5, zustand 5, lucide-react icons).
  - `main/` Electron main process (`index.ts` IPC handlers, `browser.ts`, `terminals.ts`, ...).
  - `preload/index.ts` exposes `window.odex` (request, on*, settings, secrets, terminals, browser, dialog,
    shell, fs, win, app).
  - `renderer/src/`: `App.tsx`, `store/app.ts` (zustand `useApp`: threads, projects, models, ui, ...),
    `lib/rpc.ts` (`call('method', params)` typed; `toast(text, kind)`), `lib/actions.ts` (createThread,
    sendMessage, promptText, confirmDialog, openSettings, openSidePanel, copy, normalizeBaseUrl, ...),
    `components/ui.tsx` (Modal, Menu/MenuItem, useMenu, Toggle, ResizeHandle, Identicon, relativeTime,
    formatTokens, basename), `components/Markdown.tsx`, `views/items.tsx` (MiniDiff, openFileInPanel),
    `views/settings/GeneralSettings.tsx` (exports `Row`, `useSetting` helpers), `views/settings/ModelsSettings.tsx`
    (a complete example panel).
  - CSS: tokens in `styles/tokens.css` (`--bg`, `--bg-elev`, `--bg-sunken`, `--bg-hover`, `--bg-active`,
    `--bg-selected`, `--fg`, `--fg-muted`, `--fg-subtle`, `--border`, `--border-strong`, `--accent`,
    `--success`, `--warning`, `--danger`, `--code-bg`, `--radius*`, `--font-code`, `--font-size*`,
    `--shadow*`). Utility classes in `styles/base.css`: `btn btn-primary btn-ghost btn-sm btn-danger`,
    `icon-btn sm`, `input`, `select`, `textarea`, `chip`, `badge (accent|success|warning|danger)`,
    `dot (accent|success|warning|danger)`, `card`, `tabs`/`tab` (aria-selected), `row`, `col`, `grow`,
    `spacer`, `ellipsis`, `muted`, `subtle`, `small`, `xs`, `mono`, `selectable`, `empty`, `spinner`,
    `panel-header`, `section-title`, `field`, `checkbox`, `menu-item`, `nav-item`. Both light and dark
    themes must look right (`[data-theme='dark']`). Put new CSS in a NEW file next to your component
    (e.g. `styles/review.css` imported from your component) instead of editing shared CSS files.
- Engine changes: only if a method you need is truly missing. Add it in `engine/protocol/src/` (types +
  `registry.rs`), implement in `engine/core/src/api.rs`, wire in `engine/app-server/src/lib.rs`, then
  regenerate TS with `cd engine && cargo run -q -p odex-protocol --bin odex-codegen -- ../desktop/shared-types/src/generated`.
  Other agents work in parallel: make small, additive edits with the Edit tool (never rewrite shared
  files wholesale), and keep `cargo clippy --workspace -- -D warnings` and `cargo fmt` clean.

## Shared files (edit surgically with Edit, re-read first; others are editing too)
`main/index.ts`, `preload/index.ts`, `renderer/src/store/app.ts`, `renderer/src/lib/actions.ts`,
`renderer/src/App.tsx`, `styles/*.css`, engine `api.rs` / `registry.rs` / `app-server/src/lib.rs`.

## Verify
- `cd desktop && npx tsc --noEmit -p tsconfig.json` and `npx eslint <your files>`. Ignore errors in files
  another agent owns that are mid-edit; fix all errors in yours.
- Build into YOUR OWN output dir and run Playwright Electron e2e against the mock vLLM:
  `cd desktop && ODEX_OUT=out-<yourname> npm run build && ODEX_OUT=out-<yourname> npx playwright test e2e/<your>.spec.ts`
  (bash syntax; in PowerShell set `$env:ODEX_OUT`). Engine binaries must exist:
  `cd engine && cargo build -p odex-engine -p odex-mock-vllm` (already built; rebuild if you change the engine).
- `desktop/e2e/harness.ts` gives `startMock(rules)`, `launch({ mockUrl, theme })`, `engineReady(page)`,
  `addProject(page, folder)`. See `desktop/e2e/thread.spec.ts` for a working example (mock rules format is
  documented at the top of `engine/mock-vllm/src/rules.rs`; tool names: shell, exec_command, read_file,
  write_file, edit_file, apply_patch, grep, glob, list_dir, ...).
- Take screenshots of your UI in light and dark (`page.screenshot`) into your scratch dir and LOOK at them
  (Read tool) to check layout. Fix what looks broken.
- Do NOT commit; the lead commits. Do not touch files owned by other agents. Report: files changed, engine
  changes, test results, known gaps.

## Round 2 additions (parity gap fixing)
- The parity audit is in your prompt; the row ids (A3.8, A5.6, …) refer to `docs/PARITY.md` sections.
- Build the engine into YOUR OWN target dir and point the app at it, so parallel agents don't lock each
  other's binaries: `cd engine && CARGO_TARGET_DIR=target-<yourname> cargo build -p odex-engine -p odex-mock-vllm`,
  then run Playwright with `ODEX_ENGINE_BIN=C:/Users/Mirin/Desktop/Odex/engine/target-<yourname>/debug/odex-engine.exe`
  (the mock binary is found in `engine/target/debug` — it is already built there).
- Protocol changes: after editing `engine/protocol/src`, regenerate TS with
  `cd engine && CARGO_TARGET_DIR=target-<yourname> cargo run -q -p odex-protocol --bin odex-codegen -- ../desktop/shared-types/src/generated`.
  Codegen rewrites the whole folder from the current source, which includes other agents' protocol edits — that's fine.
  Keep protocol changes additive (new optional fields / new methods); never rename or remove existing ones.
- Engine shared files (`api.rs`, `registry.rs`, `app-server/src/lib.rs`, `engine.rs`, `turn.rs`, `toolexec.rs`,
  `config_types.rs`): small Edit insertions only, re-read right before editing.
- Run the FULL desktop e2e suite at the end (`ODEX_OUT=out-<you> npx playwright test`) to be sure you broke nothing,
  plus `cargo test --workspace` / clippy / fmt (with your target dir) if you touched the engine.
- When a row can't reasonably be done, say so in your report with the reason (the lead will mark it deferred).
