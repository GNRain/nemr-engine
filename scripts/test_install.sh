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
EXPECTED_ASSERTIONS=74

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
grep -qE 'running the smoke test +ok' <<<"$out"
check $? "it verifies with the smoke test rather than trusting exit codes" "$(tail -6 <<<"$out")"
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

# 4d. THE DEFAULT: one live line and the cat, nothing else — and a result that
#     reports rather than narrates (F-30). Captured on a FIRST install.
first="$WORK/first.raw"
script -qec "bash -c 'stty cols 100 rows 30; NEMR_TEST_CURSOR_ROW=26 NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh --yes'" /dev/null >"$first" 2>&1
first_live="$(python3 - "$first" <<'PYEOF'
import subprocess, sys
raw = sys.argv[1]
d = open(raw, 'rb').read()
cut = d.rfind(b'\x1b[J')
print(subprocess.run(['python3', 'scripts/lib/render_pty.py', raw, '--cols', '100',
                      '--rows', '30', '--at', str(int(cut * 0.6) if cut > 0 else len(d))],
                     capture_output=True, text=True).stdout)
PYEOF
)"
grep -q 'Installing nemr' <<<"$first_live"
check $? "the live screen names what it is doing" "$(head -4 <<<"$first_live")"
grep -qE '\[#*-*\]  [0-9]+ of [0-9]+' <<<"$first_live"
check $? "with a bar and a count beneath it" "$(grep -n 'of ' <<<"$first_live" | head -2)"
# The bar draws in characters every font has. It was two block characters, and
# on Windows Terminal both rendered as the same solid grey — the bytes were
# right and the font was not, and no terminal can be asked whether a glyph will
# render (the Product Owner, 2026-09-10).
blocks="$(LC_ALL=C grep -c $'\xe2\x96' "$first" || true)"
check "$([[ "${blocks:-0}" == "0" ]] && echo 0 || echo 1)" \
    "and it needs no font: no block characters anywhere in the run" "$blocks"
grep -qE '\[#+-+\]' <<<"$first_live"
check $? "the bar has a filled part and an empty part" "$(grep -o '\[[#-]*\]' <<<"$first_live" | head -1)"
grep -qE '(installing|building|pulling|starting|adding|enabling|updating|delegating|disabling|checking|running|making) ' <<<"$first_live"
check $? "and the step in plain words, not the plan's sentence"
live_steps="$(grep -cE '^  (installing|building|pulling|starting|adding|enabling|updating|delegating|disabling|checking|running|making) ' <<<"$first_live")"
check "$([[ "${live_steps:-0}" -le 1 ]] && echo 0 || echo 1)" \
    "ONE step line, replaced — not a list that grows ($live_steps on screen)" \
    "$(grep -nE '^  [a-z]' <<<"$first_live" | head -4)"
# On the transcript, not on a screen sliced at a guessed offset: a slice can
# land between frames.
grep -qF "\`-.-'" "$first"
check $? "the cat plays beside it" "$(grep -c . <<<"$first_live") rows on the sliced screen"

# The result: four lines, and only the result.
first_end="$(python3 "$REPO/scripts/lib/render_pty.py" "$first" --cols 100 --rows 30)"
grep -qE '(✓|OK) nemr is installed' <<<"$first_end" \
    && grep -qE 'Version: +[0-9]' <<<"$first_end" \
    && grep -qE 'Installed: +~?/' <<<"$first_end" \
    && grep -q 'nemr create myproject' <<<"$first_end"
check $? "on success: the mark, the version, where it went, and what to try" \
    "$(tail -10 <<<"$first_end")"
narration="$(grep -cE 'digest|manifest|apt-get|containerd runc|WILL DO|Files it writes' <<<"$first_end" || true)"
check "$([[ "${narration:-0}" == "0" ]] && echo 0 || echo 1)" \
    "and no narration: no plan, no digests, no package names, no file list" "$narration lines"

# On failure: the step, the reason, the fix, the log. Nothing else.
failraw="$WORK/fail.raw"
script -qec "bash -c 'stty cols 100 rows 30; NEMR_TEST_CURSOR_ROW=26 NEMR_TEST_STEP_STUB=1 NEMR_TEST_FAIL_STEP=image $REPO/scripts/install.sh --yes'" /dev/null >"$failraw" 2>&1
fail_screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$failraw" --cols 100 --rows 30)"
grep -qE '(✗|XX) Install stopped' <<<"$fail_screen" \
    && grep -qE 'Failed at: +pulling the base image' <<<"$fail_screen" \
    && grep -qE 'Because: +.' <<<"$fail_screen" \
    && grep -qE 'Fix: +.' <<<"$fail_screen" \
    && grep -qE 'Full log: .*install-[0-9]+-[0-9]+\.log' <<<"$fail_screen"
