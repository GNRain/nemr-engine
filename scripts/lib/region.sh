#!/usr/bin/env bash
#
# The Installing block as a live two-column region (F-27).
#
#   left   the step lines, redrawn in place as steps complete
#   right  Nemr, looping beside them
#
# ONE writer owns the whole block. The renderer process draws both columns
# together, every frame, so the two can never interleave and nothing scrolls
# while the install runs. When it stops it draws the region one last time
# WITHOUT the cat and leaves the cursor below it, so the block resolves to the
# final step list and everything after it continues underneath.
#
# The main shell never draws. It publishes step state to a file; the renderer
# reads that file each frame. That is also why a step's own output can never
# tear the layout: it goes to the log, as it already did, and the region is the
# only thing writing to the terminal.
#
# THE THRESHOLDS, and what happens below them:
#
#   width  < NEMR_REGION_MIN_COLS (129 = 89 for the widest step line + 3 gap +
#          37 for the cat)  ->  no columns. Falls back to today's append-only
#          output rather than wrapping or truncating either column. The widest
#          line is measured, not guessed: the base-image step prints 91 columns
#          including its digest.
#   height < 17 rows (15 for the cat + 2)  ->  no animation at all, exactly as
#          before: the block is printed line by line as it happens.
#
# MORE STEPS THAN ROWS. The region is as tall as the cat or the step list,
# whichever is taller, and never taller than the terminal minus two. If the
# step list outgrows that, the left column becomes a WINDOW anchored on the
# step running now — the last rows of the list, with a "+N earlier" marker on
# the first line — because the point of the region is that the user never has
# to scroll to watch it. It does not scroll the terminal, and it does not
# silently hide steps: the marker says how many are above.

[[ -n "${_NEMR_REGION_SH_LOADED:-}" ]] && return 0 2>/dev/null || true
_NEMR_REGION_SH_LOADED=1

NEMR_REGION_CAT_COLS=37
NEMR_REGION_CAT_ROWS=15
NEMR_REGION_GAP=3
NEMR_REGION_STEP_COLS="${NEMR_REGION_STEP_COLS:-89}"
NEMR_REGION_MIN_COLS=$((NEMR_REGION_STEP_COLS + NEMR_REGION_GAP + NEMR_REGION_CAT_COLS))
NEMR_REGION_MIN_ROWS=$((NEMR_REGION_CAT_ROWS + 2))

_NEMR_REGION_PID=""
_NEMR_REGION_STATE=""
_NEMR_REGION_STOP=""
_NEMR_REGION_HID=""

# Columns and rows for a region, or nothing. Every "no" falls back to the
# append-only output that came before this file existed.
nemr_region_enabled() {
    nemr_cat_enabled || return 1
    local size rows cols
    size="$(nemr_term_size)" || return 1
    rows="${size% *}"
    cols="${size#* }"
    [[ "$cols" =~ ^[0-9]+$ && "$rows" =~ ^[0-9]+$ ]] || return 1
    (( cols >= NEMR_REGION_MIN_COLS )) || return 1
    (( rows >= NEMR_REGION_MIN_ROWS )) || return 1
    return 0
}

