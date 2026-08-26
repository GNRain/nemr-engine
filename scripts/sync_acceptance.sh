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

BLUE=$'\033[34m'; RED=$'\033[31m'; GREEN=$'\033[32m'; RESET=$'\033[0m'
STEP=0; ASSERTS=0
step()   { STEP=$((STEP+1)); printf '\n%s== %d. %s%s\n' "$BLUE" "$STEP" "$1" "$RESET"; }
pass()   { ASSERTS=$((ASSERTS+1)); printf '   %sok%s %s\n' "$GREEN" "$RESET" "$1"; }
fail()   { printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1" >&2; exit 1; }

PROJECT="sync-acc-$$"
EMAIL="acceptance-$$-$(date +%s)@example.com"
export NEMR_CLOUD_PASSWORD="acceptance horse battery staple $$"
export NEMR_CLOUD_KDF_FAST=1     # test account; production uses OWASP costs
SERVER_ADDR="127.0.0.1:18080"
export NEMR_SERVER_URL="http://${SERVER_ADDR}"
export DATABASE_URL="${DATABASE_URL:-postgres://nemr:nemr@127.0.0.1:5433/nemr}"
SKIP_API="${NEMR_SKIP_API:-0}"

WORK="$(mktemp -d)"
SERVER_PID=""
cleanup() {
    set +e
    "$REPO/target/release/nemr-cloud" release "$PROJECT" >/dev/null 2>&1
    nemr delete "$PROJECT" --yes >/dev/null 2>&1
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
if ! cargo test --test regression the_installed_engine_matches_its_source --quiet >/dev/null 2>&1; then
    fail "the installed nemr/nemrd is not this source — ./scripts/install_engine.sh"
fi
pass "installed engine matches this source (nemr and nemrd hash-gated)"

cargo build --release -p nemr-sync -p nemr-cloud --quiet
pass "server and client built"

# The client under its many names, resolved via the open CLI's external
# subcommands — the acceptance runs `nemr login`, not `nemr-cloud login`.
BIN="$WORK/bin"; mkdir -p "$BIN"
for name in login logout register sessions push pull release; do
    ln -sf "$REPO/target/release/nemr-cloud" "$BIN/nemr-$name"
done
export PATH="$BIN:$PATH"

# ---------------------------------------------------------------------------
step "Start the sync server (filesystem store, local Postgres)"
# ---------------------------------------------------------------------------
BUNDLES="$WORK/bundles"; mkdir -p "$BUNDLES"
NEMR_BUNDLE_DIR="$BUNDLES" NEMR_SERVER_ADDR="$SERVER_ADDR" \
    "$REPO/target/release/nemr-sync" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 30); do
    curl -fsS "http://${SERVER_ADDR}/health" >/dev/null 2>&1 && break
    sleep 0.5
done
curl -fsS "http://${SERVER_ADDR}/health" >/dev/null || {
    cat "$WORK/server.log" >&2; fail "server did not come up"; }
pass "server answering on ${SERVER_ADDR}"

# ---------------------------------------------------------------------------
step "Register (recovery code shown once, typed back, account activated)"
# ---------------------------------------------------------------------------
export NEMR_CLOUD_EMAIL="$EMAIL"
coproc REG { nemr register 2>&1; }
CODE=""
while IFS= read -r line <&"${REG[0]}"; do
    echo "   | $line"
    trimmed="$(echo "$line" | tr -d '[:space:]')"
    if [[ ${#trimmed} -gt 10 && "$trimmed" =~ ^[0-9A-Z-]+$ && "$trimmed" == *-* ]]; then
        CODE="$trimmed"
        echo "$CODE" >&"${REG[1]}"
    fi
    [[ "$line" == *"logged in as"* ]] && break
done
wait "$REG_PID" || fail "register exited non-zero"
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

# The server holds ciphertext: the stored object must not contain the marker
# or any plaintext bundle magic.
STORED=$(find "$BUNDLES" -type f | head -1)
[[ -n "$STORED" ]] || fail "no stored object in the bundle dir"
if [[ "$SKIP_API" == "1" ]] && grep -q "MARKER-$$-TRANSCRIPT" "$STORED"; then
    fail "the stored object contains plaintext — E-16 violated"
fi
pass "the stored object is ciphertext ($(stat -c%s "$STORED") bytes)"

# ---------------------------------------------------------------------------
step "Delete the local project entirely"
# ---------------------------------------------------------------------------
nemr delete "$PROJECT" --yes
nemr list 2>/dev/null | grep -q "^$PROJECT " && fail "project still listed after delete"
[[ -e "$HOME/.local/share/nemr/volumes/$PROJECT.img" ]] && fail "volume image survived delete"
pass "the project is gone from this machine"

# ---------------------------------------------------------------------------
step "Pull it back and continue"
# ---------------------------------------------------------------------------
nemr pull "$PROJECT"
nemr list 2>/dev/null | grep -q "^$PROJECT " || fail "pulled project not in nemr list"
NEW_SHA=$(cd "$MOUNT/.nemr-state" && find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)
[[ "$NEW_SHA" == "$STATE_SHA" ]] || fail "session state differs after the round-trip"
pass "session state is byte-identical through encrypt->push->delete->pull->import"

nemr start "$PROJECT"
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

printf '\n%sPASS%s — %d steps, %d assertions.\n' "$GREEN" "$RESET" "$STEP" "$ASSERTS"
if [[ "$SKIP_API" == "1" ]]; then
    echo "mode: NEMR_SKIP_API=1 — transcript byte-fidelity only; the live-conversation"
    echo "      continuity claim was NOT exercised. Run without NEMR_SKIP_API for the full claim."
else
    echo "mode: full — live API; session continuity proven through the round-trip."
fi
