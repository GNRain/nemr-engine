# AI Hub — Engineering Specification
## Phase 1: Core Container Engine

| Field | Value |
|---|---|
| Document ID | AIHUB-SPEC-001 |
| Version | 1.7 |
| Status | Approved for Implementation |
| Product Owner | Rain |
| Implementing Team | Claude Code (autonomous engineering agent) |
| Target Platform (this phase) | Linux (Ubuntu 22.04+), single-node, local execution only |
| Classification | Internal — Engineering |

### Revision History

| Version | Date | Change | Author |
|---|---|---|---|
| 1.0 | Initial draft | Baseline scope: engine-only, Go, containerd+runc | Product Owner |
| 1.1 | Revision | Language changed Go → Rust; added containerd-client wrapper-layer requirement | Product Owner |
| 1.2 | Revision | Restructured to formal engineering-spec format: requirement IDs, risk register, precise acceptance criteria, glossary, traceability matrix. All residual Go references corrected. | Product Owner |
| 1.3 | Revision | Added Section 4A (Execution Approach): prescribes `/goal` invocations mapped to each milestone's Acceptance Criteria, auto-mode pairing, turn-bound requirement, and `loop.md` guidance for post-Milestone-7 regression maintenance. Added escalation item E-06. | Product Owner |
| 1.4 | Revision | Added Section 3.7 (Privilege Model), recording the E-03 decision: rootless containerd for all container operations (PRIV-01), with a narrowly scoped sudoers exception for Milestone 3 volume provisioning only (PRIV-02–05). Updated Milestone 1 scope, Milestone 3 scope and acceptance criteria (AC-3.5 added), the corresponding `/goal` invocation in 4A.3, and added risk R-06. | Product Owner |
| 1.5 | Revision | Carried forward a Milestone 1 finding into Milestone 4's scope: gRPC-only containerd operations connect directly to the rootless socket, but `create_container()` requires the client to perform mounts and must run inside rootlesskit's namespaces — an earlier assumption that a manual `nsenter` would be needed was incorrect and is superseded by this note. Recorded so Milestone 4 does not need to rediscover this. | Product Owner, per Claude Code M1 completion report |
| 1.6 | Revision | Added Section 4A.5 (Spec Ownership): this document is now tracked in-repo as `SPEC.md`, committed as part of the Milestone 1 baseline. Claude Code updates milestone Notes and the Deviation Log directly for findings; Section 9 escalations and Sections 1–3 changes remain Product Owner decisions only. Product Owner review moves to commit-diff review of SPEC.md alongside code. Added `SPEC.md` to the Section 3.4 repository structure. | Product Owner |
| 1.7 | Correction | Corrected the Milestone 4 "Note carried forward" added in 1.5, which stated that a manual `nsenter` was the incorrect approach. That inverts the actual M1 finding: the incorrect earlier assumption was that `nsenter` would not be needed *at all*; manual `nsenter` against rootlesskit's `child_pid` is what was demonstrated working for mount-performing client operations. Note now records the validated command and the observed failure mode, and flags the engine-shape question as a possible E-04 at M4. Added a Milestone 1 note on the `containerd_client::tonic` re-export requirement. Recorded under the 4A.5 delegation. | Claude Code |

---

## 1. Purpose and Scope

### 1.1 Purpose

This document specifies the requirements, architecture, and acceptance
criteria for Phase 1 of AI Hub: a backend engine capable of provisioning
isolated, resource-bounded, pre-configured Claude Code execution
environments on a single Linux host, without dependency on Docker or
Docker Desktop.

This document is the authoritative source of truth for Phase 1 scope. Where
ambiguity exists between this document and any verbal or informal
instruction, this document governs. Deviations must be recorded in the
project README with rationale (see Section 11).

### 1.2 Product Context (informative — not in scope for Phase 1)

AI Hub is a desktop application addressing the portability and isolation
problem in Claude Code usage: a Claude Code session's context and history
are currently bound to the host machine on which they were created, with no
supported mechanism for transferring that state to another machine.

The target end-state product provides:

- Named, isolated **projects**, each backed by a fixed storage allocation
- Automatic provisioning of a pre-configured Claude Code environment per
  project, requiring no manual environment setup by the end user
- A management interface conceptually comparable to Oracle VirtualBox's VM
  list, applied to containers rather than virtual machines
- Cross-device synchronization of project state (Phase 2+)
- Multi-user collaboration on shared projects (Phase 3+)

**Phase 1 delivers none of the above end-user-facing functionality.** Phase
1 delivers the underlying provisioning engine only, exposed via a local CLI,
because every subsequent phase is architecturally dependent on this engine
functioning correctly. This is a deliberate risk-reduction sequencing
decision: the highest-uncertainty component is built and validated first, in
isolation, before investment in UI or distributed-systems work that would be
wasted if the core engine required architectural rework.

### 1.3 In Scope for Phase 1

