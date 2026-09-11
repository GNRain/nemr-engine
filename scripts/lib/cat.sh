#!/usr/bin/env bash
#
# The install progress animation: Nemr, playing with a cable (D-14).
#
# Nemr is the cat this project is named for, and he spent his life doing
# exactly this. Four frames, 15 lines, 37 columns, plain ASCII, no dependency,
# no colour.
#
# CREDIT. The drawing follows an ASCII-art cat by **Samamine**, the reference
# the Product Owner supplied — same pose, same sparse dotted-outline idiom. It
# is redrawn rather than copied, but the likeness is deliberate and close, so
# the credit is theirs: ruled 2026-09-09, and carried in README.md under
# Credits. If the art here changes, that line changes with it (D-14).
#
# See the frames without installing anything:
#
#     ./scripts/lib/cat.sh --show     # the four frames, printed once each
#     ./scripts/lib/cat.sh --demo     # animated for 5s, then cleared
#
# THE RULES IT IS HELD TO (all four are tested — scripts/test_install.sh):
#
#   1. It never gates progress. The work runs in its own process; these frames
#      are drawn beside it. The install takes the same time whether or not a
#      frame is ever drawn — the same rule the browser UI holds for state.
#   2. No terminal, no animation. Not a tty, TERM=dumb, NO_COLOR set, or the
#      caller asked for quiet, and this draws nothing at all: a piped or CI log
#      is step lines and nothing else.
#   3. It cleans up after itself — cursor restored, its own lines erased — on
#      normal exit, on failure, and on Ctrl-C. A killed install must not leave a
#      hidden cursor or half a cat.
#   4. It stays within one screen: 4 frames, all the same height, ASCII only,
#      37 columns, so it renders the same in any 80-column terminal — and it
#      refuses to draw at all in a terminal too short to hold it, where the
#      cursor arithmetic would scroll and leave a trail of half-cats.

