# GPU Phase 3 — a coding agent pointed at the local server, by hand

Phase 1 proved a rootless container on our containerd can use the GPU through
bind mounts and env alone. Phase 2 proved an 8B model serves from it, on the
GPU, at GPU-class speed, through that delta unchanged. Phase 3 asks the next
question, and only the next one:

> **Can a coding agent be pointed at that server and complete a real task —
> an edit made through a tool call, verified on disk — with the work served
> by the local model?**

Same discipline. **Build nothing**: no `setup_host.sh`, engine, base-image or
containerd-config change. "Feasible only under a condition" and "not feasible"
remain acceptable verdicts, and this is the phase where the second is most
likely. Phase 4 is the first engine work, and only once the whole stack is
proven by hand.

**Not in scope now:** model quality beyond the pass criterion; which agent
ships in the product; how weights or model choice travel in a bundle; GPU
sharing across sessions; any Nemr code. If Arm B below finds a Nemr networking
gap, it is **recorded as a Phase-4 item**, not fixed here.

**Costs, unchanged and stated:** nothing here ever has CI coverage. Nothing
new is installed — both agents are already in the base image (`image/Dockerfile`
installs `@anthropic-ai/claude-code` and `@openai/codex`), so Arm A runs the
base image by hand and Arm B runs a real session. No Docker Engine (NFR-01).

**The two traps this phase exists to catch.**

1. *Presence versus continuity, for an agent:* a model that **chats** is not a
   coding agent. The pass criterion is a **completed edit** — a file that
   exists on disk afterwards, with the right contents, that *runs* — made
   through a tool call. A completed sentence is a fail, however fluent.
2. *Which model answered:* a Nemr session has **egress** (NET-02) and carries
   the user's **real** Anthropic credential (D-02). A "working" run could have
   quietly gone to `api.anthropic.com`. So every pass needs independent proof
   the request landed on Ollama — its own log, the card, a **negative
   control**, and the model naming itself. Arm A cannot cheat (it holds no
   credential); Arm B can, and is checked for it.

**The honest ceiling, stated before the first run:** 8B on 8 GB is a plumbing
proof, not a usable assistant. The verdict will say so in those words whatever
the result. "It worked" here means "the plumbing carries an agent's tool
call"; it does not mean the model can do real work.

**Record everything with `tee`:**

```bash
cd ~/gpu-spike; mkdir -p work-a
# prefix every command block below with:  2>&1 | tee -a ~/gpu-spike/phase3-<n>.log
```

---

## The delta, verbatim — and the Phase 2 server, running

The definitions (`CTR`, `CTRUN`, `NS`, `DRV`, `GPU`, the image and model
names) now live in **one sourceable file** — the Phase 3 run found each new
terminal needed the whole block re-pasted, and `CTRUN` went missing from one.
In **every** terminal, first:

```bash
source ~/src/nemr-engine/docs/gpu-env.sh     # prints the driver store; the self-check must print nothing else
```

Then the Phase 2 server, in terminal 1, with **one addition**, explained in
Step 0:

```bash
# terminal 1 — Phase 2's server command, plus a context length (Step 0 decides the number)
CTRUN srv --rm --net-host "${GPU[@]}" \
  --mount "type=bind,src=$HOME/gpu-spike/ollama-models,dst=/root/.ollama,options=rbind:rw" \
  --env OLLAMA_HOST=0.0.0.0:11434 \
  --env OLLAMA_CONTEXT_LENGTH=8192 \
  "$OLL" ollama-srv ollama serve 2>&1 | tee ~/gpu-spike/phase3-serve.log
```

Confirm the Phase 2 gate again before anything else: `library=CUDA` naming the
3070 in the startup log. If the driver changed since Phase 2, re-record the
Phase 1 preconditions first.

---

## Step 0 — context length: the wall, and the trade-off it forces

Phase 2 ran at Ollama's VRAM-derived default of **4096** tokens. A coding
agent's *first* request carries its whole system prompt **plus every tool
schema** — for Claude Code, on the order of ten to twenty thousand tokens
before the task is even stated. At 4096 the model never sees the task, or the
tools, and the failure looks like a stupid model rather than a truncated one.

So the context must go up, and every token of it costs VRAM (KV cache), out of
a **6.9 GiB** budget that a 5.3 GiB model already mostly owns. This is the
trade-off Phase 3 is most likely to be decided by, and it is measured, not
guessed:

```bash
# with the server started at OLLAMA_CONTEXT_LENGTH=8192, load the model and check residency
NS curl -s localhost:11434/api/generate -d "{\"model\":\"$MODEL\",\"prompt\":\"OK\",\"stream\":false}" >/dev/null
NS curl -s localhost:11434/api/ps | python3 -m json.tool | grep -E '"size"|size_vram'
/usr/lib/wsl/lib/nvidia-smi --query-gpu=memory.used,memory.total --format=csv
grep -iE "truncat|context" ~/gpu-spike/phase3-serve.log | tail -5
```

**Gate:** `size_vram` still equals `size` at 8192 (full residency). If it
does, restart at **16384** and repeat; record the **largest context that stays
fully resident** — that number *is* a Phase-4 requirement. If 8192 already
breaks residency (partial offload, tok/s collapse), the honest finding is that
this card cannot hold a coding agent's context and an 8B model at once, and
Phase 3's verdict is "feasible with caveats" at best — say which caveat.

---

## Step 1 — the endpoint shape, before any agent touches it

Does Ollama speak the Anthropic Messages API, and does **tool calling survive
that layer**? Establish both *directly*, so a later failure can be attributed
to Ollama, to the model, or to the agent — three different findings.

