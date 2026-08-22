#!/usr/bin/env bash
#
# E-11 enforcement: the open half must not depend on the commercial half.
#
# Open, in `nemr-engine`: the engine, the containerd wrapper, the volume layer,
# the privileged helper, the bundle format specification.
# Commercial, in `nemr-storage`: the sync layer, cloud storage backends, the
# lease service, identity.
#
# The direction of dependency is what keeps that structural rather than
# conventional. If `nemr-engine` ever depended on `nemr-storage`, `nemr export`
# and `nemr import` would inherit an object-store dependency and stop working on
# a machine with no network and no account — the property
# `e11_export_and_import_work_with_no_network_and_no_credentials` asserts at
# runtime. This check catches it at the dependency graph, which is earlier and
# cheaper.
#
#   ./scripts/check_seam.sh
#
# Exit 0 = the seam holds, 1 = it has leaked.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

VIOLATIONS=0
report() { printf '\033[31mVIOLATION\033[0m %s\n' "$*"; VIOLATIONS=$((VIOLATIONS + 1)); }
ok()     { printf '\033[32mok\033[0m        %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. The engine's dependency graph must not contain the commercial crate.
# ---------------------------------------------------------------------------
engine_tree=$(cargo tree --package nemr-engine --edges normal 2>/dev/null)
if [[ -z "$engine_tree" ]]; then
    report "could not resolve nemr-engine's dependency tree"
else
    if grep -q 'nemr-storage' <<<"$engine_tree"; then
        report "nemr-engine depends on nemr-storage — the open half now needs the commercial half:"
        grep -n 'nemr-storage' <<<"$engine_tree" | sed 's/^/          /'
    else
        deps=$(grep -c . <<<"$engine_tree")
        ok "nemr-engine does not depend on nemr-storage ($deps dependency lines scanned)"
    fi
fi

# ---------------------------------------------------------------------------
# 2. No source file in the open half may reference the commercial crate.
# ---------------------------------------------------------------------------
# Catches a dependency added and used before the manifest is committed, and a
# `path = "…"` import that bypasses the package name.
open_sources=(src crates/nemr-containerd/src deploy/nemr-volume/src)
existing=()
for p in "${open_sources[@]}"; do [[ -d "$p" ]] && existing+=("$p"); done

if source_hits=$(grep -rn 'nemr_storage\|nemr-storage' "${existing[@]}" 2>/dev/null); then
    report "the open half references the commercial crate in source:"
    printf '%s\n' "$source_hits" | sed 's/^/          /'
else
    ok "no source file in the open half references nemr-storage"
fi

# ---------------------------------------------------------------------------
# 3. Control — the check must be capable of failing.
# ---------------------------------------------------------------------------
# Without this, a rename of the commercial crate would make every grep above
# match nothing and the check would pass while enforcing nothing. That is the
# green-over-nothing pattern this project keeps hitting; the control is cheap.
if [[ ! -f crates/nemr-storage/Cargo.toml ]]; then
    report "control failed: crates/nemr-storage does not exist, so this check is \
scanning for a crate that is not there and would pass regardless. Update this \
script if the commercial crate was renamed or moved."
else
    storage_tree=$(cargo tree --package nemr-storage --edges normal 2>/dev/null)
    if grep -q 'nemr-storage' <<<"$storage_tree"; then
        ok "control: the commercial crate exists and is resolvable, so a leak would be visible"
    else
        report "control failed: nemr-storage did not appear in its own dependency tree"
    fi
fi

printf '\n'
if [[ $VIOLATIONS -eq 0 ]]; then
    printf '\033[32mPASS\033[0m — the E-11 seam holds: the open half stands alone.\n'
    exit 0
fi
printf '\033[31mFAIL\033[0m — %d E-11 seam violation(s). The open engine must work with no network and no account.\n' "$VIOLATIONS"
exit 1
