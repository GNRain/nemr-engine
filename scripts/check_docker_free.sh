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
# 4. Registry references are NOT violations — assert we still parse them as such
# ---------------------------------------------------------------------------
# A control, so this check cannot silently degrade into "found nothing because
# it searched nothing": docker.io/ MUST appear (it is the base image reference),
# and must NOT be counted above.
if grep -rqE 'docker\.io/' src crates 2>/dev/null; then
    ok "registry references (docker.io/...) present and correctly not flagged — OCI naming, not a Docker dependency"
else
    report "control failed: expected a docker.io/ registry reference in src/ or crates/ and found none. \
Either the base image reference moved (update this control) or this check is scanning the wrong paths."
fi

printf '\n'
if [[ $VIOLATIONS -eq 0 ]]; then
    printf '\033[32mPASS\033[0m — NFR-01 holds: no Docker dependency, socket, or CLI invocation.\n'
    exit 0
fi
printf '\033[31mFAIL\033[0m — %d NFR-01 violation(s). Docker is a hard architectural constraint (SPEC §3.1, NFR-01).\n' "$VIOLATIONS"
exit 1
