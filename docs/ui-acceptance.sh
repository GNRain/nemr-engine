#!/usr/bin/env bash
# The UI acceptance, verbatim from the Product Owner (2026-09-07):
#
#   "register, log in, see the list, pull a bundle into a fresh project,
#    start, attach, continue the conversation with history intact, stop,
#    see it pushed."
#
# Every step is taken THROUGH THE BROWSER'S SURFACE — the handshake, the
# guarded /api routes, the attach WebSocket with its Origin and single-use
# ticket — by docs/ui-acceptance.py, which is the page written as a script.
# Nothing here reaches around the surface: if the page could not do it, this
# does not do it either.
#
# The one exception, stated: `nemr create` makes the session that is later
# pushed, because creating a project is deliberately NOT in the UI's first
# pass (the Product Owner's scope: no settings, no project creation, no
# ports, no theming). Making the bundle is setup, not the acceptance.
#
# The recall claim is the SPEC 1.43 form: two instructions given in one
# conversation, in order, then asked back after the session has travelled.
# The ordering exists nowhere on disk but the transcript.
#
# Usage:
#   export DATABASE_URL=postgres://nemr:nemr@127.0.0.1:5433/nemr
#   bash docs/ui-acceptance.sh
#
# NEMR_SKIP_API=1 plants a transcript-shaped file instead of holding a live
# conversation, for a host with no working Claude Code login. It downgrades
# the recall claim to a file-level one and SAYS SO.

set -uo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"
# shellcheck source=../scripts/lib/proc.sh
. "$REPO/scripts/lib/proc.sh"

