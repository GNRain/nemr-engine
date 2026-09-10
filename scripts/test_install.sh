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
EXPECTED_ASSERTIONS=56

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
did_work="$(sed -n '/^Installing/,$p' <<<"$out" | grep -E '✓' | grep -vE 'already done|already current|at the recorded digest|found at|passed:' || true)"
check "$([[ -z "$did_work" ]] && echo 0 || echo 1)" \
    "every step reports already done — nothing was redone" "${did_work:-}"
grep -qE 'smoke test — passed: create, start, attach' <<<"$out"
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
frames="$(grep -c '\*-\*' "$raw" || true)"
check "$([[ "$frames" -gt 10 ]] && echo 0 || echo 1)" \
    "on a terminal the cat plays ($frames frames drawn)" \
    "if this terminal has fewer than $(( $(bash -c '. scripts/lib/cat.sh; _nemr_cat_height') + 2 )) rows the animation is off BY DESIGN"
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
    n="$(grep -c '\*-\*' "$WORK/off.raw" || true)"
    check "$([[ "$n" == "0" ]] && echo 0 || echo 1)" "$way draws nothing" "$n frames"
done

# 4b. A terminal too short to hold the drawing: nothing, rather than a block
#     that scrolls and leaves the cursor arithmetic walking over the step lines.
script -qec 'stty rows 10 2>/dev/null; ./scripts/lib/cat.sh --demo 1' /dev/null >"$WORK/short.raw" 2>&1
n="$(grep -c '\*-\*' "$WORK/short.raw" || true)"
check "$([[ "$n" == "0" ]] && echo 0 || echo 1)" \
    "a terminal too short for the drawing gets no animation at all" "$n frames"

# 4c. THE SCREEN, not the escape sequences. Counting clears proves a clear was
#     issued, not that nothing was left behind: the drawer used to be killed
#     wherever it happened to be, so half the time the erase started part-way
#     down the block and every line above it stayed on the screen for good.
#     Measured before the fix: 9 of 20 stops left a cat. Nothing was miscounted
#     — no assertion was about the screen. This one renders the transcript
#     through a terminal and looks.
screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$raw")"
cat_on_screen=0
grep -qE '\(\"\)|o\.o|o\.O|\*-\*' <<<"$screen" && cat_on_screen=1
check "$cat_on_screen" "when the install finishes, no part of the cat is left on the screen" \
    "$(grep -nE '\(\"\)|o\.o|\*-\*' <<<"$screen" | head -3)"

# And under the stop that used to leak: many start/stop cycles, each ended at a
# different moment within a frame, must each leave the screen as they found it.
# The delays are a fixed ladder rather than $RANDOM so a failure reproduces.
cat >"$WORK/cycle.sh" <<'CYCLE'
#!/usr/bin/env bash
. "$1/scripts/lib/cat.sh"
printf 'before\n'
# The seam widens the mid-frame window on purpose: without it a regression
# here shows up two times in twelve, which is not an assertion.
NEMR_TEST_CAT_LINE_DELAY=0.02 NEMR_CAT_DELAY=0.01 nemr_cat_start
sleep "$2"
nemr_cat_stop
printf 'after\n'
CYCLE
chmod +x "$WORK/cycle.sh"
leaked=0
for delay in 0.10 0.13 0.17 0.21 0.26 0.31 0.37 0.44 0.52 0.61 0.71 0.83; do
    timeout 30 script -qec "$WORK/cycle.sh $REPO $delay" /dev/null >"$WORK/stress.raw" 2>&1
    after="$(python3 "$REPO/scripts/lib/render_pty.py" "$WORK/stress.raw")"
    if [[ "$(grep -c . <<<"$after")" -ne 2 ]]; then
        leaked=$((leaked + 1))
        [[ "$leaked" == 1 ]] && { printf '   | after a stop at %ss the screen was:\n' "$delay"
                                  printf '%s\n' "$after" | grep -n . | head -5 | sed 's/^/   | /'; }
    fi
done
check "$([[ "$leaked" == "0" ]] && echo 0 || echo 1)" \
    "stopped mid-frame twelve times over, the screen is left exactly as found" \
    "$leaked of 12 left something behind"

