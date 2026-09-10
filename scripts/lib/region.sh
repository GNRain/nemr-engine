#!/usr/bin/env bash
#
# The install screen: a live region the whole run happens in (F-27, F-29).
#
#   left    the step list, redrawn in place — never appended
#   right   Nemr, looping beside it
#
# ONE writer owns the block. The renderer draws both columns together every
# frame; the main shell only publishes step state to a file it reads. Nothing
# else may write to the terminal while it is open, or the layout tears.
#
# HOW IT TAKES THE SCREEN. It does NOT clear. Clearing would take the user's
# scrollback with it, and what was on the screen before is theirs. Instead the
# region is SCROLLED into place: the cursor goes to the bottom row and prints
# newlines, which moves everything above it up into scrollback intact, until
# the command line the user typed sits on the first row. The region then owns
# every row below it.
#
# GEOMETRY, and the thresholds (SPEC 1.144):
#
#   left column   4 + 29 + 1 + 7 = 41
#                 2 indent, 1 glyph, 1 space, 29 label, 1 space, 7 status
#   gap           2
#   cat           37
#   ------------- 80 columns, the default terminal width.
#
#   The status has a FIXED BUDGET and is truncated to it; the full text goes to
#   the log. A column sized to whatever a step decides to print is not a design:
#   sized from an idempotent run it was 89, and a first run's package list made
#   the line 145, which wrapped over the next step's row and cost the cat its
#   first line (docs/reports/2026-09-10-install-region-width.md).
#
#   Below 80 columns, or below NEMR_REGION_MIN_ROWS rows, THE REGION IS NEVER
#   STARTED and the run prints append-only, exactly as it did before.

[[ -n "${_NEMR_REGION_SH_LOADED:-}" ]] && return 0 2>/dev/null || true
_NEMR_REGION_SH_LOADED=1

NEMR_REGION_CAT_COLS=37
NEMR_REGION_CAT_ROWS=15
NEMR_REGION_GAP=2
NEMR_REGION_LABEL_COLS=29
NEMR_REGION_STATUS_COLS=7
NEMR_REGION_STEP_COLS=$((4 + NEMR_REGION_LABEL_COLS + 1 + NEMR_REGION_STATUS_COLS))
NEMR_REGION_MIN_COLS=$((NEMR_REGION_STEP_COLS + NEMR_REGION_GAP + NEMR_REGION_CAT_COLS))
NEMR_REGION_MIN_ROWS=$((NEMR_REGION_CAT_ROWS + 3))

_NEMR_REGION_PID=""
_NEMR_REGION_STATE=""
_NEMR_REGION_STOP=""
_NEMR_REGION_HID=""

nemr_region_enabled() {
    nemr_cat_enabled || return 1
    local size rows cols
    size="$(nemr_term_size)" || return 1
    rows="${size% *}"; cols="${size#* }"
    (( cols >= NEMR_REGION_MIN_COLS )) || return 1
    (( rows >= NEMR_REGION_MIN_ROWS )) || return 1
    return 0
}

# Where the cursor is, so the region knows how far to scroll. Asking the
# terminal (DSR) rather than assuming: the plan above may be one line or fifty.
_nemr_cursor_row() {
    local row
    # TEST-ONLY seam. A captured pty has no terminal emulator on the other end,
    # so nothing answers the query below and the scroll cannot be exercised in a
    # capture. The acceptance supplies the row it would have got.
    if [[ -n "${NEMR_TEST_CURSOR_ROW:-}" ]]; then
        printf '%s' "$NEMR_TEST_CURSOR_ROW"; return 0
    fi
    # The query goes to /dev/tty, not through `read -p`: that writes its prompt
    # to STDERR, and stderr is very often redirected — the query then never
    # reaches the terminal and the answer never comes.
    printf '\033[6n' >/dev/tty 2>/dev/null || return 1
    # A short wait: a terminal answers in microseconds, and anything that does
    # not answer at all must not hold the first frame back. At one second the
    # region took a visible beat to appear wherever nothing was listening.
    IFS='[;' read -rsd R -t 0.2 _ row _ </dev/tty 2>/dev/null || return 1
    [[ "$row" =~ ^[0-9]+$ ]] || return 1
    printf '%s' "$row"
}

