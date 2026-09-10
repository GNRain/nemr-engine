#!/usr/bin/env python3
"""Render a captured pty transcript to the screen it would have produced.

TEST-ONLY. Nothing the product ships uses this; it exists so an assertion can
be made about THE SCREEN rather than about the escape sequences that produced
it. Counting `\\033[J`s proves a clear was issued, not that nothing was left
behind — and a managed region makes a stray frame harder to see, not easier.

    ./scripts/lib/render_pty.py <transcript> [--rows N] [--cols N] [--at BYTES]
                                [--frame N]

Prints the final screen, one line per row, trailing blanks stripped. With
--at, prints the screen as it stood after the first N bytes, which is how a
mid-run state is inspected. --at slices wherever the byte lands, which is
usually halfway through a frame and shows half-drawn rows that were never on
anyone's screen; --frame N slices at the end of the Nth COMPLETE frame instead
(this project's renderer ends every frame by moving the cursor back up and to
column 1), which is the honest way to read a mid-run layout.

It interprets only what this project's own drawing uses: CR, LF, cursor
up/down/forward/back, absolute positioning, erase-in-line and erase-in-display.
Anything else (colour, cursor visibility) is consumed and ignored, because it
cannot move the cursor or change a glyph.
"""
import re
import sys
import unicodedata

CSI = re.compile(r"\x1b\[([0-9;?]*)([A-Za-z])")


class Screen:
    def __init__(self, rows, cols):
        self.rows, self.cols = rows, cols
        self.buf = [[" "] * cols for _ in range(rows)]
        self.r = self.c = 0

    def _scroll(self):
        self.buf.pop(0)
        self.buf.append([" "] * self.cols)
        self.r = self.rows - 1

    def put(self, ch):
        # A wide character (East Asian Width W or F — every emoji this project
        # allows in the region is W) advances the cursor by TWO columns and
        # occupies two cells. Counting it as one would show the layout as
        # correct when a real terminal tears it, which is the one thing this
        # renderer exists to prevent.
        w = 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1
        if self.c + w > self.cols:       # wrap, as a terminal does
            self.c = 0
            self.r += 1
        if self.r >= self.rows:
            self._scroll()
        self.buf[self.r][self.c] = ch
        if w == 2:
            # The second cell is held by the same glyph. It is kept as a space
            # so that len(line) is the number of COLUMNS the line occupies —
            # which is what every assertion here measures. (A write that lands
            # on this cell alone would leave half a glyph; the region redraws
            # whole rows, so that cannot arise here.)
            self.buf[self.r][self.c + 1] = " "
        self.c += w

    def feed(self, data):
        i = 0
        while i < len(data):
            ch = data[i]
            if ch == "\x1b":
                m = CSI.match(data, i)
                if m:
                    self.csi(m.group(1), m.group(2))
                    i = m.end()
                    continue
                i += 1                    # a lone ESC or a sequence we do not model
                continue
            if ch == "\r":
                self.c = 0
            elif ch == "\n":
                self.r += 1
                if self.r >= self.rows:
                    self._scroll()
            elif ch == "\b":
                self.c = max(0, self.c - 1)
            elif ch == "\t":
                self.c = min(self.cols - 1, (self.c // 8 + 1) * 8)
            elif ch >= " ":
                self.put(ch)
            i += 1

    def csi(self, params, final):
        nums = [int(p) for p in params.split(";") if p.isdigit()]
        n = nums[0] if nums else 0
        if final == "A":
            self.r = max(0, self.r - max(1, n))
        elif final == "B":
            self.r = min(self.rows - 1, self.r + max(1, n))
        elif final == "C":
            self.c = min(self.cols - 1, self.c + max(1, n))
        elif final == "D":
            self.c = max(0, self.c - max(1, n))
        elif final in "Hf":
            self.r = min(self.rows - 1, max(0, (nums[0] if nums else 1) - 1))
            self.c = min(self.cols - 1, max(0, (nums[1] if len(nums) > 1 else 1) - 1))
        elif final == "K":                # erase in line
            if n == 0:
                for c in range(self.c, self.cols):
                    self.buf[self.r][c] = " "
            elif n == 1:
                for c in range(0, self.c + 1):
                    self.buf[self.r][c] = " "
            else:
                self.buf[self.r] = [" "] * self.cols
        elif final == "J":                # erase in display
            if n == 0:
                for c in range(self.c, self.cols):
                    self.buf[self.r][c] = " "
                for r in range(self.r + 1, self.rows):
                    self.buf[r] = [" "] * self.cols
            elif n == 1:
                for r in range(0, self.r):
                    self.buf[r] = [" "] * self.cols
                for c in range(0, self.c + 1):
                    self.buf[self.r][c] = " "
            else:
                self.buf = [[" "] * self.cols for _ in range(self.rows)]

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.buf)


def main():
    args = sys.argv[1:]
    path = args[0]
    rows, cols, at, frame = 24, 80, None, None
    for i, a in enumerate(args):
        if a == "--rows":
            rows = int(args[i + 1])
        elif a == "--cols":
            cols = int(args[i + 1])
        elif a == "--at":
            at = int(args[i + 1])
        elif a == "--frame":
            frame = int(args[i + 1])
    data = open(path, "rb").read()
    if frame is not None:
        ends = [m.end() for m in re.finditer(rb"\x1b\[[0-9]+A\r", data)]
        if not ends:
            sys.exit("no complete frame in %s" % path)
        data = data[: ends[min(frame, len(ends)) - 1]]
    if at is not None:
        data = data[:at]
    s = Screen(rows, cols)
    s.feed(data.decode("utf-8", "replace"))
    print(s.text())


if __name__ == "__main__":
    main()
