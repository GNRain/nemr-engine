# GPU Phase 2 — Ollama in a container, by hand

Phase 1 (`docs/gpu-spike.md`) answered its one question: a rootless container
on our own containerd can use the GPU, through our own path, with **bind
mounts and env only**. Phase 2 asks the next one, and only the next one:

> **Can an inference server run in such a container, load a model that fits
> the card, and answer a request — with the work done on the GPU, not the
> CPU?**

Same discipline. **Build nothing**: no `setup_host.sh`, engine, base-image or
containerd-config change. Everything is done by hand with `ctr`, exactly as
Phase 1 was, and recorded. "Not feasible" and "feasible only under a
condition" remain acceptable verdicts. Phase 3 (an agent talking to it) also
touches nothing in Nemr; Phase 4 is the first engine work, and only once the
whole stack is proven by hand.

**Not in scope now:** which agent; model *quality* (any model that fits is a
plumbing choice, not a product one); how weights are stored or travel in a
bundle; sharing one GPU across sessions; **reaching the server from the host
or from another container** — that is Phase 3's question, and Nemr's answer
to it already exists (NET-02 session networking + `nemr port`). Phase 2 tests
the server from *inside* the namespace it lives in, on purpose.

**Costs, unchanged and stated:** nothing here will ever have CI coverage
(hosted runners have no GPU). The Ollama image is pulled from a registry by
`ctr` — no Docker Engine (NFR-01). No new third-party apt repos are needed for
this phase; Phase 1's hand-installed toolkit is not needed either (Phase 1
proved the delta works without it).

**The trap this phase exists to catch:** a completion that comes back proves
the *server* works. It does not prove the *GPU* did the work — Ollama falls
back to CPU silently, and an 8B model on CPU still answers, slowly. So the pass
criterion is not "it answered"; it is "it answered **and** three independent
signals say the GPU did it". Presence versus continuity, one more time.

**Record everything with `tee`:**

```bash
mkdir -p ~/gpu-spike/ollama-models && cd ~/gpu-spike
# prefix every command block below with:  2>&1 | tee -a ~/gpu-spike/phase2-<n>.log
```

---

## The delta from Phase 1, verbatim — define it once

Phase 1's Arm C delta, plus the `nsenter` wrapper and the hashed driver-store
discovery its verdict recorded. Two shell functions, used everywhere below:

```bash
export CONTAINERD_ADDRESS="${XDG_RUNTIME_DIR}/containerd/containerd.sock"
CHILD_PID=$(cat "$XDG_RUNTIME_DIR/containerd-rootless/child_pid")

# ctr, inside rootlesskit's mount + network namespaces (bare ctr run fails on the overlay mount)
CTR()   { nsenter -U --preserve-credentials -m -n -t "$CHILD_PID" \
            env CONTAINERD_ADDRESS=/run/containerd/containerd.sock ctr -n default "$@"; }
# EVERY container run: the same entry PLUS the rootless cgroup flags — rootless containerd
# cannot create the default cgroup (`mkdir /sys/fs/cgroup/default: permission denied`,
# docs/ENGINEERING.md); --runc-systemd-cgroup REQUIRES --cgroup. Tag = container id, so
# the long-lived server and a concurrent diagnostic never share a scope.
CTRUN() { local tag="$1"; shift; CTR run --runc-systemd-cgroup --cgroup "user.slice:nemr:gpu-$tag" "$@"; }
# Self-check: every container run goes through CTRUN — this must print NOTHING. It
# matches a run at command position (line start) only, so the CTRUN definition and
# prose do not false-positive; the `--help` probe is excluded (it mounts nothing):
grep -nE '^\s*(ctr( -n default)? |CTR )run ' docs/gpu-*.md | grep -v -- '--help'
# a plain command inside rootlesskit's NETWORK namespace only (for curl-ing the server)
NS()  { nsenter -U --preserve-credentials -n -t "$CHILD_PID" "$@"; }

# the driver store is hashed and changes on every Windows driver update — discover, never hardcode
DRV=$(ls -d /usr/lib/wsl/drivers/nv_dispi.inf_amd64_* | head -1); echo "driver store: $DRV"

# Phase 1's delta, as ctr flags. Nothing else.
GPU=(
  --mount "type=bind,src=/dev/dxg,dst=/dev/dxg,options=rbind:rw"
  --mount "type=bind,src=/usr/lib/wsl/lib,dst=/usr/lib/wsl/lib,options=rbind:ro"
  --mount "type=bind,src=$DRV,dst=$DRV,options=rbind:ro"
  --env   "LD_LIBRARY_PATH=/usr/lib/wsl/lib:$DRV"
)
```

