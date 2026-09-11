#!/usr/bin/env bash
#
# WP-K acceptance: the engine and the sync server, meeting for the first time.
#
# The flow a user lives: register, log in, create a session, work in it, stop,
# push. Then lose the machine — here, delete the local project entirely — pull
# the session back from the server, attach, and CONTINUE THE CONVERSATION.
#
# The last clause is the point. Presence of a file is not continuity of a
# session (M10's standard), so the continuity probe is the SPEC 1.43 form: the
# session is given two instructions in order, and after the round-trip it is
# asked — via `claude --continue`, forbidden from reading files — what the two
# instructions were, IN ORDER. The ordering exists nowhere on disk as a fact;
# only the conversation carries it.
#
# Modes, stated in the footer:
#   full          — live API: the continuity probe runs through Claude Code.
#   NEMR_SKIP_API — no API: the transcript's byte-identity through
#                   encrypt→push→delete→pull→decrypt→import is asserted
#                   instead. A weaker claim, printed as such.
#
# Requirements: a provisioned host (rootless containerd, helper, base image),
# the engine installed and current (the freshness gate runs first), Postgres
# for the server (scripts/setup_sync_test_db.sh), and nothing on port 18080.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"

# Waiting for things to come up, with the diagnostics built in (F-95).
. "$REPO/scripts/lib/proc.sh"

