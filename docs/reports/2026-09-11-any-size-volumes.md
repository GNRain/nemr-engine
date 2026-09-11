# Any size, end to end — SPEC 1.153

**Date:** 2026-09-11 · **Branch:** `arbitrary-volume-size` (stacked on `one-voice-and-one-file`, PR #94)

The three presets are gone. A session's quota is any whole number of bytes
between **64 MiB** and **1 TiB**, and the privileged helper is the thing that
decides — not the CLI, not the engine, not the page.

This report is in the order the work was asked for, and ends with the two
things that could not be run on this host and the one command that unblocks
them.

---

## 1. The privileged helper

`VALID_SIZES` was three words. It is now a range, and that changes what the
helper is: a root program that parses attacker-controlled numeric input.

### The parser accepts one thing

`parse_size_bytes` accepts **ASCII digits and nothing else**. Not "digits after
trimming"; not `u64::from_str`, which accepts a leading `+`. Everything a looser
parser would have to reason about is refused by construction:

| refused | why it would otherwise slip through |
|---|---|
| `+67108864` | `u64::from_str` accepts a leading plus |
| `-1` | a sign is not a digit |
| `0x4000000`, `0o…`, `0b1` | no radix exists here |
| ` 67108864`, `67108864\n` | nothing is trimmed, so padding cannot smuggle a value past a bound |
| `64MB` | units are the CLI's vocabulary, not this program's |
| `67108864.0`, `6.7e7` | a point and an exponent are not digits |
| `6_7108864` | a separator is not a digit |
| `٦٧١٠٨٨٦٤`, `２００…` | `is_ascii_digit`, not `is_numeric` |
| `18446744073709551616` | `checked_mul`/`checked_add` per digit — nothing wraps into a small in-range number |
| 21+ digits | refused before the accumulate loop rather than by it |

Bounds come after the parse. **Nothing derived from the value exists before
both succeed** — no syscall, no path, no allocation.

Run against the built binary, each exiting non-zero with the reason:

```
+67108864     size "+67108864" contains '+'; a size is a whole number of bytes, digits only
0x4000000     size "0x4000000" contains 'x'; …
64MB          size "64MB" contains 'M'; …
1             size 1 is below the minimum 67108864 (64 MiB): ext4 overhead would leave almost nothing usable
1099511627777 size … is above the maximum 1099511627776 (1024 GiB): …
```

### MIN = 64 MiB, and it was measured twice

**First: what a session actually puts in its volume. Nothing.** Three freshly
created 500MB volumes on this host are byte-for-byte identical to a bare
`mkfs.ext4` of the same size — 50388992 bytes in use, all of it ext4's own
metadata. The base image lives in containerd's store; the volume is only
`/workspace`. So the working set a new volume must hold is **zero**, and the
brief's "base image's working set plus ext4 overhead" is, measured, just the
overhead.

**Second: what ext4 costs at small sizes.** `mkfs.ext4`, then `dumpe2fs`, usable
= (free − reserved) × block size:

| asked | usable | overhead |
|---|---|---|
| 16 MiB | 10653696 | 37% |
| 32 MiB | 25534464 | 24% |
| 48 MiB | 40415232 | 20% |
| **64 MiB** | **55296000** | **18%** |
| 500 MB | 447684608 | 15% |

64 MiB is where the overhead stops dominating and 52.7 MiB is left. Below it a
volume is mostly journal.

For the record, the figure the brief quoted: the 500MB preset's usable space
measured here as 473899008 by free blocks (447684608 after ext4's reserve),
against the smoke test's 473923584 — the same number, within `lost+found`.

### MAX = 1 TiB

A ceiling, not a capacity. The backing file is sparse, so an absurd size costs
no disk *until it is written to* — which is exactly why unbounded is dangerous
rather than harmless: `--size 1000000GB` would succeed quietly, format, mount,
and fail with ENOSPC in the middle of somebody's work. Free disk is checked
separately, against the real filesystem.

### `normalize`, and rounding down

The engine allocates the backing file itself (unprivileged), so it must know
the rounded length **before** it allocates. `nemr-volume normalize [bytes]`
answers that, from the program that enforces it, touching nothing:

```
$ nemr-volume normalize 67112959
min=67108864
max=1099511627776
block=4096
requested=67112959
bytes=67108864
```

Rounding is **down**, never up: rounding up hands back more than the caller
checked against free disk, which is the same class of surprise as a TOCTOU. The
block size is `f_frsize` from `fstatvfs` on a pinned descriptor — the same
symlink-refusing resolver every other path here goes through, because a
resolver used only on the dangerous paths is one somebody forgets on the next.

### `mount` now checks the size it is given

Under protocol 2 the size was a preset word used **only in the audit line** — a
caller could claim any of the three and nothing looked. A byte count can be
checked against the file about to be mounted, and is: the two must agree
exactly. That makes the audit line a statement about the mount.

---

## 2. A version check that was only ever a comment

The helper's source has said since protocol 2:

> The engine checks this before invoking a privileged operation and refuses to
> run against a helper whose protocol it does not understand.

**It did not.** Nothing in the engine ever read that number; the only reader was
`setup_test_host.sh`, printing it for a human. A host running a stale helper
found out through whatever the first mismatched argument happened to do.

Protocol 3 changes the size argument from a word to a number, which is exactly
the kind of change that must not be discovered that way. So the claim is now
true, and it was driven end to end on this host — a protocol-2 helper with a
protocol-3 engine:

```
$ nemr create sizetest --size 777MB
XX the privileged helper speaks protocol 2; this nemr speaks 3.
              The helper is root-owned and is not updated by installing nemr.
              Reinstall it: sudo ./scripts/setup_test_host.sh
```

`setup_test_host.sh` also refuses an install whose installed binary does not
report the protocol its own source declares.

---

## 3. Engine and wire

`VolumeSize` is a `u64` of bytes. It parses `2GB`, `1536MB`, `67108864B` and a
bare byte count, kibibyte-based **as the presets always were** — an SI megabyte
here would silently resize every existing project's recorded quota on the next
parse. `Display` round-trips for every value (whole GB, else whole MB, else
bytes), because this is what the bundle manifest records and what the daemon
puts on the wire; a size that printed as something else would come back a
different volume on import. Asserted for nine values including `2GB + 4096`.

**The bounds are not duplicated.** They live in the helper and reach everything
else through `PrivilegedOps::size_bounds()`, parsed strictly: a helper that does
not report one of them is an error naming the reinstall, never a default. A
default there would be the engine quietly deciding a bound it does not enforce.

`read_recorded_size` no longer matches the file length against three presets and
calls anything else unknown. The length **is** the size.

### The wire change, and which kind it is

`CreateRequest.size` stays a `string` and is **unchanged** — `"2GB"` still means
the same volume — so the message is compatible in both directions. What widened
is what the engine accepts. The **additive** part is the new `SizeLimits` RPC,
and that is the part an older daemon answers with `UNIMPLEMENTED`. So the
protocol goes **3 → 4**: the handshake is what turns that into a refusal naming
the fix, which is the only reason it exists.

The default for an empty size moved into the daemon. The page used to fill in
`"2GB"` itself; two callers filling in their own default is two defaults.

---

## 4. The CLI

The daemon is consulted **before any question is asked**, because the bounds
come from the helper behind it and the free space from `statvfs`. Asking "how
much?" before those are in hand is asking a question whose answer cannot be
checked.

```
Project name: thing
You'll choose how much storage this session gets. Pick a unit
  ❯ MB
    GB
              1 to 219 GB — 214.7GiB free on this disk
How much? [2]
```

A number out of range is refused **with the figure**, and asked again:

```
✗ 500.0GiB is more than the 214.7GiB free on this disk
How much?
```

`--size` is unchanged in form, still wins in every mode, and is checked the same
way — a flag has nobody to ask again, so it is refused with the same figure.
Without a terminal it is now **required and named**, instead of quietly becoming
2GB. A scripted create that silently picks a quota is the invisible default this
project refuses everywhere else. (This is a behaviour change for a scripted
caller that omitted `--size`; nothing in this repo does.)

---

## 5. The page

The picker was a three-item `<select>` filled from three strings in `serve.rs`.
It is now a slider, and **every number in it is the daemon's**: the ends and the
block step from the helper, free space from `statvfs`, the default from the
engine.

- **Logarithmic.** The range spans four orders of magnitude; a linear slider
  would put every size anybody picks inside the first two pixels.
- **Snapped to whole blocks**, so every position it can produce is a size the
  helper accepts.
- **A number and a unit beside it** — the same two questions the CLI asks, in
  the same order — taken exactly, so a value between slider steps survives.
- **Marks** for the three old sizes and for free disk, clickable.
- **Past free disk** the figure turns and the submit is disabled.
- **A silent daemon** means the panel says it cannot offer sizes. It does not
  fall back to a range of its own, which would look like it works.

### The drift control got stronger

It used to compare the page's three strings against the CLI's `Valid sizes:`
message. A range cannot be kept in step by enumeration, so instead of weakening
the control it moved closer to the truth: it asks **`nemr-volume normalize`
directly** and requires the page's min, max and block to be that program's own
numbers — the program that refuses an out-of-range size. The free-disk figure is
checked against `statvfs` on the volumes directory.

The browser acceptance drives the real slider: both ends must land exactly on
the helper's bounds, every position it can produce (walked, not sampled) must be
a whole block in range, a typed value must survive un-snapped, and a size past
free disk must name the figure and disable the submit.

---

## 6. Auto-scale: measured, not shipped

Full measurements in `docs/volume-autoscale-spike.md`. In short:

- **The layout supports it.** ext4 here reserves GDT blocks for roughly 1000×
  online growth at every size nemr creates — 57 GiB from a 64 MiB volume, 497
  GiB from 500 MB, 2.0 TiB from 2 GB.
- **A half-completed grow is benign.** File grown, filesystem not: `e2fsck`
  clean, old size intact, and re-running the last step finishes it. The recovery
  path is the operation itself; it is idempotent because every step is "make
  sure this is at least N".
- **The dangerous order cannot happen** through a loop device: the device's
  capacity is whatever `LOOP_SET_CAPACITY` last set, so `resize2fs` cannot
  outrun it.

**What is unmeasured:** whether `resize2fs` succeeds online through a loop
device, and whether `losetup --set-capacity` works while a container holds the
mount. Both need root. The only privilege this account has is PRIV-03's grant —
one binary, one path, no argument wildcards — and that binary has no grow verb;
general `sudo` asks for a password.

So nothing ships. The brief's rule decides it: *a "Yes (recommended)" that
silently does nothing is worse than no option.* The spike document carries the
exact commands that would settle it in about two minutes on a host with root.

Worth saying: widening the range removes most of the reason anyone wanted to
grow a volume. The old answer to "500MB is too small and 2GB is too much" was to
pick wrong and live with it.

---

## 7. What was run here, and what was not

| | |
|---|---|
| helper unit tests | **23, green** — every malformed and out-of-range case through the helper's own parser |
| helper neuter | **red** — `parse_size_bytes` replaced by `size.trim().parse::<u64>()` fails both boundary tests |
| helper binary, driven directly | **green** — every refusal above, the two ends, and rounding |
| engine unit tests | **165, green** |
| `nemr-cloud` tests | **28 + 11, green** |
| `cargo fmt --all --check`, `clippy` (both crates) | **clean, zero warnings** |
| `scripts/check_seam.sh` | **green** |
| the stale-helper gate, end to end | **green** — driven against a private daemon from this build |
| `scripts/volume_size_acceptance.sh` (24 assertions) | **NOT RUN** |
| `docs/ui-acceptance.sh` / `.py` (the slider) | **NOT RUN** |

The last two need the protocol-3 helper installed, and installing it needs root:

```sh
sudo ./scripts/setup_test_host.sh
```

Until that runs, **every `nemr create` on this host refuses** — correctly, and
with the reinstall named. That is the gate working, not a regression, and it is
the sanctioned redeploy.

### Deliberate changes to things that were parsed

- The `serve.rs` test asserting `7GB` is refused now asserts **`5000GB`** is
  refused and 7GB goes through. That is the revision.
- `docs/ui-acceptance.sh`'s picker control no longer compares size lists; it
  compares the page's range against the helper's own. Stronger, not weaker.
- `docs/ui-acceptance.py` reads the hidden `createsize` field (a byte count)
  instead of `<select>` options.