# Scroll — never clear — until the user's command line is the top row.
_nemr_region_take_screen() {
    local rows="$1" here scroll
    # If the terminal will not say where the cursor is, the region draws where
    # it stands rather than guessing: scrolling by a guess would push the user's
    # own output off the screen for no reason. Every terminal worth the name
    # answers; a captured pty does not, which is what the seam above is for.
    here="$(_nemr_cursor_row)" || return 0
    scroll=$((here - 2))
    (( scroll <= 0 )) && return 0
    (( scroll > rows )) && scroll=$rows
    printf '\033[%d;1H' "$rows"                # to the bottom row
    local i
    for (( i = 0; i < scroll; i++ )); do printf '\r\n'; done
    printf '\033[2;1H'                         # under the command line
}

# One step line: "  <glyph> <label padded> <status right-aligned>", coloured by
# state. Colour only when the caller says the terminal takes it.
_nemr_region_line() {   # <state> <label> <status>
    local state="$1" label="$2" status="$3" glyph=" " colour="" reset=""
    case "$state" in
        done) glyph="+"; colour="$_NEMR_REGION_GREEN" ;;
        run)  glyph=">"; colour="$_NEMR_REGION_BRIGHT" ;;
        wait) glyph="."; colour="$_NEMR_REGION_DIM" ;;
        fail) glyph="x"; colour="$_NEMR_REGION_RED"; status="FAILED" ;;
        warn) glyph="!"; colour="$_NEMR_REGION_AMBER" ;;
    esac
    [[ -n "$colour" ]] && reset="$_NEMR_REGION_RESET"
    label="${label:0:$NEMR_REGION_LABEL_COLS}"
    status="${status:0:$NEMR_REGION_STATUS_COLS}"
    printf '%s  %s %-*s %*s%s' "$colour" "$glyph" \
        "$NEMR_REGION_LABEL_COLS" "$label" "$NEMR_REGION_STATUS_COLS" "$status" "$reset"
}