check $? "on failure: the step, the reason, the fix as a command, and the log" \
    "$(tail -10 <<<"$fail_screen")"
fail_cat="$(grep -cE '\("\)|o\.o|\*-\*' <<<"$fail_screen" || true)"
check "$([[ "${fail_cat:-0}" == "0" ]] && echo 0 || echo 1)" \
    "and the live screen is gone from it" "$fail_cat cat rows"

# 4d-bis. THE OUTPUT PANE: the real tail of what the step printed, bounded.
cat >"$WORK/pane.sh" <<'PANE'
#!/usr/bin/env bash
cd "$1"; . scripts/lib/cat.sh; . scripts/lib/region.sh; . scripts/lib/steps.sh
LOG="$2/plog"; STEP_OUT="$2/pout"; : >"$STEP_OUT"
printf 'a command line\n'
nemr_region_start "$2/pstate" "$STEP_OUT" || { echo NO-REGION; exit 0; }
_S_REGION=1
steps_seed "building the engine" "pulling the base image"
_S_IDX=0; step_begin
for i in 1 2 3 4 5 6 7 8; do printf '   Compiling crate-number-%d v0.%d.0\n' "$i" "$i" >>"$STEP_OUT"; sleep 0.3; done
# A step that prints what would tear the region if it escaped: colour, cursor
# moves, a carriage return, a tab, and a line far wider than the pane.
printf '\033[31mRED\033[0m\033[2K\033[5Amoved\ttabbed\rreturned %s\n' "$(head -c 300 /dev/zero | tr '\0' 'W')" >>"$STEP_OUT"
sleep 0.5
_S_IDX=0; step_result done new "built"   # folds the output into the log
_S_IDX=1; step_begin                    # and the pane empties with the step
sleep 0.5
nemr_region_stop
printf 'done\n'
PANE
chmod +x "$WORK/pane.sh"
script -qec "bash -c 'stty cols 100 rows 30; $WORK/pane.sh $REPO $WORK'" /dev/null >"$WORK/pane.raw" 2>&1
pane_mid="$(python3 - "$WORK/pane.raw" <<'PYEOF'
import subprocess, sys
raw = sys.argv[1]
d = open(raw, 'rb').read()
cut = d.rfind(b'\x1b[J')
print(subprocess.run(['python3', 'scripts/lib/render_pty.py', raw, '--cols', '100',
                      '--rows', '30', '--at', str(int((cut if cut > 0 else len(d)) * 0.62))],
                     capture_output=True, text=True).stdout)
PYEOF
)"
grep -qE '^  \+-+\+' <<<"$pane_mid"
check $? "the pane has a border" "$(head -8 <<<"$pane_mid")"
pane_lines="$(grep -cE '^  \| ' <<<"$pane_mid" || true)"
check "$([[ "${pane_lines:-0}" -ge 4 ]] && echo 0 || echo 1)" \
    "and shows several of the step's own lines, not one ($pane_lines)" "$(grep -E '^  \|' <<<"$pane_mid" | head -3)"
grep -qE '^  \|    Compiling crate-number-[0-9]' <<<"$pane_mid"
check $? "and they are what the step actually printed"
# The TAIL, not the head: what is on screen is the END of what was printed, so
# the numbers on it are consecutive and the highest is not among the first few.
pane_max="$(grep -oE 'crate-number-([0-9]+)' <<<"$pane_mid" | grep -oE '[0-9]+$' | sort -n | tail -1)"
pane_min="$(grep -oE 'crate-number-([0-9]+)' <<<"$pane_mid" | grep -oE '[0-9]+$' | sort -n | head -1)"
check "$([[ -n "$pane_max" && "$pane_max" -ge 4 && $(( pane_max - pane_min )) -le 4 ]] && echo 0 || echo 1)" \
    "it is the TAIL — the newest lines, scrolling as they arrive (showing $pane_min..$pane_max)"

# The noisy line — colour, a cursor move, a tab, a carriage return and 300
# characters — reaches the pane stripped and cut, and the border below it is
# still whole. Asserted on the transcript, where the pane exists.
noisy="$(LC_ALL=C grep -aoE '\| REDmoved tabbed returned W+ \|' "$WORK/pane.raw" | head -1)"
check "$([[ -n "$noisy" ]] && echo 0 || echo 1)" \
    "colour, cursor moves, tabs and carriage returns are stripped before the pane draws" "$noisy"
