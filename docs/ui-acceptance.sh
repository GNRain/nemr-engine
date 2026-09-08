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
#
# The storage backend (E-20) picks the mode: with NEMR_S3_BUCKET in the
# caller's environment the sync server stores in that bucket and the run
# reads the bucket back independently (six more assertions); otherwise a
# directory under the work dir.
if [[ -n "${NEMR_S3_BUCKET:-}" ]]; then
    STORAGE_MODE=s3; EXPECTED_ASSERTIONS=65
else
    STORAGE_MODE=local; EXPECTED_ASSERTIONS=61
fi
# The human arm (E-21) adds its own assertions when it runs.
[[ "${NEMR_HUMAN_LOGIN:-0}" == 1 ]] && EXPECTED_ASSERTIONS=$((EXPECTED_ASSERTIONS + 4))
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
SYNC_PID=""; UI_PID=""; UI2_PID=""; UI3_PID=""; NOLOGIN=""

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
    for p in "${NOLOGIN:-}"; do
        [[ -n "$p" ]] && refuse_protected "$p" 2>/dev/null && output_has "^$p " -- nemr list && { nemr stop "$p" >/dev/null 2>&1; nemr delete "$p" --yes >/dev/null 2>&1 && echo "   removed $p"; }
    done
    if [[ -n "${NEMR_HOST_CREDENTIALS:-}" ]]; then unset NEMR_HOST_CREDENTIALS; stop_daemon 2>/dev/null || true; echo "   stopped the seamed daemon; the next nemr command starts a clean one"; fi
    for pid in "$UI_PID" "$UI2_PID" "${UI3_PID:-}" "$SYNC_PID"; do
        [[ -n "$pid" && "$pid" -gt 1 ]] && kill "$pid" 2>/dev/null
    done
    wait 2>/dev/null
    echo "   evidence kept in $WORK"
}
trap finish EXIT

# Everything the surface writes for this run lives here, not in the user's
# real state directory: the acceptance must not log the user out.
export XDG_STATE_HOME="$WORK/state"
# NEMR_SERVER_URL is deliberately NOT exported (E-19): the server is named
# once, at registration, and the client remembers it across logout. A step
# that reaches the server with no address of its own is proving that.
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
# F-7: a stale install must not hide behind a run from the build tree. If a
# sync client is installed, it has to be this build — the same gate
# scripts/sync_acceptance.sh applies to the engine.
INSTALLED_CLIENT="${NEMR_INSTALL_DIR:-$HOME/.local/bin}/nemr-cloud"
if [[ -e "$INSTALLED_CLIENT" ]]; then
    inst=$(sha256sum "$INSTALLED_CLIENT" | cut -d' ' -f1); built=$(sha256sum "$REPO/target/release/nemr-cloud" | cut -d' ' -f1)
    [[ "$inst" == "$built" ]] || die "the installed sync client ($INSTALLED_CLIENT, $(date -r "$INSTALLED_CLIENT" +%F)) is not this build — ./scripts/install_sync_client.sh"
    pass "the installed sync client matches this build (hash-gated, F-7)"
else
    pass "no sync client is installed at $INSTALLED_CLIENT; nothing to gate (this run uses the build tree)"
fi

# F-9: a nemrd in another user namespace cannot run the privileged helper, and
# `ss` cannot even see its socket from here. Name it; never talk to it.
for pid in $(pgrep -x nemrd -u "$(id -u)" 2>/dev/null || true); do
    [[ "$(readlink /proc/$pid/ns/user)" == "$(readlink /proc/self/ns/user)" ]] \
        || die "nemrd pid $pid lives in another user namespace (its sudo cannot work) — kill it; a plain host shell autostarts a proper one"
done

step "Start the sync server (storage: $STORAGE_MODE)"
# The port must be free BEFORE the server is started: a stale server from an
# earlier run answers /health, the new one dies at bind, and every step after
# passes against the wrong server — which happened. A read, not a repair.
if ss -ltn 2>/dev/null | grep -q ":${SERVER_ADDR##*:} "; then
    die "something already listens on $SERVER_ADDR (a stale sync server from an earlier run?) — stop it; this script starts its own and will not talk to another"