# The renderer.
_nemr_region_loop() {
    local frames=() frame="" line
    while IFS= read -r line; do
        if [[ "$line" == "%%" ]]; then frames+=("$frame"); frame=""
        else frame+="$line"$'\n'; fi
    done < <(_nemr_cat_frames)
    [[ -n "$frame" ]] && frames+=("$frame")

    local rows size height written=0 i=0 n=${#frames[@]}
    size="$(nemr_term_size)" || size="24 80"
    rows="${size% *}"

    # shellcheck disable=SC2064
    trap 'if (( written > 0 )); then printf "\033[%dA\r\033[J" "$written"; else printf "\r\033[J"; fi; exit 0' TERM INT

    exec 9<>"$_NEMR_REGION_STOP"
    _nemr_region_take_screen "$rows"

    local reserved=0
    while :; do
        local steps=() s
        # Guarded, not silenced with 2>/dev/null on the read: a redirect that
        # fails reports on stderr before the read ever runs, and that message
        # lands in the middle of the region. It did (2026-09-10, first frame,
        # before the first publish).
        if [[ -r "$_NEMR_REGION_STATE" ]]; then
            while IFS= read -r s; do steps+=("$s"); done <"$_NEMR_REGION_STATE"
        fi

        local cat_lines=()
        while IFS= read -r line; do cat_lines+=("$line"); done \
            < <(printf '%s' "${frames[$((i % n))]}")

        local want=${#steps[@]}
        (( ${#cat_lines[@]} > want )) && want=${#cat_lines[@]}
        (( want > rows - 2 )) && want=$((rows - 2))
        (( want < 1 )) && want=1
        height=$want
        last_height=$height

        if (( height > reserved )); then
            local k
            for (( k = reserved; k < height - 1; k++ )); do printf '\r\n'; done
            (( height - 1 > reserved )) && printf '\033[%dA' "$((height - 1 - reserved))"
            printf '\r'
            reserved=$((height - 1))
        fi

        # A window over the steps when the list is taller than the region,
        # anchored on the end, so the step running now is always visible.
        local first=0 hidden=0
        if (( ${#steps[@]} > height )); then
            first=$(( ${#steps[@]} - height ))
            hidden=$first
        fi

        local r left right state label status
        for (( r = 0; r < height; r++ )); do
            left=""
            if (( r == 0 && hidden > 0 )); then
                # Says how many are above, in the label's budget. The steps
                # above are not hidden — they are in scrollback once the region
                # resolves, and this line is what points at them.
                left="$(_nemr_region_line wait "… $hidden more above" "")"
            elif (( first + r < ${#steps[@]} )); then
                IFS=$'\t' read -r state label status <<<"${steps[$((first + r))]}"
                left="$(_nemr_region_line "$state" "$label" "$status")"
            else
                left="$(printf '%*s' "$NEMR_REGION_STEP_COLS" "")"
            fi
            right=""
            (( r < ${#cat_lines[@]} )) && right="${cat_lines[$r]}"
            if [[ -n "$right" ]]; then
                printf '\033[2K%s%*s%s' "$left" "$NEMR_REGION_GAP" "" "$right"
            else
                printf '\033[2K%s' "$left"
            fi
            if (( r < height - 1 )); then
                printf '\r\n'      # never a bare \n: a step can leave the tty raw
                written=$((written + 1))
            fi
        done

        (( height > 1 )) && printf '\033[%dA' "$((height - 1))"
        printf '\r'
        written=0
        i=$((i + 1))
        if read -r -t "$NEMR_CAT_DELAY" -u 9 _; then
            # Erase the block and go. The RESOLVE — printing the list as
            # ordinary text — belongs to the main shell, because Ctrl-C reaches
            # the whole process group and this renderer dies with it. A resolve
            # that only the renderer could perform is a resolve that does not
            # happen on the one exit that most needs it.
            printf '\r\033[J'
            break
        fi
    done
}

nemr_region_start() {
    nemr_region_enabled || return 1
    [[ -n "$_NEMR_REGION_PID" ]] && return 0
    _NEMR_REGION_STATE="$1"
    : >"$_NEMR_REGION_STATE" 2>/dev/null || return 1   # exists before a frame reads it
    _NEMR_REGION_STOP="$(mktemp -u "${TMPDIR:-/tmp}/nemr-region.XXXXXX")"
    mkfifo -m 0600 "$_NEMR_REGION_STOP" 2>/dev/null || { _NEMR_REGION_STOP=""; return 1; }
    # Colour is decided once, here, by the same rules the animation follows.
    if [[ -t 1 && -z "${NO_COLOR:-}" ]] && [[ "${TERM:-dumb}" != "dumb" ]]; then
        _NEMR_REGION_GREEN=$'\033[32m'; _NEMR_REGION_BRIGHT=$'\033[1m'
        _NEMR_REGION_DIM=$'\033[2m';    _NEMR_REGION_RED=$'\033[31m'
        _NEMR_REGION_AMBER=$'\033[33m'; _NEMR_REGION_RESET=$'\033[0m'
    else
        _NEMR_REGION_GREEN=""; _NEMR_REGION_BRIGHT=""; _NEMR_REGION_DIM=""
        _NEMR_REGION_RED="";   _NEMR_REGION_AMBER="";  _NEMR_REGION_RESET=""
    fi
    printf '\033[?25l'
    _NEMR_REGION_HID=1
    _nemr_region_loop &
    _NEMR_REGION_PID=$!
    exec 7<>"$_NEMR_REGION_STOP"
    return 0
}

# Publish the step list: one "<state>\t<label>\t<status>" per line, written to a
# temp and renamed so a frame never reads half an update.
nemr_region_publish() {
    [[ -n "$_NEMR_REGION_STATE" ]] || return 0
    local tmp="${_NEMR_REGION_STATE}.new"
    printf '%s\n' "$@" >"$tmp" && mv -f "$tmp" "$_NEMR_REGION_STATE"
}

# Print the step list as ordinary scrollback text. This is the resolve, and the
# main shell does it — after the renderer has gone — so it happens identically
# whether the run finished, a step failed, or Ctrl-C killed the renderer
# outright. What is on the screen afterwards is text: scrollable, copyable, and
# still there when the script has exited.
_nemr_region_resolve() {
    [[ -r "$_NEMR_REGION_STATE" ]] || return 0
    local s state label status
    while IFS= read -r s; do
        IFS=$'\t' read -r state label status <<<"$s"
        printf '%s\r\n' "$(_nemr_region_line "$state" "$label" "$status")"
    done <"$_NEMR_REGION_STATE"
}

nemr_region_stop() {
    if [[ -n "$_NEMR_REGION_PID" ]]; then
        printf 's\n' >&7 2>/dev/null || true
        wait "$_NEMR_REGION_PID" 2>/dev/null || true
        kill -0 "$_NEMR_REGION_PID" 2>/dev/null && {
            kill "$_NEMR_REGION_PID" 2>/dev/null
            wait "$_NEMR_REGION_PID" 2>/dev/null
        }
        exec 7>&- 2>/dev/null || true
        _nemr_region_resolve
        rm -f "$_NEMR_REGION_STOP" "${_NEMR_REGION_STATE}.new"
        _NEMR_REGION_PID=""
        _NEMR_REGION_STOP=""
    fi
    if [[ -n "$_NEMR_REGION_HID" ]]; then
        printf '\033[?25h'
        _NEMR_REGION_HID=""
    fi
    return 0
}
