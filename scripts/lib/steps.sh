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
    _S_AMBER=$'\033[33m'
else
    _S_GREEN=""; _S_RED=""; _S_DIM=""; _S_RESET=""; _S_AMBER=""
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

_s_republish() {
    (( _S_REGION )) || return 0
    local out=() i
    for i in "${!_S_LABELS[@]}"; do
        out+=("$(printf '%s\t%s\t%s' "${_S_STATES[$i]}" "${_S_LABELS[$i]}" "${_S_TOKENS[$i]}")")
    done
    nemr_region_publish "${out[@]}"
}

# Seed the list, all pending.
steps_seed() {   # <label>...
    local l
    _S_LABELS=(); _S_STATES=(); _S_TOKENS=()
    for l in "$@"; do _S_LABELS+=("$l"); _S_STATES+=("wait"); _S_TOKENS+=(""); done
    _s_republish
}

# The step at $_S_IDX is the one running now.
step_begin() {
    _S_STATES[$_S_IDX]="run"
    _S_TOKENS[$_S_IDX]="…"
    _s_republish
}

# How it went. <state> is done|warn|fail; <token> fills the status column;
# <detail> is the full sentence, which the log always gets and an append-only
# run also prints.
step_result() {   # <state> <token> <detail>
    local state="$1" token="$2" detail="$3" label="${_S_LABELS[$_S_IDX]:-}"
    _S_STATES[$_S_IDX]="$state"
    _S_TOKENS[$_S_IDX]="$token"
    # A failure carries its own explanation. The cleanup prints "Stopped at …"
    # and the log path only when FAILED_STEP is set, and a step that failed
    # before something else set it left a resolved list with no reason on it.
    [[ "$state" == fail ]] && FAILED_STEP="$label"
    printf '[%s] %s — %s\n' "$state" "$label" "$detail" >>"$LOG" 2>/dev/null || true
    if (( _S_REGION )); then
        _s_republish
    else
        case "$state" in
            done) printf '  %s✓%s %s — %s\n' "$_S_GREEN" "$_S_RESET" "$label" "$detail" ;;
            warn) printf '  %s!%s %s — %s\n' "$_S_AMBER" "$_S_RESET" "$label" "$detail" ;;
            *)    printf '  %s✗%s %s — %s\n' "$_S_RED" "$_S_RESET" "$label" "$detail" ;;
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
    # A step can change the terminal's mode even with its output redirected:
    # the smoke test attaches to a session, and `nemr attach` puts the tty in
    # raw mode through /dev/tty. Save the settings and put them back, so what
    # runs next — the drawing, and the user's shell afterwards — gets the
    # terminal it expects.
    local tty_state=""
    [[ -t 1 ]] && tty_state="$(stty -g 2>/dev/null || true)"
    if (( _S_REGION )); then
        # The region draws the cat already, and it is the only writer.
        "$@" >>"$LOG" 2>&1 &
        local work=$!
        wait "$work" || rc=$?
    elif nemr_cat_enabled; then
        "$@" >>"$LOG" 2>&1 &
        local work=$!
        nemr_cat_start
        wait "$work" || rc=$?
        nemr_cat_stop
    else
        "$@" >>"$LOG" 2>&1 || rc=$?
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
    trap 'FAILED_STEP="${FAILED_STEP:-interrupted}"; nemr_region_stop 2>/dev/null; nemr_cat_stop; exit 130' INT TERM
}
_steps_cleanup() {
    local rc=$?
    nemr_region_stop 2>/dev/null || true
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
    if (( reopen )); then
        nemr_region_start "$_NEMR_REGION_STATE_PATH" && nemr_region_publish "${_S_LINES[@]}"
    fi
    (( ok )) || { FAILED_STEP="sudo"; cross "sudo refused"; exit 1; }
}