BLUE=$'\033[34m'; RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
STEP=0; ASSERTS=0
# F-6: the count is ASSERTED, not printed. The gate audit (2026-09-09) found
# six of this project's eight acceptance scripts exiting 0 over a tally nobody
# checked — a run that skipped a step read exactly like a run that made every
# assertion. Raise this number when a step is added; a run that counts anything
# else fails.
# Both modes make the same number: each NEMR_SKIP_API branch has exactly one
# `pass` on either side (the planted marker vs the live conversation; the
# file-level claim vs the recalled conversation). Measured in the skip mode.
EXPECTED_ASSERTIONS=15
step()   { STEP=$((STEP+1)); printf '\n%s== %d. %s%s\n' "$BLUE" "$STEP" "$1" "$RESET"; }
pass()   { ASSERTS=$((ASSERTS+1)); printf '   %sok%s %s\n' "$GREEN" "$RESET" "$1"; }
fail()   { printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1" >&2; exit 1; }

PROJECT="sync-acc-$$"
EMAIL="acceptance-$$-$(date +%s)@example.com"
export NEMR_CLOUD_PASSWORD="acceptance horse battery staple $$"
export NEMR_CLOUD_KDF_FAST=1     # test account; production uses OWASP costs
SERVER_ADDR="127.0.0.1:18080"
export NEMR_SERVER_URL="http://${SERVER_ADDR}"
# The database comes from the server's own settings file when the environment
# does not carry it — the same order the server uses — and only then from the
# development default.
. "$REPO/scripts/lib/settings.sh"
nemr_settings_load || exit 1
export DATABASE_URL="${DATABASE_URL:-postgres://nemr:nemr@127.0.0.1:5433/nemr}"
SKIP_API="${NEMR_SKIP_API:-0}"

WORK="$(mktemp -d)"
SERVER_PID=""
cleanup() {
    set +e
    "$REPO/target/release/nemr-cloud" release "$PROJECT" >/dev/null 2>&1
    delete_disposable "$PROJECT"
    # The throwaway account's local state must not outlive the run.
    "$REPO/target/release/nemr-cloud" logout >/dev/null 2>&1
    [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" >/dev/null 2>&1
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
step "Prerequisites (freshness gate first: prove the binaries are this source)"
# ---------------------------------------------------------------------------
command -v nemr >/dev/null || fail "nemr is not on PATH — ./scripts/install_engine.sh"
# A filter that matches nothing exits 0, so check it names a real test BEFORE
# trusting its result — otherwise a rename disarms this gate in silence.
require_test_exists the_installed_engine_matches_its_source --test regression \
    || fail "the freshness gate cannot run (see above)"
if ! cargo test --test regression the_installed_engine_matches_its_source --quiet >/dev/null 2>&1; then
    fail "the installed nemr/nemrd is not this source — ./scripts/install_engine.sh"
fi
pass "installed engine matches this source (nemr and nemrd hash-gated)"

cargo build --release -p nemr-sync -p nemr-cloud --quiet
pass "server and client built"

# The client under its many names, resolved via the open CLI's external
# subcommands — the acceptance runs `nemr login`, not `nemr-cloud login`.
BIN="$WORK/bin"; mkdir -p "$BIN"
for name in $(sed -n 's/^NAMES=(\(.*\))$/\1/p' "$REPO/scripts/install_sync_client.sh"); do
    ln -sf "$REPO/target/release/nemr-cloud" "$BIN/nemr-$name"
done
export PATH="$BIN:$PATH"

# ---------------------------------------------------------------------------
step "Start the sync server (filesystem store, local Postgres)"
# ---------------------------------------------------------------------------
BUNDLES="$WORK/bundles"; mkdir -p "$BUNDLES"
# Through `nemr server start` (SPEC 1.149), for the same reason the UI
# acceptance does: one command knows how to start a server, and a script that
# hand-assembles the environment drifts from it. Two settings this needed and
# did not have — since E-19 a server with no pepper REFUSES TO BIND, and
# without NEMR_SYNC_ENV_FILE= it would read the developer's own sync.env — so
# this start could not have worked on a clean host.
NEMR_BUNDLE_DIR="$BUNDLES" NEMR_SERVER_ADDR="$SERVER_ADDR" \
    NEMR_SYNC_ENV_FILE= NEMR_AUTH_PEPPER=ephemeral \
    NEMR_SYNC_BIN="$REPO/target/release/nemr-sync" \
    "$REPO/target/release/nemr-cloud" server start >"$WORK/server.log" 2>&1 &
SERVER_PID=$!

# Probe the database FIRST and separately, so "the server did not come up"
# cannot silently mean "the database was unreachable". Two causes that need
# different remedies must not share one message.
db_host_port="$(sed -E 's|.*@([^/]+)/.*|\1|' <<<"$DATABASE_URL")"
db_host="${db_host_port%%:*}"; db_port="${db_host_port##*:}"
[[ "$db_port" == "$db_host" ]] && db_port=5432
require_tcp "$db_host" "$db_port" "Postgres (from DATABASE_URL)" \
    "./scripts/setup_sync_test_db.sh" || exit 1
ASSERTS=$((ASSERTS + 1))

# The wait, its abort-on-death, its alive-vs-exited distinction and its log
# printing all live in wait_for_service now (F-95) — this script no longer owns
# a copy of that logic to get subtly wrong.
#
# 15s, deliberately. It was briefly raised to 30s while the cause of a CI
# failure was unknown; the cause turned out to be a missing database, so the
# raise was never attributable and was reverted. If a cold start on a loaded
# runner ever proves to need longer, that will be a measurement.
wait_for_service "the sync server" "$SERVER_PID" "$WORK/server.log" 15 \
    curl -fsS "http://${SERVER_ADDR}/health" || exit 1
ASSERTS=$((ASSERTS + 1))

# ---------------------------------------------------------------------------
step "Register (recovery code shown once, typed back, account activated)"
# ---------------------------------------------------------------------------
export NEMR_CLOUD_EMAIL="$EMAIL"
coproc REG { nemr register 2>&1; }
# bash unsets REG_PID the moment the coproc exits; hold it while it exists.
REG_WAIT_PID=$REG_PID
CODE=""
while IFS= read -r line <&"${REG[0]}"; do
    echo "   | $line"
    trimmed="$(echo "$line" | tr -d '[:space:]')"
    if [[ ${#trimmed} -gt 10 && "$trimmed" =~ ^[0-9A-Z-]+$ && "$trimmed" == *-* ]]; then
        CODE="$trimmed"
        echo "$CODE" >&"${REG[1]}"
    fi
    # UPDATED DELIBERATELY with the one-voice change (SPEC 1.151): register's
    # last line is now a result line, not "logged in as".
    [[ "$line" == *"recovery code confirmed"* || "$line" == *"logged in as"* ]] && break
done
wait "$REG_WAIT_PID" || fail "register exited non-zero"
[[ -n "$CODE" ]] || fail "no recovery code appeared"
pass "registered, recovery confirmed by typing the code back, logged in"

# ---------------------------------------------------------------------------
step "Create the session and work in it"
# ---------------------------------------------------------------------------
NEMR_NON_INTERACTIVE=1 nemr create "$PROJECT" --size 500MB
nemr start "$PROJECT"

MOUNT="$HOME/.local/share/nemr/mounts/$PROJECT"
if [[ "$SKIP_API" == "1" ]]; then
    # No API: plant a distinctive transcript-shaped file where session state
    # lives, so the round-trip's byte-fidelity can still be asserted.
    echo "mkdir -p /root/.claude/projects/-workspace && printf '%s' 'MARKER-$$-TRANSCRIPT' > /root/.claude/projects/-workspace/acceptance.jsonl" \
        | nemr attach "$PROJECT" >/dev/null 2>&1
    TRANSCRIPT="$MOUNT/.nemr-state/projects/-workspace/acceptance.jsonl"
    [[ -f "$TRANSCRIPT" ]] || fail "planted transcript not visible on the volume"
    pass "planted a transcript marker (NEMR_SKIP_API=1 — no live conversation)"
else
    # The SPEC 1.43 form: two instructions, in one conversation, in order. The
    # ordering will exist only in the transcript.
    reply=$(echo 'claude --permission-mode acceptEdits -p "Remember these two instructions, in this order. First: SAY-APRICOT. Second: COUNT-TO-NINE. Reply with exactly: STORED-BOTH"' \
        | nemr attach "$PROJECT" 2>&1 | tr -d '\r')
    grep -q "STORED-BOTH" <<<"$reply" || {
        echo "$reply" | head -20 >&2; fail "the session did not confirm the instructions"; }
    pass "live conversation established (two ordered instructions)"
fi
# Capture the session state's digest before it travels.
STATE_SHA=$(cd "$MOUNT/.nemr-state" && find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)

nemr stop "$PROJECT"

# ---------------------------------------------------------------------------
step "Push (encrypt client-side, upload, hold the lease)"
# ---------------------------------------------------------------------------
nemr push "$PROJECT"
grep -qi "$PROJECT" <<<"$(nemr sessions)" || fail "pushed session not in the index"
pass "pushed; the session is in the server index"

# E-16: the server holds ciphertext. Asserted in BOTH modes.
#
# This check used to be guarded by `[[ "$SKIP_API" == "1" ]] &&`, so in the
# full-API run — the mode that makes the strongest claim — the `pass` below
# printed with nothing behind it at all (the gate audit, 2026-09-09). Two
# needles now, and neither depends on the mode:
#
#   (a) structural: a plaintext bundle carries its manifest member's name in
#       the clear (src/bundle/manifest.rs: MANIFEST_MEMBER = "manifest.json"),
#       so finding that string in the stored object means the server holds
#       plaintext, whatever this run put in the session;
#   (b) this run's own words: the marker under NEMR_SKIP_API, and the live
#       conversation's confirmation phrase otherwise.
STORED=$(find "$BUNDLES" -type f | head -1)
[[ -n "$STORED" ]] || fail "no stored object in the bundle dir"
if grep -qa 'manifest.json' "$STORED"; then
    fail "the stored object carries the plaintext bundle's manifest member — E-16 violated"
fi
if [[ "$SKIP_API" == "1" ]]; then NEEDLE="MARKER-$$-TRANSCRIPT"; else NEEDLE="SAY-APRICOT"; fi
if grep -qa "$NEEDLE" "$STORED"; then
    fail "the stored object contains this run's plaintext ('$NEEDLE') — E-16 violated"
fi
pass "the stored object is ciphertext: no manifest member, no '$NEEDLE' ($(stat -c%s "$STORED") bytes)"

# ---------------------------------------------------------------------------
step "Delete the local project entirely"
# ---------------------------------------------------------------------------
refuse_protected "$PROJECT" || fail "refusing the delete step on a protected name"
nemr delete "$PROJECT" --yes
# output_has, not `nemr list | grep -q`: a listing that FAILED must not read as
# "the project is gone" (F-109). This assertion is the negative one, where that
# conflation is a false PASS.
output_has "^$PROJECT " -- nemr list && fail "project still listed after delete:
$_LAST_OUTPUT"
[[ -e "$HOME/.local/share/nemr/volumes/$PROJECT.img" ]] && fail "volume image survived delete"
pass "the project is gone from this machine"

# ---------------------------------------------------------------------------
step "Pull it back and continue"
# ---------------------------------------------------------------------------
nemr pull "$PROJECT"

# Restore is the one flow where a silent failure means someone's session appears
# to come back and has not. So the claim is USABLE, not merely listed, and each
# clause is asserted separately with the evidence printed when it fails.
output_has "^$PROJECT " -- nemr list || fail "pull reported success but the project is not listed.
   \`nemr list\` succeeded and named:
$_LAST_OUTPUT"
pass "the pulled project is listed"

NEW_SHA=$(cd "$MOUNT/.nemr-state" && find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)
[[ "$NEW_SHA" == "$STATE_SHA" ]] || fail "session state differs after the round-trip"
pass "session state is byte-identical through encrypt->push->delete->pull->import"

nemr start "$PROJECT"
output_has "^$PROJECT  *running" -- nemr list || fail "the pulled project did not reach running:
$_LAST_OUTPUT"
pass "the pulled project starts and reports running"

# A restored session with no network looks identical to a working one until
# Claude Code tries to reach the API — which is the failure NET-02's wiring can
# produce silently, so the restore path asserts it rather than assuming it.
resolved=$(echo 'getent hosts api.anthropic.com >/dev/null && echo NET-OK || echo NET-FAIL' \
    | nemr attach "$PROJECT" 2>&1 | tr -d '\r')
grep -q NET-OK <<<"$resolved" || {
    printf '%s\n' "$resolved" | head -20 >&2
    fail "the restored session has no working network (DNS did not resolve inside it)"; }
pass "the restored session has a working network (DNS resolves inside it)"

if [[ "$SKIP_API" == "1" ]]; then
    got=$(echo 'cat /root/.claude/projects/-workspace/acceptance.jsonl' \
        | nemr attach "$PROJECT" 2>&1 | tr -d '\r')
    grep -q "MARKER-$$-TRANSCRIPT" <<<"$got" || fail "transcript marker not readable in the restored session"
    pass "restored session serves the transcript (file-level claim only)"
else
    reply=$(echo 'claude --continue --permission-mode acceptEdits -p "Without reading any files, what were the two instructions I asked you to remember, in order? Answer with just the two, in order."' \
        | nemr attach "$PROJECT" 2>&1 | tr -d '\r')
    apricot=$(grep -obm1 "SAY-APRICOT" <<<"$reply" | cut -d: -f1 || true)
    nine=$(grep -obm1 "COUNT-TO-NINE" <<<"$reply" | cut -d: -f1 || true)
    [[ -n "$apricot" && -n "$nine" ]] || {
        echo "$reply" | head -20 >&2
        fail "the continued conversation did not recall both instructions"; }
    [[ "$apricot" -lt "$nine" ]] || fail "the instructions came back out of order"
    pass "the CONVERSATION continued: both instructions recalled, in order, files forbidden"
fi
nemr stop "$PROJECT"

# ---------------------------------------------------------------------------
step "Release the lease (clean stop)"
# ---------------------------------------------------------------------------
nemr release "$PROJECT"
pass "lease released"

if [[ "$ASSERTS" -ne "$EXPECTED_ASSERTIONS" ]]; then
    printf '\n%sFAIL%s — %d assertions, %d expected: a step was skipped, or one was\n' \
        "$RED" "$RESET" "$ASSERTS" "$EXPECTED_ASSERTIONS"
    printf '       added without raising EXPECTED_ASSERTIONS. Zero failures over too\n'
    printf '       few assertions is not a pass (F-6).\n'
    exit 1
fi
printf '\n%sPASS%s — %d steps, %d assertions, all %d expected.\n' \
    "$GREEN" "$RESET" "$STEP" "$ASSERTS" "$EXPECTED_ASSERTIONS"
if [[ "$SKIP_API" == "1" ]]; then
    echo "mode: NEMR_SKIP_API=1 — transcript byte-fidelity only; the live-conversation"
    echo "      continuity claim was NOT exercised. Run without NEMR_SKIP_API for the full claim."
else
    echo "mode: full — live API; session continuity proven through the round-trip."
fi
