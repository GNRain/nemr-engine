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
# GEOMETRY — FITTED TO THE WINDOW (SPEC 1.147):
#
#   The cat is 37 columns wide and sits against the RIGHT edge whatever the
#   terminal's width is. Everything to its left belongs to the output pane, so
#   a wider window is a wider pane, and nothing is ever drawn off the screen:
#
#       left column = cols - 2 (gap) - 37 (cat)
#
#      80 cols  ->  left 41    pane inner 35      (the default terminal)
#     100 cols  ->  left 61    pane inner 55
#     120 cols  ->  left 81    pane inner 75
#
#   THE MINIMUM PANE WIDTH is NEMR_REGION_LEFT_MIN — 40 columns of left column,
#   34 of pane inner, which is "  installing the privileged helper", the longest
#   phrase, inside its border. Below that the column can no longer hold both a
#   phrase and useful output, so THE REGION IS NEVER STARTED: under
#   NEMR_REGION_MIN_COLS = 40 + 2 + 37 = 79 columns, or under
#   NEMR_REGION_MIN_ROWS rows, the run falls back to append-only printing
#   exactly as it did before there was a screen, and the log says why.
#
#   A RESIZE MID-RUN IS HANDLED (SPEC 1.150). The width is measured when the
#   screen opens, and again on SIGWINCH: the block is erased, the widths are
#   re-fitted and it is redrawn at the new size. If the new window is under the
#   thresholds above, the screen stops instead — erased, one line saying which
#   size it has and which it needs, and the rest of the run printed plainly.
#
#   It used to be ruled out of scope, and the cost of that was a screen stacked
#   twenty deep: every frame ends by moving the cursor up by the rows it wrote,
#   and a terminal that has rewrapped those rows has moved them.
#
#   Pane content has a FIXED BUDGET and is truncated to the pane's inner width;
#   the full text goes to the log. A column sized to whatever a step decides to
#   print is not a design: sized from an idempotent run it was 89, and a first
#   run's package list made the line 145, which wrapped over the next step's row
#   and cost the cat its first line (docs/reports/2026-09-10-install-region-width.md).

[[ -n "${_NEMR_REGION_SH_LOADED:-}" ]] && return 0 2>/dev/null || true
_NEMR_REGION_SH_LOADED=1

NEMR_REGION_CAT_COLS=37
NEMR_REGION_CAT_ROWS=15
NEMR_REGION_GAP=2
# THE ICON RULE (SPEC 1.148). The two top rows carry an emoji. An emoji is ONE
# character to bash and TWO columns to the terminal, so its width is COUNTED,
# never measured: every icon is a single codepoint with East Asian Width W and
# default emoji presentation — no U+FE0F variation selector, which terminals
# disagree about — and is worth exactly NEMR_REGION_ICON_CELLS columns wherever
# a width is computed. `scripts/test_install.sh` asserts that property for every
# icon in the table against python's unicodedata, so an icon that cannot be
# guaranteed two columns fails the gate rather than tearing somebody's screen.
NEMR_REGION_ICON_CELLS=2

# Icons go when colour goes — no terminal, NO_COLOR, TERM=dumb — and in two more
# cases colour does not care about but the LAYOUT does:
#
#   TERM=linux    the physical console has 256 glyphs and no emoji among them;
#                 the substitute is narrow and the row tears.
#   a non-UTF-8 locale
#                 the shell and the terminal are then decoding differently, and
#                 four bytes drawn as four characters is a four-column icon.
#
# NEMR_NO_EMOJI=1 turns them off by hand, for the machine whose font we cannot
# reproduce. tmux and screen are deliberately NOT excluded: both have carried
# correct wide-character tables for years, and refusing there would cost the
# icons for a large share of real users to guard against a decade-old bug.
_NEMR_EMOJI_WHY=""
nemr_emoji_enabled() {
    [[ -z "${NEMR_NO_EMOJI:-}" ]] || { _NEMR_EMOJI_WHY="NEMR_NO_EMOJI is set"; return 1; }
    [[ -t 1 ]]                    || { _NEMR_EMOJI_WHY="not a terminal"; return 1; }
    [[ -z "${NO_COLOR:-}" ]]      || { _NEMR_EMOJI_WHY="NO_COLOR is set"; return 1; }
    case "${TERM:-dumb}" in
        dumb|linux) _NEMR_EMOJI_WHY="TERM=${TERM:-dumb} has no emoji font"; return 1 ;;
    esac
    case "${LC_ALL:-${LC_CTYPE:-${LANG:-}}}" in
        *UTF-8*|*utf-8*|*UTF8*|*utf8*) ;;
        *) _NEMR_EMOJI_WHY="the locale is not UTF-8"; return 1 ;;
    esac
    _NEMR_EMOJI_WHY=""
    return 0
}
NEMR_REGION_TITLE_ICON="🐈"    # U+1F408 cat, constant for the whole run

