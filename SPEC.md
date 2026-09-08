# Nemr — Engineering Specification
## Phase 1: Core Container Engine

| Field | Value |
|---|---|
| Document ID | NEMR-SPEC-001 |
| Version | 1.126 |
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
| 1.8 | Revision | Section 8: the `README.md` deliverable now requires a pointer to Section 11 rather than a reproduction of its contents, removing the duplicated deviation log. `SPEC.md` Section 11 is the single source of truth per 4A.5. Note that Section 1.1 still independently requires deviations to be "recorded in the project README with rationale" — a residual inconsistency left unedited, as Sections 1–3 are Product Owner territory. | Claude Code, per Product Owner instruction |
| 1.9 | Revision | Section 3.3: added cgroup v2 controller delegation to the host prerequisites list. Discovered at Milestone 2 — systemd delegates only `memory` and `pids` to a user session by default, and rootless container start fails on `cpu.weight` without `cpu`/`cpuset`/`io`. Procedure documented in PREREQUISITES.md Step 2a. Sections 1–3 are Product Owner territory under 4A.5; this edit was made on explicit Product Owner instruction. | Claude Code, per Product Owner instruction |
| 1.10 | Revision | Added a Milestone 5 "Note carried forward" from Milestone 2: under PRIV-01, runc's default cgroup path is unwritable and task start requires the systemd cgroup driver with an explicit scope under the delegated user slice, plus session-bus environment variables. Validated with `ctr` at M2. Flags the wrapper API shape for this as a possible E-04 at M5. Recorded under the 4A.5 delegation. | Claude Code |
| 1.11 | Revision | Rewrote PRIV-03 around a single root-owned helper binary with a wildcard-free NOPASSWD grant, after `sudoers(5)` was found to match `/` inside argument wildcards — making the originally specified path constraint unenforceable and the rule effectively equivalent to passwordless root. Corrected PRIV-02: sparse-file allocation and `mkfs.ext4` are not privileged (measured at M3), removing the most destructive verb from the privileged surface. Added PRIV-06 requiring the helper to perform mount and the subuid `chown` as one atomic operation, resolving the previously open rootless volume-ownership question without exposing `chown` as its own grant. Sections 1–3 are Product Owner territory under 4A.5; this edit was made on explicit Product Owner instruction following an E-03/PRIV-05 escalation. | Claude Code, per Product Owner instruction |
| 1.12 | Revision | Propagated the 1.11 helper-binary design into the three places still describing the superseded enumerated-command approach: AC-3.5, Milestone 3's Scope, and 4A.3's Milestone 3 `/goal` text. All references to "command forms" and enumerated command whitelisting removed, since PRIV-03 no longer describes that. Milestone 3's `/goal` file scope widened from `deploy/sudoers.d/` to `deploy/` to cover the helper crate. Sections 1–3 and Acceptance Criteria are Product Owner territory under 4A.5; this edit was made on explicit Product Owner instruction. | Claude Code, per Product Owner instruction |
| 1.13 | Correction | PRIV-06's chown target was wrong. It specified an offset within the `/etc/subuid` range; the measured namespace mapping is `inside 0 -> host 1000 (count 1)` and `inside 1 -> host 100000 (count 65536)`, so the subuid range maps to container UID 1 and above, not 0. A volume chowned there would be owned by a non-root container user, which is wrong for a base image that runs as root. Corrected to the host UID that container UID 0 maps to — the invoking user's own host UID, taken from `SUDO_UID`/`SUDO_GID`. The error originated in Claude Code's own escalation wording at version 1.11 and was caught when implementing the helper. Sections 1–3 are Product Owner territory under 4A.5; this edit was made on explicit Product Owner instruction. | Claude Code, per Product Owner instruction |
| 1.14 | Correction | AUTH-02 was mis-scoped, not merely broad: it required mounting the whole `~/.claude` *directory*, which on a real host also contains `projects/`, `history.jsonl`, caches and plugin state. Mounting all of it would expose every project's history to every container — defeating the isolation this product exists to provide — and would break Claude Code regardless, since AUTH-02 mandates a read-only mount and Claude Code writes session state under `~/.claude`. Narrowed to the credentials file alone, with other `~/.claude` content explicitly container-local. Recorded as a correction to the requirement rather than an implementation deviation, per Product Owner instruction. The filename is `.credentials.json` with a leading dot, verified on the reference host; the instruction's wording omitted it and was corrected in both the host and container paths. | Claude Code, per Product Owner instruction |
| 1.15 | Correction | Corrected the Milestone 4 "Note carried forward" now that `create_container()` has actually been run. The engine does **not** need to enter rootlesskit's namespaces: `nemr create` succeeded from the host mount namespace with no `nsenter` and no `sudo`. The constraint is specific to `ctr`, which mounts the image snapshot client-side to read the image config; the engine reads that config from the content store over gRPC and has containerd prepare the snapshot server-side, so nothing is mounted in the engine's own namespace. The `ctr`-specific guidance is retained because Milestones 2 and 6 invoke `ctr` directly. Milestone 5 is explicitly not settled by this — starting a task runs runc, which does mount. Recorded under the 4A.5 delegation, on evidence rather than assumption. | Claude Code |
| 1.16 | Revision | Project renamed from "AI Hub" to "Nemr" throughout, on Product Owner instruction. Document ID NEMR-SPEC-001 (was AIHUB-SPEC-001); crate `nemr-engine`; CLI binary `nemr`; container ID prefix `nemr-`; container labels `nemr.project`/`nemr.size`/`nemr.volume`; managed storage `~/.local/share/nemr/`; privileged helper `/usr/local/libexec/nemr-volume` and its sudoers rule. Revision history before this entry refers to the same document under its former name. Requires a root reinstall of the helper at its new path and recreation of any existing project, since container IDs, labels and volume paths all carry the name. | Claude Code, per Product Owner instruction |
| 1.17 | Revision | Added Section 3.8 (Container Process Model, PROC-01..04): PID 1 is a long-lived supervisor (`sleep infinity`), `attach` performs a task exec with a fresh TTY per call, and the supervisor command is written explicitly into the runtime spec rather than inherited from the base image. Chosen so container liveness and stop/start state do not depend on shell state, and concurrent attaches do not collide on a shared PTY. Supersedes an implicit assumption in Milestone 4's original create() work; a note in Milestone 4 records the retroactive correction, and non-conforming containers are to be recreated rather than migrated. Sections 1–3 are Product Owner territory under 4A.5; this edit was made on explicit Product Owner instruction. | Claude Code, per Product Owner instruction |
| 1.18 | Revision | Added Section 3.9 (Container Network Model, NET-01/NET-02): project containers share rootlesskit's network namespace, equivalent to `nerdctl run --net=host` under rootless; per-project network isolation is explicitly out of scope for Phase 1, with rootless CNI via `rootlesskit --detach-netns` (requires rootlesskit >= 2.0) recorded as the Phase 2 path. Found at Milestone 5: a container given its own network namespace receives an empty one — loopback only, no egress — and Claude Code failed with `ENOTIMP` against api.anthropic.com. Added risk R-07 for port collisions between projects under a shared namespace. Sections 1–3 are Product Owner territory under 4A.5; this edit was made on explicit Product Owner instruction. | Claude Code, per Product Owner instruction |
| 1.19 | Revision | Section 3.4's repository diagram brought back in step with the actual repository, which had drifted: it was missing `src/lib.rs`, both `mod.rs` files, the two Milestone 1 baseline binaries, the whole `deploy/` tree from Milestone 3, and `src/engine/tty.rs` from Milestone 5. All of these were already recorded as deviations in Section 11; the diagram simply had not been updated alongside them. Added a note that the diagram states current reality rather than an aspiration. Section 11 gained entries for `tty.rs` and `deploy/`. Structural fact rather than a new decision, edited directly on Product Owner instruction. | Claude Code, per Product Owner instruction |
| 1.20 | Revision | Added VOL-06 to Section 3.6: `start` must verify the project's volume is mounted and remount it if not, rather than proceeding. Found at Milestone 6 after a host reboot — container records persist in containerd's database while mounts and loop devices do not, so `start` succeeded against an unmounted volume and gave the container an empty `/workspace` backed by the host root filesystem with no quota. A silent VOL-05 violation: a user could work an entire session believing they were writing to their project, with no error at any point. Auto-remount rather than refuse-and-require-manual-repair, since requiring manual intervention defeats the portability this product exists for. Sections 1–3 are Product Owner territory under 4A.5; this edit was made on explicit Product Owner instruction. | Claude Code, per Product Owner instruction |
| 1.21 | Revision | Recorded the Milestone 7 finding that `attach` allocates a pty only when its own stdin is a terminal, and that this drifts from PROC-02 as written. Added the note under Milestone 5, a Section 11 deviation row, and escalation item E-07. PROC-02 itself left unedited — Section 3.8 is Product Owner territory per 4A.5. | Claude Code |
| 1.22 | Revision | Recorded the PRIV-03/PRIV-06 privileged-helper hardening: the mount point was checked with `is_dir()` (follows symlinks) and then mounted by name, a demonstrated local root escalation (mount an attacker ext4 over `/etc`); the backing file and chown were likewise re-resolved by name after validation (TOCTOU). Rewrote the helper to resolve every managed path to a file descriptor refusing symlinked components and to drive losetup/mount/fchown through `/proc/self/fd`, plus a backing-file flock against concurrent double-mount and inode-identity loop lookup. Added a `version` handshake (protocol 2). Requirement text in Section 3.7 unchanged — it already mandated symlink refusal and validation inside the helper; this is an implementation correction, recorded in Section 11 and escalated for visibility as E-08. | Claude Code |
| 1.23 | Revision | Recorded that the privileged helper attaches loop devices via the `LOOP_CONFIGURE` ioctl (Linux 5.8+), a consequence of moving loop/mount off `losetup`/`mount` subprocesses. The helper checks the running kernel and fails with an actionable error below 5.8; no pre-5.8 `LOOP_SET_FD` fallback is shipped (below the Ubuntu 22.04+ floor, untestable on the reference host). Documented in PREREQUISITES.md Step 0. Section 3.3 (a Section 1-3 requirement) left unedited; recorded here per 4A.5. | Claude Code |
| 1.24 | Revision | Added E-08 (privileged-helper escalation, already fixed) to the Section 9 escalation list for completeness — it was referenced from Section 11 and the revision history but not enumerated in Section 9. Recorded the shared `E-` escalation namespace between SPEC.md and docs/DECISIONS.md so the two do not collide (DECISIONS.md continues from E-09). | Claude Code |
| 1.25 | Revision | Recorded the single-source-of-truth precedence rule for reconciliation (A6) and the recoverable `delete` ordering. Added `nemr reconcile`. Precedence rule captured in Section 11 pending Product Owner promotion to a Section 3 subsection (a new Section 3.x is Section 1-3 territory under 4A.5). | Claude Code |
| 1.26 | Revision | Added the former Section 7 product gates to the Section 9 escalation ledger under the shared `E-` namespace: E-09 engine consumption model (RESOLVED — long-running user daemon, gRPC over a Unix domain socket, with implementation constraints), E-10 non-Linux hosts (open), E-11 open-core seam (open). Applied on Product Owner instruction; `docs/DECISIONS.md` remains the Product Owner-maintained ruling record. | Claude Code, per Product Owner instruction |
| 1.27 | Revision | Recorded the WP-C1 state-locality findings (docs/state-locality.md): a real 3-turn session writes conversation history to /root/.claude/projects on the **rootfs snapshot**, not the portable volume; only the project file lands on the volume. Credentials are a host bind-mount (never on either portable layer); /root/.claude.json holds machine/account identity (machineID, oauthAccount). Findings captured in Section 11 pending Product Owner promotion to a Section 3 subsection (4A.5). | Claude Code |
| 1.28 | Revision | Recorded M8 (WP-C2): session-critical Claude Code state relocated onto the portable volume by surgically bind-mounting `/root/.claude/projects` and `/root/.claude/sessions` from `<volume>/.nemr-state/`, keeping credentials and `/root/.claude.json` identity on the rootfs (D-02). Acceptance proven both with real Claude Code (--continue recalls after unmount/remount) and by a deterministic regression test. Section 11 deviation added pending Section 3 promotion. | Claude Code |
| 1.29 | Revision | WP-B: extracted the §3.2 wrapper layer from `src/containerd/` into a standalone `crates/nemr-containerd` crate, consumed by the engine and designed for the E-09 daemon while remaining usable standalone. Updated the §3.4 repository tree. containerd-level constants (runtime, snapshotter) moved to the wrapper crate; the product-specific cgroup prefix moved onto `ContainerSpec` so the wrapper carries no branding. The M1 connectivity baselines moved into the wrapper crate with the layer they exercise. | Claude Code |
| 1.30 | Revision | WP-B: added `tracing` with span coverage across the lifecycle and a debug mode (`NEMR_DEBUG=1`, `NEMR_LOG=<filter>`). The VOL-03/NFR-04 audit trail now flows through `tracing` at `info` so it remains on by default. Acceptance is falsifiable and was demonstrated: in debug mode the VOL-05 decision point logs the mount check, its result, and the backing device, so a working directory backed by the host root device instead of a loop device is visible on the first run. Added CI gates (NFR-01 Docker-freeness with negative tests, clippy warnings-denied, cargo-deny licences/advisories, host-backed suite + smoke on a clean runner) and scripted the base-image build and CI host provisioning. | Claude Code |
| 1.31 | Revision | Recorded three Product Owner rulings: E-11 open-core seam RESOLVED (engine + wrapper + volume layer + helper + **bundle format spec** open source; sync, lease, storage backends, identity, GUI commercial; test = "can someone use the open half productively without ever paying?"); E-12/D-07 error model RESOLVED (thiserror taxonomy now, as WP D's first commit; gRPC status mapping deferred to the daemon); F-54 `.claude.json` split RESOLVED (field-level allowlist, unrecognised fields stay and are logged). Transcribed from the Product Owner's rulings, not resolved unilaterally. | Claude Code, per Product Owner ruling |
| 1.32 | Revision | WP-D first commit per E-12: added the engine error taxonomy (`src/error.rs`). Variants exist where a caller branches or where a diagnostic needs structured fields; everything else arrives as `Internal` with the `anyhow` chain preserved. `ErrorKind` is the stable axis callers match on (InvalidRequest, Conflict, HostPrerequisite, CapacityExceeded, DataIntegrity, Incompatible, Transient, Internal), so adding a variant does not break callers. The WP-D failure modes named in the ruling each have a home before the code that raises them is written. gRPC status mapping deferred to the daemon boundary. | Claude Code |
| 1.33 | Revision | M9: added `docs/bundle-format.md` (NEMR-BUNDLE-001 schema v1) as a versioned public interface per E-11. File-level, base image by digest, manifest first, fixed 4 MiB chunks each compressed independently so the chunk boundary is a real seam for M13 deduplication and v2 encryption. Manifest carries C1's session-critical/reconstructible classification to enable lazy materialisation. Exclusion policy: credentials unconditionally (D-02), `.claude.json` by field-level allowlist with unrecognised fields staying put and logged (F-54), build artifacts by default. | Claude Code |
| 1.34 | Revision | M9/M10: implemented bundle export and import against NEMR-BUNDLE-001 v1, with `nemr export` and `nemr import` as standalone CLI surface (E-11). Import is decide-verify-extract: schema, base image digest and quota all refuse before a byte is written; chunk and member digests are verified before extraction; member paths are refused if they escape the destination. Acceptance demonstrated end to end with the live API — a session exported from one project and imported into a fresh one recalled its codeword via `claude --continue` without reading any file. Raised F-55 (MCP configuration cannot travel: `.claude.json` is on the rootfs, not the volume). | Claude Code |
| 1.35 | Revision | Ran the chunking spike before M11 (docs/chunking-spike.md) and converted the "M13 without a rewrite" claim in the format spec from a design assertion into a verified result: the v1 manifest absorbs content-defined chunking with no new fields, dedup across an early edit reached 79.1%, chunk identity is codec-independent, and the reserved v2 encryption seam composes. Recorded the two v1 choices that make this true (per-chunk `plain_bytes`; absolute member spans) so neither is changed casually. Code discarded as agreed. Also added the guard-test rule to `.claude/loop.md` and recorded F-56. | Claude Code |
| 1.36 | Revision | F-55 resolved without an M8 bind-mount change: Claude Code natively supports project-scoped MCP configuration in `.mcp.json` at the project root, which is already on the volume, so MCP config travels while `machineID`/`oauthAccount` stay on the rootfs and never reach the exportable layer. D-06 therefore not invalidated; M8 and M10 acceptances re-run regardless and both pass. F-57 recorded and fixed: four secret-absence guards grepped compressed bundle bytes and degraded to no-ops at realistic sizes. | Claude Code |
| 1.37 | Revision | M11 hardening: buried manifest, missing chunk, base image drift, quota mismatch, member digest mismatch and out-of-range member span each refuse with a D-07 error and a test, driven through the real open/check/extract path via a hostile-bundle fixture rather than hand-mutated structs. Every guard proven to go red with its guarded code disabled — which caught one assertion of mine that matched serde's error text by coincidence. Added permanent host-level regression tests for the export self-swallow and the extract() path traversal. No format change: v1 stands. | Claude Code |
| 1.38 | Revision | E-11's offline guarantee is now enforced by a test rather than held by construction: `nemr export` and `nemr import` run inside a loopback-only network namespace with a tmpfs hiding `~/.claude`, and the session must round-trip. Proven to fail both when export takes a network dependency and when it requires a credential — so M12's storage trait cannot silently become a dependency of the open CLI surface. | Claude Code |
| 1.39 | Revision | M12: S3-compatible `ObjectStore` trait in `crates/nemr-storage` — the COMMERCIAL side of E-11 — with R2 first and B2 viable behind the same implementation (they differ only in configuration, never in behaviour above the trait). The seam is enforced by dependency direction and `scripts/check_seam.sh` in CI, proven to fail when the engine depends on the storage crate. The trait is shaped by D-05: `head` and `get_range` answer the common questions with no egress. A conformance suite is executed in tests against the local backend and proven to reject a corrupting one, so the live-bucket acceptance tests the backend rather than the acceptance. Bucket round-trip is the only remaining gap; it needs a credential this session does not hold. | Claude Code |
| 1.40 | Revision | D-08 part 2: base-image resolution is by **digest, not by name**. `nemr import` now asks containerd whether the *bytes* the bundle needs are present — first by the configured reference, then by digest across every local image — so a host holding the right image under any other tag is no longer sent to a registry for something it already has. `Error::BaseImageMissing` is replaced by `BaseImageUnresolved`, which names the digest and lists each attempt in order, because "not present on this host" could not distinguish a missing image from an unreachable registry and those have different fixes. Proven against real containerd by filing the base image under a second reference and resolving by digest, with a control asserting the alias did not pre-exist; proven red by reverting resolution to name-only. Parts 1 (GHCR publish by digest) and 3 (`export --with-base-image`) are not yet implemented. | Claude Code |
| 1.41 | Revision | **F-63 fix — container creation is now lease-protected, and this is a correctness property, not an implementation detail.** containerd deletes resources nothing refers to; a snapshot becomes referenced only when a container record names it, so between `PrepareSnapshot` and `CreateContainer` it is garbage by containerd's own definition. `CreateContainer` does **not** validate that `snapshot_key` resolves, so a collection inside that window produced a container that reported created and could never start. **Requirement: container creation must hold a containerd lease acquired before snapshot preparation and released only after the record write, on both the success and the failure path.** Every lease carries `containerd.io/gc.expire` so a crashed process bounds its leak instead of pinning a snapshot forever. `prepare_snapshot` now takes an explicit lease argument, so no call site inherits the unsafe default. Audit of the wrapper for the same class: `tag_image` has a narrow equivalent window (documented, not exploitable in current call flow); `start_task` and the snapshot removal paths create nothing unreferenced. | Claude Code |
| 1.42 | Revision | D-08 part 1 **scoped down on Product Owner ruling**: `nemr` does not pull from a registry. The base image is resolved locally or the user is given the exact fetch command, targeting the rootless socket and the engine's own containerd namespace — a pull into the wrong namespace or the system daemon would leave the image invisible to nemr and violate PRIV-01 respectively. The earlier requirement to distinguish *registry unreachable* from *digest not found there* is **withdrawn**: nothing can make that distinction without attempting the fetch, and an error claiming it would be inventing a fact where a user is already stuck. Asserted against the real resolver output, including negative assertions that no reachability claim appears; proven red both by dropping the "does not fetch" statement and by reintroducing an invented "unreachable" claim. Registry pull is tracked as **D-10**, a prerequisite of the GUI rather than an optional extra. | Claude Code |
| 1.43 | **Validation** | **Cross-machine session portability demonstrated end to end — the first evidence for the premise the product rests on.** A session created on one host resumed on a second, independently built machine sharing no kernel, no containerd, no credential and no state. Transport was real: exported on the source host, uploaded to R2, downloaded on the destination with rclone, SHA-256 verified identical before import. The destination then answered `claude --continue -p "without reading any files, what were the two things I asked you to do, in order?"` with both instructions **in order**. The ordering exists nowhere on disk — the artifact the session produced carries the marker and the change, but not the sequence in which they were requested — so this is continuity of a *session*, not presence of a *file*. M10 proved a bundle moves between two projects on one host; this is the stronger claim, and the one D-01 and D-06 were designed for. Two hardenings fired on real conditions rather than constructed ones: M11's base-image drift check refused the first import (the two hosts had genuinely different images, F-74), and the D-08 unresolved-base-image error named the digest, listed every place it looked and what it found, and stated its own limits. | Product Owner, recorded by Claude Code |
| 1.44 | Revision | Host provisioning becomes a script rather than a document: `scripts/setup_host.sh` runs preflight refusals (kernel, cgroup v2, `apparmor_restrict_unprivileged_userns`, subuid/subgid, functional userns) before changing anything, is idempotent, surfaces the reboot that cgroup delegation requires as a distinct exit code rather than hiding it, and finishes by running acceptance. The systemd units now ship as files under `deploy/systemd/` and are installed by copy from `setup_host.sh` and `ci_provision_host.sh` alike, so the two cannot drift; the containerd unit shipped is byte-identical to the one proven working on the reference host. Base-image inputs pinned (F-74) and enforced by `scripts/check_base_image_pins.sh` in CI. | Claude Code |
| 1.45 | Revision | **The base image builds reproducibly** (F-74). Pinning the inputs was necessary and not sufficient: two cold builds of fully pinned source, on one host minutes apart, still produced different digests. The remaining variance was file mtimes plus files embedding the build time in their *contents* — apt, dpkg, alternatives and ldconfig caches, npm debug logs, and Node's V8 compile cache. With `SOURCE_DATE_EPOCH` + `rewrite-timestamp=true` and that residue removed in the layer that creates it, **three cold builds produced one digest and two builds without the epoch produced two**, so both halves are load-bearing. Enforced by `scripts/check_base_image_reproducible.sh` in CI, proven red by restoring one `rm -rf`. Cross-host agreement demonstrated: a clean CI runner produced `sha256:2be53736…`, the same digest as the developer host. Bounded claim: `apt-get update` remains unpinned, so this is determinism at a point in time; builds weeks apart may differ legitimately and distribution by digest (D-08/D-10) is still the only way two hosts agree indefinitely. | Claude Code |
| 1.46 | Revision | **Volume release is verified, not assumed** (F-77). A loop device attached to a *deleted* backing file keeps the unlinked inode alive, so the disk space is unreclaimable and no file-level deletion recovers it; the helper could not find such a device by inode and left it attached, and `reconcile` reported "released … (mount + loop device)" for nine volumes while detaching none. **Requirement: a release path must locate the loop device by a route that survives a deleted backing file — the mount table's source device, or the backing path reported by `/sys` — and a reconciliation report must state what the host shows afterwards rather than that the call returned success.** `list` additionally surfaces volume artifacts owned by no project, since a listing built only from container records is silent while orphaned images fill the disk. | Claude Code |
| 1.47 | Revision | **A mount at a project's mount point must be verified to BE that project's volume** (F-28). `is_mounted` answers "is something mounted here?", which is not the question: a mount landing on the wrong path is indistinguishable from the right one by presence alone, and the container is handed a foreign filesystem with nothing reported. Diagnosed from a captured F-63a failure showing `mounted=true`, a live loop device, and a volume containing only `lost+found`. **Requirement: `start` resolves the backing image of the mounted filesystem and refuses unless it is the project's own; a mount that is not loop-backed is refused outright, since it cannot be a volume this engine provisioned.** The decision is a pure function so the production path and its guard are the same code. | Claude Code |
| 1.48 | Revision | **The D-07 error taxonomy is applied, not merely declared** (F-70). Three variants were unreachable while the engine reported those conditions as untyped `anyhow` strings, so a daemon mapping `kind()` onto a status would have answered `Internal` for a user's typo. `validate_name` now returns `Error::InvalidName` and `create` returns `Error::ProjectExists`; the duplicate, unreachable `CapacityExceeded` variant is removed in favour of `QuotaMismatch`, which is produced — `ErrorKind::CapacityExceeded` is unchanged as its class. **Requirement: every `Error` variant must have a construction site; a variant nothing can produce is not a taxonomy entry.** | Claude Code |
| 1.49 | Revision | **Commands that read host state must share an enumeration** (F-79). `nemr list` reported 57 untracked volumes holding 24 GB while `nemr reconcile` reported nothing to reconcile — both correct about the part of the host they looked at, and `list` looked at `/sys` while `reconcile` looked at directories and files. **Requirement: reconciliation enumerates volume artifacts from the same source `list` does, and must account for every artifact `list` reports — released, refused, or explicitly kept. Silence about a reported artifact is a defect.** A cleanup command's false all-clear is worse than a noisy one: it ends the investigation. Verified end to end on the affected host: 14 GB free → 38 GB, 58 loop devices → 1. | Claude Code |
| 1.50 | Revision | **`nemr import <bundle>` restores without a pre-existing project.** The destination name and quota come from the manifest, which has carried both since schema v1 — **no bundle-format change was required**. `nemr import <name> <bundle>` still names the destination explicitly and `--size` overrides the recorded quota. A name already in use is **refused, never merged**: a merge would overwrite one session with another and the damage is invisible until someone opens it, so `Error::RestoreTargetExists` names both remedies (restore under another name, or delete and replace). Restoring used to be three commands, one of which demanded a quota the bundle already recorded. Raises **E-14**: the restore defers AUTH-03's credential requirement so E-11's offline guarantee holds, confined to one call site and asserted to stay there — a narrowing of a Section 3 requirement, raised rather than decided. | Claude Code |
| 1.51 | Revision | **Default output is a summary; the provisioning trace moves behind `--verbose`.** Every command printed its full trace — every privileged call with arguments, every mount, every loop device — which was right while WP A audited the privileged path and wrong as a default. The VOL-03/NFR-04 audit trail now emits at `debug`, reachable via `--verbose`, `NEMR_DEBUG` or `NEMR_LOG`, and nothing is lost. **Two properties are held by test, not by argument:** the fact of elevation stays visible with no flag (`[nemr] elevated: <op> (via the privileged helper)`) while the arguments do not, so a user can always tell privilege was used; and quieting the success path does not quieten the failure path — a failing command still names the project and says what to do next at default verbosity. Both proven red, the first by silencing the default filter, the second by leaking the full privileged command line back into it. | Claude Code |
| 1.52 | Revision | **`nemr status <project>`** — one command for what previously took `nemr list`, `df` and `losetup` correlated by hand. Reports state, container id, volume path, usage against quota, backing image and its presence, loop device, base image reference and the digest present on this host, and the host credential with when it was last written. Two of these were answerable from no command at all: **whether the mounted filesystem is the project's own volume** (F-28) and **whether a credential exists** — the second turned a three-step diagnosis of an expired credential into one line. `mount_is_correct()` returns `None` when nothing is mounted rather than `false`: an absent volume is not a wrong one, and collapsing them would report a healthy stopped project as corrupted. Both distinctions proven red. | Claude Code |
| 1.53 | Revision | **Error-message floor raised to the D-08 standard.** That error — naming the digest, listing every place it looked and what it found, stating its own limits, giving two fixes — is the bar. Five user-facing errors were a single clause with no remedy and are rewritten to name what failed, expected-versus-found where relevant, and a next step: the privileged-helper failure (now leads with the helper's own stderr and the three things actually wrong, in the order they occur), `mkfs.ext4` failure (says the image is left in place deliberately and how to remove it), exec against a stopped project, a volume image that already exists, and a credential path that is not a regular file. **Requirement: every user-facing error names its subject and offers a next step.** Held by a test that drives four failing commands through the real binary and asserts both, proven red by stripping the remedy from one variant. | Claude Code |
| 1.54 | Revision | **E-14 RESOLVED (Product Owner): AUTH-03 now distinguishes materialising a project from using one.** `nemr create`, `nemr start` and `nemr attach` require a host credential; `nemr import` does not. Restoring on a machine where the user has not yet authenticated is the normal case — D-02 makes credentials per-device and non-travelling, so a new user necessarily has a bundle before a credential. The deferral stays confined to one call site, asserted by test. | Product Owner, recorded by Claude Code |
| 1.55 | Revision | **Base image renamed to `ghcr.io/gnrain/nemr-base:0.1.0`** (D-08). Nothing about the image was ever Docker — `docker.io/` is the registry containerd fills in for a host-less name, and nothing was pushed there — but on a project whose NFR-01 forbids Docker at every layer, with a CI gate enforcing it, Docker's registry in the name of the primary artifact misled every reader. **Existing bundles are unaffected, established empirically rather than assumed:** a bundle exported before the rename imported afterwards with the old name removed from containerd entirely. It works because the manifest identifies the base image by **digest** — the reference is documented as a hint — and resolution's second attempt scans every local image by digest. That property is now pinned by `a_bundle_survives_a_rename_of_the_base_image`, proven red by removing the by-digest attempt. | Claude Code |
| 1.56 | Revision | **D-08 part 1: the base image is published to GHCR from CI**, with a digest-divergence check. `scripts/publish_base_image.sh` builds with the same reproducibility flags as the local build and pushes to `ghcr.io/gnrain/nemr-base:0.1.0`, then compares the pushed digest against `image/PUBLISHED_DIGEST` and **fails if they differ or if none is recorded** — the same argument as the binary hash gates: a reference naming bytes nobody verified is a reference to nothing. Registry credentials come from the workflow's own `GITHUB_TOKEN`, so no human hands over a secret (the posture the R2 acceptance established), and `DOCKER_CONFIG` is pointed at a private temporary directory so `~/.docker` stays meaningful as the F-49 tripwire. Deliberately a **separate** workflow, not a required check: its first run must fail, having no recorded digest to compare against, and a required check designed to fail once would block merges for reasons unrelated to the change under review. | Claude Code |
| 1.57 | Revision | **The published base image digest is recorded and the divergence check is armed.** `image/PUBLISHED_DIGEST` holds `sha256:2be53736…` — **the same digest three cold local builds and CI's reproducibility check produced**, so the published artifact is byte-identical to what this repository builds, and the F-74 reproducibility work now closes end to end: reproducible *and* distributed. Raises **F-81**: the package is not pullable without an account (HTTP 403 anonymously, against a 200 control), because GHCR defaults to private — publishing something nobody can fetch satisfies neither D-08's "obtainable without an account" nor E-11. `scripts/check_base_image_published.sh` probes anonymously with a public-package control and is wired into the publish workflow; the fix is a one-time visibility setting a workflow cannot apply to itself. Publish failures are now surfaced by a README badge and an auto-filed issue, since that workflow is deliberately not a required check and a green pull request says nothing about it. | Claude Code |
| 1.58 | Revision | **F-83 (high): project volumes are now mounted `nosuid` and `nodev`.** The privileged helper mounted user-owned ext4 images with flags of zero, so a user could plant a setuid-root binary in an image they control (`debugfs` sets inode uid/mode with no root and no mount) and have the helper mount it with setuid honoured — local privilege escalation. Proven end to end short of the escalation itself, and by the kernel's own mountinfo report. `mount_ext4` now passes `MS_NOSUID | MS_NODEV`; exec stays, because the mount is the container's `/workspace`. Guarded by a regression test asserting the mount options, proven red against the unfixed helper. Surfaced by fact-checking the README, which claimed the helper "can do nothing else". | Claude Code |
| 1.59 | Revision | **E-15: a project runs one named agent, chosen at creation and switchable later.** A new `Agent` type (Claude Code, Codex), recorded on the container label and reported by `status`; the bundle manifest records which agent **produced** the session (v1-compatible both ways — a `#[serde(default)]` field, established empirically, no schema bump). `nemr create` is now interactive when run without arguments — name, size and agent, arrow-key selection, Enter for defaults — with a strict never-hang contract: a flag always wins, prompting happens only when stdin is a TTY and `NEMR_NON_INTERACTIVE` is unset, and with neither a flag nor a terminal a field with a default uses it while one without fails **immediately** naming the flag, never waiting. `nemr switch-agent` changes a stopped project's agent and states plainly that it does not migrate the conversation (one agent at a time; cross-agent migration is a recorded post-daemon epic, and this command is its future home). | 1.60 | Revision | **The base image carries both agents** (E-15). Codex is installed alongside Claude Code, pinned and residue-stripped identically, so F-74 reproducibility survives: two cold builds produced the same digest `sha256:39c5ade9…`, recorded in `image/PUBLISHED_DIGEST`. Measured cost: 200.5 MiB → 330.2 MiB (~130 MB for Codex, as estimated). Both agents verified to run inside a container (`codex-cli 0.149.0`, `claude 2.1.240`), which needs no credential. **What is NOT yet done, and why:** Codex's session-state layout (the WP-C-equivalent empirical capture) and the M8/M10-equivalent portability acceptance require a real Codex session with API round-trips, which needs Codex authenticated on the host — unavailable this pass. The scaffolding (agent field, manifest, image) is in place for it. | Claude Code |
| 1.61 | Revision | **Codex is recorded as implemented-but-unverified** (E-15/F-84). Selecting Codex is now flagged at the point of selection — the interactive menu says so, and `nemr create`/`nemr switch-agent` print a note — because "implemented the same way as Claude Code" must not become "works" by silence. What is unverified for Codex, specifically: its session-state locality (never measured — the M8 relocation may point at the wrong paths), the M8 relocation acceptance, the M10 portability acceptance, and the D-02 credential enumeration (so D-02 is asserted for Codex, not enforced). Verifying it needs a real Codex session with API round-trips, which needs Codex authenticated on the host — unavailable, and running the capture without round-trips would observe an empty directory a real session would have filled, the WP-C failure mode inverted. Tracked as F-84. **Auth ruling (Product Owner): AUTH-01–03 and D-02 are agent-agnostic** — Codex authenticates exactly as Claude Code does, recorded as AUTH-agent above. | Claude Code |
| 1.62 | Revision | **Gemini CLI recorded as the better target for a genuinely *verified* second agent** (E-15), because its free tier is reachable without a purchase, unlike Codex on this host. The scaffolding is agent-agnostic, so only the image entry, the state-locality capture and the credential paths are agent-specific. Not scheduled; recorded so the option is not rediscovered. | Claude Code |
| 1.63 | Revision | **F-85 RESOLVED (Product Owner): the base image is versioned, and every published version is kept.** Normative rule, now a stated property of the artifact: **a base image version tag names a specific set of bytes and must never be reused for different ones. Any content change — a new agent, a new package, an edited Dockerfile — is a version bump, and every published version stays published**, because a bundle records the digest it was built from and can be restored only where that exact image is available (the same discipline the bundle format gets as a public interface under E-11). The two-agent image is bumped to `0.2.0` (`sha256:39c5ade9…`); `0.1.0` (`sha256:2be53736…`, single-agent) stays published. Each version's digest is recorded under `image/digests/<version>` — one immutable file per version, chosen over a single mapping so reuse shows as an edit to a file that must never change. The build refuses to produce a digest that disagrees with the recorded one for its version (catching a Dockerfile change with no bump), and the publish workflow refuses to push different bytes under an existing version. The D-08 pull advice now names **the version the bundle needs**, taken from the bundle's own reference, not whatever version the engine currently defaults to — so the remedy fetches the right image on another machine instead of a confusingly-wrong one. Registry-level immutability: GHCR was checked and no generally-available per-tag immutability toggle was confirmed for container packages, so the repo-side build+publish checks are the enforcement. | Product Owner ruling, recorded by Claude Code |
| 1.64 | Revision | **F-81 resolved and F-86 fixed** (both gate the D-08 remedy). F-81: the Product Owner made the GHCR package public — verified anonymously, `nemr-base:0.1.0` returns HTTP 200 — so a published version is pullable on another machine today; the ledger had it stale one pass too long, confirmed by live probe not memory. F-86: the publish workflow ran its "obtainable without an account" check BEFORE publishing, so a new version (404 until pushed) blocked its own publish — which is why 0.2.0 was unpublished while 0.1.0 was public. The check now runs after publish, and distinguishes 404 (not published) from 403 (private) with the right remedy for each. | Claude Code |
| 1.65 | Diagnosis | **F-78 diagnosed (no fix; do not lengthen the timeout).** The rare "did not exit within 10s of SIGKILL" is not a nemr bug: a bounded, self-sweeping harness (`f78_probe`, 50 runs / 2h cap) caught the `proc_06` payload `sh` in **uninterruptible D-state at `kernel_clone`** — mid-`fork()` — during the kill window, read from `/proc`. SIGKILL cannot terminate a task inside `kernel_clone` until it returns. Unloaded, 50/50 succeeded with zero kill-window D-states (all D was startup runc-exec, max 1620 ms); under CPU oversubscription it reproduced immediately as the fork's D-window stretched past 10 s. The fork-every-second test payload is unusually prone; realistic agents fork rarely. **For the daemon (E-09):** the 10 s bound is correct (lengthening only defers the hang), but under CPU starvation the runtime's timer itself fires late, so a supervisor must treat "process wedged in uninterruptible sleep" as a distinct surfaced state rather than a generic error or silent success. Harness kept as a committed diagnostic. | Claude Code |
| 1.66 | Revision | **F-87: the published-image check verifies every version, not just the current one.** F-85 guarantees old versions stay reachable at the same bytes so old bundles remain importable; `check_base_image_published.sh` checked only the current tag, so that guarantee was asserted, not enforced. It now iterates all of `image/digests/` and, per version, requires anonymous reachability AND an exact digest match — a tag drifting to different content fails as the F-85 violation it is. `NEMR_BASE_VERSION` filters to one version; the default is all. | Claude Code |
| 1.67 | Revision | **E-09 delivered (local-only): the `nemrd` daemon, and the CLI as its gRPC client.** Every command now goes through the daemon over a Unix domain socket (filesystem permissions authenticate; nothing network-facing). The CLI holds no `ContainerdClient` and calls no engine op directly — `check_cli_seam.sh` enforces that in CI, with a control that the daemon must hold the path. Two processes independently mutating containerd was the divergence class WP A eliminated; the daemon makes single-writer **structural**. The daemon is a client of `nemr-containerd`, which stays standalone. A version **handshake** refuses a client/daemon protocol mismatch cleanly (tested end to end, proven red only with both the daemon- and client-side checks removed). Attach is a bidirectional stream — bytes cross the stream, never local paths, so the transport can swap for E-10; the CLI keeps all terminal concerns, the daemon all containerd concerns. Errors preserve their message: the daemon maps F-70's `ErrorKind` to a gRPC status while carrying the full text, so CLI error quality is unchanged. **Lifecycle: autostart-on-demand** (the CLI spawns the daemon if none answers — the gpg-agent pattern, so a fresh install needs no setup); an optional systemd user unit ships for a managed daemon. Because the daemon holds no session state (the container's PID 1 is the supervisor), a daemon crash does not kill running sessions — the next command reconnects to the same containerd state. | Claude Code |
| 1.68 | Revision | **F-78 requirement built into the daemon: `StopOutcome::Wedged`.** A task SIGKILL cannot reap within the window because it is in uninterruptible sleep (a task in `kernel_clone`/D-state) is now a distinct, surfaced outcome — never a generic error, never a silent success. `nemr stop` reports it plainly and exits non-zero, saying the task may still be running. The wall-clock caution is recorded at the timeout: under CPU starvation the runtime's timer fires late, so the branch may be reached after the nominal window; the classification is correct, the timing is not to be trusted as elapsed. | Claude Code |
| 1.69 | Revision | **The VOL-03/NFR-04 audit trail moves into the daemon, and the per-command CLI elevation note is a flagged casualty.** The engine now runs in the daemon, so the privileged-operation audit trail (every helper call, every mount) is written to the daemon log (`$XDG_STATE_HOME/nemr/nemrd.log`, or the journal when systemd-managed) rather than the CLI's own output. The trail is preserved — asserted by test — but the inline note the pre-daemon CLI printed to say "the helper just ran" is gone: the CLI no longer performs privileged operations and cannot narrate them. Surfacing per-command elevation back to the CLI needs the daemon to stream audit events (which the lease/sync future wants regardless); **deferred and raised for a Product Owner ruling.** | Claude Code |
| 1.70 | Revision | **The audit trail streams to the CLI: elevation notes are inline again** (E-09, Product Owner ruling on 1.68's deferred question — build it now). A user can tell that a command they just ran elevated privileges without going to the log, restoring across the daemon boundary the inline note the pre-daemon CLI printed. The daemon captures the engine's existing `tracing` audit events with a Layer and routes each to the client whose command produced it (correlated by a `nemr-request-id` span), streaming them over a new one-way `WatchAudit` RPC — the same channel the lease/sync work will reuse. Privileged (elevation) events show by default; trace lines only under `--verbose`. The durable trail is still kept in the daemon log. The only engine change is a structured `nemr_audit` field so classification is by field, not text. | Product Owner ruling, recorded by Claude Code |
| 1.71 | Housekeeping | **The R2 bucket was deleted and its token revoked; the R2 transport leg of the 1.43 validation is no longer reproducible.** 1.43 stands as written — it is the historical record of what was done, and rewriting it to match present state is how a ledger stops being trustworthy. Recorded here alongside it: the bucket that carried `htmltest.nemr` and the base-image tarball is gone (it was in a friend's account and the risk was not worth managing), so the upload-to-R2/download-with-rclone step of 1.43 cannot be re-run today. Nothing in the repo depended on it — the storage layer is `NEMR_S3_*` env-var driven through the M12 trait with no bucket compiled in, CI never referenced R2, and the base image's authoritative source is GHCR (`ghcr.io/gnrain/nemr-base`), not R2. Concrete-looking R2 fixtures in `crates/nemr-storage` unit tests were swapped to `example.com` placeholders so no future reader mistakes one for a live bucket; the `<account>`/`<bucket>` config templates in doc-comments stay, as R2 remains a supported provider behind the trait. | Claude Code, per Product Owner ruling |
| 1.72 | Revision | **E-16 resolved and the key scheme built: `crates/nemr-crypto`** (WP-J, commercial side of E-11). The server stores ciphertext it cannot read; a random 256-bit master key is wrapped by a password-derived envelope (Argon2id → HKDF-SHA256 domain-separated auth/wrap keys → XChaCha20-Poly1305), so every future access method — OAuth, per-device caching, recovery — is another envelope over the same key with no re-encryption. Recovery is generated **at registration** and confirmed before the account is usable (Product Owner ruling, overriding a defer). New normative §3.10 fixes the Argon2id parameters (19 MiB, t=2, p=1) and the AEAD/AAD scheme; E-16 in `docs/DECISIONS.md` records the D-02-vs-E-16 distinction (Anthropic's per-device credential that never travels vs the user's own bundle key that must travel as ciphertext) and the accepted cost (a forgotten password with no recovery is permanent data loss). `scripts/check_seam.sh` generalised to a list of commercial crates with a per-crate control; the engine depends on none. 19 crypto unit tests, each red when its property is violated. | Product Owner ruling, recorded by Claude Code |
| 1.73 | Revision | **The sync server: `crates/nemr-sync`** (WP-J, commercial side of E-11). Axum + sqlx over **Postgres from day one** (D-03: the lease needs real concurrency, which SQLite cannot give). Four capabilities: **identity** (register → confirm-recovery → login; the client-derived `auth_key` is Argon2id-hashed server-side, tokens are opaque with SHA-256-at-rest and expiry, login is rate-limited per email); **session index** (unencrypted metadata so the list works on any machine — E-16); **bundle upload/download** through the M12 `ObjectStore` trait, filesystem backend, no R2; and the **D-03 lease** with a monotonic fence — a takeover advances the fence so the loser's next heartbeat *and any write it attempts* are refused server-side, which is the fencing the stateless daemon needs when it restarts silently. The server depends on `nemr-storage` but **not** on `nemr-crypto`: it stores the envelopes and ciphertext the client produces as opaque blobs it cannot open. Named `nemr-sync` (not `nemr-server`) to avoid colliding with tonic's generated `nemr_server` proto module, which would false-positive the seam grep. Validated by a Postgres-backed integration suite (register/confirm/login round-trips the master key; two-client takeover proves the loser cannot write; bundle round-trips byte-identically) run in a CI `services: postgres` job. Enumeration resistance on the pre-login KDF-params endpoint is a recorded gap (F-89). | Claude Code |
| 1.74 | Revision | **F-89 resolved — the account-enumeration oracle is closed on both endpoints.** `/v1/auth/params` returned a distinguishing 404 for an unknown email; it now returns a deterministic pepper-keyed pseudo-salt (`HMAC(NEMR_AUTH_PEPPER, email)`, 16 bytes) with the same 200 shape as a real account — unpredictable to an observer, stable across probes like a real salt. The second oracle of the same class, `/v1/login` leaking existence by timing (a wrong password ran Argon2id, a missing email did not), is closed by spending one Argon2id on the miss too. Guard test `kdf_params_does_not_reveal_whether_an_email_exists` goes red if the 404 returns. | Claude Code |
| 1.75 | Revision | **Local sync-test Postgres, mirroring CI.** `scripts/setup_sync_test_db.sh` brings up CI's exact `postgres:16` in rootless podman (matching, not substituting, the CI service container), so a local `cargo test -p nemr-sync` and the CI `sync-tests` job are the same claim — closing the gap where CI was the code's first execution. The role and database come from the image and the schema from the harness's embedded migrations, so there is no manual `createdb`/`migrate` step. Documented in PREREQUISITES.md Step 5 and provisioned (non-fatal) by `setup_host.sh`. | Claude Code |
| 1.76 | Revision | **A structural guard for the fmt lapse.** `cargo fmt --all -- --check` twice reached CI unrun after a local clippy-only check; `scripts/git-hooks/pre-push` now blocks an unformatted push (both workspaces), installed by `setup_host.sh` via `git config core.hooksPath`. The lesson — a fast check CI runs belongs in a hook, not a memory — is recorded in `.claude/loop.md`. | Claude Code |
| 1.77 | Revision | **F-90 resolved: the installed-engine gate is scoped to the engine's dependency closure, and now verifies `nemrd`.** The mtime scan swept `crates/` wholesale, so editing a commercial crate's test reported the engine stale — a gate that cries wolf teaches people to reinstall past it, and then it stops working the day the drift is real. Scoped to `src/`, `build.rs`, `proto/`, `crates/nemr-containerd/`, `Cargo.toml` (the same lesson the helper's gate learned from the other direction). Two real gaps closed while the function was open: `build.rs`/`proto/` are compiled into the binary and were never scanned — a proto edit left the gate green — and `nemrd`, which `install_engine.sh` installs and every command runs through, was never hash-verified; both are covered now. `Cargo.lock` deliberately excluded (workspace-wide: a commercial dep change touches it) with the residual — a bare `cargo update` — named. Proven: a commercial-crate edit no longer trips it; a `nemr-containerd` edit and a proto edit still do; a corrupted installed `nemrd` is caught. | Claude Code |
| 1.78 | Revision | **The open CLI gains cargo/git-style external subcommands, and `nemr list --json`.** `nemr <unknown> …` execs `nemr-<unknown> …` from PATH (args, stdio and exit code pass through unchanged; not-found fails fast with the extension form named). This is the seam-preserving bridge for the commercial half: the open CLI carries **no extension names and no dependency on any extension** — the E-11 grep proves no commercial name appears — yet `nemr login` works end to end when the sync client is installed under that name. `nemr list --json` is the stable machine-readable surface (projects + untracked volumes) so external tooling need not scrape the human table; `ProjectStatus` gains the `agent` label (proto field 9) to describe a project without guessing. | Claude Code |
| 1.79 | Revision | **Server: lease release and logout** (WP-K prerequisites). **Release** lets a clean stop free the lease immediately instead of waiting out the TTL; it **expires the row rather than deleting it**, because deletion would reset the fence to 1 on the next acquire and a zombie holder from a previous hold of the same (holder, fence) could then match the fresh lease — expiring keeps the fence monotonic for the session's whole life, so stale credentials stay stale forever. Only the current (holder, fence) may release (a stale release after takeover would free the winner's lease → 409); a retried clean release is idempotent. **Logout** revokes the presented bearer token server-side — a local-only logout would leave a live token for its whole TTL — idempotently, so it is not a token-validity oracle. Both proven by tests, including fence monotonicity across release and post-logout 401. | Claude Code |
| 1.80 | Revision | **WP-K: the sync client — the engine and the server meet.** `crates/nemr-cloud` (commercial, E-11): one binary installed as `nemr-login`/`-logout`/`-register`/`-sessions`/`-push`/`-pull`/`-release` (argv[0] dispatch), resolved through the open CLI's external subcommands. Register runs the full E-16 enrolment — recovery code shown once and **typed back through the recovery envelope** before the account activates. Push exports via the open CLI, **encrypts client-side**, uploads ciphertext under the lease's fence; pull downloads, decrypts, imports; sessions merges the server index with `nemr list --json`, marking local/remote/both. The lease is live (D-03): pull/push acquire, a detached heartbeat holder renews (setsid, the autostart pattern), a refused renewal or a TTL of silence marks the lease **lost** and the holder exits rather than continue; a stale machine's push is refused naming the current holder — and the server's fence refuses the stale write regardless of client behaviour. The master key never touches disk; each data command re-derives it from the password (per-device caching is the deferred next layer). **Scope note: D-04 upload-on-quiesce deliberately slipped to the daemon-integration pass** — explicit push proves the pipe; automatic sync builds on it. Naming note: the crate is `nemr-cloud` because tonic's generated modules reserve BOTH `nemr_server` and `nemr_client` for the `Nemr` service — the second name collision the seam grep has caught (see 1.73). Acceptance: `scripts/sync_acceptance.sh` — register → create → converse → stop → push → **delete the project entirely** → pull → attach → `claude --continue` recalls two instructions **in order** (the 1.43 form: the ordering exists nowhere on disk); plus 5 client integration tests in CI including the stale-machine lockout proof. | Claude Code |
| 1.81 | Revision | **F-92: six defects found by adversarial review of the sync client, all fixed.** A five-reviewer, twenty-three-verifier workflow over WP-K's new surface; eighteen findings survived refutation. Fixed: duplicate heartbeat holders (`pull` spawned unconditionally, leaking a detached holder per pull whose heartbeats the server still honoured); a stranded lease (a fence advanced while the old holder lived was recorded nowhere, so release sent a stale fence and reported success while the server held the lease to TTL); wrong-process signalling (substring `/proc` matching + PID reuse could SIGTERM another session's holder); heartbeat pacing inferred from the remaining slice rather than the server's now-reported `ttl_seconds`; **the plaintext bundle world-readable in /tmp** (tempdir honours the umask — measured 0775/0664 — exposing the decrypted session E-16 exists to protect); and an off-PATH exec via a `/` in a subcommand name. Five guard tests, each proven red under its true mutation **with the mutation verified to have applied** — a first round of these tests passed under mutation (green over nothing, the F-56 shape) and was rewritten. The upload's in-`UPDATE` fence re-check is kept as defence-in-depth and **explicitly not claimed as tested**, since the pre-flight check masks it. | Claude Code |
| 1.82 | Revision | **F-93: the host CI job provisions Postgres natively, because a service container cannot survive it.** The WP-K acceptance runs in the host job, which needs a database; a `services: postgres` block failed with "no Postgres listening" *after* provisioning had passed, because GitHub service containers are started by Docker, Docker runs on the system containerd, and `ci_provision_host.sh` disables `containerd.service` under PRIV-01 — the job kills the container serving its own database. `scripts/ci_provision_postgres.sh` installs Postgres from the Ubuntu archive instead (no container, matching the existing "Ubuntu archive only" posture), waits for readiness rather than trusting the unit, and verifies the exact connection string the tests use. Also hardened in the same pass: the acceptance's own failure path, which had reported "server did not come up" with an empty log — a failure that erased its own evidence (F-65 class) and would have sent the next reader hunting a server bug. It now probes the database separately and by name, distinguishes a dead server process from a silent one, and says when a log is empty rather than printing nothing. | Claude Code |
| 1.83 | Revision | **E-17 resolved: NFR-01's scope is written down — what we request is in, the provider's substrate is out.** The Docker-freeness gate fired on a comment describing how GitHub service containers start; investigating rather than silencing it established (from our own job logs: `/usr/bin/docker pull postgres:16`, `docker create`, `docker start`) that the `services: postgres` block added in WP-J **is** a Docker dependency, requested by our own workflow. NFR-01 now says so explicitly, and equally explicitly excludes the Docker daemon preinstalled on runner images — because a constraint reaching the provider's substrate is unsatisfiable on every GitHub-hosted runner, and an unsatisfiable constraint is one everyone learns to ignore. The exclusion is stated rather than inferred: "whoever wrote the gate already scanned `.github`" is not a foundation a hard constraint should rest on. **F-94** records the gate's blind spot (it matched a description of Docker while missing three literal `docker` invocations in a workflow it was scanning) and its widening to key on `services:`/`image:` with a two-way control. | Product Owner ruling, recorded by Claude Code |
| 1.84 | Revision | **F-95: the wait discipline becomes a gate, not a rule.** Seven times a wait loop failed without explaining itself; the seventh was written in a script authored after the rule against it existed, which settled that the rule needed to be structural. `scripts/lib/proc.sh` supplies `wait_for_service` / `wait_for_ready` / `require_tcp` — abort the moment the process dies, distinguish alive-but-silent from exited-with-status-N, print the captured log and say so when it is empty or absent, and name a missing dependency separately from whatever needed it. `scripts/check_wait_discipline.sh` fails the build when a script backgrounds a process without the helper; its control shares the real predicate and discriminates three ways, so it cannot go green over nothing. Both run in CI alongside the helper's 18 assertions. `sync_acceptance.sh` migrated and verified, including a red-check where a genuinely broken server surfaces its real error from the captured log. | Claude Code |
| 1.85 | Revision | **WP-M: `nemr port` — a server inside a session is reachable from the host.** The day-one blocker: a dev server in a session listens in rootlesskit's network namespace, so nothing on the host could reach it. Feasibility was established empirically first — project containers **share** rootlesskit's namespace (NET-01, verified by comparing `net:` inodes twice, including after a restart changed them), so one `rootlessctl` forward reaches the session and `--disable-host-loopback` does not block inbound. New **NET-03**: declarations live in the `nemr.ports` container label and are authoritative; the live forward set is **derived** — applied on start, withdrawn on stop and delete, swept by reconcile — so a stopped project never holds a host port against another project while the port still belongs to it across runs. Collisions name the holder (another project by name, versus something else on this host) because the remedies differ; a pre-flight bind check catches the host case on a **stopped** project, where nothing would otherwise bind until start. `127.0.0.1` by default, `--expose` warns at the moment it becomes true. `nemr status` answers "what's my URL". Acceptance: `scripts/port_acceptance.sh` runs a real server inside a session and reaches it with curl, with a control proving refusal beforehand; in CI on every PR. | Claude Code |
| 1.86 | Revision | **F-97/F-98: the port surface, after adversarial review.** A 58-agent review of WP-M raised 52 findings; 21 survived adversarial verification. Two mattered most. **F-97:** `nemr reconcile` removed every rootlesskit forward no project declared — but that table is shared with everything else the user runs, so a cleanup command was silently deleting their own configuration. Ownership is now proven by exact tuple match before anything is destroyed; a forward matching nothing is **left alone and reported**, using the distinction the report already drew between reclaimed and left-alone. **F-98:** the bind address was passed through unvalidated, so `:9000:8000` bound every interface with no warning and no exposed marker (exposure was string-equality with `0.0.0.0`), and a malformed address shifted rootlesskit's output columns so the forward could never be torn down. The address is now parsed, normalised and refused when ambiguous; exposure means **not loopback**; and the warning fires on the bind that resulted rather than on the flag. Also fixed: start-time port failures now reach the user through the audit stream (tagged `warning`) instead of a daemon log nobody opens, and `status` marks declared-but-unbound ports rather than printing them as working URLs. | Claude Code |
| 1.87 | Revision | **NET-02: each session gets its own network namespace.** The feasibility spike answered every question with commands and controls before a line was written, and corrected the estimate: forwarding stays **one hop**, because rootlesskit's port API takes a child IP — so the port work survives rather than being rewritten, and 2–3 weeks became about one. Implemented: the OCI spec requests a network namespace; `engine::netns` wires a veth pair and NAT into it through `nsenter` into rootlesskit's user and network namespaces (where the capabilities are ours, measured `=ep`); the allocation index lives on the container label; `port add` targets the session's own address. Allocation refuses on a host-route overlap with `10.99.0.0/16`. **NET-01 is retired, not deleted** — it was true and twice-verified, superseded because the design changed, with the reason written into its own entry. Also corrected by measurement: SPEC 1.18's premise that NET-02 required rootlesskit >= 2.0 for `--detach-netns`; the veth path works on the archive's 0.14.6. Acceptance, live through the real CLI: two sessions both bound to container port 8000, reachable on separate host ports; three distinct namespace inodes; DNS resolving inside an isolated session; and a real Claude Code API round-trip from one. A leak found by verifying cleanup rather than assuming it — `delete` released the veth but not the NAT rule, because it calls `stop_task` directly rather than through `stop()` — is fixed and re-verified. | Claude Code |
| 1.88 | Revision | **NET-02 after adversarial review, and the intermittent CI failure it exposed.** A 53-agent review of the namespace surface plus a 5-agent hunt for the flake. **The flake first, and its cause is NOT established.** The WP-K acceptance failed with *"pulled project not in nemr list"* having just printed `nemr pull`'s complete success; re-running the SAME commit passed, which rules out a restore path producing something `list` cannot see. Five mechanisms were investigated and all five refuted with commands and controls. The one worth recording is the near miss: `grep -q` exits at its first match, the writer takes EPIPE, and because Rust ignores SIGPIPE `println!` PANICS, which `pipefail` promotes to a failed pipeline while `2>/dev/null` deletes the explanation — a MATCH reported as "not listed", reproduced three times independently and load-dependent, exactly the right shape. It is refuted anyway: the race needs a write AFTER the match, and in that job the project was the last line (strace, the guarded untracked block, and the sort order all agree), which also makes the developer host the vulnerable one and CI immune — the opposite of what happened. So the assertion was fixed rather than the diagnosis assumed, in three places: the CLI restores SIGPIPE to `SIG_DFL` so it dies quietly like any other filter (F-110); the assertion asks its question through `output_has`, which captures and then searches, so no pipe exists to race (F-109); and the host CI job now dumps the daemon log, `nemr list`, containerd's records and rootlesskit's namespace on failure — the daemon's side of every failed command was in `nemrd.log` all along and was simply never collected. The strongest surviving candidate is a transient error inside `nemr list` itself, which lands on exactly the stderr the assertion discarded; if it recurs it now names itself. **The review's confirmed defects:** a wiring that failed halfway left a link whose NAME the next start read as proof of success, so a leaked link became a session that starts, reports success and reaches nothing — and its fixed-name peer end broke every OTHER project's start (F-104); the route-conflict guard missed every /9-/15 supernet of 10.99/16 and treated an unreadable routing table as a clean one (F-105); a project created before NET-02 was started into an empty namespace in silence, upgrading every existing project into a broken one (F-103); allocation failed open on a read error and was an unlocked read-modify-write (F-106); teardown ran whether or not the task stopped, and the test harness leaked a NAT rule per project (F-107). NET-04 and NET-05 are added: **a session's network is a fact about its host, never carried in a bundle** — a restore allocates here rather than reinstating what the source machine used — and NET-02 separates **port spaces, not sessions**, recorded so the namespace is not misread as a security boundary (F-108, needs a ruling). Also fixed on the way: `nemrd` unlinked the socket before validating anything, so a daemon that could not reach containerd deleted a working one's socket, and six live daemons were observed on one machine (F-111). | Claude Code |
| 1.89 | Revision | **F-115: a bundle records the base image the project actually runs on, not the one the exporting engine builds today.** Found while scoping the 0.3.0 base-image bump, which would have widened it from "projects older than the constant" to "every project". `export` filled both manifest fields from `config::BASE_IMAGE` and never consulted the container — for a field the format calls *"the authoritative identity… a session restored onto the wrong base image is a silent-wrong-result defect"*. Import had the mirror: it resolved the bundle's base image, **discarded the result** and built from the destination's own constant. Usually harmless because the constant is usually right; measured on the developer host, three of four projects name an image their rootfs is not, and one names a namespace nobody owns (F-25). Identity now comes from the container's snapshot parent — which IS the chain id of the image it was built from — mapped back to a local image. When no local image builds that rootfs the digest is left **empty** rather than guessed: the unprovable case has to be representable, or the only option left is a confident lie. NEMR-BUNDLE-001 gains `rootfs_chain_id`, `#[serde(default)]` and therefore v1 in both directions, carrying the truth even when the image is gone; import prefers it and **does not fall through** to the digest on a miss. **The Product Owner's ruling decided the order — truthfulness before versioning — on the grounds that a remedy which walks the user into the fault is worse than no remedy.** Three guard tests, each proven red under its own mutation; the export derivation itself is NOT test-coverable on a host whose projects all match the constant, which is recorded as F-116 rather than papered over, with the measurement against the one divergent project standing as the evidence. | Claude Code |
| 1.90 | Revision | **Base image 0.3.0: `curl` and `iproute2`.** Raised by the Product Owner after trying to check a session's own IP and reach the API from inside, and finding neither tool present. The absence was never argued — the Dockerfile justifies what it installs and weighs nothing out, and no SPEC, DECISIONS or CONFORMANCE entry mentions either tool; layer inspection (10,318 paths across 9 tars) confirms `curl`, `ip`, `ss`, `ping`, `dig`, `wget`, `nc` and `ps` all absent, only `getent` present. Ruled: *if it is something I would install every time, it is not a customisation, it is a missing default.* The asymmetry is recorded beside the packages rather than buried: **`curl` adds ZERO new source packages** (libcurl is already present via git), while **`iproute2` brings eight** — iptables, libbpf, elfutils, libbsd, libtirpc, libmnl, libcap2 — widening the apt layer's source surface by ~42% and with it the unpinned-apt drift F-74 already records. Accepted on the argument that a session which cannot inspect its own network is one you cannot debug from inside, in a container whose networking is the newest and least-exercised part of the system; reversible on a measurement. `0.2.0` stays published and old bundles keep resolving (F-85). Digest `sha256:749c092d…`, 332.4 MiB (+2.2 MiB), **verified reproducible across two builds that both provably executed** — F-117 records why that qualifier is load-bearing. | Claude Code |
| 1.91 | Revision | **Declared packages: a session's tools travel with it (F-118).** The premise the original design rested on was falsified by the Product Owner before building: packages already survive stop/start — `stop_task` deletes the task, the writable layer is the snapshot — so same-machine persistence was never the engine's job, and the cache and live watcher were solving problems that do not exist (F-118, F-119). What remains is portability, which has exactly one moment: export → import. **Detection at export** diffs the stopped container's snapshot dpkg state against the base image's (the FIRST lowerdir that has the file — three layers deep on the reference machine) and intersects with apt's manual marks. **The list records intent, not closure** (Product Owner ruling): `apt-get install jq` declares `jq`, never `libjq1`/`libonig5` — a bundle pinning the dependency closure is wrong on any destination whose apt resolves differently. **The consequence is a property, not a defect: the destination's resolution may legitimately differ from the source's — different transitive set, different versions.** The list travels in `.nemr-state/packages.json`, sorted and timestamp-free so bundle determinism holds. **`nemr provision` is explicit and on demand** — `import` suggests it and never runs it, `start` warns (one line, tagged) when declared packages are missing; both paths sit inside tested no-network guarantees that must not be spent. Verification is by outcome, never exit status: F-122 records that `apt-get update` exits 0 with every repository unreachable. Protocol v2 (adds Provision; the handshake refuses a mismatch with reinstall advice rather than an unimplemented-RPC error). | Claude Code |
| 1.92 | Revision | **F-124: the base image version lives in exactly one place.** Deferred out of the 0.3.0 bump by ruling (changing where the number comes from mid-bump risks a half-bumped state); done now with the version stationary at 0.3.0, so the only thing moving is the source of truth. `config.rs` is authoritative; `scripts/lib/base_image.sh` is the single shell extraction with its own control; the probe binaries take the image by env var so the containerd crate stays product-agnostic; and the drift guard reds on any literal reappearing, proven by leaving one consumer hardcoded. A future bump edits one file and the versioning check refuses it until the ledger digest is recorded — measured in the mutation proof, where 9.9.9 propagated everywhere and was correctly refused for having no digest. | Claude Code |
| 1.93 | Revision | **E-10 Windows half resolved: Windows support means WSL2** (Product Owner ruling, 2026-09-02). No native Windows binary, no bundled VM — a Windows user installs WSL2, runs `setup_host.sh` inside it, and works from that terminal. macOS stays deferred, the remote-engine fallback stays rejected. Sequencing reopened deliberately (a GPU test host is needed; the only NVIDIA card is in a Windows machine; VirtualBox cannot pass it through; no disk for dual-boot) and the permanent cost accepted knowingly: WSL2 becomes a second supported host that GitHub's hosted runners cannot exercise, so its verification is manual. Execution is establish-then-support: a spike (`docs/wsl2-spike.md`) proves what breaks before any code is adapted; acceptance mirrors the cross-VM run (bundle round-trip Linux↔WSL2 with history intact, dev server reachable from a Windows browser, `verify_wp_a.sh` green on WSL2 against installed binaries). Section 9 E-10 entry updated. | Product Owner ruling, recorded by Claude Code |
| 1.94 | Revision | **Provisioning now pulls the published base image and builds only as a fallback (F-126)** — the WSL2 spike's first divergence, and a script bug rather than a WSL2 problem: `setup_host.sh` always built, paying ~2 hours a fresh host never owed (D-08 published the image precisely to avoid it) and failing outright once the apt archive drifted past the recorded digest. New `scripts/fetch_base_image.sh`: every path ends at the recorded digest or a refusal naming both digests and the remedy — drift after a successful pull refuses without building (a build would mask the drift), and a fallback build that cannot reproduce the recorded bytes fails closed, because bundles pin the base digest and a divergent image exports bundles no other machine can restore. Proven red-first by a shimmed 8-vector suite with a neutered-copy control, added to CI's constraints job. The spike also settled the reproducibility question the divergence raised (F-127): two kernels (6.8 and 6.18) built identical digests the same day — F-74 holds across kernels but not across archive drift; the first CI provisioning run after the billing reset will fail F-85 by design, pending a ruling on which cost to carry. | Claude Code |
| 1.95 | Revision | **WSL2 Half 2, the mount-propagation fix (F-128)** — the spike's dominant failure, diagnosed then confirmed end-to-end by the Product Owner: WSL2's `/init` leaves `/` a private mount, so the privileged helper's volume mount never propagates into rootlesskit's `--propagation=rslave` namespace, the M8 session-state bind sources vanish for runc, and every project-starting test (12 of 12, the NET-02 ones among them — same cause, they die at `start_task` before wiring) fails with an opaque "no such file or directory". Two halves. **Guard, host-independent:** `engine::volume::mount_propagation` reads the mount's propagation from `/proc/self/mountinfo`; `project::start` refuses **before** the runc riddle, naming the fix. Pure parser, unit-tested red-first (shared/private/slave/escaped/absent), and a no-op wherever `/` is already shared — so it changes nothing on the reference host and the suite stays green. **Prevention, WSL2-only:** `deploy/systemd/nemr-mount-propagation.service`, a root oneshot ordered `Before=sysinit.target` with `ConditionVirtualization=wsl`, runs `mount --make-rshared /` on every boot *before* rootlesskit starts — the ordering the live experiment proved necessary (a live `make-rshared` did nothing until the rootless stack restarted, and did not survive `wsl --shutdown`). `setup_host.sh` installs and enables it and makes it live for the current session, then restarts the rootless stack so it re-snapshots. Acceptance is manual on WSL2 (hosted runners cannot run it): after `wsl --shutdown` and restart, 12 → 0. | Claude Code |
| 1.96 | Revision | **WSL2 Half 2, the two environment gaps (spike divergences 2 and 6)** — the onboarding-path-untested class, not WSL2 semantics. `nemr` resolves the containerd socket itself, but a bare `ctr` and the base-image remedies need `CONTAINERD_ADDRESS` in the interactive shell, which `setup_host.sh` only ever set inside its own process. Fixed: setup writes a marker-guarded managed block to `~/.bashrc` exporting `CONTAINERD_ADDRESS` (and putting `~/.local/bin` on PATH — the warning section 6 could only print), idempotent across re-runs; and `fetch_base_image.sh`'s different-digest remedy now prefixes its `ctr images rm` with `CONTAINERD_ADDRESS=…` so it is copy-pasteable from any shell, matching the engine's own advice text. | Claude Code |
| 1.97 | Revision | **Node and Claude Code are prerequisites nemr detects and instructs for, never installs (D-13)** — the WSL2 spike's divergence 3 (Node and Claude Code absent, a global `npm -g` failing EACCES mid-provision), ruled by the Product Owner. `setup_host.sh` gains a detect-and-instruct step before the credential step: it reports whether `claude` (and `node`) are present and, if not, points at the install instructions — non-fatal, since the engine and its suite provision fully without them (fixtures + placeholder credential). No NodeSource apt repo, no script-run `npm -g`, no bundled Node: NFR-01 applied one layer out (nemr does not reach outside the distribution archive, and E-17 already established NFR-01 covers everything we script or request), the same stance taken toward the GPU/CUDA host (E-10). Recorded in `docs/DECISIONS.md` D-13 and PREREQUISITES.md. | Product Owner ruling, recorded by Claude Code |
| 1.98 | Revision | **E-18: a local LLM on the host GPU — Phase 4 deferred with direction** (Product Owner ruling, 2026-09-05), closing a three-phase by-hand spike that built nothing and will never have CI coverage. Proven: a rootless container on our containerd uses the RTX 3070 through three bind mounts and one env var (fits today's `ContainerSpec`; no toolkit, apt repo or CDI); an 8B model fully resident at 74 tok/s, 8192 context; a real session reaches the server at its NET-02 gateway with NET-05 intact; `ANTHROPIC_BASE_URL` wins over the D-02 credential (the negative control failed to connect rather than reaching Anthropic). Blocked: tool calling under a real agent's toolset, in every combination of three models, two agents, two servers and three endpoints — the models emit correct calls as text and no layer converts them (ollama/ollama#15529; reproduced on llama.cpp's server at the single-tool baseline). Drafted as two upstream reports (`docs/gpu-upstream-issues.md`), not filed. The ceiling, fixed before the first run: 8B on 8 GB is a plumbing proof, not a usable assistant. Recorded in `docs/DECISIONS.md` E-18 with the settled Phase-4 inputs. | Product Owner ruling, recorded by Claude Code |
| 1.99 | Housekeeping | **Revision numbers 1.74–1.78 had been assigned twice.** The sync-side rows (F-89 through the external subcommands, 2026-08-26, continued by 1.79–1.92) and the E-10 rows (the WSL2 ruling through D-13, 2026-09-02) carried the same five numbers, so a citation of "1.76" named two different changes. The later strand is renumbered 1.93–1.97 and the E-18 row 1.98; no row text or order changed, and this row records the correction rather than hiding it. The header's Version field, unchanged since 1.39, now tracks the last row. | Claude Code |
| 1.100 | Revision | **`nemr status` reports the credential's validity, not its presence (F-129).** A present, read-only, *expired* credential put the Product Owner through three in-container logins while `status` said `present, last written 0 days ago` — true and useless, the F-56 shape (a check reporting the wrong property). `status` now reads the OAuth `expiresAt` from the credential file through `auth::credential_expiry`, a pure, **total** parser: milliseconds in, seconds out; a file with no such field — the CI placeholder, an API-key credential — is `Unknown`, never `Expired`, so nobody is nagged on evidence we do not have. It prints `EXPIRED 2 days ago` with the remedy (refresh on the host, then recreate the project, F-12), or `valid — expires in 4 hours`, or `no OAuth expiry to check`. The expiry travels as `StatusResponse.credential_expires_at_secs` so a daemon client — the UI — gets the same fact from the same place; protocol version unchanged, an added field being wire-compatible in both directions. Proven: the parser's three properties (unit conversion, Unknown-is-never-expired, inclusive boundary) each go red under a targeted neuter; the line demonstrated live through a private daemon against expired, valid and placeholder credentials. Measured on the reference host while here: the access token lives 8 hours, the refresh token about two weeks. | Claude Code |
| 1.101 | Revision | **`nemr attach` names an expired credential before Claude Code's login prompt can mislead (F-130).** The credential is mounted read-only — correctly, D-02/AUTH-02 — so `/login` inside a session validates in the browser, fails to persist silently, and the next call reads the same dead token with `401 OAuth access token has been revoked`, an error pointing at revocation rather than at the write; a loop a user runs several times. Not fixed by a writable mount. `attach` reads the host credential first and, only when it can read an expiry that has passed, prints the legible message — refresh on the host, then recreate the project (F-12) — non-fatally, since the shell stays useful. AUTH-03 already requires a credential at `attach`; this makes an *expired* one say so. The gate is proven on real files (expired fires; valid, placeholder and unreadable stay silent) and demonstrated live. Known limit, recorded as F-12's: this reads the host file, so a credential rotated on the host *after* create — the container still holding the old inode — reads valid here while the session is stale; the remedy text covers that case, the detection does not. | Claude Code |
| 1.102 | Revision | **D-02 (f): the credential is mounted read-write, so a session refreshes its own login; AUTH-02 revised; F-12 diagnosed, remedied and detected.** Established by hand before building (`docs/e13-token-spike.md`, Part C): through a writable single-file bind, Claude Code in the base image refreshed a real expired access token — `OK` in 2 s, the file rewritten **in place** (same inode), both tokens rotated — and the host continued on the rotated pair; the same run under a read-only bind is the F-130 loop. The host's own Claude Code, by contrast, rewrites the file by rename (new inode), so a running session's file bind stays pinned to the previous file (F-12, reproduced on a real project) whose refresh token has been rotated away; `stop`/`start` re-resolves it (proven), so the remedy is a restart, not delete-and-recreate. The enumeration the ruling asked for: `~/.claude` on a real host holds the credential, `history.jsonl` (cross-project prompt history), `projects/` and `sessions/` (shadowed by the M8 binds from the volume), `settings.json` (preferences — F-131), and caches, plugins, daemon state; identity (`machineID`, `userID`, `oauthAccount`) lives in `~/.claude.json`, outside the directory, never mounted. So: **not the whole directory** — one file becomes writable, nothing else moves. Built: the bind is read-write; `start` migrates a pre-(f) record (`make_bind_writable`, additive, idempotent, refuses to invent a mount; `NEMR_TEST_PRE_F12` creates the old shape and the regression proves the running task's mount reads `rw` from its `mountinfo`, with a read-only control on the spec — the F-112 discipline); the credential report gains the refresh token's expiry, the blanked shape, and an F-12 detector that compares device and inode of the task's view (`/proc/<pid>/root`) with the host's — read, never repaired; `status` and `attach` share one report, so a routine eight-hour access expiry is no longer a warning and the three states a session cannot recover from are. The exposure, stated: every process in a session could already read the credential; now it can write it — refresh it, blank it, or overwrite it — a same-user boundary, and the reason the file mount stays the only writable thing from `~/.claude`. | Product Owner ruling, recorded by Claude Code |
| 1.103 | Revision | **D-02 (f), the condition: every credential rewrite is observed, and F-12 dies.** The Product Owner approved the writable credential with one condition — the mount cannot tell a refresh from an overwrite, so a session that replaces the host's login with junk must be *observable*: which session, when, and whether what it left parses. Built as one daemon component, `daemon::credential_watch`: an inotify watch on the host file (inode-based, so a write through a session's bind is seen regardless of namespace) and on its directory; each rewrite is attributed by elimination and says so (sessions write in place because a rename over a mount point cannot succeed; the host writes by rename — so an in-place write names the running session when one is running, the list when several, the host when none), the result is parsed and named (a valid credential, a spent one, a blanked one, or NOT a credential), and the record goes to the daemon log as an audit line and to `nemr status` as `last rewrite`. Not the per-request audit stream, which is correlated by request id and a session's write happens outside any request — stated, as asked. **F-12 dies with the same events:** when the host replaces the file, every running session is re-bound to the current one — the stale bind detached, a detached clone of the host file (`open_tree`, carried as a descriptor because the host path is not visible inside the task) moved onto the mount point (`move_mount`) from inside the task's mount namespace, in a single-threaded child of the daemon binary (`nemrd __rebind`) because joining a user namespace refuses a multi-threaded process; `attach` does the same for a replacement that landed while the daemon was down. Two kernel facts found by hand and recorded: the stale bind's dentry is unlinked, so it must be detached before anything can be mounted there (`ENOENT` otherwise), and `/proc/self/fd` cannot name the source inside the task (its `/proc` is the container's). Proven: the watcher's three properties red under neuters; the inotify plumbing against a real file (in place, replaced, in place again on the new inode); on the host, a running session reads STALE after a byte-identical host rename (control), is re-bound without stop or start, then agrees with the host and stays `rw`, and a second call is a no-op. `status` shows STALE as a fact with the automatic remedy; `attach` no longer warns on it. | Product Owner condition, built and recorded by Claude Code |
| 1.104 | Revision | **The UI's HTTP surface — the handshake, with its control (E-11 ruling of 2026-09-06: commercial process, gRPC client of the daemon; the daemon keeps its socket; no port opens unless the user starts the UI).** `nemr ui` (the commercial binary's `ui` subcommand, reached through the open CLI's external-subcommand seam) binds a loopback port that exists only while it runs, mints a 32-byte launch token, writes the launch URL 0600 to the user's state directory — the same file-mode trust as the daemon's socket — and puts the token in the URL's *fragment*, which browsers never send in a request or a referrer. The page exchanges it **once** (custom header, constant-time compare, spent on first use) for a session cookie that is `HttpOnly` and `SameSite=Strict`, then scrubs the fragment. Every request must carry this origin's `Host` (DNS rebinding) and, if it carries an `Origin`, this origin (cross-site); every `/api/*` request must carry the cookie **and** a custom header a cross-site form cannot add; no CORS header is ever emitted. Stated, not hidden: another process of the same user can read the token file — the boundary the socket has today. **The control:** a request without the cookie is refused (401), as is one with a cookie no exchange produced; the matrix — wrong token, spent token, cookie without the header, cross-site origin, wrong or missing host, no CORS headers, token only in the fragment — runs as tests against the router with no socket, and each property is proven red under a targeted neuter. No engine, daemon or protocol change; `check_seam.sh` unchanged. The routes are two (`/auth/session`, `/api/ping`): the flow comes next, on this handshake. | Product Owner ruling, built and recorded by Claude Code |
| 1.105 | Revision | **`nemr-daemon-api`: the daemon's API as its own open crate — the E-11 seam of 2026-09-06.** The proto build, the generated client and server stubs, the socket path and the connect-and-handshake client (with its audit stream) moved out of the engine into `crates/nemr-daemon-api`, so a client of the daemon — the open CLI today, the commercial UI process next — links the API rather than the whole engine (the containerd wrapper, the volume layer, the bundle code). The engine re-exports `proto`, `daemon::client` and `daemon::socket` from it, so every existing call site reads unchanged, the same shape as the `containerd` re-export after the wrapper's extraction. `proto/nemr.proto` stays at the repository root as the daemon's public interface; the new crate compiles it in place. The installed-engine freshness gate (F-90) now scans the new crate instead of the removed root `build.rs`, so a proto or client edit still reports the installed engine stale; `check_seam.sh` scans the crate's sources as open. No protocol, behaviour or wire change; the protocol version stays 2. The crate had been referred to as landed before it existed (the phantom-report rule, loop.md); this row is where it lands. | Claude Code |
| 1.106 | Revision | **The first `claude` in a session opens ready (F-131, extended to identity and onboarding).** The feature as a user experiences it: signed in once on the host, `nemr create`, `start`, `attach`, `claude` — already authenticated, no theme picker, no login method, no browser. What happened instead: a fresh session's `.claude.json` is absent, so Claude Code ran first-run onboarding — the theme picker, then "Select login method" — beside a perfectly valid token. Measured on a never-used session, driven interactively under a pty: nothing seeded → theme picker and login prompt; identity (`oauthAccount`) only → theme picker, and the login prompt after it; the two onboarding flags only → neither, then the workspace trust dialog; the flags plus a fresh `/workspace` trust entry → **the ready prompt**, and Claude Code then populated the identity itself from the token (eighteen fields). So the login prompt is a step of onboarding, not a consequence of missing identity, and the token was never the problem. Built: at the first `start`, the engine seeds `/root/.claude.json` inside the session by **allowlist** from the host's — `hasCompletedOnboarding` (asserted true: the credential's presence is the onboarding), `lastOnboardingVersion`, `theme` and `oauthAccount` when the host has them — plus a fresh `projects["/workspace"].hasTrustDialogAccepted`; never the host's `projects` map, `machineID`, `userID` or caches. The write is guarded by `test -e`, so a session that has run `claude` is left alone; a failure to seed is a warning, never a failed start. Proven: the allowlist's properties red-first (the host's `projects` map made to cross; `machineID` made to cross); on the host, the first start seeds and a second start does not overwrite; `docs/first-run-acceptance.sh` drives a never-used session's interactive `claude` and reads the screen — no theme picker, no login method, no trust dialog, the ready prompt — keeping everything it captured (F-132). | Product Owner ruling, built and recorded by Claude Code |
| 1.107 | Revision | **`create` refuses a credential that cannot authenticate (AUTH-03, extended).** A blanked credential (Claude Code's dead-token clear) or one whose refresh token is spent looks present, so AUTH-03's missing-credential refusal never fired and a session provisioned against it failed at its first request with an error naming revocation. `create` now refuses those two states before provisioning anything, with the host-side remedy; an expired access token with a live refresh token is routine and passes, and a non-OAuth file is not judged (controls). Proven red-first (the blank state made to pass) and on the host: a create under a HOME holding a blanked credential is refused before any volume exists. | Claude Code |
| 1.108 | Housekeeping | **The `htmltest` protected-subject entry is retired; the artifact is gone.** The Product Owner deleted every project on the reference host to reclaim disk (83% consumed), `htmltest` among them, and no bundle of it survives there — searched: no `.nemr` anywhere on the host, only containerd's task directory for it under the runtime root, which is not a bundle; the one candidate copy is the WSL2 box (E-10). The roster in `scripts/lib/proc.sh` and the Rust harness's list are both empty, with the note, and the agreement test between them still runs; the guard **mechanism** stays armed and stays proven — the shell test now marks a disposable name protected for the length of its assertions and shows the same name passes once off the list (the Rust harness already proved it that way, F-123); the first-run acceptance's cleanup consults the roster instead of a hardcoded name. An empty list guards nothing and says so; the next irreplaceable subject goes back on both lists. | Claude Code |
| 1.109 | Revision | **The server's session list names the live lease holder.** `GET /v1/sessions` carries `held_by` and `lease_expires_at_unix` — a `LEFT JOIN` on the session's lease **where it has not expired**, so a released or lapsed lease (release expires the row, 1.79) reads as nobody. "Open on `<machine>`" is the D-03 state a user must see before pulling, and no list — server, CLI, or the coming UI — could show it. The client reads the two fields with a default, so an older server still lists; `nemr sessions` gains an OPEN ON column. Proven by the server's test (held: the holder named; released: null — and red when the expiry condition is removed from the join) and the client's merge test. | Claude Code |
| 1.110 | Revision | **The UI flow, steps 1 and 2 — login and the session list — on the handshake (ruled 2026-09-06: login, list, pull-and-start, attach, stop-and-push; least tooling).** Built as three things. **A shared core** (`nemr-cloud/src/core.rs`): what `login`, `register`, `logout` and `sessions` *do*, with the password as a parameter and the result as a value — no prompt, no print — so the CLI (which prompts and prints around it) and the browser (which does neither) run the **same** login, and the two can never disagree; the CLI's commands were rewired onto it with no behaviour change. **The routes**, behind the handshake's cookie-and-header guard like `/api/ping`: `whoami`, `login`, `register` and `register/confirm` (the recovery code shown once, typed back through the envelope — a wrong code refused with the registration kept, exactly the CLI's step), `logout`, `sessions`. The KDF runs in this process on a blocking thread; the password crosses only the loopback under the guard and is held for the request alone; what is stored is what `nemr login` stores (token, public KDF material, the sealed envelope) — E-16 unchanged. The list is the server's index merged with the daemon's projects, asked over the daemon's socket **through `nemr-daemon-api`** (the UI is the daemon's gRPC client; it does not link the engine and does not scrape the CLI); a daemon that cannot be reached is reported in the reply, and the list still renders from the server alone. **One page**, no framework, no build step, no external resource: the same HTML shows the login form or the list by asking `whoami`; a row says local / remote / both, running or stopped, size, last update, last machine, and who it is open on (1.109). **Proven:** the four routes refused without the cookie — and refused **by the guard**: the first form of this test stayed green with `sessions` moved outside the guard, because the handler's own "not logged in" answers 401 too (a green over nothing, the F-56 shape), so the test now reads the refusal's body and is red under two verified neuters (`sessions` outside the guard; the guard waving `login` through); end to end against the real sync server in-process — a wrong password refused and the machine still logged out (red when a refusal is reported as success), the right one stored as the CLI stores it, the list marking a local running project *local* and a remote one *remote* with its holder named (red when the merge drops the holder), logout clearing the account and the list refused again; registration through the surface with a wrong code refused (red when the envelope check is bypassed) and the shown code confirming with the transcription slips the CLI forgives. **Live**, with curl against the real sync server and the real daemon: the exchange, register, the wrong code, the right code, logout, a wrong password, the right one, and the list carrying this host's running project as the daemon reports it through the seam; the account file 0600 with no key material; both ports closed on exit. Not built yet, on purpose: pull-and-start, attach (xterm.js over WebSocket), stop-and-push — the Product Owner asked to see the shape of the first two first. | Claude Code |
| 1.111 | Revision | **An unconfirmed registration no longer takes the email with it.** The Product Owner asked what the server holds after a registration that never confirmed. Measured against the shipped server: the row is inserted at `register`, a second `register` with the same email is refused as a duplicate (409), and `login` with the right password is refused as pending — while the client's own abandonment message promised a confirmation path from `nemr login` that does not exist. So the trap was real, and not browser-only: a CLI whose terminal closed between the code being shown and typed back was in the same state, the browser (a reload) only made it likelier. Fixed on the server: a registration whose recovery was never confirmed **holds nothing** — login is refused before any token exists, so no session, bundle or lease can hang off it — and registering the same email again **replaces** it atomically with the new material (`INSERT … ON CONFLICT DO UPDATE … WHERE status <> 'active'`); an active account is never touched and still answers 409. The replaced attempt's recovery code confirms nothing; the new one's does, and the new password logs in. The client's message now says to register again. Proven: the new test red against the shipped server (409), red with the active-account guard removed (an active account replaced), green with the fix; the identity suite whole. | Claude Code, on a Product Owner question |
| 1.112 | Revision | **The UI flow, step 3 — pull-and-start.** The sync core now holds `push`, `pull` and `release` too, with the engine behind a three-verb trait (`list`, `export`, `import`: the CLI's subprocess, the UI's daemon client) and progress through a callback, so the browser's pull IS the CLI's pull: the same lease (reuse a healthy hold, retire a stale holder before a new fence, F-92), the same private temp directory for the plaintext (0700 before anything is written), the same decrypt-here. A lease held elsewhere is a **typed** error the core does not render — the CLI appends its `--take-over` flag, the page offers a take-over button naming the holder — so neither driver knows the other's remedy. The UI runs a pull as a **job**: `POST /api/sessions/{name}/pull` (password and take-over in the body; the password is used on a blocking thread and dropped with the request, the CLI's per-command policy) starts it, `GET /api/jobs/{id}` is polled by the page, and every line the CLI would have printed appears as it is said; after the import the daemon's `Start` runs and the row turns to running. `POST /api/sessions/{name}/start` starts a session already here. The daemon is reached for `List`, `Import`, `Export`, `Start`, `Stop` over its socket through `nemr-daemon-api`, from blocking code on the UI's own runtime. **Proven:** the CLI's nine integration tests unchanged on the moved core; the surface test against the real server — a bundle another machine pushed is refused while that machine holds the lease (holder named), refused on a wrong password before anything is imported, and with the take-over comes down as the bytes that were pushed, from a 0700 directory, imported and started, the list then saying here, running, held by this machine; red when the take-over flag is ignored, when the directory's mode is left to the umask, and when the start after the pull is skipped. Live: a real project created, pushed from the CLI to a local server and deleted, then pulled and started through the surface with curl, and `nemr list` showing it running. | Claude Code |
| 1.113 | Revision | **The UI flow, step 4 — attach: the daemon's stream in the browser.** The daemon's `Attach` RPC (the same stream `nemr attach` drives — start with the terminal's size, bytes down as stdin, stdout and stderr up, resizes, the exit code) is bridged to the page over a WebSocket and drawn by xterm.js, **pinned (5.5.0, with the fit addon 0.10.0) and served from the binary** with its MIT licence beside it — no external resource, the page still loads with the network gone. **The gate, stated:** a browser cannot put the custom header on a WebSocket handshake, so `/ws/attach/{name}` does not rely on it. It requires the session cookie; an `Origin` that is present and this origin (every browser sends one on a WebSocket handshake, so its absence is a non-browser and its wrongness a cross-site page); and a **single-use ticket** minted by the guarded `POST /api/sessions/{name}/attach-ticket` — bound to that cookie and that session name, spent on the handshake right or wrong, dead after thirty seconds — which carries the guard's proof into the one request that cannot carry the header. The page's first frame says its size; typed bytes are binary frames down; the session's output is binary frames up; a resize is a text frame; the exit or the daemon's refusal (a session not running) is the last text frame, then the socket closes. A closed page sends the session EOF, so the shell inside exits rather than lingering. **Proven** with a real WebSocket client against the served router and a fake session that echoes: refused without the cookie, without an Origin, with a cross-site Origin, without a ticket, with another session's ticket, with a spent ticket, and with a ticket minted under another cookie; accepted once with the right one; the size reaching the start, typed bytes coming back, a resize reaching the session, the exit closing the socket with its code, and a refused session refused in the first frame. Live: a session pulled through the surface in 1.112 is attached through the browser's path with a WebSocket client, a command typed, its output read back. | Claude Code |
| 1.114 | Revision | **The UI flow, step 5 — stop-and-push, and the flow is whole.** `POST /api/sessions/{name}/push` (password, release and take-over in the body) runs as a job like the pull: it **stops the session first when it is running**, because the export needs a quiescent volume and `core::push` refuses a running project — enforcing the CLI's rule without the CLI's remedy would be refusing the user for a state the button could fix — then runs the same `core::push` the CLI runs. Releasing the lease is the default from the browser, because pushing from here is handing the session on; `POST /api/sessions/{name}/stop` stops without pushing. The page grows a **push** button on a stopped local session and **stop & push** on a running one, with the same take-over offer on a lease held elsewhere. **A defect the test found before the code shipped:** the first form stopped the session and *then* derived the master key, so a mistyped password cost the user a running session for a push that was never going to happen. The password is now checked first (`core::verify_password`, the same envelope open `push` makes), and the ordering is proven red under a neuter. Proven against the real server: a wrong password refused with nothing stopped and nothing uploaded; the real one stopping, exporting, encrypting and uploading, with **what the server stores decrypting under this account's key to exactly the bytes the engine exported** and not containing them in the clear; the lease released so another machine can take it; the row then reading stopped, pushed and unheld. Red under four neuters (the password checked after the stop, the running session not stopped, the bundle uploaded as plaintext, release ignored). | Claude Code |
| 1.115 | Revision | **The browser acceptance: the whole flow, through the page (the Product Owner's words, verbatim).** `docs/ui-acceptance.sh` with `docs/ui-acceptance.py` — the page written as a script, speaking only the surface's own HTTP and WebSocket (the launch token exchanged once for the cookie, the guarded `/api` calls with their custom header, the attach socket with its Origin and single-use ticket). Nothing reaches around the surface; the single stated exception is `nemr create`, which makes the session that is later pushed, because creating a project is deliberately not in the UI's first pass. **21 assertions, PASS:** register (recovery code shown once, typed back through the envelope), log out and log in again with the password alone, create and hold a live conversation in the browser's terminal, stop-and-push, delete the project entirely, see it *remote with a bundle and held by nobody*, pull it into a fresh project and start it, find the session state **byte-identical** through encrypt-push-delete-pull-import, attach and **continue the conversation** — `1. SAY-APRICOT / 2. COUNT-TO-NINE`, both instructions recalled **in order** from a transcript that had travelled — then stop and push again and see the row stopped, pushed and released with the stored bundle rewritten. **The false pass that came first, and its control.** The first run reported a live conversation that never happened: the capture began with the shell's echo of the command just typed, and the prompt named the answer it wanted ("reply with exactly: STORED-BOTH"), so the assertion matched its own input. The attach helper now brackets each command with sentinels the **shell assembles from pieces** — the typed line carries `${S}${T}-BEGIN`, only the shell's output carries the assembled marker — and returns only what the command wrote; waiting for the closing sentinel also closed the race where the capture stopped while the command was still writing. A control now stands in front of every terminal claim: a command carrying a token it never prints, whose capture must **equal** its output exactly, from a raw screen that demonstrably carried the typed line. (Asserting by equality rather than by searching the raw is itself a correction: a terminal wraps by emitting a carriage return mid-token, so an exact-string search on the raw screen is unreliable by construction — measured, `two in\rnstructions`.) Measured on the way and stated: the pty is the size the page asks for (`stty size` returns it), and the wrap column of the *echo* varies only with when that size settles — never affecting program output. | Product Owner acceptance, built and run by Claude Code |
| 1.116 | Revision | **Three page defects found by hand after 1.115, none caught by the acceptance — fixed, and the acceptance now reads the page itself.** F-1: the login and register forms stayed on the page after login. F-2: on `exit` in the attached shell the terminal panel stayed, showing "the shell exited (0)". F-3: action panels stacked — a pull form and a push form open at once under one heading. **The root cause of F-1, measured before any fix:** the page hides everything with the `hidden` attribute, which only works through the browser's default `display: none`, and an author `display` rule on the same element outranks it — `form.login { display: grid }` kept the register form rendered with `hidden` set (Firefox: `hidden=true`, computed `display=grid`). The same defeat, through the inline `display: grid` on the pull and push forms, is why F-3's cancel never took; but F-3 had a second cause of its own: opening an action never closed the other form, so pull then push stacked with `hidden` working perfectly. The acceptance saw none of it because it drove the surface's HTTP and WebSocket, never the DOM. **Fixed:** `[hidden] { display: none !important }` makes hidden mean hidden; on authentication every auth form goes and the header carries the account and the log-out control, the forms returning only through log out or an auth refusal — and either of those closes every panel first, so nothing from before comes back with the next login; exactly one action panel is open at a time — opening one closes the other, cancel closes, and **completion closes, ok or not**, the outcome moving to the status line (the error in red, and for a lease held elsewhere the take-over offer beside it); on shell exit the terminal closes exactly as detach does, the exit code is one line of status above the table, cleared by the next attach, and the session stays running — shell exit is not stop, as after `nemr attach`. **Found by review before the PR opened, and fixed:** an action the user superseded (a job still running when another panel was opened, a shell whose exit arrived after the next attach) could close or write into the panel that replaced it, and a socket the user replaced could null the socket that replaced it — every change of what is open now bumps a generation, and a job's poll, a job's completion, a shell's exit and a socket's handlers act only under the generation they started with; and the exit line was written and then overwritten by the list refresh that follows it, so it is written after the refresh. **The acceptance reads the page:** `docs/ui-acceptance.py` gains a `Browser` that drives Firefox headless over WebDriver BiDi with nothing but the `websockets` module (the snap needs its profile under `$HOME/snap/firefox/common`; one under `/tmp` is invisible to it and Firefox then refuses on the user's own profile), and a page block that logs out and in through the real form, opens pull then push, starts a session, attaches, types `exit` as **real keyboard input**, reads the DOM with `checkVisibility()` — the attribute would have been green all along and hidden the finding — and leaves the machine logged in as it found it. Twenty-two page assertions, green with the fix; **the four fixes each red under a neuter:** the `[hidden]` rule removed reddens the after-login form checks and the pull-then-push check (F-1 and F-3: the rule is load-bearing); the login form no longer hidden on authentication reddens the after-login check (F-1); `openJob` no longer closing the other panels reddens the pull-then-push check (F-3); shell exit no longer closing the terminal reddens the after-exit check (F-2). The acceptance's count rises from 21 to 44. | Claude Code, on Product Owner findings |
| 1.117 | Decision | **E-19 — the UI launcher, and where the server address and the auth pepper live (F-4).** Recorded in `docs/DECISIONS.md` E-19 for the Product Owner's ruling; no code in this row. The facts: 1.104 names `nemr ui` and the page's own error text tells the user to run it, but `scripts/install_sync_client.sh` lays every commercial name except `ui`, so the launcher exists only as `docs/ui-acceptance.sh` running the build tree's `nemr-cloud ui`; the client's built-in server (`127.0.0.1:8080`) disagrees with the acceptance's (`18090`) and the acceptance hides it by exporting `NEMR_SERVER_URL`; the server takes its address, database, bundle directory and F-89 pepper from the environment on every start, an unset pepper being random per process so enumeration resistance resets on every restart. **Recommended, three parts:** (a) `nemr ui` is the extension form — `nemr-ui` on PATH, one name added to the install script's list, both acceptances launching through PATH from that list, the open CLI untouched (a built-in is the erosion `check_seam.sh` cannot see: it matches crate names, not verbs); (b) the client remembers the server of its last successful login or registration in `$XDG_STATE_HOME/nemr/cloud/server-url` (0600, never deleted by `logout`), precedence flag or form field > `NEMR_SERVER_URL` > remembered > built-in; (c) every server setting and the pepper in one 0600 `$XDG_CONFIG_HOME/nemr/sync.env` (`NEMR_SYNC_ENV_FILE` overrides; empty means none) that **the server reads itself**, process environment winning key by key, a wider mode refused and not repaired, key names logged and values never, the pepper generated into it once by a documented one-liner so it never touches argv or history. One rule on both halves: a per-invocation environment beats a persistent file. Nine controls the follow-up PR must carry are listed in the entry, each seen red first. **Ruled by the Product Owner, 2026-09-07:** as recommended, and the open question decided the other way — a server with no pepper **refuses to bind**, naming the file and the exact line to add; one explicit escape hatch, `NEMR_AUTH_PEPPER=ephemeral`, starts a throwaway server with a random pepper and a loud warning, and the acceptances use it. | Proposed by Claude Code; ruled by the Product Owner |
| 1.118 | Decision | **E-20 — the sync server takes its storage backend from the environment (F-5; the mechanism half of D-05).** Recorded in `docs/DECISIONS.md` E-20 for the Product Owner's ruling; no code in this row. The facts: `nemr-sync` reads `NEMR_BUNDLE_DIR` as required and builds `LocalStore` unconditionally under a header promising that R2 will be a config change; the M12 `S3Store` is consumed by nothing but `bucket_roundtrip`, so D-05 is open in practice and every bundle any server has held sits on one machine's disk — the arrangement D-01 was ruled to avoid. **Recommended:** the backend's own variables are the switch — complete `NEMR_S3_*` (provider validated as exactly `r2`, `b2` or `s3`; a typo no longer becomes the generic endpoint silently; `b2` starts with a warning citing D-09) names an object store, `NEMR_BUNDLE_DIR` names a directory that must already exist (kept for both acceptances, the harness and self-hosters); **exactly one, no default**: both set is refused naming both, neither is refused naming both, empty means unset, a half-configured S3 is the existing four-name refusal and never a fallback to a directory; no selector variable, because the exclusive rule leaves one ambiguous case and refuses it, and a selector above the trait is vendor knowledge E-11 forbids; **one egress-free list under a prefix no bundle key can match must succeed before the database is migrated and before the port binds**, so a listening server is one whose store answered, and a wrong bucket or revoked key stops the server at start with a credential-free message instead of a user's 500 on the first push; the credential never in argv, history or the journal (the startup line prints `provider:bucket`, never the endpoint, which embeds the R2 account id); `NEMR_S3_*` live in E-19's `sync.env`. The client is untouched: it uploads ciphertext to an opaque server (E-16). Guard tests for the follow-up PR are listed in the entry, each red first with a control beside it. Three side questions for the ruling: a `nemr-sync` user unit in the same PR; `_FILE` secret indirection as its own finding; `docs/ui-acceptance.sh` staying local-only with a log read-control. D-05's commercial half — the launch default and whether tiers map to providers — stays in D-05 and becomes a config change under this mechanism; CONFORMANCE M12 stays outstanding until the Product Owner runs the CLI acceptance once against a live R2 bucket, and the same run with `b2` closes D-09. **Ruled by the Product Owner, 2026-09-07: accepted as written.** The implementation proves the R2 path against a real bucket — a push from the browser landing as ciphertext, a pull on a locally deleted project coming back byte-identical, the server never seeing plaintext — red under a neuter that swaps the backend out; that proof closes D-05. | Proposed by Claude Code; ruled by the Product Owner |
| 1.119 | Revision | **F-6: the acceptance asserts its own count.** `docs/ui-acceptance.sh` printed its assertion count and never checked it, so a run that skipped a step — Firefox missing, BiDi failing to connect, a block short-circuited — would have said PASS with fewer assertions: the green-over-nothing shape `verify_wp_a.sh` guards against for the regression suite. `EXPECTED_ASSERTIONS=44` now stands at the top of the script and the verdict fails when the count differs, naming both numbers; a step added without raising the number fails the same way. Proven red by skipping the page block: the run reported 22 of 44 expected and failed; restored, 44 of 44, PASS. | Claude Code, on a Product Owner finding |
| 1.120 | Revision | **E-19 built: the launcher, the remembered server, and one file for the server's settings.** `nemr ui` is the extension form: `ui` joins the install script's name list, so `nemr-ui` is laid on PATH beside the other names and the open CLI execs it — the open tree is untouched, and a test now holds the install list equal to the binary's visible subcommands (red for `ui` before this row) and proves argv[0] `nemr-ui` is the `ui` subcommand. `$BROWSER` is tried before `xdg-open`. The client remembers the server of its last successful login or registration in `$XDG_STATE_HOME/nemr/cloud/server-url` (0600), which `logout` leaves alone; the default is `NEMR_SERVER_URL`, else the remembered server, else the built-in — a per-invocation environment beats a persistent file, the flag and the page's field beat both. Proven: the file exists 0600 after registration, survives logout, and a login with `NEMR_SERVER_URL` removed succeeds through it, while a bare machine with neither cannot log in at all (the control); logged out, `whoami` offers the remembered server and the page's form is pre-filled with it, and the acceptance logs in with an **empty** server field. The sync server reads one file, `$XDG_CONFIG_HOME/nemr/sync.env` (`NEMR_SYNC_ENV_FILE` overrides; empty means none), `KEY=VALUE` lines, the process environment winning key by key; the file is refused — not repaired — unless 0600 or tighter; key names are logged at start and values never; a malformed line is named by number, never by content (each proven in the module's tests). **The pepper, as ruled:** `NEMR_AUTH_PEPPER` is required, and a server without one refuses to bind, naming the file and the exact line to add, with a one-liner that generates it; `NEMR_AUTH_PEPPER=ephemeral` is the one escape hatch — a random per-process pepper and a loud `THROWAWAY SERVER` warning — and both acceptances now use it, asserting the warning. Measured: a server started with no pepper exits without binding. | Claude Code, on the E-19 ruling |
| 1.121 | Revision | **E-20 built, and D-05 closed: the storage backend from the environment, R2 proven through the server against a real bucket.** The backend's own variables are the switch: a complete `NEMR_S3_*` set names an object store (the provider validated as exactly `r2`, `b2` or `s3` — a typo is refused instead of becoming a generic endpoint silently), `NEMR_BUNDLE_DIR` names a directory that must already exist; exactly one, no default — both or neither is refused naming both, empty means unset, a half-configured object store is its own refusal naming what is missing and never a fall-back to a directory. `S3Store` is a `DynStore` beside `LocalStore` (the line the façade reserved), the façade gains `list`, and the start-up order is the ruling's: settings, the store opened and **probed with one egress-free list under a prefix no bundle key can match**, then the database migrated, then the port bound — a server that is listening is one whose store answered; a store that cannot list is refused naming the store, with a directory store as the passing control. The server logs `storage backend store=<provider:bucket>` and, per stored bundle, `key=… bytes=…` — never the endpoint (it embeds the account id), never a credential. `NEMR_BUNDLE_PREFIX` names the key prefix. **The proof (the ruling's words):** `docs/ui-acceptance.sh` runs in R2 mode when `NEMR_S3_BUCKET` is in the caller's environment, passing the `NEMR_S3_*` set through to the server and nothing else, under a per-run prefix. A push from the browser lands as ciphertext in the bucket — the object fetched back **by the acceptance's own AWS Signature V4 reader, standard library only, not by the server**, at the key the server logged, with the size the server logged, carrying no plaintext member name; a pull on a project deleted locally comes back **byte-identical** (the session-state digest) and continues its conversation; the second push rewrites the object at the same key with a different ciphertext; the run's objects are removed and the prefix read back empty. **Red under the neuter that swaps the backend out:** with `NEMR_S3_*` configured and the selection made to use a directory instead, the server announces a directory store and the bucket holds nothing at the logged key. Both modes assert their own counts: local 49, R2 52. The bucket named in the environment did not exist and was created by the acceptance's first probe with the same credential — stated here because it is an object on the Product Owner's account. `bucket_roundtrip` inherits the stricter provider rule. Not done here, recorded: a `nemr-sync` unit, `_FILE` secret indirection, bundle deletion on the server, TLS beyond loopback. | Claude Code, on the E-20 ruling |
| 1.122 | Revision | **F-7: the sync client's installer has the engine's hash gate, and the acceptance says which `nemr-ui` it ran.** `scripts/install_engine.sh` proves the installed binary is the one this tree built (F-62); `scripts/install_sync_client.sh` did not, and an eleven-day-old `nemr-cloud` with no `nemr-ui` link sat on the Product Owner's host while the acceptance passed from `target/release`. The installer now prints the installed and built hashes and refuses if they differ, and refuses if any of its names on PATH resolves to something other than this install — a stale or shadowed `nemr-ui` is named at install time. The acceptance gates the same way `scripts/sync_acceptance.sh` gates the engine: if a sync client is installed, it must be this build, or the run fails naming the install and its date; and it prints the `nemr-ui` it executes — the name, where it resolves, its hash beside the build's — and refuses one that is not this build. Proven red both ways: a stale `nemr-ui` shadowing the install on PATH makes the installer refuse naming both paths, and a different binary in the install slot makes the acceptance refuse at its prerequisites naming the install and its date. **Found on the way, twice.** The gate's first green was false in a way the gate itself then caught: `cargo build --release -p nemr-cloud` alone and `-p nemr-sync -p nemr-cloud` together produce **different** `nemr-cloud` binaries — cargo unifies features across the packages of one invocation — and cargo swaps between the two cached artifacts without recompiling, so the installer's build and the acceptance's build disagreed by hash while both were the same source. F-62's lesson generalised: the build command is part of the binary's identity, and a hash gate needs one canonical command on both sides; the installer now builds both packages together, as the acceptances do, and says why. And a sync server left listening by an interrupted run answered `/health` while the run's own server died at bind, and every step after passed against the wrong server — the acceptance now refuses to start when its port is already taken (a read, not a repair). | Claude Code, on a Product Owner finding |
| 1.123 | Decision | **F-8, ruled by the Product Owner: nobody creates buckets.** During the E-20 proof the bucket named in the environment did not exist, and a probe run by hand with the same credential created it. **Ruling:** the server never creates buckets; a missing bucket is a configuration error and the server refuses to bind, like a missing pepper — which is what the pre-bind probe already does, since a list against a bucket that does not exist (or that the credential is not scoped to) is an error and the probe refuses naming the store; the production credential is scoped to one existing bucket. The acceptance may create and remove a throwaway bucket under its own name if it wants to; it does not want to: it checks that the configured bucket answers before anything runs and refuses plainly if not, and its bucket reader has no bucket-creating call. The hand probe that created `nemr-dev` is recorded here as the incident that produced the rule. | Product Owner ruling, recorded by Claude Code |
| 1.124 | Decision | **E-21 — the credential step on a second machine (D-02 in the page).** Recorded in `docs/DECISIONS.md` E-21 for the Product Owner's ruling; no code in this row. The facts: on a fresh host a restore is created without a credential (E-14) and `start` then binds a file that does not exist, so the page's pull-and-start fails at a mount, naming no login; measured inside a session with no credential and no browser (Claude Code 2.1.240), the login screen prints its own OAuth URL and waits for a pasted code — the manual flow exists. **Recommended:** at `start`, a missing host credential becomes a 0600 placeholder in the engine's recognised shape, bound read-write like a real one, so the session starts; the row shows *no Claude login on this machine yet — attach and run /login*; the page's terminal turns Claude Code's OAuth URL into a link; the user authorises in the host browser and pastes the code; Claude Code writes the credential through the bind onto the host (D-02 (f)), the watcher attributes it, `status` reports it valid, the line clears. Per-device holds: minted here, never left, nothing in a bundle. Rejected: Claude Code on the host first, the page as a second OAuth client, copying the credential. The acceptance has two arms — automated (placeholder, line, login screen and link, `claude -p` saying not logged in) and human-completed once on the fresh VM (the code pasted, `status` valid, `claude -p` answering). Open for the ruling: AUTH-03 unchanged for `create`; a `NEMR_HOST_CREDENTIALS` test seam for the automated arm on hosts that have a credential; link plus text. E-22 (D-04's attach report) follows this ruling. | Claude Code, for Product Owner ruling |
| 1.125 | Revision | **E-21 built: the credential step on a second machine (AUTH-03 amended by the Product Owner's ruling).** `create`, `import` and `start` behave identically on a host with no credential: the engine writes its placeholder (0600, a shape it recognises as *no login yet* — never expired, never blank) at the host credential path and binds it read-write as it binds a real one; a present credential is still held to F-129. `status` carries `credential_present` (additive wire field; the protocol version is unchanged), and `nemr status`/`nemr attach` say *no login yet on this machine — run /login inside the session*. The test seam `NEMR_HOST_CREDENTIALS` is path-only, read by the daemon's environment alone (the page cannot reach it, the server never talks to the daemon), and the daemon warns loudly when it is set. **The page:** a machine with no login shows one line above the table; the terminal's URLs are clickable (xterm's web-links addon, pinned 0.11.0, served from the binary), and when Claude Code prints its sign-in URL the page shows it once as text — as printed — and as a link. Measured in a session at 80 columns: Claude Code 2.1.240 prints the URL inside an OSC 8 terminal hyperlink whose target is the whole URL, and as visible text the terminal wraps over six lines; the page takes the hyperlink's target first and the joined visible text second, over a rolling buffer so a URL split across two frames is still seen. The acceptance reads the terminal's screen as evidence when the URL does not appear. **Measured before building:** Claude Code 2.1.240 writes its credential store by rename in a plain directory (a new inode), so whether a write lands on the host through the single-file bind was proven, not assumed: the host test creates and starts a session against the placeholder under the seam, then provokes a store write with a fake expired credential (the dead-refresh clear, the same path a login takes, no browser) and asserts the host file holds what the session wrote. **Result, on the reference host: the write landed.** Through the read-write single-file bind the host file kept its inode (524977 before and after) and took Claude Code's content exactly (172 bytes → the 136-byte dead-refresh clear), and the session's view of the file was the same inode — so Claude Code's rename, which replaces the inode in a plain directory, does not escape the bind: what it writes reaches the host. The single-file bind stays; no directory bind is needed, and D-02 (f)'s enumeration (history, settings, caches never shared) stands. **The acceptance, two arms:** the automated arm stops the daemon, restarts it under the seam (the move `install_engine.sh` makes), creates and starts a session on a host with no login, proves the placeholder, the `status` and `attach` wording, that `claude -p` inside says it is not logged in (the control), and in the real page the login line, the sign-in screen reached through `/login`, and the URL as text and link; then removes the session, stops the seamed daemon and proves a clean one serves the real credential again. The human arm (`NEMR_HUMAN_LOGIN=1`, on a host with no login) waits for the code to be pasted, then proves the host file equals what the session sees (sha256, the required proof) and that `claude -p` answers. | Claude Code, on the E-21 ruling |
| 1.126 | Revision | **F-9: the daemon must live in the host's user namespace; a namespaced daemon is refused, never autostarted.** Found 2026-09-08 when the browser acceptance's first `nemr create` failed in the privileged helper with `sudo: /etc/sudo.conf is owned by uid 65534`. The cause, measured: the E-11 offline test runs the CLI under `unshare -rmn`; the engine installer had just stopped the daemon, so the namespaced CLI found no daemon and autostarted one *inside the namespace*; the mount namespace being a copy, that daemon bound the host's well-known socket, outlived the test as an orphan, and served every later caller on the machine with a helper that cannot elevate — while `ss` could not even see its listener (another network namespace). Three reads, no repairs: (1) `nemrd` refuses to start unless `/proc/self/uid_map` is the host's identity map, before the socket path is looked at, naming the map it saw (`nemr_daemon_api::userns`); (2) the CLI refuses to autostart from inside a namespace with the same check and message; (3) the browser acceptance dies before starting anything if any `nemrd` of this user is in another user namespace. The installer now stops every `nemrd` of this user, not only the one at its install path (the orphan from a build tree had kept the socket through a reinstall). The E-11 offline test makes one host-side `nemr list` before its namespaced run, so a daemon is answering before the CLI is unshared. Proof: `f9_nemrd_refuses_to_start_inside_a_user_namespace` runs the daemon under `unshare -rmn` on a scratch socket and requires the refusal and an untouched socket; with the guard neutered the daemon binds and serves until the test's timeout — red. | Claude Code |

---

## 1. Purpose and Scope

### 1.1 Purpose

This document specifies the requirements, architecture, and acceptance
criteria for Phase 1 of Nemr: a backend engine capable of provisioning
isolated, resource-bounded, pre-configured Claude Code execution
environments on a single Linux host, without dependency on Docker or
Docker Desktop.

This document is the authoritative source of truth for Phase 1 scope. Where
ambiguity exists between this document and any verbal or informal
instruction, this document governs. Deviations must be recorded in the
project README with rationale (see Section 11).

### 1.2 Product Context (informative — not in scope for Phase 1)

Nemr is a desktop application addressing the portability and isolation
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
| **Engine** | The Rust binary/library produced by this phase; the sole component with authority to create, mutate, or destroy containerd resources on behalf of Nemr |
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
- Cgroup v2 controller delegation for the user session (memory, pids are
  delegated by default; cpu, cpuset, io must be explicitly delegated via
  a /etc/systemd/system/user@.service.d/delegate.conf drop-in — see
  PREREQUISITES.md Step 2a for the exact procedure and why a reboot, not
  just a service restart, is required)
- Rust stable toolchain via `rustup`

Exact installation steps (package names, systemd unit management, and a
verification procedure such as `ctr version` succeeding against the local
containerd socket) shall be documented in `PREREQUISITES.md`, written such
that a person with no prior context can provision a clean Ubuntu host from
this document alone.

### 3.4 Repository Structure (Normative)

```
nemr-engine/
├── SPEC.md                    # This document — tracked in-repo per 4A.5
├── src/
│   ├── lib.rs                 # Crate root; declares the modules below
│   ├── bin/
│   │   └── nemr.rs            # CLI entrypoint; sole interface for Phase 1
│   ├── engine/                # Product logic; depends only on nemr-containerd
│   │   ├── mod.rs
│   │   ├── image.rs           # Base image build/import orchestration
│   │   ├── project.rs         # Project lifecycle: create/start/stop/list/delete
│   │   ├── tty.rs             # Terminal handling for attach sessions (3.8)
│   │   └── volume.rs          # Quota-bounded storage volume management
│   ├── config.rs              # Paths, defaults, constants
│   └── auth.rs                # Claude Code credential injection (3.5)
├── crates/
│   └── nemr-containerd/       # Wrapper layer (3.2), its own crate
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── config.rs      # containerd-level defaults (runtime, snapshotter)
│           ├── client.rs      # Connection management, shared client handle
│           ├── images.rs      # Image pull/import operations
│           ├── containers.rs  # Container + task + exec operations
│           └── bin/           # M1 connectivity baselines (AC-1.2)
├── deploy/                    # Privileged half of volume provisioning (3.7)
│   ├── nemr-volume/           # Root-owned helper crate (PRIV-03), standalone
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   └── sudoers.d/
│       └── nemr-volume        # NOPASSWD grant to exactly one fixed path
├── image/
│   └── Dockerfile             # OCI image definition (build tool per 3.1)
├── scripts/
│   └── e2e_smoke_test.sh      # Milestone 7 regression test
├── PREREQUISITES.md
├── README.md
└── Cargo.toml
```

This tree is the current state of the repository, not an aspiration. Files
added beyond the original Phase 1 sketch are recorded in Section 11 with
their rationale; keeping this diagram in step with reality is part of that
record.

### 3.5 Authentication Handling (Normative)

| Requirement ID | Requirement |
|---|---|
| AUTH-01 | Credentials shall not be baked into the base image. |
| AUTH-02 | At container creation time, the engine shall bind-mount the host user's Claude Code credentials file (`~/.claude/.credentials.json`) into the container, **read-write**, at the path Claude Code expects (`/root/.claude/.credentials.json`) — read-write so that Claude Code can refresh the login inside the session, which it does by rewriting that file (D-02 (f), SPEC 1.102; before 1.102 the mount was read-only and a session's login died at the access token's eight-hour expiry). No other host-side `~/.claude` content shall be mounted into the container — session state, history, and cache remain container-local, which is what preserves per-project isolation — and the write surface a session gains is exactly that one file. The engine shall repair the record of a project created under the read-only rule at its next start, and shall report to the user, at `status` and before `attach`, a credential the session cannot recover by itself: a spent refresh token or a file Claude Code has blanked. A host-side replacement of the file (the host's Claude Code writes by rename) shall reach every running session without a restart: the engine re-binds the current file into a running session when the replacement is observed and again before every `attach` (F-12). Because the mount cannot distinguish a refresh from an overwrite, every rewrite of the host credential shall be **observed** — recorded in the daemon log and on `status` with when it happened, which sessions could have made it, and whether the result parses as a credential. |
| AUTH-03 | **As amended by E-21 (Product Owner ruling, 2026-09-08):** a host with no credential file is a machine that has never logged in, not a fault. `nemr create`, `nemr import` and `nemr start` shall behave identically on it: the engine writes a placeholder, in a shape it recognises as "no login yet", at the host credential path (mode 0600) and binds it read-write exactly as it binds a real credential, so the session starts and Claude Code's own `/login` inside it writes the real credential through the bind onto this host, where it stays (D-02). A credential file that IS present is still held to what it says (readable; not dead — F-129). `nemr status` and `nemr attach` shall name the state ("no login yet on this machine — run /login inside the session") rather than a fault. *Before the amendment:* If no host credentials are found, `nemr create` shall fail with a clear, actionable error message directing the user to authenticate on the host, and `nemr start` and `nemr attach` shall likewise require one. **`nemr import` shall not**: restoring a bundle on a machine where the user has not yet authenticated is the normal case, not the edge case — per D-02 the credential is per-device and does not travel, so a new user necessarily has a bundle before they have a credential, and making that fatal would break the product's core flow on exactly the machine it is designed for. The distinction is between *materialising* a project and *using* one: creation and use require a credential, restoration does not. The deferral is confined to the single restore call site and asserted to stay there (E-14, resolved). Interactive in-container authentication flows remain out of scope for Phase 1. |
| AUTH-agent | **AUTH-01–03 and D-02 are agent-agnostic** (Product Owner ruling, E-15). Every agent Nemr runs authenticates the same way: a per-device credential the user obtains on the host, injected read-only at the path that agent expects, never written into a bundle. AUTH-02's concrete paths (`~/.claude/.credentials.json` → `/root/.claude/.credentials.json`) are the **verified instance** for Claude Code; the equivalent paths for any other agent are the same mechanism at that agent's own locations. For Codex those locations are **not yet enumerated**, so D-02 ("the credential never travels") is *asserted* for Codex, not *enforced* — the enumeration that would make it enforceable is part of F-84. There is deliberately no second credential model: one mechanism for all agents is one thing to get right, not two. |

### 3.6 Storage Quota Mechanism (Normative)

| Requirement ID | Requirement |
|---|---|
| VOL-01 | Each project's storage shall be a fixed-size allocation selected at creation time from a small set of presets (default set: 500MB / 2GB / 10GB), configurable via CLI flag. |
| VOL-02 | The allocation mechanism shall be a sparse file, formatted as ext4, mounted via a Linux loop device, and bind-mounted into the container as its working directory. This approach is selected over XFS project quotas specifically to avoid host filesystem prerequisites beyond a standard Ubuntu install. |
| VOL-03 | All mount, loop-device, and format operations shall be logged with sufficient detail to be independently auditable without reading source code. |
| VOL-04 | Volume creation, mounting, unmounting, and deletion logic shall use RAII patterns (Rust `Drop` implementations) to guarantee resource cleanup on error paths, not solely on the success path. |
| VOL-05 | When a volume reaches capacity, dependent container operations shall fail with a clear, human-readable error. Silent data loss is a critical defect. Auto-expansion is explicitly out of scope (Section 1.4, item 5). |
| VOL-06 | `start` shall verify via `/proc/self/mountinfo` that a project's volume is mounted before proceeding; if unmounted, `start` shall remount it using the same privileged helper as `create`, failing clearly only if the remount itself fails. Falling through to an unrelated filesystem is the specific critical defect this closes. |

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
| PRIV-02 | Loop-device attachment (`losetup`) and mounting/unmounting (`mount`/`umount`) of a project volume are genuinely privileged operations on Linux and have no rootless equivalent within this phase's chosen quota mechanism (Section 3.6). This is the sole permitted exception to PRIV-01. **Sparse-file allocation and `mkfs.ext4` are *not* privileged** — both succeed as the unprivileged user against a user-owned file, measured at Milestone 3. This corrects the original assumption in version 1.4, which listed `mkfs.ext4` among the privileged operations; it is a correction to a mistaken premise, not a relaxation of the requirement. Excluding formatting from the privileged surface is a strict security improvement, since `mkfs.ext4` is the most destructive verb in the original list. |
| PRIV-03 | The exception in PRIV-02 shall be implemented as a **single root-owned helper binary at a fixed, non-user-writable path**, with a NOPASSWD `sudo` grant scoped to exactly that path and **no argument wildcards**. All path and device construction, and all input validation, shall happen *inside* the helper: the caller supplies a volume name and a size, never a raw filesystem path, loop device, or mount target. The helper shall reject any name not matching a strict pattern before performing any privileged action. The sudoers rule and the helper's source shall both be committed to the repository (under `deploy/`) so they are reviewable, not configured ad hoc on the host. **Rationale:** a rule expressed as argument wildcards cannot express a path constraint at all. Per `sudoers(5)`, a slash *is* matched by wildcards in command-line arguments (unlike in the command's own path), so a pattern such as `.../volumes/*` also matches `.../volumes/../../../etc`. A rule of that shape would appear narrow while granting `mount` over arbitrary host paths — that is, root. Validation must therefore live in code the granted user cannot modify, which is what the helper provides. |
| PRIV-04 | Every operation performed under the PRIV-03 exception shall be logged per NFR-04, specifically identifying that it ran under elevated privilege and why. |
| PRIV-05 | This privilege split (rootless for container operations, narrowly scoped elevation for volume provisioning only) applies for the duration of Phase 1 in full — it is not re-litigated per milestone. Any milestone that appears to need elevated privilege outside the PRIV-03 scope shall be escalated per Section 9, not resolved by widening the sudoers rule or the helper's capabilities unilaterally. |
| PRIV-06 | The helper shall perform the mount and the subsequent ownership change as **one atomic privileged operation**, not as separate grants. A volume mounted by root is owned by host root, which maps to `nobody` inside the rootless container's user namespace (PRIV-01), leaving the container user unable to write to its own project volume. On mounting, the helper shall therefore `chown` the mounted volume to **the host UID that the rootless container's UID 0 maps to** — which, under this project's mapping, is the invoking user's own host UID — **not** an offset within the `/etc/subuid` range. The `/etc/subuid` range maps to container UID 1 and above; using it would leave the volume owned by a non-root container user, which is wrong for a base image that runs as root. This ownership change is an internal step of the helper's mount operation and shall not be exposed as a separately invocable privileged verb: exposing `chown` as its own grant would permit re-owning arbitrary paths, reintroducing the escalation PRIV-03 exists to prevent. The helper shall derive the target UID/GID itself, from `SUDO_UID`/`SUDO_GID`, and shall not accept them as caller-supplied arguments. |

**Rationale:** a broad root-equivalent group (the alternative considered
under E-03) would have been simpler to implement but leaves the engine's
security story as "the user has root-equivalent access to run containers
at all," which is both a larger attack surface than necessary and a weaker
position to defend when this project's output is reviewed externally.
Rootless containerd removes that requirement for the majority of the
engine's operations; the volume-provisioning exception is real but is
bounded, explicit, and auditable rather than implicit.

### 3.8 Container Process Model (Normative)

This section fixes what runs as PID 1 inside a project's container and how
an interactive session is obtained. It is recorded here rather than left to
Milestone 5 because it determines the meaning of "running" for a project,
and every lifecycle operation in Milestones 5 and 6 depends on it.

| Requirement ID | Requirement |
|---|---|
| PROC-01 | A project container's PID 1 shall be a **long-lived supervisor process** (`sleep infinity`), not an interactive shell. The container's liveness is therefore a property of the project, independent of whatever any user session is doing. |
| PROC-02 | `attach` shall obtain an interactive session by performing a **task exec with a fresh TTY per call**, not by connecting to PID 1's terminal. Each attach is an independent process; exiting one leaves the container running. |
| PROC-03 | The supervisor command shall be written **explicitly into the container's runtime spec at creation time**. It shall not be inherited from the base image's default command, so that a change to `image/Dockerfile` cannot silently alter the process model. |
| PROC-04 | `stop` shall terminate the task, including any live exec sessions, and return the project to the stopped-but-ready state of AC-4.1. A project's stopped/started state shall not depend on shell state. |

**Rationale.** The alternative — PID 1 being the image's shell, attached to
a TTY, with `attach` joining that same terminal — is a more literal reading
of "attach an interactive session," but it couples two things that should
be independent. The container would stay alive only as long as the shell
did, so exiting the shell would stop the project; and concurrent attaches
would share one PTY, with both sessions receiving each other's input and
output. Making PID 1 a supervisor decouples project liveness from session
lifetime and gives each attach its own terminal.

**Retroactive effect on Milestone 4.** This supersedes an implicit
assumption in Milestone 4's original `create()` work, which relied on the
base image's default command (`docker-entrypoint.sh /bin/bash`) as the
container's process. A container created under that assumption exits
immediately when started detached, because the shell reaches EOF on stdin.
Milestone 4's create path shall write the supervisor command explicitly per
PROC-03. Containers created before this section was added do not conform
and shall be recreated rather than migrated.

### 3.9 Container Network Model (Normative)

| Requirement ID | Requirement |
|---|---|
| NET-01 | **RETIRED, superseded by NET-02 (2026-08-28). Recorded rather than deleted.** It read: *"Project containers shall share rootlesskit's network namespace (equivalent to `nerdctl run --net=host` under rootless); no per-project network namespace isolation exists in Phase 1."* That was **true and verified** — established twice by comparing the `net:` inode of a container's PID 1 against rootlesskit's child, including after a restart changed the inodes. It is retired because the **design changed**, not because it was mistaken: one shared namespace meant two sessions could not both bind port 8000, and the second session's server failed with `EADDRINUSE` — an error raised by the user's own program, never naming nemr and pointing at nothing. That is R-07, which this requirement made unavoidable. No regression test ever asserted the shared namespace — it was established by manual inode comparison, which is precisely why it could be quietly falsified. NET-02 adds the test that was missing, asserting the inverse: a started session's namespace differs from rootlesskit's, with a comment recording what NET-01 claimed and why. |
| NET-03 | A project may declare host port forwards (`nemr port add/rm/ls`). Declarations live in the project's container label and are **authoritative**; the live forward set in rootlesskit is derived from them — applied on `start`, withdrawn on `stop` and `delete`, and reconciled by `nemr reconcile`. Forwards bind `127.0.0.1` unless `--expose` is given, which warns at the point of use. A host port already held is refused, naming the holding project where it is one of ours and saying plainly when it is not. |
| NET-02 | **Each session has its own network namespace.** The OCI spec requests one, so runc creates it; the engine wires a veth pair into it (`10.99.<index>.2/24` inside; gateway `10.99.<index>.1` in rootlesskit's namespace) and adds a MASQUERADE rule so the session reaches the internet. The allocation index is recorded on the container label, so the same addresses are reapplied at every start. Allocation **refuses** when the host already routes anything overlapping `10.99.0.0/16`, because allocating into a conflict does not error — it misroutes silently. Host forwards reach a session in **one hop**: rootlesskit's port API takes a child IP, so NET-03 keeps its shape. Entirely rootless — no root, no privileged helper, no system daemon. Supersedes NET-01; the earlier "out of scope for Phase 1, needs rootlesskit >= 2.0" position was corrected by measurement (SPEC 1.87), not by argument: the veth path works on the archive's 0.14.6. |

| NET-04 | **A session's network is a fact about the host it runs on, never about the session.** The allocation index is chosen from what is free *on this machine* and recorded on the local container record only. It is not carried in a portable bundle, and a restore allocates a fresh one here rather than reinstating whatever the source machine used — a bundle from another machine would otherwise name a `/24` that is already taken on this one, and a duplicate allocation does not error: two projects derive the same addresses and the same link name, one session ends up with no network, and the other's host forwards serve it. A project that carries no allocation — every project created before NET-02 — is allocated one on its first start: "no allocation recorded" means *not yet allocated*, never *no networking wanted*. **Its container record is migrated too.** A container's OCI spec is frozen at create time and read again at every task start, so a record created before NET-02 asks for no network namespace for ever and its task joins rootlesskit's; the engine adds the namespace to the stored spec before the task is created, additively. A task that is nevertheless found sharing rootlesskit's namespace is **refused**, not wired — wiring a session into the shared namespace puts its addresses on rootlesskit's own interfaces and collides with rootlesskit's default route (F-112). |
| NET-06 | **The session wiring reconciles to the declared state.** Applying it to a session that is already wired is a no-op, not a collision: addresses and routes are `replace`d rather than `add`ed, and the link pair is rebuilt rather than detected by name. A start path that works from clean and fails on a second run is a start path that eventually refuses to start (F-113), and the same rule already governs volumes and port forwards — the declaration is authoritative, the live state is brought to match it. |
| NET-05 | **Sessions are isolated from one another.** A single `FORWARD` rule over `10.99.0.0/16` drops session-to-session traffic; egress and host port forwards are untouched. Range-wide policy rather than per-session state, so it is not removed when a session stops — and it is re-asserted at every start, because rootlesskit's network namespace is destroyed when rootless containerd restarts and takes every rule with it. Sessions are routed to one another inside that namespace, so a per-session bridge would not isolate them; one rule does. **A port published with `--expose` binds `0.0.0.0` and is thereby published to the network, which includes other sessions on the same host** — that is what publishing means, and NET-03 already warns at the point of use. Default forwards bind `127.0.0.1` and are not reachable from a session. |

**Background.** A container given its own network namespace receives an
empty one: only loopback, no route, no egress. Nothing wires it up, because
rootless CNI is not configured. This was observed at Milestone 5 as Claude
Code failing with `ENOTIMP` against `api.anthropic.com` while otherwise
running correctly — `/proc/net/dev` inside the container listed `lo` alone.
Inheriting rootlesskit's namespace instead gives the container the `tap0`
device slirp4netns already provides, and DNS and TLS then work.

---

### 3.10 Client-Side Encryption (Normative, E-16)

The commercial sync server stores ciphertext it cannot read. The scheme is
E-16 (`docs/DECISIONS.md`), implemented in `crates/nemr-crypto`. This section
fixes the parameters so they are stated and justified in the spec, per the
standing requirement that cryptography meets the same bar as the privileged
helper.

| Requirement ID | Requirement |
|---|---|
| CRY-01 | The data key is a random 256-bit **master key (MK)**, generated once per account at registration from the OS CSPRNG. The server never receives MK or any key that derives it. |
| CRY-02 | MK is stored only **wrapped** in an AEAD envelope. The password envelope's key is `HKDF-SHA256(Argon2id(password, salt), info="nemr/kdf/wrap/v1")`. A separate `auth_key` uses `info="nemr/kdf/auth/v1"`; the two labels are the domain separation that lets `auth_key` travel to the server while `wrap_key` does not. |
| CRY-03 | **Argon2id parameters:** m = 19 MiB (19456 KiB), t = 2 iterations, p = 1 lane, 32-byte output, Argon2 v1.3. These are OWASP's 2024 recommendation; the memory floor is the barrier to GPU/ASIC brute force, since the salt is public (the server must return it at login). Parameters are stored per-user so cost can be raised for new accounts without invalidating existing ones. |
| CRY-04 | Wrapping and bundle encryption use **XChaCha20-Poly1305**. Its 192-bit nonce is drawn at random per operation; there is no nonce counter to persist or synchronise across machines. Associated data binds each ciphertext to its purpose (`nemr/envelope/v1` vs `nemr/bundle/v1`) so the two are not interchangeable under one key. |
| CRY-05 | A **recovery envelope** wraps the same MK under a key derived from a random recovery code (`info="nemr/kdf/recovery-wrap/v1"`). It is generated **at registration**, the code is shown once, and the account is not usable until the client confirms it by recovering MK through that envelope and matching `SHA-256("nemr/recovery-ack/v1" || MK)`. Recovery is not deferrable. |
| CRY-06 | No decryption path distinguishes "wrong key" from "tampered ciphertext": both surface as one opaque error, so a probe learns nothing. Every property in CRY-01–05 is asserted by a test that fails when the property is violated. |

**Consequence, accepted (E-16).** A forgotten password with no recovery
envelope means the bundles are unrecoverable, permanently — there is no
server-side reset that preserves data, because the server cannot read the data.
That is the price of the server holding nothing it can open.

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
PRIV-02/PRIV-03 exception in Section 3.7.** Both halves of that exception
shall be authored as part of this milestone's deliverables, not assumed to
pre-exist on the host: the root-owned helper binary (`deploy/nemr-volume/`),
which performs all path and device construction and all input validation
internally, and the sudoers rule (`deploy/sudoers.d/`) granting NOPASSWD
access to exactly that one fixed path with no argument wildcards.

**Acceptance Criteria:**
- AC-3.1: Volume creation at a specified size is confirmed correctly capped
  via `df`/`du` measurement.
- AC-3.2: Volume deletion leaves no residual file, loop device, or mount
  entry on the host — verified by host inspection post-deletion.
- AC-3.3: An automated test exercises create → mount → write past capacity
  → confirm enforced failure → delete, and passes.
- AC-3.4: A fault-injection test (e.g., simulated failure mid-mount)
  confirms RAII cleanup leaves no orphaned resources (validates VOL-04).
- AC-3.5: The sudoers rule at `deploy/sudoers.d/` is shown granting
  NOPASSWD access to exactly one fixed, non-user-writable helper binary
  path (`deploy/nemr-volume/`), with no argument wildcards — all
  path/device construction and validation happening inside the helper per
  PRIV-03 — and every elevated operation is shown logged per PRIV-04.

### Milestone 4 — Project Lifecycle: Create

**Scope:** `src/engine/project.rs::create()`. Given a project name and size,
creates a volume (Milestone 3) and a container from the base image
(Milestone 2), mounts the volume, injects credentials (AUTH-01–03), and
registers a discoverable named reference. Extends the wrapper module with a
general-purpose `create_container()` operation — implemented as reusable
wrapper infrastructure, not logic specific to this call site.

**Retroactive correction from Section 3.8 (container process model):**
this milestone's original `create()` relied on the base image's default
command as the container's process. Section 3.8 (PROC-03) supersedes that:
the create path shall write the supervisor command (`sleep infinity`)
explicitly into the runtime spec. A container created under the original
assumption exits immediately when started detached, so it is not in fact
"stopped-but-ready" in the sense AC-4.1 intends — it is startable but not
survivable. Projects created before this correction shall be recreated,
not migrated.

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

**Resolved at Milestone 4: the engine's `create_container()` does *not*
need this.** `nemr create` was run from the host mount namespace, with no
`nsenter` and no `sudo`, and produced a container and volume pair
discoverable via `ctr`. The constraint below is specific to `ctr`, not to
containerd's API.

The difference is where the work happens. `ctr container create` mounts the
image snapshot **client-side** to read the image config. Going through the
API, the engine reads the config out of the content store — containerd
streams the blob back over gRPC — and asks containerd to prepare the
snapshot, which containerd also does server-side. Nothing is mounted in the
engine's own mount namespace, so there is nothing for the host namespace to
refuse.

Milestone 5 is a separate question and is not settled by this: starting a
*task* runs runc, which does mount, and carries its own cgroup constraint
(see the Milestone 5 note below).

The `ctr`-specific constraint, retained because the acceptance criteria for
Milestones 2 and 6 invoke `ctr` directly:

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
- AC-4.1: `nemr create <name> --size 2GB` produces a container and volume
  pair discoverable via containerd's own listing APIs, in a stopped-but-
  ready state.
- AC-4.2: Repeating the command with a duplicate name fails with a clear
  error rather than silently overwriting or creating a conflicting state.

### Milestone 5 — Project Lifecycle: Start / Attach / Stop

**Scope:** `nemr start <name>`, `nemr attach <name>`, `nemr stop <name>`.

**Note carried forward from Milestone 2 (rootless cgroup driver):** starting a
task is where runc applies cgroup configuration, and under PRIV-01 the default
path does not work. containerd running inside rootlesskit still sees the host
`/sys/fs/cgroup`, so runc's default cgroup path is unwritable and start fails
with `mkdir /sys/fs/cgroup/default: permission denied`. The task must be placed
in a scope under the delegated user slice via the systemd cgroup driver. With
`ctr` this was validated in M2 as:

```bash
ctr run --runc-systemd-cgroup --cgroup "user.slice:nemr:<name>" ...
```

`--runc-systemd-cgroup` requires `--cgroup` to be set explicitly. The driver
talks to the user's systemd over the session bus, so `DBUS_SESSION_BUS_ADDRESS`
and `XDG_RUNTIME_DIR` must be present in the environment — `nsenter` does not
carry them in. This also depends on the cgroup v2 controller delegation now
listed in Section 3.3; without it the scope is created but start fails on
`cpu.weight: no such file or directory`.

The engine equivalent is setting the task's cgroup path through the wrapper
rather than shelling out to `ctr`. If the right wrapper API shape for this is
not obvious, that is an E-04 escalation.

**Note carried forward from Milestone 7 (attach allocates a pty only when
stdin is a terminal).** PROC-02 (Section 3.8) specifies "a task exec with a
fresh TTY per call". Implementing that literally makes a *scripted* attach —
`echo cmd | nemr attach <name>` — impossible to terminate, which is what
Milestone 7's smoke test needs. A pty has no end-of-file: the shell never
learns its input has finished, so it never exits. Writing EOT (0x04) into the
pty and calling containerd's `CloseIO` were both tried, and both hung
indefinitely.

`attach` therefore allocates a pty only when its own stdin is a terminal
(`isatty(0)`), matching what `docker exec -t` does. Without a terminal, stdin
is an ordinary pipe, closing every write end is a real EOF, and the shell
exits on its own with its own status. A separate stderr FIFO is passed only on
the pipe path, since a pty merges the two streams and containerd rejects a
spec that sets both.

This is a **drift from PROC-02 as written**, not a correction to it: the
requirement says every attach gets a TTY, and the implementation now gives one
only to interactive attaches. Section 3.8 is Product Owner territory under
4A.5, so PROC-02 is left unedited and the divergence is recorded here and in
Section 11 pending a decision. Escalated as **E-07**.

**Acceptance Criteria:**
- AC-5.1: `create` → `start` → `attach` results in an interactive shell in
  which Claude Code runs correctly against the mounted project volume.
- AC-5.2: Files written during a session persist across `stop` followed by
  `start`.
- AC-5.3: `stop` on an already-stopped project, and `attach` on a
  not-yet-started project, both fail with clear errors rather than
  undefined behavior.

### Milestone 6 — Project Lifecycle: List / Delete

**Scope:** `nemr list`, `nemr delete <name>`.

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
deploy/sudoers.d/ is shown granting NOPASSWD access to exactly one fixed,
non-user-writable helper binary path (deploy/nemr-volume/), with no
argument wildcards, and all path/device construction and validation is
shown happening inside the helper per PRIV-03; elevated operations are
shown logged per PRIV-04. Stay within src/engine/volume.rs, its tests, and
deploy/ only. Stop after 30 turns if not met.
```

**Milestone 4:**
```
/goal AC-4.1: `nemr create <name> --size 2GB` is shown producing a
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
/goal AC-6.1: `nemr list` output is shown matching actual containerd
state, cross-checked directly against ctr output. AC-6.2: `nemr delete`
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
| NFR-01 | No component of the stack may depend on Docker Engine or Docker Desktop, directly or transitively. This is a hard constraint; discovery of a hidden dependency at any point is a release-blocking defect, not a note. **Scope (E-17, ruled 2026-08-27).** The constraint covers everything we build, ship, script, or **request** — including the test harness and CI configuration. A GitHub Actions `services:` block is a *request*: the runner satisfies it with `docker pull` / `docker create` / `docker start` (verified in our own job logs), so it is in scope and is a violation. The constraint does **not** cover the Docker daemon preinstalled on the CI provider's runner images, which exists whether or not we use it and which we neither invoke nor control. That exclusion is stated rather than assumed because a constraint reaching the provider's substrate would be **unsatisfiable on every GitHub-hosted runner**, and a constraint that cannot be satisfied is not a constraint — it is a permanent violation everyone learns to ignore, which is worse than a written exclusion. Nothing else about NFR-01 is softened. |
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
| R-07 | **RESOLVED by NET-02 (2026-08-28), after first being invalidated by WP-M.** Originally: *"Two projects binding the same port collide: with a shared network namespace (NET-01) the second bind fails."* Originally assessed **Low for Phase 1 — "nothing in scope runs a service"**. That assessment was **correct when written and falsified by a feature we shipped**: WP-M added port forwarding precisely so people run services, which made the risk routine rather than hypothetical. Confirmed by measurement before acting on it: two sessions binding container port 8000 gave the second `errno -98` (`EADDRINUSE`) from its own dev server — an error raised by the user's program that never named nemr. NET-02 removes the shared namespace, so the collision cannot occur. **The general lesson, recorded because it will recur: a risk whose likelihood rests on a scope boundary must be re-checked whenever that boundary moves.** This one was not wrong; it was overtaken. A register in which every revision reads as an error teaches people to defend entries instead of revising them. |

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
- [ ] `src/bin/nemr.rs` (`create`, `start`, `attach`, `stop`, `list`, `delete`)
- [ ] `scripts/e2e_smoke_test.sh`
- [ ] `README.md`, including: architecture decisions and rationale (base
      image choice, build tool choice), measured image size, wrapper-layer
      design notes, and a pointer to Section 11 of `SPEC.md` for recorded
      deviations — the README shall reference that log, not reproduce its
      content, since `SPEC.md` is the single source of truth per 4A.5

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
- E-07: PROC-02 (Section 3.8) requires a fresh TTY per `attach`. The
  implementation allocates one only when `attach`'s own stdin is a terminal,
  because a pty has no EOF and a scripted attach could otherwise never
  terminate — see the Milestone 7 note under Milestone 5. Requires a Section
  3.8 amendment to PROC-02, which is Product Owner territory under 4A.5.
  Recommendation: amend PROC-02 to "a fresh TTY per *interactive* call;
  a non-interactive attach is pipe-backed so that end-of-input is
  representable". Consequence of not deciding: the code and the spec disagree
  on a normative requirement, and the smoke test depends on the code's
  behaviour, not the spec's.
- E-08: The privileged helper's mount path contained a local root escalation —
  the mount point was checked with a symlink-following `is_dir()` and then
  mounted by name, letting a caller mount an attacker-controlled ext4 over
  `/etc` (with backing-file and chown TOCTOU variants). Section 3.7 already
  required symlink refusal and in-helper validation, so the fix is an
  *implementation* correction, not a requirement change, and does not itself
  need a Product Owner ruling — it is raised here only for visibility of a
  security-critical change. Fixed (fd-based resolution, in-process syscalls);
  see Section 11 (2026-08-20). No decision required unless the Product Owner
  wants the threat model in Section 3.7 expanded to name the TOCTOU class
  explicitly.
- E-09: **Engine consumption model — RESOLVED 2026-08-20.** A **long-running
  user daemon, gRPC over a Unix domain socket.** Two already-resolved product
  decisions require something running while the GUI is closed: the session lease
  (`docs/DECISIONS.md` D-03) must keep heartbeating with the lid shut, and
  snapshot-on-quiesce (D-04) must watch the transcript whenever a session is
  live. A linked library would force both into a process the user closes, or
  bolt on a background helper later — a daemon arrived at by accident with an
  undesigned IPC surface. The daemon also makes single-writer *structural*: CLI
  and GUI both exist and must not independently mutate containerd/mount state,
  the divergence class WP A spent nine commits eliminating. Implementation
  constraints (binding on all downstream work): **(1)** Unix domain socket, not
  TCP — filesystem permissions are the authentication, and the E-10 remote
  fallback swaps only the transport; **(2)** a version handshake from day one,
  same pattern as the helper's protocol version, refusing a client/daemon
  mismatch cleanly; **(3)** the daemon is a *client* of the containerd wrapper
  crate, which stays usable standalone — not a replacement for it; **(4)** the
  CLI talks to the daemon and keeps no second, direct path into containerd. Not
  implemented yet: the ruling exists so WP B stops paying for library/daemon
  optionality and WP C's design can assume it.
- E-14: **A restore cannot require a host credential — RESOLVED (2026-08-23, Rain).**
  Ruling: the restore path defers the credential check; `create` keeps AUTH-03.
  AUTH-03 above now states the distinction rather than leaving it an exception in
  code. Rationale, recorded so it carries: restoring before logging in is the
  normal case — a new user has a bundle before a credential, because D-02 makes
  credentials per-device and non-travelling. AUTH-03's intent is that you cannot
  *use* a project without a credential; import does not use one, it materialises
  one. The confinement to a single call site, and the test asserting it stays
  there, are what keep this a narrowing rather than a general weakening.
  Original framing follows.

  AUTH-03 makes a missing credential fatal *at creation time*. E-11 requires
  `nemr import` to work with no network and no credential. These did not collide
  while import needed a pre-existing project — the create had already happened,
  with a credential — but a restore that creates its own project makes them
  collide directly. Restoring a bundle on a fresh machine *before* logging in is
  the normal case, and import's own output already ends "Authenticate on this
  host, then: nemr start". **Interim position, applied:** `nemr create` keeps
  AUTH-03 unchanged; the restore path defers the credential requirement, and the
  deferral is confined to exactly one call site with a test asserting it stays
  there. This narrows *where* AUTH-03 fires, not whether it does — a credential
  is still required to run anything. Raised rather than decided because AUTH-03
  is a Section 3 requirement and Section 3 is Product Owner territory (4A.5).
  **Second consequence, recorded:** a restore provisions a volume, so it needs
  the privileged helper, which cannot run inside the E-11 offline test's user
  namespace. The end-to-end no-credential assertion for import is therefore
  replaced by a policy-level one, which is weaker; the export half is unchanged.

- E-10: **Non-Linux hosts — Windows half RESOLVED 2026-09-02 (Product Owner);
  macOS deferred.** Loopback ext4 plus rootless namespaces is Linux-only.
  Ruling: **Windows support means Nemr running inside WSL2** — no native
  Windows binary, no bundled VM; a Windows user installs WSL2, runs
  `setup_host.sh` inside it, and works from that terminal. macOS (bundled VM)
  stays deferred; the remote-engine fallback stays rejected — it reverses D-01.
  Full rationale, sequencing, and the accepted manual-verification cost in
  `docs/DECISIONS.md` (E-10).
- E-11: **Open-core seam — RESOLVED 2026-08-21 (Product Owner).** **Open source
  in `nemr-engine`:** the engine, `crates/nemr-containerd`, the volume layer, the
  privileged helper, and **the bundle format specification**. **Commercial:** the
  sync layer, the lease service, cloud storage backends, identity, and the GUI.
  The format being open is load-bearing, not incidental: a proprietary format
  would mean the open engine could export nothing useful, making the open core a
  demo — which reads as bait to the developers we are selling to. An open format
  lets a self-hoster move bundles between their own machines with rsync and get
  real value, while sync-across-devices-with-a-login is the paid product. Git is
  open; GitHub is the product. **The test for any later boundary question: can
  someone use the open half productively without ever paying? If no, the line is
  in the wrong place.** Consequences binding on design: (1) the bundle format
  spec is a public interface — versioned, documented in-repo, breaking changes
  treated as breaking; (2) no commercial-only escape hatches in the format — no
  fields only the sync layer can populate or interpret, nothing that makes a
  bundle useless without the paid tier; (3) `nemr export` and `nemr import` are
  open-source CLI surface and must work standalone, against a local file, with no
  account and no network.
- E-12: **Error model — RESOLVED 2026-08-21 (Product Owner), recorded as D-07 in
  `docs/DECISIONS.md`.** An internal `thiserror` taxonomy lands **now**, as the
  first commit of WP D, before the export code; the gRPC status mapping is
  deferred to the daemon boundary. The two layers are independent — the mapping
  can be added later without touching the enum. Claude Code's recommendation to
  defer both was rejected on its own reasoning: if guessing at the gRPC surface
  means redoing work, then so does letting WP D invent ad-hoc error handling that
  the daemon must later unify. WP D introduces failure modes that do not exist
  yet — partial upload, corrupt bundle, digest mismatch, quota exceeded on
  import, base image absent on the destination — and those want a designed
  taxonomy before they are written, not a retrofit afterwards.
- F-54: **`.claude.json` portable-vs-identity split — RESOLVED 2026-08-21
  (Product Owner).** A field-level **allowlist**, not a blocklist: enumerate the
  fields that travel; everything else stays by default, **including fields that
  do not exist yet**. A blocklist would silently leak whatever Anthropic adds in
  the next Claude Code release, making D-02 true only until the schema changes —
  and we control neither that schema nor our notification of it moving. MCP
  configuration travels. `machineID`, `oauthAccount`, and anything account- or
  machine-shaped does not. **Anything unrecognised does not travel and is
  logged**, so schema drift surfaces as a visible warning rather than a silent
  inclusion or a silent drop.
**Escalation ID namespace.** These `E-0x` IDs are the canonical escalation
ledger, now including the former Section 7 product gates as E-09 (resolved),
E-10 and E-11. `docs/DECISIONS.md` is the Product Owner's record of rulings and
uses the same `E-` series plus its own `D-0x` series for decisions raised
outside the escalation path. The two files share one `E-` namespace so a
cross-reference is unambiguous; the Product Owner maintains `docs/DECISIONS.md`,
and this section is kept in step with it.

---

## 10. Definition of Done — Phase 1

Phase 1 is complete when the Product Owner can, on the reference Ubuntu VM,
execute `scripts/e2e_smoke_test.sh` from a clean checkout with only Section
3.3 prerequisites installed, and observe it: create an isolated,
quota-bounded, pre-configured Claude Code environment; execute a Claude Code
command within it; stop it; list it accurately; and delete it with no
residual host state — entirely through the `nemr` CLI, with zero Docker
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
| 2026-08-10 | 3.4 | Added `src/bin/raw_connectivity.rs` | Section 3.4's tree lists only `nemr.rs` under `src/bin/`, but Milestone 1 scope item 1 requires a raw baseline program. Kept separate from the CLI so `nemr` never links the raw `containerd-client` path. | Pending |
| 2026-08-11 | 3.4 | Added `src/bin/wrapper_connectivity.rs` | AC-1.2 requires showing wrapper output identical to the baseline's, which needs a runnable harness using only the wrapper. Adding a flag to `raw_connectivity` instead would have made the "raw" binary link the wrapper, destroying its independence as a baseline. | Pending |
| 2026-08-11 | 3.4 | Added `src/engine/tty.rs` | Interactive attach (PROC-02) needs local terminal handling: raw mode as an RAII guard, FIFO creation and opening, and IO pumps. Kept out of `project.rs` because it is host-terminal mechanics rather than project logic, and out of `src/containerd/` because it is not a containerd concern. Section 3.4's diagram updated to match. | Pending |
| 2026-08-11 | 3.4 | Added `deploy/` (helper crate + sudoers rule) | The PRIV-03 privileged helper is a separate privilege domain and a standalone crate; Section 3.4's original tree predates Section 3.7. Now reflected in the diagram. | Pending |
| 2026-08-11 | 3.3 | `PREREQUISITES.md` documents rootless tooling (`uidmap`, `rootlesskit`, `slirp4netns`) not enumerated in Section 3.3 | Section 3.7 (PRIV-01) requires rootless containerd, which needs this tooling; Section 3.3's list predates 3.7 and was not updated alongside it. Documented rather than silently assumed, since 3.3 designates `PREREQUISITES.md` as the clean-host provisioning source. Section 3.3 itself left unedited — Sections 1–3 are Product Owner territory per 4A.5. | Pending |
| 2026-08-20 | 3.8 | `attach` allocates a pty only when its own stdin is a terminal; PROC-02 requires one per call unconditionally | A pty has no EOF, so a scripted `echo cmd \| nemr attach` can never signal end-of-input and the session hangs forever. EOT and `CloseIO` were both tried against a pty and both hung. Milestone 7's smoke test is non-interactive and therefore depends on the pipe path existing. Recorded rather than resolved: PROC-02 is in Section 3.8, which is Product Owner territory per 4A.5. Escalated as E-07; full reasoning in the Milestone 7 note under Milestone 5. | **Escalated (E-07)** |
| 2026-08-20 | 3.7 | Privileged helper hardened against a demonstrated local root escalation | `cmd_mount` checked the mount point with `is_dir()` (which follows symlinks) and passed the path by name to `mount(8)`; a caller who owns `~/.local/share/nemr/mounts` replaced `<name>` with a symlink to `/etc` and the helper mounted an attacker-authored ext4 over `/etc`. The backing-file `losetup` and the post-mount `chown` were re-resolved by name after their checks (TOCTOU), and there was no lock against a concurrent double-mount. All paths are now resolved once to an `O_NOFOLLOW` descriptor and operated on via `/proc/self/fd`; the flow is serialised on the backing file. Section 3.7 already required "symlinks are refused" and all validation "inside the helper", so this is an implementation correction, not a requirement change — but it is security-critical, so it is flagged as E-08 for Product Owner visibility. | **Escalated (E-08)** |
| 2026-08-20 | 3.3 | Helper requires Linux 5.8+ (LOOP_CONFIGURE) | Loop attach moved from a `losetup` subprocess to the `LOOP_CONFIGURE` ioctl (5.8+) as part of the E-08 hardening. The helper detects an older kernel and errors clearly; PREREQUISITES.md Step 0 now checks `uname -r`. Ubuntu 22.04+/24.04 satisfy it. No fallback to pre-5.8 `LOOP_SET_FD` — below the supported floor and untestable here. Section 3.3 is Section 1-3 territory (4A.5), so recorded here rather than edited into 3.3. | Recorded |
| 2026-08-20 | 3 (new) | Single-source-of-truth precedence rule for state reconciliation (A6) | **Rule:** containerd's container records are the sole source of truth for which projects exist (there is no side database). Any mount, loop device, or snapshot with no owning container record is an orphan and is reclaimed by `nemr reconcile` / `reconcile_orphans`. The one exception is a backing *file*, which may hold user data: its mount and loop device are released, but the file is reported and kept, never auto-deleted. `delete` is ordered so the container record — the anchor `list`/`resolve` use — is removed last, after the volume is released and the backing file removed, so a failure mid-delete leaves the project listable and the delete retryable; reconciliation is the backstop for a crash after the record is gone. This wants promotion to a normative Section 3.10, which is Product Owner territory (4A.5); recorded here meanwhile. | Recorded, awaiting promotion |
| 2026-08-20 | 3 (new) | State-locality findings (WP-C1) — session state is on the rootfs, not the volume | Measured empirically (docs/state-locality.md): conversation history (`/root/.claude/projects/<hash>/*.jsonl`), session state (`sessions/`) and config+identity (`/root/.claude.json`, holding machineID/oauthAccount) all land on the ephemeral rootfs snapshot; only project files (`/workspace/**`) land on the portable volume. The credential (`/root/.claude/.credentials.json`) is a read-only host bind-mount, on neither portable layer (D-02-compliant by construction). Resumption verified by history continuity across stop/start. **Consequence:** an export of the volume today carries no history — M8 must relocate `projects/` and `sessions/` onto the volume, surgically, keeping credentials and `.claude.json` identity off it. Wants promotion to a normative Section 3 subsection (PO territory, 4A.5). | Recorded, awaiting promotion |
| 2026-08-20 | 3 (new) / M8 | Session-state relocation onto the portable volume (WP-C2) | `create` now creates `<volume>/.nemr-state/{projects,sessions}` and bind-mounts them over `/root/.claude/{projects,sessions}`, so conversation history lands on the layer that travels. Deliberately surgical: `/root/.claude.json` (machineID/oauthAccount) and `/root/.claude/.credentials.json` (the credential) stay on the rootfs, never on the volume (D-02). Verified end-to-end with real Claude Code — after stop + volume unmount + remount, `--continue` recalled the session; the transcript is on the volume backing image and absent from the rootfs snapshot upperdir — and by a non-#[ignore]d regression test (`m8_session_state_lives_on_the_volume_and_vanishes_when_unmounted`). Wants promotion to a Section 3 subsection (PO territory, 4A.5). | Recorded, awaiting promotion |

This table is the single source of truth for deviations (Section 8, 4A.5).
`README.md` points here rather than reproducing it.