Re-check Phase 1's Step 1 first (`/usr/lib/wsl/lib/nvidia-smi`), and record
the driver version again: if the Windows driver changed since Phase 1, the
hash changed with it and every Phase-1 precondition needs re-recording.

---

## Step 1 — the image and the model store

```bash
OLL=docker.io/ollama/ollama:latest      # record the resolved digest AND the Ollama version it reports
ctr -n default images pull --platform linux/amd64 "$OLL"       # pure gRPC — no wrapper needed for a pull
ctr -n default images ls | grep ollama                          # the digest
df -h ~ /                                                       # ~2 GB image + ~5 GB model on the VM's disk
```

The model store is a **host directory bind-mounted over the image's
`/root/.ollama`**, so a 5 GB model is pulled once and survives container
restarts — and so Step 6 can prove that it does. Rootless uid mapping makes the
host user the container's root, so the bind is writable without any chown.
Where weights *belong* is a Phase-4+ question; a directory under
`~/gpu-spike` is the by-hand answer and nothing more.

**Gate:** the pull lands at a recorded digest and there is disk for the model.

---

## Step 2 — start the server, and read its GPU discovery before anything else

Two terminals. Terminal 1 runs the server **in the foreground** so its startup
log — the first GPU proof — is visible and `tee`'d (`ctr run -d` would swallow
it). `--net-host` places the container in rootlesskit's network namespace,
which has egress through slirp4netns (that is how it will pull a model) and is
exactly the namespace `NS` enters.

```bash
# terminal 1
CTRUN srv --rm --net-host "${GPU[@]}" \
  --mount "type=bind,src=$HOME/gpu-spike/ollama-models,dst=/root/.ollama,options=rbind:rw" \
  --env OLLAMA_HOST=0.0.0.0:11434 \
  "$OLL" ollama-srv ollama serve 2>&1 | tee ~/gpu-spike/phase2-serve.log
```

Before pulling anything, **read the startup log.** Ollama enumerates compute
at start and says what it found. Record the lines verbatim. The two shapes:

```
inference compute ... library=cuda ... name="NVIDIA GeForce RTX 3070" ... total="8.0 GiB"   ← the GPU
inference compute ... library=cpu ...                                                      ← the trap
```

**Gate:** `library=cuda` naming the 3070 with its VRAM. If the log says `cpu`
— or shows no compute line at all — **stop here and diagnose before pulling a
5 GB model**: the server will still work, on the CPU, and every later step
would pass for the wrong reason. Diagnose with Phase 1's tools: does
`$DRV/nvidia-smi` work in *this* image with *these* flags (`CTRUN chk --rm
"${GPU[@]}" "$OLL" chk "$DRV/nvidia-smi"`)? Does Ollama's discovery honour
`LD_LIBRARY_PATH`, or does it search its own list (`OLLAMA_DEBUG=1` makes it
say which libraries it tried)? Record the answer as a divergence, class
`our-path gap` — it would be the first thing the Arm C delta does not cover.

---

## Step 3 — pull a model that fits the card

From terminal 2, **inside the same network namespace** — from the WSL2 host
shell, `localhost:11434` is *not* reachable, by design of a rootless network
namespace; that is expected, not a failure, and it is Phase 3's problem:

```bash
# terminal 2
NS curl -s localhost:11434/api/version                     # proves the server is reachable in-namespace
MODEL=llama3.1:8b        # ~4.7 GB at Q4_K_M — a PLUMBING choice; any ~4–5 GB 7B–8B quantised model will do
NS curl -s localhost:11434/api/pull -d "{\"name\":\"$MODEL\"}" | tail -3
NS curl -s localhost:11434/api/tags | python3 -m json.tool | grep -E '"name"|"size"'   # record name, digest, size
ls -la ~/gpu-spike/ollama-models/models/blobs | head       # the weights landed on the host bind
```

8 GB is enough for a ~5 GB model plus its KV cache and the CUDA context at the
default context length; it is *not* enough for a 13B, and a model that does
not fit is offloaded partially — see Step 4. Do not pick a bigger model to
"see what happens"; that is a different experiment.

**Gate:** the pull completes with egress from inside rootlesskit's namespace,
and the blobs are on the host bind. A pull that fails is an egress/DNS
divergence in rootlesskit's namespace (record `NS cat /etc/resolv.conf` and
`NS curl -sI https://ollama.com | head -1`), not a GPU finding.

