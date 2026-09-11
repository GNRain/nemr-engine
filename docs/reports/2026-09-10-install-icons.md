# Icons in the install region: colour on two rows, one emoji per step, and the column arithmetic that keeps the cat still

2026-09-10. The Product Owner asked for the two top rows of the live region to
be coloured and to carry emoji — one constant icon on the title, one fitting
icon per step — with a hard constraint:

> emoji are double-width and render inconsistently, which is what tore the
> region before. So they must be counted as two columns wherever the left
> column's width is computed, and the padding to the cat must be derived from
> that count rather than from the string length. […] If any step's emoji cannot
> be guaranteed two columns, drop emoji from the region entirely […] I would
> rather have the layout than the icons.

The icons are in, and the layout is measured rather than trusted.

---

## 1. Counted, never measured

An emoji is **one character** to bash and **two columns** to the terminal.
`${#var}` returns the first number; the layout needs the second. So the left
column's rows are composed from parts whose widths are known, and the padding
comes from that sum:

```
    left column = 2 (indent) + 2 (icon) + 1 (space) + the phrase's characters
    padding     = NEMR_REGION_LEFT_COLS - that sum
```

The phrase is scrubbed to ASCII before it is counted, so "characters are
columns" is enforced rather than assumed, and it is truncated to what is left
of the column, so a longer phrase added later cannot push the cat either.

At the narrowest supported width, 79 columns, that is:

| | columns |
|---|---|
| indent | 2 |
| icon | 2 |
| space | 1 |
| longest phrase, `installing the privileged helper` | 32 |
| **left column total** | **37 of the 40 available** |
| gap | 2 |
| cat | 37 |
| **row** | **79, exactly the terminal** |

## 2. The icons

One per step, chosen once, never animated. The title's is constant.

| Step | Icon | Codepoint |
|---|---|---|
| *(title, constant)* | 🐈 | U+1F408 cat |
| installing packages | 📦 | U+1F4E6 package |
| disabling the system containerd | 🛑 | U+1F6D1 octagonal sign |
| making the mount shared (WSL2) | 🔗 | U+1F517 link |
| adding subuid/subgid ranges | 🆔 | U+1F194 ID button |
| delegating cgroup controllers | 🔀 | U+1F500 twisted arrows |
| enabling lingering | ⏳ | U+23F3 hourglass with flowing sand |
| installing the user units | 📋 | U+1F4CB clipboard |
| starting rootless containerd | 🚀 | U+1F680 rocket |
| updating the shell environment | 🐚 | U+1F41A spiral shell |
| building the engine | 🔨 | U+1F528 hammer |
| installing the privileged helper | 🔐 | U+1F510 closed lock with key |
| building the client | 🔧 | U+1F527 wrench |
| pulling the base image | 📥 | U+1F4E5 inbox tray |
| checking for Claude Code | 🤖 | U+1F916 robot |
| running the smoke test | ✅ | U+2705 check mark button |

**Every one is a single codepoint with East Asian Width `W`.** That is the
property that makes two columns a fact rather than a hope, and it is asserted
against python's `unicodedata` for every entry in the table, so an icon added
later that cannot be guaranteed two columns fails the gate instead of tearing
somebody's screen.

What that rule excludes, and why it has to:

| Rejected | Why |
|---|---|
| ⚙ ⬇ ✔ ⏱ 🛠 🗂 🏷 🛡 | East Asian Width `N` — one column unless a U+FE0F variation selector is added, which terminals disagree about |
| ⛓ | Width `A` (ambiguous) — one column or two depending on the font |
| 👍🏽 🧑‍💻 🇬🇧 1️⃣ | more than one codepoint: skin tones, zero-width joiners, flags and keycaps are 2 columns to a terminal that clusters graphemes and 4 to one that sums `wcwidth`. Single-codepoint `W` is the intersection where both models answer 2 |
| 🧩 🧰 | width-correct, but Unicode 11 (2018): missing from Windows 10 before 1809, where the substitute is a **narrow** box. Replaced with 🔀 and 🔧, both Unicode 6.0 |

