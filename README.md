# Nemr — Engine (Phase 1)

Backend engine that provisions isolated, resource-bounded, pre-configured
Claude Code execution environments on a single Linux host, with no dependency
on Docker at any layer.

Authoritative specification: [`SPEC.md`](SPEC.md) (NEMR-SPEC-001 v1.20),
tracked in this repository per Section 4A.5. Where this README and the
specification disagree, the specification governs.

## Status

| Milestone | State |
|---|---|
| M1 — Containerd connectivity + wrapper foundation | Complete — AC-1.1, AC-1.2, AC-1.3 met |
| M2 — Base image build | Complete — AC-2.1, AC-2.2, AC-2.3 met |
| M3 — Volume creation with quota | Complete — AC-3.1 … AC-3.5 met |
| M4 — Project lifecycle: create | Complete — AC-4.1, AC-4.2 met |
| M5 — Start / attach / stop | Complete — AC-5.1, AC-5.2, AC-5.3 met |
| M6 — List / delete | Complete — AC-6.1, AC-6.2 met |
| M7 — End-to-end validation | Not started |

The host is fully provisioned per `PREREQUISITES.md`, including cgroup v2
controller delegation (Step 2a). No blockers outstanding.

## Architecture

Rust engine → `containerd-client` (gRPC) → **rootless** containerd daemon → runc.

The engine never invokes runc directly (Section 3.1) and never invokes
`containerd-client` directly outside the wrapper layer (Section 3.2).

### Privilege model (Section 3.7)

The engine talks to a **rootless** containerd owned by the invoking user
(PRIV-01) — not the root-owned system service that the `containerd` package
enables. Container operations in Milestones 1, 4, 5, and 6 require no `sudo`
and no root-equivalent group membership.

```
$ ls -l $XDG_RUNTIME_DIR/containerd/containerd.sock
srw-rw---- 1 nemr nemr /run/user/1000/containerd/containerd.sock
```

Loop-device attach and mount are the sole exception (PRIV-02), bounded by a
NOPASSWD grant to one fixed root-owned helper at `deploy/nemr-volume/`, with no
argument wildcards (PRIV-03). Sparse allocation and `mkfs.ext4` were measured to
need no privilege and are excluded from that surface. See "Volumes and the
privileged helper" below.

The root-owned system service is **disabled** on this host, so the rootless
instance is the only containerd running:

```
$ systemctl is-enabled containerd   ->  disabled
$ systemctl is-active  containerd   ->  inactive
$ ls /run/containerd/containerd.sock ->  No such file or directory
```

That is deliberate rather than incidental. Leaving it running would mean a
client that reaches `/run/containerd/containerd.sock` is silently talking to
the wrong daemon, and any acceptance criterion validated against it would be
void. Disabling it makes that mistake impossible instead of merely unlikely.

The engine reinforces this in code: socket resolution honours
`$CONTAINERD_ADDRESS`, then falls back to
`$XDG_RUNTIME_DIR/containerd/containerd.sock`, and **never** falls back to the
system path. A missing `XDG_RUNTIME_DIR` is a hard error rather than a cue to
guess.

```
src/bin/          CLI entrypoint + Milestone 1 raw connectivity baseline
src/containerd/   Wrapper layer over containerd-client  ← the only caller of the crate
src/engine/       Product logic                          ← depends only on src/containerd/
src/config.rs     Paths, defaults, constants
src/auth.rs       Claude Code credential injection (AUTH-01..03)
image/            Base image definition (Milestone 2)
scripts/          End-to-end regression test (Milestone 7)
```

### The wrapper layer

`containerd-client` provides raw gRPC/protobuf bindings. Unlike containerd's
Go client, it has no high-level operations — no single "pull this image" or
"create and start this container" call. `src/containerd/` is where that gap is
closed, once, deliberately.

Two rules hold for every milestone after this one:

1. Code in `src/engine/` must not reference `containerd_client`.
2. When a milestone needs a new containerd operation, it **extends** the
   wrapper with a general-purpose version of that operation. It does not add
   a shortcut at the call site, and it does not duplicate wrapper logic.

