# The first run on a clean distro: a silent installer, a half-empty window, and four pairs of temp files

2026-09-10. Three findings from a first install on a fresh WSL2 distro, plus
one question about a log file. Measured, fixed, and each fix proved red under a
neuter before it was proved green.

Reported by the Product Owner:

> The installer can produce NO output at all. First run on the clean distro:
> `./scripts/install.sh --yes`, sudo prompted for my password, and the shell
> prompt came straight back — no plan, no region, no result, no error, nothing.

---

## 1. The silent run (F-32)

### What happened, exactly

`reboot_gate` is the check that cgroup delegation has actually taken effect.
On a machine where it has not — which is every machine on its first install,
because delegation applies only after the user manager restarts — it printed a
heredoc **to stderr** and exited 3.

By then the live region owned the screen. The region's renderer erases its
block on the way out, and the erase covered the rows the heredoc had just
written. The EXIT trap then looked for a recorded failure, found none (the gate
had set no `FAILED_STEP`), and printed nothing. The log was written and the
prompt came back.

Three separate things had to be true, and all three were:

| | |
|---|---|
| The message went to stderr | the region's renderer writes to stdout and erases by rows, not by stream |
| The gate recorded no outcome | `FAILED_STEP` was empty, so the trap had nothing to print |
| The trap printed only on failure | success and "handled" were the only other cases it knew |

**It is first-run-only**, which is why it survived every acceptance run here:
on a provisioned host the gate returns early and the path is never taken.

### The class, not the instance

Any exit that happens while the region is open and has not recorded an outcome
is silent. That is not one path, it is a shape: the reboot gate, a consent
decline that exits 0, a sudo refusal, a Ctrl-C, and any exit nobody has thought
of yet — including the next one somebody adds.

### The fix: one authority, and it is the exit itself

`_steps_cleanup` in `scripts/lib/steps.sh` is now the only place that speaks.
Every exit passes through it, and it does three things in this order: stop the
region, remove this run's temp files, print **exactly one** outcome to stdout.

It dispatches on `RESULT_STATE`, which a path sets instead of printing:

| `RESULT_STATE` | What is printed |
|---|---|
| `ok` | the four-line success block |
| `failed` | the failure block: step, reason, fix, log path |
| `paused` | "Almost there — reboot, then run this again", why, and what to do |
| `handled` | nothing: an early refusal (preflight, consent) already spoke, to stdout |
| anything else | **the backstop** — "Install stopped unexpectedly", naming the log |

The backstop is the part that answers the class. An exit that records nothing
still prints something, and what it prints says plainly that it is a bug worth
reporting rather than pretending to be a diagnosis.

Ctrl-C and a sudo refusal are now recorded rather than printed at the point
they happen, for the same reason the original message was lost: anything
written while the region is open is erased by it.

### Evidence: every exit path, and what reached stdout

Driven for real, stderr diverted to a file so the transcript **is** stdout.
The region-active paths run under a 100x30 pty and are rendered back to the
screen they produced.

| Exit path | rc | stdout | What it says |
|---|---|---|---|
| `--help` | 0 | 685 B | the usage block |
| unknown option | 2 | 721 B | `install.sh: unknown option --bogus` |
| preflight refusal | 1 | 384 B | "nemr cannot be installed on this machine yet — nothing has been changed" |
| no terminal, no `--yes` | 2 | 4177 B | the whole plan, then "Refusing to go ahead without asking" |
| declined at the prompt | 0 | — | "Nothing was changed" |
| **reboot gate** | 3 | 466 B | "Almost there — reboot, then run this again" |
| step failure | 1 | 746 B | "Install stopped", the step, the reason, the fix, the log |
| success | 0 | 710 B | "nemr is installed", version, path, what to try |
| **unmodelled exit** | 1 | 655 B | "Install stopped unexpectedly", and the log path |
| sudo refused | 1 | — | "Failed at: asking for your password" |
| Ctrl-C | 130 | — | "Because: you pressed Ctrl-C", after the region is gone |

The reboot gate, on the screen it now produces:

```
!! Almost there — reboot, then run this again.

      Why:   cgroup delegation applies only after the user manager restarts
      Do:    sudo reboot
             then ./scripts/install.sh again

    It skips everything done so far and carries on from here.
```

And the backstop, for an exit that recorded nothing at all:

```
XX Install stopped unexpectedly.

    It ended on a path it has no message for. This is a bug worth
    reporting; the full log has what happened:

      ~/.local/state/nemr/install-20260910-190819.log
```

### Asserted

`scripts/test_install.sh` drives all eleven paths and asks two questions of
each: **is stdout non-empty and is the screen it leaves not blank**, and does
it say the right thing. Both halves of the first, because under a live region
stdout is never literally empty — the renderer writes thousands of bytes and
erases them, which is how the reported run was silent with a busy stdout. One
more assertion covers the authority being single: every result path prints
exactly one outcome block, never two.