The table is written as literal UTF-8 bytes, never as `$'\U0001F4E6'` — bash
does not interpret `\U` under `LC_ALL=C`, and would put ten ASCII characters
where two columns were counted. A static assertion refuses that escape, and
refuses variation selectors, joiners, skin tones and keycaps, anywhere in the
three scripts that draw the screen.

## 3. Measured: the cat does not move

A relative check cannot see this. The icon rows are shifted on *every* frame, so
comparing frames with each other finds nothing — the first version of this
measurement passed happily against a deliberately broken build.

So the check is absolute. `scripts/lib/region_columns.py` reads the cat's own
frames from `scripts/lib/cat.sh --show`, which is where the art and therefore
its per-line indent lives, and for every complete frame in a captured run it
requires each drawn row to put that art at exactly

```
    (cols - 2 - 37) + 2 + that art line's own leading spaces
```

At 79 columns, with icons on:

```
frames 25
widths 0 55 59 62 63 64 69 71 72 74 75 76 78 79
columns exact (24 frames checked against the art)
```

And with the padding measured (`${#plain}`) instead of counted — the one-line
neuter — the same capture:

```
columns WRONG row 1: '_' is in column 55, should be 54
columns WRONG row 2: '\`*-.' is in column 55, should be 54
```

Two rows, one column, on every frame. That is the whole failure mode, and it is
now the thing that fails the gate.

## 4. When they go

Icons follow colour — no terminal, `NO_COLOR`, `TERM=dumb` is plain text, the
same rule the ✓ and ✗ marks follow — **and two more that colour does not care
about but the layout does**:

| Also off when | Because |
|---|---|
| `TERM=linux` | the physical console has 256 glyphs and no emoji among them; the substitute is narrow and the row tears |
| the locale is not UTF-8 | the shell and the terminal are then decoding differently, and four bytes drawn as four characters is a four-column icon |
| `NEMR_NO_EMOJI=1` | by hand, for the machine whose font we cannot reproduce |

tmux and screen are deliberately **not** excluded: both have carried correct
wide-character tables for years, and refusing there would cost the icons for a
large share of real users to guard against a decade-old bug.

Every one of those is asserted by counting the two-column characters in a real
capture: it must be zero. The log says which way it went and why:

```
    icons:     no — the locale is not UTF-8  (2 cells each, counted not measured)
```

## 5. Found on the way

The audit run before the change went looking for every place a width is
computed. Three of the four things it found were already live defects.

**The pane could tear on its own.** It shows the tail of what the current step
prints, and build output is not ASCII: cargo prints arrows, apt prints accented
package names, a test prints a check mark. Any of those is one character and
one, two or zero columns, and the pane truncated by characters and padded by a
field width. One wide glyph in a step's output would have moved the pane's right
border and the cat with it. Everything above ASCII is now **removed** before the
pane draws — not sliced, removed — which is what makes byte, character and
column the same number in that column. The log still has the full text.

**A dead branch that would have torn it too.** `tick` and `cross` had a region
path that published the whole line array into `nemr_region_publish`'s
`<phrase> <done> <total> <icon>` signature, which would have put a line of text
where the counts go, and an East-Asian-*ambiguous* ✓ inside the fixed-width
column. Nothing in the installer reaches it — only `install_server.sh` calls
those, and it has no region — which is exactly why it would have been found the
hard way. They now route through the notes, printed once the region resolves.

**An exit inside a redirect went into the log.** While building this, a `set -u`
error inside the `{ … } >>"$LOG"` block sent the whole failure report into the
log file and left the screen blank — the single authority speaking to whatever
stdout happened to be at that moment. It now saves the real stdout on fd 3 when
the trap is installed and speaks there, whatever a later block has redirected.
Asserted both ways: the outcome reaches stdout, and it does **not** reach the
file the block was writing to.

**A stray line on a machine with no controlling terminal.** `stty size </dev/tty`
had its redirections in the order that reports the failure on the real stderr.
Reordered.

## 6. What it looks like

These are rendered from captured pty transcripts, so what you see is what the
terminal was sent. One artefact of that: an emoji holds two cells, and the
renderer prints the second as a space — so the icons below look as though they
are followed by two spaces where a terminal shows the glyph spanning both cells
and then one space.