fi
# NEMR_AUTH_PEPPER=ephemeral is E-19's one escape hatch: a throwaway server
# with a random pepper and a loud warning. A server with no pepper refuses
# to bind, which is what a real deployment gets. NEMR_SYNC_ENV_FILE is
# emptied so the developer's own ~/.config/nemr/sync.env is never read here.
BUNDLE_PREFIX="ui-acceptance-$$"
if [[ "$STORAGE_MODE" == s3 ]]; then
    # F-8 (ruled): nobody creates buckets — not the server, not this script.
    # A bucket that does not exist is a configuration error, refused here
    # before anything runs, the way a server without a pepper refuses.
    python3 "$REPO/docs/ui-acceptance.py" s3-list x "$BUNDLE_PREFIX/" >/dev/null 2>"$WORK/bucket.err" \
        || { sed 's/^/   | /' "$WORK/bucket.err" | tail -3; die "the bucket named by NEMR_S3_BUCKET is not readable (does it exist? is the credential scoped to it?) — this script never creates one (F-8)"; }
    pass "the bucket named by NEMR_S3_BUCKET exists and answers; nothing here will create one (F-8)"
    # The bucket's own variables select it (E-20): NEMR_S3_* are inherited
    # from the caller's environment — never echoed, never written — and
    # NEMR_BUNDLE_DIR is unset, since exactly one backend is allowed. A
    # per-run prefix keeps this run's objects apart and lets cleanup find
    # exactly them.
    env -u NEMR_BUNDLE_DIR NEMR_SYNC_ENV_FILE= NEMR_SERVER_ADDR="$SERVER_ADDR" \
        NEMR_AUTH_PEPPER=ephemeral NEMR_BUNDLE_PREFIX="$BUNDLE_PREFIX" \
        "$REPO/target/release/nemr-sync" >"$WORK/server.log" 2>&1 &
else
    mkdir -p "$WORK/bundles"
    env -u NEMR_S3_BUCKET -u NEMR_S3_PROVIDER -u NEMR_S3_ENDPOINT -u NEMR_S3_ACCESS_KEY_ID -u NEMR_S3_SECRET_ACCESS_KEY \
        NEMR_SYNC_ENV_FILE= NEMR_BUNDLE_DIR="$WORK/bundles" NEMR_SERVER_ADDR="$SERVER_ADDR" \
        NEMR_AUTH_PEPPER=ephemeral NEMR_BUNDLE_PREFIX="$BUNDLE_PREFIX" \
        "$REPO/target/release/nemr-sync" >"$WORK/server.log" 2>&1 &
fi
SYNC_PID=$!
wait_for_service "the sync server" "$SYNC_PID" "$WORK/server.log" 15 \
    curl -fsS "$SERVER_URL/health" || exit 1
pass "the sync server is up on $SERVER_ADDR"
# The server's log, plain: no colour codes even if a subscriber emits them.
server_log() { sed 's/\x1b\[[0-9;]*m//g' "$WORK/server.log"; }
server_log | grep -q 'THROWAWAY SERVER' || die "the ephemeral pepper did not announce itself loudly"
pass "the server says loudly that its pepper is ephemeral (E-19's escape hatch)"
if [[ "$STORAGE_MODE" == s3 ]]; then
    server_log | grep -q "storage backend store=r2:" \
        || { server_log | grep 'storage backend' | sed 's/^/   | /'; die "the server did not choose the object store from NEMR_S3_*"; }
    pass "the server chose the object store from its own variables (E-20)"
    server_log | grep -q 'storage backend reachable' || die "the pre-bind probe did not pass"
    pass "the egress-free probe passed before the port bound"
else
    server_log | grep -q "storage backend store=local:" || die "the server did not choose the directory backend"
    pass "the server chose the directory backend from NEMR_BUNDLE_DIR (E-20)"
    server_log | grep -q 'storage backend reachable' || die "the pre-bind probe did not pass"
    pass "the egress-free probe passed before the port bound"
fi

step "Start the UI through the open CLI (nemr ui → nemr-ui on PATH; the only port that opens, and only because we asked)"
# E-19: the launcher is the extension form. The build tree's binary is laid
# on PATH under the same names the install script lays, and the open `nemr`
# execs it — so the launcher this run proves is the one a user has.
mkdir -p "$WORK/bin"
for name in $(sed -n 's/^NAMES=(\(.*\))$/\1/p' "$REPO/scripts/install_sync_client.sh"); do
    ln -sf "$REPO/target/release/nemr-cloud" "$WORK/bin/nemr-$name"
