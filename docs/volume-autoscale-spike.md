# Growing a volume after creation — measured, and the decision

**Date:** 2026-09-11 · **SPEC 1.153, step 5** · **Outcome: NOT SHIPPED**

The instruction was "measure, then decide. Do not build the prompt first", and
the fallback was stated: *"If it is not [safe], say so plainly and ship without
it — a 'Yes (recommended)' that silently does nothing is worse than no
option."* This is that measurement and that decision.

**The decision: no auto-scale prompt ships in 1.153.** Not because growth looks
unsafe — everything measurable points the other way — but because the two steps
that decide it need root on a host where I have no general `sudo`, and shipping
a "Yes (recommended)" I could not demonstrate is the exact thing the brief
forbids.

---

## What the operation would be

A quota is a sparse file, a loop device, and an ext4 filesystem. Growing it is
three steps, in this order and no other:

1. `fallocate`/`set_len` the backing file to the new size — unprivileged, the
   engine already does this at create.
2. `losetup --set-capacity` (`LOOP_SET_CAPACITY`) so the loop device sees the
   longer file — **root**.
3. `resize2fs` on the mounted filesystem — **root**.

Each step only makes more room available to the next. The filesystem's own size
changes last, which is what makes the ordering interesting.

---

## What was measured

### 1. ext4 here can be grown, and the layout reserves room for it

`mkfs.ext4` on this host produces `resize_inode` and reserved GDT blocks. Those
blocks are the online-growth budget: each one describes another
`4096/64 × 32768` blocks — 8 GiB — of future filesystem.

| created at | blocks | reserved GDT | ceiling those blocks reach | headroom |
|---|---|---|---|---|
| 64 MiB | 16384 | 7 | 57 GiB | ~900× |
| 500 MB | 128000 | 62 | 497 GiB | ~1017× |
| 2 GB | 524288 | 255 | 2.0 TiB | ~1021× |
| 10 GB | 2621440 | 1024 | 8.1 TiB | ~820× |

So the layout is not the constraint: every size nemr can create reserves room
to grow roughly a thousandfold, past MAX in every case but the smallest — and
64 MiB reaches 57 GiB, which is far past anything a session at that size would
want.

### 2. A grow that stops half way is benign, and finishing it later works

The failure that matters is "the backing file grew, the filesystem did not" —
the state a crash between steps 1/2 and step 3 leaves. Measured directly, on an
image file:

```
64 MiB ext4, a directory written into it, file truncated up to 256 MiB:
  Filesystem state:  clean
  Block count:       16384          <- unchanged, the old size
  e2fsck -fn:        12/16384 files, 2066/16384 blocks   (clean)
  resize2fs later:   Block count: 65536                  (completes)
```

The filesystem does not notice and does not care. It keeps its old size, passes
a full `e2fsck`, mounts, and the grow can be finished at any later moment by
re-running the last step. Nothing has to be undone, and the extra file length is
sparse, so the half state costs no disk either.

**That is the recovery path, and it is the same command as the operation:** run
the grow again. It is idempotent by construction, because every step is "make
sure this is at least N".

### 3. The dangerous order cannot happen through a loop device

Told to grow a filesystem past its container, `resize2fs` on a plain *file*
simply extends the file (measured: a 64 MiB image asked for 256 MiB became a
256 MiB image with a 65536-block filesystem, `e2fsck` clean). A **loop device**
is not extensible that way — its capacity is whatever `LOOP_SET_CAPACITY` last
set — so on the real path step 3 cannot outrun step 2. The order is enforced by
the device, not by the code being careful.

---

## What was NOT measured, and why

| Question | Status |
|---|---|
| Does `resize2fs` succeed **online**, on a mounted ext4, through a loop device? | **unmeasured** |
| Does `losetup --set-capacity` work while a container holds the mount? | **unmeasured** |
| What does a failure between those two leave on a live mount? | **unmeasured** (the offline analogue is §2, and it is benign) |

Both need root. The only privilege this account has is the `NOPASSWD` grant for
one binary at one fixed path with no argument wildcards (PRIV-03) — which is
the point of that design — and that binary has no grow verb. General `sudo`
asks for a password. So these three cannot be answered here, and I will not
report a guess as a measurement.

## What would settle it

On a host with root, with a session created and started:

```sh
name=growtest
img=~/.local/share/nemr/volumes/$name.img
dev=$(losetup -j "$img" | cut -d: -f1)
mnt=~/.local/share/nemr/mounts/$name

truncate -s 4G "$img"          # 1. the file
sudo losetup --set-capacity "$dev"   # 2. the device — does it work with the mount live?
sudo resize2fs "$dev"                # 3. the filesystem — online?
df -B1 "$mnt"                        # did the container's /workspace actually grow?
```

Run it with a container attached and writing, and again with the middle step
killed, to see what a live half state looks like. If all three answer yes, the
prompt is a small change: a `grow` verb on the helper taking the same bounded
byte count `mount` already takes, and the same round trip the CLI already makes
for its limits.

## What ships instead

Nothing silent. A session's quota is fixed at creation, as it was — but it is
now any size between 64 MiB and 1 TiB rather than one of three, which removes
most of the reason anyone wanted to grow one: the old answer to "500MB is too
small and 2GB is too much" was to pick wrong and live with it.