# Nemr, sitting, with a cable swinging past his paw.
#
# The drawing follows a reference the Product Owner sent (an ASCII-art cat
# signed "Samamine"): same pose, same sparse dotted-outline idiom, same
# character vocabulary — ears and the `)` inner ear, one `o` eye under its brow
# bar, the back sloping down to the rump, the tail curling forward, three
# `*-*` paw clusters on the floor. See the note above about crediting it.
#
# Frames are separated by a line of "%%" and are all the same height (the
# drawer reads the height from the art). The CAT IS BYTE-IDENTICAL in all four
# frames: only the cable's free end moves, and the near front paw, once per
# cycle. The cable swings as a pendulum — out to the left with its tip lifted
# (1), down through the bottom of the arc trailing left (2), out to the right
# where the paw comes off the floor to meet it (3), and back down through the
# bottom trailing right (4). Both extremes lift the tip by the same amount, so
# the cable never appears to stretch.
_nemr_cat_frames() {
    cat <<'FRAMES'
            _
            \`*-.
             )  _`-.
            .  : `. .
`-._        : _   '  \
    `-.\    ; o` _.   `*-._
        |   `-.-'          `-.
        |     ;       `       `.
        /     :.       .        \
       /      . \  .   :   .-'   .
      /       '  `+.;  ;  '      :
     |        :  '  |    ;       ;-.
     _)       ; '   : :`-:     _.`* ;
             /  .*' ; .*`- +'  `*'
            *-*   `*-*  `*-*'
%%
            _
            \`*-.
             )  _`-.
            .  : `. .
`-._        : _   '  \
    `-.\    ; o` _.   `*-._
        |   `-.-'          `-.
        |     ;       `       `.
       /      :.       .        \
      |       . \  .   :   .-'   .
      \       '  `+.;  ;  '      :
       \      :  '  |    ;       ;-.
        |     ; '   : :`-:     _.`* ;
        _)   /  .*' ; .*`- +'  `*'
            *-*   `*-*  `*-*'
%%
            _
            \`*-.
             )  _`-.
            .  : `. .
`-._        : _   '  \
    `-.\    ; o` _.   `*-._
        |   `-.-'          `-.
        |     ;       `       `.
        \     :.       .        \
         \    . \  .   :   .-'   .
          \   '  `+.;  ;  '      :
           |  :  '  |    ;       ;-.
           _) ; '   : :`-:     _.`* ;
            *-* .*' ; .*`- +'  `*'
                  `*-*  `*-*'
%%
            _
            \`*-.
             )  _`-.
            .  : `. .
`-._        : _   '  \
    `-.\    ; o` _.   `*-._
        |   `-.-'          `-.
        |     ;       `       `.
         \    :.       .        \
          |   . \  .   :   .-'   .
          /   '  `+.;  ;  '      :
         /    :  '  |    ;       ;-.
        |     ; '   : :`-:     _.`* ;
        _)   /  .*' ; .*`- +'  `*'
            *-*   `*-*  `*-*'
FRAMES
}

# The height is READ FROM THE FRAMES, never assumed: the drawer moves the cursor
# back by exactly as many lines as it wrote, and a hardcoded number that drifted
# from the art would leave a trail of half-cats up the terminal.
_nemr_cat_height() {
    local n=0 line
    while IFS= read -r line; do
        [[ "$line" == "%%" ]] && break
        n=$((n + 1))
    done < <(_nemr_cat_frames)
    printf '%s' "$n"
}

NEMR_CAT_DELAY="${NEMR_CAT_DELAY:-0.16}"

# The terminal's size: "<rows> <cols>", or nothing at all.
#
# The ioctl FIRST, because `tput` answers from the LINES and COLUMNS
# environment variables when they are set — that is how a 22-row terminal
# reported 24 and a block drew two rows more than it had. But a pty can exist
# with no size set at all (`stty size` says "0 0"), and terminfo is the better
# answer there, so an unset ioctl falls through to tput rather than to nothing.
nemr_term_size() {
    local size rows cols
    # The ioctl first, through the controlling terminal: it is the kernel's own
    # record of the window, it is correct the instant after a resize, and it is
    # right under `script(1)` too (there /dev/tty is script's pty).
    size="$(stty size 2>/dev/null </dev/tty)" || size="$(stty size 2>/dev/null)" || size=""
    rows="${size%% *}"; cols="${size##* }"
    if ! [[ "$rows" =~ ^[1-9][0-9]*$ && "$cols" =~ ^[1-9][0-9]*$ ]]; then
        # tput, but ONLY where it can see a terminal. ncurses looks at stdout
        # and then stderr; when NEITHER is a terminal it does not fail — it
        # answers from the static terminfo entry, which for xterm-256color is
        # 80x24. Measured on a 132x40 terminal with both redirected: it says
        # 80x24, a plausible wrong answer that would draw the screen at the
        # wrong width, or refuse a window that is plenty big. Better no answer.
        if [[ -t 1 || -t 2 ]]; then
            rows="$(tput lines 2>/dev/null || echo 0)"
            cols="$(tput cols 2>/dev/null || echo 0)"
        else
            rows=0; cols=0
        fi
    fi
    [[ "$rows" =~ ^[1-9][0-9]*$ && "$cols" =~ ^[1-9][0-9]*$ ]] || return 1
    printf '%s %s' "$rows" "$cols"
}

# May we draw? Every "no" is a deliberate one (rule 2). NEMR_CAT=0 is the
# caller's own off switch (--quiet passes it); NEMR_CAT=1 forces it on for the
# test that needs a pty to prove the drawing half.
# Every "no" names itself in _NEMR_SCREEN_WHY. The refusals used to be silent —
# eleven of them, in two files — so a report that the live screen did not appear
# could not be answered from here at all: the log was byte-for-byte identical
# whether the region drew or not (measured, 2026-09-10). The reason is computed
# either way; throwing it away was the whole defect.
_NEMR_SCREEN_WHY=""

nemr_cat_enabled() {
    [[ "${NEMR_CAT:-}" == "0" ]] && { _NEMR_SCREEN_WHY="quiet (NEMR_CAT=0)"; return 1; }
    [[ "${NEMR_CAT:-}" == "1" ]] && return 0
    [[ -t 1 ]] || { _NEMR_SCREEN_WHY="not-a-tty (output is redirected or piped)"; return 1; }
    [[ -n "${NO_COLOR:-}" ]] && { _NEMR_SCREEN_WHY="NO_COLOR is set"; return 1; }
    case "${TERM:-dumb}" in dumb|"") _NEMR_SCREEN_WHY="TERM=${TERM:-unset}"; return 1 ;; esac
    # Too short a terminal and the block scrolls: the cursor-up would then walk
    # over the step lines instead of its own, leaving a trail. Rather than draw
    # something broken, draw nothing.
    local rows size
    size="$(nemr_term_size)" || { _NEMR_SCREEN_WHY="the terminal would not report its size"; return 1; }
    rows="${size%% *}"
    (( rows >= $(_nemr_cat_height) + 2 )) \
        || { _NEMR_SCREEN_WHY="$rows rows, the animation needs $(( $(_nemr_cat_height) + 2 ))"; return 1; }
    return 0
}

_NEMR_CAT_PID=""
_NEMR_CAT_HID=""
_NEMR_CAT_STOP=""

# THE DRAWER OWNS ITS OWN LINES, AND ITS OWN EXIT.
#
# The first version let the parent kill the drawer and then erase from wherever
# the cursor happened to be. A kill lands mid-frame roughly half the time, so
# the cursor sat part-way down the block and `\033[J` erased from there —
# leaving every line above it on the screen for good. Measured before the fix:
# 9 of 20 stops left a cat behind (2026-09-10). Nothing counted escape
# sequences wrongly; the assertion simply was not about the screen.
#
# So: the drawer stops only at a FRAME BOUNDARY, where the cursor is provably
# back at the top of its block, and it erases its own lines on the way out. If
# it is signalled instead — Ctrl-C reaches the whole process group — its trap
# knows how many lines of the current frame it has written and erases exactly
# those. Either way nothing else has to guess where the cursor is.
_nemr_cat_loop() {
    local frames=() frame="" line written=0 height
    while IFS= read -r line; do
        if [[ "$line" == "%%" ]]; then frames+=("$frame"); frame=""
        else frame+="$line"$'\n'; fi
    done < <(_nemr_cat_frames)
    [[ -n "$frame" ]] && frames+=("$frame")
    height="$(printf '%s' "${frames[0]}" | grep -c '')"

    # Signalled mid-frame: erase what this frame has written, and only that.
    # shellcheck disable=SC2064
    trap 'if (( written > 0 )); then printf "\033[%dA\r\033[J" "$written"; else printf "\r\033[J"; fi; exit 0' TERM INT

    # The stop channel. `read -t` on it is the frame delay AND the stop check in
    # one: a byte arrives and the read returns at once, so stopping costs
    # nothing — the animation must never make the install wait for it.
    exec 9<>"$_NEMR_CAT_STOP"

    # NEVER a newline on the last row of the block. At the foot of the screen
    # that newline scrolls the terminal, and the cursor-up that follows is
    # RELATIVE — so the block's idea of its own top drifts a row per frame and
    # the final erase leaves stranded rows above it. Print height-1 lines with
    # newlines, the last without, then come back up height-1.
    # RESERVE THE BLOCK FIRST. A 15-row block started within 15 rows of the
    # bottom scrolls while its first frame is being written, and the rows
    # written before the scroll end up ABOVE the block — outside what the
    # drawer thinks it owns, so its erase never reaches them. Measured at the
    # foot of a 40-row screen (2026-09-10): four stranded rows, every run.
    # Emitting the blank lines up front scrolls once, harmlessly, and from then
    # on every frame lands inside a block that is fully on screen.
    local k
    for (( k = 1; k < height; k++ )); do printf '\r\n'; done
    (( height > 1 )) && printf '\033[%dA' "$((height - 1))"
    printf '\r'

    local i=0 n=${#frames[@]} idx
    while :; do
        written=0
        idx=0
        # Process substitution, not a here-string: `<<<` appends a newline to a
        # frame that already ends with one, so every frame wrote one line more
        # than `height` and the block crept down the screen a line per frame.
        while IFS= read -r line; do
            printf '\033[2K%s' "$line"
            idx=$((idx + 1))
            if (( idx < height )); then
                # \r\n, never a bare \n: a step can leave the terminal in raw
                # mode — the smoke test's `nemr attach` does — and there a bare
                # newline does not return the carriage, so every row of the
                # block draws one column further right and the erase misses it.
                # Drawing must not depend on the line discipline.
                printf '\r\n'
                written=$((written + 1))
            fi
            # TEST-ONLY seam (unset in every real run): slows the frame so a
            # stop lands mid-write on purpose. Without it the mid-write window
            # is a couple of milliseconds wide and the assertion that the screen
            # is left clean only catches a regression two times in twelve.
            [[ -n "${NEMR_TEST_CAT_LINE_DELAY:-}" ]] && sleep "$NEMR_TEST_CAT_LINE_DELAY"
        done < <(printf '%s' "${frames[$((i % n))]}")
        (( height > 1 )) && printf '\033[%dA' "$((height - 1))"
        printf '\r'
        written=0                      # cursor is back at the top of the block
        i=$((i + 1))
        if read -r -t "$NEMR_CAT_DELAY" -u 9 _; then break; fi
    done
    printf '\r\033[J'                  # the block was ours; leave nothing
}

# Start drawing. Returns immediately.
nemr_cat_start() {
    nemr_cat_enabled || return 0
    [[ -n "$_NEMR_CAT_PID" ]] && return 0
    _NEMR_CAT_STOP="$(mktemp -u "${TMPDIR:-/tmp}/nemr-cat.XXXXXX")"
    mkfifo -m 0600 "$_NEMR_CAT_STOP" 2>/dev/null || { _NEMR_CAT_STOP=""; return 0; }
    printf '\033[?25l'          # hide the cursor
    _NEMR_CAT_HID=1
    _nemr_cat_loop &
    _NEMR_CAT_PID=$!
    exec 8<>"$_NEMR_CAT_STOP"   # <> so opening never waits for the other end
}

# Stop drawing and leave the terminal as it was found. Safe to call when
# nothing is running, and safe to call twice — it is the EXIT/INT/TERM handler
# as well as the end of a step.
nemr_cat_stop() {
    if [[ -n "$_NEMR_CAT_PID" ]]; then
        # Ask, do not kill: the drawer finishes the frame it is on, erases its
        # own lines and exits. The read it is blocked on returns the instant
        # this byte lands, so the ask costs no measurable time.
        # A LINE, not a byte: `read` returns on a newline, so a bare byte would
        # sit in the pipe while the drawer timed out around it forever.
        printf 's\n' >&8 2>/dev/null || true
        wait "$_NEMR_CAT_PID" 2>/dev/null || true
        # Only if it somehow outlived the ask. Its trap still cleans up.
        kill -0 "$_NEMR_CAT_PID" 2>/dev/null && {
            kill "$_NEMR_CAT_PID" 2>/dev/null
            wait "$_NEMR_CAT_PID" 2>/dev/null
        }
        exec 8>&- 2>/dev/null || true
        rm -f "$_NEMR_CAT_STOP"
        _NEMR_CAT_PID=""
        _NEMR_CAT_STOP=""
    fi
    if [[ -n "$_NEMR_CAT_HID" ]]; then
        printf '\033[?25h'
        _NEMR_CAT_HID=""
    fi
    return 0
}

# ---------------------------------------------------------------------------
# Run directly to look at him.
# ---------------------------------------------------------------------------
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    case "${1:---show}" in
        --show)
            n=1
            printf 'frame 1\n'
            # No blank line between frames: a reader counting lines here (the
            # acceptance does) must see exactly the art, so the header is the
            # only separator and a blank line in the art counts as art.
            while IFS= read -r line; do
                if [[ "$line" == "%%" ]]; then
                    n=$((n + 1)); printf 'frame %d\n' "$n"; continue
                fi
                printf '%s\n' "$line"
            done < <(_nemr_cat_frames)
            ;;
        --demo)
            trap 'nemr_cat_stop; exit 130' INT TERM
            trap 'nemr_cat_stop' EXIT
            printf 'a long step is running...\n'
            NEMR_CAT="${NEMR_CAT:-}" nemr_cat_start
            sleep "${2:-5}"
            nemr_cat_stop
            printf 'done.\n'
            ;;
        *) echo "usage: $0 [--show|--demo [seconds]]" >&2; exit 2 ;;
    esac
fi