done
export PATH="$WORK/bin:$PATH"
[[ -x "$WORK/bin/nemr-ui" ]] || die "the install script's name list does not include ui"
command -v nemr >/dev/null || die "the open CLI (nemr) is not on PATH"
# Say exactly which nemr-ui this run executes (F-7): the name, where it
# points, and its hash beside the build's.
ui_exe=$(command -v nemr-ui); ui_real=$(readlink -f "$ui_exe")
echo "   nemr-ui → $ui_exe → $ui_real (sha256 $(sha256sum "$ui_real" | cut -c1-16)…; built $(sha256sum "$REPO/target/release/nemr-cloud" | cut -c1-16)…)"
[[ "$ui_real" == "$(readlink -f "$REPO/target/release/nemr-cloud")" ]] || die "nemr-ui on PATH is not this build"
nemr ui --no-open >"$WORK/ui.log" 2>&1 &
UI_PID=$!
for _ in $(seq 1 60); do grep -q 'nemr ui: http' "$WORK/ui.log" && break; sleep 0.25; done
LAUNCH_URL=$(grep -o 'http://127.0.0.1:[0-9]*/#token=[0-9a-f]*' "$WORK/ui.log" | head -1)
[[ -n "$LAUNCH_URL" ]] || { cat "$WORK/ui.log"; die "the UI did not print a launch URL"; }
UI_PORT=${LAUNCH_URL#http://127.0.0.1:}; UI_PORT=${UI_PORT%%/*}
grep -q 'daemon reachable' "$WORK/ui.log" && pass "the UI reached the daemon before opening its port"
pass "the UI is serving on 127.0.0.1:$UI_PORT, launched as nemr ui through the open CLI (token in the fragment, single-use)"

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

# What the server stored, read back from where it stored it. In s3 mode
# the object is fetched from the bucket by the acceptance's own signer —
# not by the server — at the key the server logged, and its size must be
# the size the server logged.
stored_key=$(server_log | sed -n 's/.*stored bundle key=\([^ ]*\) bytes=\([0-9]*\).*/\1/p' | tail -1)
stored_bytes=$(server_log | sed -n 's/.*stored bundle key=\([^ ]*\) bytes=\([0-9]*\).*/\2/p' | tail -1)
[[ -n "$stored_key" ]] || die "the server logged no stored bundle"
if [[ "$STORAGE_MODE" == s3 ]]; then
    [[ "$stored_key" == "$BUNDLE_PREFIX/"* ]] || die "the stored key is outside this run's prefix: $stored_key"
    STORED="$WORK/from-bucket.bin"
    got=$(python3 "$REPO/docs/ui-acceptance.py" s3-get "$LAUNCH_URL" "$stored_key" "$STORED") \
        || die "the object the server logged is not in the bucket at $stored_key"
    [[ "$got" == "$stored_bytes" ]] || die "the bucket holds $got bytes at that key; the server logged $stored_bytes"
    pass "the bucket holds the object at the key the server logged, $got bytes, read back by the acceptance's own signer (E-20)"
else
    STORED=$(find "$WORK/bundles" -type f | head -1)
    [[ -n "$STORED" ]] || die "the server holds no bundle after the push"
fi
if [[ "$SKIP_API" == "1" ]] && grep -q "MARKER-$$-TRANSCRIPT" "$STORED"; then
    die "the stored object contains plaintext — E-16 violated"
fi
# A plaintext bundle is a tar of the session; ciphertext carries none of its
# member names. The transcript directory's name is the one every session has.
if grep -aq 'projects/-workspace' "$STORED"; then
    die "the stored object carries a plaintext member name — the server saw plaintext"
fi
pass "the server holds ciphertext ($(stat -c%s "$STORED") bytes); no plaintext member name is in it"

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
done < <(python3 -c 'import json,sys; [print(r["ok"], r["name"], " ".join(str(r.get("detail","")).split()), sep="\t") for r in json.load(open(sys.argv[1]))]' "$WORK/page.json")
[[ "$page_rc" -eq 0 ]] || { grep -v Gtk-Message "$WORK/page.err" | tail -5 | sed 's/^/   | /'; die "the page did not show what it should (details above)"; }
# The page started $LOCAL2 on the way; stop it and remove it now, so the
# rest of the flow sees exactly what it saw before this block.
kill "$UI2_PID" 2>/dev/null; wait "$UI2_PID" 2>/dev/null; UI2_PID=""
nemr stop "$LOCAL2" >/dev/null 2>&1; nemr delete "$LOCAL2" --yes >/dev/null 2>&1
output_has "^$LOCAL2 " -- nemr list && die "$LOCAL2 survived its removal"
pass "the page block left nothing behind"

# ---------------------------------------------------------------------------
step "The credential step on a machine with no Claude login (E-21) — the automated arm"
# ---------------------------------------------------------------------------
# A host that has never logged in cannot be produced on this host any other
# way: the ruled test seam (NEMR_HOST_CREDENTIALS, path-only) points the
# daemon at a scratch path. The seam reaches the daemon only through its
# environment, so the running daemon is stopped and the next `nemr` command
# autostarts one that inherits it — the move install_engine.sh makes. At the
# end the seamed daemon is stopped again, and the next command starts a
# clean one. Nothing logs in here; the human arm does that, once.
NOCRED="$WORK/nocred/.credentials.json"
stop_daemon() {
    # Only the daemon on this user's socket, found by its socket's owner pid.
    local sock="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/nemr/nemrd.sock"
    [[ -S "$sock" ]] || return 0
    for pid in $(pgrep -x nemrd -u "$(id -u)"); do kill "$pid" 2>/dev/null; done
    for _ in $(seq 1 50); do pgrep -x nemrd -u "$(id -u)" >/dev/null || break; sleep 0.2; done
    rm -f "$sock"
}
stop_daemon
export NEMR_HOST_CREDENTIALS="$NOCRED"
NOLOGIN="uiacc-$$-nologin"
NEMR_NON_INTERACTIVE=1 nemr create "$NOLOGIN" --size 500MB >"$WORK/create3.log" 2>&1 \
    || { cat "$WORK/create3.log"; die "create must succeed on a host with no login (AUTH-03 as amended by E-21)"; }
pass "create succeeded on a host with no Claude login (AUTH-03 amended: create and restore alike)"
[[ -f "$NOCRED" ]] || die "no placeholder was written at $NOCRED"
[[ "$(stat -c %a "$NOCRED")" == "600" ]] || die "the placeholder is mode $(stat -c %a "$NOCRED"), not 600"
grep -q '_nemr_placeholder' "$NOCRED" || die "the file at the credential path is not the engine's placeholder"
pass "the engine wrote its placeholder at the credential path, mode 600"
nemr status "$NOLOGIN" 2>/dev/null | grep -q 'NO LOGIN YET' || die "nemr status does not say NO LOGIN YET"
pass "nemr status says: no login yet on this machine"
nemr start "$NOLOGIN" >/dev/null 2>&1 || die "start must succeed against the placeholder"
pass "the session started against the placeholder"
attach_out=$(echo 'claude -p "Reply with OK" --output-format json < /dev/null 2>&1 | tail -c 300; exit' | nemr attach "$NOLOGIN" 2>&1 | tr -d '\r')
grep -qi 'not logged in\|login\|authenticate' <<<"$attach_out" || { printf '%s\n' "$attach_out" | tail -5 | sed 's/^/   | /'; die "claude -p should say it is not logged in (the control that a placeholder is not a login)"; }
pass "CONTROL: inside the session, claude -p says it is not logged in — the placeholder is not a login"
grep -q 'no Claude login on this machine yet' <<<"$(echo 'true' | nemr attach "$NOLOGIN" 2>&1)" \
    || die "nemr attach did not print the no-login-yet notice"
pass "nemr attach names the state: no Claude login on this machine yet, run /login"
"$REPO/target/release/nemr-cloud" ui --no-open >"$WORK/ui3.log" 2>&1 &
UI3_PID=$!
for _ in $(seq 1 60); do grep -q 'nemr ui: http' "$WORK/ui3.log" && break; sleep 0.25; done
LAUNCH3=$(grep -o 'http://127.0.0.1:[0-9]*/#token=[0-9a-f]*' "$WORK/ui3.log" | head -1)
[[ -n "$LAUNCH3" ]] || { cat "$WORK/ui3.log"; die "the third UI did not print a launch URL"; }
cred_out=$(python3 "$REPO/docs/ui-acceptance.py" credential-step "$LAUNCH3" "$NOLOGIN" 2>"$WORK/cred.err"); cred_rc=$?
if [[ -z "$cred_out" ]]; then grep -v Gtk-Message "$WORK/cred.err" | tail -10 | sed 's/^/   | /'; die "the credential-step driver produced no result (rc=$cred_rc)"; fi
printf '%s\n' "$cred_out" > "$WORK/cred.json"
while IFS=$'\t' read -r ok name detail; do
    if [[ "$ok" == "True" ]]; then pass "$name"; else fail "$name${detail:+ — $detail}"; fi
done < <(python3 -c 'import json,sys; [print(r["ok"], r["name"], r.get("detail",""), sep="\t") for r in json.load(open(sys.argv[1]))["checks"]]' "$WORK/cred.json")
SIGNIN_URL=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["url"])' "$WORK/cred.json")
[[ "$cred_rc" -eq 0 ]] || die "the page did not show the credential step as ruled (details above)"
kill "$UI3_PID" 2>/dev/null; wait "$UI3_PID" 2>/dev/null; UI3_PID=""
nemr stop "$NOLOGIN" >/dev/null 2>&1; nemr delete "$NOLOGIN" --yes >/dev/null 2>&1
output_has "^$NOLOGIN " -- nemr list && die "$NOLOGIN survived its removal"
unset NEMR_HOST_CREDENTIALS
stop_daemon
nemr list >/dev/null 2>&1 || die "a clean daemon did not come back after the seamed one was stopped"
nemr status "$PROJECT" 2>/dev/null | grep -q 'NO LOGIN YET' && die "the clean daemon still sees the seam"
pass "the seamed daemon is gone; a clean one serves the real credential again"

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

