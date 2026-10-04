# vLLM `serve` flags and API behaviors for self-hosted coding models

Reference for shipping model presets. Researched 2026-10-04.

- **Latest stable vLLM:** v0.30.0 (released 2026-09-22) [S24]. `docs.vllm.ai/en/latest` tracks `main`. The only registry differences between main and v0.30.0 are main-only `hf` (tool+reasoning), `plamo3` (tool+reasoning) and `granite_thinking_parser` (reasoning).
- **Recipes:** `docs.vllm.ai/projects/recipes/en/latest/` now just points to **recipes.vllm.ai**. The data lives in `github.com/vllm-project/recipes`, as `models/<org>/<model>.yaml` and `<Vendor>/*.md`. Snapshot used: commit `cbf76c0`, 2026-10-03 [S25].
- **Method:** I read the vLLM source at tag v0.30.0, and the protocol and serving files at older release tags to build the version history. I also read the recipe YAMLs and Hugging Face model cards plus `generation_config.json` (fetched raw).

**Confidence tags used below**

| Tag | Meaning |
|---|---|
| [src] | Read in the vLLM source code |
| [doc] | vLLM docs |
| [recipe] | vLLM recipes repo |
| [card] | HF model card or `generation_config.json` |
| [derived] | Inferred from a chat template or parser code, not stated by vLLM |
| **[UNVERIFIED]** | Could not confirm |

Source IDs (S1, S2, ...) are listed at the end.

---

## 0. Key takeaways for presets

1. **Pair the flags.** `--enable-auto-tool-choice` needs `--tool-call-parser <name>`. Without the parser, startup fails with `Error: --enable-auto-tool-choice requires --tool-call-parser`. A request with `tool_choice:"auto"` sent to a server without these flags gets HTTP 400 `"auto" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set` [src S31].
2. **Read the reasoning field defensively:**
   - v0.16.0 and later: `reasoning` only.
   - v0.11.1 to v0.15.x: both `reasoning` and `reasoning_content` (same value).
   - v0.11.0 and earlier: `reasoning_content` only.

   So read `delta.reasoning ?? delta.reasoning_content` [src S21].
3. **Structured outputs.** `response_format` (`json_schema` / `json_object`) works on every version checked. The `structured_outputs` extra-body field exists from v0.11.0. `guided_json` and the other `guided_*` fields were deprecated in v0.11.x and **removed in v0.12.0**. Since then they are silently ignored: HTTP 200 with unconstrained output and only a server-side warning [src S13, S21; doc S5].
4. **`/v1/models` includes `max_model_len`** (all versions checked, v0.10.0 to v0.30.0). `/tokenize`, `/detokenize`, `/health`, `/version` all exist. They sit at the **server root, not under `/v1`**, and are not covered by `--api-key` [src S14-S17].
5. **Don't set a reasoning parser on non-thinking-only models** (Qwen3-Coder, Qwen3-*-Instruct-2507, Kimi-K2-Instruct). In v0.30 the `qwen3`, `kimi_k2` and `glm45/47` parsers start in "reasoning" state unless the request turns thinking off. On those models the output would land in `reasoning` [src S26, S27].
6. **Sampling defaults come from the model.** vLLM applies the model's `generation_config.json` (`--generation-config auto`, the default). Only `temperature`, `top_p`, `top_k`, `min_p`, `repetition_penalty` and `max_new_tokens` are taken from it. **`presence_penalty` is never taken from the model config, so send it per request** [src S23].
7. **Don't copy model-card commands verbatim:**
   - `--enable-reasoning` (still in some Qwen cards) does not exist in v0.10.0 and later [src arg_utils].
   - `--rope-scaling` is not in v0.30.0's arg parser. Recipes now use `--hf-overrides '{"rope_parameters":{...}}'` [src, recipe].
8. **Bundled chat-template paths.** `--chat-template examples/...jinja` paths are relative to the vLLM repo. They exist in the Docker image under `/vllm-workspace/examples/` (the vLLM docs use this path for Apertus). A plain `pip install vllm` does **not** ship them, so the app must download the file from GitHub at the matching tag [doc S3; packaging **UNVERIFIED**].

---

## 1. Parser names: registry at v0.30.0, and the first release that has each

**Tool parsers in v0.30.0** [src S1]:
`apertus cohere_command3 cohere_command4 deepseek_v3 deepseek_v31 deepseek_v32 deepseek_v4 deepseek_v41 dots ernie45 functiongemma gemma4 gigachat3 glm45 glm47 granite granite-20b-fc granite4 hermes hunyuan_a13b hy_v3 hy_v4 inkling internlm jamba k2_horizon kimi_k2 kimi_k3 lfm2 ling3 llama3_json llama4_json llama4_pythonic longcat mimo minicpm5 minimax_m2 minimax_m3 mistral muse_glimmer olmo3 openai phi4_mini_json poolside_v1 pythonic qwen3_coder qwen3_xml seed_oss step3 step3p5 xlam`
(main also has `hf`, `plamo3`).

**Reasoning parsers in v0.30.0** [src S2]:
`cohere_command3 cohere_command4 deepseek_r1 deepseek_v3 deepseek_v4 deepseek_v41 ernie45 gemma4 glm45 glm47 granite holo2 hunyuan_a13b hy_v3 hy_v4 inkling k2_horizon kimi_k2 kimi_k3 ling3 mimo minimax_m2 minimax_m2_append_think minimax_m3 mistral muse_glimmer nemotron_v3 olmo3 openai_gptoss poolside_v1 qwen3 seed_oss step3 step3p5`
(main also has `granite_thinking_parser`, `hf`, `plamo3`).

**Aliases in v0.30.0** [src S1, S2]:
- `qwen3_coder` and `qwen3_xml` are the same class.
- `glm45` and `glm47` are the same class, for both tool and reasoning parsing.
- `llama3_json` and `llama4_json` are the same class.

In older releases the members of each pair were separate implementations.

**First release containing each parser name** (relevant ones only) [src]. This was built by scanning registries at every tag from v0.10.0 to v0.30.0; a parser may predate v0.10.0.

| Parser | Tool parser first in | Reasoning parser first in |
|---|---|---|
| `hermes`, `mistral`, `llama3_json`, `llama4_json`, `llama4_pythonic`, `pythonic`, `kimi_k2` (tool), `qwen3_coder`, `deepseek_v3` (tool) | <= v0.10.0 | n/a |
| `qwen3`, `deepseek_r1`, `mistral` (reasoning) | n/a | <= v0.10.0 |
| `glm45` (`glm4_moe` in v0.10.0) | v0.10.1 | v0.10.1 |
| `deepseek_v31`, `openai`, `seed_oss` (tool) | v0.10.2 | n/a |
| `openai_gptoss` | n/a | v0.10.2 |
| `qwen3_xml`, `longcat` | v0.11.0 | n/a |
| `seed_oss` (reasoning) | n/a | v0.11.0 |
| `minimax_m2` | v0.11.1 | v0.11.1 (also `minimax_m2_append_think`) |
| `deepseek_v3` (reasoning), `kimi_k2` (reasoning) | n/a | v0.11.1 |
| `deepseek_v32` | v0.13.0 | none (V3.2 uses `deepseek_v3`) |
| `glm47` | v0.14.0 | v0.24.0 |
| `deepseek_v4`, `mimo` | v0.20.0 | v0.20.0 |
| `poolside_v1` | v0.21.0 | v0.21.0 |
| `minimax_m3` | v0.24.0 | v0.24.0 |
| `kimi_k3` | v0.27.0 | v0.27.0 |
| `deepseek_v41`, `k2_horizon` | v0.30.0 | v0.30.0 |
| `minimax` (MiniMax-M1, legacy) | <= v0.10.0, removed after v0.23.0 | n/a |

