# GPU spike — two upstream reports, drafted, not filed

Both findings come from the Phase 3 run of the local-LLM spike
(`docs/gpu-phase3-agent.md`, "Step 7 and Arm B — run"). They are written to be
pasted as issues by the Product Owner, who ran the reproductions and holds the
terminal history. Discipline: evidence first, reproduction steps, verbatim
output where we have it, **no speculation about cause**. Fields marked
`<fill: …>` were not in the run report and must come from the box before
filing; do not guess them.

Neither report mentions Nemr. The reproductions stand on their own: a stock
image, a stock model, a stock request.

---

## 1. ollama/ollama — tool calls emitted as text under a multi-tool schema set; a single-tool request works (0.33.3)

**Title:** Tool calls are returned as text, not structured tool calls, when a
request carries a coding agent's full tool set — single-tool requests work
(0.33.3, `/v1/messages` and `/v1/chat/completions`)

**Related:** #15529 (closed without a linked fix; this reproduces on 0.33.3).

### Environment

- Ollama `0.33.3` (`/api/version`), image `docker.io/ollama/ollama:latest`,
  digest `<fill: ctr images ls | grep ollama>`
- Host: WSL2 (Ubuntu), NVIDIA RTX 3070 8 GB, `library=CUDA`, fully resident
  models (`/api/ps` `size_vram == size` in every run below)
- Models (all from the library, unmodified): `llama3.1:8b`,
  `qwen2.5-coder:7b`, `qwen3-coder:30b`
- Context length: `OLLAMA_CONTEXT_LENGTH=8192` (fully resident; the failure
  is identical at the default 4096)
- Clients: Claude Code `<fill: claude --version>` via `ANTHROPIC_BASE_URL`
  (Anthropic-compatible `/v1/messages`); OpenAI Codex CLI `<fill: codex
  --version>` via `codex exec --oss --local-provider ollama`
  (`/v1/chat/completions`)

### What works — one tool, direct request, no agent

```bash
curl -s localhost:11434/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: ollama' -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"llama3.1:8b","max_tokens":256,
       "tools":[{"name":"write_file","description":"Write a file",
                 "input_schema":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}}],
       "messages":[{"role":"user","content":"Use the write_file tool to write hello.txt containing hi."}]}'
```

Response (abridged, verbatim fields):

```
"type": "tool_use", "name": "write_file",
"input": {"path": "hello.txt", "content": "hi"}
"stop_reason": "tool_use"
```

The Messages envelope is correct throughout (`content[]`, `stop_reason`,
`usage`). `/v1/models` returns 200. (`/v1/messages/count_tokens` returns 404;
noted, not part of this report.)

### What fails — the same models under a coding agent's full tool set

Task, in a fresh empty directory, non-interactive: *"Create a file named
hello.py in the current directory containing a Python program that prints
exactly: hello from ollama. Use your file-writing tool, then stop."*

Claude Code, `claude --bare -p --output-format json --allowedTools
"Write,Read,Bash"`, `ANTHROPIC_BASE_URL=http://127.0.0.1:11434`,
`ANTHROPIC_API_KEY=ollama`, `ANTHROPIC_MODEL=<model>`. The request carries
the agent's tool definitions (Write, Read, Bash and the rest of its set —
`<fill: tool count from a captured request, if available>`).

| # | Model | Client | API path | Result |
|---|---|---|---|---|
| 1 | `llama3.1:8b` | Claude Code | `/v1/messages` | text: `{"name": "write_file", "parameters": {"content": "print(", …}}` |
| 2 | `qwen2.5-coder:7b` | Claude Code | `/v1/messages` | text: `{"name": "Write", "parameters": {"path": …, "text": "print(", …}}` |
| 3 | `qwen3-coder:30b` | Claude Code | `/v1/messages` | text: `<function=Edit><parameter=file_path>/work/hello.py</parameter>…</tool_call>` |
| 4 | `qwen2.5-coder:7b` | Codex `--oss` | `/v1/chat/completions` | text, markdown-fenced: ` ```json {"name": "exec_command", "arguments": {"cmd": "echo '…' > hello.py"}} ``` ` |
| 5 | `<fill: confirm — qwen3-coder:30b, or row 4 re-run with sandbox_mode=workspace-write>` | Codex `--oss` | `/v1/chat/completions` | text, same shape as 4 (`<fill: verbatim line>`) |

In every run: no `tool_use` block (Messages) / no `tool_calls` (chat
completions) in the response; `stop_reason`/`finish_reason` is the plain
end-of-turn value; no file was created; the directory stayed empty. Codex's
`sandbox_mode=workspace-write` changed nothing.

Row 3 is the one worth reading closely: Qwen3-Coder emitted its own native
tool-call XML, well-formed, and a chat-template `</tool_call>` marker is
present verbatim in the returned text.

### What was ruled out

- **The model:** three models, 7B dense to 30B MoE, one result.
- **The client:** two agents with different request shapes, one result.
- **The API path:** both the Anthropic-compatible and the OpenAI-compatible
  endpoints, one result.
- **Residency / degraded generation:** every run fully on the GPU
  (`size_vram == size`, 33/33 layers, GPU-class tok/s).
- **Tool support as such:** the single-tool request above returns a correct
  structured call from the same server, same model, same context length.

The one variable that separates the working case from the failing ones is
the number and size of tool definitions in the request.