---

## Step 4 — the decisive check: inference on the GPU, not the CPU

Ask for something with a checkable answer, at temperature 0, non-streaming so
the response carries its timing fields:

```bash
NS curl -s localhost:11434/api/generate -d "{
  \"model\": \"$MODEL\",
  \"prompt\": \"What is 17 * 23? Reply with the number only.\",
  \"stream\": false,
  \"options\": {\"temperature\": 0}
}" | tee ~/gpu-spike/phase2-generate.json | python3 -c '
import json,sys; r=json.load(sys.stdin)
print("response:", r["response"].strip())
print("eval tokens/s: %.1f" % (r["eval_count"] / (r["eval_duration"] / 1e9)))
print("load ms: %d  prompt tok/s: %.1f" % (r["load_duration"]/1e6, r["prompt_eval_count"]/(r["prompt_eval_duration"]/1e9)))'
```

`391` is the right answer, but the arithmetic is a sanity check, not the pass
criterion — models get sums wrong. The pass is **a coherent completion, and
three independent signals that the GPU produced it**:

1. **Ollama's own accounting:**
   ```bash
   NS curl -s localhost:11434/api/ps | python3 -m json.tool | grep -E '"name"|size_vram|"size"'
   ```
   `size_vram` equal to `size` means the whole model is resident on the GPU
   (`ollama ps` renders this as `100% GPU`). Less than that is **partial
   offload** — record the fraction; it is a caveat, not a pass.
2. **The server log at model load** (terminal 1): the runner reports how many
   layers it offloaded — the shape is `offloaded N/N layers to GPU` (all of
   them) versus `N/M` (some) versus nothing (CPU). Paste the line.
3. **The card itself, from the WSL2 host, while the model is loaded:**
   ```bash
   /usr/lib/wsl/lib/nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu --format=csv
   ```
   Memory used should have jumped by roughly the model's size after load,
   and utilisation should spike during a generation. (Ollama keeps a model
   loaded ~5 minutes after last use, so run this promptly.)

Then the number that discriminates hardest: **eval tokens/s.** An 8B Q4 model
on this CPU is single digits to low teens; on the 3070, tens. If the tok/s
looks like a CPU, believe the number over the log. Record it, and record that
it is the *paravirtualised* path — expect it below a bare-metal 3070, and note
the gap as a finding, not a failure.

**Gate:** coherent completion **and** signals 1–3 agree on full GPU residency.
Any disagreement is the finding — write down which signal dissented.

---

## Step 5 — a second request, and a longer one

**The first generation is warmup; the second is the measurement.** The first
request pays the model load *and* a cold prompt-eval that Phase 2 measured at
**58× slower** than warm (1.43 tok/s cold, 83.1 warm) — anyone measuring once
would file a false finding about the paravirtualised path. Never record a
throughput number from a single request. Send the same prompt again (load
should be ~0 ms; that is the number to keep), then something that generates a few hundred
tokens — `"Write a 200-word explanation of what a mount namespace is."` — and
watch signal 3 during it. This is where a partial offload or a VRAM ceiling
shows up as a stall or an OOM in terminal 1. Record tok/s for the long one too.

---

## Step 6 — stop, restart, and the store survives

```bash
# terminal 1: Ctrl-C the server (the --rm container is removed)
/usr/lib/wsl/lib/nvidia-smi --query-gpu=memory.used --format=csv    # VRAM released? record the number
# restart with the SAME model-store bind (the Step 2 command), then, in terminal 2:
NS curl -s localhost:11434/api/tags | grep -c "$MODEL"     # 1: present, no re-pull
```

Then a third check that costs nothing and answers a Phase-4 question early:
**does the container's VRAM get released when the container dies uncleanly?**
Start the server, load the model (one generation), then from another shell
`CTR task kill -s SIGKILL ollama-srv`, and read `nvidia-smi` again. A card
that keeps the memory of a dead container is a finding for GPU sharing later.

**Gate:** model present without re-pull; VRAM returns to baseline after a
clean stop *and* after a SIGKILL.

---

## Predictions — what we expect to break, and why

Ranked. Held loosely — Phase 1's mattered where they were wrong, and the value
was the template being ready.