# 4d. THE LIVE REGION at its real threshold: 80 columns, the default terminal,
#     on a FIRST install — the run every capture so far has skipped, and the
#     one whose long statuses used to tear the layout (F-29).
first="$WORK/first80.raw"
script -qec "bash -c 'stty cols 80 rows 30; NEMR_TEST_CURSOR_ROW=26 NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh --yes'" /dev/null >"$first" 2>&1
first_mid="$(python3 - "$first" <<'PYEOF'
import subprocess, sys
raw = sys.argv[1]
d = open(raw, 'rb').read()
i, j = d.find(b'Installing'), d.rfind(b'nemr is installed')
out = subprocess.run(['python3', 'scripts/lib/render_pty.py', raw, '--cols', '80',
                      '--rows', '30', '--at', str(i + int((j - i) * 0.6))],
                     capture_output=True, text=True).stdout
print(out)
PYEOF
)"
two_col="$(grep -cE '^  [+>.!x] .{28,} +[a-zA-Z…]* +[^ ]' <<<"$first_mid" || true)"
check "$([[ "${two_col:-0}" -ge 3 ]] && echo 0 || echo 1)" \
    "at exactly 80 columns, a FIRST install draws both columns ($two_col rows carry step and cat)" \
    "$(head -6 <<<"$first_mid")"
# The region's own rows only: the plan above it is ordinary prose that the
# terminal wraps, and always did.
over="$(grep -E '^  [+>.!x] ' <<<"$first_mid" | awk 'length($0) > 80' | wc -l)"
check "$([[ "$over" == "0" ]] && echo 0 || echo 1)" \
    "and no row exceeds 80 columns — the status has a budget and is truncated to it" \
    "$over rows over"
grep -q 'packages' <<<"$first_mid" && grep -qE '(new|done|ok)' <<<"$first_mid"
check $? "the step list carries a short status token, not the sentence"
grep -q 'installed: containerd runc uidmap' "$WORK/../"*/install-*.log 2>/dev/null \
    || grep -rq 'installed: containerd runc uidmap' "$HOME/.local/state/nemr/" 2>/dev/null
check $? "and the sentence itself is in the log"

# Below the threshold there are no columns at all, and nothing is truncated:
# today's append-only output, and the region is never started.
narrow="$WORK/narrow79.raw"
script -qec "bash -c 'stty cols 79 rows 30; NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh --yes'" /dev/null >"$narrow" 2>&1
narrow_screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$narrow" --cols 79 --rows 30)"
narrow_cat="$(grep -cE '\("\)|o\.o|\*-\*' <<<"$narrow_screen" || true)"
check "$([[ "${narrow_cat:-0}" == "0" ]] && echo 0 || echo 1)" \
    "at 79 columns — one under the threshold — the region is never started" "$narrow_cat cat rows"
grep -q 'nemr is installed' <<<"$narrow_screen"
check $? "and the run still finishes, append-only, as before"

# A terminal too short: same rule, no region at all.
short="$WORK/short.raw"
script -qec "bash -c 'stty cols 100 rows 17; NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh --yes'" /dev/null >"$short" 2>&1
short_screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$short" --cols 100 --rows 17)"
short_cat="$(grep -cE '\("\)|o\.o|\*-\*' <<<"$short_screen" || true)"
check "$([[ "${short_cat:-0}" == "0" ]] && echo 0 || echo 1)" \
    "a terminal too short to hold the region never starts one" "$short_cat cat rows"

# THE REGION RESOLVES INTO SCROLLBACK when a step fails: the list, the failure,
# and the log path, as ordinary text that outlives the script.
failraw="$WORK/fail.raw"
script -qec "bash -c 'stty cols 80 rows 30; NEMR_TEST_CURSOR_ROW=26 NEMR_TEST_STEP_STUB=1 NEMR_TEST_FAIL_STEP=image $REPO/scripts/install.sh --yes'" /dev/null >"$failraw" 2>&1
fail_screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$failraw" --cols 80 --rows 30)"
grep -qE '^  x base image +FAILED' <<<"$fail_screen" \
    && grep -q 'Stopped at: base image' <<<"$fail_screen" \
    && grep -qE 'install-[0-9]+-[0-9]+\.log' <<<"$fail_screen"
check $? "a failed step resolves to text: the list, the FAILED line, and the log path" \
    "$(tail -6 <<<"$fail_screen")"
fail_cat="$(grep -cE '\("\)|o\.o|\*-\*' <<<"$fail_screen" || true)"
check "$([[ "${fail_cat:-0}" == "0" ]] && echo 0 || echo 1)" \
    "and the cat is gone from the failure screen" "$fail_cat cat rows"