At 79 columns, the narrowest width the region runs at:

```
  🐈  Installing nemr...                               _
  🐚  updating the shell environment                   \`*-.
                                                       )  _`-.
  [##########--------]  8 of 14                       .  : `. .
                                          `-._        : _   '  \
  +------------------------------------+      `-.\    ; o` _.   `*-._
  |                                    |          |   `-.-'          `-.
  |                                    |          |     ;       `       `.
  |                                    |          /     :.       .        \
  |                                    |         /      . \  .   :   .-'   .
  |                                    |        /       '  `+.;  ;  '      :
  +------------------------------------+       |        :  '  |    ;       ;-.
                                               _)       ; '   : :`-:     _.`* ;
                                                       /  .*' ; .*`- +'  `*'
                                                      *-*   `*-*  `*-*'
```

At 132 columns, where the pane takes everything the cat leaves:

```
  🐈  Installing nemr...                                                                                    _
  📥  pulling the base image                                                                                \`*-.
                                                                                                            )  _`-.
  [###############---]  12 of 14                                                                           .  : `. .
                                                                                               `-._        : _   '  \
  +-----------------------------------------------------------------------------------------+      `-.\    ; o` _.   `*-._
  |                                                                                         |          |   `-.-'          `-.
  |                                                                                         |          |     ;       `       `.
  |                                                                                         |          /     :.       .        \
  |                                                                                         |         /      . \  .   :   .-'   .
  |                                                                                         |        /       '  `+.;  ;  '      :
  +-----------------------------------------------------------------------------------------+       |        :  '  |    ;       ;-.
                                                                                                    _)       ; '   : :`-:     _.`* ;
                                                                                                            /  .*' ; .*`- +'  `*'
                                                                                                           *-*   `*-*  `*-*'
```

And with the pane carrying a step's own output, at 79 columns:

```
  🐈  Installing nemr...                               _
  🔨  building the engine                              \`*-.
                                                       )  _`-.
  [------------------]  0 of 8                        .  : `. .
                                          `-._        : _   '  \
  +------------------------------------+      `-.\    ; o` _.   `*-._
  |    Compiling nemr-daemon-api v0.1. |          |   `-.-'          `-.
  |    Compiling nemr-daemon-api v0.1. |          |     ;       `       `.
  |    Compiling nemr-daemon-api v0.1. |           \    :.       .        \
  |    Compiling nemr-daemon-api v0.1. |            |   . \  .   :   .-'   .
  |    Compiling nemr-daemon-api v0.1. |            /   '  `+.;  ;  '      :
  +------------------------------------+           /    :  '  |    ;       ;-.
                                                  |     ; '   : :`-:     _.`* ;
                                                  _)   /  .*' ; .*`- +'  `*'
                                                      *-*   `*-*  `*-*'
```

## 7. Asserted

`scripts/test_install.sh`, all green, the count itself asserted:

- every step has an icon, and every icon is a single codepoint of width `W`
- at 79 and at 132 columns the cat is in exactly the column the art says, on
  every frame, with the longest label and its icon on screen
- the block is exactly as wide as the terminal, and stays on the same row from
  frame to frame
- no variation selector, joiner, skin tone, flag or `$'\U…'` escape anywhere in
  the code that draws the screen
- wide characters in a step's own output never reach the pane
- every step phrase is ASCII and short enough to carry an icon at 79 columns
- five ways of turning the icons off, each measured as zero two-column
  characters in a real capture
- an exit inside a redirected block still prints its outcome to the real stdout

## 8. Proved red first

| Neuter | The guard's answer |
|---|---|
| the left column pads from `${#plain}` instead of the cell count | RED — `row 1: '_' is in column 55, should be 54`, on every frame |
| the pane stops removing bytes above ASCII | RED — the border moves out of column and 15 two-column characters reach the pane |

The second, with the filter disabled, drew this:

```
  |    正 在 编 译  café ✅  crate-1                       |
```

Two columns of ink per glyph, one column of arithmetic, and the border pushed
six columns past where the cat starts.
