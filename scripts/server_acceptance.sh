#!/usr/bin/env bash
# The acceptance for `nemr server start|stop|status` (E-24 / SPEC 1.149).
#
#   ./scripts/server_acceptance.sh
#
# Nothing has to be exported: the database and the storage come from the
# server's own settings file, `sync.env`, exactly as they do for the server.
# Export something and it wins, key by key, which is the rule everywhere else.
#
# WHAT IT PROVES. The lifecycle in BOTH storage modes — start, status, stop —
# and the four refusals the Product Owner asked for by name: no settings at
# all, no pepper, a Postgres that does not answer, and a store that does not.
# Every refusal is checked for what it must SAY and for what it must never say:
# a password and an S3 endpoint are the two values that must not reach a
# terminal, and both appear in the settings this command reads.
#
# WHAT IT CANNOT PROVE HERE. Nothing about a server nobody is watching: this
# command is foreground by ruling, and the background question is E-24 in
# docs/DECISIONS.md, unruled.
#
# The object-store arm runs only when the caller's environment carries the
# bucket (the same rule docs/ui-acceptance.sh follows). Without it the run says
# so and expects fewer assertions — it never counts a skipped arm as passed.

set -uo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"
# shellcheck source=lib/proc.sh
. "$REPO/scripts/lib/proc.sh"
# shellcheck source=lib/settings.sh
. "$REPO/scripts/lib/settings.sh"
# The server's own settings, for the run that tests the server. The
# environment still wins; this only fills what it does not carry.
nemr_settings_load || exit 1

GREEN=$'\033[32m'; RED=$'\033[31m'; BOLD=$'\033[1m'; RESET=$'\033[0m'
PASS=0; FAIL=0
# Asserted, not merely printed: a run that skipped a case would otherwise say
# PASS with fewer assertions.
if [[ -n "${NEMR_S3_BUCKET:-}" ]]; then
    STORAGE_MODE=s3; EXPECTED_ASSERTIONS=68
else
    STORAGE_MODE=local; EXPECTED_ASSERTIONS=63
