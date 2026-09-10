# The default install screen: one live line, and a result

**Built to the Product Owner's brief, 2026-09-10:** *"The difference isn't
colour, it's that ours narrates the work while theirs reports the result. Make
the default do the same."* Compared against Claude Code's own installer: four
lines — it worked, the version, the location, what to run next.

Every screen below is a **rendered pty transcript** — the bytes a terminal would
have received, replayed through `scripts/lib/render_pty.py`.

---

## 1. Why the region did not render on WSL2 — asked first, answered first

**Your suspicion is refuted, mechanically.** An unanswered DSR query cannot
cause the fallback. `scripts/lib/region.sh`: `here="$(_nemr_cursor_row)" || return 0`
— a silent terminal makes the region draw *where it stands*; it does not stop it
drawing. DSR is read inside the renderer **after** the region has already
started, so it cannot be what refused.

**What can refuse, and did not get to say so.** Twelve conditions, in two files,
every one of them a bare `return 1`: `--verbose`, `--quiet`/`NEMR_CAT=0`, stdout
not a tty, `NO_COLOR`, `TERM` dumb or unset, an unreadable terminal size, rows
below the animation's 17, columns below the threshold, rows below the region's
18, an unwritable state file, and a `mkfifo` that fails. **Measured: a run that
fell back was byte-for-byte identical, in the terminal and in the log, to a run
that drew.** That is why your report could not be answered from here — and it is
the real defect.

**The strongest remaining candidate is rows, not columns.** You measured the
width ("well over 80") and the width gate is 79. The row gate is 18, and it went
from 17 to 18 in the same commit that took the width to 80. A wide, short pane —
a split, or a window dragged wide — refuses on rows and looks exactly like what
you saw. Two others remain possible and are equally silent: stdout not a tty
(`| tee`), and `mkfifo` failing when `TMPDIR` points at a Windows drive, where
FIFOs do not exist.

**So every refusal now names itself**, always in the log and, when the cause is
the terminal rather than your own instruction, in one dim line on screen:

```
  no live screen — 70 columns, the screen needs 79 (see the log)
```

and in the log, on every run, whichever way it went:

```
--- install screen
    live:      NO — 70 columns, the screen needs 79
    measured:  70 cols x 30 rows   by: stty /dev/tty
    needs:     79 cols x 18 rows
    terminal:  TERM=xterm-256color  tty=yes  NO_COLOR=unset  NEMR_CAT=unset
    host:      linux  TMPDIR=/tmp
    note:      the cursor query (DSR) does not gate this — it only
               decides how far to scroll the screen into place
```

**One line from that machine settles it**, and now needs no second run: the
`--- install screen` block from `~/.local/state/nemr/install-*.log` of either
run you already made — if those logs predate this commit they carry nothing, in
which case one fresh run will.

---

## 2. What the default is now

During the run: **one line, replaced in place**, the bar, the count, and the
cat. No step list, no plan, no file names, no digests, no package names. When it
finishes: the result, and only the result.

### 1. A first install, live

```
rain@Rain:~/nemr-engine$ ./scripts/install.sh --yes

  Installing nemr...                                  _
  starting rootless containerd                        \`*-.
                                                       )  _`-.
  ███████░░░░░░░░░░░  6 of 14                         .  : `. .
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
```

### 2. The same run when it finishes

```
✓ nemr is installed.

      Version:   0.1.0
      Installed: ~/.local/bin/nemr

    Try:  nemr create myproject
          nemr ui
```

### 3. An idempotent run

```
✓ nemr is installed.

      Version:   0.1.0
      Installed: ~/.local/bin/nemr

    Try:  nemr create myproject
          nemr ui
```

### 4. When Claude Code is missing

```
✓ nemr is installed.

      Version:   0.1.0
      Installed: ~/.local/bin/nemr

    Try:  nemr create myproject
          nemr ui

  ⚠ Claude Code isn't installed yet — nemr needs it inside each
    session. See claude.ai/code
```

### 5. A run where a step fails

```
✗ Install stopped.

      Failed at:  pulling the base image
      Because:    the base image could not be obtained
      Fix:        ./scripts/fetch_base_image.sh, then run this again

    Full log: ~/.local/state/nemr/install-20260910-135920.log
```

### 6. With no live screen — a 70-column terminal

```
  no live screen — 70 columns, the screen needs 79 (see the log)
  installing packages                new
  disabling the system containerd    new
  adding subuid/subgid ranges        new
  delegating cgroup controllers      new
  enabling lingering                 new
  installing the user units          new
  starting rootless containerd       new
  updating the shell environment     new
  building the engine                new
  installing the privileged helper   new
  building the client                new
  pulling the base image             new
  checking for Claude Code           absent
  running the smoke test             ok
✓ nemr is installed.
      Version:   0.1.0
      Installed: ~/.local/bin/nemr
    Try:  nemr create myproject
          nemr ui
```

---

## 3. The rules

**Colour and marks.** `✓` green, `✗` red, `⚠` amber — and only **outside** the
live region, because they are double-width in some terminals and inconsistent in
others, which is what would tear a fixed-width two-column layout. Inside the
region: plain ASCII, plus the bar's two block characters, which are single-width
and fall back to `#`/`-` when the locale is not UTF-8. Colour is green for done,
dim for pending, bright for the step running now, red uppercase for FAILED,
amber for warnings — all of it off without a tty, with `TERM=dumb`, or with
`NO_COLOR`, and **the marks go with the colour**, replaced by `OK`, `XX`, `!!`.

**The plan is the consent, and that does not change.** An interactive run prints
it in full and asks. A `--yes` run has already consented, so it gets the screen
and not the recital — that recital was most of the hundred lines.

**`--verbose` keeps everything** today's run showed: the full plan, every step's
detail, appended as it happens, no live screen.

**Failure carries a reason and a fix, in plain words.** Each step has a default,
and a small table reads a handful of causes out of the log — a missing
`iptables`, a full disk, no network, no sudo, a refusal from ghcr.io. Anything
unrecognised says what step failed and points at the log rather than guessing:
a guess dressed as a diagnosis is worse than "read this".

**When there is no live screen it still has to look right**, because that is
what you keep getting: one short line per step in plain words with a status
token, then the same four-line result. **23 lines**, where the old default
printed about a hundred.

---

## 4. What is asserted

`scripts/test_install.sh`, **63 assertions**, count asserted. New here: the live
screen names what it is doing, with a bar and a count; **one** step line on
screen, not a list; the cat beside it; the success block's four facts; no
narration in it (no plan, no digests, no package names, no file list); the
failure block's step, reason, fix and log path; a 70-column terminal getting no
live screen, saying so, and still ending in the same result, in 23 lines; the
decision recorded in the log with the size measured, the thresholds, the
terminal and DSR named as not gating; the plan shown interactively and not on
`--yes`; `--verbose` showing everything; colour on a terminal; `NO_COLOR`
turning colour and marks off together.
