#!/usr/bin/env bash
#
# NFR-01 enforcement: fail the build if a Docker dependency, socket path, or CLI
# invocation reappears anywhere in the tree.
#
# NFR-01 is release-blocking, but until now it was defended by memory and by a
# one-off manual audit. This makes the pipeline the defender.
#
#   ./scripts/check_docker_free.sh
#
# Exit 0 = clean, 1 = a violation (with the offending lines printed).
#
# What is NOT a violation, and why the check is written to know the difference:
# `docker.io/...` is an **OCI registry reference**, not a Docker dependency —
# the registry protocol is an OCI standard that containerd speaks natively, and
# `docker.io` is simply Docker Hub's hostname. Likewise `image/Dockerfile` is a
# widely-implemented *file format* (BuildKit consumes it without Docker). A check
# that flagged those would cry wolf until someone disabled it, which is worse
# than no check.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

VIOLATIONS=0
report() { printf '\033[31mVIOLATION\033[0m %s\n' "$*"; VIOLATIONS=$((VIOLATIONS + 1)); }
ok()     { printf '\033[32mok\033[0m        %s\n' "$*"; }

# Source files to scan: our code and config, never build artifacts or vendored
# sources (target/ contains dependency source we do not control).
scan_paths=(src crates deploy/nemr-volume/src deploy/sudoers.d scripts image .github Cargo.toml)
existing=()
for p in "${scan_paths[@]}"; do [[ -e "$p" ]] && existing+=("$p"); done

# ---------------------------------------------------------------------------
# 1. Docker CLI invocation, or a Docker daemon socket path
# ---------------------------------------------------------------------------
# Word-boundary `docker` as a command, or the well-known socket paths. Excludes
# this script itself (it necessarily contains the patterns it searches for).
cli_hits=$(grep -rnE '(^|[^a-zA-Z0-9_./-])(docker|dockerd)([[:space:]]|$)|/var/run/docker\.sock|/run/docker\.sock|DOCKER_HOST' \
    "${existing[@]}" 2>/dev/null \
    | grep -v 'check_docker_free.sh' \
    | grep -vE '^\S+:[0-9]+:\s*(//|#)' || true)
if [[ -n "$cli_hits" ]]; then
    report "Docker CLI invocation or daemon socket referenced:"
    printf '%s\n' "$cli_hits" | sed 's/^/          /'
else
    ok "no Docker CLI invocation or daemon socket path"
fi

# ---------------------------------------------------------------------------
# 2. Docker-related crates in the dependency graph (transitive included)
# ---------------------------------------------------------------------------
# NFR-01 says "directly OR transitively", so this walks the resolved graph, not
# the manifests.
check_tree() {
    local dir="$1" label="$2"
    local tree
    tree=$( (cd "$dir" && cargo tree --workspace --edges normal 2>/dev/null) || (cd "$dir" && cargo tree 2>/dev/null) )
    if [[ -z "$tree" ]]; then
        report "$label: could not resolve the dependency tree (cargo tree failed)"
        return
    fi
    local hits
    hits=$(printf '%s\n' "$tree" | grep -iE '\b(bollard|shiplift|dockworker|docker[_-]?(api|cli|credential|compose)|testcontainers)\b' || true)
    if [[ -n "$hits" ]]; then
        report "$label: Docker-related crate in the dependency graph:"
        printf '%s\n' "$hits" | sed 's/^/          /'
    else
        local count
        count=$(printf '%s\n' "$tree" | grep -c . )
        ok "$label: no Docker-related crate ($count dependency lines scanned)"
    fi
}
check_tree "." "engine workspace"
check_tree "deploy/nemr-volume" "privileged helper"

# ---------------------------------------------------------------------------
# 3. The base image build path must not require a Docker daemon
# ---------------------------------------------------------------------------
# The documented builder is BuildKit via buildctl (daemonless, rootless). Flag
# any instruction telling a human or a script to run `docker build`.
build_hits=$(grep -rniE 'docker[[:space:]]+(build|run|pull|push|compose)' \
    README.md PREREQUISITES.md "${existing[@]}" 2>/dev/null \
    | grep -v 'check_docker_free.sh' || true)
if [[ -n "$build_hits" ]]; then
    report "a Docker build/run command is documented or scripted:"
    printf '%s\n' "$build_hits" | sed 's/^/          /'
else
    ok "no Docker build/run command in docs or scripts"
fi