**Tokenizer modes in v0.30.0:**
- Literal values: `auto, hf, slow, mistral, deepseek_v32, deepseek_v4, inkling, kimi_k3, cohere` [src S30].
- The docstring also lists `deepseek_v41`, which the DeepSeek-V4.1 recipe uses.
- `auto` uses `mistral_common` for Mistral models when it is available.
- `deepseek_v32` is present from at least v0.11.2.

---

## 2. Per-family presets

Columns:
- **Tool** = `--tool-call-parser` (always together with `--enable-auto-tool-choice`).
- **Reasoning** = `--reasoning-parser`.
- **Ctx** = native context length (`max_position_embeddings` from `config.json` unless noted).

Parallelism flags (`-tp`, `-dp`, `--enable-expert-parallel`) depend on hardware and are left out.

### 2.1 Qwen3-Coder (non-thinking only)

| HF id | Tool | Reasoning | Other flags | Ctx | Min vLLM | Sources |
|---|---|---|---|---|---|---|
| `Qwen/Qwen3-Coder-30B-A3B-Instruct` (also `-FP8`) | `qwen3_coder` (or `qwen3_xml` on v0.11.0+) | none | none required | 262,144 | 0.10.0 | [recipe][doc S3][card] |
| `Qwen/Qwen3-Coder-480B-A35B-Instruct` (also `-FP8`) | `qwen3_coder` | none | none required (the AMD FP8 recipe adds `--trust-remote-code`) | 262,144 | 0.10.0 | [recipe][card] |
| `Qwen/Qwen3-Coder-Next` (exists; arch `qwen3_next`) | `qwen3_coder` | none | none | 262,144 | **0.15.0** | [card] |

**Notes**
- The vLLM docs name `qwen3_xml`; recipes and the Qwen cards use `qwen3_coder`. In v0.30.0 they are identical [src S1]. Prefer `qwen3_coder` for compatibility back to v0.10.0.
- Suggested `--max-model-len`: Qwen suggests dropping to 32,768 if you run out of memory [card]. The vLLM recipe uses 32000 (BF16) or 131072 (FP8) on 8xH200 [recipe].
- **Thinking:** none. The cards say these models "support only non-thinking mode", and `enable_thinking` is not needed [card].
- With structured outputs plus a reasoning parser on Qwen3-Coder, vLLM documents `--structured-outputs-config.enable_in_reasoning=True`. This is irrelevant unless you set a reasoning parser, and you shouldn't [doc S5].

**Sampling** [card + generation_config]

| Model | temperature | top_p | top_k | min_p | repetition_penalty | presence_penalty |
|---|---|---|---|---|---|---|
| Qwen3-Coder-30B / 480B | 0.7 | 0.8 | 20 | n/a | 1.05 | n/a |
| Qwen3-Coder-Next | 1.0 | 0.95 | 40 | n/a | n/a | n/a |

### 2.2 Qwen3 text models: hybrid Qwen3, 2507 Instruct/Thinking, Qwen3-Next, Qwen3.5/3.6

| HF id (examples) | Tool | Reasoning | Other flags | Ctx | Min vLLM | Sources |
|---|---|---|---|---|---|---|
| `Qwen/Qwen3-8B`, `-14B`, `-32B`, `Qwen3-30B-A3B`, `Qwen3-235B-A22B` (hybrid, original) | `hermes` | `qwen3` | Long context (optional): `--hf-overrides '{"max_position_embeddings":131072,"rope_parameters":{"rope_type":"yarn","factor":4.0,"original_max_position_embeddings":32768}}' --max-model-len 131072` | 32,768 native (config 40,960); 131,072 with YaRN | 0.8.5 | [recipe][card] |
| `Qwen/Qwen3-30B-A3B-Instruct-2507`, `Qwen/Qwen3-235B-A22B-Instruct-2507` | `hermes` | **none** (non-thinking only) | The 235B recipe adds `--trust-remote-code` | 262,144 | 0.10.0 | [recipe][card] |
| `Qwen/Qwen3-30B-A3B-Thinking-2507`, `Qwen/Qwen3-235B-A22B-Thinking-2507` | `hermes` [derived from template] | `deepseek_r1` (Qwen card; works on all versions) **or** `qwen3` (v0.17.0+ only) | none | 262,144; Qwen strongly advises > 131,072 | 0.10.0 | [card][src S26] |
| `Qwen/Qwen3-Next-80B-A3B-Instruct` | `hermes` | none | Optional MTP: `--speculative-config '{"method":"qwen3_next_mtp","num_speculative_tokens":2}'` | 262,144 (1,010,000 with YaRN) | card 0.10.2; recipe 0.17.0 | [recipe][card] |
| `Qwen/Qwen3-Next-80B-A3B-Thinking` | `hermes` [derived] | `deepseek_r1` (card) or `qwen3` (v0.17.0+) | none | 262,144; > 131,072 advised | 0.10.2 | [card] |
| `Qwen/Qwen3.5-27B`, `-35B-A3B`, `-122B-A10B`, `-397B-A17B`, small sizes 0.8B to 9B (multimodal; exists) | `qwen3_coder` | `qwen3` | `--trust-remote-code` (recipe base args). Optional `--language-model-only` to skip the vision encoder | 262,144; keep >= 128K for thinking | **0.17.0** | [recipe][card] |
| `Qwen/Qwen3.6-27B`, `Qwen/Qwen3.6-35B-A3B` (exist) | `qwen3_coder` | `qwen3` | `--trust-remote-code`; optional `--language-model-only` | 262,144 | 0.17.0 (card recommends >= 0.19.0) | [recipe][card] |

**Why the `qwen3` reasoning parser needs v0.17.0 for some models**
- Up to v0.16.x, `qwen3` only split reasoning when **both** `<think>` and `</think>` appeared in the output.
- Thinking-2507 and Qwen3.5+ templates put `<think>` in the prompt, so only `</think>` is generated. On those versions the reasoning ends up in `content`.
- From v0.17.0 the parser handles both styles [src S26].
- On older vLLM, use `deepseek_r1` for these models.

