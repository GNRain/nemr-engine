#!/usr/bin/env bash
#
# The install register: one line per step, a tick or a cross and a short
# phrase; everything a command prints goes to a log the script names only if
# something fails (D-14).
#
# Sourced by scripts/install.sh (the engine, on the machine a person works on)
# and scripts/install_server.sh (a self-hosted sync server). The consent
# predicate lives here so there is ONE of it: two copies is two places for
# F-15's rule — no terminal and no --yes means refuse, never proceed silently —
# to regress independently.
#
# Requires: scripts/lib/cat.sh sourced first (the animation).

if [[ -t 1 && -z "${NO_COLOR:-}" ]]; then
    _S_GREEN=$'\033[32m'; _S_RED=$'\033[31m'; _S_DIM=$'\033[2m'; _S_RESET=$'\033[0m'
    _S_AMBER=$'\033[33m'; _S_BRIGHT=$'\033[1m'
else
    _S_GREEN=""; _S_RED=""; _S_DIM=""; _S_RESET=""; _S_AMBER=""; _S_BRIGHT=""
fi

# When a live region owns the screen (scripts/lib/region.sh), these do not
# print — they update the line this step owns and hand the whole list to the
# renderer, which is the only thing writing to the terminal. Nothing else may
# write while it is open, or the layout tears.
_S_REGION=0
_S_IDX=0
_S_LINES=()
_S_NOTES=()

# THE STEP LIST'S STATE. One entry per step: what it is called (short, for the
# region), what state it is in, and a status token with a fixed budget. The
# detail — the digest, the package list, the socket path — goes to the log,
# because a column sized to whatever a step prints is not a design (F-29).
_S_LABELS=()
_S_STATES=()
_S_TOKENS=()

# What the live line says right now, and how far along the run is. ONE line,
# replaced — never a list that grows (the Product Owner, 2026-09-10).
_s_republish() {
    (( _S_REGION )) || return 0
    local done=0 i
    for i in "${!_S_STATES[@]}"; do
        case "${_S_STATES[$i]}" in done|warn|fail) done=$((done + 1)) ;; esac
    done
    nemr_region_publish "${_S_LABELS[$_S_IDX]:-}" "$done" "${#_S_LABELS[@]}"
}

# Seed the list, all pending.
steps_seed() {   # <label>...
    local l
    _S_LABELS=(); _S_STATES=(); _S_TOKENS=()
    for l in "$@"; do _S_LABELS+=("$l"); _S_STATES+=("wait"); _S_TOKENS+=(""); done
    _s_republish
}

# The step at $_S_IDX is the one running now. Its output pane starts empty:
# what the last step printed belongs to the last step.
step_begin() {
    _S_STATES[$_S_IDX]="run"
    _S_TOKENS[$_S_IDX]="…"
    [[ -n "${STEP_OUT:-}" ]] && : >"$STEP_OUT"
    _s_republish
}

# How it went. <state> is done|warn|fail; <token> fills the status column;
# <detail> is the full sentence, which the log always gets and an append-only
# run also prints.
step_result() {   # <state> <token> <detail>
    local state="$1" token="$2" detail="$3" label="${_S_LABELS[$_S_IDX]:-}"
    _S_STATES[$_S_IDX]="$state"
    _S_TOKENS[$_S_IDX]="$token"
    # A failure carries its own explanation, and it records the run's OUTCOME
    # so the single authority (the EXIT trap) prints the failure block from the
    # one place every exit passes through.
    if [[ "$state" == fail ]]; then
        FAILED_STEP="$label"
        RESULT_STATE=failed
    fi
    # Everything the step printed joins the log, in order, then the pane's file
    # is emptied for the next step.
    if [[ -n "${STEP_OUT:-}" && -s "$STEP_OUT" ]]; then
        cat "$STEP_OUT" >>"$LOG" 2>/dev/null || true
        : >"$STEP_OUT"
    fi
    printf '[%s] %s — %s\n' "$state" "$label" "$detail" >>"$LOG" 2>/dev/null || true
    if (( _S_REGION )); then
        _s_republish
    elif (( ${_S_VERBOSE:-0} )); then
        # --verbose: every line the old run showed, detail and all.
        case "$state" in
            done) printf '  %s+%s %s — %s\n' "$_S_GREEN" "$_S_RESET" "$label" "$detail" ;;
            warn) printf '  %s!%s %s — %s\n' "$_S_AMBER" "$_S_RESET" "$label" "$detail" ;;
            *)    printf '  %s%sx%s %s — %s\n' "$_S_RED" "$_S_BRIGHT" "$_S_RESET" "$label" "$detail" ;;
        esac
    else
        # The default with no live region — which is what a terminal that cannot
        # hold one gets, and it has to look right there too: the step in plain
        # words and a short token, never the detail. The detail is in the log.
        case "$state" in
            done) printf '  %s%-34s %s%s\n' "$_S_GREEN" "$label" "$token" "$_S_RESET" ;;
            warn) printf '  %s%-34s %s%s\n' "$_S_AMBER" "$label" "$token" "$_S_RESET" ;;
            *)    printf '  %s%-34s %s%s\n' "$_S_RED" "$label" "FAILED" "$_S_RESET" ;;
        esac
    fi
}