Milestone 1's wrapper surface is intentionally just `list_images()` and
`list_containers()`. The full API is not designed up front — it grows as
concrete needs arise (Section 4, Milestone 1 scope note).

Three design decisions taken at Milestone 1, since they set the pattern later
milestones extend:

- **The seam is enforced by visibility, not convention.** `ContainerdClient`
  owns the raw `containerd_client::Client`, exposed only via a `pub(crate)`
  accessor. Sibling modules in `src/containerd/` can build on it; `src/engine/`
  cannot reach it. Rule 1 above is therefore a compile error, not a code-review
  note.
- **Wrapper methods return projections, not protobuf types.** `ImageSummary`
  and `ContainerSummary` carry the fields the engine uses. Returning generated
  protobuf types would push the gRPC schema into every caller — the coupling
  Section 3.2 exists to prevent — and would make the wrapper a pass-through
  rather than an abstraction.
- **Listing results are sorted.** containerd guarantees no ordering. Sorting in
  the wrapper makes output deterministic, which AC-1.2's comparison depends on
  and which later milestones' assertions will too.

One dependency note worth keeping: **do not add `tonic` as a direct
dependency.** Use `containerd_client::tonic`, which the crate re-exports.
Declaring our own lets cargo resolve a second, incompatible tonic into the
graph, and the generated gRPC clients then reject our `Request` type with a
confusing "multiple different versions of crate `tonic`" error. `Cargo.toml`
carries this as a comment where the dependency would otherwise go.

### Raw baseline program

`src/bin/raw_connectivity.rs` talks to `containerd-client` directly, bypassing
the wrapper. This is intentional and is the one permitted exception to rule 1
above. It exists to establish that connectivity works *before* any abstraction
is introduced, so that AC-1.2 can compare wrapper output against a known-good
pre-abstraction baseline. If the wrapper's output ever diverges from this
program's, the wrapper is wrong.

## Build

Requires the host setup in `PREREQUISITES.md`.

```bash
cargo build
```

## Reproduction steps — Milestone 1

Verifies AC-1.1 and AC-1.2 from a clean checkout.

```bash
# 0. Host setup per PREREQUISITES.md. Confirm the rootless daemon answers and
#    the root-owned system daemon is disabled (PRIV-01):
systemctl --user is-active containerd-rootless.service   # active
systemctl is-enabled containerd                          # disabled
ls -l "$XDG_RUNTIME_DIR/containerd/containerd.sock"      # owned by you, not root

# 1. Build
cd nemr-engine
cargo build

# 2. AC-1.1 — raw connectivity baseline exits 0 (no sudo, per PRIV-01).
#    Run WITHOUT CONTAINERD_ADDRESS so default socket resolution is exercised.
env -u CONTAINERD_ADDRESS ./target/debug/raw_connectivity; echo "exit=$?"

# 3. AC-1.2 — wrapper output byte-identical to the baseline.
#    stdout carries the comparable payload; stderr carries commentary.
diff <(env -u CONTAINERD_ADDRESS ./target/debug/raw_connectivity 2>/dev/null) \
     <(env -u CONTAINERD_ADDRESS ./target/debug/wrapper_connectivity 2>/dev/null) \
  && echo "AC-1.2: identical"
```

Note the absence of `sudo` at steps 2 and 3 — that is PRIV-01 working as
intended, not an omission.

To reproduce the listing fixtures on an empty host:

```bash
export CONTAINERD_ADDRESS="$XDG_RUNTIME_DIR/containerd/containerd.sock"
ctr images pull docker.io/library/alpine:latest

# Container creation mounts client-side, so it must run inside the daemon's
# namespaces — see PREREQUISITES.md, "When nsenter IS required".
CHILD_PID=$(cat "$XDG_RUNTIME_DIR/containerd-rootless/child_pid")
nsenter -U --preserve-credentials -m -n -t "$CHILD_PID" \
    env CONTAINERD_ADDRESS=/run/containerd/containerd.sock \
    ctr container create docker.io/library/alpine:latest m1-fixture
```