NEMR_REGION_LEFT_MIN=40       # "  installing the privileged helper" is 34
NEMR_REGION_LEFT_COLS=$NEMR_REGION_LEFT_MIN   # refitted to the window at start
NEMR_REGION_BAR_CELLS=18
NEMR_REGION_PANE_ROWS=5       # lines of the step's own output, inside a border
NEMR_REGION_PANE_TOP=5        # the pane's border starts here (rows 0-4 above it)
NEMR_REGION_MIN_COLS=$((NEMR_REGION_LEFT_MIN + NEMR_REGION_GAP + NEMR_REGION_CAT_COLS))
NEMR_REGION_MIN_ROWS=$((NEMR_REGION_CAT_ROWS + 3))

# Compose an icon row: two spaces of indent, the icon, one space, the text —
# and the number of COLUMNS that occupies, built from the parts rather than
# measured with \${#...}, which counts a two-column icon as one and would walk
# the cat a column left on every row that has one.
#
# The text is truncated to what is left of the left column, so a step phrase
# longer than the column can never push the cat either.
_nemr_region_iconed() {   # <icon> <text>   -> _icon_plain, _icon_cells
    local icon="$1" text="$2" lead=2 icells=0 room
    if (( _NEMR_EMOJI_ON )) && [[ -n "$icon" ]]; then
        icells=$(( NEMR_REGION_ICON_CELLS + 1 ))     # the icon, and the space after it
    fi
    room=$(( NEMR_REGION_LEFT_COLS - lead - icells ))
    (( room < 0 )) && room=0
    # Scrubbed to ASCII first, so "characters are columns" is enforced rather
    # than assumed: a phrase carrying a wide or a zero-width character would be
    # counted wrong in every locale, and the icon is the only non-ASCII thing
    # this column is allowed to hold.
    text="${text//[^ -~]/}"
    text="${text:0:$room}"
    if (( icells )); then
        _icon_plain="  $icon $text"
    else
        _icon_plain="  $text"
    fi
    _icon_cells=$(( lead + icells + ${#text} ))
}
_icon_plain=""; _icon_cells=0

# Fit the layout to the window: the cat against the right edge, the pane taking
# what that leaves. Called once per run — see the resize note above.
_nemr_region_fit() {   # <cols>
    local cols="${1:-80}"
    NEMR_REGION_LEFT_COLS=$(( cols - NEMR_REGION_GAP - NEMR_REGION_CAT_COLS ))
    (( NEMR_REGION_LEFT_COLS < NEMR_REGION_LEFT_MIN )) && NEMR_REGION_LEFT_COLS=$NEMR_REGION_LEFT_MIN
    _nemr_pane_inner=$(( NEMR_REGION_LEFT_COLS - 6 ))
    # The border, built once rather than once per row per frame.
    printf -v _NEMR_REGION_RULE '%*s' "$(( _nemr_pane_inner + 2 ))" ""
    _NEMR_REGION_RULE="${_NEMR_REGION_RULE// /-}"
}
_NEMR_REGION_RULE=""


_NEMR_REGION_TOP=""     # the row the block starts on, when the terminal said
_NEMR_EMOJI_ON=0        # icons go exactly when colour goes (decided at start)
_NEMR_REGION_CYAN=""
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
    _nemr_region_fit "$cols"
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
    if (( scroll <= 0 )); then
        _NEMR_REGION_TOP="$here"                # already there; this is the row
        return 0
    fi
    (( scroll > rows )) && scroll=$rows
    printf '\033[%d;1H' "$rows"                # to the bottom row
    local i
    for (( i = 0; i < scroll; i++ )); do printf '\r\n'; done
    printf '\033[2;1H'                         # under the command line
    # THE ROW THE BLOCK STARTS ON. Only the resize handler uses it, and only
    # because relative movement cannot survive a reflow: see the comment there.
    _NEMR_REGION_TOP=2
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

# Sets _NEMR_BAR_TEXT rather than printing it: a command substitution is a
# process, and this one ran every frame for a string of twenty characters.
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
    printf -v _NEMR_BAR_TEXT '[%s]  %d of %d' "$out" "$done" "$total"
}
_NEMR_BAR_TEXT=""


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
_nemr_pane_inner=$((NEMR_REGION_LEFT_COLS - 6))   # a default; _nemr_region_fit sets it

# The pane's lines for THIS frame, cleaned ONCE. They used to be recomputed for
# every row, which read and cleaned the whole tail five times a frame — around
# forty processes per frame where six will do. On a loaded machine that was the
# difference between an animation and a slideshow: measured here, a six-second
# step drew two frames.
_nemr_region_pane_rows=()
_nemr_region_pane_refresh() {
    _nemr_region_pane_rows=()
    [[ -n "$_NEMR_REGION_TAIL" && -s "$_NEMR_REGION_TAIL" ]] || return 0
    local line
    while IFS= read -r line; do
        # Cut to the pane's inner width. The tail above has already had
        # every byte above ASCII removed, which is what makes this safe: a
        # step's own output is arbitrary (cargo prints arrows, apt prints
        # accented names), and one wide glyph in here would move the pane's
        # right-hand border and with it the cat. Removed, never sliced.
        _nemr_region_pane_rows+=("${line:0:$_nemr_pane_inner}")
    done < <(grep -v '^[[:space:]]*$' "$_NEMR_REGION_TAIL" 2>/dev/null \
             | tail -n "$NEMR_REGION_PANE_ROWS" \
             | sed -e 's/\x1b\[[0-9;?]*[a-zA-Z]//g' -e 's/\r/ /g' -e 's/\t/ /g' \
             | LC_ALL=C tr -d '\000-\011\013-\037\177-\377')
    return 0
}

# The left column. ONE line is the step running now, and it is REPLACED, not
# appended to: at any moment the screen carries the current step and nothing
# else. No list, no file names, no digests.
# Sets _NEMR_LEFT_TEXT rather than printing it: read through a command
# substitution this was a process per ROW per frame — fifteen of them — for a
# string the shell had already built.
_nemr_region_left() {   # <row> <phrase> <done> <total> <pane?> <icon>
    local row="$1" phrase="$2" done="$3" total="$4" pane="$5" icon="${6:-}"
    local plain="" pre="" post="" n cells=0
    case "$row" in
        0)  _nemr_region_iconed "$NEMR_REGION_TITLE_ICON" "Installing nemr..."
            plain="$_icon_plain"; cells=$_icon_cells
            pre="$_NEMR_REGION_BRIGHT$_NEMR_REGION_CYAN"; post="$_NEMR_REGION_RESET" ;;
        1)  _nemr_region_iconed "$icon" "$phrase"
            plain="$_icon_plain"; cells=$_icon_cells
            pre="$_NEMR_REGION_GREEN"; post="$_NEMR_REGION_RESET" ;;
        3)  _nemr_region_bar "$done" "$total"
            plain="  $_NEMR_BAR_TEXT"; cells=${#plain} ;;
        *)
            if (( pane )) && (( row >= NEMR_REGION_PANE_TOP )) \
               && (( row <= NEMR_REGION_PANE_TOP + NEMR_REGION_PANE_ROWS + 1 )); then
                n=$(( row - NEMR_REGION_PANE_TOP ))
                if (( n == 0 || n == NEMR_REGION_PANE_ROWS + 1 )); then
                    plain="  +${_NEMR_REGION_RULE}+"
                else
                    printf -v plain '  | %-*s |' "$_nemr_pane_inner" \
                        "${_nemr_region_pane_rows[$((n - 1))]:-}"
                fi
                cells=${#plain}
                pre="$_NEMR_REGION_DIM"; post="$_NEMR_REGION_RESET"
            fi ;;
    esac
    # Padded from the COUNT, not from the length. The two differ by exactly one
    # column per icon, which is the whole reason this is written down.
    local pad=$(( NEMR_REGION_LEFT_COLS - cells ))
    (( pad < 0 )) && pad=0
    printf -v _NEMR_LEFT_TEXT '%s%s%s%*s' "$pre" "$plain" "$post" "$pad" ""
}
_NEMR_LEFT_TEXT=""

