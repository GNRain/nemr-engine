#!/usr/bin/env bash
#
# WP-M acceptance: run a server inside a session and reach it from this host.
#
# That is the thing that did not work — someone runs their dev server, opens a
# browser, gets connection-refused, and concludes the tool is broken — so that
# is the thing this proves. Every assertion below is on HOST-observable state:
# what curl gets, and what ss reports, not what the engine says it did.
#
#   ./scripts/port_acceptance.sh
#
# Needs a provisioned host and the engine installed (the freshness gate runs
# first). Uses a disposable project and removes it, verifying as it goes.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"
. "$REPO/scripts/lib/proc.sh"

BLUE=$'\033[34m'; RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
STEP=0; ASSERTS=0
# F-6: the count is ASSERTED, not printed. The gate audit (2026-09-09) found
# six of this project's eight acceptance scripts exiting 0 over a tally nobody
# checked — a run that skipped a step read exactly like a run that made every
# assertion. Raise this number when a step is added; a run that counts anything
# else fails.
EXPECTED_ASSERTIONS=11
step() { STEP=$((STEP+1)); printf '\n%s== %d. %s%s\n' "$BLUE" "$STEP" "$1" "$RESET"; }
pass() { ASSERTS=$((ASSERTS+1)); printf '   %sok%s %s\n' "$GREEN" "$RESET" "$1"; }
fail() { printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1" >&2; exit 1; }

PROJECT="port-acc-$$"
HOST_PORT="${NEMR_TEST_PORT:-18234}"
MARKER="PORT-ACC-$$"
SOCK="${XDG_RUNTIME_DIR}/containerd-rootless/api.sock"

cleanup() {
    set +e
    delete_disposable "$PROJECT"
    # Belt and braces: if delete failed, do not leave a host port bound.
    for id in $(rootlessctl --socket="$SOCK" list-ports 2>/dev/null \
                | awk -v p="$HOST_PORT" 'NR>1 && $4==p {print $1}'); do
        rootlessctl --socket="$SOCK" remove-ports "$id" >/dev/null 2>&1
    done
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
step "Prerequisites"
# ---------------------------------------------------------------------------
command -v nemr >/dev/null || fail "nemr is not on PATH — ./scripts/install_engine.sh"
command -v rootlessctl >/dev/null || fail "rootlessctl is missing — see PREREQUISITES.md"
# A filter that matches nothing exits 0, so check it names a real test BEFORE
# trusting its result — otherwise a rename disarms this gate in silence.
require_test_exists the_installed_engine_matches_its_source --test regression \
    || fail "the freshness gate cannot run (see above)"
if ! cargo test --test regression the_installed_engine_matches_its_source --quiet >/dev/null 2>&1; then
    fail "the installed nemr/nemrd is not this source — ./scripts/install_engine.sh"
fi
pass "installed engine matches this source"

# The port must be free, or a pass could be someone else's server answering.
if timeout 2 bash -c ">/dev/tcp/127.0.0.1/${HOST_PORT}" 2>/dev/null; then
    fail "host port ${HOST_PORT} is already in use; this test would prove nothing.
        Set NEMR_TEST_PORT to a free port."
fi
pass "host port ${HOST_PORT} is free (so a later success is ours)"

# ---------------------------------------------------------------------------
step "Create a session and run a server inside it"
# ---------------------------------------------------------------------------
NEMR_NON_INTERACTIVE=1 nemr create "$PROJECT" --size 500MB >/dev/null
nemr start "$PROJECT" >/dev/null
pass "session running"

# node is what the base image ships. setsid so it outlives the attach.
echo "setsid nohup node -e \"require('http').createServer((q,s)=>s.end('${MARKER}')).listen(8000,'0.0.0.0')\" >/tmp/srv.log 2>&1 </dev/null & sleep 2; echo spawned" \
    | nemr attach "$PROJECT" >/dev/null 2>&1
pass "a server is listening on 8000 inside the session"

# ---------------------------------------------------------------------------
step "CONTROL: the host cannot reach it yet"
# ---------------------------------------------------------------------------
# Without this the test could pass against a port that was reachable anyway,
# proving nothing about forwarding.
if curl -fsS --max-time 3 "http://127.0.0.1:${HOST_PORT}/" >/dev/null 2>&1; then
    fail "host port ${HOST_PORT} answered BEFORE any forward existed"
fi
pass "refused before forwarding — the blocker, reproduced"

# ---------------------------------------------------------------------------
step "Forward it, and reach it"
# ---------------------------------------------------------------------------
nemr port add "$PROJECT" "${HOST_PORT}:8000" >/dev/null
got=$(curl -fsS --max-time 5 "http://127.0.0.1:${HOST_PORT}/" 2>/dev/null || true)
[[ "$got" == "$MARKER" ]] || fail "host got ${got:-<nothing>}, expected ${MARKER}"
pass "the host reached the session's server and got its marker back"

ss -tlnp 2>/dev/null | grep -q ":${HOST_PORT} " \
    || fail "no host listener on ${HOST_PORT} despite a successful request"
pass "ss confirms a real host listener (not a cached response)"

# ---------------------------------------------------------------------------
step "The declaration belongs to the project, the forward does not"
# ---------------------------------------------------------------------------
nemr stop "$PROJECT" >/dev/null
if ss -tlnp 2>/dev/null | grep -q ":${HOST_PORT} "; then
    fail "a stopped project still holds host port ${HOST_PORT}"
fi
pass "stop released the host port"
nemr port ls "$PROJECT" | grep -q "${HOST_PORT}" \
    || fail "the declaration did not survive stop"
pass "the declaration survived stop"

nemr start "$PROJECT" >/dev/null
wait_for_ready "the re-applied forward" 10 \
    bash -c "ss -tlnp 2>/dev/null | grep -q ':${HOST_PORT} '" \
    || fail "start did not re-apply the declared forward"
pass "start re-applied it without being asked"

# ---------------------------------------------------------------------------
step "Delete releases the port"
# ---------------------------------------------------------------------------
delete_disposable "$PROJECT" || fail "delete refused (protected-subject guard?)"
if ss -tlnp 2>/dev/null | grep -q ":${HOST_PORT} "; then
    fail "delete left host port ${HOST_PORT} bound to a project that no longer exists"
fi
pass "delete released the host port"

if [[ "$ASSERTS" -ne "$EXPECTED_ASSERTIONS" ]]; then
    printf '\n%sFAIL%s — %d assertions, %d expected: a step was skipped, or one was\n' \
        "$RED" "$RESET" "$ASSERTS" "$EXPECTED_ASSERTIONS"
    printf '       added without raising EXPECTED_ASSERTIONS. Zero failures over too\n'
    printf '       few assertions is not a pass (F-6).\n'
    exit 1
fi
printf '\n%sPASS%s — %d steps, %d assertions, all %d expected.\n' \
    "$GREEN" "$RESET" "$STEP" "$ASSERTS" "$EXPECTED_ASSERTIONS"
echo "A server inside a session is reachable from this host. That is the thing"
echo "that did not work."