if [[ "${NEMR_HUMAN_LOGIN:-0}" == 1 ]]; then
    # -----------------------------------------------------------------------
    step "The credential step — the human arm (E-21): /login inside the pulled session, on this machine"
    # -----------------------------------------------------------------------
    # Only where the ruling's words are literally true: a host with no Claude
    # login. The pulled session above started against the engine's
    # placeholder; a person now signs in through the page's terminal, and
    # the proof the Product Owner required follows: the HOST credential file
    # must contain exactly what Claude Code wrote inside the session.
    # F-14: the host credential lives in a dedicated directory bound over
    # /root/.claude, not ~/.claude. Read its path from the engine rather than
    # assuming it.
    HOST_CRED=$(nemr status "$PROJECT" 2>/dev/null | sed -n 's/.*path: *//p' | tail -1)
    [[ -n "$HOST_CRED" ]] || HOST_CRED="$HOME/.local/share/nemr/host-credential/.credentials.json"
    if [[ -f "$HOST_CRED" ]] && ! grep -q '_nemr_placeholder' "$HOST_CRED"; then
        die "NEMR_HUMAN_LOGIN=1 needs a host with no Claude login; $HOST_CRED is a real credential"
    fi
    nemr status "$PROJECT" 2>/dev/null | grep -q 'NO LOGIN YET' || die "the pulled session should be waiting for a login"
    pass "the pulled session started on this machine against the placeholder (no login yet)"
    "$REPO/target/release/nemr-cloud" ui --no-open >"$WORK/ui-human.log" 2>&1 &
    UI3_PID=$!
    for _ in $(seq 1 60); do grep -q 'nemr ui: http' "$WORK/ui-human.log" && break; sleep 0.25; done
    HUMAN_URL=$(grep -o 'http://127.0.0.1:[0-9]*/#token=[0-9a-f]*' "$WORK/ui-human.log" | head -1)
    printf '\n   >>> Open this in your browser:  %s\n' "$HUMAN_URL"
    printf '   >>> Attach %s, run  claude  then  /login , open the sign-in link the page shows, paste the code.\n' "$PROJECT"
    printf '   >>> Waiting up to 20 minutes for the login to land on this machine...\n'
    landed=0
    for _ in $(seq 1 240); do
        if nemr status "$PROJECT" 2>/dev/null | grep -q 'credential:   present at'; then landed=1; break; fi
        sleep 5
    done
    [[ "$landed" -eq 1 ]] || die "no login landed on this machine within 20 minutes"
    pass "a login landed on this machine: nemr status reports the credential present"
    host_sha=$(sha256sum "$HOST_CRED" | cut -d' ' -f1)
    host_ino=$(stat -c %i "$HOST_CRED")
    # F-14: compare the INODE as well as the hash. A single-file bind could pass
    # the hash on the login's first (in-place) write yet leave a later
    # rename on a container-only inode; equal inodes prove the write landed on
    # the host file itself, through the directory bind.
    inside=$(echo 'printf "SHA=%s INO=%s\n" "$(sha256sum /root/.claude/.credentials.json | cut -d" " -f1)" "$(stat -c %i /root/.claude/.credentials.json)"; exit' | nemr attach "$PROJECT" 2>&1 | tr -d '\r')
    inside_sha=$(grep -oE 'SHA=[0-9a-f]{64}' <<<"$inside" | head -1 | cut -d= -f2)
    inside_ino=$(grep -oE 'INO=[0-9]+' <<<"$inside" | head -1 | cut -d= -f2)
    [[ -n "$inside_sha" && -n "$inside_ino" ]] || die "could not read the credential's hash and inode inside the session"
    [[ "$host_sha" == "$inside_sha" ]] || die "REQUIRED PROOF FAILED: the host file ($host_sha) is not what Claude Code wrote inside ($inside_sha) — Claude Code's write escaped the bind"
    [[ "$host_ino" == "$inside_ino" ]] || die "REQUIRED PROOF FAILED: the host inode ($host_ino) is not the session's ($inside_ino) — a later login write took a new inode inside; the directory bind did not hold"
    pass "REQUIRED PROOF: the host credential file is exactly what Claude Code wrote inside the session — same bytes AND same inode (F-14)"
    grep -q '_nemr_placeholder' "$HOST_CRED" && die "the host file is still the placeholder"
    answer=$(echo 'claude -p "Reply with the single word OK" --output-format json < /dev/null 2>&1 | tail -c 300; exit' | nemr attach "$PROJECT" 2>&1 | tr -d '\r')
    grep -q '"result":"OK' <<<"$answer" || { printf '%s\n' "$answer" | tail -3 | sed 's/^/   | /'; die "claude -p did not answer after the login"; }
    pass "claude -p answers inside the session after the login"
    kill "$UI3_PID" 2>/dev/null; wait "$UI3_PID" 2>/dev/null; UI3_PID=""