1. **CPU fallback masquerading as success — the most consequential, medium
   likelihood.** Ollama's GPU discovery `dlopen`s `libcuda`/`libnvidia-ml`
   from its own search list; whether it honours `LD_LIBRARY_PATH` for the
   WSL projection (it has explicit WSL2 handling that looks in
   `/usr/lib/wsl/lib`) decides Step 2. If it reports `library=cpu`, every
   later step still "works". That is why Step 2 gates on the log before the
   pull, and why Step 4 demands three agreeing signals.

2. **Egress for the model pull from rootlesskit's namespace — medium-low.**
   slirp4netns gives egress (our sessions get out the same way, one hop
   further). The risk is DNS: WSL's generated `resolv.conf` copied-up into
   rootlesskit's `/etc`, versus slirp4netns's own resolver. Fires as a pull
   that cannot resolve `ollama.com`; a network divergence, not a GPU one.

3. **`localhost:11434` unreachable from the WSL2 host shell — certain, by
   design, benign.** A rootless network namespace is not the host's. Test
   from inside with `NS`; reaching it from outside is Phase 3, and NET-02 +
   `nemr port` is how Nemr would do it. A "connection refused" from the host
   is expected — record it once and move on.

4. **Partial offload on 8 GB — low with a ~5 GB Q4 model, certain with a
   bigger one.** Ollama offloads as many layers as fit; the rest run on CPU
   and the tok/s collapses. Signals 1 and 2 catch it. The condition, if it
   fires, is "model ≤ ~5 GB at default context" — a Phase-2 fact about this
   card, not a defect.

5. **Managed memory / the paravirtualised path costs throughput — near
   certain, a finding not a failure.** Phase 1 proved `cudaMallocManaged`
   works; it did not measure speed. Expect tok/s below bare-metal 3070
   numbers. Record it; Phase 4 will want the number.

6. **The Ollama image's bundled CUDA runtime vs the Windows driver — low.**
   The image ships its own CUDA runtime libraries; the driver must support
   that version (Phase 1 Step 1's number). A mismatch says so verbatim
   (`driver version is insufficient`), and the fix is a driver update — a
   host precondition, recorded.

7. **The hashed driver store, again — certain over time.** Ollama's runner
   needs the driver's dependent libraries, which is why the third bind exists.
   After a Windows driver update the hash changes; `DRV` discovery handles it
   here, and Phase 4 must discover it at start, never hardcode it.

8. **`--net-host` under rootless is not "the host" — benign, but easy to
   misread.** It is rootlesskit's namespace. Nothing binds to the WSL2 host's
   interfaces; nothing is exposed to Windows. Worth stating so nobody reads
   `0.0.0.0:11434` as a security question in this phase.

9. **VRAM not released after a SIGKILL — low, and exactly the kind of thing
   worth measuring now.** The `dxg` path frees on process exit like bare
   metal is expected to. If it does not, GPU sharing across sessions (out of
   scope now) has a hard constraint, and finding it in Step 6 costs nothing.

---

## What to record — the preconditions list

Everything from Phase 1's list is still in force (driver version **and date**,
`wsl --version`, kernel, `ctr`/`runc` versions, `findmnt -no PROPAGATION /`),
re-recorded if the driver changed. Plus:

- Ollama image: tag, **digest**, and the version the server logs at start.
- The model: name, digest, size on disk; the exact `MODEL` string.
- The server's startup compute line(s), verbatim; the layer-offload line at
  load, verbatim.
- `/api/ps` output (`size` vs `size_vram`); `nvidia-smi` memory before load,
  after load, during generation, after clean stop, after SIGKILL.
- **eval tokens/s** for the short and the long generation; load ms.
- Disk used by image + model; the model-store path.
- `NS cat /etc/resolv.conf` (what resolver the namespace actually used).
- Every command's exit status.

---

## The verdict

- **Feasible** — `library=cuda` at start, full residency (signals 1–3 agree),
  a coherent completion at GPU-class tok/s, the store survives a restart, VRAM
  releases on stop and on SIGKILL. Phase 3 may begin.
- **Feasible with caveats — name the condition.** "Only with partial
  offload at this model size"; "only if `LD_LIBRARY_PATH` is supplemented by
  X"; "VRAM is not released on SIGKILL"; "egress needs resolver Y". Each is a
  Phase-4 requirement or a reason to stop; say which.
- **Not feasible on this stack** — the step, the verbatim output, what was
  tried, what would have to change. CPU-only inference is **not feasible** for
  this line of work, whatever the completion said.

State plainly what could not be verified. Steps skipped, signals unavailable,
the long generation not run — listed, not smoothed.