**Thinking toggle**
- Hybrid Qwen3, Qwen3.5 and Qwen3.6 think by default.
- Per request: `"chat_template_kwargs": {"enable_thinking": false}`.
- Server-wide default: `--default-chat-template-kwargs '{"enable_thinking": false}'` [doc S4][recipe].
- Since v0.30, `reasoning_effort: "none"` auto-injects `enable_thinking=false`, and `"low"|"medium"|"high"` inject `true` [doc S4].
- Qwen3.6 also takes `"chat_template_kwargs": {"preserve_thinking": true}` to keep thinking from earlier turns [card].
- The 2507 Instruct and Thinking models are fixed-mode and ignore the switch [card].

**Sampling** [card / generation_config]

| Model / mode | temperature | top_p | top_k | min_p | presence_penalty | repetition_penalty |
|---|---|---|---|---|---|---|
| Hybrid Qwen3, thinking | 0.6 | 0.95 | 20 | 0 | n/a | n/a |
| Hybrid Qwen3, non-thinking | 0.7 | 0.8 | 20 | 0 | n/a | n/a |
| 2507 Instruct, Next Instruct | 0.7 | 0.8 | 20 | 0 | 0 to 2 against repetition (optional) | n/a |
| 2507 Thinking, Next Thinking | 0.6 | 0.95 | 20 | 0 | 0 to 2 (optional) | n/a |
| Qwen3.5, thinking, general | 1.0 | 0.95 | 20 | 0.0 | 1.5 | 1.0 |
| Qwen3.5, thinking, precise coding | 0.6 | 0.95 | 20 | 0.0 | 0.0 | 1.0 |
| Qwen3.5, non-thinking, general | 0.7 | 0.8 | 20 | 0.0 | 1.5 | 1.0 |
| Qwen3.6, thinking, general | 1.0 | 0.95 | 20 | 0.0 | 1.5 (35B-A3B) / 0.0 (27B) | 1.0 |
| Qwen3.6, thinking, precise coding | 0.6 | 0.95 | 20 | 0.0 | 0.0 | 1.0 |
| Qwen3.6, non-thinking | 0.7 | 0.8 | 20 | 0.0 | 1.5 | 1.0 |

`generation_config.json` temperature defaults differ by size: Qwen3.5-35B-A3B 1.0, Qwen3.5-27B and 397B 0.6, Qwen3.6 1.0.

### 2.3 Qwen3-VL (vision)

| HF id | Tool | Reasoning | Other flags | Ctx | Min vLLM | Sources |
|---|---|---|---|---|---|---|
| `Qwen/Qwen3-VL-30B-A3B-Instruct`, `Qwen/Qwen3-VL-235B-A22B-Instruct` (also `-FP8`) | `hermes` [derived: the chat template emits `<tool_call>{"name":..,"arguments":..}</tool_call>`] | none | Optional `--limit-mm-per-prompt.video 0` (image-only, saves memory), `--mm-encoder-tp-mode data`, `--async-scheduling`. Optional `--language-model-only` for text-only use | 262,144; recipe suggests `--max-model-len 128000` | 0.11.0 | [recipe][derived] |
| `Qwen/Qwen3-VL-30B-A3B-Thinking`, `-235B-A22B-Thinking` | `hermes` [derived] | `qwen3` (v0.17.0+) or `deepseek_r1` [derived] | as above | 262,144 | 0.11.0 | [derived] |

- The vLLM recipe and docs do **not** list tool or reasoning parsers for Qwen3-VL. The parser choices above come from the HF chat templates. **[UNVERIFIED end-to-end]**
- **Sampling**, from the Qwen3-VL GitHub README:

  | Variant | temperature | top_p | top_k | repetition_penalty | presence_penalty |
  |---|---|---|---|---|---|
  | Instruct | 0.7 | 0.8 | 20 | 1.0 | 1.5 |
  | Thinking | 0.6 | 0.95 | 20 | 1.0 | 0.0 |

  The Thinking `generation_config.json` says temperature 0.8.

### 2.4 GLM (Z.ai)

| HF id | Tool | Reasoning | Other flags | Ctx | Min vLLM | Sources |
|---|---|---|---|---|---|---|
| `zai-org/GLM-4.5`, `zai-org/GLM-4.5-Air` (also `-FP8`) | `glm45` | `glm45` | `--trust-remote-code` (recipe base args) | 131,072 | parsers v0.10.1; recipe says 0.11.0 | [doc S3, S4][recipe][card] |
| `zai-org/GLM-4.6` | `glm45` | `glm45` | `--trust-remote-code` | 202,752 | 0.11.0 | [doc S3][recipe] |
| `zai-org/GLM-4.7` (`-FP8`), `zai-org/GLM-4.7-Flash` | **`glm47`** | `glm45` (card; works from v0.10.1) or `glm47` (v0.24.0+) | `--trust-remote-code`. Optional MTP: `--speculative-config.method mtp --speculative-config.num_speculative_tokens 1` | 202,752 | `glm47` tool parser v0.14.0; recipe says 0.24.0 | [doc S3][card][recipe] |
| `zai-org/GLM-4.5V` (vision) | `glm45` | `glm45` | `--trust-remote-code --allowed-local-media-path / --mm-encoder-tp-mode data --mm-processor-cache-type shm` (recipe). Card adds `--media-io-kwargs '{"video": {"num_frames": -1}}'` | 65,536 | card 0.10.2; recipe 0.12.0 | [recipe][card] |

**Thinking toggle**
- On by default. Turn it off with `"chat_template_kwargs": {"enable_thinking": false}` [card].
- The vLLM parser reads `thinking` or `enable_thinking` from the request's `chat_template_kwargs` [src S27].
- GLM-4.7 "Preserved Thinking": `{"enable_thinking": true, "clear_thinking": false}`. The card says "only sglang support", so vLLM behaviour is **[UNVERIFIED]**.

**Sampling** [card]

| Model / task | temperature | top_p | top_k |
|---|---|---|---|
| GLM-4.6, general | 1.0 | n/a | n/a |
| GLM-4.6, code | 1.0 | 0.95 | 40 |
| GLM-4.7 / 4.7-Flash, default | 1.0 | 0.95 | n/a |
| GLM-4.7, SWE-bench / Terminal-Bench | 0.7 | 1.0 | n/a |
| GLM-4.5 / 4.5-Air | **[UNVERIFIED]** | | |

- GLM-4.5 and 4.5-Air: the card has no explicit recommendation, and `generation_config.json` has no sampling keys.
- **Warning for GLM-4.5V:** its `generation_config.json` sets `top_k: 1, top_p: 0.0001`, which is effectively greedy. vLLM applies that as the default unless the request overrides it or the server runs with `--generation-config vllm`.

### 2.5 gpt-oss (OpenAI)

