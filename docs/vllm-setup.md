# Serving models for Odex with vLLM

Odex talks to any server that speaks the Chat Completions API (`/v1/chat/completions`), and it is tuned for [vLLM](https://docs.vllm.ai). This guide covers:

- which `vllm serve` flags each model needs, and why
- how to run vLLM on one or more GPUs, locally or remotely
- what Odex's Doctor checks
- how to fix the common problems

Parser names and API behavior were checked against **vLLM v0.30.0** (September 2026). The research notes, with sources, are in [`research/vllm-flags.md`](research/vllm-flags.md). The commands below are the ones shipped in [`presets/models.toml`](../presets/models.toml), so Odex's Doctor suggests the same flags.

> **Status:** these commands come from the vLLM source, the vLLM recipes and the model cards. The Odex test endpoint was unreachable while this guide was written, so they have not yet been run end to end against Odex. Run the [smoke suite](#smoke-testing-a-server-with-odex) on your own server to check your setup.

## Contents

1. [Requirements](#requirements)
2. [Quick start](#quick-start)
3. [Per-model commands](#per-model-commands)
4. [What each flag does](#what-each-flag-does)
5. [Context length (`--max-model-len`)](#context-length---max-model-len)
6. [Multiple GPUs and multiple models](#multiple-gpus-and-multiple-models)
7. [API keys and remote endpoints](#api-keys-and-remote-endpoints)
8. [Doctor](#doctor)
9. [Smoke-testing a server with Odex](#smoke-testing-a-server-with-odex)
10. [Troubleshooting](#troubleshooting)

---

## Requirements

- **vLLM on Linux with an NVIDIA or AMD GPU.** vLLM doesn't run natively on Windows. On a Windows machine, use one of these:
  - WSL2 with the NVIDIA driver's CUDA-on-WSL support: `pip install vllm` inside Ubuntu on WSL. WSL forwards `localhost:8000` to Windows.
  - Docker Desktop with the WSL2 backend and GPU support: `docker run --gpus all -p 8000:8000 vllm/vllm-openai:latest <model> <flags>`.
  - A Linux GPU server on your network (see [remote endpoints](#api-keys-and-remote-endpoints)).
- **vLLM version.** v0.10.2 or later works for most models. Some parsers need newer releases (listed per model below). The latest release is the safest choice: newer versions fix parser bugs, for example the `qwen3` reasoning parser from v0.17.
- **VRAM** for the model weights plus the KV cache for your `--max-model-len` (see [context length](#context-length---max-model-len)).

## Quick start

```bash
# 1. Serve a coding model (Linux or WSL2).
pip install -U vllm
vllm serve Qwen/Qwen3-Coder-30B-A3B-Instruct \
  --enable-auto-tool-choice --tool-call-parser qwen3_coder \
  --max-model-len 131072 --enable-prefix-caching

# 2. Check it from the machine running Odex.
curl http://localhost:8000/v1/models
```

3. Start Odex. Onboarding detects `localhost:8000`, lists the served models with their context windows, lets you assign roles and runs Doctor. Without the app:

```bash
odex-engine --base-url http://localhost:8000/v1 doctor
```

Or add the endpoint to `~/.odex/config.toml`. See [`config.md`](config.md) for every key.

```toml
[model_providers.local]
base_url = "http://localhost:8000/v1"

[roles]
main = "local:Qwen/Qwen3-Coder-30B-A3B-Instruct"
```

Odex picks a preset by matching the **served model id** against each preset's `match` globs, for example `*qwen3-coder*`. If you rename the model with `--served-model-name`, choose the preset explicitly:

```toml
[models.coder]
provider = "local"
model = "my-coder"            # the --served-model-name
preset = "qwen3-coder"
```

---

## Per-model commands

Each command is the preset's `serve` line. Sampling parameters (temperature, top_p, top_k, ...) are **not** server flags: Odex sends the preset's values with every request. Parallelism flags (`-tp`, `-dp`, expert parallel) depend on your hardware, so they are left out; see [multiple GPUs](#multiple-gpus-and-multiple-models).

### Qwen

| Preset | Models | Command | Min vLLM | Notes |
|---|---|---|---|---|
| `qwen3-coder` | Qwen3-Coder-30B-A3B / 480B-A35B Instruct (+FP8) | `vllm serve Qwen/Qwen3-Coder-30B-A3B-Instruct --enable-auto-tool-choice --tool-call-parser qwen3_coder --max-model-len 131072 --enable-prefix-caching` | 0.10.0 | Non-thinking: **no** reasoning parser. 256k native; drop to `--max-model-len 32768` if memory is tight. `qwen3_xml` is an alias from v0.11. |
| `qwen3-coder-next` | Qwen3-Coder-Next | `vllm serve Qwen/Qwen3-Coder-Next --enable-auto-tool-choice --tool-call-parser qwen3_coder --max-model-len 131072 --enable-prefix-caching` | 0.15.0 | Non-thinking. |
| `qwen3_5` | Qwen3.5 / 3.6 / 3.8 | `vllm serve Qwen/Qwen3.6-35B-A3B --trust-remote-code --enable-auto-tool-choice --tool-call-parser qwen3_coder --reasoning-parser qwen3 --max-model-len 131072 --enable-prefix-caching` | 0.17.0 | Thinks by default. `/reasoning none` sends `enable_thinking=false`. Add `--language-model-only` to skip the vision encoder. Keep at least 128k for long thinking. |
| `qwen3-thinking` | Qwen3-*-Thinking-2507, Qwen3-Next-Thinking | `vllm serve Qwen/Qwen3-30B-A3B-Thinking-2507 --enable-auto-tool-choice --tool-call-parser hermes --reasoning-parser deepseek_r1 --max-model-len 131072 --enable-prefix-caching` | 0.10.0 | The chat template pre-fills `<think>`. `deepseek_r1` handles that on every version; `qwen3` only from v0.17. |
| `qwen3-instruct` | Qwen3-*-Instruct-2507, Qwen3-Next-Instruct | `vllm serve Qwen/Qwen3-30B-A3B-Instruct-2507 --enable-auto-tool-choice --tool-call-parser hermes --max-model-len 131072 --enable-prefix-caching` | 0.10.0 | Non-thinking: **no** reasoning parser. The 235B model also needs `--trust-remote-code`. |
| `qwen3-hybrid` | Qwen3-8B / 14B / 32B / 30B-A3B / 235B-A22B (original) | `vllm serve Qwen/Qwen3-32B --enable-auto-tool-choice --tool-call-parser hermes --reasoning-parser qwen3 --max-model-len 32768 --enable-prefix-caching` | 0.8.5 | 32k native. For 128k add `--hf-overrides '{"max_position_embeddings":131072,"rope_parameters":{"rope_type":"yarn","factor":4.0,"original_max_position_embeddings":32768}}' --max-model-len 131072` (`--rope-scaling` no longer exists). |
| `qwen3-vl` | Qwen3-VL / Qwen2.5-VL Instruct | `vllm serve Qwen/Qwen3-VL-30B-A3B-Instruct --enable-auto-tool-choice --tool-call-parser hermes --max-model-len 128000 --limit-mm-per-prompt.video 0 --enable-prefix-caching` | 0.11.0 | The `vision` role. Points in normalized 0–1000 coordinates. Thinking variants add `--reasoning-parser qwen3` (v0.17+) or `deepseek_r1`. Parsers were derived from the chat template. |

### GLM (Z.ai)

| Preset | Models | Command | Min vLLM | Notes |
|---|---|---|---|---|
| `glm-4_5` | GLM-4.5, 4.5-Air, 4.6 | `vllm serve zai-org/GLM-4.5-Air --trust-remote-code --enable-auto-tool-choice --tool-call-parser glm45 --reasoning-parser glm45 --max-model-len 131072 --enable-prefix-caching` | 0.10.1 | Hybrid thinking; `/reasoning none` turns it off. |
| `glm-4_7` | GLM-4.7, 4.7-Flash, GLM-5 | `vllm serve zai-org/GLM-4.7-FP8 --trust-remote-code --enable-auto-tool-choice --tool-call-parser glm47 --reasoning-parser glm45 --max-model-len 131072 --enable-prefix-caching` | 0.14.0 | GLM-5 uses `--reasoning-parser glm47` (v0.24+) and the recipe adds `--chat-template-content-format=string`. |
| `glm-4v` | GLM-4.5V, 4.1V, 4.6V | `vllm serve zai-org/GLM-4.5V --trust-remote-code --enable-auto-tool-choice --tool-call-parser glm45 --reasoning-parser glm45 --max-model-len 65536 --mm-encoder-tp-mode data --enable-prefix-caching` | 0.10.2 | Its `generation_config.json` is almost greedy (`top_k: 1`). That's harmless with Odex, which always sends explicit sampling. |

### gpt-oss

| Preset | Models | Command | Min vLLM | Notes |
|---|---|---|---|---|
| `gpt-oss` | gpt-oss-20b, gpt-oss-120b | `vllm serve openai/gpt-oss-20b --enable-auto-tool-choice --tool-call-parser openai --reasoning-parser openai_gptoss --max-model-len 131072 --enable-prefix-caching` | 0.10.2 | Harmony format; no chat template needed. **Always set both parsers.** Effort maps to the `reasoning_effort` request field. |

### DeepSeek

| Preset | Models | Command | Min vLLM | Notes |
|---|---|---|---|---|
| `deepseek-v4` | DeepSeek-V4-Flash, V4-Pro | `vllm serve deepseek-ai/DeepSeek-V4-Flash --trust-remote-code --tokenizer-mode deepseek_v4 --enable-auto-tool-choice --tool-call-parser deepseek_v4 --reasoning-parser deepseek_v4 --kv-cache-dtype fp8 --block-size 256 --max-model-len 131072 --enable-prefix-caching` | 0.20.0 | Thinking via `chat_template_kwargs.thinking`. |
| `deepseek-v3_2` | DeepSeek-V3.2 (+Exp) | `vllm serve deepseek-ai/DeepSeek-V3.2 --trust-remote-code --tokenizer-mode deepseek_v32 --enable-auto-tool-choice --tool-call-parser deepseek_v32 --reasoning-parser deepseek_v3 --max-model-len 131072 --enable-prefix-caching` | 0.13.0 | Thinking is off by default; tool calls work in both modes. |
| `deepseek-v3_1` | DeepSeek-V3.1, V3.1-Terminus | `vllm serve deepseek-ai/DeepSeek-V3.1-Terminus --trust-remote-code --enable-auto-tool-choice --tool-call-parser deepseek_v31 --reasoning-parser deepseek_v3 --chat-template examples/tool_chat_template_deepseekv31.jinja --max-model-len 131072 --enable-prefix-caching` | 0.11.1 | Tool calls only work **without** thinking, so the default effort is `none`. See [chat templates](#chat-templates). |

### Mistral

| Preset | Models | Command | Min vLLM | Notes |
|---|---|---|---|---|
| `devstral` | Devstral, Devstral 2, Mistral Small/Medium/Large, Magistral, Codestral, Ministral | `vllm serve mistralai/Devstral-Small-2507 --tokenizer-mode mistral --config-format mistral --load-format mistral --enable-auto-tool-choice --tool-call-parser mistral --max-model-len 131072 --enable-prefix-caching` | 0.10.0 | Devstral 2 detects the Mistral format automatically (`mistral_common >= 1.8.6`). Reasoning variants (Magistral, Mistral-Small-4) add `--reasoning-parser mistral`. Mistral uses 9-character tool-call ids; Odex returns them unchanged. |

### Moonshot, MiniMax, Meta, ByteDance, JetBrains

| Preset | Models | Command | Min vLLM | Notes |
|---|---|---|---|---|
| `kimi-k2` | Kimi-K2-Instruct (-0905) | `vllm serve moonshotai/Kimi-K2-Instruct-0905 --trust-remote-code --enable-auto-tool-choice --tool-call-parser kimi_k2 --max-model-len 131072 --enable-prefix-caching` | 0.12.0 | Non-thinking: **don't** set `--reasoning-parser kimi_k2`, which assumes thinking. |
| `kimi-k2-thinking` | Kimi-K2-Thinking, K2.5, K2.6, K2.7 | `vllm serve moonshotai/Kimi-K2-Thinking --trust-remote-code --enable-auto-tool-choice --tool-call-parser kimi_k2 --reasoning-parser kimi_k2 --max-model-len 131072 --enable-prefix-caching` | 0.12.0 | Interleaved thinking: Odex sends reasoning back every turn (`reasoning_history = "all"`). |
| `minimax-m2` | MiniMax-M2, M2.1, M2.5, M2.7 | `vllm serve MiniMaxAI/MiniMax-M2 --trust-remote-code --enable-auto-tool-choice --tool-call-parser minimax_m2 --reasoning-parser minimax_m2 --max-model-len 131072 --enable-prefix-caching` | 0.11.1 | Interleaved thinking. Use `minimax_m2`, not `minimax_m2_append_think`: Odex sends back the `reasoning` field. |
| `llama4` | Llama 4 Scout / Maverick | `vllm serve meta-llama/Llama-4-Scout-17B-16E-Instruct --enable-auto-tool-choice --tool-call-parser llama4_pythonic --chat-template examples/tool_chat_template_llama4_pythonic.jinja --max-model-len 131072 --enable-prefix-caching` | 0.10.0 | Native windows are huge (10M for Scout), so set `--max-model-len` to what fits in memory. |
| `llama3` | Llama 3.1 / 3.2 / 3.3 | `vllm serve meta-llama/Llama-3.3-70B-Instruct --enable-auto-tool-choice --tool-call-parser llama3_json --chat-template examples/tool_chat_template_llama3.1_json.jinja --max-model-len 131072 --enable-prefix-caching` | 0.10.0 | No parallel tool calls. Odex uses the `minimal` tool profile. |
| `seed-oss` | Seed-OSS-36B-Instruct | `vllm serve ByteDance-Seed/Seed-OSS-36B-Instruct --trust-remote-code --enable-auto-tool-choice --tool-call-parser seed_oss --reasoning-parser seed_oss --max-model-len 65536 --enable-prefix-caching` | 0.11.0 | Thinking budget via `chat_template_kwargs.thinking_budget` (Odex maps effort levels to 0 / 512 / 1024 / 4096 / unlimited). |
| `mellum2` | Mellum2-12B-A2.5B-Instruct | `vllm serve JetBrains/Mellum2-12B-A2.5B-Instruct --trust-remote-code --enable-auto-tool-choice --tool-call-parser hermes --max-model-len 131072 --enable-prefix-caching` | 0.23.0 | The `-Thinking` variant adds `--reasoning-parser qwen3`. |
| `generic` | anything else | `vllm serve <model> --enable-auto-tool-choice --tool-call-parser hermes --enable-prefix-caching` | — | Fallback. Run Doctor to find out what the model supports, and check the model card for the right parser. |

---

## What each flag does

| Flag | Why Odex needs it |
|---|---|
| `--enable-auto-tool-choice` | Odex sends `tool_choice: "auto"` on every agent request. Without this flag, vLLM rejects those requests with HTTP 400: `"auto" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set`. |
| `--tool-call-parser <name>` | Turns the model's tool-call markup into structured `tool_calls`. It must match the model family. With the wrong parser (or none), tool calls arrive as plain text in `content`. Odex's fallback parser recovers them, but streaming and parallel calls suffer, and Doctor warns. vLLM refuses to start if `--enable-auto-tool-choice` is set without a parser. |
| `--reasoning-parser <name>` | Moves the model's thinking into the `reasoning` field so it doesn't pollute the answer or the history. **Only for thinking models.** On a non-thinking model (Qwen3-Coder, Qwen3-Instruct-2507, Kimi-K2-Instruct), recent parsers start in "reasoning" state, so the whole answer lands in `reasoning` and `content` comes back empty. |
| `--max-model-len <n>` | The context window. vLLM reserves KV cache for it, and Odex reads it from `/v1/models` to budget every request. See [below](#context-length---max-model-len). |
| `--enable-prefix-caching` | Reuses the KV cache for a shared prompt prefix. Odex keeps the prefix byte-stable (fixed system prompt, deterministic tool order, AGENTS.md near the top, volatile data late) and prunes in large batches, so each step of a long turn only prefills the new tokens. That's the biggest latency win for agents. Recent vLLM versions enable it by default; the presets pass it explicitly anyway. Doctor measures the speed-up. |
| `--trust-remote-code` | Needed by models that ship custom code or tokenizers (GLM, DeepSeek, Kimi, MiniMax, Qwen3.5+). |
| `--tokenizer-mode` | `mistral` for Mistral-format checkpoints, `deepseek_v32` and `deepseek_v4` for the new DeepSeek chat formats. Without it, the chat format and tool calls are wrong. |
| `--chat-template <file>` | Some models need vLLM's tool-aware template (Llama 3.x and 4, DeepSeek-V3.1). See [chat templates](#chat-templates). |
| `--served-model-name <id>` | Optional. Odex matches presets by served id, so either keep the HF id in the name or set `preset = "..."` in `[models.<key>]`. |
| `--api-key <key>` | Optional. See [API keys](#api-keys-and-remote-endpoints). |
| `--kv-cache-dtype fp8` | Halves the KV cache, so a longer `--max-model-len` fits in the same memory. Quality cost is small on recent GPUs. |
| `--gpu-memory-utilization 0.9` | Share of VRAM vLLM may use (default 0.9). Lower it when other processes share the GPU. |
| `--max-num-seqs <n>` | Concurrent sequences. Several Odex threads and subagents run in parallel; set Odex's `max_concurrent_requests` to the same value or lower. |

Flags Odex does **not** need:

- Sampling flags (`--generation-config`, `--override-generation-config`): Odex sends sampling with every request. vLLM never reads `presence_penalty` from the model config anyway.
- `--default-chat-template-kwargs`: Odex sends `chat_template_kwargs` per request (for example `enable_thinking`, controlled by `/reasoning`). A server-wide default still works as a fallback.
- `--enable-reasoning`: removed in v0.10. Some old model cards still show it.

### Chat templates

`--chat-template examples/<file>.jinja` paths are relative to the vLLM repository. The official Docker image ships them in `/vllm-workspace/examples/`. A `pip install vllm` may not include them, so download the file from the vLLM GitHub repository at the tag that matches your version:

```bash
VLLM_TAG=v$(python -c "import vllm; print(vllm.__version__)")
curl -LO https://raw.githubusercontent.com/vllm-project/vllm/$VLLM_TAG/examples/tool_chat_template_llama3.1_json.jinja
vllm serve meta-llama/Llama-3.3-70B-Instruct ... --chat-template ./tool_chat_template_llama3.1_json.jinja
```

---

## Context length (`--max-model-len`)

- **Bigger is better, up to what fits.** Odex works with any window: the context engine prunes and compacts automatically, and the soak test runs 200 steps on a 4,096-token window. But each compaction costs a summarization call and loses detail, so give agents room. Aim for **64k–128k** for coding agents, and at least **32k**.
- **Thinking models need more.** Reasoning tokens count toward the window while a turn runs. Keep at least 64k, and 128k for Qwen3.5/3.6 and the Thinking-2507 models.
- **Memory.** The KV cache grows linearly with the window and with concurrency. If vLLM says the KV cache can't fit `max_model_len`, reduce the window, use `--kv-cache-dtype fp8`, quantize (FP8/AWQ), or add GPUs.
- **Odex reads it automatically** from `/v1/models`. To make Odex budget for less than the server allows (to leave room for other users, or to test compaction), set `context_window` in `[models.<key>]`. Never set it higher than the server's limit.
- **Small windows (≤ 8,192 tokens)** switch Odex to a compact system prompt and a 7-tool set, and cap AGENTS.md at about 12% of the window.

## Multiple GPUs and multiple models

- **One model across GPUs in one machine:** `--tensor-parallel-size N` (`-tp N`), where N divides the attention head count (2, 4, 8). For large MoE models (Qwen3-Coder-480B, DeepSeek, Kimi-K2, GLM-4.x), add `--enable-expert-parallel`. The vLLM recipes list tested combinations per model.
- **More throughput for many parallel agents:** `--data-parallel-size N` runs N replicas behind one endpoint. Combine it with `-tp` as memory requires.
- **Across machines:** `--pipeline-parallel-size` with a Ray cluster. See the vLLM distributed-serving docs.
- **Vision models:** `--mm-encoder-tp-mode data` runs the vision encoder data-parallel, which is faster for screenshot-heavy computer use.
- **Several models on one box:** run one `vllm serve` per model on different ports and GPUs. Each one only claims `--gpu-memory-utilization` of the GPUs it can see.

  ```bash
  CUDA_VISIBLE_DEVICES=0,1 vllm serve Qwen/Qwen3-Coder-30B-A3B-Instruct --port 8000 -tp 2 ...
  CUDA_VISIBLE_DEVICES=2   vllm serve Qwen/Qwen3-VL-30B-A3B-Instruct    --port 8001 ...
  ```

  Then add one provider per port and assign roles:

  ```toml
  [model_providers.coder]
  base_url = "http://gpu-box:8000/v1"
  [model_providers.vision]
  base_url = "http://gpu-box:8001/v1"
  [roles]
  main = "coder:Qwen/Qwen3-Coder-30B-A3B-Instruct"
  vision = "vision:Qwen/Qwen3-VL-30B-A3B-Instruct"
  ```

- **Small helper models.** The `compactor` and `utility` roles (summaries, titles, commit messages) can use a smaller, faster model. That frees the main model for the agent.

## API keys and remote endpoints

- **Server side:** `vllm serve ... --api-key <key>`, or set `VLLM_API_KEY`. vLLM then requires `Authorization: Bearer <key>` on `/v1/*` routes. A wrong key gets HTTP 401 with the body `{"error":"Unauthorized"}`. `/health`, `/version`, `/tokenize` and `/metrics` stay **unauthenticated**, so don't expose the port to untrusted networks.
- **Odex side:** enter the key in Settings → Models & Endpoints. It is encrypted with the OS keychain and only held in the engine's memory. For the CLI, use `api_key_env`:

  ```toml
  [model_providers.gpu1]
  base_url = "http://10.0.0.5:8000/v1"
  api_key_env = "GPU1_VLLM_KEY"
  ```

- **Remote over SSH:** the simplest secure option is a tunnel, `ssh -N -L 8000:localhost:8000 gpu-box`, with Odex pointed at `http://localhost:8000/v1`.
- **Behind a reverse proxy (TLS, auth):**
  - **Turn off response buffering** for `/v1/chat/completions`, or streaming arrives in bursts and the idle timeout fires. For nginx: `proxy_buffering off; proxy_read_timeout 600s;`.
  - Allow long requests. Odex's `stream_idle_timeout_ms` defaults to 300 s.
  - Keep the `/v1` prefix in `base_url`, for example `https://llm.example.com/vllm/v1`. Odex finds `/health` and `/tokenize` by stripping the trailing `/v1`.
  - Gateways that need extra headers or query parameters: use `headers` and `query_params` on the provider.
- **Concurrency:** each provider has a `max_concurrent_requests` queue (default 8). Raise it for a big server with `--max-num-seqs` to match. vLLM's continuous batching makes parallel threads and subagents cheap.

---

## Doctor

Doctor probes one model on one endpoint and shows a pass / warn / fail table, plus the `vllm serve` flags that would fix what failed (with a copy button in the app). Results are cached in `~/.odex/models_cache.json` and feed the model's detected capabilities.

Run it from Settings → Models & Endpoints → Doctor, at the end of onboarding, or from the CLI:

```bash
odex-engine doctor                       # first model of every configured endpoint
odex-engine doctor --model coder         # a [models] key, provider:model, or a served id
odex-engine doctor --quick               # skip the prefix-cache timing
odex-engine doctor --json                # machine-readable report
odex-engine --base-url http://gpu-box:8000/v1 doctor   # no config needed
```

| Check | What it does | If it fails or warns |
|---|---|---|
| `connect`: Endpoint reachable | `GET /v1/models` and `/version`; checks that the model is served and reads `max_model_len`. | **Fail:** wrong `base_url` (did you include `/v1`?), server down, firewall, wrong API key (401), or the model id isn't served. The detail lists the served ids. Doctor stops here. |
| `health`: /health | `GET /health` at the server root. | **Fail:** vLLM reports its engine as dead; restart it. **Warn:** no `/health` (not vLLM, or a proxy only forwards `/v1`). Harmless. |
| `streaming`: Streaming + usage | Streams a short reply with `stream_options.include_usage`. | **Fail:** streaming is broken (often a buffering proxy); Doctor stops here. **Warn:** no incremental deltas (buffering proxy) or no `usage` chunk. Without usage, Odex can't calibrate token counts as well. |
| `toolCall`: Native tool call | Asks for a `get_weather` call with tools offered. | **Fail:** no tool call at all, or a 400 like `"auto" tool choice requires --enable-auto-tool-choice`. Add `--enable-auto-tool-choice --tool-call-parser <parser>`. **Warn:** the call came back as text and the client fallback parser recovered it: the parser is missing or wrong for this model. |
| `streamedToolCall`: Streamed tool call | The same, streamed: expects `tool_calls` deltas. | **Fail:** no calls. **Warn:** calls only appeared after fallback parsing. Same fix as above. |
| `parallelToolCalls`: Parallel tool calls | Asks for two independent calls in one response. | **Warn** only: the model calls one tool at a time. Odex still works, just with more round trips. Llama 3.x can't do parallel calls. |
| `reasoning`: Reasoning parser | Reasoning models only: asks for a short chain of thought. | **Pass:** thinking arrived in a dedicated field. **Warn:** thinking was mixed into `content` (`</think>` seen). Add `--reasoning-parser <parser>`, or upgrade vLLM (see [reasoning leaks](#reasoning-text-shows-up-in-the-answer)). Odex strips it on the client meanwhile. **Warn:** no reasoning returned; thinking may be disabled. **Skip:** not a reasoning model. |
| `vision`: Vision (image input) | Sends a 16×16 red PNG and asks for its color. | **Skip:** the server rejected the image (text-only model). Assign a `vision` role model for computer use. **Warn:** the image was accepted but the answer was wrong. |
| `tokenize`: /tokenize | Counts tokens for a chat via `POST /tokenize`. | **Warn** only: Odex estimates token counts instead. Usually means a proxy doesn't forward the server root. |
| `prefixCache`: Prefix caching | Sends the same ~5k-token prompt twice and compares time to first token (or cached-token usage). | **Warn:** less than a 1.3× speed-up and no cached tokens: add `--enable-prefix-caching`. Skipped with `--quick`. |
| `structuredOutput`: Structured output | Requests a JSON object via `response_format: {type: "json_schema"}`. | **Fail:** the server rejected `response_format`. **Warn:** the output didn't match the schema. Odex then repairs JSON on the client and falls back to an extractive summary if compaction fails. Make sure you aren't relying on `guided_json` (see [structured outputs](#structured-outputs-response_format-vs-guided_json)). |

The suggested command combines the model's preset `serve` line with the flags from the failing checks.

## Smoke-testing a server with Odex

Headless. `--base-url` only applies when `~/.odex/config.toml` defines no `[model_providers]`; pass `--home <empty dir>` to ignore your config and keep the test thread out of your history.

```bash
mkdir /tmp/odex-smoke && cd /tmp/odex-smoke
odex-engine --base-url http://gpu-box:8000/v1 exec \
  "Create hello.txt containing 'hello from odex', then print it with a shell command." \
  --auto-approve
cat hello.txt
```

Useful `exec` flags: `-m <model>`, `-C <dir>`, `--permission-mode read-only|auto|full-access`, `--effort <level>`, `--json` (events as JSON lines), `--plan`, `--resume <thread-id>`, `--output-last-message <file>` and `--timeout <secs>`. Without `--auto-approve`, approval requests are denied.

The opt-in real-vLLM test suite (`engine/core/tests/real_vllm.rs`) automates this. It runs discovery plus Doctor, a file-writing task, fixing a failing Node test, an MCP round-trip, and compaction on a 4,096-token window:

```bash
cd engine
cargo build -p odex-mcp-client --bins     # optional: enables the MCP round-trip
ODEX_E2E_BASE_URL=http://gpu-box:8000/v1 \
ODEX_E2E_MODEL=Qwen/Qwen3-Coder-30B-A3B-Instruct \
  cargo test -p odex-core --test real_vllm -- --nocapture --test-threads=1
```

`ODEX_E2E_API_KEY` is passed to the server if set. `ODEX_E2E_TIMEOUT_SECS` changes the per-turn timeout (default 600). Without `ODEX_E2E_BASE_URL`, every test prints "skipped" and passes.

---

## Troubleshooting

### Context-overflow errors

vLLM answers HTTP 400 when prompt plus `max_tokens` exceeds `--max-model-len`. Depending on the version, the message is one of:

- `This model's maximum context length is N tokens. However, you requested M output tokens and your prompt contains T input tokens…` (v0.18+)
- `This model's maximum context length is N tokens. However, your request has T input tokens…` or `'max_tokens' or 'max_completion_tokens' is too large…` (up to v0.17)
- `max_tokens=M cannot be greater than max_model_len=…` (v0.18+)

Odex parses all of them and recovers on its own: it prunes, compacts, trims and retries (Tier 3), so a thread shows a compaction notice instead of an error. If you still see overflow errors:

- **`context_window` set higher than the server's `--max-model-len`.** Remove the override, or lower it.
- **A proxy or gateway rewrites the error body.** Odex needs the text above to detect an overflow. Pass the vLLM error through unchanged.
- **`max_output_tokens` larger than the window** (the `max_tokens … cannot be greater than max_model_len` message). Lower it in `[models.<key>]`.
- **The compactor role points at a model with a smaller window** than the history it summarizes. Odex runs map-reduce in chunks, but a very small compactor makes compaction slow. Use a model with at least 16k.

### Tool calls arrive as text

The symptoms: `<tool_call>{...}</tool_call>`, `[TOOL_CALLS]`, `<|tool▁calls▁begin|>` or similar markup in the answer, or Doctor's `toolCall` check warns "recovered by the client fallback parser".

- The server has no tool parser, or the wrong one. Restart vLLM with `--enable-auto-tool-choice --tool-call-parser <parser>` from the [table above](#per-model-commands).
- Meanwhile, Odex's fallback parser recognizes the Hermes, Qwen3-Coder XML, GLM, Mistral, DeepSeek, Kimi, Llama-3 JSON and pythonic formats automatically. It pauses streaming at the marker so raw markup doesn't flash in the thread, and it ignores calls to tool names that weren't offered.
- If you get HTTP 400 `"auto" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set`, the flags are missing entirely. Odex can't work around that, because the request is rejected.
- If the model ignores tools altogether, check that the chat template supports tools (Llama 3.x and DeepSeek-V3.1 need `--chat-template`) and that the preset didn't set `capabilities.tools = false`.

### Reasoning text shows up in the answer

The symptoms: `<think>…</think>` in messages, or the model's deliberation appears as the answer.

- **No reasoning parser.** Add `--reasoning-parser <parser>` for thinking models (Qwen3 hybrid and Thinking, Qwen3.5+, GLM, gpt-oss, DeepSeek with thinking, Kimi-K2-Thinking, MiniMax-M2, Seed-OSS).
- **The `qwen3` parser on vLLM before v0.17** only splits reasoning when both `<think>` and `</think>` are generated. Thinking-2507 and Qwen3.5+ templates pre-fill `<think>`, so only `</think>` appears. Upgrade to v0.17+ or use `--reasoning-parser deepseek_r1`.
- Odex strips `<think>` blocks on the client for every model, and holds back early content on reasoning models to catch a pre-filled `<think>`. So the thread stays clean, but history and token counts are better with a server-side parser.
- **The opposite problem, an empty answer with everything in "reasoning",** means a reasoning parser is set on a **non-thinking** model (Qwen3-Coder, Qwen3-Instruct-2507, Kimi-K2-Instruct). Remove `--reasoning-parser`.
- **Field names changed across versions:** `reasoning_content` up to v0.11.0, both names from v0.11.1 to v0.15, and only `reasoning` from v0.16. Odex reads both.

### Structured outputs: `response_format` vs `guided_json`

Odex uses structured output for compaction summaries, commit messages, `/review` findings and automatic-review verdicts.

- Odex sends **`response_format: {"type": "json_schema", ...}`**, which works on every vLLM version checked (v0.10 to v0.30).
- vLLM's old **`guided_json`** (and the other `guided_*` fields) was deprecated in v0.11 and **removed in v0.12**. Since then it is **silently ignored**: HTTP 200 with unconstrained output and only a server-side warning. Don't set `structured_output = "guided_json"` unless the server is older than v0.11. Newer servers accept `structured_outputs: {...}` in the request body (v0.11+), but Odex doesn't need it.
- If summaries keep falling back to the extractive mode (the compaction notice says so), run Doctor and look at `structuredOutput`. Fixes: upgrade vLLM; choose a backend with `--structured-outputs-config.backend xgrammar` or `guidance` if the default fails on a schema; or point the `compactor` role at a model that follows JSON schemas well.
- With a reasoning parser set, the constraint applies to the content after reasoning. Qwen3-Coder doesn't need a reasoning parser at all.

### Other problems

- **401 `{"error":"Unauthorized"}`:** the API key is missing or wrong. Check Settings → Models & Endpoints, or the variable named in `api_key_env`.
- **"model … does not exist" (404):** the model key doesn't match the served id. `curl <base_url>/models` shows the exact ids. Mind `--served-model-name`.
- **The answer stops mid-sentence (`finish_reason: length`):** `max_output_tokens` is too low, or long thinking used up the budget. Raise it, or lower the effort with `/reasoning`.
- **Repetition loops:** Odex breaks repeated identical tool calls and degenerate text with a nudge. Persistent loops usually mean the sampling is wrong. Keep the preset's values, which include `presence_penalty` where the vendor recommends it. Avoid `temperature = 0` for Qwen3 and GLM.
- **Very slow first token on every step:** prefix caching is off or ineffective. Check Doctor's `prefixCache`. Also make sure no proxy or middleware changes the prompt between requests.
- **`ValueError: … KV cache … max_model_len` at startup:** the window doesn't fit in VRAM. Lower `--max-model-len`, use `--kv-cache-dtype fp8`, raise `--gpu-memory-utilization`, or add GPUs (`-tp`).
- **Chat template file not found:** see [chat templates](#chat-templates).
- **Mistral tool-call ids:** with the HF tokenizer, Mistral templates need 9-character ids. Odex keeps the ids vLLM returns; don't put a proxy in between that rewrites them.