# The renderer. Draws both columns from the state file until asked to stop,
# then draws the final frame without the cat and leaves the cursor below it.
_nemr_region_loop() {
    local frames=() frame="" line
    while IFS= read -r line; do
        if [[ "$line" == "%%" ]]; then frames+=("$frame"); frame=""
        else frame+="$line"$'\n'; fi
    done < <(_nemr_cat_frames)
    [[ -n "$frame" ]] && frames+=("$frame")

    local rows height written=0 i=0 n=${#frames[@]} size
    size="$(nemr_term_size)" || size="24 80"
    rows="${size% *}"

    # Erase exactly what this frame has written if we are signalled part-way
    # through it — the F-26 rule, which this renderer is held to as well.
    # shellcheck disable=SC2064
    trap 'if (( written > 0 )); then printf "\033[%dA\r\033[J" "$written"; else printf "\r\033[J"; fi; exit 0' TERM INT

    exec 9<>"$_NEMR_REGION_STOP"

    local stopping=0 last_height=0 reserved=0
    while :; do
        # The state file, read whole each frame: the main shell replaces it
        # atomically, so a frame never sees half an update.
        local steps=() s
        while IFS= read -r s; do steps+=("$s"); done <"$_NEMR_REGION_STATE" 2>/dev/null

        local cat_lines=()
        if (( ! stopping )); then
            while IFS= read -r line; do cat_lines+=("$line"); done \
                < <(printf '%s' "${frames[$((i % n))]}")
        fi

        # How tall: the taller column, capped by the terminal.
        local want=${#steps[@]}
        (( ${#cat_lines[@]} > want )) && want=${#cat_lines[@]}
        # The final pass must cover every row the live frames covered, or the
        # rows the cat used and the steps do not are left holding the cat.
        (( stopping && last_height > want )) && want=$last_height
        (( want > rows - 2 )) && want=$((rows - 2))
        (( want < 1 )) && want=1
        height=$want
        last_height=$height

        # Reserve the block before drawing into it, for the same reason the
        # cat's drawer does: a block started near the foot of the screen
        # scrolls mid-frame and strands the rows written before the scroll.
        if (( height > reserved )); then
            local k
            for (( k = reserved; k < height - 1; k++ )); do printf '\r\n'; done
            (( height - 1 > reserved )) && printf '\033[%dA' "$((height - 1 - reserved))"
            printf '\r'
            reserved=$((height - 1))
        fi

        # A window over the steps when the list is taller than the region,
        # anchored on the end — what is happening now is always visible.
        local first=0 hidden=0
        if (( ${#steps[@]} > height )); then
            first=$(( ${#steps[@]} - height ))
            hidden=$first
        fi

        local r left right
        for (( r = 0; r < height; r++ )); do
            left=""
            if (( r == 0 && hidden > 0 )); then
                left="$(printf '  … %d earlier step(s) above' "$hidden")"
            elif (( first + r < ${#steps[@]} )); then
                left="${steps[$((first + r))]}"
            fi
            right=""
            (( r < ${#cat_lines[@]} )) && right="${cat_lines[$r]}"
            if [[ -n "$right" ]]; then
                printf '\033[2K%-*s%*s%s' \
                    "$NEMR_REGION_STEP_COLS" "$left" "$NEMR_REGION_GAP" "" "$right"
            else
                printf '\033[2K%s' "$left"
            fi
            # No newline on the block's last row: at the foot of the screen it
            # would scroll, and every cursor-up here is relative (F-26's lesson,
            # which cost four stranded rows in a 20-row terminal).
            if (( r < height - 1 )); then
                printf '\r\n'      # never a bare \n: see cat.sh on raw mode
                written=$((written + 1))
            fi
        done

        if (( stopping )); then
            # The final frame stays: no full cursor-up, so everything after the
            # region continues below it. But the region was as tall as the CAT,
            # and the steps are shorter — come back up over those now-blank rows
            # so the block resolves exactly to the list, with nothing after it.
            # The cursor is on the block's last row, with no newline after it.
            # Come back up over the rows the cat used and the steps do not, then
            # one newline to leave the cursor under the list.
            local blank=$((height - ${#steps[@]}))
            (( blank > 0 )) && printf '\033[%dA' "$blank"
            printf '\r\n'
            written=0
            break
        fi

        (( height > 1 )) && printf '\033[%dA' "$((height - 1))"
        printf '\r'
        written=0
        i=$((i + 1))
        if read -r -t "$NEMR_CAT_DELAY" -u 9 _; then
            stopping=1          # one more pass: the steps, without the cat
        fi
    done
}

# Open the region. `$1` is the file the main shell publishes step lines to.
nemr_region_start() {
    nemr_region_enabled || return 1
    [[ -n "$_NEMR_REGION_PID" ]] && return 0
    _NEMR_REGION_STATE="$1"
    _NEMR_REGION_STOP="$(mktemp -u "${TMPDIR:-/tmp}/nemr-region.XXXXXX")"
    mkfifo -m 0600 "$_NEMR_REGION_STOP" 2>/dev/null || { _NEMR_REGION_STOP=""; return 1; }
    printf '\033[?25l'
    _NEMR_REGION_HID=1
    _nemr_region_loop &
    _NEMR_REGION_PID=$!
    exec 7<>"$_NEMR_REGION_STOP"
    return 0
}

# Publish the step lines. Written to a temp and renamed, so the renderer
# always reads a whole state.
nemr_region_publish() {   # <line>...
    [[ -n "$_NEMR_REGION_STATE" ]] || return 0
    local tmp="${_NEMR_REGION_STATE}.new"
    printf '%s\n' "$@" >"$tmp" && mv -f "$tmp" "$_NEMR_REGION_STATE"
}

# Close it: the renderer draws the steps once more without the cat, leaves the
# cursor below them, and exits. Safe twice; safe when nothing is open.
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
