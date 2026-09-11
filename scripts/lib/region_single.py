#!/usr/bin/env python3
"""TEST-ONLY. Is there exactly ONE region on screen, at every frame?

    ./scripts/lib/region_single.py <transcript> <cols> <rows> [--resizes FILE]
                                   [--reflow wrap|truncate]

The invariant the Product Owner stated: *the region occupies exactly one block
on screen at all times, and anything it drew before is erased before anything
is drawn again.* Resizing the terminal mid-run used to break it — the block
redrew at the new width without erasing the old one and walked down the screen,
a copy per frame.

So this replays the capture, stops at every complete frame, renders the screen
as it stood, and counts the region's landmarks on it: the title line, the
progress bar, and the pane's border. More than one of any of them is more than
one region. It reports the worst frame it saw.

Terminals disagree about what happens to lines that no longer fit, so this is
run under both models (see render_pty.py --reflow); a claim that holds under
only one of them is not a claim about terminals.
"""
import importlib.util
import os
import re
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "render_pty", os.path.join(_HERE, "render_pty.py"))
render_pty = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(render_pty)

TITLE = re.compile(r"Installing nemr\.\.\.")
BAR = re.compile(r"\[[#-]{10,}\]\s+\d+ of \d+")
PANE_TOP = re.compile(r"^\s+\+-{6,}\+")


def screen_at(data, events, cols, rows, cut, reflow):
    """The screen as it stood after the first `cut` bytes, resizes applied."""
    s = render_pty.Screen(rows, cols)
    pos = 0
    for off, ncols, nrows in events:
        if off >= cut:
            break
        s.feed(data[pos:off].decode("utf-8", "replace"))
        s.resize(ncols, nrows, reflow)
        pos = off
    s.feed(data[pos:cut].decode("utf-8", "replace"))
    return s.text()


def main():
    args = sys.argv[1:]
    path, cols, rows = args[0], int(args[1]), int(args[2])
    resizes = None
    reflow = "wrap"
    for i, a in enumerate(args):
        if a == "--resizes":
            resizes = args[i + 1]
        elif a == "--reflow":
            reflow = args[i + 1]

    data = open(path, "rb").read()
    events = []
    if resizes:
        for line in open(resizes):
            parts = line.split()
            if len(parts) == 3:
                events.append((int(parts[0]), int(parts[1]), int(parts[2])))
        events.sort()

    ends = [m.end() for m in re.finditer(rb"\x1b\[[0-9]+A\r", data)]
    if len(ends) < 2:
        print("frames %d" % len(ends))
        print("worst NO-FRAMES")
        return 1

    worst = (0, -1, "")
    for n, cut in enumerate(ends[1:], start=2):
        lines = screen_at(data, events, cols, rows, cut, reflow).split("\n")
        titles = sum(1 for l in lines if TITLE.search(l))
        bars = sum(1 for l in lines if BAR.search(l))
        tops = sum(1 for l in lines if PANE_TOP.match(l))
        # A frame caught before the block is first drawn has none of these, and
        # none is not two: only an EXCESS is a second region.
        excess = max(titles, bars, max(0, tops - 2))
        if excess > worst[0]:
            worst = (excess, n, "titles=%d bars=%d pane-borders=%d" % (titles, bars, tops))
    # Did it RE-FIT? After a resize the block should be as wide as the window
    # again; a block that merely froze is still at the old width. Compared
    # against the width actually in force at the last complete frame, not
    # against a number written down here — which resize is the last one to
    # land while the screen is still up depends on how fast the machine is.
    last = screen_at(data, events, cols, rows, ends[-1], reflow).split("\n")
    widest = max((len(l) for l in last), default=0)
    in_force = cols
    for off, ncols, _nrows in events:
        if off < ends[-1]:
            in_force = ncols
    print("widest %d expected %d" % (widest, in_force))
    print("fitted %s" % ("yes" if widest == in_force else "no"))
    print("frames %d" % len(ends))
    if worst[0] > 1:
        print("worst frame %d: %s" % (worst[1], worst[2]))
        return 1
    print("worst 1 region on screen, across %d frames" % len(ends))
    return 0


if __name__ == "__main__":
    sys.exit(main())