# The renderer.
_nemr_region_loop() {
    local frames=() frame="" line
    while IFS= read -r line; do
        if [[ "$line" == "%%" ]]; then frames+=("$frame"); frame=""
        else frame+="$line"$'\n'; fi
    done < <(_nemr_cat_frames)
    [[ -n "$frame" ]] && frames+=("$frame")

    # Split every frame into rows ONCE. The four frames do not change for the
    # life of the run, and re-splitting them per frame was another process per
    # frame for an answer that was already known.
    local -a fr_rows=()
    local fr_h=0 f cnt
    for (( f = 0; f < ${#frames[@]}; f++ )); do
        cnt=0
        while IFS= read -r line; do fr_rows+=("$line"); cnt=$((cnt + 1)); done \
            < <(printf '%s' "${frames[$f]}")
        (( f == 0 )) && fr_h=$cnt
    done

    local rows cols size height written=0 i=0 n=${#frames[@]}
    # Measured ONCE, here: the block keeps this width for the whole run.
    size="$(nemr_term_size)" || size="24 80"
    rows="${size% *}"; cols="${size#* }"
    _nemr_region_fit "$cols"

    # shellcheck disable=SC2064
    trap 'if (( written > 0 )); then printf "\033[%dA\r\033[J" "$written"; else printf "\r\033[J"; fi; exit 0' TERM INT
    # A RESIZE (SPEC 1.150). Noted here and acted on at the top of the next
    # frame, where the block can be erased from its anchor and redrawn at the
    # new size. It is only a flag because a trap that drew would draw into the
    # middle of a half-written frame.
    local winch=0
    trap 'winch=1' WINCH

    exec 9<>"$_NEMR_REGION_STOP"
    _nemr_region_take_screen "$rows"

    local reserved=0 gave_up=""
    while :; do
        # THE RESIZE, handled before anything is drawn.
        #
        # Why the block's anchor ROW and not cursor arithmetic: when a terminal
        # narrows, the rows already on screen are rewrapped, so each of our
        # full-width rows becomes two — and the `\033[<h-1>A` that ends every
        # frame then lands half a block too low. That is the whole defect: the
        # block walks down the screen, a copy per frame, twenty copies by the
        # time the drag ends. No relative movement can be trusted across a
        # reflow, and no arithmetic can be either, because terminals do not
        # agree on whether they reflow at all. An absolute row does not care.
        if (( winch )); then
            winch=0
            # ERASE FROM THE BLOCK'S OWN FIRST ROW.
            #
            # Not from the cursor. Between frames the cursor is *meant* to be
            # on the block's first line, but a resize that lands while a frame
            # is being written leaves it lower: the rows already on screen get
            # rewrapped, so the `\033[<h-1>A` that ends the frame walks up
            # through rows that are now taller and stops short. Measured under
            # a drag: exactly the two widest rows had wrapped, the walk-back
            # stopped two rows low, and erasing from there left those two rows
            # — the title and the step line — above the redraw.
            #
            # The row the block starts on does not move: nothing above it is
            # ever redrawn, and the block is the bottom of the screen's
            # content. Where the terminal would not say what that row is, the
            # cursor is the only handle there is, and it is right whenever the
            # resize lands between frames — which is most of the time.
            #
            # Two ways to be wrong, so both are covered. If a frame was being
            # written when the resize landed, the walk-back at its end stopped
            # SHORT and the cursor is below the block's first row — then the
            # recorded row is right. If the terminal scrolled its top away to
            # make room for rewrapped lines, the block moved UP and the
            # recorded row is too low — then the cursor is right, because it
            # travelled with its own line. Whichever is higher on the screen is
            # the one that covers the whole block, so ask, and take it.
            local top="$_NEMR_REGION_TOP" here
            here="$(_nemr_cursor_row)" || here=""
            if [[ -n "$here" && "$here" =~ ^[0-9]+$ ]]; then
                if [[ -z "$top" ]] || (( here < top )); then top="$here"; fi
            fi
            if [[ -n "$top" ]]; then
                printf '\033[%d;1H\033[J' "$top"
            else
                printf '\r\033[J'
            fi
            written=0
            reserved=0
            size="$(nemr_term_size)" || size="24 80"
            rows="${size% *}"; cols="${size#* }"
            if (( cols < NEMR_REGION_MIN_COLS || rows < NEMR_REGION_MIN_ROWS )); then
                gave_up="the window is now ${cols}x${rows}; this screen needs ${NEMR_REGION_MIN_COLS}x${NEMR_REGION_MIN_ROWS}"
                break
            fi
            _nemr_region_fit "$cols"
        fi

        local phrase="" done=0 total=1 icon="" line2
        if [[ -r "$_NEMR_REGION_STATE" ]]; then
            IFS=$'\t' read -r phrase done total icon < "$_NEMR_REGION_STATE"
        fi
        [[ "$done" =~ ^[0-9]+$ ]] || done=0
        [[ "$total" =~ ^[0-9]+$ ]] || total=1

        local base=$(( (i % n) * fr_h ))
        height=$fr_h
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

        _nemr_region_pane_refresh

        local r left right frame_out="" gap
        printf -v gap '%*s' "$NEMR_REGION_GAP" ""
        for (( r = 0; r < height; r++ )); do
            _nemr_region_left "$r" "$phrase" "$done" "$total" "$pane" "$icon"
            left="$_NEMR_LEFT_TEXT"
            right=""
            (( r < fr_h )) && right="${fr_rows[$((base + r))]}"
            frame_out+=$'\033[2K'"${left}${gap}${right}"
            if (( r < height - 1 )); then
                frame_out+=$'\r\n'   # never a bare \n: a step can leave the tty raw
            fi
        done
        (( height > 1 )) && frame_out+=$'\033['"$((height - 1))"'A'
        frame_out+=$'\r'
        # One write. A resize that lands between two halves of a drawn block
        # reflows the half that is already there, and every relative move after
        # it is then wrong by however much that half grew.
        printf '%s' "$frame_out"
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

    # Gave up on a resize: the block is already erased (from its anchor), so
    # say why, once, and leave a flag the main shell reads before its next
    # step — from here on the run prints one plain line per step, which is
    # what it does on any terminal too small for a live screen.
    if [[ -n "$gave_up" ]]; then
        printf '  %s.\r\n' "$gave_up"
        printf '  The rest of this run prints plainly.\r\n'
        : >"${_NEMR_REGION_STATE}.off" 2>/dev/null || true
    fi
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
        _NEMR_REGION_CYAN=$'\033[36m'
    else
        _NEMR_REGION_GREEN=""; _NEMR_REGION_BRIGHT=""; _NEMR_REGION_DIM=""
        _NEMR_REGION_RED="";   _NEMR_REGION_AMBER="";  _NEMR_REGION_RESET=""
        _NEMR_REGION_CYAN=""
    fi
    # Decided ONCE, here, before the renderer is forked, so the two processes
    # cannot disagree about how wide a row is.
    if nemr_emoji_enabled; then _NEMR_EMOJI_ON=1; else _NEMR_EMOJI_ON=0; fi
    printf '\033[?25l'
    _NEMR_REGION_HID=1
    _nemr_region_loop &
    _NEMR_REGION_PID=$!
    exec 7<>"$_NEMR_REGION_STOP"
    return 0
}

# Did the renderer stop by itself? It does that when the window is resized to
# something it cannot draw in, or when it cannot know where it is drawn. The
# main shell asks before every step and, once it is told, prints plainly for
# the rest of the run rather than publishing to a file nobody is reading.
nemr_region_gave_up() {
    [[ -n "$_NEMR_REGION_STATE" && -e "${_NEMR_REGION_STATE}.off" ]] || return 1
    rm -f "${_NEMR_REGION_STATE}.off" 2>/dev/null || true
    if [[ -n "$_NEMR_REGION_PID" ]]; then
        wait "$_NEMR_REGION_PID" 2>/dev/null || true
        _NEMR_REGION_PID=""
    fi
    # The cursor is ours to give back whether or not the block is gone.
    if [[ -n "$_NEMR_REGION_HID" ]]; then
        printf '\033[?25h'
        _NEMR_REGION_HID=""
    fi
    return 0
}

# Publish what is happening now: the phrase, and how many steps are done out of
# how many. Written to a temp and renamed, so a frame never reads half of it.
nemr_region_publish() {   # <phrase> <done> <total> [<icon>]
    [[ -n "$_NEMR_REGION_STATE" ]] || return 0
    local tmp="${_NEMR_REGION_STATE}.new"
    printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "${4:-}" >"$tmp" \
        && mv -f "$tmp" "$_NEMR_REGION_STATE"
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
