# The install screen: 80 columns, live, and what it leaves behind

**Built to the Product Owner's brief, 2026-09-10** — option C from
[the width report](2026-09-10-install-region-width.md), plus the default run's
layout, plus the two rules that layout forces.

Every capture below is a **rendered pty transcript**: the bytes a real terminal
would have received, replayed through `scripts/lib/render_pty.py` and printed
as the screen it would have produced.

---

## 1. What was fixed first: the mid-run collapse (F-28)

The region covered the "Installing" block only. The Claude Code check and the
smoke test ran after it, in append-only mode, so a second full-width cat was
drawn under a finished block and the smoke test's own scrolling took the list
off the screen. Three screenshots at 200 columns, from the Product Owner.

Both are steps now. They are probed, they appear in the list from the first
frame, and they resolve in place. **One region spans the whole run** and closes
once, at the end, with nothing drawn outside it. Nothing else here was built
until that was true.

---

## 2. The width: 80 columns, and a status that cannot set it

| | |
|---|---|
| left column | **41** = 2 indent + 1 glyph + 1 space + **29 label** + 1 + **7 status** |
| gap | 2 |
| cat | 37 |
| **threshold** | **80** — the default terminal |

The status has a **fixed budget and is truncated to it**; the full sentence goes
to the log. That is the principle rather than the number: sized from an
idempotent run the column was 89, and a first run's package list made the line
145, which wrapped over the next step's row and cost the cat its first line.
A column sized to whatever a step decides to print is not a design.

The labels lost their explanations (`lingering, so the user manager runs without
a login` → `lingering`). The plan above the region still names every step in
full, and the plan is what the user consents to.

---

## 3. The captures

The first two are a **first install** — the run every capture before this one
skipped, and the one whose long statuses used to tear the layout. Its steps are
stubbed (`NEMR_TEST_STEP_STUB`, test-only) so the layout can be exercised with
first-run statuses without provisioning this machine again; everything about the
screen is real.

### A first install, 80 columns — live

```
  write, copy or read a Claude login — you log in with /login inside a session (
  + packages                          new              _
  + system containerd off             new              \`*-.
  + subuid/subgid ranges              new               )  _`-.
  + cgroup v2 delegation              new              .  : `. .
  + lingering                         new  `-._        : _   '  \
  + user units                        new      `-.\    ; o` _.   `*-._
  + rootless containerd               new          |   `-.-'          `-.
  > shell environment                 …          |     ;       `       `.
  . the engine (nemr, nemrd)                       /     :.       .        \
  . helper + sudoers grant                        /      . \  .   :   .-'   .
  . client CLI (ui, push, pull)                      /   '  `+.;  ;  '      :
  . base image                                      /    :  '  |    ;       ;-.
  . Claude Code (prerequisite)                     |     ; '   : :`-:     _.`* ;
  . smoke test                                     _)   /  .*' ; .*`- +'  `*'
                                                       *-*   `*-*  `*-*'
```

### The same run, resolved

```
  write, copy or read a Claude login — you log in with /login inside a session (
  + packages                          new
  + system containerd off             new
  + subuid/subgid ranges              new
  + cgroup v2 delegation              new
  + lingering                         new
  + user units                        new
  + rootless containerd               new
  + shell environment                 new
  + the engine (nemr, nemrd)          new
  + helper + sudoers grant            new
  + client CLI (ui, push, pull)       new
  + base image                        new
  ! Claude Code (prerequisite)     absent
  + smoke test                         ok
nemr is installed.
  nemr create myproject     make a session and attach to it
  nemr ui                   the same thing in a browser
Inside a session, run /login the first time — that logs this machine in, and
the login stays here (it is never copied into a bundle or to another machine).
```

### An idempotent run, resolved

```
  write, copy or read a Claude login — you log in with /login inside a session (
  + packages                         done
  + system containerd off            done
  + subuid/subgid ranges             done
  + cgroup v2 delegation             done
  + lingering                        done
  + user units                       done
  + rootless containerd              done
  + shell environment                done
  + the engine (nemr, nemrd)         done
  + helper + sudoers grant           done
  + client CLI (ui, push, pull)      done
  + base image                         ok
  + Claude Code (prerequisite)       done
  + smoke test                         ok
nemr is installed.
  nemr create myproject     make a session and attach to it
  nemr ui                   the same thing in a browser
Inside a session, run /login the first time — that logs this machine in, and
the login stays here (it is never copied into a bundle or to another machine).
```