Three test-only seams were added to make the unreachable reachable:
`NEMR_TEST_FORCE_REBOOT_GATE` takes the gate on a machine where delegation is
already live, `NEMR_TEST_FORCE_ABRUPT` exits with nothing recorded while the
region is open — the shape of a path nobody wired — and
`NEMR_TEST_FORCE_SUDO_ASK` drives the password prompt so a refusing `sudo`
shim on `PATH` can exercise the refusal. The real `sudo` is never touched.

---

## 2. The window is not the layout (F-33)

> The cat should sit against the RIGHT edge of the terminal whatever the width,
> and the output pane should take the space that leaves — wider window, wider
> pane. The pane needs a minimum width below which the region falls back as
> now; state it. Also state whether a resize mid-run is supported, or
> explicitly not.

The layout was fixed at 79 columns wherever it ran, so a 132-column window drew
the same narrow block with 53 columns of nothing to the right of the cat.

It is now measured once and fitted: **left column = cols − 2 (gap) − 37 (cat)**.

| Terminal | Left column | Pane inner | Widest row drawn |
|---|---|---|---|
| 79 | 40 | 34 | 79 |
| 80 | 41 | 35 | 80 |
| 100 | 61 | 55 | 100 |
| 132 | 93 | 87 | 132 |
| 160 | 121 | 115 | 160 |

The widest row equals the terminal width at every size, which is the cat
sitting on the right edge; nothing is drawn past it, so no row wraps.

**The minimum pane width is 34 columns of pane inner (40 of left column).**
That is `  installing the privileged helper`, the longest step phrase, inside
its border. Below it the column cannot hold both a phrase and useful output, so
the region is **never started**: under **79 columns** (40 + 2 + 37), or under
**18 rows**, the run falls back to append-only printing exactly as it did
before there was a screen, and the log records the refusal by name:

```
--- install screen
    live:      NO — 78 columns, the screen needs 79
```

**A resize mid-run is explicitly NOT supported.** *(Superseded on 2026-09-11:
the Product Owner revisited this ruling, and a resize is now handled — see
`2026-09-11-install-resize.md`. The rest of this section stands as it was
measured.)* The width is read once, when
the region opens, and the block keeps it until the run ends. Redrawing at a new
width means rewriting rows already written at the old one, and a region that
tears while a window is dragged is worse than one that keeps its shape.
Narrowing the window mid-run therefore wraps the live block's rows until the
run finishes; the result printed afterwards is ordinary text and reflows like
anything else. The log now states the policy rather than leaving it to be
discovered:

```
    fitted:    left 61 + gap 2 + cat 37 (right edge)   pane inner 55
    resize:    not supported mid-run — the width is read once, at the start
```

### Found while measuring: the renderer forked about fifty times a frame

Each of the pane's five lines was computed by reading and cleaning the **whole**
tail file — a `grep | tail | sed | tr` pipeline per row, five times a frame —
and each of the fifteen rows measured its own visible width with another `sed`,
inside another command substitution. The cat's frames were re-split from text
every frame, and the progress bar was built in a subshell.

Now: the pane is cleaned once a frame into an array, the plain text is kept
beside the styled text instead of being stripped back out of it, the frames are
split once at start, and the bar and the left column are built with `printf -v`.
**About fifty processes a frame became five.** Measured on a deliberately slow
harness here — a six-second step with output arriving throughout:

| | Frames drawn | Wall clock |
|---|---|---|
| before | 2 | 16.2 s |
| after | 15 | 10.3 s |

This sandbox forks in ~25 ms where an ordinary machine takes ~1 ms, so the
absolute numbers are its own; the ratio is the point. The acceptance no longer
depends on how fast the machine is either — it reads the screen at a **frame
boundary** rather than at an arbitrary byte offset, which is a new `--frame`
option on the test-only renderer.

---

## 3. The temp files (F-34)

> After four runs `~/.local/state/nemr/` contains region.409, region.135604,
> step.409 and step.135604 — one pair per run, never cleaned.

They were removed in the success block only, so every run that ended any other
way left its pair behind. They are now removed at the **same single exit point
that prints the outcome**, which means success, failure, the reboot gate, a
sudo refusal, an unmodelled exit and Ctrl-C all get it.

A `kill -9` cannot run a trap, so the pair it leaves is swept by the **next**
run — and only after checking the pid is really gone:

```bash
for _stray in "$LOG_DIR"/region.* "$LOG_DIR"/step.*; do
    [[ -e "$_stray" ]] || continue
    _spid="${_stray##*.}"
    [[ "$_spid" =~ ^[0-9]+$ ]] && ! kill -0 "$_spid" 2>/dev/null && rm -f "$_stray"
done
```

Asserted both ways, because cleanup that cannot tell the difference would
delete a concurrent run's state file out from under it: a dead pid's pair is
swept, and **a live pid's pair is left alone**.

---

## 4. Found while measuring: three instruments that could only give one answer