check "$([[ "${#noisy}" -le 44 ]] && echo 0 || echo 1)" \
    "and a 300-character line is cut to the pane's width (${#noisy} columns)"
after_noisy="$(LC_ALL=C grep -aA1 -E 'REDmoved' "$WORK/pane.raw" | tail -1)"
grep -qE '\+-+\+' <<<"$after_noisy"
check $? "the border below it is whole" "$after_noisy"
pane_end="$(python3 "$REPO/scripts/lib/render_pty.py" "$WORK/pane.raw" --cols 100 --rows 30)"
grep -q 'Compiling crate-number' <<<"$pane_end" && r=1 || r=0
check $r "the pane is emptied when the step changes"
grep -q 'Compiling crate-number-8' "$WORK/plog"
check $? "and everything it showed is in the log" "$(ls "$WORK" | tr '\n' ' ')"

# 4e. WITH NO LIVE SCREEN — what a terminal that cannot hold one gets, which is
#     what the Product Owner keeps getting. It has to look right there too.
plain="$WORK/plain.raw"
script -qec "bash -c 'stty cols 70 rows 30; NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh --yes'" /dev/null >"$plain" 2>&1
plain_screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$plain" --cols 70 --rows 30)"
plain_cat="$(grep -cE '\("\)|o\.o|\*-\*' <<<"$plain_screen" || true)"
check "$([[ "${plain_cat:-0}" == "0" ]] && echo 0 || echo 1)" \
    "at 70 columns there is no live screen at all" "$plain_cat"
grep -qE '(✓|OK) nemr is installed' <<<"$plain_screen"
check $? "and the run still ends in the same four-line result"
plain_lines="$(grep -c . <<<"$(python3 "$REPO/scripts/lib/render_pty.py" "$plain" --cols 70 --rows 30)")"
check "$([[ "${plain_lines:-99}" -le 26 ]] && echo 0 || echo 1)" \
    "and it is short — $plain_lines lines on screen, where the old default printed about a hundred"
grep -qE 'no live screen — [0-9]+ columns' <<<"$plain_screen"
check $? "and it SAYS why there is no live screen" "$(grep -n 'live screen' <<<"$plain_screen" | head -2)"

# 4f. THE DECISION IS IN THE LOG, on every run, whichever way it went — the
#     thing whose absence made a WSL2 report unanswerable from here.
newest_log="$(ls -t "$HOME/.local/state/nemr"/install-*.log 2>/dev/null | head -1)"
grep -q -- '--- install screen' "$newest_log"
check $? "the log records the screen decision"
grep -qE 'live: +(yes|NO)' "$newest_log" \
    && grep -qE 'measured: +[0-9]+ cols x [0-9]+ rows' "$newest_log" \
    && grep -qE 'needs: +[0-9]+ cols x [0-9]+ rows' "$newest_log" \
    && grep -qE 'TERM=.*tty=' "$newest_log" \
    && grep -q 'DSR' "$newest_log"
check $? "with the size it measured, the thresholds, the terminal, and DSR named as not gating" \
    "$(sed -n '/--- install screen/,/^$/p' "$newest_log" | head -8)"

# 4g. THE PLAN IS THE CONSENT, and only that: an interactive run shows it, a
#     --yes run does not, --verbose shows everything.
interactive="$(printf 'n\n' | script -qec "bash -c 'stty cols 100 rows 30; NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh'" /dev/null 2>&1)"
grep -q 'the plan for this machine' <<<"$interactive" && grep -q 'Go ahead?' <<<"$interactive"
check $? "an interactive run still shows the full plan before the question"
yes_plan="$(grep -c 'the plan for this machine' <<<"$first_end" || true)"
check "$([[ "${yes_plan:-0}" == "0" ]] && echo 0 || echo 1)" \
    "a --yes run does not recite it — that consent was already given" "$yes_plan"
verbose="$(script -qec "bash -c 'stty cols 100 rows 30; NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh --yes --verbose'" /dev/null 2>&1)"
grep -q 'the plan for this machine' <<<"$verbose" && grep -q 'pulled ghcr.io' <<<"$verbose"
check $? "--verbose shows everything: the full plan and every step's detail"

# 4h. Colour and marks: on for a terminal, and the emoji go with the colour.
colour="$(grep -c $'\033\[3[123]m' "$first" || true)"
check "$([[ "${colour:-0}" -gt 0 ]] && echo 0 || echo 1)" "the result is coloured on a terminal"
grep -q '✓' <<<"$first_end"
check $? "and carries the tick"
nocolour="$WORK/nocolour.raw"
script -qec "bash -c 'stty cols 100 rows 30; NO_COLOR=1 NEMR_TEST_STEP_STUB=1 $REPO/scripts/install.sh --yes'" /dev/null >"$nocolour" 2>&1
n_esc="$(grep -c $'\033\[3[123]m' "$nocolour" || true)"
nc_screen="$(python3 "$REPO/scripts/lib/render_pty.py" "$nocolour" --cols 100 --rows 30)"
check "$([[ "${n_esc:-0}" == "0" ]] && echo 0 || echo 1)" "NO_COLOR turns every colour off" "$n_esc"
grep -q 'OK nemr is installed' <<<"$nc_screen" && ! grep -q '✓' <<<"$nc_screen"
check $? "and the marks go with it, replaced by plain ASCII"


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