### Known constraint for Milestones 4–5

Pure gRPC calls (list, pull, version) reach the rootless socket directly from
the host namespace. Operations where the **client** performs a mount —
container creation being the one that matters — must run inside rootlesskit's
user and mount namespaces, or they fail with `operation not permitted` on the
overlay mount. The engine will have to account for this when it gains
`create_container()` at Milestone 4. Recorded here so it is not rediscovered
as a surprise then.

## Base image (Milestone 2)

### Build tool: BuildKit via `buildctl`

The "standalone `buildctl`" option from Section 3.1. Not packaged for Ubuntu
22.04, so installed from upstream release v0.32.2 into `~/.local/bin` and run
as a **rootless** systemd user unit (`buildkitd-rootless.service`), mirroring
the containerd setup — no `sudo`, consistent with PRIV-01.

Chosen over `buildah`, which is in the Ubuntu archive and would have been one
`apt install`: `buildah` is outside Section 3.1's candidate list and keeps its
own image store, so images would need an extra push into containerd regardless.
`buildctl` operated correctly on first attempt, so no E-02 escalation.

BuildKit uses its own OCI worker and exports an OCI archive, which is then
imported into containerd. Keeping the two decoupled means buildkitd's lifetime
is not tied to containerd's rootlesskit child PID.

### Base image: Debian-slim (`node:22-slim`)

E-01 anticipates escalation *if Alpine exhibits musl libc problems* with
Node.js or Claude Code — Alpine is the option carrying that risk. Debian-slim
is glibc and avoids the failure mode E-01 exists to catch, so it is the initial
selection and no escalation was required. Alpine remains a size optimisation
worth revisiting once the engine works end to end.

Node 22 matches the LTS line and the host's own Node (v22.23.2).

### Measured size (AC-2.2)

| Artifact | Size |
|---|---|
| OCI archive (`/tmp/nemr-base.tar`) | 201 MB |
| **Image in containerd** (`docker.io/nemr/base:0.1.0`) | **200.5 MiB** |

Recorded per AC-2.2. This is larger than an Alpine-based equivalent would be
(~80–130 MiB); the trade is glibc compatibility against size, per E-01 above.
Worth revisiting for Phase 2 planning alongside NFR-02, per R-04.

### Build and import

```bash
export PATH="$HOME/.local/bin:$PATH"
export BUILDKIT_HOST=unix:///run/user/1000/buildkit/buildkitd.sock

buildctl build \
  --frontend dockerfile.v0 \
  --local context=image \
  --local dockerfile=image \
  --output type=oci,dest=/tmp/nemr-base.tar,name=docker.io/nemr/base:0.1.0

export CONTAINERD_ADDRESS="$XDG_RUNTIME_DIR/containerd/containerd.sock"
ctr images import /tmp/nemr-base.tar
ctr images list
```

No Docker daemon is involved at any point (AC-2.1); `docker`/`dockerd` are not
installed on the host and `docker.service` is inactive.

### Verifying the image (AC-2.3)

`ctr run` mounts client-side, so it must run inside rootlesskit's namespaces —
see the Milestone 1 note above and PREREQUISITES.md.

```bash
CHILD_PID=$(cat "$XDG_RUNTIME_DIR/containerd-rootless/child_pid")
nsenter -U --preserve-credentials -m -n -t "$CHILD_PID" \
    env CONTAINERD_ADDRESS=/run/containerd/containerd.sock \
        DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus" \
        XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" \
    ctr run --rm --runc-systemd-cgroup --cgroup "user.slice:nemr:m2verify" \
        docker.io/nemr/base:0.1.0 m2-verify \
        /bin/bash -lc 'claude --version'
```

Three requirements are doing real work in that command, each learned from a
failure rather than assumed:

1. **`nsenter`** — `ctr run` mounts client-side, so it must be inside
   rootlesskit's namespaces (Milestone 1 note above). Without it:
   `failed to mount ... fstype: overlay ... operation not permitted`.