```bash
MODEL=llama3.1:8b
# 1a. a plain Messages request — expect Messages-shaped JSON (content[], stop_reason, usage)
NS curl -s localhost:11434/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: ollama' -H 'anthropic-version: 2023-06-01' \
  -d "{\"model\":\"$MODEL\",\"max_tokens\":64,\"messages\":[{\"role\":\"user\",\"content\":\"Reply with the single word OK.\"}]}" \
  | python3 -m json.tool | head -30

# 1b. the same, with ONE trivial tool — the early tool-calling signal, with no agent in the loop
NS curl -s localhost:11434/v1/messages \
  -H 'content-type: application/json' -H 'x-api-key: ollama' -H 'anthropic-version: 2023-06-01' \
  -d "{\"model\":\"$MODEL\",\"max_tokens\":256,
       \"tools\":[{\"name\":\"write_file\",\"description\":\"Write a file\",
                  \"input_schema\":{\"type\":\"object\",\"properties\":{\"path\":{\"type\":\"string\"},\"content\":{\"type\":\"string\"}},\"required\":[\"path\",\"content\"]}}],
       \"messages\":[{\"role\":\"user\",\"content\":\"Use the write_file tool to write hello.txt containing hi.\"}]}" \
  | tee ~/gpu-spike/phase3-toolprobe.json | python3 -c '
import json,sys; r=json.load(sys.stdin)
kinds=[b.get("type") for b in r.get("content",[])]
print("stop_reason:", r.get("stop_reason"), " content types:", kinds)
print("TOOL_USE PRESENT" if "tool_use" in kinds else "NO tool_use — text only")'

# 1c. endpoints an agent may call before its first message — a 404 here is a finding, not a mystery later
NS curl -s -o /dev/null -w 'count_tokens: %{http_code}\n' localhost:11434/v1/messages/count_tokens \
  -H 'content-type: application/json' -H 'x-api-key: ollama' -H 'anthropic-version: 2023-06-01' \
  -d "{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"x\"}]}"
NS curl -s -o /dev/null -w 'models: %{http_code}\n' localhost:11434/v1/models
```

**Gate:** 1a returns a Messages-shaped body; 1b returns a `tool_use` block
with a sane `input`. If 1a fails, the compatibility layer is absent or
different and Claude Code cannot be the first agent — go to Step 5. If 1a
passes and 1b is text-only, the tool-calling break is **below** the agent:
Ollama's translation or the model. Try the second model (Step 6) *here*, at
this probe, before spending an agent run on it.

---

## Step 2 — Arm A, the control: agent and server in the same namespace

The base image, run by hand with `--net-host`, so the agent shares
rootlesskit's network namespace and Ollama is `127.0.0.1:11434`. This arm
holds **no Anthropic credential** — the image has none and nothing is
mounted — so the only way a task can complete is through Ollama. That makes it
the control for "which model answered" *by construction*.

```bash
rm -rf ~/gpu-spike/work-a && mkdir -p ~/gpu-spike/work-a ~/gpu-spike/prompts   # work-a is FRESH and EMPTY: a file appearing in it is proof a tool ran
# The prompt goes in through STDIN FROM A FILE, on its own read-only bind so the
# workspace stays empty. `ctr run` does not attach stdin and nested quoting eats a
# prompt argument — Phase 3 measured `claude -p` waiting 3 s then failing with
# "Input must be provided". `< /dev/null` is not enough; a file is.
printf '%s\n' "Create a file named hello.py in the current directory containing a Python program that prints exactly: hello from ollama. Use your file-writing tool, then stop." > ~/gpu-spike/prompts/task1.txt
# The agent command and its environment come from gpu-env.sh (CLAUDE_TASK, AGENT_ENV) so
# every arm runs the SAME shape; only the base URL is spelled here, because it is the one
# thing that differs per arm. `--bare` (found in the Phase 3 run): skips hooks and
# prefetches — including the count_tokens call Ollama 404s on — and forces
# ANTHROPIC_API_KEY auth, which is also the cheapest guard against prediction 4.
CTRUN agent-a --rm --net-host \
  --mount "type=bind,src=$HOME/gpu-spike/work-a,dst=/work,options=rbind:rw" \
  --mount "type=bind,src=$HOME/gpu-spike/prompts,dst=/prompts,options=rbind:ro" \
  "${AGENT_ENV[@]/#/--env=}" --env=ANTHROPIC_BASE_URL=http://127.0.0.1:11434 \
  "$BASE" agent-a sh -c "$(CLAUDE_TASK /work /prompts/task1.txt)" \
  2>&1 | tee ~/gpu-spike/phase3-arm-a.json
```

Flag names are Claude Code's; **check `claude --help` in this image** before
assuming them — they vary by version. If a permission prompt blocks a
non-interactive run, `--dangerously-skip-permissions` is acceptable in a
throwaway container and nowhere else; record that it was needed.

**Then the verification, on the host — the pass criterion lives here, not in
the agent's output:**

```bash
ls -la ~/gpu-spike/work-a/                       # the file exists? (the dir was empty)
cat ~/gpu-spike/work-a/hello.py
python3 ~/gpu-spike/work-a/hello.py              # it RUNS and prints the string — continuity
grep -c '"tool_use"' ~/gpu-spike/phase3-arm-a.json   # the agent's own record of a tool call
grep -E '"POST /v1/messages' ~/gpu-spike/phase3-serve.log | tail -3   # Ollama saw the requests
```

**Gate:** `hello.py` exists in a directory that was empty, prints the string,
and Ollama's log shows the Messages requests. A response that *describes*
the file without creating it is the chat-only fail. Record the first-turn
wall time separately from a second run — the first turn is warmup (Phase 2's
rule), and the big prompt is cold.

---

## Step 3 — Arm B, the real topology: a Nemr session reaches the server at its gateway

This is the arm that matters for Phase 4, and it changes **nothing** in Nemr.
A session runs in its own network namespace (NET-02), joined to rootlesskit's
by a veth whose rootlesskit end — `10.99.N.1`, the session's **default
gateway** — is where Ollama is already listening on `0.0.0.0`. Nemr's answer
to "reach a rootlesskit-namespace service from a session" already exists; it
has never been exercised for this.