Every run writes an `--- install screen` block to its log, added after the last
report so that "the screen did not render on my machine" could be answered by
reading the log the reporter already has. Its first line is the decision:

```
--- install screen
    live:      NO — not-a-tty (output is redirected or piped)
```

**It said exactly that on every run ever recorded — 399 logs on this machine,
not one of them saying `yes`** — including the runs whose screens are captured
in this report.

The block re-asked the question inside its own `{ ... } >>"$LOG"` redirect.
Inside that redirect stdout is the log file, so `[[ -t 1 ]]` is false by
construction and the answer can only ever be "not-a-tty". The `terminal:`
line's `tty=` field had the same fault.

The decision is now made once, before the block, on the real stdout, and the
block reports what was decided rather than re-deciding it. The same value is
what actually starts the region, so the log cannot disagree with the screen:

```
    live:      yes
    measured:  100 cols x 30 rows   by: stty /dev/tty
    needs:     79 cols x 18 rows
    fitted:    left 61 + gap 2 + cat 37 (right edge)   pane inner 55
    resize:    not supported mid-run — the width is read once, at the start
```

and at 70 columns, on the same terminal:

```
    live:      NO — 70 columns, the screen needs 79
```

### The same fault, one line lower: `tty=`

The block's `terminal:` line reported `tty=no` on runs that plainly had a
terminal, because it asked like this:

```bash
_screen_tty="$([[ -t 1 ]] && echo yes || echo no)"
```

Inside `$( )` stdout is a pipe by definition, so `-t 1` is false there whatever
the real stdout is. It is now asked with a plain `if`, outside any substitution.

### And the interrupt test that could not deliver an interrupt

`scripts/test_install.sh` has asserted since the animation was written that an
interrupt gives the cursor back and leaves no drawing process behind. It sent
the signal like this:

```bash
script -qec './scripts/lib/cat.sh --demo 20' /dev/null >"$WORK/int.raw" 2>&1 &
sp=$!
...
kill -INT -- "-$pg"
```

**Bash sets SIGINT to `SIG_IGN` in a child it starts in the background when job
control is off**, and an ignore inherited at entry cannot be trapped or reset —
so the demo never saw the signal. It ran all twenty iterations, exited
normally, restored the cursor on its way out, and the assertion went green for
a reason that has nothing to do with interrupts. The same shape made the new
Ctrl-C assertion fail: the installer carried on and exited 0.

Both harnesses now run the capture under `set -m`, which gives the job its own
process group and the default disposition, and the demo's exit status is
asserted to be non-zero — a run that finished on its own can no longer pass as
a run that was interrupted.

All three are the class the twelve silent refusals belong to: an instrument
that cannot report anything but one answer, read as if it could.

---

## 5. `nemrd.log` in the same directory (answered)

> nemrd.log sits in the same directory as the install logs. Say whether that is
> intended; it is the daemon's, not the installer's.

**It is intended, and it is not the installer's.** `~/.local/state/nemr/` is
the product's XDG state directory, not the installer's — the installer is a
guest there. `nemrd.log` is the daemon's audit trail, written by the daemon
(`crates/nemr-daemon-api/src/client.rs` opens it when the CLI autostarts one),
and its location is fixed by **SPEC 1.69**: the privileged-operation audit
trail — every helper call, every mount — goes to `$XDG_STATE_HOME/nemr/nemrd.log`,
or to the journal when systemd manages the daemon.

The installer never writes it and never reads it. The two are told apart by
name: `install-YYYYMMDD-HHMMSS.log` is one run of the installer, `nemrd.log` is
the daemon's running trail. No change recommended.

---

## Proved red before it was proved green

Each new guard was run with the code it guards disabled, and each failed. The
neuters were applied to a copy-restored working tree, one at a time:

| Neuter | The guard's answer |
|---|---|
| `_steps_cleanup`'s dispatch replaced by `:` | RED — the paused run leaves a blank screen |
| `_nemr_region_fit` pinned to the minimum | RED — the widest row at 132 columns is 79 |
| `steps_tempclean` returns without removing anything | RED — `step.117270` left behind |
| the screen decision moved back inside the log's redirect | RED — the log says `NO — not-a-tty` on a run that drew |
| the interrupt capture started without `set -m` | RED — the demo exits 0, having run to the end |

A note on the first one, because it changed the assertion. Under a live region
stdout is **never** literally empty: the renderer writes thousands of bytes and
then erases them, which is exactly how the reported run managed to be silent
with a busy stdout. So the assertion is both halves at once — bytes on stdout
**and** something left on the screen the transcript renders to. Asserting only
the first would have passed with the authority disabled.

## What is asserted now

`scripts/test_install.sh` — **117 assertions, all green**, with the count
itself asserted so a skipped case cannot pass as green.

Run it with `./scripts/test_install.sh`. The first run on a machine with none
of nemr present is still not covered here, by nature; that arm is
`docs/install-acceptance.md`, on a VM you can snapshot.