tick()  {
    if (( _S_REGION )); then
        _S_LINES[$_S_IDX]="  ✓ $1"
        nemr_region_publish "${_S_LINES[@]}"
    else
        printf '  %s✓%s %s\n' "$_S_GREEN" "$_S_RESET" "$1"
    fi
}
cross() {
    if (( _S_REGION )); then
        _S_LINES[$_S_IDX]="  ✗ $1"
        nemr_region_publish "${_S_LINES[@]}"
    else
        printf '  %s✗%s %s\n' "$_S_RED" "$_S_RESET" "$1"
    fi
}
note()  {
    if (( _S_REGION )); then
        # Kept, not dropped: printed under the region once it resolves. A note
        # is ours, but the region is a fixed list of steps and a note would
        # push it around.
        _S_NOTES+=("$1")
    else
        printf '    %s%s%s\n' "$_S_DIM" "$1" "$_S_RESET"
    fi
}

# ---------------------------------------------------------------------------
# The result. Four lines on success, four on failure — the shape of Claude
# Code's own installer, which reports the result where this used to narrate the
# work (the Product Owner, 2026-09-10). Everything else is behind --verbose.
#
# The marks live OUT HERE, never in the live region: ✓ ✗ ⚠ are double-width in
# some terminals and render inconsistently in others, which is exactly what
# tears a fixed-width two-column layout. When colour is off they go too,
# replaced by plain ASCII.
# ---------------------------------------------------------------------------
_S_MARK_OK="✓"; _S_MARK_BAD="✗"; _S_MARK_WARN="⚠"
if [[ -z "$_S_GREEN" ]]; then _S_MARK_OK="OK"; _S_MARK_BAD="XX"; _S_MARK_WARN="!!"; fi

result_ok() {   # <version> <installed path> [claude-missing]
    printf '\n%s%s%s %snemr is installed.%s\n\n' \
        "$_S_GREEN" "$_S_MARK_OK" "$_S_RESET" "$_S_BRIGHT" "$_S_RESET"
    printf '      Version:   %s\n' "$1"
    printf '      Installed: %s\n\n' "$2"
    printf '    Try:  nemr create myproject\n'
    printf '          nemr ui\n'
    if [[ -n "${3:-}" ]]; then
        printf '\n  %s%s%s Claude Code isn'"'"'t installed yet — nemr needs it inside each\n' \
            "$_S_AMBER" "$_S_MARK_WARN" "$_S_RESET"
        printf '    session. See claude.ai/code\n'
    fi
    printf '\n'
}

result_failed() {   # <step> <because> <fix>
    printf '\n%s%s%s %sInstall stopped.%s\n\n' \
        "$_S_RED" "$_S_MARK_BAD" "$_S_RESET" "$_S_BRIGHT" "$_S_RESET"
    printf '      Failed at:  %s\n' "$1"
    printf '      Because:    %s\n' "$2"
    printf '      Fix:        %s\n\n' "$3"
    printf '    Full log: %s\n\n' "${LOG/#$HOME/\~}"
}

# A third terminal outcome, as real as success and failure: the run did its
# part and now needs a reboot to continue (cgroup delegation applies when the
# user manager restarts). It used to print a heredoc to stderr from inside
# reboot_gate WHILE THE REGION OWNED THE SCREEN, then clear FAILED_STEP and
# exit — so the EXIT trap said nothing and the region erased the message on its
# way out: a first install that produced no output at all (F-32). Now it is a
# state the one authority prints, after the region is down.
result_paused() {   # <why>
    printf '\n%s%s%s %sAlmost there — reboot, then run this again.%s\n\n' \
        "$_S_AMBER" "$_S_MARK_WARN" "$_S_RESET" "$_S_BRIGHT" "$_S_RESET"
    printf '      Why:   %s\n' "$1"
    printf '      Do:    sudo reboot\n'
    printf '             then ./scripts/install.sh again\n\n'
    printf '    It skips everything done so far and carries on from here.\n\n'
}