```bash
nemr create gpuagent                     # needs the real credential (AUTH-03) — that is the point of this arm
nemr start gpuagent
SESS=nemr-gpuagent                       # the container id nemr gives a project

# 3a. discover the gateway FROM INSIDE the session, and prove reachability before any agent runs
CTR task exec --exec-id gw "$SESS" sh -c 'ip route | awk "/^default/{print \$3}"'      # expect 10.99.N.1
GW=<paste it>
CTR task exec --exec-id ver "$SESS" curl -s "http://$GW:11434/api/version"             # expect Ollama's version JSON
```

**Gate:** the version JSON comes back. If it does not, that is a **Nemr
networking gap** — a session cannot reach a service in rootlesskit's
namespace — and it is recorded as a Phase-4 item (class `nemr-networking gap`),
with `CTR task exec … ip route`, `iptables -S FORWARD` from `NS`, and the
verbatim curl error. Do not fix it here. (Prediction: it works — the NET-05
isolation rule is a `FORWARD` drop between sessions; delivery to the gateway
address is `INPUT`.)

```bash
# Every prompt in this arm goes in as a FILE, the same shape as Arm A (CLAUDE_TASK reads
# it from stdin). Write the three prompts into the session first, outside /workspace so
# the workspace's contents stay the evidence:
printf '%s\n' "Reply with OK." > ~/gpu-spike/prompts/ok.txt
printf '%s\n' "Which model are you, and who made you? One line." > ~/gpu-spike/prompts/who.txt
for f in ok who task1; do
  CTR task exec --exec-id "p-$f" "$SESS" sh -c "mkdir -p /tmp/prompts && cat > /tmp/prompts/$f.txt" < ~/gpu-spike/prompts/$f.txt
done

# 3b. the NEGATIVE control first: a base URL that cannot answer. This MUST fail.
CTR task exec --exec-id neg -t "$SESS" env "${AGENT_ENV[@]}" ANTHROPIC_BASE_URL="http://$GW:1" \
  sh -c "$(CLAUDE_TASK /workspace /tmp/prompts/ok.txt)" ; echo "exit=$?"
# Expect ~3 minutes before it fails: Claude Code retries a refused connection with
# backoff (180 s, `ConnectionRefused`, exit 1, measured on the reference host with this
# exact shape). That wait is the control working, not hanging.
```

If the negative control **succeeds**, Claude Code ignored the base URL and
used the real credential D-02 mounts into every session — and everything
that follows would prove nothing. That is a finding of the first order:
record it, class `agent gap`, and see prediction 4 for what it means.

```bash
# 3c. the real run — into the session's /workspace (the project volume), through the gateway.
CTR task exec --exec-id agent -t "$SESS" env "${AGENT_ENV[@]}" ANTHROPIC_BASE_URL="http://$GW:11434" \
  sh -c "$(CLAUDE_TASK /workspace /tmp/prompts/task1.txt)" \
  2>&1 | tee ~/gpu-spike/phase3-arm-b.json

# 3d. which model answered — ask it, and watch the card
CTR task exec --exec-id who -t "$SESS" env "${AGENT_ENV[@]}" ANTHROPIC_BASE_URL="http://$GW:11434" \
  sh -c "$(CLAUDE_TASK /workspace /tmp/prompts/who.txt)"
/usr/lib/wsl/lib/nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv   # during/just after a turn
grep -E '"POST /v1/messages' ~/gpu-spike/phase3-serve.log | tail -3
```

`nemr attach` may drop straight into Claude Code's own process rather than a
shell (PROC-03: the process model is explicit); `ctr task exec` is used above
so the environment can be set by hand without touching how Nemr starts the
session. If `attach` does give a shell, the same `export`s work there.

**Verification, and the bonus the volume gives for free:**

```bash
CTR task exec --exec-id chk "$SESS" sh -c 'ls -la /workspace; cat /workspace/hello.py; python3 /workspace/hello.py'
nemr stop gpuagent && nemr start gpuagent
CTR task exec --exec-id chk2 "$SESS" python3 /workspace/hello.py     # the edit persisted on the volume across a restart
```

**Gate:** the negative control failed; the real run created `hello.py` on the
session's volume; it runs; it survived a stop/start; Ollama's log shows the
requests; the model names itself as a Llama (not Claude); the card was busy.
Any one dissenting signal is the finding.

**Cleanup — verify before destroying, and it is not a protected subject:**

```bash
nemr list | grep gpuagent          # confirm it is the throwaway, then:
nemr delete gpuagent
```

---

## Step 4 — a task with one more step in it

If Steps 2–3 pass, one task that needs **two** tool calls and a *read*, so the
agent must act on what it observed rather than emit a file from memory:

> "Read hello.py. Change it to print the string in upper case. Then run it
> with python3 and tell me the output."

Pass: the file changed (diff it), and the reported output matches what
`python3 hello.py` prints on the host. This is where an 8B model most often
comes apart — a read it did not do, an edit it described but did not make, an
output it invented. Record exactly what happened; it is the ceiling being
measured, not the plumbing.

---

## Step 5 — the second agent (Codex `--oss`), only if Claude Code's path is blocked

Run this **only** if Step 1a fails (no Messages compatibility) or Claude Code
cannot be made to send tool calls at all. Codex is already in the base image
and its `--oss` mode talks to a local Ollama over the OpenAI-compatible API,
which Ollama has served for longer than the Anthropic one. Two things to
establish, not assume: `codex --help` for how `--oss` locates the server (in
Arm A it is `127.0.0.1:11434`; in Arm B it must be pointed at `$GW`, and
whether it *can* be is the question), and the same fresh-directory,
file-exists, file-runs pass criterion. The command is `CODEX_TASK` from
`gpu-env.sh` — prompt from a file via stdin, `--skip-git-repo-check` carried
(divergence 4: with an argument prompt outside a repo Codex hangs silently) —
run exactly as Arm A with `sh -c "$(CODEX_TASK /work /prompts/task1.txt)"`. Codex's own auth is a separate open
item; `--oss` should need none — if it demands a login, record it and stop.
Why Claude Code first regardless: it is the product's agent, its session
model (D-02, M8) is built around it, and `ANTHROPIC_BASE_URL` is a documented
override — a Phase-4 design that works for it works for the product.