| # | Capability |
|---|---|
| 1 | Build a minimal OCI-compliant container image containing Claude Code and its runtime dependencies |
| 2 | Create a named project: an isolated, size-bounded storage volume plus a container instantiated from the base image, with the volume mounted into it |
| 3 | Start, stop, and attach an interactive session to a project's container |
| 4 | List all projects with current status and storage utilization |
| 5 | Delete a project, releasing all associated resources |
| 6 | CLI as the sole user interface for this phase |

### 1.4 Out of Scope for Phase 1

The following are explicitly excluded. Their exclusion is a scope decision,
not an oversight — do not implement, even partially, without a scope change
approved by the Product Owner.

| # | Excluded item | Rationale |
|---|---|---|
| 1 | Graphical user interface (Electron, Tauri, web, or otherwise) | UI is the lowest-risk, most well-understood component of this product; building it before the engine is proven wastes effort if the engine's API shape changes |
| 2 | Cross-device sync of project volumes | Depends on a stable, tested local volume model, which this phase establishes |
| 3 | Multi-user collaboration, locking, or shared sessions | Depends on sync (item 2); no shared-state model exists yet to collaborate over |
| 4 | macOS or Windows support, including any VM-shim layer (Lima, WSL2, Apple Containerization, etc.) | Containerd/runc run natively only on Linux; every other target requires an additional virtualization layer that is a distinct, separable engineering problem |
| 5 | Auto-expanding storage quotas | Fixed quota at creation time is sufficient to validate the quota mechanism itself; dynamic resizing is an independent feature |
| 6 | Structured "task" objects for collaborative work | Depends on collaboration (item 3) |
| 7 | Any dependency on Docker or Docker Desktop, at any layer, for any purpose, including transitively | Hard architectural constraint — see Section 4.1 |

---

## 2. Definitions and Glossary

| Term | Definition |
|---|---|
| **Project** | The user-facing unit of isolation: one named, size-bounded storage volume paired with one containerized Claude Code environment |
| **Engine** | The Rust binary/library produced by this phase; the sole component with authority to create, mutate, or destroy containerd resources on behalf of AI Hub |
| **Base image** | The OCI image built once (Milestone 2) and instantiated per project; contains the OS userland, Node.js, and Claude Code |
| **Wrapper layer** | The `src/containerd/` module providing an internal, higher-level API over the low-level `containerd-client` crate (see Section 4.1) |
| **Volume** | A fixed-size, loopback-mounted ext4 filesystem bind-mounted into a project's container as its working directory |
| **Host** | The Linux machine (in current development, an Ubuntu VM under Oracle VirtualBox) on which containerd, runc, and the engine run |
| **OCI** | Open Container Initiative — the specification standard for container images and runtimes that containerd and runc implement |

---

## 3. Technical Architecture

### 3.1 Runtime Stack (Normative)

| Layer | Selection | Rationale |
|---|---|---|
| Implementation language | **Rust**, stable toolchain via `rustup` | No garbage collector, predictable resource usage, memory safety without runtime cost — directly supports the product's "lightweight" requirement and is particularly load-bearing in Section 3.6 (volume/mount handling), where resource-leak bugs are the primary risk |
| Container runtime | **containerd** | Industry-standard runtime; daemon-based lifecycle management; no dependency on Docker Engine |
| Low-level runtime | **runc**, invoked exclusively by containerd | Do not call runc directly under any circumstances; all container operations go through containerd's API |
| containerd bindings | `containerd-client` crate (`containerd/rust-extensions`, official upstream project) | First-party Rust bindings maintained by the containerd organization |
| Async runtime | `tokio` | Required by `containerd-client`'s gRPC transport (`tonic`) |
| Image build tooling | Daemonless OCI builder — candidates: `nerdctl build` (BuildKit-backed), standalone `buildctl`, or `img`. Final selection recorded in README per Section 11. | Must not require a running Docker daemon |

**Hard constraint:** No component of this stack may depend on Docker Engine
or Docker Desktop, directly or transitively. If any candidate tool is later
found to shell out to Docker internally, that tool is disqualified and must
be reported per Section 9.

### 3.2 Architectural Note: `containerd-client` Is a Low-Level Binding

The Rust `containerd-client` crate provides direct gRPC/protobuf bindings to
containerd's API surface. Unlike the official Go client library, it does
**not** provide high-level convenience operations (e.g., "pull an image,"
"create and start a container" as single calls). This is an intrinsic
property of the crate, not a defect to work around ad hoc.

**Requirement:** Phase 1 shall produce an internal wrapper module
(`src/containerd/`) that provides the higher-level operations the engine
needs, implemented once, on top of the raw crate. All engine logic
(`src/engine/`) shall depend exclusively on this wrapper module and shall
never invoke `containerd-client` directly. This is a load-bearing
architectural decision: the wrapper module is first-class infrastructure,
designed and reviewed at Milestone 1, and extended (not duplicated or
bypassed) by every subsequent milestone.