---

## Recording divergences

Same template, same classes, one entry per divergence, in order encountered:

```
### <n>. <one-line title>
step:      <which step, which command>
expected:  <the prediction, or the documented behaviour>
observed:  <verbatim output — paste, don't paraphrase — including exit status>
class:     WSL2 semantics | rootless semantics | ollama gap | our-path gap | docs gap | blocker (best guess)
```

`our-path gap` still means: something needed beyond Phase 1's Arm C delta.
If Phase 2 finds none, the delta is confirmed as the whole GPU contract, and
Phase 4 inherits it unchanged.

**Not in scope, on purpose:** any change to `setup_host.sh`, the engine, the
base image or containerd's configuration; model quality; agent choice; weights
in bundles; GPU sharing; reaching the server from the host or from another
container (Phase 3).

---

## Verdict — Phase 2: PASSES on every gate (2026-09-04, Product Owner, on the WSL2 box)

An 8B model runs on the GPU, in a rootless container, on our own containerd,
**through Phase 1's delta unchanged**. No `our-path gap` was found: Ollama
located the GPU through the bind mounts and `LD_LIBRARY_PATH` alone. **Phase
1's Arm C delta is confirmed as the whole GPU contract.**

**Step 2, the gate, verbatim:**

```
msg="inference compute" id=0 library=CUDA compute=8.6 name=CUDA0
  description="NVIDIA GeForce RTX 3070" libdirs=ollama,cuda_v13 driver=13.3
  type=discrete total="8.0 GiB" available="6.9 GiB"
msg="vram-based default context" default_num_ctx=4096
```

`available="6.9 GiB"` — Windows holds ~1.1 GiB, so **6.9 GiB is the real
budget**, and Ollama sized its default context from it.

**Step 3:** pull succeeded from inside rootlesskit's namespace
(`{"status":"success"}`) — egress and DNS both work there. `llama3.1:8b`,
4,920,753,328 bytes, blobs on the host bind.

**Step 4, all three signals, no dissent:** response `391`; `/api/ps`
`size` = `size_vram` = 5271715839 (full residency); server log `offloaded
33/33 layers to GPU`; `nvidia-smi` 6477 MiB used of 8192 (~5.3 GiB arrived
against a ~1.1 GiB baseline). Cold: eval 123.7 tok/s, load 42.9 s, prompt
eval 1.4 tok/s.

**Step 5, warm — and it changes the picture:** load 4 ms, prompt 83.1 tok/s,
**eval 74.5 tok/s**. GPU-class by any reading.

**Step 6:** model present after restart without re-pull. SIGKILL: 6507 MiB
before → 1304 MiB after. **VRAM is released on an unclean death** — no
constraint on GPU sharing from that direction, measured now rather than at
Phase 4.

**Scorecard.** #1 (CPU fallback) dead. #2 (egress/DNS) dead. #3 as designed.
#5 (paravirtualised cost) **wrong** — the path costs little. #9 (VRAM on
SIGKILL) dead. Six of nine dead or benign; the two that fired were neither
predicted nor GPU problems. The template earned its keep again; the
predictions did not.

**Two findings the predictions did not cover:**

1. **`disabling mmap for llama-server load due to host memory pressure`** —
   `system_total="7.7 GiB"`, `model_size="4.6 GiB"`, so the loader read the
   whole file instead of mapping it: that is the 41-second cold load. A **host
   RAM** finding, not a GPU one; WSL2's default memory allocation sets it and
   `.wslconfig` `memory=` is the lever. Phase 4 should know cold start scales
   with host RAM.
2. **Cold vs warm prompt eval differ by 58×** (1.43 → 83.1 tok/s). A single
   measurement files a false finding about the paravirtualised path. Folded
   into Step 5 as a rule: the first generation is warmup; the second is the
   measurement.

**One divergence, in this runbook — class `docs gap`, fixed above:** the `CTR`
wrapper carried Phase 1's `nsenter` fix but **not** `--runc-systemd-cgroup
--cgroup`, so the first run failed with `mkdir /sys/fs/cgroup/default:
permission denied` — the error docs/ENGINEERING.md documents. Same shape as
Phase 1's two gaps: a fix known elsewhere in the repo, not carried into the
runbook. Fixed structurally in both runbooks: every container run is `CTRUN`,
which folds in both, and the self-check grep at the top verifies it
mechanically rather than by eye.

Phase 3 — a coding agent pointed at this server — is `docs/gpu-phase3-agent.md`.
