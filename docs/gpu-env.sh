# docs/gpu-env.sh — the by-hand GPU spike's shell definitions, in ONE sourceable file.
#
#   source ~/src/nemr-engine/docs/gpu-env.sh     # in EVERY new terminal
#
# Phase 3 found that each new terminal needed the whole definition block re-pasted,
# and that CTRUN had gone missing from one of them — three terminals is friction the
# runbooks were creating. This file is the single source; the runbooks reference it.
# It is a spike helper for docs/gpu-*.md and nothing else: not product code, not
# something setup_host.sh installs (build nothing — Phase 4 is the first engine work).
#
# Every definition here is the one the runbooks proved, with its reason kept:

export CONTAINERD_ADDRESS="${XDG_RUNTIME_DIR}/containerd/containerd.sock"
CHILD_PID=$(cat "$XDG_RUNTIME_DIR/containerd-rootless/child_pid" 2>/dev/null)
[[ -n "$CHILD_PID" ]] || echo "gpu-env: rootlesskit child_pid not found — is containerd-rootless running?" >&2

# ctr inside rootlesskit's mount + network namespaces — for pulls, task ops, anything
# but `run`. A bare `ctr run` fails on the overlay mount (PREREQUISITES.md, "When
# nsenter IS required"); Phase 1 lost two cycles to this line being missing.
CTR()   { nsenter -U --preserve-credentials -m -n -t "$CHILD_PID" \
            env CONTAINERD_ADDRESS=/run/containerd/containerd.sock ctr -n default "$@"; }

# EVERY container run: the same entry PLUS the rootless cgroup flags. Rootless containerd
# cannot create the default cgroup (`mkdir /sys/fs/cgroup/default: permission denied`,
# docs/ENGINEERING.md:334); --runc-systemd-cgroup REQUIRES --cgroup. The tag names the
# scope — use the container id, so a long-lived server and a concurrent diagnostic never
# share one. Phase 2 lost a cycle to these flags being missing from CTR.
#   CTRUN <tag> [ctr run flags] IMAGE ID [CMD...]
CTRUN() { local tag="$1"; shift; CTR run --runc-systemd-cgroup --cgroup "user.slice:nemr:gpu-$tag" "$@"; }

# A plain command inside rootlesskit's NETWORK namespace only — for curl-ing a server
# that lives there. localhost:<port> is NOT reachable from the WSL2 host shell, by design.
NS()    { nsenter -U --preserve-credentials -n -t "$CHILD_PID" "$@"; }

# The NVIDIA driver store is a HASHED directory whose name changes on every Windows
# driver update. Discover it — never hardcode it (Phase 4 must do the same).
DRV=$(ls -d /usr/lib/wsl/drivers/nv_dispi.inf_amd64_* 2>/dev/null | head -1)
[[ -n "$DRV" ]] && echo "gpu-env: driver store $DRV" || echo "gpu-env: no WSL driver store found (not WSL2, or no NVIDIA driver)" >&2

# Phase 1's Arm C delta, as ctr flags — confirmed by Phase 2 as the WHOLE GPU contract.
# Nothing else: no device entries, no hooks, no cgroup rules.
GPU=(
  --mount "type=bind,src=/dev/dxg,dst=/dev/dxg,options=rbind:rw"
  --mount "type=bind,src=/usr/lib/wsl/lib,dst=/usr/lib/wsl/lib,options=rbind:ro"
  --mount "type=bind,src=$DRV,dst=$DRV,options=rbind:ro"
  --env   "LD_LIBRARY_PATH=/usr/lib/wsl/lib:$DRV"
)

# Images and the model, as the runbooks use them. Plumbing choices, not product ones.
OLL=docker.io/ollama/ollama:latest
LCPP=ghcr.io/ggml-org/llama.cpp:server-cuda
BASE=ghcr.io/gnrain/nemr-base:0.3.0
MODEL="${MODEL:-llama3.1:8b}"
MODELS_DIR="$HOME/gpu-spike/ollama-models"

# The agent commands — ONE shape for both agents, and the prompt ALWAYS comes from a
# FILE via stdin. Phase 3 divergence 1: `ctr run` attaches no stdin, so `claude -p` waits
# 3 s and fails with "Input must be provided"; `< /dev/null` is not enough, and nested
# quoting eats a prompt argument. Divergence 4: Codex hangs silently with an argument
# prompt outside a repo and fails fast from stdin. Each function PRINTS the command that
# runs inside the container — hand it to `sh -c`. The prompt file must already be
# visible in the container: Arm A binds ~/gpu-spike/prompts at /prompts read-only, Arm B
# copies it in with `task exec … cat > /tmp/prompts/…`.
#   sh -c "$(CLAUDE_TASK /work /prompts/task1.txt)"
#   sh -c "$(CODEX_TASK  /work /prompts/task1.txt)"
CLAUDE_TASK() { printf 'cd %q && claude --bare -p --output-format json --allowedTools "Write,Read,Bash" < %q' "$1" "$2"; }
CODEX_TASK()  { printf 'cd %q && codex exec --oss --skip-git-repo-check --local-provider ollama -m %q < %q' "$1" "$MODEL" "$2"; }

# Everything Claude Code needs to be pointed at a local server EXCEPT the base URL — the
# one value that differs per arm (127.0.0.1:11434 in Arm A, $GW:11434 in Arm B, :8080 for
# llama-server), so it is passed next to these, never inside them. `--bare` forces
# ANTHROPIC_API_KEY auth and skips the count_tokens prefetch Ollama 404s on. Two
# spellings for the two entry points:
#   ctr run:    "${AGENT_ENV[@]/#/--env=}" --env=ANTHROPIC_BASE_URL=http://127.0.0.1:11434
#   task exec:  env "${AGENT_ENV[@]}" ANTHROPIC_BASE_URL="http://$GW:11434"
# MODEL is read when this file is sourced — re-source after changing it.
AGENT_ENV=(
  ANTHROPIC_API_KEY=ollama ANTHROPIC_MODEL="$MODEL" ANTHROPIC_SMALL_FAST_MODEL="$MODEL"
  DISABLE_TELEMETRY=1 DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
)

# Self-check: every container run in the runbooks goes through CTRUN. This must print
# NOTHING. It matches a run at command position (line start) only, so the CTRUN
# definition and prose cannot false-positive; the `--help` probe is excluded (it mounts
# nothing). Runs against the docs next to this file, wherever it is sourced from.
grep -nE '^\s*(ctr( -n default)? |CTR )run ' "$(dirname "${BASH_SOURCE[0]}")"/gpu-*.md | grep -v -- '--help'
