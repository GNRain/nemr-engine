#!/usr/bin/env bash
#
# E-11 enforcement: the open half must not depend on the commercial half.
#
# Open, in `nemr-engine`: the engine, the containerd wrapper, the volume layer,
# the privileged helper, the bundle format specification.
# Commercial: the sync layer, cloud storage backends, the lease service,
# identity, and the client-side encryption. Each lives in its own workspace
# crate, listed in COMMERCIAL below.
#
# The direction of dependency is what keeps that structural rather than
# conventional. If `nemr-engine` ever depended on a commercial crate, `nemr
# export` and `nemr import` would inherit that dependency and stop working on a
# machine with no network and no account — the property
# `e11_export_and_import_work_with_no_network_and_no_credentials` asserts at
# runtime. This check catches it at the dependency graph, which is earlier and
# cheaper.
#
#   ./scripts/check_seam.sh
#
# Exit 0 = the seam holds, 1 = it has leaked.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# The commercial crates. Add a crate here the moment it is created, or the seam
# check silently stops covering it.
COMMERCIAL=(nemr-storage nemr-crypto nemr-sync)

VIOLATIONS=0
report() { printf '\033[31mVIOLATION\033[0m %s\n' "$*"; VIOLATIONS=$((VIOLATIONS + 1)); }
ok()     { printf '\033[32mok\033[0m        %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. The engine's dependency graph must not contain any commercial crate.
# ---------------------------------------------------------------------------
engine_tree=$(cargo tree --package nemr-engine --edges normal 2>/dev/null)
if [[ -z "$engine_tree" ]]; then
    report "could not resolve nemr-engine's dependency tree"
else
    leaked=0
    for crate in "${COMMERCIAL[@]}"; do
        if grep -q "$crate" <<<"$engine_tree"; then
            report "nemr-engine depends on $crate — the open half now needs the commercial half:"
            grep -n "$crate" <<<"$engine_tree" | sed 's/^/          /'
            leaked=1
        fi
    done
    if [[ $leaked -eq 0 ]]; then
        deps=$(grep -c . <<<"$engine_tree")
        ok "nemr-engine depends on no commercial crate (${#COMMERCIAL[@]} checked, $deps dependency lines scanned)"
    fi
fi

# ---------------------------------------------------------------------------
# 2. No source file in the open half may reference a commercial crate.
# ---------------------------------------------------------------------------
# Catches a dependency added and used before the manifest is committed, and a
# `path = "…"` import that bypasses the package name.
open_sources=(src crates/nemr-containerd/src deploy/nemr-volume/src)
existing=()
for p in "${open_sources[@]}"; do [[ -d "$p" ]] && existing+=("$p"); done

# Build an alternation like `nemr_storage\|nemr-storage\|nemr_crypto\|nemr-crypto`.
pattern=""
for crate in "${COMMERCIAL[@]}"; do
    underscore="${crate//-/_}"
    pattern+="${pattern:+\\|}${underscore}\\|${crate}"
done

if source_hits=$(grep -rn "$pattern" "${existing[@]}" 2>/dev/null); then
    report "the open half references a commercial crate in source:"
    printf '%s\n' "$source_hits" | sed 's/^/          /'
else
    ok "no source file in the open half references a commercial crate"
fi

# ---------------------------------------------------------------------------
# 3. Control — the check must be capable of failing.
# ---------------------------------------------------------------------------
# Without this, a rename of a commercial crate would make every grep above match
# nothing and the check would pass while enforcing nothing. That is the
# green-over-nothing pattern this project keeps hitting; the control is cheap.
for crate in "${COMMERCIAL[@]}"; do
    if [[ ! -f "crates/$crate/Cargo.toml" ]]; then
        report "control failed: crates/$crate does not exist, so this check is \
scanning for a crate that is not there and would pass regardless. Update \
COMMERCIAL in this script if the crate was renamed, moved, or removed."
    else
        crate_tree=$(cargo tree --package "$crate" --edges normal 2>/dev/null)
        if grep -q "$crate" <<<"$crate_tree"; then
            ok "control: $crate exists and is resolvable, so a leak would be visible"
        else
            report "control failed: $crate did not appear in its own dependency tree"
        fi
    fi
done

printf '\n'
if [[ $VIOLATIONS -eq 0 ]]; then
    printf '\033[32mPASS\033[0m — the E-11 seam holds: the open half stands alone.\n'
    exit 0
fi
printf '\033[31mFAIL\033[0m — %d E-11 seam violation(s). The open engine must work with no network and no account.\n' "$VIOLATIONS"
exit 1
