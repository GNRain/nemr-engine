#!/usr/bin/env bash
#
# Acceptance for the installers (D-14).
#
#   ./scripts/test_install.sh
#
# WHAT THIS CAN PROVE HERE, AND WHAT IT CANNOT.
#
# It proves, on any host that already passes: every preflight refusal, by name
# and with nothing changed; that the plan is shown in full and names every
# privileged command the script can actually run; the consent rule in both
# directions (a terminal that declines, and no terminal at all); that a second
# run does nothing and says so; and all four rules the animation is held to.
#
# It CANNOT prove the first run on a machine with none of it present. That
# needs sudo, a reboot for cgroup delegation, a GHCR pull and a host that has
# never been provisioned — so it is a VM-snapshot arm, scripted step by step in
# docs/install-acceptance.md. Nothing here pretends to cover it.
#
# Exit codes: 0 every assertion passed; 1 an assertion failed.

set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
REPO="$PWD"

GREEN=$'\033[32m'; RED=$'\033[31m'; BOLD=$'\033[1m'; RESET=$'\033[0m'
PASS=0; FAIL=0
# Asserted, not merely printed: a run that skipped a case would otherwise say
# PASS with fewer assertions — the green-over-nothing shape this project keeps
# guarding against. Raise this when a case is added.
EXPECTED_ASSERTIONS=37

