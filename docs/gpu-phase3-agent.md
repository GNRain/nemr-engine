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

The same definitions as Phase 2 (`CTR`, `CTRUN`, `NS`, `DRV`, `GPU`), unchanged
— copy them from `docs/gpu-phase2-ollama.md`, including the self-check grep
that must print nothing. Then the Phase 2 server, in terminal 1, with **one
addition**, explained in Step 0:

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
BASE=ghcr.io/gnrain/nemr-base:0.3.0          # already in containerd; record the digest
rm -rf ~/gpu-spike/work-a && mkdir -p ~/gpu-spike/work-a   # a FRESH, EMPTY workspace: a file appearing in it is proof a tool ran
CTRUN agent-a --rm --net-host \
  --mount "type=bind,src=$HOME/gpu-spike/work-a,dst=/work,options=rbind:rw" \
  --env ANTHROPIC_BASE_URL=http://127.0.0.1:11434 \
  --env ANTHROPIC_API_KEY=ollama \
  --env ANTHROPIC_MODEL="$MODEL" --env ANTHROPIC_SMALL_FAST_MODEL="$MODEL" \
  --env DISABLE_TELEMETRY=1 --env DISABLE_AUTOUPDATER=1 --env CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
  --cwd /work \
  "$BASE" agent-a claude -p --output-format json \
    --allowedTools "Write,Read,Bash" \
    "Create a file named hello.py in the current directory containing a Python program that prints exactly: hello from ollama. Use your file-writing tool, then stop." \
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
# 3b. the NEGATIVE control first: a base URL that cannot answer. This MUST fail.
CTR task exec --exec-id neg -t "$SESS" env \
  ANTHROPIC_BASE_URL="http://$GW:1" ANTHROPIC_API_KEY=ollama ANTHROPIC_MODEL="$MODEL" \
  claude -p "Reply with OK." ; echo "exit=$?"
```

If the negative control **succeeds**, Claude Code ignored the base URL and
used the real credential D-02 mounts into every session — and everything
that follows would prove nothing. That is a finding of the first order:
record it, class `agent gap`, and see prediction 4 for what it means.

```bash
# 3c. the real run — into the session's /workspace (the project volume), through the gateway
CTR task exec --exec-id agent -t "$SESS" env \
  ANTHROPIC_BASE_URL="http://$GW:11434" ANTHROPIC_API_KEY=ollama \
  ANTHROPIC_MODEL="$MODEL" ANTHROPIC_SMALL_FAST_MODEL="$MODEL" \
  DISABLE_TELEMETRY=1 DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
  sh -c 'cd /workspace && claude -p --output-format json --allowedTools "Write,Read,Bash" \
    "Create a file named hello.py in the current directory containing a Python program that prints exactly: hello from ollama. Use your file-writing tool, then stop."' \
  2>&1 | tee ~/gpu-spike/phase3-arm-b.json

# 3d. which model answered — ask it, and watch the card
CTR task exec --exec-id who -t "$SESS" env ANTHROPIC_BASE_URL="http://$GW:11434" ANTHROPIC_API_KEY=ollama ANTHROPIC_MODEL="$MODEL" \
  claude -p "Which model are you, and who made you? One line."
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
file-exists, file-runs pass criterion. Codex's own auth is a separate open
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
