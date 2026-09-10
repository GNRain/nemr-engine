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

# May we draw? Every "no" is a deliberate one (rule 2). NEMR_CAT=0 is the
# caller's own off switch (--quiet passes it); NEMR_CAT=1 forces it on for the
# test that needs a pty to prove the drawing half.
nemr_cat_enabled() {
    [[ "${NEMR_CAT:-}" == "0" ]] && return 1
    [[ "${NEMR_CAT:-}" == "1" ]] && return 0
    [[ -t 1 ]] || return 1
    [[ -n "${NO_COLOR:-}" ]] && return 1
    case "${TERM:-dumb}" in dumb|"") return 1 ;; esac
    # Too short a terminal and the block scrolls: the cursor-up would then walk
    # over the step lines instead of its own, leaving a trail. Rather than draw
    # something broken, draw nothing.
    local rows
    rows="$(tput lines 2>/dev/null || echo 0)"
    [[ "$rows" =~ ^[0-9]+$ ]] || rows=0
    (( rows >= $(_nemr_cat_height) + 2 )) || return 1
    return 0
}

_NEMR_CAT_PID=""
_NEMR_CAT_HID=""

# Draw until killed. A separate process, so nothing here can hold up the work.
_nemr_cat_loop() {
    local frames=() frame="" line
    while IFS= read -r line; do
        if [[ "$line" == "%%" ]]; then frames+=("$frame"); frame=""
        else frame+="$line"$'\n'; fi
    done < <(_nemr_cat_frames)
    [[ -n "$frame" ]] && frames+=("$frame")

    local i=0 n=${#frames[@]} height
    height="$(printf '%s' "${frames[0]}" | grep -c '')"
    while :; do
        # Erase each line before writing it, so a shorter frame cannot leave
        # the tail of a longer one behind.
        printf '%s' "${frames[$((i % n))]}" | while IFS= read -r line; do
            printf '\033[2K%s\n' "$line"
        done
        printf '\033[%dA' "$height"
        i=$((i + 1))
        sleep "$NEMR_CAT_DELAY"
    done
}

# Start drawing. Returns immediately.
nemr_cat_start() {
    nemr_cat_enabled || return 0
    [[ -n "$_NEMR_CAT_PID" ]] && return 0
    printf '\033[?25l'          # hide the cursor
    _NEMR_CAT_HID=1
    _nemr_cat_loop &
    _NEMR_CAT_PID=$!
}

# Stop drawing and leave the terminal as it was found: the cat's lines erased,
# the cursor visible. Safe to call when nothing is running, and safe to call
# twice — it is the EXIT/INT/TERM handler as well as the end of a step.
nemr_cat_stop() {
    if [[ -n "$_NEMR_CAT_PID" ]]; then
        kill "$_NEMR_CAT_PID" 2>/dev/null || true
        wait "$_NEMR_CAT_PID" 2>/dev/null || true
        _NEMR_CAT_PID=""
        # The cursor is at the top of the block; erase from here to the end of
        # the screen. Nothing is drawn below the cat, so this takes exactly its
        # lines and no others.
        printf '\r\033[J'
    fi
    # Only if we hid it. The flag outlives the drawer, so a drawer killed hard
    # still leaves the terminal restored here; a run that never drew anything
    # emits nothing at all, which is what keeps a captured log clean.
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