# ---------------------------------------------------------------------------
# 3b. A CI service container IS a Docker dependency (F-94)
# ---------------------------------------------------------------------------
# GitHub Actions `services:` blocks are started by the runner with literal
# `docker pull` / `docker create` / `docker start` — verified in our own job
# logs, not inferred. They are therefore a Docker dependency we REQUEST, which
# E-17 puts squarely inside NFR-01.
#
# Section 3's pattern could never see them: a `services:` block names no
# command, so it matched a *comment about* Docker while `docker pull postgres`
# executed three jobs above, in a workflow this script was already scanning.
# A guard that catches descriptions of the thing but not the thing (the F-56
# family, third variant). Hence a check keyed on the shape that requests a
# container rather than on the word "docker".
service_hits=$(grep -rnE '^[[:space:]]*(services:|image:[[:space:]]*[^[:space:]#])' \
    .github 2>/dev/null || true)
if [[ -n "$service_hits" ]]; then
    report "a CI service container is declared — the runner starts these with \`docker\` (F-94, E-17):"
    printf '%s\n' "$service_hits" | sed 's/^/          /'
    printf '          %s\n' "Run the dependency under podman instead (scripts/setup_sync_test_db.sh)."
else
    ok "no CI service container declared (no docker-started containers requested)"
fi

# Control for 3b, self-contained: the pattern must actually catch a services
# block, or the 'ok' above is green over nothing — which is exactly how this
# dependency survived from WP-J until now.
svc_probe=$(mktemp -d)
cat > "$svc_probe/workflow.yml" <<'PROBE'
jobs:
  x:
    services:
      postgres:
        image: postgres:16
PROBE
cat > "$svc_probe/innocent.yml" <<'PROBE'
jobs:
  x:
    steps:
      - run: echo "no containers here"
PROBE
svc_caught=$(grep -rnE '^[[:space:]]*(services:|image:[[:space:]]*[^[:space:]#])' "$svc_probe/workflow.yml" 2>/dev/null || true)
svc_false=$(grep -rnE '^[[:space:]]*(services:|image:[[:space:]]*[^[:space:]#])' "$svc_probe/innocent.yml" 2>/dev/null || true)
rm -rf "$svc_probe"
if [[ -z "$svc_caught" ]]; then
    report "control failed: a 'services:' block was NOT caught, so 3b proves nothing."
elif [[ -n "$svc_false" ]]; then
    report "control failed: a workflow with no containers was flagged; 3b is too broad."
else
    ok "control: a services: block is caught, a plain workflow is not — 3b discriminates"
fi

# ---------------------------------------------------------------------------
# 4. Registry references are NOT violations — assert we still parse them as such
# ---------------------------------------------------------------------------
# A control, so this check cannot silently degrade into "found nothing because
# it searched nothing". It must show the discrimination the check depends on:
# `docker.io/...` is an OCI registry reference and must NOT be flagged, while
# `docker run` is a real violation and MUST be.
#
# This used to assert that a docker.io/ reference existed somewhere under src/
# — true while the base image was named `docker.io/nemr/base`. After the rename
# to ghcr.io the only remaining matches were doc comments *about* the rename, so
# the control still passed while testing nothing. A control that survives by
# accident is worse than one that fails: it reports confidence it has not
# earned. It is now self-contained, so it holds whatever the tree happens to
# contain.
probe=$(mktemp -d)
trap 'rm -rf "$probe"' EXIT
cat > "$probe/registry_reference.txt" <<'PROBE'
image = "docker.io/library/alpine:latest"
PROBE
cat > "$probe/real_violation.txt" <<'PROBE'
docker run --rm alpine true
PROBE

probe_pattern='(^|[^a-zA-Z0-9_./-])(docker|dockerd)([[:space:]]|$)|/var/run/docker\.sock|/run/docker\.sock|DOCKER_HOST'
reference_flagged=$(grep -rnE "$probe_pattern" "$probe/registry_reference.txt" 2>/dev/null || true)
violation_flagged=$(grep -rnE "$probe_pattern" "$probe/real_violation.txt" 2>/dev/null || true)

if [[ -n "$reference_flagged" ]]; then
    report "control failed: a docker.io/ registry reference was flagged as a Docker dependency. \
The pattern is too broad and would reject legitimate OCI naming."
elif [[ -z "$violation_flagged" ]]; then
    report "control failed: 'docker run' was NOT flagged. The pattern matches nothing, so every \
'ok' above is meaningless."
else
    ok "control: registry references pass, 'docker run' is caught — the pattern discriminates"
fi

printf '\n'
if [[ $VIOLATIONS -eq 0 ]]; then
    printf '\033[32mPASS\033[0m — NFR-01 holds: no Docker dependency, socket, or CLI invocation.\n'
    exit 0
fi
printf '\033[31mFAIL\033[0m — %d NFR-01 violation(s). Docker is a hard architectural constraint (SPEC §3.1, NFR-01).\n' "$VIOLATIONS"
exit 1
