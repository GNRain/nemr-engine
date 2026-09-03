#!/usr/bin/env bash
#
# Tests for scripts/fetch_base_image.sh — pull-first base-image provisioning
# (F-126, the WSL2 spike's divergence 1).
#
# Every path of the fetch script is exercised against a shimmed `ctr` and a
# shimmed builder, so the suite needs no network, no containerd, and no root —
# and a CONTROL proves the suite goes red when the digest refusals are
# disabled, so a passing run demonstrates the guard exists rather than
# assuming it.
#
#   ./scripts/test_fetch_base_image.sh
#
# The shims are state-driven: `ctr images ls` reports whatever digest the
# state file holds; `ctr images pull` and the fake builder write to it, or
# fail, per FAKE_* env. Every invocation is logged so the suite can assert
# what was NOT called (the builder must not run when the registry drifted).

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

SCRIPT=scripts/fetch_base_image.sh
RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'

# The neutered CONTROL copy must live in scripts/ — the fetch script cds
# relative to its own location, so a copy elsewhere fails on sourcing its lib
# instead of failing on the neutered guard, which would prove nothing.
WORK="$(mktemp -d)"
NEUTERED="scripts/.fetch_neutered_test.$$"
trap 'rm -rf "$WORK" "$NEUTERED"' EXIT
mkdir -p "$WORK/bin"

# The real image reference and recorded digest — the suite tests against the
# genuine ledger so it cannot drift from what provisioning actually uses.
. scripts/lib/base_image.sh
IMAGE="$(nemr_base_image)"
VERSION="${IMAGE##*:}"
RECORDED="$(tr -d '[:space:]' < "image/digests/${VERSION}")"
WRONG="sha256:0000000000000000000000000000000000000000000000000000000000000000"

# ---------------------------------------------------------------------------
# Shims
# ---------------------------------------------------------------------------
cat > "$WORK/bin/ctr" <<'SHIM'
#!/usr/bin/env bash
echo "ctr $*" >> "$FAKE_LOG"
case " $* " in
    *" images ls "*)
        echo "REF TYPE DIGEST SIZE PLATFORMS LABELS"
        if [[ -s "$FAKE_STATE" ]]; then
            echo "$FAKE_IMAGE application/vnd.oci.image.manifest.v1+json $(cat "$FAKE_STATE") 100MiB linux/amd64 -"
        fi
        ;;
    *" images pull "*)
        if [[ "${FAKE_PULL:-fail}" == "ok" ]]; then
            echo "${FAKE_PULL_DIGEST:?}" > "$FAKE_STATE"
        else
            echo "ctr: failed to resolve reference (shim: pull disabled)" >&2
            exit 1
        fi
        ;;
    *)
        echo "shim ctr: unexpected invocation: $*" >&2
        exit 99
        ;;
esac
SHIM
cat > "$WORK/bin/fake-builder" <<'SHIM'
#!/usr/bin/env bash
echo "builder" >> "$FAKE_LOG"
if [[ "${FAKE_BUILD:-fail}" == "ok" ]]; then
    echo "${FAKE_BUILD_DIGEST:?}" > "$FAKE_STATE"
else
    echo "fake-builder: build failed (shim)" >&2
    exit 1
fi
SHIM
chmod +x "$WORK/bin/ctr" "$WORK/bin/fake-builder"

# ---------------------------------------------------------------------------
# Harness
# ---------------------------------------------------------------------------
failures=0
n=0

# run <want-exit> <label> [VAR=value ...]
#   PRESENT=<digest>  pre-seed the state file (image already in containerd)
run() {
    local want="$1" label="$2"; shift 2
    n=$((n + 1))
    local state="$WORK/state.$n" log="$WORK/log.$n" out="$WORK/out.$n"
    : > "$log"; : > "$state"
    local envs=() kv
    for kv in "$@"; do
        if [[ "$kv" == PRESENT=* ]]; then
            echo "${kv#PRESENT=}" > "$state"
        else
            envs+=("$kv")
        fi
    done
    local got=0
    env PATH="$WORK/bin:$PATH" \
        FAKE_STATE="$state" FAKE_LOG="$log" FAKE_IMAGE="$IMAGE" \
        "${envs[@]}" NEMR_TEST_BUILDER="$WORK/bin/fake-builder" \
        "${FETCH:-$SCRIPT}" > "$out" 2>&1 || got=$?
    if [[ "$got" -ne "$want" ]]; then
        printf '%sred%s   [%d] %s (want exit %s, got %s)\n' "$RED" "$RESET" "$n" "$label" "$want" "$got"
        sed 's/^/        /' "$out"
        failures=$((failures + 1))
        return
    fi
    printf '%sok%s    [%d] %s\n' "$GREEN" "$RESET" "$n" "$label"
}