# The same on Ctrl-C, which kills the renderer outright: the resolve belongs to
# the main shell, or it does not happen on the exit that most needs it.
cat >"$WORK/intr.sh" <<'INTR'
#!/usr/bin/env bash
. "$1/scripts/lib/cat.sh"
. "$1/scripts/lib/region.sh"
. "$1/scripts/lib/steps.sh"
LOG="$2/ilog"; : >"$LOG"
trap 'FAILED_STEP="${FAILED_STEP:-interrupted}"; nemr_region_stop 2>/dev/null; nemr_cat_stop; printf "\r\nStopped at: %s\r\n" "$FAILED_STEP"; exit 130' INT TERM
printf 'a command line\n'
nemr_region_start "$2/istate" || { printf 'NO-REGION\n'; exit 0; }
_S_REGION=1
steps_seed "packages" "user units" "the engine (nemr, nemrd)" "smoke test"
_S_IDX=0; step_begin; step_result done new "installed"
_S_IDX=1; step_begin; step_result done new "installed"
_S_IDX=2; step_begin
echo $$ >"$2/ipid"
sleep 30
INTR
chmod +x "$WORK/intr.sh"
rm -f "$WORK/ipid"
( timeout 60 script -qec "bash -c 'stty cols 80 rows 24; $WORK/intr.sh $REPO $WORK'" /dev/null >"$WORK/intr.raw" 2>&1 ) &
intr_job=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do [[ -s "$WORK/ipid" ]] && break; sleep 0.4; done
[[ -s "$WORK/ipid" ]] && kill -INT "$(cat "$WORK/ipid")" 2>/dev/null
wait "$intr_job" 2>/dev/null
intr_screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$WORK/intr.raw" --cols 80 --rows 24)"
grep -q 'a command line' <<<"$intr_screen" \
    && grep -qE '^  \+ packages' <<<"$intr_screen" \
    && grep -qE '^  > the engine' <<<"$intr_screen" \
    && grep -q 'Stopped at: interrupted' <<<"$intr_screen"
check $? "Ctrl-C resolves the region into text: the list as it stood, and why it stopped" \
    "$(head -8 <<<"$intr_screen")"

# COLOUR: on for a terminal, off for everything else.
colour="$(grep -c $'\033\[3[123]m' "$first" || true)"
check "$([[ "${colour:-0}" -gt 0 ]] && echo 0 || echo 1)" "the step list is coloured on a terminal"
nocolour="$WORK/nocolour.raw"
script -qec "bash -c 'stty cols 80 rows 30; NO_COLOR=1 NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh --yes'" /dev/null >"$nocolour" 2>&1
n_esc="$(grep -c $'\033\[3[123]m' "$nocolour" || true)"
check "$([[ "${n_esc:-0}" == "0" ]] && echo 0 || echo 1)" "NO_COLOR turns every colour off" "$n_esc"


