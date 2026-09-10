#!/usr/bin/env python3
"""TEST-ONLY. Is the cat in exactly the column it should be, on every frame?

    ./scripts/lib/region_columns.py <transcript> <cols>

The install region is a fixed two-column block: a left column of
(cols - 2 - 37) columns, a two-column gap, then the cat's 37 columns against
the right edge of the terminal. The left column's top two rows carry an EMOJI,
which is one character to bash and two columns to the terminal. If any width
in there were measured (\\${#...}) rather than counted, those two rows — and
only those two — would be one column wide, and the cat would step sideways on
them. That is a tear too small to see and fatal to the block.

Relative checks cannot catch it: the icon rows are shifted on EVERY frame, so
comparing frames to each other finds nothing. So this is absolute. It reads the
cat's own frames from `scripts/lib/cat.sh --show`, which is where the art and
therefore its per-line indent actually lives, and for every complete frame in
the capture it requires each drawn row to put that art at EXACTLY

    (cols - 2 - 37) + 2 + (that art line's own leading spaces)

It prints `frames`, the distinct row widths, and either `columns exact` or one
line per row that is in the wrong column. Exit 1 if anything is out of place,
or if the capture held no frames to look at.
"""
import re
import subprocess
import sys


def cat_frames():
    """The art as the product ships it: a list of frames, each a list of lines."""
    out = subprocess.run(["./scripts/lib/cat.sh", "--show"],
                         capture_output=True, text=True).stdout
    frames, cur = [], None
    for line in out.split("\n"):
        if line.startswith("frame "):
            if cur is not None:
                frames.append(cur)
            cur = []
        elif cur is not None:
            cur.append(line)
    if cur:
        frames.append(cur)
    return [f for f in frames if f]


def render(raw, cols, i):
    return subprocess.run(
        ["python3", "scripts/lib/render_pty.py", raw,
         "--cols", str(cols), "--rows", "30", "--frame", str(i)],
        capture_output=True, text=True).stdout.split("\n")


def main():
    raw, cols = sys.argv[1], int(sys.argv[2])
    left = cols - 2 - 37
    art_frames = cat_frames()
    data = open(raw, "rb").read()
    n = len(re.findall(rb"\x1b\[[0-9]+A\r", data))
    if n < 2:
        print("frames 0")
        print("columns NO-FRAMES")
        return 1

    widths, bad, matched = set(), [], 0
    for i in range(2, n + 1):
        rows = render(raw, cols, i)
        for r in rows:
            widths.add(len(r))
        # The drawn rows to the right of the left column, in order.
        tails = [r[left:] for r in rows]
        drawn = [(k, t) for k, t in enumerate(tails) if t.strip()]
        if not drawn:
            continue
        # Which of the four frames is on screen? The one whose stripped lines
        # match, in order, what was drawn.
        want = None
        for art in art_frames:
            texts = [a.strip() for a in art if a.strip()]
            if [t.strip() for _, t in drawn] == texts:
                want = art
                break
        if want is None:
            continue          # a frame caught mid-draw; the next one will do
        matched += 1
        art_lines = [a for a in want if a.strip()]
        for (k, tail), art in zip(drawn, art_lines):
            expect = left + 2 + (len(art) - len(art.lstrip()))
            got = left + len(tail) - len(tail.lstrip())
            if got != expect:
                bad.append("row %d: %r is in column %d, should be %d"
                           % (k, art.strip()[:24], got, expect))
    print("frames %d" % n)
    print("widths %s" % " ".join(str(w) for w in sorted(widths)))
    if matched == 0:
        print("columns NO-MATCHED-FRAMES")
        return 1
    if bad:
        for b in bad[:8]:
            print("columns WRONG %s" % b)
        return 1
    print("columns exact (%d frames checked against the art)" % matched)
    return 0


if __name__ == "__main__":
    sys.exit(main())
