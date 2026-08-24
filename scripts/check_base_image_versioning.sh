#!/usr/bin/env bash
#
# The base image version tag must name exactly one set of bytes (F-85).
#
# A version tag identifies a specific image and must never be reused for
# different bytes — otherwise a bundle recording the old digest becomes
# unrecoverable while the D-08 error's pull advice fetches the wrong image,
# looking correct. This check enforces the discipline at review time, without
# needing a build or a network.
#
#   ./scripts/check_base_image_versioning.sh
#
# What it verifies:
#   1. The version in src/config.rs BASE_IMAGE has a recorded digest under
#      image/digests/<version>.
#   2. Every recorded digest is a well-formed sha256.
#   3. No two versions record the SAME digest (identical bytes under two
#      versions is a different-but-related mistake — a needless second version).

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
fail=0
ok()  { printf '%sok%s        %s\n' "$GREEN" "$RESET" "$1"; }
bad() { printf '%sFAIL%s      %s\n' "$RED" "$RESET" "$1"; fail=1; }

DIGESTS_DIR="image/digests"

# The authoritative version: the tag on BASE_IMAGE in src/config.rs.
base_image_line=$(grep -E 'pub const BASE_IMAGE' src/config.rs | head -1)
version="${base_image_line##*:}"
version="${version%\"*}"

# CONTROL: we must have actually extracted a version, or every check below
# passes vacuously. A version looks like N.N.N.
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    bad "control: could not parse a version from src/config.rs BASE_IMAGE (got ${version:-empty}); \
this check would otherwise verify nothing"
    printf '\n%sFAIL%s\n' "$RED" "$RESET"
    exit 1
fi
ok "control: BASE_IMAGE version parses as ${version}"

# 1. The current version has a recorded digest.
if [[ -f "${DIGESTS_DIR}/${version}" ]]; then
    ok "current version ${version} has a recorded digest"
else
    bad "current version ${version} has NO digest recorded at ${DIGESTS_DIR}/${version}.
            Build the image and record it: echo <digest> > ${DIGESTS_DIR}/${version}"
fi

# 2. Every recorded digest is a well-formed sha256, and 3. none is duplicated.
declare -A seen_digest
for f in "${DIGESTS_DIR}"/*; do
    name="$(basename "$f")"
    [[ "$name" == "README.md" ]] && continue
    d="$(tr -d '[:space:]' < "$f")"
    if [[ ! "$d" =~ ^sha256:[0-9a-f]{64}$ ]]; then
        bad "${f} is not a well-formed sha256 digest: ${d:-empty}"
        continue
    fi
    if [[ -n "${seen_digest[$d]:-}" ]]; then
        bad "versions ${seen_digest[$d]} and ${name} record the SAME digest ${d} — \
two versions for identical bytes is a needless second version"
    else
        seen_digest[$d]="$name"
    fi
    ok "version ${name}: ${d}"
done

echo
if [[ "$fail" -ne 0 ]]; then
    printf '%sFAIL%s — the base image versioning ledger is inconsistent (F-85).\n' "$RED" "$RESET"
    exit 1
fi
printf '%sPASS%s — every version names one set of bytes; the current version is recorded.\n' "$GREEN" "$RESET"