### 3.3 Host Prerequisites

The following must be present on the host and are **provisioned manually**
for Phase 1 — the engine does not install its own dependencies at this
stage:

- `containerd`, running as a systemd-managed daemon
- `runc`
- Linux kernel with cgroups v2 and overlay filesystem support (standard on
  Ubuntu 22.04+)
- Rust stable toolchain via `rustup`

Exact installation steps (package names, systemd unit management, and a
verification procedure such as `ctr version` succeeding against the local
containerd socket) shall be documented in `PREREQUISITES.md`, written such
that a person with no prior context can provision a clean Ubuntu host from
this document alone.

### 3.4 Repository Structure (Normative)

```
ai-hub-engine/
├── SPEC.md                    # This document — tracked in-repo per 4A.5
├── src/
│   ├── bin/
│   │   └── aihub.rs           # CLI entrypoint; sole interface for Phase 1
│   ├── containerd/            # Wrapper layer over containerd-client (3.2)
│   │   ├── client.rs          # Connection management, shared client handle
│   │   ├── images.rs          # Image pull/import operations
│   │   └── containers.rs      # Container create/start/stop/delete operations
│   ├── engine/                # Product logic; depends only on src/containerd/
│   │   ├── image.rs           # Base image build/import orchestration
│   │   ├── project.rs         # Project lifecycle: create/start/stop/list/delete
│   │   └── volume.rs          # Quota-bounded storage volume management
│   ├── config.rs              # Paths, defaults, constants
│   └── auth.rs                # Claude Code credential injection (3.5)
├── image/
│   └── Dockerfile             # OCI image definition (build tool per 3.1)
├── scripts/
│   └── e2e_smoke_test.sh      # Milestone 7 regression test
├── PREREQUISITES.md
├── README.md
└── Cargo.toml
```

### 3.5 Authentication Handling (Normative)

| Requirement ID | Requirement |
|---|---|
| AUTH-01 | Credentials shall not be baked into the base image. |
| AUTH-02 | At container creation time, the engine shall bind-mount the host user's existing Claude Code credentials directory into the container, **read-only**, at the path Claude Code expects. |
| AUTH-03 | If no host credentials are found at creation time, the engine shall fail with a clear, actionable error message directing the user to authenticate on the host. Interactive in-container authentication flows are out of scope for Phase 1. |

### 3.6 Storage Quota Mechanism (Normative)

| Requirement ID | Requirement |
|---|---|
| VOL-01 | Each project's storage shall be a fixed-size allocation selected at creation time from a small set of presets (default set: 500MB / 2GB / 10GB), configurable via CLI flag. |
| VOL-02 | The allocation mechanism shall be a sparse file, formatted as ext4, mounted via a Linux loop device, and bind-mounted into the container as its working directory. This approach is selected over XFS project quotas specifically to avoid host filesystem prerequisites beyond a standard Ubuntu install. |
| VOL-03 | All mount, loop-device, and format operations shall be logged with sufficient detail to be independently auditable without reading source code. |
| VOL-04 | Volume creation, mounting, unmounting, and deletion logic shall use RAII patterns (Rust `Drop` implementations) to guarantee resource cleanup on error paths, not solely on the success path. |
| VOL-05 | When a volume reaches capacity, dependent container operations shall fail with a clear, human-readable error. Silent data loss is a critical defect. Auto-expansion is explicitly out of scope (Section 1.4, item 5). |

### 3.7 Privilege Model (Normative)

This section records the engine's privilege boundary as a deliberate
architecture decision, not an implementation detail resolved ad hoc during
a milestone. It exists because the engine's privilege model is directly
part of what any third party evaluating this project's output will assess
— it is documented here so that assessment has a clear, single source of
truth rather than requiring inspection of implementation history.

**Decision (E-03, recorded 2026-08-10):** the engine shall not require
root or root-equivalent group membership for its container-management
operations. Volume provisioning is the sole exception, and is scoped as
narrowly as the underlying Linux primitives allow.

