# The bar, the output pane, and what the region costs in rows

**Three things from the Product Owner, 2026-09-10**, after the layout rendered
on Windows Terminal maximized.

---

## 1. The bar was a font problem, and now it is not

You saw a solid grey block. **The bytes were right**: a capture from this
machine writes `█████░░░░░░░░░░░░░  4 of 14` — U+2588 FULL BLOCK and U+2591
LIGHT SHADE, correctly interleaved. So the two characters were rendering as the
same thing in that font, or in whatever font was substituted for them.

I could not see the screenshot, so this is reasoned from your description and
from the bytes. It does not matter much, because **there is no way to ask a
terminal whether a glyph will render**, and a bar that depends on the answer is
a bar that will do this again on some other machine. So it no longer depends on
one:

```
[------------------]  0 of 14
[#####-------------]  4 of 14
[###########-------]  9 of 14
[##################]  14 of 14
```

ASCII, bracketed so the empty half reads as part of the bar rather than as
trailing punctuation. It draws identically in every font, which is worth more
than looking better where the font happens to cooperate. Asserted: no block
character appears anywhere in a run.

---

## 2. The output pane

The last five lines the current step actually printed, bordered, emptied when
the step changes. Each step's output now goes to its own file first — the pane
tails that — and the whole of it is folded into the log when the step ends, so
the log reads exactly as it did.

### Mid-build — the crates cargo is compiling

```
  Installing nemr...                                  _
  building the client                                 \`*-.
                                                       )  _`-.
  [############------]  10 of 14                      .  : `. .
                                          `-._        : _   '  \
  +------------------------------------+      `-.\    ; o` _.   `*-._
  | $ ./scripts/install_sync_client.sh |          |   `-.-'          `-.
  |    Compiling nemr-cloud v0.1.0 (/h |          |     ;       `       `.
  |     Finished `release` profile [op |          /     :.       .        \
  |                                    |         /      . \  .   :   .-'   .
  |                                    |        /       '  `+.;  ;  '      :
  +------------------------------------+       |        :  '  |    ;       ;-.
                                               _)       ; '   : :`-:     _.`* ;
                                                       /  .*' ; .*`- +'
                                                      *-*   `*-*  `*-*'
```

### Mid-pull — the image step, pulling by digest

```
  Installing nemr...                                  _
  pulling the base image                              \`*-.
                                                       )  _`-.
  [##############----]  11 of 14                      .  : `. .
                                          `-._        : _   '  \
  +------------------------------------+      `-.\    ; o` _.   `*-._
  | $ ./scripts/fetch_base_image.sh    |          |   `-.-'          `-.
  | ==> Pulling ghcr.io/gnrain/nemr-ba |          |
  |                                    |          \     :.       .        \
  |                                    |           \    . \  .   :   .-'   .
  |                                    |            \   '  `+.;  ;  '      :
  +------------------------------------+             |  :  '  |    ;       ;-.
                                                     _) ; '   : :`-:     _.`* ;
                                                      *-* .*' ; .*`- +'  `*'
                                                            `*-*  `*-*'
```

### Mid-smoke-test — the assertions as they pass

```
  Installing nemr...                                  _
  running the smoke test                              \`*-.
                                                       )  _`-.
  [################--]  13 of 14                      .  : `. .
                                          `-._        : _   '  \
  +------------------------------------+      `-.\    ; o` _.   `*-._
  |           NEMR_SKIP_API=1 — skippi |          |   `-.-'          `-.
  | [06] persistence across stop/start |          |     ;       `       `.
  |      ok   task gone after stop     |           \    :.       .        \
  |      ok   container record survive |            |   . \  .   :   .-'   .
  |      ok   volume stays mounted acr |            /   '  `+.;  ;  '      :
  +------------------------------------+[0
                                                     _) ; '   : :`-:     _.`* ;
                                                      *-* .*' ; .*`- +'  `*'
                                                            `*-*  `*-*'
```


**Containment.** Nothing from a step can escape the pane, because the pane
never trusts it: escape sequences are stripped (a step that colours its output
or moves the cursor would otherwise write outside its own frame), every
remaining control byte is removed **except the newline**, carriage returns and
tabs are flattened, and what is left is cut to the pane's inner width **by
characters, not bytes**, so a multi-byte glyph is never sliced in half.

Two defects were found doing this, both by looking at the screen rather than the
code. A half-written escape sequence caught mid-read left a dangling `ESC [`
that ate the pane's own border — the stray `[2` a row below it. And the first
version of the control-byte strip deleted `\n` along with the rest, which
collapsed the whole tail into a single line: the pane showed one line where it
should have shown five. Both are asserted now, including a deliberately hostile
step that prints colour, a cursor move, a tab, a carriage return and a
300-character line.

**If a step prints nothing the pane stays empty.** It invents nothing. Blank
lines are skipped when choosing the last five, because a pane of five showing
two blanks is not the tail of anything.

---

## 3. What the region costs in rows — before you decide anything

You asked for the trade rather than the change, so nothing here is changed.

| | columns | rows |
|---|---|---|
| **as it is now — cat + pane** | **79** | **18** |
| the same without the pane | 79 | 18 |
| without the cat, keeping the pane | 40 | 14 |
| without the cat or the pane — the line and the bar alone | 40 | 6 |

**The pane costs nothing in rows.** It sits at rows 5–11 of the left column,
inside the cat's own 15, so on any terminal tall enough for the region there is
room for both. The ordering you asked for — drop the pane before the cat — is
implemented and correct, but at the current threshold it never triggers.

**The 18 rows are the cat's** (15, plus the command line, plus a margin). That
is the whole of the row gate. If a normal-sized Windows Terminal window turns
out to be the 17-or-fewer case, the choice is not to shrink the cat — it is
**whether the cat is worth the 4 rows** between 14 and 18. At 14 the screen
still has the live line, the bar, the count and the pane; at 6 it has the line
and the bar. I would keep the cat and let short windows fall back, because the
fallback is now short and says why — but that is yours.

The refusal reason ships in this branch, so your rerun at a normal window size
will name the gate in one line on screen and in the `--- install screen` block
of the log.

---

## 4. What is asserted

`scripts/test_install.sh`, **74 assertions**. New: the bar's shape and that no
block character appears anywhere; the pane's border, that it shows several of
the step's own lines and that they are the newest ones; that colour, cursor
moves, tabs, carriage returns and a 300-character line are all stripped or cut
before drawing and the border survives them; that the pane empties when the step
changes; and that everything it showed reaches the log.
