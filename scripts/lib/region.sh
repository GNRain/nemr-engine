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
NEMR_REGION_LEFT_COLS=40      # "  installing the privileged helper" is 34
NEMR_REGION_BAR_CELLS=18
NEMR_REGION_PANE_ROWS=5       # lines of the step's own output, inside a border
NEMR_REGION_PANE_TOP=5        # the pane's border starts here (rows 0-4 above it)
NEMR_REGION_MIN_COLS=$((NEMR_REGION_LEFT_COLS + NEMR_REGION_GAP + NEMR_REGION_CAT_COLS))
NEMR_REGION_MIN_ROWS=$((NEMR_REGION_CAT_ROWS + 3))

_NEMR_REGION_PID=""
_NEMR_REGION_STATE=""
_NEMR_REGION_TAIL=""
_NEMR_REGION_STOP=""
_NEMR_REGION_HID=""

nemr_region_enabled() {
    nemr_cat_enabled || return 1
    local size rows cols
    size="$(nemr_term_size)" || { _NEMR_SCREEN_WHY="the terminal would not report its size"; return 1; }
    rows="${size% *}"; cols="${size#* }"
    NEMR_SCREEN_MEASURED="$cols cols x $rows rows"
    (( cols >= NEMR_REGION_MIN_COLS )) \
        || { _NEMR_SCREEN_WHY="$cols columns, the screen needs $NEMR_REGION_MIN_COLS"; return 1; }
    (( rows >= NEMR_REGION_MIN_ROWS )) \
        || { _NEMR_SCREEN_WHY="$rows rows, the screen needs $NEMR_REGION_MIN_ROWS"; return 1; }
    _NEMR_SCREEN_WHY=""
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

# The bar, in characters every font has.
#
# It was drawn with U+2588 FULL BLOCK and U+2591 LIGHT SHADE, and on Windows
# Terminal it came out as one solid grey block (the Product Owner, 2026-09-10).
# The bytes were correct — a capture from here shows "█████░░░░░░░░░░░░░  4 of
# 14" — so the two characters were rendering as the same thing in that font, or
# in whatever font was substituted for them. There is no way to ask a terminal
# whether a glyph will render, so the only honest fix is not to need one: a bar
# made of ASCII draws the same everywhere, which is worth more than a bar that
# is prettier where the font happens to cooperate.
_NEMR_BAR_FULL="#"
_NEMR_BAR_EMPTY="-"

_nemr_region_bar() {   # <done> <total>
    local done="$1" total="$2" filled i out=""
    (( total < 1 )) && total=1
    filled=$(( done * NEMR_REGION_BAR_CELLS / total ))
    (( filled > NEMR_REGION_BAR_CELLS )) && filled=$NEMR_REGION_BAR_CELLS
    (( filled < 0 )) && filled=0
    for (( i = 0; i < NEMR_REGION_BAR_CELLS; i++ )); do
        if (( i < filled )); then out+="$_NEMR_BAR_FULL"; else out+="$_NEMR_BAR_EMPTY"; fi
    done
    # Bracketed, so the empty half is unmistakably part of the bar rather than
    # trailing punctuation.
    printf '[%s]  %d of %d' "$out" "$done" "$total"
}

# THE OUTPUT PANE: the last few lines the CURRENT step actually printed — the
# crate names while cargo builds, the packages while apt runs, the layers while
# the image pulls. Bordered, fixed height, emptied when the step changes.
#
# Every line is contained before it is drawn: escape sequences stripped (a step
# that prints colour or moves the cursor would otherwise write outside its own
# pane and tear the region), carriage returns and tabs flattened, and the result
# cut to the pane's inner width by CHARACTERS, not bytes, so a multi-byte glyph
# is never sliced in half. If the step printed nothing the pane is empty; it
# invents nothing to fill itself.
_nemr_pane_inner=$((NEMR_REGION_LEFT_COLS - 6))

_nemr_region_pane_line() {   # <row within the pane>
    local n="$1" line=""
    if (( n == 0 || n == NEMR_REGION_PANE_ROWS + 1 )); then
        printf '  +%s+' "$(printf '%*s' "$((_nemr_pane_inner + 2))" "" | tr ' ' '-')"
        return
    fi
    if [[ -n "$_NEMR_REGION_TAIL" && -s "$_NEMR_REGION_TAIL" ]]; then
        # The last few lines that CARRY something. Blank lines are printed lines,
        # but a pane of five that shows two of them is not the tail of anything.
        #
        # Then containment, in this order and for these reasons:
        #   sed    removes escape sequences — a step that colours its output or
        #          moves the cursor would otherwise write outside its own pane;
        #   tr     removes every remaining control byte EXCEPT the newline —
        #          deleting \012 as well collapses the whole tail into one line,
        #          which is what it did the first time. Including a lone ESC:
        #          The file is being written while this reads it, so the tail can
        #          catch half an escape sequence; truncation then leaves a
        #          dangling "\033[" that eats the pane's own border. Seen doing
        #          exactly that (2026-09-10) — the border lost its right edge and
        #          a stray "[2" appeared a row down.
        line="$(grep -v '^[[:space:]]*$' "$_NEMR_REGION_TAIL" 2>/dev/null \
                | tail -n "$NEMR_REGION_PANE_ROWS" \
                | sed -e 's/\x1b\[[0-9;?]*[a-zA-Z]//g' -e 's/\r/ /g' -e 's/\t/ /g' \
                | LC_ALL=C tr -d '\000-\011\013-\037\177' \
                | sed -n "${n}p")"
    fi
    line="${line:0:$_nemr_pane_inner}"
    printf '  | %-*s |' "$_nemr_pane_inner" "$line"
}

# The left column. ONE line is the step running now, and it is REPLACED, not
# appended to: at any moment the screen carries the current step and nothing
# else. No list, no file names, no digests.
_nemr_region_left() {   # <row> <phrase> <done> <total> <pane?>
    local row="$1" phrase="$2" done="$3" total="$4" pane="$5" text=""
    case "$row" in
        0) text="$_NEMR_REGION_BRIGHT  Installing nemr...$_NEMR_REGION_RESET" ;;
        1) text="  $phrase" ;;
        3) text="  $(_nemr_region_bar "$done" "$total")" ;;
        *)
            if (( pane )) && (( row >= NEMR_REGION_PANE_TOP )) \
               && (( row <= NEMR_REGION_PANE_TOP + NEMR_REGION_PANE_ROWS + 1 )); then
                text="$_NEMR_REGION_DIM$(_nemr_region_pane_line "$((row - NEMR_REGION_PANE_TOP))")$_NEMR_REGION_RESET"
            fi ;;
    esac
    # Padded to the column width, ignoring the escape sequences' own length.
    local visible="${text//$'\033'\[[0-9]m/}"
    visible="$(printf '%s' "$text" | sed 's/\x1b\[[0-9;]*m//g')"
    printf '%s%*s' "$text" "$(( NEMR_REGION_LEFT_COLS - ${#visible} ))" ""
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
        local phrase="" done=0 total=1 line2
        if [[ -r "$_NEMR_REGION_STATE" ]]; then
            IFS=$'\t' read -r phrase done total < "$_NEMR_REGION_STATE"
        fi
        [[ "$done" =~ ^[0-9]+$ ]] || done=0
        [[ "$total" =~ ^[0-9]+$ ]] || total=1

        local cat_lines=()
        while IFS= read -r line; do cat_lines+=("$line"); done \
            < <(printf '%s' "${frames[$((i % n))]}")

        height=${#cat_lines[@]}
        (( height > rows - 2 )) && height=$((rows - 2))
        (( height < 1 )) && height=1
        # The pane only when the rows are there for it. It sits inside the cat's
        # own height, so on any terminal tall enough for the region it costs
        # nothing; this is what happens when the region is squeezed anyway.
        local pane=0
        (( height >= NEMR_REGION_PANE_TOP + NEMR_REGION_PANE_ROWS + 2 )) && pane=1

        if (( height > reserved )); then
            local k
            for (( k = reserved; k < height - 1; k++ )); do printf '\r\n'; done
            (( height - 1 > reserved )) && printf '\033[%dA' "$((height - 1 - reserved))"
            printf '\r'
            reserved=$((height - 1))
        fi

        local r left right
        for (( r = 0; r < height; r++ )); do
            left="$(_nemr_region_left "$r" "$phrase" "$done" "$total" "$pane")"
            right=""
            (( r < ${#cat_lines[@]} )) && right="${cat_lines[$r]}"
            printf '\033[2K%s%*s%s' "$left" "$NEMR_REGION_GAP" "" "$right"
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
            # Erase the block and go. What replaces it — the result, in four
            # lines — is the main shell's to print, because Ctrl-C reaches the
            # whole process group and kills this renderer outright.
            printf '\r\033[J'
            break
        fi
    done
}

nemr_region_start() {   # <state file> [<the current step's output file>]
    nemr_region_enabled || return 1
    [[ -n "$_NEMR_REGION_PID" ]] && return 0
    _NEMR_REGION_STATE="$1"
    _NEMR_REGION_TAIL="${2:-}"
    : >"$_NEMR_REGION_STATE" 2>/dev/null \
        || { _NEMR_SCREEN_WHY="cannot write $_NEMR_REGION_STATE"; return 1; }
    _NEMR_REGION_STOP="$(mktemp -u "${TMPDIR:-/tmp}/nemr-region.XXXXXX")"
    # A fifo cannot live on a Windows drive under WSL2, and TMPDIR sometimes
    # points at one — a silent fallback on exactly the platform that reported one.
    mkfifo -m 0600 "$_NEMR_REGION_STOP" 2>/dev/null \
        || { _NEMR_SCREEN_WHY="no fifo in ${TMPDIR:-/tmp}"; _NEMR_REGION_STOP=""; return 1; }
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

# Publish what is happening now: the phrase, and how many steps are done out of
# how many. Written to a temp and renamed, so a frame never reads half of it.
nemr_region_publish() {   # <phrase> <done> <total>
    [[ -n "$_NEMR_REGION_STATE" ]] || return 0
    local tmp="${_NEMR_REGION_STATE}.new"
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >"$tmp" && mv -f "$tmp" "$_NEMR_REGION_STATE"
}

# Nothing to resolve into: the live block is erased and the RESULT is printed
# after it by the caller, in four lines. The step list it used to resolve into
# is gone with the narration (SPEC 1.145).
nemr_region_stop() {
    if [[ -n "$_NEMR_REGION_PID" ]]; then
        printf 's\n' >&7 2>/dev/null || true
        wait "$_NEMR_REGION_PID" 2>/dev/null || true
        kill -0 "$_NEMR_REGION_PID" 2>/dev/null && {
            kill "$_NEMR_REGION_PID" 2>/dev/null
            wait "$_NEMR_REGION_PID" 2>/dev/null
        }
        exec 7>&- 2>/dev/null || true
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
