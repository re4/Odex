# Odex configuration reference

Odex reads its settings from `~/.odex/config.toml`. Trusted projects can add `.odex/config.toml` on top. Every key is optional: with an empty file, Odex talks to a vLLM server at `http://localhost:8000/v1` and picks the first model it serves.

The schema below comes from `engine/protocol/src/config_types.rs` (`ConfigToml` and the structs it nests). Defaults come from `engine/config/src/resolved.rs`, and per-model defaults come from the matching preset in `presets/models.toml`. The Settings window edits the same file and keeps your comments and formatting. Unknown top-level keys are ignored, with a warning in the engine log.

Contents:
[Files and directories](#files-and-directories) ·
[Layering, profiles and trust](#layering-profiles-and-trust) ·
[Top-level keys](#top-level-keys) ·
[`[model_providers]`](#model_providersid) ·
[`[models]`](#modelskey) ·
[`[roles]`](#roles) ·
[`[comfyui]`](#comfyui) ·
[`[context]`](#context) ·
[`[sandbox]`](#sandbox) ·
[`[mcp_servers]` / `[mcp]`](#mcp_serversname) ·
[`[hooks]`](#hooks) ·
[`[computer_use]`](#computer_use) ·
[`[browser]`](#browser) ·
[`[memories]`](#memories) ·
[`[notifications]`](#notifications) ·
[`[automatic_review]`](#automatic_review) ·
[`[git]`](#git) ·
[`[worktrees]`](#worktrees) ·
[`[skills]`](#skills) ·
[`[features]`](#features) ·
[`[profiles]`](#profilesname) ·
[`[projects]`](#projectspath) ·
[Secrets](#secrets) ·
[Other files](#other-project-files)

---

## Files and directories

Set `ODEX_HOME` (or pass `odex-engine --home <dir>`) to use a directory other than `~/.odex`. On Windows, `~` is `%USERPROFILE%`.

| Path | What it is |
|---|---|
| `~/.odex/config.toml` | This file. Created with commented examples on first run. |
| `~/.odex/presets.toml` | Optional user model presets. Same format as `presets/models.toml`. An entry with an existing id replaces the built-in one, and new entries are matched before the built-ins. |
| `~/.odex/AGENTS.md` | Global agent instructions, injected before the project's AGENTS.md files. |
| `~/.odex/sessions/` | Append-only JSONL rollouts, one per thread. Threads are replayed from these after a restart or crash. |
| `~/.odex/odex.sqlite` | Index of threads, full-text search and `recall`, automations, memories metadata and token usage. |
| `~/.odex/outputs/` | Full tool outputs, addressed by `ref:` ids (`read_output`). |
| `~/.odex/media/` | Screenshots, appshots and attachments. |
| `~/.odex/worktrees/<project>/<thread>` | Worktrees for threads in Worktree mode (see `worktrees_dir`). |
| `~/.odex/skills/<name>/SKILL.md` | User skills. |
| `~/.odex/plugins/` | Installed local plugins (`<id>/odex-plugin.toml`) and their registry `plugins.json`. |
| `~/.odex/rules/*.toml` | Exec-policy rules: command prefixes marked `allow`, `prompt` or `forbid`. |
| `~/.odex/memories/` | Approved and proposed memories: `global.json` and `MEMORIES.md`, plus one file per project under `projects/`. |
| `~/.odex/models_cache.json` | Discovered models, Doctor results and calibrated chars-per-token ratios. |
| `~/.odex/comfyui/*.json` | ComfyUI workflows for image and 3D generation (see [`[comfyui]`](#comfyui)). |
| `~/.odex/trusted_hooks.json` | Hashes of hooks you approved in the trust review. |
| `~/.odex/mcp_tokens.json` | OAuth tokens for MCP HTTP servers (written by the engine). |
| `~/.odex/secrets.json` | Endpoint API keys and the GitHub token, encrypted by the desktop app (see [Secrets](#secrets)). |
| `~/.odex/desktop.json` | Desktop-only preferences: theme, fonts, shortcuts, notifications, keep-awake, tray, automatic updates (`autoUpdate`). |
| `~/.odex/logs/`, `~/.odex/tmp/` | Logs and scratch space. |

Exec-policy rule files look like this:

```toml
[[rule]]
prefix = ["git", "push"]
decision = "prompt"            # allow | prompt | forbid
justification = "Publishing commits"
# pattern = "regex matched against the whole simple command"
```

The engine loads rule files from `~/.odex/rules/` when it starts (restart the engine after editing them). A trusted project can add its own rules in `.odex/rules/*.toml` (in the project folder or a thread's worktree); these are read on every command, so edits apply immediately. Project rules are ignored for untrusted folders.

## Layering, profiles and trust

Odex merges settings in this order. A later layer wins key by key, and tables merge recursively.

1. Built-in defaults (listed with each key below).
2. `~/.odex/config.toml`.
3. The active profile: `profile = "<name>"` in the file, or `odex-engine --profile <name>`.
4. The project's `.odex/config.toml`, only when the folder is trusted.
5. Per-thread choices made in the composer: model, reasoning effort and permission mode.

**Project layer restrictions.** A cloned repository must not be able to redirect your endpoints or loosen your sandbox. So these keys in `.odex/config.toml` are ignored: `model_providers`, `sandbox`, `sandbox_mode`, `approval_policy`, `projects`, `profiles`, `profile`, and `permission_mode = "full-access"`. A project can set everything else, for example `model`, `[models]` entries that point at your existing providers, `[context]`, `[mcp_servers]`, `[hooks]` and `custom_instructions`.

**Trust.** When you open a folder for the first time, the app asks whether to trust it. The answer is stored in the user config under `[projects]`. A folder inherits the trust of its closest configured ancestor. An untrusted project ignores its `.odex/` directory entirely: config, hooks, skills, actions and environments. Hooks need their own trust review even in trusted projects (see [`[hooks]`](#hooks)).

---

## Top-level keys

| Key | Type | Default | Description |
|---|---|---|---|
| `model` | string | first discovered chat model | Model key for the `main` role. Shorthand for `roles.main`, and it wins over it. A key can be a `[models.<key>]` name, `provider:served-model-id`, or a bare served model id found on any endpoint. |
| `profile` | string | none | Active profile from `[profiles]`. |
| `permission_mode` | `"read-only"` \| `"auto"` \| `"full-access"` | `"auto"` | Default permission mode for new threads. `read-only` means no writes and approval for anything not known to be safe. `auto` means a workspace-write sandbox that asks before escalating or using the network. `full-access` means no sandbox and no approvals; the app shows a warning first. |
| `sandbox_mode` | `"read-only"` \| `"workspace-write"` \| `"danger-full-access"` | derived | Compatibility alias. Used only when `permission_mode` is unset: it maps to `read-only`, `auto` and `full-access`. |
| `approval_policy` | `"untrusted"` \| `"on-failure"` \| `"on-request"` \| `"never"` | `"on-request"` | Refines `permission_mode = "auto"`. `untrusted`: ask before any command that isn't a known-safe read. `on-failure`: run everything in the sandbox and ask only to retry a command the sandbox blocked. `on-request`: ask when the agent requests escalation or network access. `never`: never ask; anything needing approval is refused back to the agent. Read-only and full access ignore it. Command rules marked `prompt` still ask (except under `never`, where they are refused). |
| `reasoning_effort` | `"none"` \| `"minimal"` \| `"low"` \| `"medium"` \| `"high"` \| `"xhigh"` | the model's `default_reasoning_effort` | Default effort for new threads. It only has an effect when the model's `reasoning_effort_map` has an entry for the level. |
| `project_doc_max_bytes` | integer | `32768` | Cap on the AGENTS.md text injected into the prompt (global file plus project root down to the cwd). Small-window models (≤ 8,192 tokens) are capped further, at about 12% of the window. |
| `custom_instructions` | string | none | Text appended to every system prompt ("Personalization"). |
| `default_shell` | string | `powershell` on Windows, `zsh` on macOS, else `bash` | Shell for the agent's `shell` tool: `powershell`, `pwsh`, `cmd`, `bash`, `zsh` or `sh`. |
| `worktrees_dir` | path | `~/.odex/worktrees` | Root directory for thread worktrees. |
| `review_instructions` | string | none | Standing guidelines appended to every review the agent runs (`/review`, "Ask agent to review"). Settings → Code review. The reviewer model is `roles.reviewer`. |
| `hidden_models` | array of `provider:served-model-id` | `[]` | Discovered models left out of the model list, the pickers and bare-id lookup. The trash button in Settings → Models & Endpoints adds to it (and deletes a model's `[models]` entry and any role pointing at it); "Removed models" there restores one. A `[models.<key>]` entry for the same model still shows. |

```toml
model = "coder"
permission_mode = "auto"
reasoning_effort = "medium"
custom_instructions = "Prefer small, reviewable commits. Use pnpm, never npm."
```

---

## `[model_providers.<id>]`

One Chat Completions endpoint, usually a vLLM server. You can configure several and use them at the same time, for example a coder model on one GPU box and a vision model on another. When none are configured, Odex uses a provider called `local` at `$ODEX_BASE_URL` (or `odex-engine --base-url`), else `http://localhost:8000/v1`. In that case onboarding runs in the app.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` | string | the id | Display name. |
| `base_url` | string | `http://localhost:8000/v1` | Must include `/v1` for vLLM. A trailing `/` is removed. Odex derives `/health`, `/version` and `/tokenize` from the server root. |
| `api_key` | string | none | Plain-text key, sent as `Authorization: Bearer`. Prefer `api_key_env` or the desktop's encrypted store. |
| `api_key_env` | string | none | Name of an environment variable that holds the key. If it is set, it takes precedence over `api_key`. |
| `headers` | table of strings | `{}` | Extra HTTP headers on every request. |
| `query_params` | table of strings | `{}` | Extra query parameters on every request (some gateways need them). |
| `wire_api` | `"chat"` \| `"responses"` | `"chat"` | Odex sends Chat Completions requests. `responses` is reserved and currently behaves like `chat`. |
| `max_concurrent_requests` | integer | `8` | Requests in flight against this endpoint across all threads. More requests queue. vLLM batches concurrent requests, so parallel agents are cheap. |
| `request_max_retries` | integer | `4` | Retries for connection errors, HTTP 429 and 5xx, with backoff and jitter. |
| `stream_max_retries` | integer | `5` | Retries for streams that drop mid-response or go idle. |
| `stream_idle_timeout_ms` | integer | `300000` | No bytes for this long counts as a dead stream, which is retried rather than left hanging. |
| `connect_timeout_ms` | integer | `10000` | TCP and TLS connect timeout. |
| `request_timeout_ms` | integer | none | Optional cap on a whole request. |
| `enabled` | bool | `true` | Disabled providers are skipped by discovery and role resolution. |

On start, and when you press Refresh, Odex calls `GET /v1/models` on every enabled provider. Each model's `max_model_len` becomes its context window unless `context_window` overrides it. A model that doesn't report one gets 32,768.

```toml
[model_providers.gpu1]
name = "Workstation"
base_url = "http://10.0.0.5:8000/v1"
api_key_env = "GPU1_VLLM_KEY"
max_concurrent_requests = 16

[model_providers.vision-box]
base_url = "https://vllm.example.internal/v1"
headers = { "X-Team" = "platform" }
```

## `[models.<key>]`

Per-model settings, layered on a preset. Models that appear on an endpoint but have no entry here still work: their key is `provider:served-model-id`, and they use the first preset whose `match` glob fits the served id (or `generic`). Add an entry to pin a provider, give a short key, override sampling or the context window, or pick a different preset.

| Key | Type | Default | Description |
|---|---|---|---|
| `provider` | string | first configured provider | Provider id from `[model_providers]`. |
| `model` | string | the key | Model id as served (`--served-model-name`, as listed by `/v1/models`). |
| `preset` | string | first matching preset, else `generic` | Preset id from `presets/models.toml` or `~/.odex/presets.toml`. |
| `display_name` | string | last path segment of `model` | Name shown in pickers. |
| `context_window` | integer | discovered `max_model_len` | Overrides the window used for budgeting. Set it lower than the server's limit to save KV cache, or to test compaction. Never set it higher than the server's limit. |
| `max_output_tokens` | integer | preset value, else `8192` | Cap on generated tokens per request. Each request asks for `min(max_output_tokens, window − prompt − margin)`. |
| `temperature`, `top_p`, `top_k`, `min_p`, `repetition_penalty`, `presence_penalty`, `frequency_penalty` | numbers (`top_k` is an integer) | preset values | Sampling parameters, sent on every request. vLLM never reads `presence_penalty` from the model's `generation_config.json`, so set it here if you need it. |
| `capabilities` | inline table | preset values; otherwise tools `true`, others `false` | `{ tools, vision, parallel_tools, reasoning }`. Without this table, Doctor's findings (cached in `models_cache.json`) adjust the preset: they can turn off `tools` and `parallel_tools`, and detect `vision` and `reasoning`. Setting the table here pins the values. |
| `chat_template_kwargs` | table | none | Sent verbatim as `chat_template_kwargs`, for example `{ enable_thinking = false }`. |
| `extra_body` | table | none | Merged verbatim into every request body, after the effort mapping. |
| `reasoning_effort_map` | table: effort → table | preset values | For each effort level, JSON merged into the request body. Examples: `high = { reasoning_effort = "high" }` (gpt-oss), or `none = { chat_template_kwargs = { enable_thinking = false } }` (Qwen3, GLM). The levels listed here are the ones `/reasoning` offers. |
| `default_reasoning_effort` | effort | preset value | Effort used when the thread doesn't choose one. |
| `tool_profile` | `"codex"` \| `"extended"` \| `"minimal"` | `"extended"` | `codex`: core tools only (shell, exec sessions, `apply_patch`, `update_plan`, `view_image`). `extended`: core plus `read_file`, `list_dir`, `grep`, `glob`, `edit_file`, `write_file`, `read_output` and `recall`. `minimal`: shell, edit, read and plan, for small models. Models with a window ≤ 8,192 tokens always get the compact 7-tool set (D-014). |
| `reasoning_history` | `"drop"` \| `"current_turn"` \| `"all"` | `"current_turn"` | How much earlier reasoning is sent back. Interleaved-thinking models (Kimi-K2-Thinking, MiniMax-M2) need `all`. |
| `coordinate_space` | `"pixels"` \| `"normalized_1000"` \| `"normalized_1"` | `"pixels"` | How a vision model expresses screen coordinates in computer use. Qwen-VL and GLM-V use `normalized_1000`. |
| `structured_output` | `"auto"` \| `"json_schema"` \| `"guided_json"` \| `"none"` | `"auto"` | How compaction summaries, commit messages, reviews and auto-review verdicts are constrained. `auto` and `json_schema` send `response_format: {type: "json_schema"}`. `guided_json` is the legacy vLLM field, which v0.12 and later ignore **silently**; only use it for old servers. `none` sends no constraint and repairs JSON on the client. |
| `tool_call_format` | string | preset value, else `auto` | The model's native tool-call markup (`hermes`, `qwen3_coder`, `llama3_json`, `mistral`, `deepseek`, `pythonic`, `glm45`, `kimi_k2`). Informational for now: the client-side fallback parser detects every supported format automatically. |
| `max_image_px` | integer | `1568` | Longest image edge sent to this model. Larger screenshots are downscaled. |
| `tokenizer_path` | path | none | Reserved for local token counting with an HF `tokenizer.json` (D-012). Odex currently estimates from calibrated ratios and uses `/tokenize` in Doctor. |

```toml
[models.coder]
provider = "gpu1"
model = "Qwen/Qwen3-Coder-30B-A3B-Instruct"
temperature = 0.7
max_output_tokens = 16384

[models.thinker]
provider = "gpu1"
model = "Qwen/Qwen3.6-35B-A3B"
default_reasoning_effort = "low"
[models.thinker.reasoning_effort_map]
none = { chat_template_kwargs = { enable_thinking = false } }
low = { chat_template_kwargs = { enable_thinking = true }, max_tokens = 8192 }

[models.vl]
provider = "vision-box"
model = "Qwen/Qwen3-VL-30B-A3B-Instruct"
capabilities = { tools = true, vision = true, parallel_tools = true, reasoning = false }
coordinate_space = "normalized_1000"
```

## `[roles]`

Maps each role to a model key. Every role falls back to `main`, and `main` falls back to the first discovered chat model.

| Role | Used for |
|---|---|
| `main` | The agent. The top-level `model` key sets it too. |
| `compactor` | Context summaries during compaction. `[context] compactor_model` overrides it. |
| `reviewer` | `/review` and automatic review of escalation requests. |
| `vision` | Describing screenshots and locating UI elements when `main` can't see images. |
| `utility` | Thread titles, commit messages, PR drafts, follow-up suggestions and memory proposals. |
| `embedding` | Reserved for embedding-based recall. Recall uses SQLite FTS5 (BM25) today. |

```toml
[roles]
main = "coder"
compactor = "gpu1:Qwen/Qwen3-30B-A3B-Instruct-2507"
utility = "gpu1:Qwen/Qwen3-30B-A3B-Instruct-2507"
vision = "vl"
```

Image and 3D generation are roles too in Settings, but they pick ComfyUI workflows rather than chat models; see [`[comfyui]`](#comfyui).

## `[comfyui]`

Image and 3D generation through a [ComfyUI](https://github.com/comfyanonymous/ComfyUI) server. With a URL and a workflow set, the agent gets `generate_image` and/or `generate_3d`. Each run queues the workflow on `/prompt`, waits for it on `/history`, and saves every file its output nodes wrote (`SaveImage`, `SaveGLB`, ...) into the workspace, by default as `generated/<first prompt words>.<ext>` (never overwriting). Writes follow the thread's permission mode like any file edit. Not offered in plan or review mode.

| Key | Type | Default | Description |
|---|---|---|---|
| `url` | string | none | Server URL, for example `http://127.0.0.1:8188`. Generation is off without it. |
| `api_key` | string | none | Plain API key for a server behind an authenticating proxy (nginx, Caddy, Cloudflare). Prefer the desktop's encrypted store (Settings → Models & Endpoints → ComfyUI → API key), which takes precedence, or `api_key_env`. |
| `api_key_env` | string | none | Environment variable holding the API key. |
| `api_key_header` | string | none | Header that carries the key. Unset: `Authorization: Bearer <key>` (a key that already starts with `Bearer `, `Basic ` or `Token ` is sent as it is). Set, for example to `X-API-Key`: the key as-is in that header. |
| `headers` | table | `{}` | Extra headers sent with every request, for example Cloudflare Access service-token headers. |
| `comfy_org_api_key` | string | none | Comfy.org API key for partner (API) nodes such as Ideogram, sent with each run as `extra_data.api_key_comfy_org` (ComfyUI's own page uses your sign-in instead, but a workflow queued over the API needs the key). Create one at [platform.comfy.org](https://platform.comfy.org/login). Prefer the desktop's encrypted store (Settings → Models & Endpoints → ComfyUI → Comfy.org API key) or `comfy_org_api_key_env`. |
| `comfy_org_api_key_env` | string | none | Environment variable holding the Comfy.org API key. |
| `image_workflow` | string | none | Workflow (file name without `.json`) behind `generate_image`. Settings → Models & Endpoints → Roles → Image generation. |
| `model3d_workflow` | string | none | Workflow behind `generate_3d`. |
| `workflows_dir` | path | `~/.odex/comfyui` | Folder of workflow files. "Import workflow" in Settings copies a file here after checking it. |
| `timeout_secs` | integer | `900` | Longest wait for one run. An interrupted turn removes the run from ComfyUI's queue. |

**Node packs.** When the server has the ComfyUI-Ideogram4 nodes (`Ideogram4PipelineLoader`, `Ideogram4Generate`) or the ComfyUI_RH_Pixal3D nodes (`RunningHubPixal3DModelLoader`, `RunningHubPixal3DImageTo3D`, `RunningHubPixal3DSaveGLB`), Odex builds their workflow itself and lists it first under "Ready on your ComfyUI": "Ideogram 4.0 (ComfyUI-Ideogram4 nodes)" for images (prompt, width and height from the agent) and "Pixal3D (ComfyUI_RH_Pixal3D nodes)" for 3D (the agent's image through Load Image, saved as GLB, with the loader's `attention_backend` set to `sdpa`, PyTorch's built-in attention, so no flash-attention build is needed, and `low_vram` on so it fits smaller GPUs). Every other setting is the node's own default from `/object_info` (for example Ideogram's default weights and sampler preset, Pixal3D's sparse-convolution backend); edit the saved workflow in `workflows_dir` to change them. Random seeds stay below 2^31, the limit some nodes (Pixal3D) have.

**Ideogram 4.** Ideogram 4 was trained on structured JSON captions, and its built-in safety filter refuses short or context-free prompts more often (a gray image reading "Image blocked by safety filter"; a bare "a scythe" is one). For any Ideogram 4 workflow (the ComfyUI-Ideogram4 nodes, the IdeogramV4 partner node, or the local template) the agent is asked to write a full JSON caption (`high_level_description`, `style_description`, `compositional_deconstruction`) that gives objects their everyday context, and a plain-text prompt is sent as `{"high_level_description": ...}`. When the result is the gray refusal image, nothing is saved and the agent is told to rewrite the prompt with more context.

**ComfyUI's templates.** The Image generation and 3D generation dropdowns also offer the templates of ComfyUI's own library ("Browse Templates", read from the server's `/templates/index.json`) that the server can run: the text-to-image ones for images and the image-to-3D ones for 3D, each checked against `/object_info` for every node type and every model file it uses. "Ready on your ComfyUI" lists the local ones (for example Ideogram v4 Int8 or Pixal3D & TRELLIS.2); "Comfy.org partners (credits)" lists the cloud ones, which appear once a Comfy.org API key is set. Picking a template converts it, adds its placeholders (for image-to-3D only `{{image}}`), saves it under its title and assigns it. Settings says how many are ready and, under "What the other templates need", which model files or nodes the others are missing. The check runs when the page opens (cached for 10 minutes; "Check again" refreshes it).

**Picking a workflow.** The dropdowns also list the workflows already imported and, under "Saved in ComfyUI", the ones saved on the server (`user/<user>/workflows`, read through ComfyUI's `/userdata` API). Picking a saved one converts it from ComfyUI's UI format to API format with the server's node definitions (`/object_info`), saves it in `workflows_dir` and assigns it. Only workflows saved on the server are listed (in ComfyUI: Workflow → Save, or Ctrl+S); tabs open in the browser live only there. Settings says when nothing is saved yet, or why the list couldn't be read (servers or proxies without `/userdata` are listed through `/v2/userdata`), and lists again when the window regains focus. "Import workflow" takes either an API export (Workflow → Export (API)) or a saved workflow file; the latter is converted the same way, so the server must be reachable. Conversion unpacks subgraphs the way ComfyUI does (nodes inside instance `98` become `98:24`), follows links through subgraph inputs and outputs, Reroute, Get/Set and bypassed nodes, leaves out muted and frontend-only nodes (notes, primitives), and keeps only what the workflow's Save nodes need, so previews and prompt helpers don't run. Legacy group nodes aren't converted: export those with Workflow → Export (API). The trash button next to an imported workflow moves its file to the Recycle Bin (Trash) and turns off the roles that used it.

**Placeholders.** Odex fills placeholders in the widgets the agent controls. When a workflow has none, importing adds them: `{{prompt}}` goes into the positive prompt (found by following a sampler's `positive` input, or a guider's `conditioning`, to its text encoder; else the only text encoder; else the only node with a `prompt` text input, as partner nodes like Ideogram have), and `{{image}}` into a lone Load Image node. The negative prompt and every other widget keep the values saved in ComfyUI. To choose other widgets, type the placeholders into them in ComfyUI before saving or exporting:

| Placeholder | Filled with |
|---|---|
| `{{prompt}}` | The agent's prompt. Required for image workflows. |
| `{{negative_prompt}}` | What to avoid (empty when not given). |
| `{{width}}`, `{{height}}` | Pixels, default 1024. A widget holding only the placeholder gets a number. |
| `{{seed}}` | The agent's seed, else random. Without this placeholder, every `seed` / `noise_seed` input gets a fresh random seed per run, like ComfyUI's "randomize". |
| `{{image}}` | A workspace image the agent passes as `image`; Odex uploads it with `/upload/image`. Put it in a Load Image node for image-to-3D. |

The tools' parameters follow the placeholders the workflow uses. `[models.<key>]` settings do not apply here.

```toml
[comfyui]
url = "http://127.0.0.1:8188"
image_workflow = "flux-dev"
model3d_workflow = "hunyuan3d-2"
```

A server behind a proxy that checks an API key answers `HTTP 401`; Settings shows "the server needs an API key" (none sent) or "the server rejected the API key" (wrong key or header). Every request carries the key: `/system_stats`, `/prompt`, `/history`, `/view`, `/upload/image`, `/queue` and `/interrupt`.

```toml
[comfyui]
url = "https://comfy.example.com"
api_key_env = "COMFYUI_API_KEY"
api_key_header = "X-API-Key"   # omit for Authorization: Bearer
```

## `[context]`

Smart context engine settings, also editable in Settings → Context. Ratios are fractions: thresholds are fractions of the **budget**, which is `window − reserved output − margin` (D-013). Out-of-range values are clamped to the range shown.

| Key | Type | Default (range) | Description |
|---|---|---|---|
| `prune_at` | float | `0.70` (0.2–0.98) | Tier 1 (no LLM call): old tool outputs become one-line stubs, superseded file reads and old screenshots are dropped, duplicates are collapsed. |
| `compact_at` | float | `0.85` (≥ `prune_at`, ≤ 0.99) | Tier 2: the compactor summarizes older history into a structured handoff. |
| `keep_recent_ratio` | float | `0.20` (0.02–0.45) | Fraction of the window kept verbatim as recent turns when compacting. |
| `target_after_compact` | float | `0.50` (0.2–0.8) | Fraction of the window to aim for after compaction. |
| `reserve_output_ratio` | float | `0.25` (0.05–0.5) | Fraction of the window reserved for output, capped by the model's `max_output_tokens`. |
| `margin_ratio` | float | `0.03` (0–0.2) | Safety margin for token-estimate error. |
| `tool_output_max_tokens` | integer | `0` | Maximum tokens of a single tool output kept inline (head plus tail, with a `ref:` for the rest). `0` scales the cap to the window. |
| `stub_after_turns` | integer | `3` | Tool outputs older than this many turns are eligible for stubbing in Tier 1. |
| `max_images` | integer | `2` | Screenshots kept as images. Older ones become text stubs. |
| `mcp_tool_budget_ratio` | float | `0.15` (0.01–0.9) | When MCP tool schemas exceed this fraction of the window, they are loaded lazily through `search_tools` (see `[mcp] lazy_tools`). |
| `notes_max_bytes` | integer | `8192` | Cap on `.odex/NOTES.md` (the agent's working notes), which is pinned through compactions. |
| `memories_max_tokens` | integer | `1500` | Token budget for approved memories in the system prompt (the smaller of this and `memories.max_tokens` applies). |
| `compactor_model` | string | `compactor` role | Model key used for compaction. |

Tier 3 (emergency) needs no setting. It runs when the server returns a context-overflow 400 or an estimate turns out wrong: prune aggressively, compact, hard-trim the oldest unpinned items, then retry. Your newest message is never dropped.

```toml
[context]
compact_at = 0.80
keep_recent_ratio = 0.25
compactor_model = "gpu1:Qwen/Qwen3-30B-A3B-Instruct-2507"
```

## `[sandbox]`

User config only; projects can't set it.

| Key | Type | Default | Description |
|---|---|---|---|
| `windows_backend` | `"restricted-token"` \| `"appcontainer"` \| `"none"` | `"restricted-token"` | Windows sandbox for commands in `auto` and `read-only` mode (D-017). `restricted-token` uses a write-restricted, Low-integrity token in a Job Object. It can read your toolchains but **does not isolate the network**, so commands that look like network access ask first. `appcontainer` isolates the network but can't read toolchains under your profile. `none` disables sandboxing, so every command needs approval. Linux uses bubblewrap (`bwrap`) and macOS uses `sandbox-exec`. When a backend is unavailable, commands need approval; they never run unsandboxed silently. |
| `network_access` | bool | `false` | Allow network inside the workspace-write sandbox. |
| `writable_roots` | array of paths | `[]` | Extra directories writable in workspace-write mode, besides the thread's cwd, worktree and temp dir. |

```toml
[sandbox]
writable_roots = ["C:\\Users\\me\\.cargo\\registry"]
```

## `[mcp_servers.<name>]`

MCP servers, also editable in Settings → MCP. Servers start in parallel. A failing server shows a warning and never blocks a thread. Tools are exposed to the model as `mcp__<server>__<tool>` (sanitized to 64 characters).

| Key | Type | Default | Description |
|---|---|---|---|
| `command` | string | none | stdio transport: the executable (`npx`, `uvx`, `node`, a path…). On Windows, `.cmd` shims are resolved. |
| `args` | array of strings | `[]` | Arguments. |
| `env` | table of strings | `{}` | Extra environment variables. |
| `cwd` | path | the engine's cwd | Working directory. |
| `url` | string | none | Streamable HTTP transport endpoint. Set either `command` or `url`. |
| `bearer_token` | string | none | Static bearer token for HTTP servers. |
| `bearer_token_env_var` | string | none | Environment variable holding the bearer token. |
| `headers` | table of strings | `{}` | Extra HTTP headers. |
| `oauth` | bool | `false` | Use OAuth (sign in from Settings → MCP). Tokens are stored in `~/.odex/mcp_tokens.json`. |
| `startup_timeout_ms` | integer | `30000` | Initialize timeout. |
| `tool_timeout_ms` | integer | `120000` | Per-call timeout. |
| `enabled` | bool | `true` | Disabled servers aren't started. |
| `enabled_tools` | array of strings | all | Allow-list of tool names. |
| `disabled_tools` | array of strings | `[]` | Tools to hide. |
| `auto_approve_tools` | array of strings | `[]` | Tools that never ask for approval. "Don't ask again" in an approval card adds to this list. |

```toml
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "..." }
disabled_tools = ["delete_repository"]

[mcp_servers.search]
url = "https://mcp.example.com/mcp"
oauth = true
auto_approve_tools = ["web_search"]
```

## `[mcp]`

| Key | Type | Default | Description |
|---|---|---|---|
| `lazy_tools` | `"auto"` \| `"always"` \| `"never"` | `"auto"` | `auto` switches to lazy tool loading when MCP schemas exceed `context.mcp_tool_budget_ratio` of the window. The model then sees a `search_tools(query)` tool instead of every schema. |

## `[hooks]`

Command hooks, one array of tables per event: `session_start`, `user_prompt_submit`, `pre_tool_use`, `post_tool_use`, `stop`, `notification`. Hooks come from the user config, trusted projects and plugins. **No hook runs until you approve it in the trust review** (Settings → Hooks). The approval records a hash of the command, the matcher and any script file the command refers to, so editing the hook or its script requires a new review.

| Key | Type | Default | Description |
|---|---|---|---|
| `command` | string | required | Shell command. It receives the event as JSON on stdin, with `"hook_event"` set to the event name. |
| `matcher` | string (regex) | all tools | Tool-name filter, for `pre_tool_use` and `post_tool_use` only. |
| `timeout_ms` | integer | `60000` | A timed-out hook is reported as an error and doesn't block. |
| `name` | string | none | Label shown in the UI. |

How the engine reads the result:

- Exit code **2** blocks the action. stderr (or stdout) is the reason.
- Exit code **0** with JSON on stdout, `{"decision": "block" | "allow", "reason": "...", "modified_input": {...}, "additional_context": "..."}`, can block the action, rewrite the tool input, or add context. Any other non-empty stdout is added as context.
- Any other exit code, a timeout or a spawn failure is reported and doesn't block.
- A `stop` hook that blocks makes the agent continue, at most 3 times in a row (D-030).

```toml
[[hooks.pre_tool_use]]
name = "no force push"
matcher = "^(shell|exec_command)$"
command = "node C:/tools/odex-hooks/deny-force-push.js"
timeout_ms = 5000

[[hooks.stop]]
command = "pwsh -File C:/tools/odex-hooks/run-lint.ps1"
```

## `[computer_use]`

Off until you enable it. Also in Settings → Computer Use.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Expose the computer-use tools (`screenshot`, `ui_tree`, `ui_action`, `mouse`, `keyboard`, `window`, `clipboard`, `wait`). |
| `allowed_apps` | array of strings | `[]` | Executables the agent may act on, for example `notepad.exe` or `Code.exe`. |
| `require_approval` | bool | `true` | Ask before each action. |
| `kill_switch` | string | `"Ctrl+Alt+Escape"` | Global hotkey that stops all computer and browser actions. The desktop app registers it (Settings → Computer Use). |
| `prefer_background` | bool | `true` | Prefer non-intrusive UI Automation patterns and background capture over real mouse and keyboard input. |

Password fields, UAC and credential prompts are never touched, whatever these settings say.

## `[browser]`

The in-app browser and browser-use tools.

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `true` | Expose the `browser_*` tools. |
| `allowed_sites` | array of host globs | `[]` | Sites the agent may use without asking, for example `localhost`, `*.example.com`. |
| `blocked_sites` | array of host globs | `[]` | Sites the agent may never open. |
| `developer_mode` | bool | `false` | Enable `browser_eval` and raw CDP access. |
| `cdp_url` | string | none | CDP endpoint for headless `exec` runs, for example `http://127.0.0.1:9222`. Without it, `exec` launches a headless Edge or Chrome. |

## `[memories]`

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Inject approved memories into the system prompt. Memories are opt-in and stored locally in `~/.odex/memories/`. |
| `generate` | bool | same as `enabled` | Have the `utility` model propose memories when a thread ends. Proposals wait for approval in Settings → Memories. |
| `max_tokens` | integer | `1500` | Token cap for memories in the prompt. The smaller of this and `context.memories_max_tokens` applies. |

## `[notifications]`

Reserved. The desktop app keeps notification preferences in `~/.odex/desktop.json`: turn-complete alerts (`background`, `always` or `never`), approval alerts, and keeping the computer awake while threads run. You can change them in Settings → General or Settings → Notifications.

| Key | Type | Description |
|---|---|---|
| `turn_complete` | bool | Reserved. |
| `approval_needed` | bool | Reserved. |
| `keep_awake` | bool | Reserved. |

## `[automatic_review]`

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | The `reviewer` model judges each escalation request (sandbox escape, network, writes outside the workspace) against your goal and a risk rubric. It allows, denies or asks you. `/approve` overrides one denial. |
| `rubric` | string | none | Extra rubric text appended to the reviewer prompt. |

## `[git]`

Settings → Git edits this table.

| Key | Type | Default | Description |
|---|---|---|---|
| `branch_prefix` | string | `"odex/"` | Prefix of the branch created for each worktree thread (`<prefix><thread-name>-<id>`). `""` means no prefix. Characters that are not valid in a ref name become `-`. |
| `allow_force_push` | bool | `false` | Offer "Force with lease" in the push dialog. While it is off, `git/push` refuses `--force-with-lease`. Only the user config counts: a project's `.odex/config.toml` cannot turn it on. |
| `commit_prompt` | string | none | Extra instructions appended to the `utility` model's prompt when it writes a commit message. |
| `pr_prompt` | string | none | Extra instructions appended to the prompt that drafts a pull request title and description. |

```toml
review_instructions = "Flag SQL built with string formatting. New endpoints need tests."

[git]
branch_prefix = "me/"
allow_force_push = true
commit_prompt = "Use Conventional Commits (feat:, fix:, chore:)."
pr_prompt = "Add a Risks section."
```

**GitHub access.** Pull requests (create, view, inbox, reviews, check logs) use the GitHub CLI (`gh`) when it is installed, otherwise the REST API. The token is, in order: the one saved in Settings → Git (stored encrypted as the `github:token` secret, see [Secrets](#secrets); `gh` is run with it as `GH_TOKEN`), then `GITHUB_TOKEN`, then `GH_TOKEN`. The repository is taken from the `origin` remote; GitHub Enterprise hosts use `https://<host>/api/v3`. `ODEX_GITHUB_API` (environment) overrides the REST base URL and always selects the REST API, for proxies and tests.

## `[worktrees]`

Retention of thread worktrees (Settings → Worktrees edits this table; `worktrees_dir` above sets where they live).

| Key | Type | Default | Description |
|---|---|---|---|
| `keep` | integer | `15` | Worktrees to keep on disk. When there are more (counting active and archived threads), the oldest worktrees of **archived** threads are removed, least recently updated first. Worktrees of active threads are never removed automatically. |
| `auto_cleanup` | bool | `true` | Apply `keep` automatically after a thread is archived and after a worktree is created. When off, Settings → Worktrees → "Clean up now" (`worktree/prune`) applies it on demand. |

Before a worktree is removed (by retention, by archiving with "remove worktree", or from Settings), its full working tree, including uncommitted and untracked files, is saved under the hidden ref `refs/odex/archived/<thread-id>`; a worktree whose snapshot fails is kept. The thread keeps its branch and worktree info, so **unarchiving the thread re-creates the worktree** from its branch and restores the snapshot. If that is impossible (the branch is gone), the thread continues in the local checkout.

```toml
[worktrees]
keep = 30
auto_cleanup = true
```

## `[skills]`

Skills live in `~/.odex/skills/<name>/SKILL.md` and in a trusted project's `.odex/skills/`. Only the front-matter `name` and `description` go into the prompt; the body loads when the skill is used.

| Key | Type | Default | Description |
|---|---|---|---|
| `disabled` | array of strings | `[]` | Skill names that stay out of the prompt. |

## `[features]`

| Key | Type | Default | Description |
|---|---|---|---|
| `follow_up_suggestions` | bool | `true` | Suggest follow-up prompts after a turn (`utility` model). |
| `auto_title` | bool | `true` | Name new threads automatically (`utility` model). |
| `undo_snapshots` | bool | `true` | Snapshot the working tree as a hidden git ref after each turn, so a thread can be rolled back. |

## `[profiles.<name>]`

A profile overrides a subset of settings. Select it with `profile = "<name>"` or `odex-engine --profile <name>`. These keys are allowed:

`model`, `permission_mode`, `approval_policy`, `sandbox_mode`, `reasoning_effort`, `custom_instructions`, `[profiles.<name>.roles]` and `[profiles.<name>.context]`.

```toml
profile = "laptop"

[profiles.laptop]
model = "gpu1:Qwen/Qwen3-30B-A3B-Instruct-2507"
reasoning_effort = "none"
[profiles.laptop.context]
compact_at = 0.75

[profiles.careful]
permission_mode = "read-only"
[profiles.careful.roles]
reviewer = "thinker"
```

## `[projects."<path>"]`

Per-folder trust, written by the app when you answer the trust prompt.

| Key | Type | Description |
|---|---|---|
| `trust_level` | `"trusted"` \| `"untrusted"` | Trust for this folder and its subfolders. The longest matching path wins. Paths compare case-insensitively on Windows. |

```toml
[projects.'C:\code\my-app']
trust_level = "trusted"
```

Use a TOML literal string (single quotes) for Windows paths so you don't have to escape the backslashes.

---

## Secrets

Prefer one of these to `api_key` in plain text:

1. **The desktop app's encrypted store.** Keys you enter in Settings → Models & Endpoints, and the GitHub token for PRs, are encrypted with Electron `safeStorage` (DPAPI on Windows, Keychain on macOS, libsecret on Linux) and saved in `~/.odex/secrets.json`. When the engine starts, the desktop decrypts them and passes them in the `initialize` request; a key saved later is pushed to the running engine with `secrets/set`, so it applies without a restart. The engine keeps them in memory only and never writes them to disk or logs. A stored key (`provider:<id>:api_key`) takes precedence over `api_key` and `api_key_env`. On Linux without a keyring, the app refuses to store keys in plain text; use `api_key_env` instead.
2. **`api_key_env`.** The engine reads the variable when it loads the config. This is the right choice for `odex-engine exec`, CI and the smoke suite.

MCP OAuth tokens are the exception: the engine stores them itself, in `~/.odex/mcp_tokens.json` (D-022).

## Other project files

Inside a trusted project's `.odex/` directory:

| File | Purpose |
|---|---|
| `.odex/config.toml` | Project config layer (see [restrictions](#layering-profiles-and-trust)). |
| `.odex/actions.toml` | Run buttons: `[[action]]` with `id`, `name`, `command`, optional `commands = { windows = "...", macos = "...", linux = "..." }` (the entry for the current OS replaces `command`), `cwd`, `icon` (`play`, `test`, `lint`, `build`, `server`) and `openUrl` (opened in the in-app browser once the action is running). Actions run in the integrated terminal with the thread's environment variables. |
| `.odex/environments.toml` | Environments: `[[environment]]` with `id`, `name`, `setup_script` (the default), per-OS `setup_scripts = { windows = "...", macos = "...", linux = "..." }` (the entry for the current OS replaces `setup_script`) and `env`. See [Environments](#environments). |
| `.odex/skills/<name>/SKILL.md` | Project skills. |
| `.odex/NOTES.md` | The agent's working notes. Pinned through compactions, capped by `context.notes_max_bytes`. |
| `AGENTS.md` / `AGENTS.override.md` | Project instructions, discovered from the project root down to the cwd. An override file replaces the `AGENTS.md` in the same directory. Deeper files take precedence. `/init` generates one. |

```toml
# .odex/actions.toml
[[action]]
id = "dev"
name = "Dev server"
command = "npm run dev"
icon = "server"
openUrl = "http://localhost:5173"

[[action]]
id = "test"
name = "Tests"
command = "cargo test"
cwd = "engine"
icon = "test"
```

```toml
# .odex/environments.toml
[[environment]]
id = "default"
name = "Install deps"
setup_script = "npm ci"
setup_scripts = { windows = "npm.cmd ci --no-audit" }
env = { NODE_ENV = "development" }
```

### Environments

**Which environment a thread uses.** `thread/start` takes an optional `environmentId`: an id from the project's `environments.toml` selects it (an unknown id is an error), and `""` means no environment. Without one, the thread uses the project's default environment (set in Edit project → Environments or Settings → Local environments; the default lives in the app's project list, not in the repository), and when no default is set, the **first** environment in the file. The first-environment fallback keeps a repository's committed `environments.toml` working in a fresh clone where nobody chose a default. The choice is stored on the thread (`Thread.environmentId`); "Move to worktree…" also offers a choice.

**Setup script.** When a worktree is created for a thread (worktree mode, fork to worktree, or "Move to worktree…"), the environment's setup script for the current OS runs in the worktree in the background, without the sandbox, in the agent's `default_shell` (PowerShell on Windows unless configured otherwise), for at most 30 minutes. Its output goes to `~/.odex/logs/setup-<thread-id>.log`. The worktree's `setupStatus` is `running`, then `ok` or `failed (exit N): <last output>`; the thread header, the Git panel and Settings → Worktrees show it, with "Rerun setup" (`worktree/setup`) and the log. An agent turn started meanwhile waits for the script to finish. Besides the environment's variables, the script gets `ODEX_WORKTREE_PATH` (the new worktree), `ODEX_SOURCE_TREE_PATH` (the main checkout, e.g. to copy `.env` files) and `ODEX_ENVIRONMENT` (the environment id).

**Variables.** `env` applies to the setup script, every command the agent runs (`shell`, `exec_command`), `!cmd` commands, and the integrated terminals and project actions opened for the thread. Odex's own variables (`ODEX`, `GIT_TERMINAL_PROMPT`, `PAGER`, …) win over them.

Projects edited in the app keep both the default and the per-OS scripts; a client that sends an environment without `setupScripts` keeps the per-OS scripts already in the file (likewise `commands` of actions).

### Desktop preferences for projects

These live in `~/.odex/desktop.json` (not in the repository):

| Setting | Where | Description |
|---|---|---|
| `editor` | Settings → General → "Open files in" | Command for "Open in editor". `{file}` and `{line}` (and `{col}`) are replaced inside each word, so `vim +{line} {file}` or `"C:\Program Files\Sublime Text\subl.exe" {file}:{line}` work with paths that contain spaces; each argument is quoted for `cmd.exe` on Windows and passed as-is elsewhere. Without `{file}`, `code`/`cursor`/`windsurf`/`codium` get `-g file:line` and other editors get the file appended. `system` opens the file with its default app. |
| `projectEditors` | Edit project → General → "Open files with" | Per-project editor command (same format), keyed by project id. It applies to files under the project's folders and under worktrees of its threads. |
| `openDevServerUrls` | Settings → General | Default on. When an agent's `exec_command` process or a project action's terminal prints a local URL with a port (`http://localhost:5173`, `127.0.0.1`, `0.0.0.0`, `[::1]`), it opens once per port in the in-app browser panel. Actions with an `openUrl` open that URL instead. |
