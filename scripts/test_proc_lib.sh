#!/usr/bin/env bash
#
# Tests for scripts/lib/proc.sh.
#
# The helper exists to make failures legible, so its tests assert on what it
# SAYS, not only on its exit status. A wait helper that returns 1 without
# explaining is the defect it was written to end.
#
#   ./scripts/test_proc_lib.sh

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
. scripts/lib/proc.sh

# Fast polls: these tests are about behaviour, not patience.
NEMR_WAIT_INTERVAL=0.05

GREEN=$'\033[32m'; RED=$'\033[31m'; RESET=$'\033[0m'
PASS=0; FAIL=0
# F-6: the count is ASSERTED, not printed. The gate audit (2026-09-09) found
# six of this project's eight acceptance scripts exiting 0 over a tally nobody
# checked — a run that skipped a step read exactly like a run that made every
# assertion. Raise this number when a step is added; a run that counts anything
# else fails.
# Measured: three consecutive clean runs, 34 each.
EXPECTED_ASSERTIONS=34
ok()   { PASS=$((PASS+1)); printf '%sok%s   %s\n' "$GREEN" "$RESET" "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '%sFAIL%s %s\n' "$RED" "$RESET" "$1" >&2; }
check() { # <desc> <haystack> <needle>
    if [[ "$2" == *"$3"* ]]; then ok "$1"; else
        bad "$1 — expected to find: $3"
        printf '   got: %s\n' "$2" >&2
    fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# --- wait_for_service: the happy path ---------------------------------------
: > "$WORK/a.log"
sleep 5 & PID=$!
out=$(wait_for_service "probe-a" "$PID" "$WORK/a.log" 2 true 2>&1); rc=$?
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
[[ $rc -eq 0 ]] && ok "succeeds when the probe succeeds" || bad "should have succeeded (rc=$rc)"
check "says which service answered" "$out" "probe-a is answering"

# --- a process that dies: must be detected, with its status -----------------
: > "$WORK/b.log"
( exit 3 ) & PID=$!
sleep 0.2
out=$(wait_for_service "probe-b" "$PID" "$WORK/b.log" 10 false 2>&1); rc=$?
[[ $rc -ne 0 ]] && ok "fails when the process is dead" || bad "should have failed"
check "reports the exit status, not just 'did not come up'" "$out" "exited with status 3"

# --- and it must not burn the whole window on a corpse ----------------------
( exit 1 ) & PID=$!
sleep 0.2
start=$SECONDS
wait_for_service "probe-c" "$PID" "$WORK/b.log" 20 false >/dev/null 2>&1
elapsed=$(( SECONDS - start ))
[[ $elapsed -lt 5 ]] && ok "aborts the wait when the process dies (${elapsed}s of a 20s window)" \
    || bad "burned ${elapsed}s waiting on a dead process"

# --- alive but silent: distinguished from dead ------------------------------
: > "$WORK/c.log"
sleep 5 & PID=$!
out=$(wait_for_service "probe-d" "$PID" "$WORK/c.log" 1 false 2>&1); rc=$?
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
[[ $rc -ne 0 ]] && ok "fails when a live process never answers" || bad "should have failed"
check "distinguishes alive-but-silent from exited" "$out" "still running but did not answer"

# --- the log: printed, prefixed, and an empty one SAYS it is empty ----------
printf 'line one\nline two\n' > "$WORK/d.log"
sleep 5 & PID=$!
out=$(wait_for_service "probe-e" "$PID" "$WORK/d.log" 1 false 2>&1)
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
check "prints the log contents" "$out" "line two"
check "prefixes log lines so they are distinguishable" "$out" "| line one"

: > "$WORK/empty.log"
sleep 5 & PID=$!
out=$(wait_for_service "probe-f" "$PID" "$WORK/empty.log" 1 false 2>&1)
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
check "an EMPTY log says so rather than printing nothing" "$out" "empty — the process wrote nothing"

sleep 5 & PID=$!
out=$(wait_for_service "probe-g" "$PID" "$WORK/does-not-exist.log" 1 false 2>&1)
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null
check "a MISSING log says so rather than printing nothing" "$out" "no such file"

# --- wait_for_ready ---------------------------------------------------------
out=$(wait_for_ready "thing-a" 2 true 2>&1); rc=$?
[[ $rc -eq 0 ]] && ok "wait_for_ready succeeds on a passing probe" || bad "should have succeeded"
out=$(wait_for_ready "thing-b" 1 false 2>&1); rc=$?
[[ $rc -ne 0 ]] && ok "wait_for_ready fails on a failing probe" || bad "should have failed"
check "wait_for_ready names what it probed" "$out" "probed with: false"

# --- require_tcp ------------------------------------------------------------
# Self-contained: bind a port here rather than depending on the host's services.
python3 -c "
import socket,time,sys
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
s.bind(('127.0.0.1',0)); s.listen(1)
print(s.getsockname()[1],flush=True)
time.sleep(20)
" > "$WORK/port.txt" &
LISTENER=$!
for _ in $(seq 1 40); do [[ -s "$WORK/port.txt" ]] && break; sleep 0.1; done
PORT=$(cat "$WORK/port.txt")
out=$(require_tcp 127.0.0.1 "$PORT" "the probe listener" "start it" 2>&1); rc=$?
[[ $rc -eq 0 ]] && ok "require_tcp succeeds against a real listener" || bad "should have succeeded: $out"
kill "$LISTENER" 2>/dev/null; wait "$LISTENER" 2>/dev/null

out=$(require_tcp 127.0.0.1 "$PORT" "the probe listener" "run ./scripts/start-it.sh" 2>&1); rc=$?
[[ $rc -ne 0 ]] && ok "require_tcp fails once the listener is gone" || bad "should have failed"
check "require_tcp names the remedy" "$out" "run ./scripts/start-it.sh"
check "require_tcp says this is not the dependent's failure" "$out" "not a failure of whatever needed it"


# --- output_has -------------------------------------------------------------
# The helper exists to keep three outcomes apart that `cmd | grep -q` collapses
# into two. Each is asserted, and the third is asserted with a CONTROL showing
# the old shape getting it wrong on the same input.

output_has "^b" -- printf 'a\nb\nc\n'; rc=$?
[[ $rc -eq 0 ]] && ok "output_has says yes when the output matches" || bad "should have matched"

output_has "^z" -- printf 'a\nb\nc\n'; rc=$?
[[ $rc -eq 1 ]] && ok "output_has says no when the output does not match" || bad "should have missed"

# A command that FAILS while its output happens to contain the pattern. Read as
# a pipeline this is the worst case: the answer is "yes" and the instrument is
# broken, and `grep -q` reports "no" either way.
out=$( (output_has "^b" -- bash -c 'echo b; echo "boom" >&2; exit 3') 2>&1 ); rc=$?
[[ $rc -ne 0 ]] && ok "output_has refuses to answer when the command failed" || bad "should have refused"
check "it names the exit status"              "$out" "exited 3"
check "it says this is not an absence"        "$out" 'not "^b is absent"'
check "it prints the command's stderr"        "$out" "boom"

# CONTROL: the shape this replaces, on the same input, under the same options.
# It reports a failure indistinguishable from "the pattern was absent" — which
# is the whole defect, so it is proven here rather than asserted in a comment.
( set -o pipefail; bash -c 'echo b; echo "boom" >&2; exit 3' 2>/dev/null | grep -q "^b" ); ctl=$?
[[ $ctl -ne 0 ]] \
    && ok "CONTROL: \`cmd | grep -q\` reports failure for a MATCH, indistinguishably" \
    || bad "control did not reproduce the conflation — the helper's premise is wrong"

# The haystack is kept, so an assertion can show what it actually saw.
output_has "^z" -- printf 'a\nb\n' || true
check "output_has leaves the output for the caller to print" "$_LAST_OUTPUT" "b"

# --- delete_disposable (the protected-subject guard, F-123) ------------------
# Proven with a STUB nemr, so the refusal is shown to happen BEFORE any real
# command would run — and so this test needs no host and cannot touch anything.
# The stub records its calls in a FILE, because the assertions run the guard
# inside $(...) subshells and a variable set there never reaches this shell —
# the first version of this test failed on its own instrumentation.
STUB_LOG="$WORK/nemr-stub-calls"
: > "$STUB_LOG"
nemr() { echo "$*" >> "$STUB_LOG"; }

out=$(delete_disposable "scratch-123" 2>&1); rc=$?
[[ $rc -eq 0 ]] && ok "delete_disposable deletes a disposable project" || bad "should have deleted"
grep -q "delete scratch-123" "$STUB_LOG" && ok "  ...by actually invoking the engine" || bad "the engine was never invoked"

# The list is empty since htmltest was retired (2026-09-06), so the guard is
# proven the way the Rust harness proves it: a disposable name temporarily
# marked protected. The mechanism is what is under test, not the roster.
saved_protected="$NEMR_PROTECTED_SUBJECTS"
NEMR_PROTECTED_SUBJECTS="keep-me"
: > "$STUB_LOG"
out=$(delete_disposable "keep-me" 2>&1); rc=$?
[[ $rc -ne 0 ]] && ok "delete_disposable REFUSES the protected subject" || bad "keep-me was not refused"
check "the refusal names the rule" "$out" "protected subject"
[[ ! -s "$STUB_LOG" ]] \
    && ok "  ...and the engine was NEVER invoked — refusal precedes any command" \
    || bad "the engine was invoked for a protected name: $(cat "$STUB_LOG")"
unset -f nemr

# refuse_protected alone — the flow-step guard. Both directions.
out=$(refuse_protected "scratch-1" 2>&1); rc=$?
[[ $rc -eq 0 ]] && ok "refuse_protected passes a disposable name" || bad "refused a disposable name"
out=$(refuse_protected "keep-me" 2>&1); rc=$?
[[ $rc -ne 0 ]] && ok "refuse_protected refuses the protected subject" || bad "did not refuse"
NEMR_PROTECTED_SUBJECTS="$saved_protected"
# And with the roster as shipped (empty today), the same name passes: the
# refusal above came from the list, not from the name.
out=$(refuse_protected "keep-me" 2>&1); rc=$?
[[ $rc -eq 0 ]] && ok "refuse_protected passes the same name once it is off the list" || bad "refused a name that is not on the list"

printf '\n'
if [[ $FAIL -eq 0 ]]; then
    if [[ "$PASS" -ne "$EXPECTED_ASSERTIONS" ]]; then
        printf '%sFAIL%s — %d assertions, %d expected: a case was skipped, or one was\n' \
            "$RED" "$RESET" "$PASS" "$EXPECTED_ASSERTIONS"
        printf '       added without raising EXPECTED_ASSERTIONS (F-6).\n'
        exit 1
    fi
    printf '%sPASS%s — %d assertions, all %d expected.\n' \
        "$GREEN" "$RESET" "$PASS" "$EXPECTED_ASSERTIONS"; exit 0
fi
printf '%sFAIL%s — %d passed, %d failed.\n' "$RED" "$RESET" "$PASS" "$FAIL"; exit 1