# assert_log <yes|no> <pattern> — about the MOST RECENT vector's shim log
assert_log() {
    local mode="$1" pattern="$2" hit=0
    grep -q "$pattern" "$WORK/log.$n" && hit=1
    if { [[ "$mode" == yes && "$hit" -eq 0 ]] || [[ "$mode" == no && "$hit" -eq 1 ]]; }; then
        printf '%sred%s   [%d] expected %s invocation matching %q\n' "$RED" "$RESET" "$n" "$mode" "$pattern"
        failures=$((failures + 1))
    fi
}

# ---------------------------------------------------------------------------
# Vectors
# ---------------------------------------------------------------------------
run 0 "already present at the recorded digest — nothing to do" \
    "PRESENT=$RECORDED"
assert_log no "images pull"
assert_log no "builder"

run 1 "present at a DIFFERENT digest — refuse, never repair by re-pull" \
    "PRESENT=$WRONG"
assert_log no "images pull"
assert_log no "builder"

run 0 "absent, pull delivers the recorded digest — the normal fresh-host path" \
    FAKE_PULL=ok "FAKE_PULL_DIGEST=$RECORDED"
assert_log yes "images pull"
assert_log no "builder"

run 1 "absent, pull delivers a DRIFTED digest — refuse, and do NOT build over it" \
    FAKE_PULL=ok "FAKE_PULL_DIGEST=$WRONG"
assert_log no "builder"

run 0 "absent, pull fails, fallback build reproduces the recorded digest" \
    FAKE_PULL=fail FAKE_BUILD=ok "FAKE_BUILD_DIGEST=$RECORDED"
assert_log yes "builder"

run 1 "absent, pull fails, fallback build produces a DIVERGENT digest — fail closed" \
    FAKE_PULL=fail FAKE_BUILD=ok "FAKE_BUILD_DIGEST=$WRONG"

run 1 "absent, pull fails, fallback build fails — fail closed" \
    FAKE_PULL=fail FAKE_BUILD=fail

# The drift refusal must NAME both digests, or the operator is left guessing.
if ! grep -q "$WRONG" "$WORK/out.4" || ! grep -q "$RECORDED" "$WORK/out.4"; then
    printf '%sred%s   [4] the drift refusal must name both the recorded and the served digest\n' "$RED" "$RESET"
    failures=$((failures + 1))
fi

# ---------------------------------------------------------------------------
# CONTROL — the suite must go red when the refusals are disabled.
#
# A copy of the fetch script with its sole refusal site neutered must ALLOW
# the drifted-pull vector. If it still refuses, the refusals are not flowing
# through the guarded site and every red assertion above proves less than it
# claims. Same construction as test_git_guard.sh's neutered-copy control.
# ---------------------------------------------------------------------------
sed 's/exit 1/exit 0/g' "$SCRIPT" > "$NEUTERED"
chmod +x "$NEUTERED"
if [[ "$(grep -c 'exit 0' "$NEUTERED")" -lt 1 ]]; then
    printf '%sred%s   CONTROL: neutering changed nothing — the refusal site moved\n' "$RED" "$RESET"
    failures=$((failures + 1))
fi
FETCH="$NEUTERED" run 0 "CONTROL: neutered copy allows the drifted pull (proves the guard bites)" \
    FAKE_PULL=ok "FAKE_PULL_DIGEST=$WRONG"
unset FETCH

# ---------------------------------------------------------------------------
echo
if [[ "$failures" -ne 0 ]]; then
    printf '%s%d red%s\n' "$RED" "$failures" "$RESET"
    exit 1
fi
echo "0 red"
echo "PASS"
