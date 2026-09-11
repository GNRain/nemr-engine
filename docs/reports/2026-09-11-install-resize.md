# Resizing the window mid-install: one region, or none, but never twenty

2026-09-11. The Product Owner, revisiting a ruling rather than reporting a bug
against it:

> Resizing the terminal mid-install destroys the display. The region redraws at
> the new width without erasing the old one, so every frame stacks down the
> screen — twenty copies of the step line, the bar, the pane and the cat. […]
> Out of scope is not an acceptable outcome for someone who drags a window
> during a two-minute build, which is a normal thing to do.

Both offered behaviours are now in, and which one you get depends on whether
the new window can still hold the screen: **(a) re-fit** when it can, **(b) give
up cleanly** when it cannot. The invariant is the same either way, and it is
asserted.

---

## 1. Why it stacked

Every frame ends by moving the cursor **up** by the number of rows it wrote.
That is correct only while those rows are still one row each. When the terminal
narrows, rows wider than the new width are rewrapped into two, so the walk-back
stops short — by exactly as many rows as wrapped — and the next frame draws
below the last instead of over it. Twelve of those in a drag is twelve copies.

Measured, before any change, at 100 columns narrowing to 96 and then 92:

```
worst frame 19: titles=5 bars=6 pane-borders=2
```

Five step lines, six bars, on one screen.

## 2. What it does now

**SIGWINCH is handled.** The renderer traps it, notes it, and acts at the top of
the next frame, where nothing is half-drawn. Then:

| The new window | What happens |
|---|---|
| still 79x18 or larger | **(a)** erase the whole block, re-measure, re-fit the widths, redraw at the new size |
| smaller than that | **(b)** erase the block, say so once, and print one plain line per step for the rest of the run |

Option (b) is what a terminal under the minimum has always got; a resize now
reaches the same place from the other direction. It says which size it has and
which it needs:

```
  the window is now 70x44; this screen needs 79x18.
  The rest of this run prints plainly.
```

## 3. The three things that had to be true

**The frame is one write.** The rows used to go out one `printf` at a time. A
resize landing between two of them rewrapped the half already on screen, and
every relative move after that was wrong. One buffer, one write, and the window
in which that can happen shrinks to the terminal's own parsing. This is
load-bearing: written in pieces again, the drag test fails.

**The erase starts at the block's first row, not at the cursor.** If a resize
lands while a frame is being written, the walk-back at its end stops short and
the cursor is *below* the block's top — erasing from there leaves the rows
above it. Measured: exactly two rows survived, the title and the step line,
because exactly two rows had wrapped. The row the block starts on is recorded
when the screen is taken, and the erase starts there.

**And the cursor is asked about anyway.** There is a second way to be wrong: if
a terminal scrolls its top away to make room for rewrapped lines, the block
moves *up*, and the recorded row is then too low. So the handler asks the
terminal where the cursor is and erases from whichever row is higher. See the
honesty note in §5 — this one could not be made to fail.

## 4. How it is tested

`script(1)` gives a pty but never resizes it, so nothing here could reproduce
what a person does every day. `scripts/lib/resize_pty.py` does: it opens a pty,
makes it the child's controlling terminal so SIGWINCH is really delivered,
**tracks the screen** so the cursor-position query is answered with the row the
cursor is really on, and calls `TIOCSWINSZ` at given times.

That last part matters more than it sounds. With a canned answer to that query,
code that re-anchors itself on it passes a test it should fail — which is what
happened here before the harness was taught to track the cursor.

`scripts/lib/region_single.py` replays a capture, stops at **every** complete
frame, renders the screen as it stood, and counts the region's landmarks on it:
the title, the bar, the pane's borders. More than one of any of them is more
than one region.

Terminals do not agree about what happens to lines that no longer fit, so every
claim is made under all three readings:

| Model | What it says |
|---|---|
| `wrap` | lines rewrap and push what is below into the blank space at the foot of the screen; the cursor travels with its own line (VTE, xterm) |
| `wrap-scroll` | the same, but the top scrolls away instead — the harsh reading, where a block can end up above where it was drawn |
| `truncate` | lines are clipped in place and the cursor keeps its row |

**Thirteen assertions**, in `scripts/test_install.sh`: a drag through twelve
sizes under each of the three models; that the block ends up at the width now
in force rather than the old one; three deliberate resizes narrower and wider;
the too-small path, by what it says and by the run still finishing; and a
terminal that never answers the cursor query, which draws in place and survives
a resize anyway.

## 5. Proved red first, and one that could not be

| Neuter | The guard's answer |
|---|---|
| the renderer stops trapping SIGWINCH | RED — six regions on one screen, under every model |
| the frame is written in pieces again | RED — two regions: a resize inside a half-written frame |
| a window too small to draw in is not noticed | RED — the message is gone, and three regions appear |
| **the erase stops asking where the cursor is** | **GREEN — it could not be made to fail** |

The last row is the honest one. That branch only matters if a terminal scrolls
the block above the row it was drawn at, and across six runs under the harshest
reflow model the harness could not produce that. It is six lines, it is free,
and it is kept — but it is defence, not a proven guard, and it is recorded here
as such rather than counted as one.

## 6. Found on the way: `tput` answers when it should refuse

`nemr_term_size` asks the kernel first (`stty size </dev/tty`) and falls back to
`tput`. ncurses looks at stdout and then stderr for a terminal; when **neither**
is one it does not fail — it answers from the static terminfo entry, which for
`xterm-256color` is 80x24. Measured on a 132x40 terminal with both redirected:
it reports 80x24.

That is a plausible wrong answer, which is the worst kind: it would draw the
screen at the wrong width, or refuse a window that is plenty big, and say
nothing. The fallback now runs only where `tput` can see a terminal, and
otherwise the size probe refuses — which the region already knows how to
handle, because refusing is what it does on any terminal it cannot measure.