fi
step() { printf '\n%s== %s%s\n' "$BOLD" "$1" "$RESET"; }
pass() { PASS=$((PASS+1)); printf '   %sok%s   %s\n' "$GREEN" "$RESET" "$1"; }
fail() { FAIL=$((FAIL+1)); printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1"; }
check() { if [[ "$1" == "0" ]]; then pass "$2"; else fail "$2${3:+ — $3}"; fi; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/nemr-server-acceptance.XXXXXX")"
SERVER_PID=""
cleanup() {
    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill -TERM "$SERVER_PID" 2>/dev/null
        wait "$SERVER_PID" 2>/dev/null
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

nemr_settings_require DATABASE_URL || {
    printf '  A throwaway one:  ./scripts/setup_sync_test_db.sh\n\n' >&2
    exit 1
}
CLOUD="$REPO/target/release/nemr-cloud"
SYNC="$REPO/target/release/nemr-sync"
cargo build --release -p nemr-sync -p nemr-cloud --quiet || { fail "the release build failed"; exit 1; }
[[ -x "$CLOUD" && -x "$SYNC" ]] || { fail "nemr-cloud and nemr-sync must be built"; exit 1; }

ADDR="127.0.0.1:18101"
mkdir -p "$WORK/home" "$WORK/state" "$WORK/bundles"

# Every run of the command under test goes through this: an environment that
# carries nothing of the developer's own. NEMR_SYNC_ENV_FILE= means "read no
# file", so a real ~/.config/nemr/sync.env cannot change what this measures.
server() {   # <extra env>... -- <args>...
    local -a envs=()
    while [[ "${1:-}" != "--" ]]; do envs+=("$1"); shift; done
    shift
    env -i PATH="$PATH" HOME="$WORK/home" XDG_STATE_HOME="$WORK/state" \
        NEMR_SYNC_BIN="$SYNC" NEMR_SYNC_ENV_FILE= \
        "${envs[@]}" "$CLOUD" server "$@" 2>&1
}
# The settings a working server needs, in the shape a caller would set them.
GOOD_LOCAL=(NEMR_BUNDLE_DIR="$WORK/bundles" NEMR_AUTH_PEPPER=ephemeral
            DATABASE_URL="$DATABASE_URL" NEMR_SERVER_ADDR="$ADDR")

# ---------------------------------------------------------------------------
step "It says who it is for"
# ---------------------------------------------------------------------------
help_out="$("$CLOUD" server --help 2>&1)"
grep -qi 'self-hosting and development' <<<"$help_out"
check $? "the help says this is for self-hosting and development"
grep -qi 'hosted product does not need' <<<"$help_out"
check $? "and that the hosted product does not need it"
grep -qi 'does not install or start Postgres' <<<"$help_out"
check $? "and that it does not install or start Postgres"

# ---------------------------------------------------------------------------
step "First run with nothing configured: the file it wants, and no pepper invented"
# ---------------------------------------------------------------------------
out="$(server NEMR_SYNC_ENV_FILE="$WORK/home/.config/nemr/sync.env" -- start)"; rc=$?
check "$([[ $rc -eq 1 ]] && echo 0 || echo 1)" "it refuses (exit 1) rather than starting something half-configured" "exit $rc"
for want in DATABASE_URL NEMR_AUTH_PEPPER NEMR_BUNDLE_DIR NEMR_S3_BUCKET NEMR_SERVER_ADDR; do
    grep -q "$want" <<<"$out" || { fail "the template names $want"; break; }
done
grep -q DATABASE_URL <<<"$out" && grep -q NEMR_AUTH_PEPPER <<<"$out" &&
    grep -q NEMR_BUNDLE_DIR <<<"$out" && grep -q NEMR_S3_BUCKET <<<"$out" &&
    grep -q NEMR_SERVER_ADDR <<<"$out"
check $? "the template names every setting, both backends included"
grep -q "$WORK/home/.config/nemr/sync.env" <<<"$out"
check $? "and names the file it wants, by path"
grep -qi 'NOTHING GENERATES ONE FOR YOU' <<<"$out"
check $? "it says plainly that no pepper is generated for you (E-19)"
grep -q 'NEMR_AUTH_PEPPER=ephemeral' <<<"$out" && grep -qi 'throwaway' <<<"$out"
check $? "and the escape hatch stays explicit: the literal value, marked throwaway"
check "$([[ ! -e "$WORK/home/.config/nemr/sync.env" ]] && echo 0 || echo 1)" \
    "it wrote no file — a refusal that invents a pepper is the thing E-19 forbids"

# ---------------------------------------------------------------------------
step "The refusals, each naming what is wrong and never a secret"
# ---------------------------------------------------------------------------
out="$(server NEMR_BUNDLE_DIR="$WORK/bundles" DATABASE_URL="$DATABASE_URL" -- start)"; rc=$?
grep -q 'NEMR_AUTH_PEPPER' <<<"$out" && grep -q 'sync.env' <<<"$out" && [[ $rc -eq 1 ]]
check $? "no pepper: it refuses, naming the variable and the file" "$(head -3 <<<"$out")"
# AND it refuses BEFORE anything starts. Three mechanisms name the pepper — the
# check in this command, its generic backstop, and E-19's own refusal inside
# the server — so a run that merely SAYS the right thing proves nothing about
# which of them spoke. What this command adds is that no server was ever
# started: with its checks disabled the run reaches "starting" and takes the
# address, and only then dies.
grep -q 'nemr server: starting' <<<"$out" && r=1 || r=0
check $r "and it refuses before starting anything — not after exec'ing a server that then dies"

SECRET="hunter2-$$"
out="$(server "${GOOD_LOCAL[@]}" DATABASE_URL="postgres://nemr:$SECRET@127.0.0.1:5999/nemr" -- start)"; rc=$?
[[ $rc -eq 1 ]] && grep -q '127.0.0.1:5999' <<<"$out"
check $? "Postgres unreachable: it refuses, naming the connection it tried" "$(head -3 <<<"$out")"
grep -q "$SECRET" <<<"$out" && r=1 || r=0
check $r "and the password is redacted — the refusal is safe to paste into a bug report"
grep -q 'setup_sync_test_db.sh' <<<"$out"
check $? "and it says how to get one"
grep -qi 'does not install or start a database' <<<"$out"
check $? "and that starting one is not this command's promise"

out="$(server NEMR_AUTH_PEPPER=ephemeral DATABASE_URL="$DATABASE_URL" \
    NEMR_S3_PROVIDER=r2 NEMR_S3_BUCKET=nemr-acceptance-nowhere \
    NEMR_S3_ENDPOINT=http://127.0.0.1:1 \
    NEMR_S3_ACCESS_KEY_ID=acc-key-$$ NEMR_S3_SECRET_ACCESS_KEY=acc-secret-$$ -- start)"; rc=$?
[[ $rc -eq 1 ]] && grep -q 'r2:nemr-acceptance-nowhere' <<<"$out"
check $? "an unreachable store: it refuses, naming provider:bucket" "$(head -3 <<<"$out")"
grep -qE 'acc-secret-|acc-key-|127.0.0.1:1' <<<"$out" && r=1 || r=0
check $r "and never the endpoint or the credential (E-20: the endpoint carries the account id)"
grep -qi 'before it binds' <<<"$out"
check $? "and says the probe runs before the port binds, not at a user's first push"

mkdir -p "$WORK/readonly"; chmod 500 "$WORK/readonly"
out="$(server NEMR_BUNDLE_DIR="$WORK/readonly" NEMR_AUTH_PEPPER=ephemeral \
    DATABASE_URL="$DATABASE_URL" -- start)"; rc=$?
chmod 700 "$WORK/readonly"
[[ $rc -eq 1 ]] && grep -qi 'not writable' <<<"$out"
check $? "a bundle directory that exists but cannot be written to is refused" "$(head -3 <<<"$out")"
grep -qi 'first push' <<<"$out"
check $? "and the refusal says where it would otherwise have failed instead"

# ---------------------------------------------------------------------------
step "The lifecycle, in a directory-backed server"
# ---------------------------------------------------------------------------
out="$(server "${GOOD_LOCAL[@]}" -- status)"
grep -qE 'running: +no' <<<"$out"
check $? "status with nothing running says so" "$(head -3 <<<"$out")"
grep -qE 'database: .*answers' <<<"$out" && grep -qE 'storage: .*answers' <<<"$out"
check $? "and still answers for Postgres and the store — the five questions in one"

out="$(server "${GOOD_LOCAL[@]}" -- stop)"; rc=$?
[[ $rc -eq 0 ]] && grep -qi 'nothing to stop' <<<"$out"
check $? "stop with nothing running says so and exits 0" "$out"

server "${GOOD_LOCAL[@]}" -- start >"$WORK/server.log" 2>&1 &
SERVER_PID=$!
wait_for_service "the sync server" "$SERVER_PID" "$WORK/server.log" 20 \
    curl -fsS "http://$ADDR/health" || { fail "the server did not come up"; exit 1; }
pass "start brings the server up in the foreground"
grep -qE "address: +$ADDR" "$WORK/server.log" && grep -qE 'storage: +local:' "$WORK/server.log"
check $? "and says the address and the storage it chose before the log begins" "$(head -5 "$WORK/server.log")"
grep -q 'THROWAWAY SERVER' "$WORK/server.log"
check $? "the ephemeral pepper still announces itself loudly (E-19)"

out="$(server "${GOOD_LOCAL[@]}" -- status)"
grep -qE 'running: +yes' <<<"$out" && grep -q "$ADDR" <<<"$out"
check $? "status says it is running, and on what address" "$(head -4 <<<"$out")"
grep -qE '/health: +answers' <<<"$out"
check $? "and that it answers /health"
grep -qE 'storage: +local:.*answers' <<<"$out"
check $? "and which storage backend, and that the backend answers"
grep -qE 'database: .*answers' <<<"$out"
check $? "and that Postgres answers"

out="$(server "${GOOD_LOCAL[@]}" -- start)"; rc=$?
[[ $rc -eq 1 ]] && grep -qi 'already running' <<<"$out"
check $? "a second start refuses rather than fighting for the port" "$(head -3 <<<"$out")"

out="$(server "${GOOD_LOCAL[@]}" -- stop)"; rc=$?
[[ $rc -eq 0 ]] && grep -qi 'stopped' <<<"$out"
check $? "stop stops it" "$out"
wait "$SERVER_PID" 2>/dev/null; SERVER_PID=""
curl -fsS --max-time 2 "http://$ADDR/health" >/dev/null 2>&1 && r=1 || r=0
check $r "and nothing answers on the address afterwards"
check "$([[ ! -e "$WORK/state/nemr/sync-server.pid" ]] && echo 0 || echo 1)" \
    "and the pid file is gone"

# ---------------------------------------------------------------------------
step "A server binary that does not understand --check is stopped, not waited for"
# ---------------------------------------------------------------------------
# MEASURED, not imagined: a nemr-sync older than this client ignores `--check`
# and starts a SERVER. `nemr server status` — which promises to change nothing
# — spawned the installed binary, which opened the real bundle store, bound a
# port and ran for three minutes while status waited for output that was never
# coming. The stand-in below has the same shape: it ignores its arguments and
# does not exit.
cat >"$WORK/old-nemr-sync" <<'OLD'
#!/usr/bin/env bash
# A nemr-sync from before --check existed: the argument means nothing to it and
# it goes on to be a server, which here is a process that does not exit.
exec sleep 300
OLD
chmod +x "$WORK/old-nemr-sync"
before="$(date +%s)"
out="$(server NEMR_SYNC_BIN="$WORK/old-nemr-sync" "${GOOD_LOCAL[@]}" -- status)"; rc=$?
elapsed=$(( $(date +%s) - before ))
[[ $rc -ne 0 ]] && grep -qi 'did not answer' <<<"$out"
check $? "it refuses rather than waiting for a report that is not coming" "$(head -3 <<<"$out")"
check "$([[ $elapsed -lt 40 ]] && echo 0 || echo 1)" \
    "and it refuses on a clock, in ${elapsed}s — a read-only command cannot hang forever"
grep -qi 'older than this client' <<<"$out"
check $? "and says what is most likely wrong, and how to fix it"
sleep 1
pgrep -f "$WORK/old-nemr-sync" >/dev/null && r=1 || r=0
check $r "and the process it started is stopped — a status command leaves no server behind"

# ---------------------------------------------------------------------------
step "A database whose clock has drifted is named, because it looks like a lease bug"
# ---------------------------------------------------------------------------
# Measured on this machine (2026-09-11): the development Postgres container had
# drifted 61 SECONDS ahead of its host, and four of the six lease tests failed
# with "the current holder must be able to write" — a lease that had just been
# taken looked long expired. Restarting the container fixed it and they passed
# twice. The skew is now reported, so the next hour is not spent the same way.
cat >"$WORK/skewed-check" <<'SKEW'
#!/usr/bin/env bash
# A preflight report from a server whose database is a minute ahead.
cat <<'REPORT'
env_file_state=disabled
settings_state=ok
addr=127.0.0.1:18103
pepper=configured
backend=local:/tmp
backend_writable=yes
backend_state=ok
database=postgres://nemr:***@127.0.0.1:5433/nemr
database_state=ok
database_clock_skew_ms=61000
REPORT
SKEW
chmod +x "$WORK/skewed-check"
out="$(server NEMR_SYNC_BIN="$WORK/skewed-check" "${GOOD_LOCAL[@]}" -- status)"
grep -qi 'the database is 61.0s AHEAD' <<<"$out"
check $? "status says the database clock has drifted, and by how much" "$(grep -i clock <<<"$out")"
grep -qi 'look like a lease bug' <<<"$out"
check $? "and says what it will look like instead, so the hour is not spent on the lease"

# ---------------------------------------------------------------------------
step "A record is a claim about a pid, and it is verified before it is believed"
# ---------------------------------------------------------------------------
# The file says a server is running. It names THIS SCRIPT's pid, which is alive
# and is not a sync server. A stop that trusted the number would signal the
# shell that asked it to — which is how this project has twice killed itself.
mkdir -p "$WORK/state/nemr/cloud"
printf 'pid=%s\naddr=%s\nbackend=local:/nowhere\n' "$$" "$ADDR" \
    >"$WORK/state/nemr/cloud/server"
out="$(server "${GOOD_LOCAL[@]}" -- stop)"; rc=$?
check "$([[ $rc -eq 0 ]] && echo 0 || echo 1)" "a record naming a live process that is NOT the server is not acted on" "$out"
grep -qi 'nothing to stop' <<<"$out"
check $? "it says there is nothing to stop rather than signalling a stranger"
check "$([[ -n "$(ps -o pid= -p $$ 2>/dev/null)" ]] && echo 0 || echo 1)" \
    "and this script is still alive to say so"

# ---------------------------------------------------------------------------
if [[ "$STORAGE_MODE" == s3 ]]; then
step "The same, in an object-store server (E-20)"
# ---------------------------------------------------------------------------
# The bucket comes from the caller's environment and is never echoed. A
# per-run prefix keeps this run's objects apart from anything else there.
S3_ENV=(NEMR_S3_PROVIDER="$NEMR_S3_PROVIDER" NEMR_S3_BUCKET="$NEMR_S3_BUCKET"
        NEMR_S3_ENDPOINT="$NEMR_S3_ENDPOINT" NEMR_S3_ACCESS_KEY_ID="$NEMR_S3_ACCESS_KEY_ID"
        NEMR_S3_SECRET_ACCESS_KEY="$NEMR_S3_SECRET_ACCESS_KEY"
        NEMR_BUNDLE_PREFIX="server-acc-$$" NEMR_AUTH_PEPPER=ephemeral
        DATABASE_URL="$DATABASE_URL" NEMR_SERVER_ADDR="$ADDR")
out="$(server "${S3_ENV[@]}" -- status)"
grep -qE "storage: +${NEMR_S3_PROVIDER}:${NEMR_S3_BUCKET}.*answers" <<<"$out"
check $? "status reaches the real bucket and says it answers" "$(grep storage <<<"$out")"
grep -q "$NEMR_S3_SECRET_ACCESS_KEY" <<<"$out" && r=1 || r=0
check $r "and prints no credential"
grep -q "$NEMR_S3_ENDPOINT" <<<"$out" && r=1 || r=0
check $r "and no endpoint (it carries the account id)"

server "${S3_ENV[@]}" -- start >"$WORK/s3.log" 2>&1 &
SERVER_PID=$!
wait_for_service "the object-store server" "$SERVER_PID" "$WORK/s3.log" 30 \
    curl -fsS "http://$ADDR/health" || { fail "the object-store server did not come up"; exit 1; }
pass "start brings up a server backed by the object store"
out="$(server "${S3_ENV[@]}" -- stop)"
grep -qi 'stopped' <<<"$out"
check $? "and stop stops that one too" "$out"
wait "$SERVER_PID" 2>/dev/null; SERVER_PID=""
else
    printf '\n   %s(the object-store arm did not run: NEMR_S3_BUCKET is not in this environment)%s\n' \
        "$BOLD" "$RESET"
fi

# ---------------------------------------------------------------------------
step "sync.env is the only file: nothing exported, everything from it (SPEC 1.151)"
# ---------------------------------------------------------------------------
# THE COMPLAINT: "I still set variables by hand every session — set -a; source
# ~/.config/nemr/r2.env; set +a — and export DATABASE_URL=...". The server has
# always been able to read all of it from sync.env (E-20 put NEMR_S3_* there on
# purpose); nothing proved it, and one script said the opposite. This proves it,
# with `env -i`: not one variable is exported, and the file carries everything.
SOLO="$WORK/solo"; mkdir -p "$SOLO"
solo_env() {   # <file> — a complete settings file, mode 0600
    umask 077
    { printf 'NEMR_SERVER_ADDR=%s\n' "127.0.0.1:18131"
      printf 'DATABASE_URL=%s\n' "$DATABASE_URL"
      printf 'NEMR_AUTH_PEPPER=ephemeral\n'
      cat; } > "$1"
    chmod 600 "$1"
}
mkdir -p "$SOLO/bundles"
printf 'NEMR_BUNDLE_DIR=%s\n' "$SOLO/bundles" | solo_env "$SOLO/local.env"
out="$(env -i PATH="$PATH" HOME="$SOLO" NEMR_SYNC_BIN="$SYNC" NEMR_SYNC_ENV_FILE="$SOLO/local.env" \
    "$CLOUD" server status 2>&1)"
grep -qE 'database: .*answers' <<<"$out" && grep -qE 'storage: .*answers' <<<"$out"
check $? "with NOTHING exported, a directory-backed server reads it all from the file" \
    "$(head -6 <<<"$out")"
grep -q "$SOLO/local.env" <<<"$out"
check $? "and says which file it read"

# ---------------------------------------------------------------------------
step "nemr server configure writes that file, and refuses rather than surprising you"
# ---------------------------------------------------------------------------
CONF="$WORK/conf"; mkdir -p "$CONF"
configure() {   # <stdin...> — a real pty, because it asks questions
    # NEMR_SYNC_ENV_FILE is cleared deliberately: it names a file, and this
    # run is about the file the command CHOOSES from XDG_CONFIG_HOME. With it
    # set, the caller's own settings file would be the subject.
    printf '%s\n' "$@" | script -qec \
        "env -u NEMR_SYNC_ENV_FILE HOME=$CONF XDG_CONFIG_HOME=$CONF/config NEMR_SYNC_BIN=$SYNC \
             DATABASE_URL=$DATABASE_URL $CLOUD server configure" /dev/null 2>&1
}
# No terminal at all: it asks questions, so it refuses rather than guessing.
out="$(env -u NEMR_SYNC_ENV_FILE HOME="$CONF" XDG_CONFIG_HOME="$CONF/config" NEMR_SYNC_BIN="$SYNC" \
    "$CLOUD" server configure </dev/null 2>&1)"; rc=$?
[[ $rc -eq 2 ]] && grep -qi 'needs a terminal' <<<"$out"
check $? "with no terminal it refuses, saying why" "$(head -3 <<<"$out")"

# The happy path: port, storage 1 (a folder), the folder, the database.
out="$(configure "18132" "1" "$CONF/bundles" "$DATABASE_URL")"
written="$CONF/config/nemr/sync.env"
check "$([[ -f "$written" ]] && echo 0 || echo 1)" "it writes the file" "$(tail -4 <<<"$out")"
check "$([[ "$(stat -c '%a' "$written" 2>/dev/null)" == "600" ]] && echo 0 || echo 1)" \
    "0600, because it holds the pepper" "$(stat -c '%a' "$written" 2>/dev/null)"
grep -q 'THIS IS A SECRETS FILE' "$written"
check $? "and its header says so in as many words"
grep -qE '^NEMR_AUTH_PEPPER=.{40,}' "$written"
check $? "it generated the pepper itself — nobody was asked to invent one"
grep -qE '^NEMR_BUNDLE_DIR=' "$written" && ! grep -q '^NEMR_S3_' "$written"
check $? "exactly one storage backend is written (E-20)"
grep -qi 'nemr server start' <<<"$out"
check $? "and it ends by saying what to run next"
# THE SECRET IS NEVER SHOWN. Not in the confirmation, not anywhere.
pepper="$(sed -n 's/^NEMR_AUTH_PEPPER=//p' "$written")"
grep -qF "$pepper" <<<"$out" && r=1 || r=0
check $r "the pepper it generated does not appear in its own output"
# A file that exists is never overwritten, and never silently.
out="$(configure "18132" "1" "$CONF/bundles" "$DATABASE_URL")"; rc=$?
[[ $rc -ne 0 ]] && grep -q "already exists" <<<"$out"
check $? "a second run refuses rather than overwriting, and names the file" "$(head -3 <<<"$out")"

# And the file it wrote starts a server, with nothing exported.
out="$(env -i PATH="$PATH" HOME="$CONF" NEMR_SYNC_BIN="$SYNC" \
    NEMR_SYNC_ENV_FILE="$written" "$CLOUD" server status 2>&1)"
grep -qE 'database: .*answers' <<<"$out" && grep -qE 'storage: .*answers' <<<"$out"
check $? "the file it wrote answers for both, with nothing exported" "$(head -6 <<<"$out")"

# ---------------------------------------------------------------------------
step "One voice: result first, colour with meaning, and none of it when piped"
# ---------------------------------------------------------------------------
# Every command opens with a line that says whether it worked. Piped, that line
# is `OK`/`XX`/`!!` and there is not one escape byte anywhere.
for spec in "server status:$SOLO/local.env" "server stop:$SOLO/local.env"; do
    cmd="${spec%%:*}"; envf="${spec##*:}"
    # shellcheck disable=SC2086
    out="$(env -i PATH="$PATH" HOME="$SOLO" XDG_STATE_HOME="$SOLO/state" \
        NEMR_SYNC_BIN="$SYNC" NEMR_SYNC_ENV_FILE="$envf" "$CLOUD" $cmd 2>&1 || true)"
    head -1 <<<"$out" | grep -qE '^(OK|XX|!!) '
    check $? "nemr $cmd piped: the first line is the result" "$(head -1 <<<"$out")"
    LC_ALL=C grep -q $'\033' <<<"$out" && r=1 || r=0
    check $r "nemr $cmd piped: not one escape byte"
done
# On a terminal the same command is coloured, and the VALUE carries it.
out="$(script -qec "env HOME=$SOLO XDG_STATE_HOME=$SOLO/state NEMR_SYNC_BIN=$SYNC \
    NEMR_SYNC_ENV_FILE=$SOLO/local.env $CLOUD server status" /dev/null 2>&1)"
LC_ALL=C grep -aq $'\033\[32m' <<<"$out"
check $? "on a terminal it is coloured"
LC_ALL=C grep -aqE $'running\033\\[0m\033\\[2m:' <<<"$out"
check $? "the label is dim and the value is what carries the colour"
# NO_COLOR and TERM=dumb turn it off, like everything else here.
for way in "NO_COLOR=1" "TERM=dumb"; do
    out="$(script -qec "env $way HOME=$SOLO XDG_STATE_HOME=$SOLO/state NEMR_SYNC_BIN=$SYNC \
        NEMR_SYNC_ENV_FILE=$SOLO/local.env $CLOUD server status" /dev/null 2>&1)"
    LC_ALL=C grep -aq $'\033\[3' <<<"$out" && r=1 || r=0
    check $r "$way: no colour, even on a terminal"
done

# ---------------------------------------------------------------------------
printf '\n'
if (( FAIL > 0 )); then
    printf '%sFAIL%s — %d assertion(s) failed, %d passed.\n' "$RED" "$RESET" "$FAIL" "$PASS"
    exit 1
fi
if (( PASS != EXPECTED_ASSERTIONS )); then
    printf '%sFAIL%s — expected %d assertions in %s mode, counted %d: a case was skipped or added without raising EXPECTED_ASSERTIONS.\n' \
        "$RED" "$RESET" "$EXPECTED_ASSERTIONS" "$STORAGE_MODE" "$PASS"
    exit 1
fi
printf '%sPASS%s — %d assertions (%s storage).\n' "$GREEN" "$RESET" "$PASS" "$STORAGE_MODE"
