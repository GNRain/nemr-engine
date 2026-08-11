# AI Hub — Engine (Phase 1)

Backend engine that provisions isolated, resource-bounded, pre-configured
Claude Code execution environments on a single Linux host, with no dependency
on Docker at any layer.

Authoritative specification: [`SPEC.md`](SPEC.md) (AIHUB-SPEC-001 v1.7),
tracked in this repository per Section 4A.5. Where this README and the
specification disagree, the specification governs.

## Status

| Milestone | State |
|---|---|
| M1 — Containerd connectivity + wrapper foundation | Complete — AC-1.1, AC-1.2, AC-1.3 met |
| M2 — Base image build | Not started |
| M3 — Volume creation with quota | Not started |
| M4 — Project lifecycle: create | Not started |
| M5 — Start / attach / stop | Not started |
| M6 — List / delete | Not started |
| M7 — End-to-end validation | Not started |

The host is fully provisioned per `PREREQUISITES.md`. No blockers outstanding
for Milestone 1.

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

Volume provisioning (Milestone 3: `losetup`, `mkfs.ext4`, `mount`) is the sole
exception, and is bounded by a narrowly scoped sudoers rule committed to
`deploy/sudoers.d/` (PRIV-02–03). That rule is a Milestone 3 deliverable and
does not exist yet.

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
cd ai-hub-engine
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

## Current blockers

None for Milestone 1. E-03 is resolved by Section 3.7 of the specification;
the host is provisioned and rootless containerd is verified reachable.

## Deviation log

Deviations from AIHUB-SPEC-001, per Section 11. Structural additions required
by the Rust module system are recorded here for transparency; none changes the
layering the specification mandates.

This table mirrors [`SPEC.md`](SPEC.md) Section 11, which is canonical. It is
duplicated here because Section 1.1 requires deviations to be recorded in the
README.

| Date | Section | Deviation | Rationale | Approved |
|---|---|---|---|---|
| 2026-08-10 | 3.4 | Added `src/lib.rs` | Section 3.4's tree has no crate root, but `src/bin/*.rs` cannot import `src/containerd/` without a library target. Declares modules only. | Pending |
| 2026-08-10 | 3.4 | Added `src/containerd/mod.rs`, `src/engine/mod.rs` | Rust requires a `mod.rs` for a directory to form a module. Declares submodules only. | Pending |
| 2026-08-10 | 3.4 | Added `src/bin/raw_connectivity.rs` | Section 3.4's tree lists only `aihub.rs` under `src/bin/`, but Milestone 1 scope item 1 requires a separate raw baseline program, kept distinct from the CLI so the CLI never links the raw crate path. | Pending |
| 2026-08-11 | 3.4 | Added `src/bin/wrapper_connectivity.rs` | AC-1.2 requires showing wrapper output identical to the baseline's, which needs a runnable harness that uses only the wrapper. Adding a flag to `raw_connectivity` instead would have made the "raw" binary link the wrapper and destroyed its independence as a baseline. | Pending |

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