| Requirement ID | Requirement |
|---|---|
| PRIV-01 | The engine's containerd interactions (all operations in Milestones 1, 4, 5, and 6) shall run against a **rootless containerd** instance. Containers execute within an unprivileged user namespace; container UID 0 maps to an unprivileged host UID. No user is added to a root-equivalent group (e.g., a socket-access group with root-equivalent capability) as part of Phase 1. |
| PRIV-02 | Volume provisioning (Milestone 3: `losetup`, `mkfs.ext4`, `mount`/`umount`) is a genuinely privileged operation on Linux and has no rootless equivalent within this phase's chosen quota mechanism (Section 3.6). This is the sole permitted exception to PRIV-01. |
| PRIV-03 | The exception in PRIV-02 shall be implemented as a narrowly scoped sudoers rule limited to the exact command forms volume provisioning requires (specific `losetup`, `mkfs.ext4`, `mount`, `umount` invocations constrained to paths under the engine's managed volume directory), not blanket root access and not unrestricted `sudo`. The rule shall be committed to the repository (e.g., under `deploy/sudoers.d/`) so it is reviewable, not configured ad hoc on the host. |
| PRIV-04 | Every operation performed under the PRIV-03 exception shall be logged per NFR-04, specifically identifying that it ran under elevated privilege and why. |
| PRIV-05 | This privilege split (rootless for container operations, narrowly scoped elevation for volume provisioning only) applies for the duration of Phase 1 in full — it is not re-litigated per milestone. Any milestone that appears to need elevated privilege outside the PRIV-03 scope shall be escalated per Section 9, not resolved by widening the sudoers rule unilaterally. |

**Rationale:** a broad root-equivalent group (the alternative considered
under E-03) would have been simpler to implement but leaves the engine's
security story as "the user has root-equivalent access to run containers
at all," which is both a larger attack surface than necessary and a weaker
position to defend when this project's output is reviewed externally.
Rootless containerd removes that requirement for the majority of the
engine's operations; the volume-provisioning exception is real but is
bounded, explicit, and auditable rather than implicit.

---

## 4. Functional Requirements and Milestones

Milestones shall be implemented and validated **strictly in sequence**. Each
milestone is a discrete, independently reviewable unit of work (one
commit/PR per milestone). A milestone shall not begin until the prior
milestone's Definition of Done is met and confirmed.

### Milestone 1 — Containerd Connectivity and Wrapper Foundation

**Scope:**
1. A minimal Rust program that connects to the local containerd socket via
   `containerd-client` directly (no wrapper) and lists existing
   containers/images. Establishes a connectivity baseline prior to any
   abstraction. **The target containerd instance shall be running in
   rootless mode per PRIV-01 (Section 3.7)** — connectivity is validated
   against the rootless socket, not a system-wide root-owned instance.
2. Initial implementation of `src/containerd/`: `client.rs` (connection
   setup) and minimal `images.rs`/`containers.rs` exposing async
   `list_images()` and `list_containers()`. Scope is intentionally minimal;
   the wrapper's surface area grows in later milestones as concrete needs
   arise (image pull in Milestone 2, container create/start in Milestones
   4–5). Do not attempt to design the complete wrapper API in this
   milestone.

**Note carried forward from Milestone 1 (tonic version skew):** any
milestone extending `src/containerd/` will need `tonic` types such as
`Request`. Import them from `containerd_client::tonic`, which the crate
re-exports. Declaring `tonic` as a direct dependency in `Cargo.toml` lets
cargo resolve a second, incompatible version into the graph, and the
generated gRPC clients then reject the wrong `Request` type with a
"multiple different versions of crate `tonic`" error that does not name the
real cause. `Cargo.toml` carries a comment where the dependency would
otherwise go.

**Acceptance Criteria:**
- AC-1.1: Raw connectivity program exits 0 against a live containerd
  instance on the target Ubuntu host.
- AC-1.2: `list_images()` and `list_containers()` in the wrapper module
  return results identical to the raw baseline program's output.
- AC-1.3: `PREREQUISITES.md` and `README.md` document reproduction steps
  sufficient for a clean-host rebuild.

### Milestone 2 — Base Image Build

**Scope:** `image/Dockerfile` and a documented, daemonless build command
(per Section 3.1) producing the base image specified in Section 1.3, item 1.

**Acceptance Criteria:**
- AC-2.1: Image build completes successfully with no Docker daemon present
  on the host at any point.
- AC-2.2: Final image size is measured and recorded in README.
- AC-2.3: `ctr run` (or the selected tool's equivalent) against the raw
  built image, without engine involvement, drops into a shell in which the
  Claude Code CLI's version command succeeds.

### Milestone 3 — Volume Creation with Quota

**Scope:** `src/engine/volume.rs`, implementing VOL-01 through VOL-05.
**This is the sole milestone permitted to use elevated privilege, per the
PRIV-02/PRIV-03 exception in Section 3.7.** The sudoers rule scoping that
exception (`deploy/sudoers.d/`) shall be authored as part of this
milestone's deliverables, not assumed to pre-exist on the host.

**Acceptance Criteria:**
- AC-3.1: Volume creation at a specified size is confirmed correctly capped
  via `df`/`du` measurement.
- AC-3.2: Volume deletion leaves no residual file, loop device, or mount
  entry on the host — verified by host inspection post-deletion.
- AC-3.3: An automated test exercises create → mount → write past capacity
  → confirm enforced failure → delete, and passes.
- AC-3.4: A fault-injection test (e.g., simulated failure mid-mount)
  confirms RAII cleanup leaves no orphaned resources (validates VOL-04).
- AC-3.5: The sudoers rule at `deploy/sudoers.d/` is shown restricted to
  the exact command forms and path constraints required by PRIV-03 — not
  unrestricted `sudo` — and every elevated operation is shown logged per
  PRIV-04.

### Milestone 4 — Project Lifecycle: Create

**Scope:** `src/engine/project.rs::create()`. Given a project name and size,
creates a volume (Milestone 3) and a container from the base image
(Milestone 2), mounts the volume, injects credentials (AUTH-01–03), and
registers a discoverable named reference. Extends the wrapper module with a
general-purpose `create_container()` operation — implemented as reusable
wrapper infrastructure, not logic specific to this call site.

**Note carried forward from Milestone 1 (rootless namespace boundary):**
gRPC-only containerd operations (list/pull/version) connect directly to the
rootless socket from outside its namespaces — this was validated in M1 and
requires no special handling. `create_container()` is different: it
requires the *client* to perform mount operations as part of container
creation, which the host mount namespace cannot do. Attempting it from
outside fails with:

```
ctr: failed to mount ... fstype: overlay ... err: operation not permitted
```

Such operations must run inside rootlesskit's user and mount namespaces.
In M1 this was demonstrated working with a manual `nsenter` against the
`child_pid` rootlesskit records in its state directory:

```bash
nsenter -U --preserve-credentials -m -n -t \
    "$(cat "$XDG_RUNTIME_DIR/containerd-rootless/child_pid")" \
    env CONTAINERD_ADDRESS=/run/containerd/containerd.sock \
    ctr container create <image> <id>
```

Confirm the approach before implementing `create_container()` rather than
assuming M1's connection pattern extends unchanged. Shelling out to
`nsenter` is the validated baseline, not necessarily the right shape for
engine code — having the engine re-enter the namespaces itself is worth
evaluating at M4. If that evaluation would materially shape the wrapper
API, it is an E-04 escalation.

**Acceptance Criteria:**
- AC-4.1: `aihub create <name> --size 2GB` produces a container and volume
  pair discoverable via containerd's own listing APIs, in a stopped-but-
  ready state.
- AC-4.2: Repeating the command with a duplicate name fails with a clear
  error rather than silently overwriting or creating a conflicting state.

### Milestone 5 — Project Lifecycle: Start / Attach / Stop

**Scope:** `aihub start <name>`, `aihub attach <name>`, `aihub stop <name>`.

**Acceptance Criteria:**
- AC-5.1: `create` → `start` → `attach` results in an interactive shell in
  which Claude Code runs correctly against the mounted project volume.
- AC-5.2: Files written during a session persist across `stop` followed by
  `start`.
- AC-5.3: `stop` on an already-stopped project, and `attach` on a
  not-yet-started project, both fail with clear errors rather than
  undefined behavior.

### Milestone 6 — Project Lifecycle: List / Delete

**Scope:** `aihub list`, `aihub delete <name>`.

**Acceptance Criteria:**
- AC-6.1: `list` output (status, storage used vs. quota) matches actual
  containerd state, verified by cross-checking against `ctr` output
  directly.
- AC-6.2: `delete` requires explicit confirmation and, once executed, leaves
  zero orphaned containers, volumes, or loop devices — verified by host
  inspection.

### Milestone 7 — End-to-End Validation

**Scope:** `scripts/e2e_smoke_test.sh`: a scripted, non-interactive run of
the complete lifecycle — create → start → attach (scripted Claude Code
invocation) → stop → list → delete — asserting success at each step.

**Acceptance Criteria:**
- AC-7.1: Script completes with zero manual intervention on a freshly
  provisioned Ubuntu host with only Section 3.3 prerequisites installed.
- AC-7.2: Script is adopted as the standing regression test for all future
  engine changes and is referenced as such in README.

---

## 4A. Execution Approach — Driving Milestones with `/goal`

This section prescribes how each milestone in Section 4 is to be executed in
Claude Code, so that milestone completion is verified by a consistent
mechanism rather than left to ad hoc judgment in each session.

### 4A.1 Rationale

Claude Code's `/goal` command sets a completion condition and continues
working, turn after turn, until a separate evaluator model confirms the
condition holds — rather than stopping after a single pass and waiting for
manual review at each step. Each milestone in Section 4 is already written
with falsifiable Acceptance Criteria (AC-x.x); this is a deliberate spec
design choice made specifically so those criteria can be used directly as
`/goal` conditions, without rewriting or loosening them for the purpose.

**Constraint on the evaluator:** the `/goal` evaluator does not run
commands or read files independently — it judges only what Claude has
already surfaced in the conversation transcript. Every `/goal` invocation
below is therefore phrased so the condition asks for evidence Claude's own
output will contain (command results, inspection output, test results
shown in-session), not evidence the evaluator would need to go verify
itself.

### 4A.2 Standard Invocation Pattern

For each milestone, the Product Owner shall invoke Claude Code with the
milestone's **Scope** (from Section 4) as the working instruction, and the
milestone's **Acceptance Criteria** (verbatim, with IDs) as the `/goal`
condition. Pair every invocation with auto mode, since `/goal` alone still
prompts for individual tool-call permissions and defeats the purpose of an
unattended milestone run.

General form:

```
/goal <milestone's Acceptance Criteria, verbatim, each demonstrated by
command output or inspection results shown in the transcript>. Do not
proceed to work outside this milestone's scope. Stop after <N> turns if
the condition is not met and report what remains.
```

A turn-bound clause (`stop after N turns`) is required on every invocation
per Section 4A — an unbounded goal on an underspecified milestone risks
looping indefinitely; see Escalation item E-06 (Section 9) for the
procedure when a milestone hits its turn bound without meeting its
condition.

### 4A.3 Per-Milestone `/goal` Conditions

**Milestone 1:**
```
/goal AC-1.1: the raw connectivity program exits 0 against the live
containerd instance, with the command and output shown. AC-1.2: the
wrapper module's list_images() and list_containers() return output shown
to be identical to the raw baseline program's output. AC-1.3:
PREREQUISITES.md and README.md contain reproduction steps, with their
relevant contents shown. Stay within src/bin (raw program) and
src/containerd/ (wrapper) only. Stop after 25 turns if not met.
```

**Milestone 2:**
```
/goal AC-2.1: the image build command is shown completing successfully,
with confirmation no Docker daemon was running on the host at any point.
AC-2.2: the final image size is measured and shown, and recorded in
README. AC-2.3: ctr run (or the selected tool's equivalent) against the
raw built image is shown dropping into a shell where the Claude Code CLI
version command succeeds. Stay within image/Dockerfile and the documented
build command only. Stop after 25 turns if not met.
```

**Milestone 3:**
```
/goal AC-3.1: volume creation at a specified size is shown correctly
capped via df/du output. AC-3.2: volume deletion is shown to leave no
residual file, loop device, or mount entry, verified by host inspection
commands and their output. AC-3.3: an automated test covering create →
mount → write past capacity → confirm enforced failure → delete is shown
passing. AC-3.4: a fault-injection test confirming RAII cleanup (VOL-04)
leaves no orphaned resources is shown passing. AC-3.5: the sudoers rule at
deploy/sudoers.d/ is shown restricted to the exact command forms and path
constraints required by PRIV-03, and elevated operations are shown logged
per PRIV-04. Stay within src/engine/volume.rs, its tests, and
deploy/sudoers.d/ only. Stop after 30 turns if not met.
```

**Milestone 4:**
```
/goal AC-4.1: `aihub create <name> --size 2GB` is shown producing a
container and volume pair discoverable via containerd's own listing APIs,
in a stopped-but-ready state. AC-4.2: repeating the command with a
duplicate name is shown failing with a clear error rather than silently
overwriting state. Confirm the src/containerd/ wrapper was extended with a
general-purpose create_container() operation, not call-site-specific
logic. Stay within src/engine/project.rs's create() path and the wrapper
extension only. Stop after 25 turns if not met.
```

**Milestone 5:**
```
/goal AC-5.1: create → start → attach is shown resulting in an interactive
shell where Claude Code runs correctly against the mounted project volume.
AC-5.2: files written during a session are shown persisting across stop
followed by start. AC-5.3: stop on an already-stopped project, and attach
on a not-yet-started project, are both shown failing with clear errors.
Stay within the start/attach/stop CLI paths only. Stop after 25 turns if
not met.
```

**Milestone 6:**
```
/goal AC-6.1: `aihub list` output is shown matching actual containerd
state, cross-checked directly against ctr output. AC-6.2: `aihub delete`
is shown requiring explicit confirmation and, once executed, leaving zero
orphaned containers, volumes, or loop devices, verified by host
inspection. Stay within the list/delete CLI paths only. Stop after 20
turns if not met.
```

**Milestone 7:**
```
/goal AC-7.1: scripts/e2e_smoke_test.sh is shown completing with zero
manual intervention on a freshly provisioned Ubuntu host with only Section
3.3 prerequisites installed. AC-7.2: README is shown referencing this
script as the standing regression test for all future engine changes.
Stop after 20 turns if not met.
```

### 4A.4 Post-Milestone Review (not automated)

`/goal` completion is a signal that the evaluator judged the transcript
sufficient — it is not a substitute for the Product Owner reviewing the
resulting diff. Each milestone's commit/PR shall still be reviewed against
Section 4's Scope and this section's condition before the next milestone
begins, per the sequencing rule in Section 4's preamble.

### 4A.5 Spec Ownership: This Document Lives in the Repository

This document (`SPEC.md`) shall be committed to the repository as part of
the Milestone 1 baseline and tracked by version control alongside the
code it governs. It is no longer maintained as an external artifact edited
outside the implementation loop.

This changes who updates what, and how:

| Change type | Who updates it | Mechanism |
|---|---|---|
| A finding, correction, or note scoped to work already done or about to be done (e.g., a technical detail discovered mid-milestone that a later milestone needs to know) | **Claude Code**, directly | Appended to the relevant milestone's Scope as a "Note carried forward," or to Section 11 (Deviation Log), as part of that milestone's own commit — not raised as a separate report awaiting manual transcription |
| A genuine decision: anything in Section 9's escalation list (E-0x), any change to Sections 1–3 (purpose, scope, architecture), any change to a milestone's Acceptance Criteria | **Product Owner**, via explicit instruction | Claude Code shall surface the decision and wait; it shall not resolve an E-0x item and record it unilaterally, even in the Deviation Log |

**Rationale:** prior to this revision, findings surfaced in a Claude Code
session had to be manually relayed and re-entered into this document by
the Product Owner — a process that does not scale past a small number of
milestones and introduces transcription risk. Facts Claude Code has
already validated firsthand (e.g., a namespace-boundary finding, a
dependency-version fix) do not need to pass through a human relay to be
recorded correctly; genuine decisions still do, and this table exists so
that distinction is never ambiguous in practice.

**Review mechanism:** the Product Owner reviews spec changes the same way
as code changes — via the commit diff (`git log -p -- SPEC.md` or the
equivalent PR view) — rather than via a separately generated summary.
Section 4A.4 already requires diff review before the next milestone
begins; this extends to the spec file itself, which is reviewed as part of
that same diff, not separately.

### 4A.6 `/loop` for Post-Milestone-7 Regression Maintenance

`/loop` is not used during Milestones 1–7 — each is a bounded task with a
provable end state, which is what `/goal` is for for. Once Milestone 7
exists, a project-level `.claude/loop.md` shall be added to keep
`scripts/e2e_smoke_test.sh` honest across any further engine work in later
phases:

```markdown
Run scripts/e2e_smoke_test.sh. If it fails, diagnose the failing step,
propose a minimal fix, and report what broke. If it passes, say so in
one line.
```

This is invoked ad hoc with `/loop 30m` (fixed interval) or a bare `/loop`
(Claude chooses the interval) during any session doing further work on the
engine, not run continuously outside active development sessions.

---

## 5. Non-Functional Requirements

| Requirement ID | Requirement |
|---|---|
| NFR-01 | No component of the stack may depend on Docker Engine or Docker Desktop, directly or transitively. This is a hard constraint; discovery of a hidden dependency at any point is a release-blocking defect, not a note. |
| NFR-02 | Project creation and start shall complete in low single-digit seconds under normal conditions. Deviation indicates a defect in image size, build approach, or engine logic and shall be flagged (Section 9), not silently optimized around. |
| NFR-03 | No operation shall leave orphaned loop devices, mounts, or containerd resources after `delete` completes successfully. |
| NFR-04 | Every destructive or system-level operation (mount, loop-device attach/detach, filesystem format) shall be logged with sufficient detail for the Product Owner to reconstruct what occurred without reading Rust source. |
| NFR-05 | The engine shall not silently escalate privileges. Any operation genuinely requiring elevated privileges shall be explicit and documented, with the security implication stated (see Section 9). |

---

## 6. Risk Register

| ID | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R-01 | `containerd-client`'s low-level API surface makes a required operation significantly harder to implement correctly than anticipated | Medium | Medium | Wrapper layer isolates this risk to `src/containerd/`; escalate per Section 9 if a specific operation proves disproportionately difficult |
| R-02 | Loopback/mount handling leaks resources under error conditions | Medium | High | RAII-based cleanup (VOL-04), explicit fault-injection test (AC-3.4) |
| R-03 | Chosen daemonless image-build tool has undocumented Docker dependency | Low | High (violates NFR-01) | Explicit verification step in Milestone 2; disqualify and re-select if found |
| R-04 | Base image size or startup time exceeds "lightweight" expectations | Medium | Medium | Measured and recorded at Milestone 2 and validated against NFR-02; not a blocking defect at Phase 1 but must be visible for Phase 2 planning |
| R-05 | Credential mount approach (AUTH-02) has unintended host filesystem permission implications | Low | Medium | Read-only mount is a hard requirement; review at Milestone 4 |
| R-06 | Rootless containerd (PRIV-01) introduces setup friction or feature gaps (e.g., storage driver limitations, networking constraints) not present in a root-owned daemon | Medium | Medium | Validated directly at Milestone 1, before any engine logic depends on it; escalate per E-03 pattern (Section 9) if a required capability turns out to be unavailable rootless |

---

## 7. Traceability Matrix

| Scope Item (Section 1.3) | Milestone(s) | Key Requirements |
|---|---|---|
| Base image | M2 | Section 3.1, 3.4 |
| Project creation | M1, M3, M4 | AUTH-01–03, VOL-01–05 |
| Start/stop/attach | M5 | — |
| List | M6 | — |
| Delete | M3, M6 | VOL-04, NFR-03 |
| No Docker dependency | All | NFR-01 |

---

## 8. Deliverables Checklist

- [ ] `SPEC.md` — this document, committed as the Milestone 1 baseline (4A.5)
- [ ] `PREREQUISITES.md`
- [ ] `image/Dockerfile` + documented daemonless build command
- [ ] `src/containerd/` wrapper module (client, images, containers)
- [ ] `src/engine/volume.rs` + tests (including AC-3.4 fault injection)
- [ ] `src/engine/image.rs`
- [ ] `src/engine/project.rs` + tests
- [ ] `src/auth.rs`
- [ ] `src/bin/aihub.rs` (`create`, `start`, `attach`, `stop`, `list`, `delete`)
- [ ] `scripts/e2e_smoke_test.sh`
- [ ] `README.md`, including: architecture decisions and rationale (base
      image choice, build tool choice), measured image size, wrapper-layer
      design notes, any recorded deviations from this specification (Section
      11)

---

## 9. Escalation — Items Requiring Explicit Product Owner Decision

The following shall be surfaced to the Product Owner for a decision, not
resolved unilaterally, if encountered during implementation:

- E-01: Choice between Alpine and Debian-slim for the base image, if
  Claude Code or Node.js exhibits musl libc (Alpine) compatibility issues
- E-02: Selection among daemonless OCI build tools, if the initial choice
  proves substantially harder to operate reliably on Ubuntu than expected
- E-03: Any containerd operation that appears to require host root
  privileges — state the security implication explicitly; do not resolve by
  silently wrapping calls in `sudo`
- E-04: Any `containerd-client` operation where the "correct" wrapper API
  shape is not obvious, particularly where wrapper ergonomics trade off
  against fidelity to the underlying gRPC calls — expected friction is
  acceptable without escalation; friction that would materially shape the
  wrapper API's long-term design is not
- E-05: Any measured deviation from NFR-02 (startup time) or NFR-04
  (logging clarity) that cannot be resolved without a scope or architecture
  change
- E-06: Any `/goal` invocation (Section 4A) that reaches its turn bound
  without the evaluator confirming the condition met — report what
  remains unmet and why, per the evaluator's most recent reason, rather
  than re-running the same `/goal` invocation unchanged or silently
  narrowing the condition to force a pass

---

## 10. Definition of Done — Phase 1

Phase 1 is complete when the Product Owner can, on the reference Ubuntu VM,
execute `scripts/e2e_smoke_test.sh` from a clean checkout with only Section
3.3 prerequisites installed, and observe it: create an isolated,
quota-bounded, pre-configured Claude Code environment; execute a Claude Code
command within it; stop it; list it accurately; and delete it with no
residual host state — entirely through the `aihub` CLI, with zero Docker
involvement at any layer.

---

## 11. Deviation Log

*(To be maintained by the implementing team. Record any point at which
implementation diverges from this specification, with rationale, date, and
Product Owner sign-off status.)*

| Date | Section | Deviation | Rationale | Approved |
|---|---|---|---|---|
| 2026-08-10 | 3.4 | Added `src/lib.rs` | Section 3.4's tree defines no crate root, but `src/bin/*.rs` cannot import `src/containerd/` without a library target. Declares modules only; no logic. | Pending |
| 2026-08-10 | 3.4 | Added `src/containerd/mod.rs`, `src/engine/mod.rs` | Rust requires a `mod.rs` for a directory to form a module. Declares submodules only. | Pending |
| 2026-08-10 | 3.4 | Added `src/bin/raw_connectivity.rs` | Section 3.4's tree lists only `aihub.rs` under `src/bin/`, but Milestone 1 scope item 1 requires a raw baseline program. Kept separate from the CLI so `aihub` never links the raw `containerd-client` path. | Pending |
| 2026-08-11 | 3.4 | Added `src/bin/wrapper_connectivity.rs` | AC-1.2 requires showing wrapper output identical to the baseline's, which needs a runnable harness using only the wrapper. Adding a flag to `raw_connectivity` instead would have made the "raw" binary link the wrapper, destroying its independence as a baseline. | Pending |
| 2026-08-11 | 3.3 | `PREREQUISITES.md` documents rootless tooling (`uidmap`, `rootlesskit`, `slirp4netns`) not enumerated in Section 3.3 | Section 3.7 (PRIV-01) requires rootless containerd, which needs this tooling; Section 3.3's list predates 3.7 and was not updated alongside it. Documented rather than silently assumed, since 3.3 designates `PREREQUISITES.md` as the clean-host provisioning source. Section 3.3 itself left unedited — Sections 1–3 are Product Owner territory per 4A.5. | Pending |

**Note on duplication:** `README.md` carries a mirror of this table, because
Section 1.1 requires deviations to be recorded in the README. This table is
canonical. Consolidating the two would require editing Section 1.1, which is
Product Owner territory under 4A.5.
