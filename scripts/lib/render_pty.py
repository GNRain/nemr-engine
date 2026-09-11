#!/usr/bin/env python3
"""Render a captured pty transcript to the screen it would have produced.

TEST-ONLY. Nothing the product ships uses this; it exists so an assertion can
be made about THE SCREEN rather than about the escape sequences that produced
it. Counting `\\033[J`s proves a clear was issued, not that nothing was left
behind — and a managed region makes a stray frame harder to see, not easier.

    ./scripts/lib/render_pty.py <transcript> [--rows N] [--cols N] [--at BYTES]
                                [--frame N] [--resizes FILE] [--reflow wrap|truncate]

Prints the final screen, one line per row, trailing blanks stripped. With
--at, prints the screen as it stood after the first N bytes, which is how a
mid-run state is inspected. --at slices wherever the byte lands, which is
usually halfway through a frame and shows half-drawn rows that were never on
anyone's screen; --frame N slices at the end of the Nth COMPLETE frame instead
(this project's renderer ends every frame by moving the cursor back up and to
column 1), which is the honest way to read a mid-run layout.

With --resizes (the sidecar `resize_pty.py` writes) the screen is resized
mid-replay at the byte offsets where the real pty was resized. Terminals do not
agree on what happens to already-drawn lines that no longer fit, so both
answers are implemented and a claim worth making is one that holds under both:

    --reflow wrap         lines longer than the new width re-wrap onto more
                          rows, pushing what is below them into the blank space
                          at the foot of the screen; the cursor travels with
                          its own line (VTE, xterm)
    --reflow wrap-scroll  the same, but the top of the screen scrolls away to
                          make room instead — the harsher reading, in which a
                          block can end up ABOVE the row it was drawn at
    --reflow truncate     lines are clipped in place and the cursor keeps its row

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

    def resize(self, cols, rows, model):
        """What a terminal does to what is already on screen when it changes size.

        The cursor is the delicate part. Under `wrap` it belongs to its own
        line: if lines above it grow, it moves down with them, which is exactly
        why a block redrawn with relative cursor movement walks down the screen
        after a resize. Under `truncate` it simply stays where it is.
        """
        if model == "truncate":
            buf = []
            for row in self.buf:
                r = row[:cols] + [" "] * max(0, cols - len(row))
                buf.append(r)
            if len(buf) > rows:
                buf = buf[len(buf) - rows:]
            while len(buf) < rows:
                buf.append([" "] * cols)
            self.buf = buf
            self.rows, self.cols = rows, cols
            self.r = min(self.r, rows - 1)
            self.c = min(self.c, cols - 1)
            return

        # wrap: every row is a logical line, re-wrapped to the new width.
        lines = []
        cursor_line, cursor_col = self.r, self.c
        for row in self.buf:
            text = "".join(row).rstrip()
            lines.append(text)
        out, new_r, new_c = [], 0, 0
        for i, text in enumerate(lines):
            start = len(out)
            if not text:
                pieces = [""]
            else:
                pieces = [text[j:j + cols] for j in range(0, len(text), cols)]
            for piece in pieces:
                out.append(list(piece) + [" "] * (cols - len(piece)))
            if i == cursor_line:
                new_r = start + (cursor_col // cols)
                new_c = cursor_col % cols
        # Growing lines push what is below them DOWN, into the blank space at
        # the foot of the screen; only when that space runs out does the top
        # scroll away. Dropping from the top while the screen is half empty
        # would move a block that a terminal leaves exactly where it is — and
        # would make this renderer disagree with the thing it is modelling.
        if model != "wrap-scroll":
            while len(out) > rows and not "".join(out[-1]).strip() and len(out) - 1 >= new_r + 1:
                out.pop()
        if len(out) > rows:
            dropped = len(out) - rows
            out = out[dropped:]
            new_r -= dropped
        while len(out) < rows:
            out.append([" "] * cols)
        self.buf = out
        self.rows, self.cols = rows, cols
        self.r = max(0, min(new_r, rows - 1))
        self.c = max(0, min(new_c, cols - 1))

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.buf)


def main():
    args = sys.argv[1:]
    path = args[0]
    rows, cols, at, frame = 24, 80, None, None
    resizes_path, reflow = None, "wrap"
    for i, a in enumerate(args):
        if a == "--rows":
            rows = int(args[i + 1])
        elif a == "--cols":
            cols = int(args[i + 1])
        elif a == "--at":
            at = int(args[i + 1])
        elif a == "--frame":
            frame = int(args[i + 1])
        elif a == "--resizes":
            resizes_path = args[i + 1]
        elif a == "--reflow":
            reflow = args[i + 1]
    data = open(path, "rb").read()
    if frame is not None:
        ends = [m.end() for m in re.finditer(rb"\x1b\[[0-9]+A\r", data)]
        if not ends:
            sys.exit("no complete frame in %s" % path)
        data = data[: ends[min(frame, len(ends)) - 1]]
    if at is not None:
        data = data[:at]
    s = Screen(rows, cols)
    events = []
    if resizes_path:
        for line in open(resizes_path):
            parts = line.split()
            if len(parts) == 3:
                events.append((int(parts[0]), int(parts[1]), int(parts[2])))
        events.sort()
    if not events:
        s.feed(data.decode("utf-8", "replace"))
    else:
        # Replay in slices, resizing where the real pty was resized. The slice
        # boundary is a byte offset, so it can land mid-escape-sequence; the
        # feeder is fed the whole prefix each time instead, which cannot.
        pos = 0
        for off, ncols, nrows in events:
            off = max(pos, min(off, len(data)))
            s.feed(data[pos:off].decode("utf-8", "replace"))
            s.resize(ncols, nrows, reflow)
            pos = off
        s.feed(data[pos:].decode("utf-8", "replace"))
    print(s.text())


if __name__ == "__main__":
    main()