| HF id | Tool | Reasoning | Other flags | Ctx | Min vLLM | Sources |
|---|---|---|---|---|---|---|
| `openai/gpt-oss-20b`, `openai/gpt-oss-120b` | `openai` | `openai_gptoss` (used in vLLM's own BFCL benchmark example; the recipe only sets the tool parser) | none. No chat template needed: Harmony rendering is automatic for `model_type == gpt_oss` | 131,072 | 0.10.1 (recipe); `openai` tool parser registered from v0.10.2 | [recipe][doc docs/benchmarking/cli.md][src S29] |

- When any tool or reasoning parser is set, vLLM uses its `HarmonyParser`, which routes the `analysis` channel to `reasoning` [src S29].
- With **no** parser flags at all, `ParserManager.get_parser` returns `None`. Output separation is then **[UNVERIFIED]**, so always set both flags.
- **Reasoning effort:** use the request field `reasoning_effort: "low"|"medium"|"high"` [card, recipe].
- The full built-in tool set (browser, python, MCP) is only on `/v1/responses` with `--tool-server ...`. Chat Completions supports user-defined functions [recipe].
- **Sampling:** temperature 1.0, top_p 1.0 (gpt-oss GitHub README, "Recommended Sampling Parameters").

### 2.6 DeepSeek V3.1 / V3.2

| HF id | Tool | Reasoning | Other flags | Ctx | Min vLLM | Sources |
|---|---|---|---|---|---|---|
| `deepseek-ai/DeepSeek-V3.1`, `deepseek-ai/DeepSeek-V3.1-Terminus` | `deepseek_v31` | `deepseek_v3` | `--chat-template examples/tool_chat_template_deepseekv31.jinja`; `--trust-remote-code` (recipe base args) | config 163,840 (card says 128K) | `deepseek_v31` v0.10.2; reasoning v0.11.1; recipe 0.12.0 | [doc S3, S4][recipe][card] |
| `deepseek-ai/DeepSeek-V3.2` (also `-Exp`) | `deepseek_v32` | `deepseek_v3` | **`--tokenizer-mode deepseek_v32`** (needed for the new chat format); `--trust-remote-code` | 163,840 | tool parser v0.13.0; recipe says 0.18.0 (V3.2) / 0.12.0 (Exp) | [recipe][card] |

**Thinking toggle**
- Off by default. Enable with `"chat_template_kwargs": {"thinking": true}`. `enable_thinking` is also accepted by the `deepseek_v3` parser [doc S4][src S27].
- V3.1: tool calling works **only in non-thinking mode** [doc S4][card].
- V3.2: tool calls inside thinking mode are supported [recipe].

**Sampling** [card / generation_config]

| Model | temperature | top_p |
|---|---|---|
| V3.1 / Terminus / V3.2-Exp | 0.6 | 0.95 |
| V3.2 (card recommends) | 1.0 | 0.95 |

### 2.7 Mistral / Devstral

| HF id | Tool | Reasoning | Other flags | Ctx | Sources |
|---|---|---|---|---|---|
| `mistralai/Devstral-Small-2507` (also 2505) | `mistral` | none | `--tokenizer_mode mistral --config_format mistral --load_format mistral` | 131,072 | [card] |
| `mistralai/Devstral-Small-2-24B-Instruct-2512`, `mistralai/Devstral-2-123B-Instruct-2512` | `mistral` | none | Card adds `--max-model-len 262144` for Small 2. Needs `mistral_common >= 1.8.6`. Mistral-format flags are auto-detected (`tokenizer_mode auto` picks `mistral_common`) | 262,144 | [card][src S30] |
| `mistralai/Ministral-3-14B-Instruct-2512`, `mistralai/Mistral-Large-3-675B-Instruct-2512` | `mistral` | none | `--tokenizer_mode mistral --config_format mistral --load_format mistral` | 262,144 / 294,912 | [recipe] |
| `mistralai/Ministral-3-8B-Reasoning-2512`, `mistralai/Mistral-Medium-3.5-128B`, `mistralai/Mistral-Small-4-119B-2603` | `mistral` | `mistral` | Mistral-format flags (Small-4 recipe: `--max-model-len 262144`) | 262,144 | [recipe] |

**Notes**
- Transformers-format alternative: `--tokenizer_mode hf --config_format hf --load_format hf --chat-template examples/tool_chat_template_mistral_parallel.jinja` [doc S3].
- With the HF tokenizer, Mistral templates need **9-character tool-call IDs**. When sending tool results back, keep the `tool_call_id` vLLM returned [doc S3].
- Mistral-Small-4 turns reasoning on per request with `reasoning_effort` set to `"none"` or `"high"` only; other values are rejected by the template [recipe].

**Sampling**
- Devstral (all variants): temperature 0.15 [card; Devstral 2 `generation_config.json` = 0.15].
- Mistral-Small-4: temperature 0.7 at `reasoning_effort="high"`, and 0.0 to 0.7 at `"none"` [recipe].

### 2.8 Kimi K2 (Moonshot)

| HF id | Tool | Reasoning | Other flags | Ctx | Min vLLM (recipe) | Sources |
|---|---|---|---|---|---|---|
| `moonshotai/Kimi-K2-Instruct`, `moonshotai/Kimi-K2-Instruct-0905` | `kimi_k2` | **none** | `--trust-remote-code` (recipe also passes `--tokenizer-mode auto`) | 131,072 / 262,144 (0905) | 0.12.0 | [doc S3][recipe][card] |
| `moonshotai/Kimi-K2-Thinking` | `kimi_k2` | `kimi_k2` | `--trust-remote-code` (native INT4) | 262,144 | 0.12.0 (reasoning parser v0.11.1) | [recipe][card][doc S6] |
| `moonshotai/Kimi-K2.5`, `Kimi-K2.6` (multimodal), `Kimi-K2.7-Code` | `kimi_k2` | `kimi_k2` | `--trust-remote-code`; optional `--language-model-only` | 262,144 | 0.19.1 / 0.25.0 / 0.19.1 | [recipe] |

**Notes**
- Do **not** set `--reasoning-parser kimi_k2` on K2-Instruct. In v0.30 the parser assumes thinking unless `chat_template_kwargs` has `thinking` / `enable_thinking` = false [src S27].
- Tool-call IDs look like `functions.<name>:<idx>` [src kimi_k2 parser].
- K2-Thinking is an interleaved-thinking model. Send each assistant turn's `reasoning` back in the history (see section 3.4) [doc S6].
- K2.5 and K2.6 have instant and thinking modes, toggled with `chat_template_kwargs` `thinking` (the parser reads it). K2.7-Code is thinking-only [recipe].

**Sampling** [card]

| Model | temperature | top_p |
|---|---|---|
| K2-Instruct / 0905 | 0.6 | n/a |
| K2-Thinking | 1.0 | n/a |
| K2.7-Code | 1.0 | 0.95 [recipe] |

### 2.9 MiniMax M2 family

| HF id | Tool | Reasoning | Other flags | Ctx | Min vLLM | Sources |
|---|---|---|---|---|---|---|
| `MiniMaxAI/MiniMax-M2`, `-M2.1`, `-M2.5`, `-M2.7` | `minimax_m2` | `minimax_m2` (recipes, interleaved-thinking doc) **or** `minimax_m2_append_think` (vLLM reasoning doc table, MiniMax's own vLLM guide) | `--trust-remote-code` | 196,608 | 0.11.1 (M2/M2.1), 0.20.x (M2.5/M2.7) | [recipe][doc S4, S6][card] |

**The two reasoning parsers** [src S28]
- `minimax_m2` splits `<think>` text into `reasoning`.
- `minimax_m2_append_think` does **not** split. It returns everything in `content`, with a `<think>` prefix added.
- MiniMax requires the `<think>...</think>` text to be passed back unchanged in history.
- Pick `append_think` if the client round-trips raw `content`. Pick `minimax_m2` if the client round-trips the `reasoning` field.

**Sampling:** temperature 1.0, top_p 0.95, top_k 40 [card + generation_config].

### 2.10 Llama 3.3 / Llama 4 (Meta)

| HF id | Tool | Reasoning | Other flags | Ctx | Sources |
|---|---|---|---|---|---|
| `meta-llama/Llama-3.3-70B-Instruct` | `llama3_json` | none | `--chat-template examples/tool_chat_template_llama3.1_json.jinja` | 131,072 | [doc S3]; 3.3 itself is [derived] (docs list 3.1, 3.2, 4) |
| `meta-llama/Llama-4-Scout-17B-16E-Instruct`, `meta-llama/Llama-4-Maverick-17B-128E-Instruct(-FP8)` | `llama4_pythonic` (recommended) | none | `--chat-template examples/tool_chat_template_llama4_pythonic.jinja`. Alternative: `llama4_json` with `examples/tool_chat_template_llama4_json.jinja` | Scout 10,485,760 / Maverick 1,048,576. Set `--max-model-len` to fit memory | [doc S3] |

- Llama 3.x does not support parallel tool calls; Llama 4 does [doc S3].
- The vLLM recipes for Llama 3.3 and 4 cover only throughput tuning, with no tool flags [recipe].
- **Sampling:** temperature 0.6, top_p 0.9. This comes from `generation_config.json` on the `unsloth/` mirrors, because the Meta repos are gated. **[UNVERIFIED against Meta originals]**

### 2.11 Seed-OSS (ByteDance)

| HF id | Tool | Reasoning | Other flags | Ctx | Min vLLM | Sources |
|---|---|---|---|---|---|---|
| `ByteDance-Seed/Seed-OSS-36B-Instruct` | `seed_oss` | `seed_oss` [registry v0.11.0+; not in recipe or card] | `--trust-remote-code` (card, AMD recipe). The card also passes `--chat-template <repo>/chat_template.jinja`, which is the repo's own template and probably redundant **[UNVERIFIED]** | 524,288; recipe suggests `--max-model-len 65536` | 0.11.0 | [recipe][card][src S2] |

- **Thinking budget:** `"chat_template_kwargs": {"thinking_budget": N}`. `-1` (default) means unlimited, `0` means answer directly. Use multiples of 512 [recipe][card].
- **Sampling:** temperature 1.1, top_p 0.95 [card + generation_config].

### 2.12 Other recent coding-relevant models in the recipes repo

All taken from recipe YAMLs [recipe S25]. Not individually checked against HF cards.

| HF id | Tool | Reasoning | Notable flags | Ctx | Min vLLM | Sampling / toggles (recipe guide) |
|---|---|---|---|---|---|---|
| `Qwen/Qwen3.8-27B` | `qwen3_xml` | `qwen3` | optional `--language-model-only` | 262,144 | 0.17.0 | temp 1.0, top_p 0.95, top_k 20. `chat_template_kwargs` `{"enable_thinking": false}` or `{"reasoning_effort": "low"\|"medium"\|"xhigh"}` |
| `moonshotai/Kimi-K2.7-Code` | `kimi_k2` | `kimi_k2` | `--trust-remote-code` | 262,144 | 0.19.1 | thinking only; temp 1.0, top_p 0.95 |
| `zai-org/GLM-5`, `GLM-5.1` | `glm47` | `glm47` | `--trust-remote-code --chat-template-content-format=string` | 202,752 | 0.24.0 | temp 1; `enable_thinking` toggle |
| `zai-org/GLM-5.3` | `glm47` | `glm47` | `--kv-cache-dtype fp8_e4m3` | 1,048,576 | 0.29.0 | `reasoning_effort` `low` / `high` / `max` (default max) |
| `MiniMaxAI/MiniMax-M2.5`, `-M2.7` | `minimax_m2` | `minimax_m2` | `--trust-remote-code` | 196,608 | 0.20.x | see 2.9 |
| `MiniMaxAI/MiniMax-M3` | `minimax_m3` | `minimax_m3` | `--block-size 128`; optional `--default-chat-template-kwargs '{"thinking_mode": "enabled"}'` | 1,048,576 | 0.24.0 (nightly) | n/a |
| `deepseek-ai/DeepSeek-V4-Flash`, `-V4-Pro` | `deepseek_v4` | `deepseek_v4` | `--tokenizer-mode deepseek_v4 --trust-remote-code --kv-cache-dtype fp8 --block-size 256` | 1,048,576 | 0.20.0 | temp 1.0, top_p 1.0 (0.95 agentic). `chat_template_kwargs` `{"thinking": true, "reasoning_effort": "high"\|"max"}` |
| `JetBrains/Mellum2-12B-A2.5B-Instruct` / `-Thinking` | `hermes` | none / `qwen3` | `--trust-remote-code` (Instruct) | 131,072 | 0.23.0 | temp 0.6, top_p 0.95, top_k 20 |
| `poolside/Laguna-XS.2`, `Laguna-XS-2.1`, `Laguna-S-2.1`, `Laguna-M.1` | `poolside_v1` | `poolside_v1` | `--trust-remote-code` (XS/S) | 262,144 | 0.21.0 / 0.25.0 | `enable_thinking` toggle; XS-2.1: temp 0.7, top_p 1.0, top_k 20 |
| `mistralai/Mistral-Small-4-119B-2603` | `mistral` | `mistral` | `--max-model-len 262144` | 262,144 | 0.20.0 | see 2.7 |

**Rust frontend:** many recipes set `default_frontend: rust`. The recipe site then sets `VLLM_USE_RUST_FRONTEND=1`, which runs the separate `vllm-rs` HTTP binary instead of the Python API server. It is off by default in vLLM [src S33]. Whether the Rust frontend has the same endpoint and field coverage as described in section 3 is **[UNVERIFIED]**. Section 3 describes the Python server.

---

## 3. API behaviors (Python OpenAI-compatible server)

### 3.1 `GET /v1/models`

- **Yes, `max_model_len` is included.** Base-model cards are built with `max_model_len=self.model_config.max_model_len`, which reflects `--max-model-len` [src S13, S14].
- The field is present in `ModelCard` at v0.10.0 and still at v0.30.0 [src S21].
- The response is `JSONResponse(content=models_.model_dump())`, so null fields are emitted.
- LoRA adapter entries have `max_model_len: null` and `parent: <base model>`.

```json
{"object":"list","data":[{"id":"<served-model-name>","object":"model","created":1759600000,
  "owned_by":"vllm","root":"<HF id or local path>","parent":null,"max_model_len":32768,
  "permission":[{"id":"modelperm-...","object":"model_permission", "...":"..."}]}]}
```

`id` is `--served-model-name`, or the model path if that flag is unset. `root` is the real model path.

### 3.2 Utility endpoints

All present at v0.10.0 and v0.30.0 [src S15, S16; doc S7].

| Endpoint | Behavior |
|---|---|
| `GET /health` | 200 with empty body when healthy, 503 when the engine is dead. Render-only servers always return 200 |
| `GET /version` | `{"version": "<vllm version>"}` |
| `POST /tokenize` | Body is either `{model?, prompt, add_special_tokens?, return_token_strs?}` or the chat form `{model?, messages, add_generation_prompt?, continue_final_message?, chat_template?, chat_template_kwargs?, tools?, return_token_strs?}`. Response: `{count, max_model_len, tokens, token_strs?}`. Note the chat form accepts `tools`, so the count includes the tool prompt |
| `POST /detokenize` | `{model?, tokens}` returns `{prompt}` |
| `GET /tokenizer_info` | Registered only with `--enable-tokenizer-info-endpoint` |
| `GET /load`, `GET|POST /ping`, `GET /metrics` | Load metrics, SageMaker ping, Prometheus |
| `POST /v1/messages`, `/v1/messages/count_tokens` | Anthropic Messages API (v0.30) [doc S7] |

**Auth:** `--api-key` guards only paths starting with `/v1`, `/v2`, `/inference` or `/cohere`. That leaves `/health`, `/version`, `/tokenize` and `/metrics` **unauthenticated**. A failure returns HTTP 401 with body `{"error":"Unauthorized"}` (not the usual error envelope) [src S17].

### 3.3 Structured outputs: parameter names by version

| vLLM version | Accepted | Notes |
|---|---|---|
| <= v0.10.2 | `response_format` (`json_schema`, `json_object`, `text`) and extra-body `guided_json`, `guided_regex`, `guided_choice`, `guided_grammar`, `guided_decoding_backend` | [src S21] |
| v0.11.0 to v0.11.2 | `response_format`; new extra-body `structured_outputs: {json\|regex\|choice\|grammar\|json_object\|structural_tag\|...}`; `guided_*` still accepted (deprecated) | [src S21] |
| **v0.12.0 and later** (incl. v0.30.0) | `response_format` and `structured_outputs` only. **`guided_*` removed** | Unknown fields are allowed (`extra="allow"`), so old fields return 200 with **unconstrained** output. v0.30 logs a warning naming the removed fields [src S13][doc S5] |

- `structured_outputs` fields in v0.30: `json, regex, choice, grammar, json_object, disable_any_whitespace, disable_additional_properties, whitespace_pattern, structural_tag` [src sampling_params.py].
- v0.30 `response_format.type` values: `"text" | "json_object" | "json_schema"`, plus structural-tag formats [src].
- **Portable choice:** `response_format: {"type":"json_schema","json_schema":{"name":..,"schema":{..}}}`.
- Backend selection is server-side: `--structured-outputs-config.backend` (default `auto`; xgrammar or guidance) [doc S5].
- With a reasoning parser set, the constraint applies to the post-reasoning content [doc S5].

### 3.4 Reasoning text: field names by version

**Response field**

| vLLM version | Non-streaming `message` / streaming `delta` |
|---|---|
| <= v0.11.0 | `reasoning_content` only |
| v0.11.1 to v0.15.x | **both** `reasoning` and `reasoning_content`. A validator copies `reasoning` into `reasoning_content`; marked deprecated |
| **v0.16.0 and later** (incl. v0.30.0) | `reasoning` only |

[src S21: protocol.py at v0.11.0, v0.11.1, v0.15.0, v0.15.1, v0.16.0, v0.17.0; doc S4 warns "`reasoning` used to be called `reasoning_content`"]

**Request side (history)**
- v0.30 accepts assistant messages with `reasoning` or the legacy `reasoning_content`; the latter is renamed on input [src S9].
- vLLM passes the value to the chat template as both `reasoning` and `reasoning_content`, for interleaved thinking (Kimi-K2-Thinking, MiniMax-M2) [src S32][doc S6].

**Other knobs** [doc S4][src S9]

| Knob | Effect | Version |
|---|---|---|
| `include_reasoning: false` | Reasoning is still generated but stripped from the response | — |
| `reasoning_effort` | Accepts `none, minimal, low, medium, high, xhigh, max`. Auto-injects `enable_thinking` | v0.30 |
| `thinking_token_budget` | Per-request reasoning cap. The end string is configured with `--reasoning-config '{"reasoning_start_str":..,"reasoning_end_str":..}'` | — |
| `--default-chat-template-kwargs '{...}'` | Server-wide default; request-level `chat_template_kwargs` win | — |

**Usage accounting:** with a reasoning parser set, v0.30 reports `usage.completion_tokens_details.reasoning_tokens` [src S10].

### 3.5 Streaming tool-call deltas

Per v0.30 source [src S10, S11, S12]:

- **First chunk:** `delta: {"role":"assistant","content":""}`.
- **Serialization:** chunks are serialized with `exclude_unset=True`, so absent keys are omitted, not `null`. An empty `tool_calls` list is dropped.
- **First delta of each tool call:** `{"index":i,"id":"chatcmpl-tool-<uuid>","type":"function","function":{"name":"<fn>"}}`. The `arguments` key may be **missing** here.
- **Later deltas:** `{"index":i,"function":{"arguments":"<json fragment>"}}`. Concatenate the fragments per `index`.
- **Short calls:** if a call finishes before its name was streamed, a single delta carries `name` and the full `arguments`. Deltas for the same index within a chunk are coalesced.
- **ID formats:** the default is `chatcmpl-tool-<uuid>`. Kimi uses `functions.<name>:<idx>`. Mistral uses 9-character IDs.
- **`finish_reason`:** `"tool_calls"` when tools were streamed under `auto` / `required`. Named-function `tool_choice` gives `"stop"` (OpenAI semantics).
- **End of stream:** `data: [DONE]`. A mid-stream error is sent as a `data: {"error":{...}}` event, then `[DONE]`.
- **Usage:** `stream_options.include_usage` adds a final usage-only chunk.

### 3.6 Parallel tool calls

- `parallel_tool_calls` is a chat request field, defaulting to `true` in v0.11.1 and later. In v0.11.0 and earlier it defaulted to `false` and was ignored, per a code comment saying "the model determines the behavior" [src S21].
- **v0.12.0 and later:** `parallel_tool_calls: false` is enforced **after generation** by keeping only tool call index 0. This applies to non-streaming responses and streaming deltas [src S22].
- `true` lets multiple calls through, but whether they appear depends on the model [doc S8].
- Per-model notes [doc S3]:

  | Parallel calls | Models / parsers |
  |---|---|
  | Supported | Llama 4, Hermes, Granite, xLAM, pythonic |
  | Not supported | Llama 3.x |
  | Weak | Mistral 7B |

### 3.7 `tool_choice` and strictness

[doc S3][src S9]

- **Accepted values:** `none` (default when no tools), `auto`, `required` (v0.8.3+), or a named function `{"type":"function","function":{"name":..}}`.
- **Named and `required`:** always schema-constrained via structured outputs.
- **`auto`:** constrained only if at least one tool sets `strict: true` and `VLLM_ENFORCE_STRICT_TOOL_CALLING=true` (the default). Otherwise calls are parsed from free text.
- **`none`:** tools stay in the prompt unless the server runs with `--exclude-tools-when-tool-choice-none`.

### 3.8 Error envelope and context-overflow messages

**Envelope** [src S13, S18, S21]
- **v0.10.1 and later** (incl. v0.30):
  ```json
  {"error":{"message":"...","type":"BadRequestError","param":"<str|null>","code":400}}
  ```
  The HTTP status equals `error.code`.
- **v0.10.0:** flat `{"object":"error","message":..,"type":..,"param":..,"code":..}`.
- v0.30 strips file paths and tracebacks from messages (`sanitize_message`).
- Validation errors from v0.14 onward append ` (parameter=<p>, value=<v>)` to the message (`VLLMValidationError.__str__`). `param` is also set.

**Context-overflow message text by version** (all HTTP 400, `type: "BadRequestError"`)

| Version | Message template |
|---|---|
| v0.10.2 to v0.17.x, prompt alone too long | `This model's maximum context length is {N} tokens. However, your request has {T} input tokens. Please reduce the length of the input messages.` [src S20] |
| v0.10.2 to v0.17.x, prompt + max_tokens too long | `'max_tokens' or 'max_completion_tokens' is too large: {M}. This model's maximum context length is {N} tokens and your request has {T} input tokens ({M} > {N} - {T}).` [src S20] |
| v0.18.0 to v0.30.0, token check | `This model's maximum context length is {N} tokens. However, you requested {M} output tokens and your prompt contains [at least ]{T} input tokens, for a total of [at least ]{T+M} tokens. Please reduce the length of the input prompt or the number of requested output tokens. (parameter=input_tokens, value={T})` [src S19] |
| v0.18.0 to v0.30.0, character pre-check (very long text) | `This model's maximum context length is {N} tokens. However, you requested {M} output tokens and your prompt contains {C} characters (more than {X} characters, which is the upper bound for {I} input tokens). Please reduce the length of the input prompt or the number of requested output tokens. (parameter=input_text, value={C})` [src S19] |
| v0.18.0 to v0.30.0, max_tokens alone > max_model_len | `max_tokens={M} cannot be greater than max_model_len=max_total_tokens={N}. Please request fewer output tokens. (parameter=max_tokens, value={M})`. The odd `max_total_tokens=` comes from an f-string `{x=}` in the source; `max_completion_tokens` is named instead if that was sent [src S19] |

Additional rules:
- In v0.18+, when `max_tokens` is omitted, the output budget is treated as 0. The check is then prompt <= `max_model_len`.
- **Detection regex for all versions:** `maximum context length is (\d+) tokens`. Also match `cannot be greater than max_model_len` (v0.18+) and `'max_tokens' or 'max_completion_tokens' is too large` (<= v0.17).
- Pydantic request-schema errors also return 400 in the same envelope (v0.30 `RequestValidationError` handler).

### 3.9 Default sampling and generation config

- `--generation-config auto` (the default) loads the model's `generation_config.json`. Of its keys, only `repetition_penalty, temperature, top_k, top_p, min_p, max_new_tokens` override vLLM defaults; `max_new_tokens` becomes the server-wide `max_tokens` cap [src S23].
- `--generation-config vllm` ignores the model file.
- `--override-generation-config '{"temperature":0.7}'` merges on top, but is limited to the same six keys. Because of this, **`presence_penalty` must be sent per request.**
- Request values always win.
- vLLM's own defaults if nothing is set: temperature 1.0, repetition_penalty 1.0 [src S9].

---

## 4. Not verified or uncertain

- Qwen3-VL tool and reasoning parsers (`hermes`, `qwen3` / `deepseek_r1`) are derived from the chat templates. vLLM docs and recipes don't state them.
- Seed-OSS `--reasoning-parser seed_oss`: the parser is registered, but the recipe and card don't use it.
- gpt-oss output separation when **no** parser flag is set.
- GLM-4.7 `clear_thinking` preserved-thinking behaviour on vLLM (the card says SGLang only).
- GLM-4.5 / 4.5-Air recommended sampling (none published in the card).
- Llama 3.3 / 4 sampling values (from the unsloth mirror; Meta repos are gated). Llama 3.3 is not named in the vLLM tool docs; it shares the 3.1 JSON format.
- Whether the pip wheel ships `examples/*.jinja`. Assume it doesn't; the Docker image has `/vllm-workspace/examples/`.
- Rust frontend (`VLLM_USE_RUST_FRONTEND=1`) API parity with the Python server.
- Min-vLLM values marked "recipe" are what the recipe YAML states. They are sometimes higher than the version where the parser first appears, likely for kernels or fixes.
- Exact behaviour of each parser on v0.10.x / v0.11.x beyond name availability; I only checked the registries there.
- The `(parameter=..., value=...)` suffix on v0.14 to v0.17 messages depends on that version's `create_error_response` using `str(exc)`. Likely, not checked.

---

## 5. Sources

vLLM source files are at tag v0.30.0 unless noted: `https://github.com/vllm-project/vllm/blob/v0.30.0/<path>`.

- **S1** `vllm/tool_parsers/__init__.py`. Older tags: `vllm/entrypoints/openai/tool_parsers/`.
- **S2** `vllm/reasoning/__init__.py`.
- **S3** https://docs.vllm.ai/en/latest/features/tool_calling.html (same as `docs/features/tool_calling.md` @v0.30.0).
- **S4** https://docs.vllm.ai/en/latest/features/reasoning_outputs.html (`docs/features/reasoning_outputs.md`).
- **S5** https://docs.vllm.ai/en/latest/features/structured_outputs.html (`docs/features/structured_outputs.md`).
- **S6** https://docs.vllm.ai/en/latest/features/interleaved_thinking.html.
- **S7** `docs/serving/online_serving/README.md` (endpoint list: Instrumentator, Tokenize, Anthropic APIs).
- **S8** https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server.html (`parallel_tool_calls` note).
- **S9** `vllm/entrypoints/openai/chat_completion/protocol.py` (request fields, `reasoning_content` input rename, `build_tok_params`, default sampling params).
- **S10** `vllm/entrypoints/openai/chat_completion/serving.py` (stream generator, `finish_reason`, `[DONE]`, usage).
- **S11** `vllm/entrypoints/generate/base/protocol.py` (`DeltaMessage`, `DeltaToolCall`, `DeltaFunctionCall`, `ToolCall`, `ResponseFormat`).
- **S12** `vllm/parser/engine/parser_engine.py` (tool-call delta emission and coalescing).
- **S13** `vllm/entrypoints/serve/engine/protocol.py` (`OpenAIBaseModel` extra-field handling and removed `guided_*` warning, `ErrorResponse`, `ModelCard`).
- **S14** `vllm/entrypoints/openai/models/serving.py`, `.../models/api_router.py`.
- **S15** `vllm/entrypoints/serve/tokenize/api_router.py`, `.../tokenize/protocol.py`.
- **S16** `vllm/entrypoints/serve/instrumentator/health.py`, `.../basic.py`. At v0.10.0: `vllm/entrypoints/openai/api_server.py`.
- **S17** `vllm/entrypoints/serve/middleware/authenticate.py`.
- **S18** `vllm/entrypoints/serve/exception_handling/error_response.py`, `vllm/exceptions.py`.
- **S19** `vllm/renderers/params.py` (context-length validation, v0.18 and later).
- **S20** Older context-length messages:
  - https://github.com/vllm-project/vllm/blob/v0.10.2/vllm/entrypoints/openai/serving_engine.py
  - https://github.com/vllm-project/vllm/blob/v0.13.0/vllm/entrypoints/openai/serving_engine.py
  - https://github.com/vllm-project/vllm/blob/v0.14.0/vllm/entrypoints/openai/serving_engine.py
  - https://github.com/vllm-project/vllm/blob/v0.17.0/vllm/entrypoints/openai/engine/serving.py
  - https://github.com/vllm-project/vllm/blob/v0.18.0/vllm/entrypoints/openai/engine/serving.py
- **S21** Protocol history:
  - `vllm/entrypoints/openai/protocol.py` @ v0.10.0, v0.10.1, v0.10.2, v0.11.0, v0.11.1, v0.11.2, v0.12.0, v0.13.0, v0.14.0
  - `vllm/entrypoints/openai/engine/protocol.py` and `.../chat_completion/protocol.py` @ v0.15.0, v0.15.1, v0.16.0, v0.17.0
- **S22** `vllm/entrypoints/serve/utils/tool_calls_utils.py`; `vllm/entrypoints/openai/serving_chat.py` @v0.11.1 (no filter) vs @v0.12.0 (filter present).
- **S23** `vllm/config/model.py` (`generation_config`, `override_generation_config`, `get_diff_sampling_param`).
- **S24** https://github.com/vllm-project/vllm/releases (v0.30.0 published 2026-09-22T05:20:54Z).
- **S25** https://github.com/vllm-project/recipes @ `cbf76c0` (2026-10-03): `models/<org>/*.yaml` and `Qwen/`, `GLM/`, `DeepSeek/`, `OpenAI/GPT-OSS.md`, `MiniMax/MiniMax-M2.md`, `moonshotai/`, `Seed/Seed-OSS-36B.md`, `Llama/`. Site: https://recipes.vllm.ai.
- **S26** `vllm/parser/qwen3.py`. History: `vllm/reasoning/qwen3_reasoning_parser.py` @v0.16.0 (needs both tags) vs @v0.17.0 / v0.20.0 (handles a prompt-side `<think>`).
- **S27** `vllm/parser/kimi_k2.py`, `vllm/parser/glm47_moe.py`, `vllm/reasoning/deepseek_v3_reasoning_parser.py` (thinking defaults from `chat_template_kwargs`).
- **S28** `vllm/reasoning/minimax_m2_reasoning_parser.py`.
- **S29** `vllm/parser/harmony.py`, `vllm/parser/parser_manager.py`, `docs/benchmarking/cli.md` (gpt-oss BFCL server command).
- **S30** `vllm/config/model.py` (`TokenizerMode`).
- **S31** `vllm/entrypoints/launchers/cli_args.py`, `vllm/renderers/online_renderer.py`, `vllm/parser/parser_manager.py`.
- **S32** `vllm/entrypoints/chat_utils.py` (assistant `reasoning` / `reasoning_content` passed to the template).
- **S33** `vllm/envs.py` (`VLLM_USE_RUST_FRONTEND`); recipes `src/lib/command-synthesis.js`.

**Hugging Face model cards** (`README.md`, `generation_config.json`, `config.json`, chat templates), fetched raw from `https://huggingface.co/<id>/raw/main/...`. Not cited above as S-numbers.

| Family | Repos |
|---|---|
| Qwen | `Qwen/Qwen3-Coder-30B-A3B-Instruct`, `Qwen3-Coder-480B-A35B-Instruct`, `Qwen3-Coder-Next`, `Qwen3-30B-A3B-Instruct-2507`, `Qwen3-30B-A3B-Thinking-2507`, `Qwen3-235B-A22B-Instruct-2507`, `Qwen3-235B-A22B-Thinking-2507`, `Qwen3-Next-80B-A3B-Instruct`, `Qwen3-Next-80B-A3B-Thinking`, `Qwen3-32B`, `Qwen3.5-27B`, `Qwen3.5-35B-A3B`, `Qwen3.5-397B-A17B`, `Qwen3.6-27B`, `Qwen3.6-35B-A3B`, `Qwen3-VL-30B-A3B-Instruct`, `Qwen3-VL-30B-A3B-Thinking`, `Qwen3-VL-235B-A22B-Instruct` |
| GLM | `zai-org/GLM-4.5`, `GLM-4.5-Air`, `GLM-4.6`, `GLM-4.7`, `GLM-4.7-Flash`, `GLM-4.5V` |
| OpenAI | `openai/gpt-oss-20b`, `gpt-oss-120b` |
| DeepSeek | `deepseek-ai/DeepSeek-V3.1`, `DeepSeek-V3.1-Terminus`, `DeepSeek-V3.2`, `DeepSeek-V3.2-Exp` |
| Mistral | `mistralai/Devstral-Small-2507`, `Devstral-Small-2-24B-Instruct-2512`, `Devstral-2-123B-Instruct-2512` |
| Moonshot | `moonshotai/Kimi-K2-Instruct`, `Kimi-K2-Instruct-0905`, `Kimi-K2-Thinking` (+ `docs/deploy_guidance.md`) |
| MiniMax | `MiniMaxAI/MiniMax-M2` (+ `docs/vllm_deploy_guide.md`), `MiniMax-M2.1`, `MiniMax-M2.5` |
| ByteDance | `ByteDance-Seed/Seed-OSS-36B-Instruct` |
| Meta mirrors | `unsloth/Llama-3.3-70B-Instruct`, `unsloth/Llama-4-Scout-17B-16E-Instruct`, `unsloth/Llama-4-Maverick-17B-128E-Instruct` (the Meta originals returned HTTP 401) |

**Other upstream READMEs**
- https://github.com/QwenLM/Qwen3-VL/blob/main/README.md (Generation Hyperparameters)
- https://github.com/openai/gpt-oss/blob/main/README.md (Recommended Sampling Parameters)