---

## Step 6 — the model as a variable

If Step 1b is text-only for `llama3.1:8b`, try one more model *at Step 1b*
before touching an agent: `qwen2.5-coder:7b` (tool-capable, coding-tuned,
~4.7 GB). A plumbing variable, not a product choice; if it changes the
answer, record both. Two models that both fail at 1b make the finding "8B
tool calling through this layer" rather than "this model".

---

## Step 7 — the llama.cpp probe: grammar-constrained tool calling, no new code

*Added after the Phase 3 run (see "Findings before the verdict", below). Run
this before the verdict is written.*

The run isolated the failure to **Ollama's tool translation under a real
agent's schema set** — five combinations, three models, two agents, two API
paths, one result — and the model was doing its part (Qwen3-Coder emitted its
native tool XML correctly, with a `</tool_call>` marker leaking through
unconverted). That is a known, reported Ollama defect
([ollama/ollama#15529](https://github.com/ollama/ollama/issues/15529): closed
without a linked fix, still reproducing on 0.33.3).

Ollama wraps llama.cpp. llama.cpp's **own** server takes a different route:
with `--jinja` it renders the tool schemas through the model's chat template
and, per its docs, generates a grammar from them so that a call is emitted as
a structured block rather than parsed out of free text afterwards
([docs/function-calling.md](https://github.com/ggml-org/llama.cpp/blob/master/docs/function-calling.md);
Llama 3.1 and Qwen 2.5 Coder are in its natively-supported list). And it
speaks the **Anthropic Messages API natively** — `POST /v1/messages` with
`tool_use`/`tool_result` blocks, SSE streaming, **and** `count_tokens`
([announcement](https://huggingface.co/blog/ggml-org/anthropic-messages-api-in-llamacpp),
[server README](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)).
So Claude Code points at it directly. If this works, the feature arrives with
**no new code and no proxy** — the outcome worth one more probe.

One caveat carried from the docs: extreme KV-cache quantization degrades tool
calling. The probe leaves KV at its default.

**7a — the binary, and the blob.** Ollama's runner links llama.cpp as a
*library*; the standalone `llama-server` is not expected in the Ollama image
(its log line "starting llama server" names the runner, not a binary). Check,
then use the official image — a registry pull by `ctr`, no Docker:

```bash
source ~/src/nemr-engine/docs/gpu-env.sh
CTRUN chk --rm "$OLL" chk sh -c 'find / -name "llama-server*" -type f 2>/dev/null; echo "---"; ls /usr/lib/ollama /usr/local/bin 2>/dev/null'
# expected: nothing found → the official server image:
ctr -n default images pull --platform linux/amd64 "$LCPP"
ctr -n default images ls | grep llama.cpp                     # record the digest
CTRUN chk2 --rm "$LCPP" chk2 sh -c 'ls /app; /app/llama-server --version'   # where the binary is, and its version

# the GGUF is Ollama's model-layer blob — find it via the manifest, don't guess
MAN="$MODELS_DIR/models/manifests/registry.ollama.ai/library/${MODEL%%:*}/${MODEL##*:}"
GGUF=$(python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print([l["digest"] for l in m["layers"] if l["mediaType"].endswith(".model")][0])' "$MAN" | sed 's/:/-/')
ls -la "$MODELS_DIR/models/blobs/$GGUF"; head -c4 "$MODELS_DIR/models/blobs/$GGUF"; echo   # "GGUF" magic
```

**7b — the server.** Same GPU delta, the blob directory bound read-only,
`--jinja`, all layers on the card, the measured context ceiling:

```bash
# terminal 1
CTRUN lcpp --rm --net-host "${GPU[@]}" \
  --mount "type=bind,src=$MODELS_DIR/models,dst=/models,options=rbind:ro" \
  "$LCPP" llama-srv /app/llama-server -m "/models/blobs/$GGUF" \
    --jinja -ngl 99 -c 8192 --host 0.0.0.0 --port 8080 \
  2>&1 | tee ~/gpu-spike/phase3-llama-serve.log
```

Read the startup log: layers offloaded to CUDA (all of them), the context, and
that `--jinja` took the model's template. If the extension-less blob is
refused, symlink it as `<name>.gguf` in a scratch dir and bind that — record
the divergence. Then, from terminal 2, `nvidia-smi` for residency.

**7c — the baseline: the Step 1b trivial-tool probe, against port 8080.** Same
request, same model, `count_tokens` included (Ollama 404s it; this should
200). Expect a `tool_use` block; a text-only answer here means `--jinja` did
not take and nothing below can pass.

**7d — the decisive one.** Arm A exactly as Step 2 — fresh empty `work-a`,
`CLAUDE_TASK` with the prompt from the file, `AGENT_ENV` — with **only the
base URL changed** to `--env=ANTHROPIC_BASE_URL=http://127.0.0.1:8080`. Full toolset. Then the host verification: does
`hello.py` exist, does it run, does the JSON show a `tool_use`, does the
llama-server log show the `/v1/messages` requests. If it passes, run Step 4's
read-edit-run task against it too, and `codex exec --oss` against
`/v1/chat/completions` if `--local-provider` can be pointed at port 8080
(probe `codex --help`; if it cannot, Claude Code alone decides — the Anthropic
endpoint makes it the primary arm here).

**What 7d can show, and what each outcome means:**

| 7c | 7d | Meaning |
|---|---|---|
| tool_use | file on disk, runs | Grammar-constrained calling survives a real schema set. The feature needs **no new code**: Phase 4's inference server is llama-server, not Ollama, and the Arm C delta plus a `--jinja` flag is the whole contract. |
| tool_use | text again | The break is above the grammar — the schema *set*, not the format. Record what the grammar produced (a refusal? an empty call?); this is the finding that would justify looking at vLLM's per-model parsers as a *dependency*, never our own parser. |
| tool_use | error before the first message | An endpoint or shape mismatch at the Anthropic layer; verbatim, attributable. |
| text | — | `--jinja` or the template did not take; fix that before reading 7d. |

**The proxy question, answered by survey, not by building one.** Proxies that
put Claude Code in front of Ollama exist — UniClaudeProxy, several
`claude-code-proxy` variants, LiteLLM. The ones that *do* make text tool calls
work do it by injecting `<tool_call>` XML into the system prompt and parsing it
back out — a per-model text parser for undocumented formats, exactly the class
this project has spent weeks eliminating. The translation-only ones do not fix
a backend that emits text. So the survey's answer is: **no existing dependency
solves this cleanly; the backend that never needs parsing is the fix**, and
that is what 7d measures. A parser proxy stays a fallback on paper, not a plan.

## Predictions — what we expect to break, and why

Ranked. Held loosely — Phase 2's scorecard was six of nine dead or benign,
and the two that fired were unpredicted.

1. **Context length is the wall — near-certain at 4096, likely still a
   constraint at 8192.** A coding agent's first request is its whole prompt
   plus every tool schema. At Phase 2's default the model never sees the
   task; it answers something plausible and calls no tool, which looks like
   stupidity and is truncation. Fires as `truncating input prompt` in the
   server log or an off-task reply. The condition is `OLLAMA_CONTEXT_LENGTH`
   ≥ 8k–16k, **paid in VRAM** from a 6.9 GiB budget that a 5.3 GiB model
   already owns — Step 0 measures whether full residency survives it. This
   is the number most likely to define the verdict.

2. **Tool calling does not survive — high, and the phase's central
   question.** Either Ollama's Messages layer does not translate `tools`, or
   the 8B model emits text where a `tool_use` block was needed, or a
   malformed one. Step 1b isolates the layer from the agent; Step 6 isolates
   the model from the layer. A model that chats but cannot edit is **not a
   coding agent** — fail, however fluent.

3. **Claude Code pre-flight endpoints 404 on Ollama — medium.** An agent may
   call `count_tokens` or `models` before its first message; Step 1c asks
   Ollama directly so the failure names its path rather than surfacing as a
   generic startup error.

4. **The base URL is not honoured when a real credential is present — medium,
   and the one that would invalidate Arm B silently.** D-02 mounts the
   user's OAuth credential into every session; the agent may prefer it over
   an API-key env and go to `api.anthropic.com`, and everything would
   "work". Step 3b's negative control exists for exactly this. If it fires,
   the condition is "a session can only use a local model when the real
   credential is absent or the override is X" — and since D-02 makes the
   credential present by design, that is a **Phase-4 design question**, not
   a tweak.

5. **Session → gateway:11434 unreachable — low.** NET-05's isolation drop is
   `FORWARD`-chain; delivery to the gateway is `INPUT`. If it fires, it is a
   Nemr networking gap, recorded for Phase 4, not fixed here.

6. **Permission prompts block the non-interactive run — medium, benign.**
   `--allowedTools`, else `--dangerously-skip-permissions` in a throwaway.
   Flag names vary by Claude Code version: `claude --help` first.

7. **Streaming / `max_tokens` / stop-sequence shape mismatches — low-medium.**
   Verbatim error; attributable to the layer by Step 1a.

8. **The model completes the trivial task and nothing real — certain, and
   not a failure.** Step 4 measures the ceiling on purpose. The verdict says
   "plumbing proof, not a usable assistant" whatever Step 4 shows.

9. **The first agent turn is slow — certain, benign.** The big prompt is
   cold; Phase 2's rule applies: the second turn is the measurement.

---

## What to record — the preconditions list

Everything from Phases 1 and 2 still in force. Plus:

- The context length used, and the **largest fully-resident** one (`/api/ps`
  `size` vs `size_vram` at each setting; `nvidia-smi` memory).
- Step 1's three probe results verbatim (Messages body shape; `tool_use`
  present or not; the two HTTP codes).
- Claude Code version (`claude --version` in the image) and the exact flags
  that worked; whether `--dangerously-skip-permissions` was needed.
- The base image digest; the session name and its gateway address.
- The negative control's exit status and error.
- `ls -la` of each fresh workspace before and after; `hello.py` contents; the
  `python3` output; the `tool_use` count from the JSON.
- Ollama's log lines for the requests; `nvidia-smi` during a turn; the
  model's self-identification.
- First-turn and second-turn wall times, separately.
- Every command's exit status.

---

## The verdict

- **Feasible** — Arm B: negative control fails, the real run makes a
  file on the session volume that runs and survives a restart, Ollama's log
  and the card and the model's self-id all agree — at a context length that
  stays fully resident. **And, in the same breath: 8B on 8 GB is a plumbing
  proof, not a usable assistant.** Phase 4 may be scoped.
- **Feasible with caveats — name the condition.** "Only at 8k context, which
  is [not] fully resident"; "only with the credential absent" (Phase-4 design
  question); "only with model X"; "only Arm A — the session cannot reach
  the gateway" (Phase-4 networking item); "tool calls only via Codex". Each
  is a Phase-4 requirement or a reason to stop; say which.
- **Not feasible on this stack** — tool calling does not survive the layer
  for any model that fits the card, or the context a coding agent needs
  cannot be held resident. Say which step, the verbatim output, what was
  tried. That is a legitimate outcome, and the honest one if it is true.

State plainly what could not be verified. Arms not run, Step 4 skipped, Step
5 untried — listed, not smoothed.

---

## Recording divergences

Same template, one entry per divergence, in order encountered:

```
### <n>. <one-line title>
step:      <which step / arm, which command>
expected:  <the prediction, or the documented behaviour>
observed:  <verbatim output — paste, don't paraphrase — including exit status>
class:     ollama gap | agent gap | model limit | nemr-networking gap | our-path gap | docs gap | blocker (best guess)
```

`nemr-networking gap` and `agent gap` are the two classes that become Phase-4
items. `our-path gap` would mean the Phase 1 delta is not the whole contract
after all — Phase 2 found none; Phase 3 is the last chance to.

**Not in scope, on purpose:** any change to `setup_host.sh`, the engine, the
base image or containerd's configuration; fixing a networking or credential
gap found here; model quality beyond the pass criterion; the product's agent
choice; weights in bundles; GPU sharing.

---

## Findings before the verdict — the Phase 3 run (2026-09-04, Product Owner, on the WSL2 box)

**The plumbing passes; tool calling fails; and it is not the agent.** (Written
before Step 7 and Arm B ran; their results and the verdict follow this
section.) What was established:

**Step 0, measured.** `4096`: fully resident (5.27 GB). **`8192`: fully
resident (5.81 GB; 6868 MiB used) — the ceiling on this card.** `16384`: 87%
resident (size 7.26 GB, vram 6.31 GB), partial offload, 43.5 tok/s against
74.5. Sixteen thousand works at ~40% throughput cost. Both numbers are
Phase-4 inputs: a trade-off, not a wall.

**Step 1.** Ollama speaks Messages correctly — envelope, `stop_reason`,
`usage`. With **one trivial tool** it produced a perfect
`tool_use` (`write_file`, `{"path":"hello.txt","content":"hi"}`,
`stop_reason: tool_use`). `count_tokens` 404s (**prediction 3 fired**);
`/v1/models` 200s. `claude --bare` skips the prefetch that hits it, and
forces API-key auth — which also addresses prediction 4's risk.

**Step 2, Arm A — five combinations, one failure.** With Claude Code's
**full** toolset, every model emitted its tool call as **text**, never a
structured block, and no file was written in any run:

```
llama3.1:8b        {"name": "write_file", "parameters": {"content": "print(", ...}}
qwen2.5-coder:7b   {"name": "Write", "parameters": {"path": ..., "text": "print(", ...}}
qwen3-coder:30b    <function=Edit><parameter=file_path>/work/hello.py</parameter>...</tool_call>
```

The third is the diagnostic one: Qwen3-Coder emitted **its own native XML,
correctly**, with a chat-template `</tool_call>` marker leaking through
unconverted — the model knew what to do; nothing converted it. Changing the
agent (`codex exec --oss --local-provider ollama`, a different API path,
`/v1/chat/completions`) gave the same failure, now markdown-fenced — the model
*writing about* a tool call. `sandbox_mode=workspace-write` changed nothing.
**Three models (7B dense to 30B MoE), two agents, two API paths: the variable
is neither the agent nor the model.** Ollama's tool translation works with one
tool and stops under a real agent's schema set. Ollama 0.33.3 — not a stale
build. Class: `ollama gap`. Known upstream:
[ollama/ollama#15529](https://github.com/ollama/ollama/issues/15529).

**Prediction 2 — fired, but not where predicted.** Step 1b's clean `tool_use`
made it look dead; it was the schema *set*, not tool calling as such, that
broke it. Prediction 1 (context) was real and is now measured. Prediction 4
is addressed by `--bare` pending Arm B's negative control.

**Arm B — unrun, and it is ours rather than Ollama's.** Two Nemr questions
that Phase 4 needs regardless of tool calling: can a session reach a
rootlesskit-namespace service at its gateway (predicted yes; untested), and
does the credential negative control hold (the sharpest control in this
runbook; unexercised). Run both, even with tool calling broken.

**Divergences recorded (Phase 3 run):**

1. `ctr run` does not attach stdin, so `claude -p` waits 3 s and fails with
   `Input must be provided`; `< /dev/null` is not enough and nested quoting
   eats a prompt argument. **Fixed above:** the prompt comes from a file via
   stdin, on its own read-only bind. Class `docs gap`.
2. `CTRUN` was missing from Phase 2's definitions in a fresh terminal, and
   every terminal needed the whole block re-pasted. **Fixed:** the definitions
   now live in `docs/gpu-env.sh`, sourced once per terminal. Class `docs gap`.
3. `--rm` does not clean up after `SIGKILL`: after Phase 2's kill test the
   container record survived as `running` with a dead task; `container rm`
   refused, then reported not-found on the next call. Ambiguous, and it
   matters if Phase 4 ever manages GPU containers. Class `rootless semantics`;
   **Phase-4 item** (lifecycle of a killed GPU container).
4. Codex needs `--skip-git-repo-check` outside a repo, and hangs silently
   without it when the prompt is an argument; it fails fast with a message
   when the prompt comes from stdin. Class `agent gap`.
5. `Model metadata not found` for both Qwen models under Codex; it falls back
   to default context assumptions. Class `agent gap`.
6. Claude Code reports `"contextWindow": 200000` regardless of the server's
   actual 8192 — the agent does not know the ceiling it is running under.
   Class `agent gap`; relevant to Phase 4's context budgeting.

**The ceiling, as this runbook required:** whatever Step 7 shows, **8B on
8 GB is a plumbing proof, not a usable assistant.** Even with perfect tool
calling, models that fit this card are not coding agents. The feature is
waiting on hardware as much as on software.

## Step 7 and Arm B — run (2026-09-05, Product Owner, on the WSL2 box)

**7a.** `/usr/lib/ollama/llama-server` **is** in the Ollama image — the
runbook's expectation was wrong: Ollama ships the standalone binary alongside
its runner library. `XGRAMMAR_LICENSE` is there too, so the image carries
grammar machinery it evidently does not use on this path. The GGUF was found
through the manifest: 4.68 GB, `GGUF` magic.

**7b.** The official image `ghcr.io/ggml-org/llama.cpp:server-cuda`, `-ngl 99
-c 8192`: `offloaded 29/29 layers to GPU`, 4168 MiB model buffer on CUDA0.
Fully resident — a third server image through the same three binds and one
env var, the GPU contract unchanged again.

**7c — fails at the trivial-tool baseline, on both endpoints.**

```
/v1/messages         → text, ```xml-fenced JSON, stop_reason: end_turn
/v1/chat/completions → text, <function-call>...</function-call>, finish_reason: stop
                       62.9 tok/s — GPU speed, so not a degraded model
```

This is the case Ollama handled correctly at Step 1b. llama.cpp is *worse* at
the baseline, not better. `count_tokens` returns 200 — the runbook's
prediction held, and it decides nothing.

**Every confound eliminated, in order:**

- *Template provenance.* The source GGUF pulled directly
  (`bartowski/Qwen2.5-Coder-7B-Instruct-GGUF:Q4_K_M`) — Ollama's blob out of
  the picture. The startup log: `tokenizer.chat_template = {%- if tools %}…`
  — Qwen's real tool branch, loaded. Still text.
- *Residency.* A two-server collision produced 0.16 tok/s on one run; fixed
  and re-run at 62.9 tok/s. Still text.
- *Parser forced.* `--no-skip-chat-parsing --chat-template-kwargs
  '{"enable_thinking":false}'`. Still text — now `<function_call>` fenced as
  xml.
- *`--jinja`* was already the default in this build; passing it changed
  nothing.

**The diagnostic detail.** Qwen's native tool format is `<tool_call>`. It
never appeared once. What appeared: `<function-call>`, `<function_call>`,
fenced JSON, fenced XML — four different guesses at the wire format. The
model knows it should make a tool call and is guessing at the format, with
the template's tool branch present in the file. Recorded as the observation
the upstream report carries (`docs/gpu-upstream-issues.md`), not explained
here.

**7d was gated on 7c, and 7c failed** — the outcome table's last row. Not run
as a decisive step; nothing below a failed baseline can pass.

**Space covered, in total:** three models (7B dense → 30B MoE), two agents,
two servers, three endpoints, two GGUF sources, the parser forced. One result.
It does not get stronger with more runs.

**Arm B — both Nemr questions answered, both the right way.**

*Gateway reachability — works, no Nemr change needed:*

```
root@Rain:/workspace# GW=$(ip route | awk '/default/{print $3}'); echo $GW
10.99.0.1
root@Rain:/workspace# curl -s http://$GW:11434/api/version
{"version":"0.33.3"}
```

Prediction 5 dead, by the predicted mechanism: NET-05's isolation is a
`FORWARD` drop; delivery to the gateway address is `INPUT`. A session reaches
a service in rootlesskit's namespace at its own gateway, through NET-02, with
the isolation intact.

*The negative control held — the important one:*

```
ANTHROPIC_BASE_URL=http://127.0.0.1:1 … claude -p --bare …
→ "is_error": true, "num_turns": 0
  "Failed to connect to 127.0.0.1:1 after 0 ms: Couldn't connect to server"
```

With the real credential present (D-02) and egress available, Claude Code
**did not fall back to `api.anthropic.com`**. The override wins. Prediction 4
dead, and the Phase-4 design question it would have opened is closed: a GPU
session can carry the user's credential and point at a local model without
the two colliding.

*Arm B proper:* routed correctly — `provider: firstParty`, `canonicalModel:
qwen2.5-coder:7b`; the server log shows the request arriving from
`10.99.0.2`, the session's address. Tool calling failed identically to Arm A
(fenced JSON as text): consistent, expected, and attributable to the same
layer.

**Divergences (continued from 6):**

7. `nemr status` shows only forwarded ports — not the session's gateway
   address, the one address a session needs to reach a host-side service. It
   was discovered with `ip route` inside the session instead. Class `docs
   gap`; **Phase-4 item** (surface the gateway in `status`).
8. llama-server's Hugging Face download (7.5 min) landed in the container's
   writable layer: cached across restarts, gone on `delete`. Phase 4 puts a
   model cache on a bind, next to the weights. Class `our-path` note — a
   lifecycle choice, not a gap in the contract.
9. Step 7a's expectation was wrong: `llama-server` ships in the Ollama image.
   Class `docs gap`; harmless — the official image was used anyway, for a
   known build.

---

## Scorecard — the nine predictions, and Step 7's own

| # | Prediction | Outcome |
|---|---|---|
| 1 | Context is the wall — near-certain at 4096, likely still at 8192 | **Real, measured, and a trade-off rather than a wall.** 8192 fully resident (5.81 GB, 6868 MiB used); 16384 at 87% residency and ~40% throughput cost. Both numbers are Phase-4 inputs. |
| 2 | Tool calling does not survive — the phase's central question | **Fired, but not where predicted.** Step 1b's clean `tool_use` looked like a pass; the break is the schema *set* on Ollama, and the trivial baseline itself on llama.cpp. Not the agent, not the model. |
| 3 | Claude Code pre-flight endpoints 404 on Ollama | **Fired**: `count_tokens` 404. Sidestepped by `--bare`; llama-server returns 200. Either way it decides nothing. |
| 4 | The base URL is not honoured when a real credential is present | **Dead — the sharpest control in the runbook held.** The override wins; the Phase-4 design question is closed. |
| 5 | Session → gateway unreachable | **Dead.** `10.99.0.1:11434` answers from inside a session; `INPUT`, not `FORWARD`, as predicted. |
| 6 | Permission prompts block the non-interactive run | **Not exercised.** No structured tool call ever reached the permission layer; `--allowedTools` was passed and never tested. |
| 7 | Streaming / `max_tokens` / stop-sequence shape mismatches | **Dead.** Both servers produced a correct Messages envelope; no shape error in any run. |
| 8 | The model completes the trivial task and nothing real | **Moot.** The trivial task itself never completed; the ceiling sentence stands regardless. |
| 9 | The first agent turn is slow | **Unscored** — first- and second-turn wall times were not recorded separately. Nothing in the outcome depended on it. |
| 7a | `llama-server` is not in the Ollama image | **Wrong.** It is (`/usr/lib/ollama/llama-server`). |
| 7c | llama-server answers `count_tokens` with 200 | Held. |
| 7c | llama-server with `--jinja` returns `tool_use` for one trivial tool | **Wrong** — text on both endpoints, with every confound removed. |
| 7d | (gated on 7c) | Not reached. |

Three real (1, 2, 3), three dead (4, 5, 7), three never exercised (6, 8, 9).
Of Step 7's, one held and the two that mattered were wrong. Phase 2's pattern
repeated: the ones that mattered went against expectation — the safe-looking
baseline (7c) is where it broke, and the frightening ones (4, 5) were dead.

---

## Verdict — written after Step 7 and Arm B (2026-09-05)

**Feasible with caveats. The caveat is precise, well-evidenced, and outside
the project.**

**Proven, by hand, on our containerd, rootless, nothing built:**

- The GPU reaches a rootless container with three bind mounts and one env
  var — Phase 1's Arm C delta, unchanged through Phase 2 (Ollama), Phase 3
  (a session) and Step 7 (a third server image). That delta is the whole GPU
  contract, and it fits today's `ContainerSpec` (mounts + env; no device
  entries, hooks or cgroup rules).
- An 8B model runs fully resident at 74 tok/s with 8192 context; 16384 is
  available at 87% residency and ~40% throughput cost.
- Both compatibility layers are reachable: Ollama's Anthropic Messages API
  (correct envelope, one-tool `tool_use` correct) and llama-server's
  (`count_tokens` 200).
- A real Nemr session reaches the model at its gateway (`10.99.N.1`) with no
  networking change, NET-05 intact.
- `ANTHROPIC_BASE_URL` wins over the mounted credential: the negative control
  failed to connect rather than reaching Anthropic. A GPU session can carry
  the user's credential and use a local model without the two colliding.

**Blocked:** tool calling under a real agent's toolset, across every
combination available — three models (7B dense to 30B MoE), two agents, two
servers, three endpoints, two GGUF sources, the parser forced. The models
produce correct calls; no layer converts them into structured blocks. On
Ollama this is documented upstream
([ollama/ollama#15529](https://github.com/ollama/ollama/issues/15529), closed
stale, unfixed on 0.33.3) and fires under a multi-tool schema set; on
llama.cpp's own server it fires at the trivial single-tool baseline for this
GGUF, with the template's tool branch loaded. Both are drafted as upstream
reports in `docs/gpu-upstream-issues.md`, evidence-first, for the Product
Owner to file. No pass criterion was met: no file ever appeared in an empty
directory.

**The ceiling, as this runbook required before the first run:** **8B on 8 GB
is a plumbing proof, not a usable assistant.** Even with tool calling fixed,
the models that fit this card are not coding agents. The feature waits on
hardware as much as on software.

**Phase 4 is deferred, not cancelled** — recorded as E-18 in
`docs/DECISIONS.md` with the settled inputs listed so they are not re-tested.
The delta is known and small. It reopens when tool calling works through a
server we would run as a dependency, verified by re-running 7c then 7d of
this runbook, and it is worth shipping when there is a card that runs a model
that matters. The proxy stays a fallback on paper, not a plan: the ones that
"fix" this are per-model text parsers, the class this project eliminates.

**Not verified, listed not smoothed:**

- Step 4 (read-edit-run) was never reached: no trivial edit completed.
- Step 5 ran only against Ollama; Codex against llama-server (`:8080`) was
  not attempted — 7c gated it.
- 7d was not run as a decisive step (gated on 7c).
- Arm B's stop/start persistence check could not be measured: no file was
  ever written to persist.
- Prediction 9's wall times were not recorded.
- Two identifiers are absent from the run report and are placeholders in the
  upstream drafts: the llama.cpp image digest / `llama-server --version`, and
  the Claude Code version in the base image.

**Preconditions, in force for any re-run:** everything from Phases 1 and 2
(driver store discovered, never hardcoded; the Arm C delta; `CTRUN` for every
run; the three-signal residency check; the warmup rule). Plus: Ollama 0.33.3;
`ghcr.io/ggml-org/llama.cpp:server-cuda` (digest to record); models
`llama3.1:8b`, `qwen2.5-coder:7b`, `qwen3-coder:30b`, and the source GGUF
`bartowski/Qwen2.5-Coder-7B-Instruct-GGUF:Q4_K_M`; context 8192; `claude
--bare -p` with `--allowedTools`; the prompt from a file via stdin; the
session's gateway from `ip route` inside it; the negative control's ~3-minute
retry before it fails.

**What Phase 4 inherits, settled:** the GPU contract (three binds + one env);
driver-store discovery at start; the 8192 / 16384 trade-off; the gateway as
the session's route to a host-side service, and that `nemr status` should
show it; the base-URL override holding with the credential present; the
model cache on a bind; the lifecycle of a killed GPU container (`--rm` does
not clean up after SIGKILL); `--bare` for the pre-flight; the agent not
knowing its real context window. And what it does **not** need: no
`nvidia-container-toolkit`, no third-party apt repo, no CDI — the D-13-class
question the Phase 1 runbook flagged never arises, because bind mounts and
env are the whole contract.