### Steps to reproduce

1. `ollama serve` (0.33.3) with `OLLAMA_CONTEXT_LENGTH=8192`; `ollama pull
   llama3.1:8b`.
2. Run the single-tool `curl` above — observe a `tool_use` block.
3. In an empty directory, with Claude Code installed:
   ```bash
   ANTHROPIC_BASE_URL=http://127.0.0.1:11434 ANTHROPIC_API_KEY=ollama \
   ANTHROPIC_MODEL=llama3.1:8b ANTHROPIC_SMALL_FAST_MODEL=llama3.1:8b \
   claude --bare -p --output-format json --allowedTools "Write,Read,Bash" \
     <<< 'Create a file named hello.py in the current directory containing a Python program that prints exactly: hello from ollama. Use your file-writing tool, then stop.'
   ls   # empty; the JSON result contains the tool call as text
   ```
4. Optionally the Codex path: `codex exec --oss --skip-git-repo-check
   --local-provider ollama -m qwen2.5-coder:7b <<< '<same prompt>'`.

A captured request body from step 3 (the exact tool array) would make this
self-contained without either client; `<fill: attach if captured>`.

---

## 2. ggml-org/llama.cpp — server: single trivial tool call returned as text (never the model's native `<tool_call>`) for Qwen2.5-Coder-7B-Instruct Q4_K_M with `--jinja`, on both `/v1/messages` and `/v1/chat/completions`

**Title:** server: tool call emitted as free text in four different formats
(never `<tool_call>`) for Qwen2.5-Coder-7B-Instruct-GGUF Q4_K_M, `--jinja`,
one trivial tool — template's tool branch is loaded

### Environment

- Image `ghcr.io/ggml-org/llama.cpp:server-cuda`, digest `<fill>`;
  `llama-server --version` → `<fill>`
- Host: WSL2 (Ubuntu), NVIDIA RTX 3070 8 GB; CUDA backend
- Model: `bartowski/Qwen2.5-Coder-7B-Instruct-GGUF`, `Q4_K_M`, pulled
  directly from Hugging Face (`<fill: the exact -hf / -m flag as run>`); the
  same result with a Q4_K_M GGUF of the same model from another source
  (Ollama's library blob for `qwen2.5-coder:7b`, 4.68 GB)
- Server flags: `--jinja -ngl 99 -c 8192 --host 0.0.0.0 --port 8080`
- Startup log confirms the template and residency:
  ```
  tokenizer.chat_template = {%- if tools %}…            <fill: paste the full line>
  offloaded 29/29 layers to GPU
  <fill: the "CUDA0 model buffer size = 4168 MiB" line>
  ```
- KV cache at defaults (no `-ctk`/`-ctv` quantization).

### Request — one trivial tool, no client in the loop

Anthropic-compatible endpoint:

```bash
curl -s localhost:8080/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: x' -H 'anthropic-version: 2023-06-01' \
  -d '{"model":"qwen2.5-coder","max_tokens":256,
       "tools":[{"name":"write_file","description":"Write a file",
                 "input_schema":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}}],
       "messages":[{"role":"user","content":"Use the write_file tool to write hello.txt containing hi."}]}'
```

OpenAI-compatible endpoint, the same tool in `tools[].function` form against
`/v1/chat/completions` (`<fill: paste the body as run>`).

### Observed

```
/v1/messages         → content is a single text block: a ```xml-fenced JSON object; stop_reason: end_turn
/v1/chat/completions → content is text: <function-call>…</function-call>;      finish_reason: stop
                       62.9 tok/s
```

No `tool_use` block, no `tool_calls` array, in any run. Across the runs
below the text form varied — `<function-call>…</function-call>`,
`<function_call>…</function_call>`, a fenced JSON object, a fenced XML block —
and Qwen's own `<tool_call>` format did not appear in any of them.
`/v1/messages/count_tokens` returns 200.

`<fill: paste one full response body per endpoint, verbatim>`

### What was ruled out, in order

1. **The GGUF's origin.** First run used Ollama's Q4_K_M blob for the same
   model; re-run with the file pulled directly from `bartowski/…`.
   Same result. The startup log shows the template with its `{%- if tools %}`
   branch loaded.
2. **Residency / speed.** One run collided with a second server on the card
   and generated at 0.16 tok/s; isolated and re-run at 62.9 tok/s. Same
   result.
3. **Chat parsing.** `--no-skip-chat-parsing --chat-template-kwargs
   '{"enable_thinking":false}'`. Same result — the text form became
   `<function_call>` inside an xml fence.
4. **`--jinja`.** It is already the default in this build; passing it
   explicitly changed nothing.

For comparison, the same single-tool request against Ollama 0.33.3 with the
library's `qwen2.5-coder:7b` returns a structured `tool_use` block — so the
model family can produce a parseable call under some serving path.

### Steps to reproduce

```bash
docker run --gpus all -p 8080:8080 ghcr.io/ggml-org/llama.cpp:server-cuda \
  -hf bartowski/Qwen2.5-Coder-7B-Instruct-GGUF:Q4_K_M \
  --jinja -ngl 99 -c 8192 --host 0.0.0.0 --port 8080
# then the curl above; observe a text block, not tool_use
```

(The reporter's run used containerd rather than Docker — the image, model and
flags are the same.)

`<fill: attach the server log from startup through the first request>`