step() { printf '\n%s== %s%s\n' "$BOLD" "$1" "$RESET"; }
pass() { PASS=$((PASS + 1)); printf '   %sok%s   %s\n' "$GREEN" "$RESET" "$1"; }
fail() { FAIL=$((FAIL + 1)); printf '   %sFAIL%s %s\n' "$RED" "$RESET" "$1"; }
check() { if [[ "$1" == "0" ]]; then pass "$2"; else fail "$2${3:+ — $3}"; fi; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/nemr-install-acceptance.XXXXXX")"
STATE_DIR="$HOME/.local/state/nemr"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# A run of the installer with no terminal on either side. Prints its output;
# the caller reads the exit code from $RC.
RC=0
run_piped() { local out; out="$("$@" 2>&1)"; RC=$?; printf '%s' "$out"; }

# Count the log files, to prove a refused or declined run creates none.
logs_now() { ls -1 "$STATE_DIR"/install-*.log 2>/dev/null | wc -l; }

# ---------------------------------------------------------------------------
step "Preflight refuses, names what is missing, and changes nothing"
# ---------------------------------------------------------------------------
# The Rust toolchain, removed for real rather than seamed: a PATH without it.
shim="$WORK/nocargo"; mkdir -p "$shim"
for c in $(compgen -c | sort -u | head -0); do :; done   # (no shims; PATH is filtered instead)
filtered_path="$(printf '%s' "$PATH" | tr ':' '\n' | grep -v cargo | grep -v rustup | paste -sd:)"

before_logs="$(logs_now)"
out="$(env PATH="$filtered_path" ./scripts/install.sh 2>&1)"; rc=$?
check "$([[ $rc -eq 1 ]] && echo 0 || echo 1)" "a missing prerequisite exits 1, not 0" "exit $rc"
grep -q "the Rust toolchain" <<<"$out" && grep -q "rustup" <<<"$out"
check $? "it names the Rust toolchain and how to get it (rustup)"
grep -q "nothing has been changed" <<<"$out"
check $? "it says nothing has been changed"
grep -q "the plan for this machine" <<<"$out" && r=1 || r=0
check $r "it refuses BEFORE the plan — a host that cannot run this is never half-installed"
check "$([[ "$(logs_now)" == "$before_logs" ]] && echo 0 || echo 1)" "a refused run writes no log file"

out="$(NEMR_TEST_KERNEL=4.19.0 ./scripts/install.sh 2>&1)"; rc=$?
grep -q "a kernel of 5.8 or newer" <<<"$out" && [[ $rc -eq 1 ]]
check $? "an old kernel is refused by name (found: 4.19.0)"

out="$(NEMR_TEST_CGROUP_MARKER=/nemr/no/such/path ./scripts/install.sh 2>&1)"
grep -q "the cgroup v2 unified hierarchy" <<<"$out"
check $? "a cgroup v1 host is refused by name"

printf '1\n' >"$WORK/userns"
out="$(NEMR_TEST_USERNS_KNOB="$WORK/userns" ./scripts/install.sh 2>&1)"
grep -q "apparmor_restrict_unprivileged_userns = 0" <<<"$out" && grep -q "sysctl" <<<"$out"
check $? "the apparmor userns knob is refused by name, with the sysctl to fix it"

out="$(NEMR_TEST_AVAIL_KB=1048576 ./scripts/install.sh 2>&1)"
grep -q "at least 5 GiB free" <<<"$out"
check $? "too little disk is refused by name, before the build fails late"

out="$(NEMR_TEST_KERNEL=4.19.0 NEMR_TEST_AVAIL_KB=1048576 ./scripts/install.sh 2>&1)"
[[ "$(grep -c '  needs: ' <<<"$out")" == "2" ]]
check $? "every missing prerequisite is listed at once, not one per run"

# ---------------------------------------------------------------------------
step "The plan is shown in full, and hides no privileged action"
# ---------------------------------------------------------------------------
plan="$(./scripts/install.sh 2>&1)"
for section in "Steps" "Files it writes" "Privileged actions" "What it downloads" "What it will not do"; do
    grep -q "^$section" <<<"$plan" || { fail "the plan has no '$section' section"; break; }
done
grep -q "^Steps" <<<"$plan" && grep -q "^Files it writes" <<<"$plan" &&
    grep -q "^Privileged actions" <<<"$plan" && grep -q "^What it downloads" <<<"$plan" &&
    grep -q "^What it will not do" <<<"$plan"
check $? "the plan names its steps, the files it writes, the sudo it runs and what it downloads"

# Every `sudo <command>` the script can actually run has to appear in the plan.
# This is what stops a privileged action being added without being disclosed.
undisclosed=""
while read -r cmd; do
    [[ -z "$cmd" ]] && continue
    grep -qF "$cmd" <<<"$plan" || undisclosed+="$cmd "
done < <(grep -oE '(logged|logged_long) sudo [a-z-]+' scripts/install.sh | awk '{print $3}' | sort -u)
check "$([[ -z "$undisclosed" ]] && echo 0 || echo 1)" \
    "every sudo command in the script is named in the plan" "undisclosed: $undisclosed"

grep -q "visudo -c BEFORE it is installed" <<<"$plan"
check $? "the plan says the sudoers file is validated before it is installed"
grep -q "digest" <<<"$plan" && grep -qE "sha256:[0-9a-f]{64}" <<<"$plan"
check $? "the plan names the exact image digest it will pull"

# ---------------------------------------------------------------------------
step "Consent: it asks once, and refuses rather than proceeding silently"
# ---------------------------------------------------------------------------
before_logs="$(logs_now)"
out="$(./scripts/install.sh 2>&1)"; rc=$?
[[ $rc -eq 2 ]] && grep -q -- "--yes" <<<"$out" && grep -q "Refusing to go ahead without asking" <<<"$out"
check $? "no terminal and no --yes: it refuses (exit 2) and names the flag (F-15)"
# The plan must come first even in the refusal, so the reader sees what they
# would be accepting.
plan_line="$(grep -n "the plan for this machine" <<<"$out" | cut -d: -f1)"
refuse_line="$(grep -n "Refusing to go ahead" <<<"$out" | cut -d: -f1)"
check "$([[ -n "$plan_line" && -n "$refuse_line" && "$plan_line" -lt "$refuse_line" ]] && echo 0 || echo 1)" \
    "the whole plan is shown before the refusal"
check "$([[ "$(logs_now)" == "$before_logs" ]] && echo 0 || echo 1)" "the refused run wrote no log file"

# A terminal that says no. `script` gives a real pty; "n" is fed into it.
out="$(echo n | script -qec './scripts/install.sh' /dev/null 2>&1)"; rc=$?
[[ $rc -eq 0 ]] && grep -q "Nothing was changed" <<<"$out"
check $? "declining at the prompt exits 0 and says nothing was changed"
check "$([[ "$(logs_now)" == "$before_logs" ]] && echo 0 || echo 1)" "declining wrote no log file — so 'n' is the dry run"

# ---------------------------------------------------------------------------
step "A second run does nothing, and says what it skipped"
# ---------------------------------------------------------------------------
out="$(./scripts/install.sh --yes 2>&1)"; rc=$?
check "$([[ $rc -eq 0 ]] && echo 0 || echo 1)" "the run succeeds" "exit $rc; see $STATE_DIR"
did_work="$(sed -n '/^Installing/,/^Verifying/p' <<<"$out" | grep -E '✓' | grep -vE 'already done|already current|at the recorded digest|Claude Code found' || true)"
check "$([[ -z "$did_work" ]] && echo 0 || echo 1)" \
    "every step reports already done — nothing was redone" "${did_work:-}"
grep -q "the smoke test passed" <<<"$out"
check $? "it verifies with the smoke test rather than trusting exit codes"
grep -q "nemr create myproject" <<<"$out" && grep -q "nemr ui" <<<"$out"
check $? "it ends by saying what to do next"

# ---------------------------------------------------------------------------
step "The animation: it never gates progress, and never appears where it should not"
# ---------------------------------------------------------------------------
# 1. Never gates progress. The same work, with the drawing forced on and forced
#    off, must take the same time — the work runs in its own process.
timed() {
    local t0 t1
    t0=$(date +%s.%N)
    ( . scripts/lib/cat.sh; . scripts/lib/steps.sh; LOG=/dev/null; logged_long sleep 2 ) >/dev/null 2>&1
    t1=$(date +%s.%N)
    awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.2f", b-a}'
}
off="$(NEMR_CAT=0 timed)"; on="$(NEMR_CAT=1 timed)"
check "$(awk -v a="$off" -v b="$on" 'BEGIN{print (b-a < 0.5 && b-a > -0.5) ? 0 : 1}')" \
    "a 2s step takes the same time with the animation on (${on}s) and off (${off}s)"

# 2. No terminal, no animation: a piped run carries not one escape byte.
esc="$(./scripts/install.sh --yes 2>&1 | grep -c $'\033' || true)"
check "$([[ "$esc" == "0" ]] && echo 0 || echo 1)" "a piped run emits no escape sequences at all" "$esc lines"

# 3. On a terminal it draws, hides the cursor, and gives it back.
raw="$WORK/pty.raw"
script -qec './scripts/install.sh --yes' /dev/null >"$raw" 2>&1
frames="$(grep -c '(")_(")' "$raw" || true)"
check "$([[ "$frames" -gt 10 ]] && echo 0 || echo 1)" "on a terminal the cat plays ($frames frames drawn)"
hide="$(grep -o $'\033\[?25l' "$raw" | wc -l)"; show="$(grep -o $'\033\[?25h' "$raw" | wc -l)"
check "$([[ "$hide" -gt 0 && "$hide" == "$show" ]] && echo 0 || echo 1)" \
    "the cursor is hidden and given back the same number of times ($hide/$show)"
clears="$(grep -c $'\r\033\[J' "$raw" || true)"
check "$([[ "$clears" == "$hide" ]] && echo 0 || echo 1)" \
    "each animated step erases its own lines, leaving the tick ($clears cleared / $hide drawn)"

# 4. The three ways to turn it off, each on a real terminal.
for way in "NO_COLOR=1" "TERM=dumb" "--quiet"; do
    if [[ "$way" == "--quiet" ]]; then
        script -qec './scripts/install.sh --yes --quiet' /dev/null >"$WORK/off.raw" 2>&1
    else
        script -qec "env $way ./scripts/install.sh --yes" /dev/null >"$WORK/off.raw" 2>&1
    fi
    n="$(grep -c '(")_(")' "$WORK/off.raw" || true)"
    check "$([[ "$n" == "0" ]] && echo 0 || echo 1)" "$way draws nothing" "$n frames"
done

# 5. Interrupted, it leaves no hidden cursor and no half a cat.
script -qec './scripts/lib/cat.sh --demo 20' /dev/null >"$WORK/int.raw" 2>&1 &
sp=$!
sleep 1.5
# The demo's OWN process, not the `script` wrapper whose command line carries
# the same words — and never this script's process group. Signalling by pattern
# has twice in this project killed the shell doing the signalling; this is the
# guard, not care.
demo=""
for p in $(pgrep -f 'cat\.sh --demo' 2>/dev/null); do
    [[ "$(cat "/proc/$p/comm" 2>/dev/null)" == "bash" ]] || continue
    demo="$p"; break
done
if [[ -n "$demo" ]]; then
    pg="$(ps -o pgid= -p "$demo" | tr -d ' ')"
    if [[ -n "$pg" && "$pg" != "$(ps -o pgid= -p $$ | tr -d ' ')" ]]; then
        kill -INT -- "-$pg" 2>/dev/null
    else
        fail "refusing to interrupt: the demo shares this script's process group"
    fi
fi
wait "$sp" 2>/dev/null
tail_bytes="$(tail -c 20 "$WORK/int.raw" | cat -v)"
grep -q '\[?25h' <<<"$tail_bytes"
check $? "an interrupt gives the cursor back (transcript ends with the show-cursor sequence)"
check "$([[ -z "$(pgrep -f '_nemr_cat_loop' || true)" ]] && echo 0 || echo 1)" \
    "an interrupt leaves no drawing process behind"

# 6. Small, and the same in any 80-column terminal.
frames_out="$(./scripts/lib/cat.sh --show)"
n_frames="$(grep -c '^frame ' <<<"$frames_out")"
check "$([[ "$n_frames" == "4" ]] && echo 0 || echo 1)" "four frames" "$n_frames"
widest="$(awk '{ if (length($0) > m) m = length($0) } END { print m }' <<<"$frames_out")"
check "$([[ "$widest" -le 40 ]] && echo 0 || echo 1)" "no frame is wider than 40 columns ($widest)"
LC_ALL=C grep -q '[^ -~]' <<<"$frames_out" && r=1 || r=0
check $r "plain ASCII only — no wide characters, nothing that renders differently"
heights="$(awk '/^frame /{if (n) print n; n=0; next} NF{n++} END{print n}' <<<"$frames_out" | sort -u | tr '\n' ' ')"
check "$([[ "$heights" == "3 " ]] && echo 0 || echo 1)" "every frame is exactly 3 lines high ($heights)"

# ---------------------------------------------------------------------------
printf '\n'
if (( FAIL > 0 )); then
    printf '%sFAIL%s — %d assertion(s) failed, %d passed.\n' "$RED" "$RESET" "$FAIL" "$PASS"
    exit 1
fi
if (( PASS != EXPECTED_ASSERTIONS )); then
    printf '%sFAIL%s — expected %d assertions, counted %d: a case was skipped or added without raising EXPECTED_ASSERTIONS.\n' \
        "$RED" "$RESET" "$EXPECTED_ASSERTIONS" "$PASS"
    exit 1
fi
printf '%sPASS%s — %d assertions.\n' "$GREEN" "$RESET" "$PASS"
printf 'Not covered here, by nature: the first run on a machine with none of it\n'
printf 'present. That arm is docs/install-acceptance.md, on a VM you can snapshot.\n'