2. **`--runc-systemd-cgroup --cgroup user.slice:...`** — rootless containerd
   inside rootlesskit still sees the host `/sys/fs/cgroup`, so runc's default
   path is unwritable. Without these:
   `mkdir /sys/fs/cgroup/default: permission denied`. The systemd cgroup driver
   places the container in a scope under the delegated user slice instead.
   `--runc-systemd-cgroup` requires `--cgroup` to be set explicitly.
3. **cgroup v2 controller delegation** (PREREQUISITES.md Step 2a). Without it,
   the scope is created but start fails on
   `.../cpu.weight: no such file or directory`, because only `memory` and
   `pids` are delegated by default.

`DBUS_SESSION_BUS_ADDRESS` and `XDG_RUNTIME_DIR` are passed through because the
systemd cgroup driver talks to the user's systemd over the session bus, and
`nsenter` does not carry them in.

## Volumes and the privileged helper (Milestone 3)

A volume is a sparse file, formatted ext4, attached to a loop device and
mounted (VOL-02). Measured split of what actually needs privilege:

| Step | Privileged? |
|---|---|
| Sparse allocation | no |
| `mkfs.ext4` | no |
| `losetup` attach/detach | **yes** |
| `mount` / `umount` | **yes** |

Formatting being unprivileged is worth noting: it is the most destructive verb
involved, and it stays off the privileged surface entirely.

### Why a helper binary rather than a sudoers command list

The intuitive rule enumerates commands with path wildcards:

```
nemr ALL=(root) NOPASSWD: /usr/bin/mount /dev/loop* /home/nemr/.local/share/nemr/mounts/*
```