### A run where a step fails — what is left on the screen

```
  write, copy or read a Claude login — you log in with /login inside a session (
  + packages                          new
  + system containerd off             new
  + subuid/subgid ranges              new
  + cgroup v2 delegation              new
  + lingering                         new
  + user units                        new
  + rootless containerd               new
  + shell environment                 new
  + the engine (nemr, nemrd)          new
  x helper + sudoers grant         FAILED
  . client CLI (ui, push, pull)
  . base image
  . Claude Code (prerequisite)
  . smoke test
Stopped at: helper + sudoers grant
What every command printed is in:
  /home/nemr/.local/state/nemr/install-20260910-065147.log
Nothing after that step ran. Fix the cause and run this again;
it skips what is already done.
```

…and its last lines, which are the point of §4:

```
  . smoke test
Stopped at: helper + sudoers grant
What every command printed is in:
  /home/nemr/.local/state/nemr/install-20260910-065147.log
Nothing after that step ran. Fix the cause and run this again;
it skips what is already done.
```

---

## 4. The two rules the layout forces

**On failure or Ctrl-C the region resolves into normal scrollback text.** The
step list, the failure and the log path are printed as ordinary lines, so they
survive the script exiting and can be scrolled to and copied. The **resolve
belongs to the main shell**, not the renderer: Ctrl-C reaches the whole process
group and kills the renderer outright, and a resolve only the renderer could
perform is a resolve that does not happen on the one exit that most needs it.
Ctrl-C, mid-run:

```
a command line
  + packages                          new
  + user units                        new
  > the engine (nemr, nemrd)          …
  . smoke test

Stopped at: interrupted
```

**A terminal too short gets today's output, and the region is never started.**
Below 18 rows (the cat's 15, plus the command line, plus room) or below 80
columns, `nemr_region_start` returns non-zero and the run appends line by line
exactly as it did before. Nothing is wrapped and nothing is truncated to make it
fit.

---

## 5. How it takes the screen

It does **not** clear. Clearing would take the user's scrollback with it, and
what was on the screen before is theirs. The region is **scrolled** into place:
the cursor goes to the bottom row and prints newlines until the command line the
user typed sits on the first row, which moves everything above it up into
scrollback intact. The region then owns every row below.

How far to scroll is asked of the terminal (DSR, `ESC [ 6 n`). Two things were
learned doing it: the query must go to `/dev/tty` rather than through `read -p`,
whose prompt goes to stderr and is very often redirected; and the wait for the
answer must be short — at one second, a terminal that does not answer held the
first frame back by a visible beat. A terminal that does not answer at all gets
the region drawn where the cursor already is, rather than a guess that would
push its output off the screen.

`script` does not answer DSR — there is no terminal emulator on the other end of
a capture — so the captures above supply the row through a test-only seam
(`NEMR_TEST_CURSOR_ROW`). **The scroll itself is the one thing here not verified
end to end locally.** On a real terminal it is the DSR path.

---

## 6. Colour

| state | shown as | colour |
|---|---|---|
| finished, did work | `+ label   new` | green |
| finished, already so | `+ label  done` | green |
| finished, verified | `+ label    ok` | green |
| running now | `> label     …` | bright |
| pending | `. label` | dim |
| failed | `x label FAILED` | red, uppercase |
| warning | `! label absent` | amber |

Plain ASCII glyphs, no emoji, no icons. All colour is off without a tty, with
`TERM=dumb`, or with `NO_COLOR` — asserted both ways.

---

## 7. Defaults and flags

- `--yes` — this screen.
- interactive — the plan first, then the question, then this screen. The plan is
  the consent, and it scrolls into scrollback when the region takes the screen.
- `--verbose` — everything today's run shows: the full plan and every step's
  detail, appended as it happens, no live screen.
- `--quiet` — step lines only, no animation.

---

## 8. What is asserted

`scripts/test_install.sh`, **56 assertions**, count asserted. New here: both
columns at exactly 80 on a first install; no region row over 80; the status
token rather than the sentence, with the sentence in the log; 79 columns starts
no region; 17 rows starts no region; a failed step resolving to text with the
log path; Ctrl-C resolving to text with the reason; the window with its count
when 30 steps meet a 22-row terminal; colour on a terminal and off under
`NO_COLOR`.

Not asserted here, by nature: a genuine first install on a machine with none of
it present. That is the VM arm, `docs/install-acceptance.md`.