# The backstop. If the script reaches its end by a path nobody modelled — a
# set -e abort mid-step, a future exit someone adds without wiring an outcome —
# this is what prints, so the run can NEVER finish silent (F-32, the class the
# Product Owner named). It is deliberately blunt: something went wrong that the
# installer did not have words for, and here is the log to send.
result_unexpected() {
    printf '\n%s%s%s %sInstall stopped unexpectedly.%s\n\n' \
        "$_S_RED" "$_S_MARK_BAD" "$_S_RESET" "$_S_BRIGHT" "$_S_RESET"
    printf '    It ended on a path it has no message for. This is a bug worth\n'
    printf '    reporting; the full log has what happened:\n\n'
    printf '      %s\n\n' "${LOG/#$HOME/\~}"
}

# Remove this run's own temp files. Registered by the caller (install.sh sets
# STEPS_TEMP_FILES); the ONE exit point is the one place that can promise they
# go on success, on failure and on Ctrl-C alike (F-34).
steps_tempclean() {
    local f
    for f in ${STEPS_TEMP_FILES:+"${STEPS_TEMP_FILES[@]}"}; do
        [[ -n "$f" ]] && rm -f "$f" "${f}.new" 2>/dev/null || true
    done
}

# Print anything the region held back, once it has resolved.
flush_notes() {
    local n
    for n in ${_S_NOTES+"${_S_NOTES[@]}"}; do
        printf '    %s%s%s\n' "$_S_DIM" "$n" "$_S_RESET"
    done
    _S_NOTES=()
}
head2() { printf '\n%s\n' "$1"; }

LOG_DIR="$HOME/.local/state/nemr"
LOG="${LOG:-$LOG_DIR/install-$(date +%Y%m%d-%H%M%S).log}"
# Not created on load: a run that only shows its plan, or is declined, leaves
# this machine exactly as it found it — including this directory.
open_log() { mkdir -p "$LOG_DIR"; : >>"$LOG"; }

FAILED_STEP=""

# WHERE A STEP'S OUTPUT GOES. To a file, always — never to the terminal, which
# is the containment rule the region depends on. When STEP_OUT is set it goes
# there first, so the live pane can show the tail of what THIS step printed, and
# the whole of it is appended to the log when the step ends. The log therefore
# reads exactly as it did before.
_step_sink() { printf '%s' "${STEP_OUT:-$LOG}"; }

# Run a command with its output in the log, never on the terminal.
logged() {
    local sink; sink="$(_step_sink)"
    printf '\n$ %s\n' "$*" >>"$sink"
    "$@" >>"$sink" 2>&1
}

# A long step: the work runs in its own process and the cat plays beside it.
# The install takes the same time whether or not a frame is ever drawn.
logged_long() {
    local rc=0 sink; sink="$(_step_sink)"
    printf '\n$ %s\n' "$*" >>"$sink"
    # A step can change the terminal's mode even with its output redirected:
    # the smoke test attaches to a session, and `nemr attach` puts the tty in
    # raw mode through /dev/tty. Save the settings and put them back, so what
    # runs next — the drawing, and the user's shell afterwards — gets the
    # terminal it expects.
    local tty_state=""
    [[ -t 1 ]] && tty_state="$(stty -g 2>/dev/null || true)"
    if (( _S_REGION )); then
        # The region draws the cat already, and it is the only writer.
        "$@" >>"$sink" 2>&1 &
        local work=$!
        wait "$work" || rc=$?
    elif nemr_cat_enabled; then
        "$@" >>"$sink" 2>&1 &
        local work=$!
        nemr_cat_start
        wait "$work" || rc=$?
        nemr_cat_stop
    else
        "$@" >>"$sink" 2>&1 || rc=$?
    fi
    [[ -n "$tty_state" ]] && stty "$tty_state" 2>/dev/null || true
    return "$rc"
}

# The cursor comes back and the cat is erased on every exit — normal, failed
# or interrupted — and a failure names the log rather than dumping it.
steps_trap() {
    trap '_steps_cleanup' EXIT
    # Ctrl-C resolves the region into ordinary text FIRST, then exits: the step
    # list, the failure and the log path have to survive the script and be
    # scrollable and copyable afterwards. A region that vanished with the
    # process would take the reason with it.
    trap '_steps_interrupted' INT TERM
}
# Ctrl-C is an outcome like any other: it is recorded here and SPOKEN by the
# single authority below, after the region is gone. Nothing prints from this
# trap — a message written while the region still owns the screen is erased.
_steps_interrupted() {
    trap - INT TERM
    if [[ -z "${RESULT_STATE:-}" ]]; then
        RESULT_STATE=failed
        RESULT_FAILED_STEP="${_S_LABELS[$_S_IDX]:-${FAILED_STEP:-a step}}"
        RESULT_BECAUSE="you pressed Ctrl-C"
        RESULT_FIX="run this again; it carries on from the step it was on"
    fi
    exit 130
}

