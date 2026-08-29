#!/usr/bin/env bash
#
# The base image must build to the same digest twice (F-74).
#
# This is the check that was believed to already exist. It did not: CI had five
# jobs, none of which built the image twice or compared anything, so
# `FROM node:22-slim` and an unpinned Claude Code produced different images on
# different hosts and the drift only surfaced when a bundle failed to import
# across two real machines.
#
# It builds twice, cold, and compares the manifest digest. Both halves of the
# fix are exercised: the pinned inputs, and the residue cleanup plus timestamp
# normalisation that make the remaining bytes stable.
#
#   ./scripts/check_base_image_reproducible.sh
#
# WHAT THIS DOES NOT CLAIM. `apt-get update` is still unpinned, so two builds
# WEEKS apart can legitimately differ when Debian publishes an update. This
# asserts determinism at a point in time — which is what "two hosts set up the
# same day disagree" was — not immutability over time. Distribution by digest
# (D-08/D-10) is the only thing that makes two hosts agree indefinitely.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The version lives in src/config.rs; this reads it (F-124).
. "scripts/lib/base_image.sh"
IMAGE="$(nemr_base_image)"

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export BUILDKIT_HOST="${BUILDKIT_HOST:-unix://${XDG_RUNTIME_DIR}/buildkit/buildkitd.sock}"
export PATH="$HOME/.local/bin:$PATH"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-1700000000}"

RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
command -v buildctl >/dev/null || { echo "buildctl not found on PATH" >&2; exit 2; }
[[ -S "${BUILDKIT_HOST#unix://}" ]] || { echo "buildkitd not answering at $BUILDKIT_HOST" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

digest_of() {
    tar -xOf "$1" index.json \
        | python3 -c 'import sys,json;print(json.load(sys.stdin)["manifests"][0]["digest"])'
}

build_to() {
    buildctl build \
        --frontend dockerfile.v0 \
        --local context=image \
        --local dockerfile=image \
        --no-cache \
        --opt "build-arg:SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH}" \
        --output "type=oci,dest=$1,name=${IMAGE},rewrite-timestamp=true"
}

echo "==> Build 1 of 2 (cold)"
build_to "$work/a.tar"
echo "==> Build 2 of 2 (cold)"
build_to "$work/b.tar"

a="$(digest_of "$work/a.tar")"
b="$(digest_of "$work/b.tar")"
echo
echo "    build 1: $a"
echo "    build 2: $b"

# CONTROL: a digest must actually have been read. An empty or malformed value
# compares equal to itself and would report a clean pass over nothing.
if [[ ! "$a" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    printf '\n%sFAIL%s — could not read a manifest digest (%s); this check verified nothing.\n' \
        "$RED" "$RESET" "${a:-empty}"
    exit 1
fi

echo
if [[ "$a" == "$b" ]]; then
    printf '%sPASS%s — two cold builds produced the same image.\n' "$GREEN" "$RESET"
    printf '       Determinism at a point in time. apt remains unpinned, so builds weeks\n'
    printf '       apart may still differ; see the Dockerfile and D-08.\n'
else
    printf '%sFAIL%s — two cold builds of identical source produced different images.\n' "$RED" "$RESET"
    printf '       Two hosts following the same instructions will get images that M11\n'
    printf '       correctly refuses to move bundles between.\n'
    exit 1
fi