# 4e. A STEP'S OWN OUTPUT CANNOT ENTER THE REGION. Its stdout and stderr go to
#     the log, as they always did; the region is the only writer. (A step that
#     wrote to /dev/tty directly would bypass every redirect there is — the one
#     that does, `nemr attach` in the smoke test, runs after the region closes.)
cat >"$WORK/noisy.sh" <<'NOISY'
#!/usr/bin/env bash
. "$1/scripts/lib/cat.sh"
. "$1/scripts/lib/region.sh"
printf 'header
'
nemr_region_start "$2/state" || { printf 'NO-REGION
'; exit 0; }
steps_seed "step one" "step two"
( echo "STDOUT-NOISE-FROM-A-STEP"; echo "STDERR-NOISE-FROM-A-STEP" >&2 ) >>"$2/log" 2>&1
sleep 0.4
_S_IDX=0; step_result done new "did it"; _S_IDX=1; step_result done new "did it"
sleep 0.3
nemr_region_stop
printf 'footer
'
NOISY
chmod +x "$WORK/noisy.sh"
script -qec "bash -c 'stty cols 80 rows 30; $WORK/noisy.sh $REPO $WORK'" /dev/null >"$WORK/noisy.raw" 2>&1
noisy_screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$WORK/noisy.raw" --cols 80 --rows 30)"
noise=0
grep -q 'NOISE-FROM-A-STEP' <<<"$noisy_screen" && noise=1
check "$noise" "a step's stdout and stderr reach the log, never the region" \
    "$(grep -n 'NOISE' <<<"$noisy_screen" | head -2)"
grep -q 'STDOUT-NOISE-FROM-A-STEP' "$WORK/log"
check $? "and the log has them"

# 4f. MORE STEPS THAN ROWS. The region never scrolls the terminal: the left
#     column becomes a window anchored on the step running now, and says how
#     many are above it rather than hiding them silently.
cat >"$WORK/many.sh" <<'MANY'
#!/usr/bin/env bash
. "$1/scripts/lib/cat.sh"
. "$1/scripts/lib/region.sh"
. "$1/scripts/lib/steps.sh"
LOG="$2/mlog"
printf 'header\n'
nemr_region_start "$2/manystate" || { printf 'NO-REGION\n'; exit 0; }
_S_REGION=1
lines=(); for i in $(seq 1 30); do lines+=("step $i of thirty"); done
steps_seed "${lines[@]}"
sleep 1
for i in $(seq 1 26); do _S_IDX=$((i - 1)); step_result done new "did it"; done
sleep 1
nemr_region_stop
printf 'footer\n'
MANY
chmod +x "$WORK/many.sh"
script -qec "bash -c 'stty cols 80 rows 22; $WORK/many.sh $REPO $WORK'" /dev/null >"$WORK/many.raw" 2>&1
# Asserted on the transcript, not on a screen rendered at a guessed offset: the
# window is what the LIVE region does, and the resolve afterwards deliberately
# prints the WHOLE list as text so the earlier steps land in scrollback.
# Only the LIVE part: everything up to the renderer's final erase. After that
# the main shell resolves by printing all thirty steps as text, which is the
# point — the ones the window did not show are in scrollback.
many_live="$(python3 - "$WORK/many.raw" <<'PYEOF'
import sys
d = open(sys.argv[1], 'rb').read()
cut = d.rfind(b'\x1b[J')
sys.stdout.write(d[:cut if cut > 0 else len(d)].decode('utf-8', 'replace'))
PYEOF
)"
windowed=0
grep -qE '… [0-9]+ more above' <<<"$many_live" || windowed=1        # says how many are above
grep -q 'step 30 of thirty' <<<"$many_live" || windowed=1           # the last step is visible
grep -q 'step 11 of thirty' <<<"$many_live" && windowed=1           # an early one is NOT, while live
grep -q 'footer' "$WORK/many.raw" || windowed=1                     # and the run finished
grep -q 'step 11 of thirty' "$WORK/many.raw" || windowed=1          # the ones it did not show resolve into scrollback
check "$windowed" \
    "30 steps in a 22-row terminal: a window anchored on the last, and a count of what is above" \
    "$(grep -aoE '… [0-9]+ more above|step (11|30) of thirty' <<<"$many_live" | sort -u | tr '\n' ' ')"

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
check "$([[ "$widest" -le 78 ]] && echo 0 || echo 1)" \
    "no frame is wider than 78 columns, so it renders the same in any 80-column terminal ($widest)"
LC_ALL=C grep -q '[^ -~]' <<<"$frames_out" && r=1 || r=0
check $r "plain ASCII only — no wide characters, nothing that renders differently"
# Same height for every frame, or the redraw walks up the terminal leaving a
# trail. The number itself is the art's business; that they AGREE is not.
heights="$(awk '/^frame /{if (n) print n; n=0; next} {n++} END{print n}' <<<"$frames_out" | sort -u | tr '\n' ' ')"
check "$([[ "$(wc -w <<<"$heights")" == "1" ]] && echo 0 || echo 1)" \
    "every frame is the same height ($heights lines)"
# And the drawer moves back over exactly the rows it wrote a newline for: one
# fewer than the frame's height, because the last row deliberately carries no
# newline (a newline there scrolls a block at the foot of the screen, and every
# move here is relative). Pinned to the art, not to a constant beside it.
up="$(grep -o $'\033\[[0-9]*A' "$raw" | sort -u | tr -d '\033[A' | tr '\n' ' ')"
expect_up=$(( $(echo $heights) - 1 ))
check "$([[ "$(echo $up)" == "$expect_up" ]] && echo 0 || echo 1)" \
    "the cursor comes back over exactly the rows it newlined: ${up}for a ${heights}row frame"

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