# THE SINGLE AUTHORITY (F-32). Every exit — normal, failed, interrupted, or a
# path nobody modelled — passes through here, and here is the one place that:
#   1. tears the region down (so nothing it prints can be erased),
#   2. removes this run's temp files (F-34), and
#   3. prints EXACTLY ONE outcome to stdout, always.
# The invariant the Product Owner asked for — "never finish without printing a
# result or a failure, on every path" — is this function, and it is asserted:
# scripts/test_install.sh drives every exit path and checks stdout is non-empty.
_steps_cleanup() {
    local rc=$?
    nemr_region_stop 2>/dev/null || true
    nemr_cat_stop
    steps_tempclean
    case "${RESULT_STATE:-}" in
        ok)
            result_ok "${RESULT_OK_VERSION:-unknown}" "${RESULT_OK_DEST:-~/.local/bin/nemr}" \
                      "${RESULT_OK_CLAUDE:-}" ;;
        failed)
            result_failed "${RESULT_FAILED_STEP:-${FAILED_STEP:-a step}}" \
                          "${RESULT_BECAUSE:-it did not finish}" \
                          "${RESULT_FIX:-read the full log, then run this again}" ;;
        paused)
            result_paused "${RESULT_PAUSED_WHY:-a setting applies only after a reboot}" ;;
        handled)
            : ;;   # an early refusal (preflight, consent) already spoke, to stdout
        *)
            # Unmodelled. If a step recorded a failure but the state was lost,
            # print that; otherwise the blunt backstop. Either way, not silent.
            if [[ -n "${FAILED_STEP:-}" ]]; then
                result_failed "${RESULT_FAILED_STEP:-$FAILED_STEP}" \
                              "${RESULT_BECAUSE:-it did not finish}" \
                              "${RESULT_FIX:-read the full log, then run this again}"
            else
                result_unexpected
            fi ;;
    esac
    return $rc
}

# Ask once, after the plan. `--yes` (yes=1) skips the question; with no
# terminal and no --yes this refuses and names the flag (F-15) rather than
# proceeding silently. Anything but yes leaves the machine untouched, so
# declining is the dry run.
#
#   nemr_consent "$YES" "./scripts/install.sh"
nemr_consent() {
    local yes="$1" invocation="$2" reply=""
    [[ "$yes" == "1" ]] && return 0
    if [[ ! -t 0 ]]; then
        RESULT_STATE=handled
        printf '\n%sRefusing to go ahead without asking.%s\n' "$_S_RED" "$_S_RESET"
        printf 'It writes the files listed above, runs the privileged commands listed above and\n'
        printf 'downloads what is listed above, so it asks first — and there is no terminal here\n'
        printf 'to ask on. Pass --yes to accept that plan without the question:\n\n    %s --yes\n\n' "$invocation"
        exit 2
    fi
    printf '\nGo ahead? [y/N] '
    read -r reply || true
    case "$reply" in
        y|Y|yes|YES) printf '\n' ;;
        *) RESULT_STATE=handled; printf '\nNothing was changed.\n'; exit 0 ;;
    esac
}

# Ask for the password outside a running animation, so the prompt is never
# drawn over. A fresh timestamp makes the next sudo silent.
sudo_refresh() {
    # Nothing may write to the terminal while a live region owns it, and a
    # password prompt is a write. So the region is resolved first, the prompt
    # happens on a clean screen, and a fresh region opens under it.
    local reopen=0
    if (( _S_REGION )); then
        nemr_region_stop
        reopen=1
    fi
    local ok=0
    sudo -v && ok=1
    if (( ! ok )); then
        # Refused: the region is NOT reopened (nothing is going to run under
        # it) and the failure is recorded for the single authority to speak.
        RESULT_STATE=failed
        RESULT_FAILED_STEP="asking for your password"
        RESULT_BECAUSE="sudo did not accept it, or was refused"
        RESULT_FIX="run this again and enter your password when asked"
        exit 1
    fi
    if (( reopen )); then
        nemr_region_start "$_NEMR_REGION_STATE_PATH" "${STEP_OUT:-}" && _s_republish
    fi
}
