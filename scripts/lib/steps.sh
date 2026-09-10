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
else
    _S_GREEN=""; _S_RED=""; _S_DIM=""; _S_RESET=""
fi

tick()  { printf '  %s✓%s %s\n' "$_S_GREEN" "$_S_RESET" "$1"; }
cross() { printf '  %s✗%s %s\n' "$_S_RED" "$_S_RESET" "$1"; }
note()  { printf '    %s%s%s\n' "$_S_DIM" "$1" "$_S_RESET"; }
head2() { printf '\n%s\n' "$1"; }

LOG_DIR="$HOME/.local/state/nemr"
LOG="${LOG:-$LOG_DIR/install-$(date +%Y%m%d-%H%M%S).log}"
# Not created on load: a run that only shows its plan, or is declined, leaves
# this machine exactly as it found it — including this directory.
open_log() { mkdir -p "$LOG_DIR"; : >>"$LOG"; }

FAILED_STEP=""

# Run a command with its output in the log, never on the terminal.
logged() {
    printf '\n$ %s\n' "$*" >>"$LOG"
    "$@" >>"$LOG" 2>&1
}

# A long step: the work runs in its own process and the cat plays beside it.
# The install takes the same time whether or not a frame is ever drawn.
logged_long() {
    local rc=0
    printf '\n$ %s\n' "$*" >>"$LOG"
    if nemr_cat_enabled; then
        "$@" >>"$LOG" 2>&1 &
        local work=$!
        nemr_cat_start
        wait "$work" || rc=$?
        nemr_cat_stop
    else
        "$@" >>"$LOG" 2>&1 || rc=$?
    fi
    return "$rc"
}

# The cursor comes back and the cat is erased on every exit — normal, failed
# or interrupted — and a failure names the log rather than dumping it.
steps_trap() {
    trap '_steps_cleanup' EXIT
    trap 'FAILED_STEP="${FAILED_STEP:-interrupted}"; nemr_cat_stop; exit 130' INT TERM
}
_steps_cleanup() {
    local rc=$?
    nemr_cat_stop
    if [[ -n "$FAILED_STEP" ]]; then
        printf '\n%sStopped at: %s%s\n' "$_S_RED" "$FAILED_STEP" "$_S_RESET" >&2
        if [[ -s "$LOG" ]]; then
            printf 'What every command printed is in:\n  %s\n' "$LOG" >&2
        fi
        printf 'Nothing after that step ran. Fix the cause and run this again;\n' >&2
        printf 'it skips what is already done.\n' >&2
    fi
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
        printf '\n%sRefusing to go ahead without asking.%s\n' "$_S_RED" "$_S_RESET" >&2
        printf 'It writes the files listed above, runs the privileged commands listed above and\n' >&2
        printf 'downloads what is listed above, so it asks first — and there is no terminal here\n' >&2
        printf 'to ask on. Pass --yes to accept that plan without the question:\n\n    %s --yes\n\n' "$invocation" >&2
        exit 2
    fi
    printf '\nGo ahead? [y/N] '
    read -r reply || true
    case "$reply" in
        y|Y|yes|YES) printf '\n' ;;
        *) printf '\nNothing was changed.\n'; exit 0 ;;
    esac
}

# Ask for the password outside a running animation, so the prompt is never
# drawn over. A fresh timestamp makes the next sudo silent.
sudo_refresh() {
    sudo -v || { FAILED_STEP="sudo"; cross "sudo refused"; exit 1; }
}