fi

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
if [[ "$STORAGE_MODE" == s3 ]]; then
    before=$(sha256sum "$STORED" | cut -d' ' -f1)
else
    before=$(stat -c%Y "$STORED")
fi
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
if [[ "$STORAGE_MODE" == s3 ]]; then
    key2=$(server_log | sed -n 's/.*stored bundle key=\([^ ]*\) bytes=.*/\1/p' | tail -1)
    [[ "$key2" == "$stored_key" ]] || die "the second push went to a different key ($key2)"
    python3 "$REPO/docs/ui-acceptance.py" s3-get "$LAUNCH_URL" "$stored_key" "$WORK/from-bucket-2.bin" >/dev/null \
        || die "the rewritten object is not in the bucket"
    after=$(sha256sum "$WORK/from-bucket-2.bin" | cut -d' ' -f1)
    [[ "$after" != "$before" ]] || die "the object in the bucket was not rewritten by the second push"
    pass "the list shows it stopped, pushed and released; the object in the bucket was rewritten (a different ciphertext at the same key)"
else
    after=$(stat -c%Y "$(find "$WORK/bundles" -type f | head -1)")
    [[ "$after" -gt "$before" ]] || die "the stored bundle was not rewritten by the second push"
    pass "the list shows it stopped, pushed and released; the stored bundle was rewritten"
fi
if [[ "$STORAGE_MODE" == s3 ]]; then
    # The pull that came back byte-identical above came from the bucket:
    # nothing else held the bundle once the local project was deleted.
    pass "the pull on the deleted project came back byte-identical from the bucket (E-20's proof; D-05)"
    # Leave the bucket as it was found: exactly this run's objects removed.
    removed=$(python3 "$REPO/docs/ui-acceptance.py" s3-delete-prefix "$LAUNCH_URL" "$BUNDLE_PREFIX/") \
        || die "could not remove this run's objects from the bucket"
    left=$(python3 "$REPO/docs/ui-acceptance.py" s3-list "$LAUNCH_URL" "$BUNDLE_PREFIX/" | grep -c . || true)
    [[ "$removed" -ge 1 && "$left" -eq 0 ]] || die "cleanup: removed $removed, $left left under $BUNDLE_PREFIX/"
    pass "the bucket is left as it was found: $removed object(s) under this run's prefix removed, none left"
fi

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
