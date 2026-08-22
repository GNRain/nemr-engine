#!/usr/bin/env bash
#
# The base image's network inputs must be pinned (F-74).
#
# There was never a CI check for base-image reproducibility — the belief that
# one existed and was passing is what this script answers. CI builds the image
# on every runner and compares nothing, so `FROM node:22-slim` and an unpinned
# `npm install -g @anthropic-ai/claude-code` produced different bytes on
# different days, and the drift only surfaced when a bundle failed to import
# across two real machines.
#
# This does not verify reproducibility — it cannot; `apt-get update` remains
# unpinned and BuildKit output is not bit-stable. It verifies the narrower,
# checkable thing: that the inputs which demonstrably drifted are pinned, and
# stay pinned.
#
#   ./scripts/check_base_image_pins.sh

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

DOCKERFILE=image/Dockerfile
RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
failures=0
ok()   { printf '%sok%s        %s\n' "$GREEN" "$RESET" "$1"; }
bad()  { printf '%sFAIL%s      %s\n' "$RED" "$RESET" "$1"; failures=$((failures + 1)); }

[[ -f "$DOCKERFILE" ]] || { echo "$DOCKERFILE not found" >&2; exit 2; }

# 1. Every FROM must select by digest. A tag is a moving target.
while read -r line; do
    [[ -z "$line" ]] && continue
    if [[ "$line" == *"@sha256:"* ]] || [[ "$line" == *'@${'* ]]; then
        ok "FROM pinned by digest: ${line}"
    else
        bad "FROM is not pinned by digest: ${line}
            fix: resolve the tag and pin it —
                 FROM node:22-slim@sha256:<digest>"
    fi
done < <(grep -E '^\s*FROM ' "$DOCKERFILE" || true)

# 2. Every global npm install must carry an explicit version.
while read -r line; do
    [[ -z "$line" ]] && continue
    # A version is either a literal @x.y.z after the package name, or a
    # substituted build arg.
    if [[ "$line" =~ @[0-9]+\.[0-9]+ ]] || [[ "$line" == *'${'* ]]; then
        ok "npm install pinned: $(echo "$line" | tr -s ' ')"
    else
        bad "npm install without a version: $(echo "$line" | tr -s ' ')
            fix: pin it — npm install -g \"pkg@1.2.3\", ideally behind an ARG"
    fi
done < <(grep -E 'npm install' "$DOCKERFILE" || true)

# 3. The build script must pass the reproducibility flags. Pinning inputs is
#    necessary and not sufficient: measured, two cold builds of fully pinned
#    source still differed until timestamps were normalised and build residue
#    removed.
for needle in "SOURCE_DATE_EPOCH" "rewrite-timestamp=true"; do
    if grep -q -- "$needle" scripts/build_base_image.sh; then
        ok "build_base_image.sh passes $needle"
    else
        bad "build_base_image.sh does not pass $needle
            fix: without it the image is not reproducible even with pinned inputs"
    fi
done

# CONTROL: this file must actually contain the constructs being checked. Without
# it, renaming the Dockerfile or rewording a directive makes every grep match
# nothing and the script reports a clean pass over an empty search.
from_count=$(grep -cE '^\s*FROM ' "$DOCKERFILE" || true)
npm_count=$(grep -cE 'npm install' "$DOCKERFILE" || true)
if (( from_count > 0 && npm_count > 0 )); then
    ok "control: found ${from_count} FROM and ${npm_count} npm install directive(s) to check"
else
    bad "control: found ${from_count} FROM and ${npm_count} npm install directives — \
this script checked nothing and a clean result would be meaningless"
fi

echo
if (( failures > 0 )); then
    printf '%sFAIL%s — the base image has unpinned inputs; two hosts will build different images.\n' "$RED" "$RESET"
    exit 1
fi
printf '%sPASS%s — the base image inputs that drifted are pinned.\n' "$GREEN" "$RESET"
printf '       Not a reproducibility guarantee: apt remains unpinned (see the Dockerfile).\n'
