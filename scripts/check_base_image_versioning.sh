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

# The authoritative version, through the ONE shared extraction (F-124) — this
# check used to carry its own private parser, which worked partly by luck of
# the constant's quoting. The helper controls its own extraction (loud failure
# on a mangled constant, never an empty string), and the shape check here is
# the belt to that braces.
. "scripts/lib/base_image.sh"
version="$(nemr_base_version)" || {
    bad "control: the shared extraction could not read a version from src/config.rs; \
this check would otherwise verify nothing"
    printf '\n%sFAIL%s\n' "$RED" "$RESET"
    exit 1
}
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    bad "control: extracted version is not N.N.N (got ${version:-empty})"
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

# 4. THE DRIFT GUARD (F-124): no production consumer hardcodes the version.
#
# The version lives in src/config.rs and everything else reads it through
# scripts/lib/base_image.sh. A literal `nemr-base:<digits>` reappearing in a
# script or a probe binary is the six-file bump coming back — config.rs and a
# consumer disagreeing means the host suite builds one image and tests
# another. Scope: scripts/ and the probe binaries; src/ is not scanned because
# test fixtures legitimately use arbitrary example references.
drift=$(grep -rn "nemr-base:[0-9]" scripts/ crates/*/src/bin/ 2>/dev/null | grep -v "check_base_image_versioning.sh" || true)
if [[ -n "$drift" ]]; then
    bad "a production consumer hardcodes the base image version — the six-file
          bump is back. The version lives in src/config.rs; read it through
          scripts/lib/base_image.sh:
$drift"
else
    ok "no script or probe hardcodes the version (all read src/config.rs)"
fi

# CONTROL: the same pattern must match the one place the version DOES live,
# or the guard above passes because the grep is broken, not because the tree
# is clean.
if [[ "$(grep -c 'nemr-base:[0-9]' src/config.rs)" -ge 1 ]]; then
    ok "control: the drift pattern matches src/config.rs itself — the grep works"
else
    bad "control: the drift pattern matches NOTHING in src/config.rs, so the
          clean result above is a broken instrument, not a clean tree"
fi

echo
if [[ "$fail" -ne 0 ]]; then
    printf '%sFAIL%s — the base image versioning ledger is inconsistent (F-85).\n' "$RED" "$RESET"
    exit 1
fi
printf '%sPASS%s — every version names one set of bytes; the current version is recorded.\n' "$GREEN" "$RESET"