GREEN=$'\033[32m'; RED=$'\033[31m'; BOLD=$'\033[1m'; RESET=$'\033[0m'
PASS=0; FAIL=0
# The count is asserted, not merely printed (F-6). A run that skipped a step
# — Firefox missing, BiDi failing, a block silently short-circuited — would
# otherwise say PASS with fewer assertions, the green-over-nothing shape
# verify_wp_a.sh guards against for the regression suite. Raise this number
# when a step is added; a run that counts anything else fails.
EXPECTED_ASSERTIONS=44
step() { printf '\n%s== %s%s\n' "$BOLD" "$1" "$RESET"; }
pass() { PASS=$((PASS+1)); printf '   %sok%s   %s\n' "$GREEN" "$RESET" "$1"; }
fail() { FAIL=$((FAIL+1)); printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1"; }
die()  { fail "$1"; finish; exit 1; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/nemr-ui-acceptance.XXXXXX")
PROJECT="uiacc-$$"
LOCAL2="uiacc-$$-local"   # a second, local-only session for the page block (a push button beside a pull button)
SERVER_ADDR=127.0.0.1:18090
SERVER_URL="http://$SERVER_ADDR"
EMAIL="ui-acceptance-$$@example.com"
PASSWORD="ui acceptance horse battery staple $$"
SKIP_API="${NEMR_SKIP_API:-0}"
SYNC_PID=""; UI_PID=""; UI2_PID=""

finish() {
    printf '\n%s== Cleanup%s\n' "$BOLD" "$RESET"
    # Verify before destroying, and never touch a protected subject.
    for p in "$PROJECT" "$LOCAL2"; do
        if refuse_protected "$p" 2>/dev/null; then
            if output_has "^$p " -- nemr list; then
                nemr stop "$p" >/dev/null 2>&1
                nemr delete "$p" --yes >/dev/null 2>&1 && echo "   removed $p"
            else
                echo "   no local $p to remove"
            fi
        else
            echo "   REFUSED to touch $p: it is a protected subject"
        fi
    done
    for pid in "$UI_PID" "$UI2_PID" "$SYNC_PID"; do
        [[ -n "$pid" && "$pid" -gt 1 ]] && kill "$pid" 2>/dev/null
    done
    wait 2>/dev/null
    echo "   evidence kept in $WORK"
}
trap finish EXIT

# Everything the surface writes for this run lives here, not in the user's
# real state directory: the acceptance must not log the user out.
export XDG_STATE_HOME="$WORK/state"
export NEMR_SERVER_URL="$SERVER_URL"
export NEMR_CLOUD_KDF_FAST=1   # test cost; the binary says so on stderr
# Every browser action goes through the page-as-a-script, carrying the one
# session cookie the handshake produced.
UI() { local op="$1"; shift; NEMR_UI_COOKIE="${COOKIE:-}" python3 "$REPO/docs/ui-acceptance.py" "$op" "$LAUNCH_URL" "$@"; }

step "Prerequisites"
[[ -n "${DATABASE_URL:-}" ]] || die "DATABASE_URL must be set (scripts/setup_sync_test_db.sh)"
python3 -c 'import websockets' 2>/dev/null || die "python3 -m pip install websockets (the browser half needs a WebSocket client)"
command -v firefox >/dev/null || die "firefox is needed: the page itself is driven in a headless browser for the claims about what it shows"
db_host_port="$(sed -E 's|.*@([^/]+)/.*|\1|' <<<"$DATABASE_URL")"
require_tcp "${db_host_port%%:*}" "${db_host_port##*:}" "Postgres (from DATABASE_URL)" \
    "./scripts/setup_sync_test_db.sh" || exit 1
pass "Postgres is reachable"

cargo build --release -p nemr-sync -p nemr-cloud --quiet || die "the release build failed"
pass "nemr-sync and nemr-cloud built"

step "Start the sync server"
mkdir -p "$WORK/bundles"
NEMR_BUNDLE_DIR="$WORK/bundles" NEMR_SERVER_ADDR="$SERVER_ADDR" \
    NEMR_AUTH_PEPPER="$(head -c 32 /dev/urandom | base64 -w0)" \
    "$REPO/target/release/nemr-sync" >"$WORK/server.log" 2>&1 &
SYNC_PID=$!
wait_for_service "the sync server" "$SYNC_PID" "$WORK/server.log" 15 \
    curl -fsS "$SERVER_URL/health" || exit 1
pass "the sync server is up on $SERVER_ADDR"

step "Start the UI (this is the only port that opens, and only because we asked)"
"$REPO/target/release/nemr-cloud" ui --no-open >"$WORK/ui.log" 2>&1 &
UI_PID=$!
for _ in $(seq 1 60); do grep -q 'nemr ui: http' "$WORK/ui.log" && break; sleep 0.25; done
LAUNCH_URL=$(grep -o 'http://127.0.0.1:[0-9]*/#token=[0-9a-f]*' "$WORK/ui.log" | head -1)
[[ -n "$LAUNCH_URL" ]] || { cat "$WORK/ui.log"; die "the UI did not print a launch URL"; }
UI_PORT=${LAUNCH_URL#http://127.0.0.1:}; UI_PORT=${UI_PORT%%/*}
grep -q 'daemon reachable' "$WORK/ui.log" && pass "the UI reached the daemon before opening its port"
pass "the UI is serving on 127.0.0.1:$UI_PORT (token in the fragment, single-use)"

# The page itself, and the terminal it draws with — served from the binary,
# so the acceptance also proves the page needs nothing from the network.
for asset in /assets/xterm.js /assets/xterm.css /assets/addon-fit.js; do
    code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$UI_PORT$asset")
    [[ "$code" == 200 ]] || die "the page's $asset was not served ($code)"
done
pass "the page and its terminal are served from the binary (no external resource)"

step "Register through the browser (recovery code shown once, typed back)"
COOKIE=""
out=$(UI register "$EMAIL" "$PASSWORD" "$SERVER_URL") || die "registration through the surface failed: $out"
COOKIE=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["cookie"])' "$out")
CODE=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["recovery_code"])' "$out")
export COOKIE
[[ -n "$COOKIE" && -n "$CODE" ]] || die "no cookie or recovery code came back"
printf '%s\n' "$CODE" > "$WORK/recovery-code"
pass "registered as $EMAIL; the recovery code was shown once and typed back through the envelope"

step "Log in through the browser"
# Log out first, so the login below is the real thing and not the
# registration's leftover session.
python3 - "$LAUNCH_URL" "$COOKIE" <<'PY' || exit 1
import json, sys, urllib.request, re
base = re.match(r"(http://127\.0\.0\.1:\d+)", sys.argv[1]).group(1)
req = urllib.request.Request(base + "/api/logout", data=b"", method="POST")
req.add_header("X-Nemr-Request", "1"); req.add_header("Cookie", sys.argv[2])
with urllib.request.urlopen(req) as r:
    assert r.status == 200, r.status
PY
out=$(UI login "$EMAIL" "$PASSWORD" "$SERVER_URL") || die "login through the surface failed: $out"
grep -q "$EMAIL" <<<"$out" || die "the login did not come back as $EMAIL: $out"
pass "logged out and logged back in through the page, with the password alone"

step "Create the session and hold a conversation in it (setup: create is not in the UI's first pass)"
NEMR_NON_INTERACTIVE=1 nemr create "$PROJECT" --size 500MB >"$WORK/create.log" 2>&1 \
    || { cat "$WORK/create.log"; die "nemr create failed"; }
nemr start "$PROJECT" >/dev/null 2>&1 || die "nemr start failed"
pass "created and started $PROJECT"

MOUNT="$HOME/.local/share/nemr/mounts/$PROJECT"

# THE CONTROL for every terminal claim below. The first run of this
# acceptance reported a live conversation that never happened, because the
# capture began with the shell's echo of the command we typed and the
# prompt named the very answer it wanted. So, before any claim rests on
# what the terminal showed: type a command carrying a token it never
# prints, and require that token to be ABSENT from what we capture. If it
# is present, the capture contains our own input and every grep below can
# satisfy itself.
# The canary is asserted by EQUALITY, not by searching the raw screen for
# it: a terminal wraps a long line by emitting a carriage return mid-token,
# so an exact-string search on the raw is unreliable by construction (the
# first version of this control failed on exactly that, with the canary
# plainly on the screen). Equality is the stronger claim anyway — if any of
# the echo leaked in, the capture would not equal the command's output.
ECHO_CANARY="CANARY-$$-$RANDOM-ONLY-IN-THE-TYPED-LINE"
UI attach "$PROJECT" "test -n '$ECHO_CANARY' && echo canary-command-ran" "$WORK/canary.raw" > "$WORK/canary.txt" 2>&1
captured=$(tr -d '\n' < "$WORK/canary.txt")
[[ "$captured" == "canary-command-ran" ]] || {
    echo "   captured: $(head -c 400 "$WORK/canary.txt")"
    die "the capture is not exactly the command's output — it carries something we typed, and every terminal assertion below could then match its own input"; }
# ...and there really was an echo to exclude: the raw screen is much bigger
# and carries the shell's prompt, so the exclusion is work, not an empty
# stream reported as clean.
raw_bytes=$(stat -c%s "$WORK/canary.raw"); cap_bytes=$(stat -c%s "$WORK/canary.txt")
[[ "$raw_bytes" -gt $((cap_bytes * 4)) ]] && grep -q 'root@' "$WORK/canary.raw" \
    || die "the raw screen ($raw_bytes bytes) shows no echo to exclude, so this control proves nothing"
pass "CONTROL: the capture is exactly the command's output ($cap_bytes bytes) from a $raw_bytes-byte screen that carried our typed line"
if [[ "$SKIP_API" == "1" ]]; then
    echo "mkdir -p /root/.claude/projects/-workspace && printf '%s' 'MARKER-$$-TRANSCRIPT' > /root/.claude/projects/-workspace/acceptance.jsonl" \
        | nemr attach "$PROJECT" >/dev/null 2>&1
    [[ -f "$MOUNT/.nemr-state/projects/-workspace/acceptance.jsonl" ]] \
        || die "planted transcript not visible on the volume"
    pass "planted a transcript marker (NEMR_SKIP_API=1 — NO live conversation; the recall claim below is file-level only)"
else
    # Through the browser's attach, so the conversation is established the way
    # a user of the page would establish it.
    #
    # The helper returns ONLY what the command wrote — the terminal's echo of
    # what we typed is excluded structurally. This matters here more than
    # anywhere: the prompt names the answer it wants, so a capture that
    # included the echo would match itself. The first run of this acceptance
    # did exactly that and reported a conversation that never happened.
    UI attach "$PROJECT" \
        'claude --permission-mode acceptEdits -p "Remember these two instructions, in this order. First: SAY-APRICOT. Second: COUNT-TO-NINE. Reply with exactly: STORED-BOTH"' \
        "$WORK/converse.raw" > "$WORK/converse.txt" 2>"$WORK/converse.err"
    grep -q 'STORED-BOTH' "$WORK/converse.txt" \
        || { echo "   --- what the command wrote:"; sed 's/^/   | /' "$WORK/converse.txt" | tail -20
             die "the session did not confirm the two instructions (raw screen in $WORK/converse.raw)"; }
    pass "held a live conversation through the browser's terminal (two ordered instructions)"
fi

STATE_SHA=$(cd "$MOUNT/.nemr-state" && find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)

step "Stop and push, through the browser"
out=$(UI push "$PROJECT" "$PASSWORD" release) || die "the push call failed: $out"
python3 -c 'import json,sys; d=json.loads(sys.argv[1]); [print("   |",l["text"]) for l in d["lines"]]; sys.exit(0 if d["ok"] else 1)' "$out" \
    || die "stop-and-push failed: $(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["error"])' "$out")"
pass "the browser stopped the running session and pushed it"

STORED=$(find "$WORK/bundles" -type f | head -1)
[[ -n "$STORED" ]] || die "the server holds no bundle after the push"
if [[ "$SKIP_API" == "1" ]] && grep -q "MARKER-$$-TRANSCRIPT" "$STORED"; then
    die "the stored object contains plaintext — E-16 violated"
fi
pass "the server holds ciphertext ($(stat -c%s "$STORED") bytes); the marker is not in it"

step "Delete the project entirely, so the pull has to be real"
refuse_protected "$PROJECT" || die "refusing to delete a protected name"
nemr delete "$PROJECT" --yes >/dev/null 2>&1
output_has "^$PROJECT " -- nemr list && die "the project is still listed after delete: $_LAST_OUTPUT"
[[ -e "$HOME/.local/share/nemr/volumes/$PROJECT.img" ]] && die "the volume image survived the delete"
pass "the project is gone from this machine"

step "See the list in the browser"
list=$(UI sessions) || die "the list call failed: $list"
python3 - "$list" "$PROJECT" <<'PY' || exit 1
import json, sys
d = json.loads(sys.argv[1]); name = sys.argv[2]
row = next((r for r in d["rows"] if r["name"] == name), None)
assert row, f"no row for {name} in {d['rows']}"
assert row["where"] == "remote", f"a deleted project must read remote: {row}"
assert row["has_bundle"], f"the pushed bundle must show: {row}"
assert row["held_by"] is None, f"the lease was released, so nobody holds it: {row}"
print(f"   row: {name} where={row['where']} bundle={row['has_bundle']} held_by={row['held_by']} last_machine={row['last_machine']}")
PY
pass "the browser's list shows it remote, with a bundle, held by nobody"

step "The page itself, in a headless browser: what it shows (F-1, F-3, F-2)"
# The claims here are about the DOM the user sees — a form gone after
# login, one action panel at a time, the terminal closing on shell exit —
# so they are made against the real page in a real browser (Firefox,
# headless, driven over WebDriver BiDi), with real keyboard input for the
# `exit`. A second UI instance serves it: a launch token is single-use and
# the page must spend its own. A second, local-only session gives the list a
# push button beside the first session's pull button.
NEMR_NON_INTERACTIVE=1 nemr create "$LOCAL2" --size 500MB >"$WORK/create2.log" 2>&1 \
    || { cat "$WORK/create2.log"; die "nemr create $LOCAL2 failed"; }
"$REPO/target/release/nemr-cloud" ui --no-open >"$WORK/ui2.log" 2>&1 &
UI2_PID=$!
for _ in $(seq 1 60); do grep -q 'nemr ui: http' "$WORK/ui2.log" && break; sleep 0.25; done
LAUNCH2=$(grep -o 'http://127.0.0.1:[0-9]*/#token=[0-9a-f]*' "$WORK/ui2.log" | head -1)
[[ -n "$LAUNCH2" ]] || { cat "$WORK/ui2.log"; die "the second UI did not print a launch URL"; }
page_out=$(python3 "$REPO/docs/ui-acceptance.py" page "$LAUNCH2" "$EMAIL" "$PASSWORD" "$SERVER_URL" "$PROJECT" "$LOCAL2" 2>"$WORK/page.err"); page_rc=$?
if [[ -z "$page_out" ]]; then
    sed 's/^/   | /' "$WORK/page.err" | grep -v Gtk-Message | tail -15
    die "the page-driving half produced no result (its stderr above; rc=$page_rc)"
fi
printf '%s\n' "$page_out" > "$WORK/page.json"
while IFS=$'\t' read -r ok name detail; do
    if [[ "$ok" == "True" ]]; then pass "$name"; else fail "$name${detail:+ — $detail}"; fi
done < <(python3 -c 'import json,sys; [print(r["ok"], r["name"], r.get("detail",""), sep="\t") for r in json.load(open(sys.argv[1]))]' "$WORK/page.json")
[[ "$page_rc" -eq 0 ]] || { grep -v Gtk-Message "$WORK/page.err" | tail -5 | sed 's/^/   | /'; die "the page did not show what it should (details above)"; }
# The page started $LOCAL2 on the way; stop it and remove it now, so the
# rest of the flow sees exactly what it saw before this block.
kill "$UI2_PID" 2>/dev/null; wait "$UI2_PID" 2>/dev/null; UI2_PID=""
nemr stop "$LOCAL2" >/dev/null 2>&1; nemr delete "$LOCAL2" --yes >/dev/null 2>&1
output_has "^$LOCAL2 " -- nemr list && die "$LOCAL2 survived its removal"
pass "the page block left nothing behind"

step "Pull it into a fresh project and start it, from the browser"
out=$(UI pull "$PROJECT" "$PASSWORD") || die "the pull call failed: $out"
python3 -c 'import json,sys; d=json.loads(sys.argv[1]); [print("   |",l["text"]) for l in d["lines"]]; sys.exit(0 if d["ok"] else 1)' "$out" \
    || die "pull-and-start failed: $(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["error"])' "$out")"
output_has "^$PROJECT " -- nemr list || die "the pull reported success but the project is not listed: $_LAST_OUTPUT"
pass "pulled and started; the engine lists it"

NEW_SHA=$(cd "$MOUNT/.nemr-state" && find . -type f -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)
[[ "$NEW_SHA" == "$STATE_SHA" ]] \
    || die "the session state differs after the round trip ($STATE_SHA -> $NEW_SHA)"
pass "the session state is byte-identical through encrypt, push, delete, pull, import"

list=$(UI sessions)
python3 - "$list" "$PROJECT" <<'PY' || exit 1
import json, sys
d = json.loads(sys.argv[1]); name = sys.argv[2]
row = next(r for r in d["rows"] if r["name"] == name)
assert row["where"] == "both" and row["running"], f"the row must say here and running: {row}"
assert row["held_by"] == d["this_machine"], f"this machine now holds the lease: {row}"
print(f"   row: {name} where={row['where']} running={row['running']} held_by={row['held_by']}")
PY
pass "the browser's list now says both, running, held by this machine"

step "Attach in the browser and continue the conversation"
if [[ "$SKIP_API" == "1" ]]; then
    UI attach "$PROJECT" 'cat /root/.claude/projects/-workspace/acceptance.jsonl' "$WORK/recall.raw" > "$WORK/recall.txt" 2>&1
    grep -q "MARKER-$$-TRANSCRIPT" "$WORK/recall.txt" || die "the transcript marker is not readable in the restored session"
    pass "the restored session serves the transcript through the browser's terminal (FILE-LEVEL claim only — NEMR_SKIP_API=1)"
else
    # The question deliberately does NOT contain the two tokens, so the
    # answer cannot be satisfied by the question.
    UI attach "$PROJECT" \
        'claude --continue --permission-mode acceptEdits -p "Without reading any files, what were the two instructions I asked you to remember, in order? Answer with just the two, in order."' \
        "$WORK/recall.raw" > "$WORK/recall.txt" 2>&1
    apricot=$(grep -obm1 'SAY-APRICOT' "$WORK/recall.txt" | cut -d: -f1)
    nine=$(grep -obm1 'COUNT-TO-NINE' "$WORK/recall.txt" | cut -d: -f1)
    [[ -n "$apricot" && -n "$nine" ]] \
        || { echo "   --- what the continued session answered:"; sed 's/^/   | /' "$WORK/recall.txt" | tail -25
             die "the continued conversation did not recall both instructions (raw screen in $WORK/recall.raw)"; }
    [[ "$apricot" -lt "$nine" ]] || die "the instructions came back out of order"
    pass "the conversation continued in the browser's terminal: both instructions recalled, IN ORDER"
fi

step "Stop and push again, and see it pushed"
before=$(stat -c%Y "$STORED")
sleep 1
out=$(UI push "$PROJECT" "$PASSWORD" release) || die "the second push call failed: $out"
python3 -c 'import json,sys; d=json.loads(sys.argv[1]); [print("   |",l["text"]) for l in d["lines"]]; sys.exit(0 if d["ok"] else 1)' "$out" \
    || die "the second stop-and-push failed"
pass "the browser stopped it again and pushed"

list=$(UI sessions)
python3 - "$list" "$PROJECT" <<'PY' || exit 1
import json, sys
d = json.loads(sys.argv[1]); name = sys.argv[2]
row = next(r for r in d["rows"] if r["name"] == name)
assert not row["running"], f"the row must say stopped: {row}"
assert row["has_bundle"], f"the bundle is there: {row}"
assert row["held_by"] is None, f"released, so another machine can take it: {row}"
print(f"   row: {name} running={row['running']} bundle={row['has_bundle']} held_by={row['held_by']} updated={row['updated_at_unix']}")
PY
after=$(stat -c%Y "$(find "$WORK/bundles" -type f | head -1)")
[[ "$after" -gt "$before" ]] || die "the stored bundle was not rewritten by the second push"
pass "the list shows it stopped, pushed and released; the stored bundle was rewritten"

printf '\n'
if [[ $FAIL -eq 0 && $PASS -ne $EXPECTED_ASSERTIONS ]]; then
    fail "expected $EXPECTED_ASSERTIONS assertions, counted $PASS — a step was skipped or added without raising EXPECTED_ASSERTIONS"
fi
if [[ $FAIL -eq 0 ]]; then
    printf '%sPASS%s — the whole flow ran through the browser: %d assertions, all %d expected.\n' "$GREEN" "$RESET" "$PASS" "$EXPECTED_ASSERTIONS"
    [[ "$SKIP_API" == "1" ]] && printf '  (NEMR_SKIP_API=1: the recall claim was file-level, not a live conversation.)\n'
    exit 0
fi
printf '%sFAIL%s — %d passed, %d failed. Evidence in %s\n' "$RED" "$RESET" "$PASS" "$FAIL" "$WORK"
exit 1