That does not constrain paths. Per `sudoers(5)`, a slash **is** matched by
wildcards in command *arguments* (unlike in the command's own path), so
`mounts/*` also matches `mounts/../../../../etc` — mounting over `/etc` as
root. The rule looks narrow and is effectively passwordless root.

A path constraint therefore cannot be expressed in sudoers at all. It has to
live in code the granted user cannot modify:

```
nemr ALL=(root) NOPASSWD: /usr/local/libexec/nemr-volume
```

One fixed path, no wildcards. `deploy/nemr-volume/` takes a volume **name**
and a **size preset** — never a path, device, or UID — and derives and
validates everything internally. Zero dependencies (std only): a privileged
binary's supply chain is part of its attack surface.

Its defences: refuses to run without `SUDO_UID`/`SUDO_GID`; refuses to act for
root; names matched against `^[a-z0-9][a-z0-9-]{0,31}$`; sizes from a
three-item list; extra arguments rejected; symlinked backing files refused;
paths re-checked after canonicalisation; backing file must belong to the
invoker.

### Ownership (PRIV-06)

Mount and `chown` are one atomic operation. A freshly formatted ext4 has a
root-owned root inode, and host UID 0 is unmapped inside the rootless user
namespace, so it appears as `nobody` and the container cannot write to its own
volume. The measured mapping is:

```
inside 0 → host 1000 (count 1)
inside 1 → host 100000 (count 65536)
```

So the volume is chowned to the invoker's own UID — which appears as UID 0
inside the container. **Not** the `/etc/subuid` range, which maps to container
UID 1 and above. `chown` is deliberately not a separately invocable verb;
exposing it would permit re-owning arbitrary paths.

### Installing the helper

```bash
cd deploy/nemr-volume && cargo build --release && cd ../..
sudo install -o root -g root -m 0755 \
    deploy/nemr-volume/target/release/nemr-volume /usr/local/libexec/nemr-volume
visudo -c -f deploy/sudoers.d/nemr-volume        # validate BEFORE installing
sudo install -o root -g root -m 0440 \
    deploy/sudoers.d/nemr-volume /etc/sudoers.d/nemr-volume
```

A malformed file in `/etc/sudoers.d/` can lock every user out of sudo, hence
the `visudo -c` step. Verify afterwards that the helper is `root:root 755` — if
the invoking user can write it, the grant becomes unrestricted root.

### Running the volume tests

Unit tests are hermetic. The integration tests mount real filesystems, so they
are `#[ignore]`d and need the helper installed:

```bash
cargo test --lib                                              # 7 hermetic
cargo test --lib -- --ignored --nocapture --test-threads=1    # 4 integration
(cd deploy/nemr-volume && cargo test)                        # 6 helper
```

`--test-threads=1` is required: these attach loop devices and assert on global
host state.

Verify independently afterwards — the tests assert, but the host is the
authority:

```bash
losetup -a | grep -i "nemr\|deleted"      # expect no output
grep nemr /proc/self/mountinfo            # expect no output
```

That second check is not decorative. An early version of AC-3.4 passed while
leaking a loop device, because the test probed by path and a device whose
backing file has been unlinked no longer matches its path — `losetup -a`
reports it as `(deleted)`. The residue check now scans the whole table, and
`Volume::create` releases privileged resources *before* unlinking the backing
file, since unlinking first makes the device unfindable and permanently
stranded.

## Projects (Milestone 4)

`nemr create <name> --size <500MB|2GB|10GB>` provisions a project: a
quota-bounded volume (Milestone 3) plus a container from the base image
(Milestone 2), in a stopped, ready-to-start state.

```bash
nemr create myproject --size 2GB
```

### Discoverability

State lives in containerd, not in a side database the engine would have to keep
in sync. The container record carries labels:

```
nemr.project = myproject
nemr.size    = 2GB
nemr.volume  = /home/nemr/.local/share/nemr/mounts/myproject
```

so `ctr containers info nemr-<name>` is the source of truth, and Milestone 6's
`list` reads it back rather than tracking projects separately.

### Mounts

| Host | Container | Mode |
|---|---|---|
| `~/.local/share/nemr/mounts/<name>` | `/workspace` | rw |
| `~/.claude/.credentials.json` | `/root/.claude/.credentials.json` | **ro** |

Only the credentials *file* is mounted, never the whole `~/.claude` directory
(AUTH-02). Mounting the directory would expose every project's history to every
container and would break Claude Code anyway, since the mount is read-only and
Claude Code writes session state there.

### Ordering, and why it is what it is

Creation validates the name, rejects a duplicate, then resolves credentials —
all before allocating storage, since there is no point provisioning a volume for
a container that could not authenticate. If container creation fails, the volume
guard's `Drop` releases the mount and loop device and the backing file is
removed. Only once the container exists does the volume `persist()`, because the
container now depends on it.

### The engine does not need nsenter

`ctr container create` must run inside rootlesskit's namespaces because **`ctr`**
mounts the image snapshot client-side to read the image config. The engine does
not: it reads that config from the content store over gRPC and has containerd
prepare the snapshot server-side, so nothing is mounted in the engine's own
namespace. `nemr create` runs from the host namespace with no `nsenter` and no
`sudo`.

This does not extend to Milestone 5 — starting a task runs runc, which does
mount, and carries its own cgroup constraint.

## Project lifecycle (Milestone 5)

```bash
nemr start myproject      # start the container
nemr attach myproject     # interactive shell inside it
nemr stop myproject       # stop it; the volume and container record survive
```

### Process model (Section 3.8)

PID 1 is a **supervisor** (`sleep infinity`), not a shell. `attach` is a task
exec with its own TTY, one per call. This decouples two things that would
otherwise be tangled: the project stays alive independently of any session, and
concurrent attaches get separate terminals instead of fighting over one PTY.

The supervisor command is written explicitly into the runtime spec, never
inherited from the base image, so a change to `image/Dockerfile` cannot
silently alter the process model.

Two consequences worth knowing:

- **`stop` escalates to SIGKILL.** Per `pid_namespaces(7)` the kernel delivers a
  signal to a namespace's PID 1 only if that process installed a handler —
  SIGKILL and SIGSTOP from an ancestor namespace excepted. `sleep infinity`
  installs none, so SIGTERM against it is silently discarded and waiting for
  exit blocks forever. `stop` sends SIGTERM, waits 5s, then SIGKILL.
- **Exiting an attach does not stop the project.** That is PROC-02 working;
  use `nemr stop`.

### Attach internals

`src/engine/tty.rs` handles the terminal side. Three details it exists for:

- **Raw mode**, as an RAII guard. Without it Ctrl-C kills `nemr` rather than the
  process inside. `Drop` restores the original `termios` on every path, since a
  terminal left raw looks broken to the user and needs a blind `reset`.
- **`O_RDWR` FIFO opens.** A FIFO opened read-only blocks until a writer
  appears, write-only until a reader does; with both ends opened by different
  processes at unpredictable times, either ordering can deadlock. `O_RDWR` never
  blocks on Linux.
- **SIGWINCH forwarding**, or the process keeps believing the terminal is
  whatever size it was at start.

### Networking (Section 3.9)

Containers share rootlesskit's network namespace — equivalent to
`nerdctl run --net=host` under rootless. A container given its *own* network
namespace receives an empty one (loopback only, no egress), which surfaced as
Claude Code failing with `ENOTIMP` against `api.anthropic.com`.

There is therefore **no per-project network isolation in Phase 1**, and two
projects binding the same port will collide (risk R-07). Storage isolation is
unaffected. The Phase 2 path is rootless CNI.

## Listing and deleting (Milestone 6)

```bash
nemr list                 # all projects: status, usage vs quota, volume path
nemr delete <name>        # prompts for the project name to confirm
nemr delete <name> --yes  # non-interactive, for scripts
```

`list` reads state from containerd — the `nemr.*` labels written at create time
are the source of truth, so there is no engine-side database to fall out of step
with reality. Only containers carrying a `nemr.project` label are reported;
unrelated containers in the namespace are not.

### Usage figures follow `df`'s definitions

Two `statvfs` subtleties, both of which produced plausible-but-wrong numbers
before being caught by cross-checking against `df`:

- **Used is `total - f_bfree`, not `total - f_bavail`.** The difference is
  ext4's root reserve (5% by default), which `df` counts as neither used nor
  available. Using `f_bavail` reported 190MiB/10% where `df` said 73M/4%.
- **Percentage is `used / (used + available)`**, which is what `df` does, not
  `used / total`.

`usage()` also checks `/proc/self/mountinfo` before trusting `statvfs`, because
`statvfs` succeeds on a directory that is not a mount point and silently reports
the filesystem underneath it. An unmounted volume once showed as "30.6GiB used
of 2GB" — that was the host root disk.

### Deletion order

The reverse of creation: stop the task, remove the container record and
snapshot, unmount and detach, then remove the backing file. Releasing the volume
first would pull the mount from under a container still referencing it; removing
the backing file before detaching would strand the loop device permanently,
which is the bug Milestone 3 hit.

### Surviving a host reboot (VOL-06)

Container records live in containerd's database and survive a reboot. Mounts and
loop devices do not. `start` therefore checks `/proc/self/mountinfo` and
remounts the volume if it is missing:

```
$ nemr start myproject
[nemr:volume] volume for "myproject" is not mounted at …/mounts/myproject; remounting (VOL-06)
[nemr:volume] ELEVATED: sudo -n /usr/local/libexec/nemr-volume mount myproject 2GB
started project "myproject"
```

Without this, `start` succeeded against an unmounted volume and the container
got an empty `/workspace` backed by the **host root filesystem** — no data, and
no quota (98G where 2GB was promised). Nothing errored, so a user could work an
entire session believing they were writing to their project. That is a silent
VOL-05 violation, which is why `start` remounts rather than merely warning.

Remount is chosen over refuse-and-repair-by-hand deliberately: the backing file
is intact and the helper already knows how to mount it, so requiring manual
intervention would defeat the portability the product exists for. Failure to
remount *is* fatal — proceeding is the thing being guarded against.

Regression tests simulate the post-reboot state (unmount + detach via the same
helper) rather than requiring an actual reboot, and assert the *same* filesystem
returns by checking a marker file written beforehand.

## Installing the CLI

```bash
cargo install --path . --bin nemr --root ~/.local --force
```

Installs a release binary to `~/.local/bin/nemr`. It is a snapshot, not a
symlink into `target/`, so rerun it after changing engine code.

## Current blockers

None for Milestone 1. E-03 is resolved by Section 3.7 of the specification;
the host is provisioned and rootless containerd is verified reachable.

## Deviation log

Recorded deviations from NEMR-SPEC-001 live in **[`SPEC.md`](SPEC.md),
Section 11** — the single source of truth, per Section 8 and 4A.5. They are
deliberately not reproduced here; a second copy would drift.

At the M1 baseline they are all structural additions required by the Rust
module system (`src/lib.rs`, the two `mod.rs` files, the two `src/bin/`
programs) plus one documentation-coverage note. None changes the layering the
specification mandates.

## End-to-end regression test (Milestone 7, AC-7.2)

`scripts/e2e_smoke_test.sh` is the **standing regression test for all future
engine changes**. Run it before merging anything that touches the engine, the
privileged helper, or the base image:

```bash
export CONTAINERD_ADDRESS="$XDG_RUNTIME_DIR/containerd/containerd.sock"
./scripts/e2e_smoke_test.sh                  # full run, including the API round-trip
NEMR_SKIP_API=1 ./scripts/e2e_smoke_test.sh  # skip the API round-trip
```

It drives the complete lifecycle non-interactively — create, start, attach with
a scripted Claude Code invocation, stop, restart, list, delete — plus reboot
survival (VOL-06) and a host-cleanliness sweep. Exit codes: `0` all assertions
passed, `1` an assertion failed, `2` prerequisites missing (see
[PREREQUISITES.md](PREREQUISITES.md)).

It deliberately asserts on **host** state — loop devices, `/proc/self/mountinfo`
entries, backing files, systemd scopes — rather than only on what the engine
reports. The engine agreeing with itself is not evidence: VOL-05 and VOL-06 were
both cases where every engine-reported check was green while the data was going
to the wrong filesystem.

## Logging and debugging

Default output is the audit trail NFR-04 requires — every mount, loop-device
attach/detach and elevated invocation, in plain text on stderr:

```bash
nemr start myproject
```

Two levers turn up the detail:

```bash
NEMR_DEBUG=1 nemr start myproject          # decision points, spans, devices
NEMR_LOG=nemr_containerd=trace nemr start myproject   # full env-filter syntax
```

Debug mode exists to satisfy a falsifiable requirement: **VOL-05 must be obvious
on the first run.** VOL-05 was a container running against the host root
filesystem instead of the project volume, with every message reporting success;
it was found after a reboot by inspecting host state by hand. Debug mode logs the
mount check, its result, and the device backing the working directory:

```
DEBUG ensure_volume_mounted{project=demo}: checked whether the project volume is mounted
      mount_point=/home/u/.local/share/nemr/mounts/demo mounted=false backing_device=<none>
 INFO ensure_volume_mounted{project=demo}: [nemr:volume] volume for "demo" is not mounted; remounting (VOL-06)
DEBUG ensure_volume_mounted{project=demo}: remounted the project volume
      mount_point=/home/u/.local/share/nemr/mounts/demo remounted=true backing_device=/dev/loop23
```

A working directory backed by the host root device rather than a loop device is
visible in that output directly.

## Pending decisions

Recorded here when made, per Section 11:

- Daemonless OCI build tool selection (Section 3.1) — Milestone 2.
- Base image: Alpine vs. Debian-slim (E-01) — Milestone 2.
- Measured base image size (AC-2.2) — Milestone 2.
- **E-03 containerd privilege model — RESOLVED 2026-08-10.** Recorded as
  Section 3.7 (PRIV-01–05): rootless containerd for container operations, with
  a narrowly scoped sudoers exception for Milestone 3 volume provisioning only.
- **Open, raised for Milestone 3 scoping:** a volume mounted by root under the
  PRIV-03 exception is owned by host root, which maps to `nobody` inside the
  rootless container's user namespace — so the container user would be unable
  to write to its own project volume. The likely fix (chown the mount into the
  user's mapped subuid range at provisioning time) belongs in `volume.rs`, and
  therefore in Milestone 3's scope. Surfaced per PRIV-05; not yet decided.
