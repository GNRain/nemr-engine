# Nemr — Engineering Specification
## Phase 1: Core Container Engine

| Field | Value |
|---|---|
| Document ID | NEMR-SPEC-001 |
| Version | 1.39 |
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
| AUTH-02 | At container creation time, the engine shall bind-mount the host user's Claude Code credentials file (`~/.claude/.credentials.json`) into the container, **read-only**, at the path Claude Code expects (`/root/.claude/.credentials.json`). No other host-side `~/.claude` content shall be mounted into the container — session state, history, and cache remain container-local, which is what preserves per-project isolation. |
| AUTH-03 | If no host credentials are found at creation time, the engine shall fail with a clear, actionable error message directing the user to authenticate on the host. Interactive in-container authentication flows are out of scope for Phase 1. |

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
| NET-01 | Project containers shall share rootlesskit's network namespace (equivalent to `nerdctl run --net=host` under rootless); no per-project network namespace isolation exists in Phase 1. |
| NET-02 | Per-project network isolation is explicitly out of scope for Phase 1 (extends Section 1.4). The Phase 2 path is rootless CNI via `rootlesskit --detach-netns` (requires rootlesskit >= 2.0, not present in the Ubuntu archive version currently installed), which changes host setup and the container spec, not the engine's architecture. |

**Background.** A container given its own network namespace receives an
empty one: only loopback, no route, no egress. Nothing wires it up, because
rootless CNI is not configured. This was observed at Milestone 5 as Claude
Code failing with `ENOTIMP` against `api.anthropic.com` while otherwise
running correctly — `/proc/net/dev` inside the container listed `lo` alone.
Inheriting rootlesskit's namespace instead gives the container the `tap0`
device slirp4netns already provides, and DNS and TLS then work.

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
| R-07 | Two projects binding the same port collide: with a shared network namespace (NET-01) the second bind fails | Medium once containers run real services | Low for Phase 1 — nothing in scope runs a service; the base image ships no server and the engine starts only a supervisor | Accepted for Phase 1 and tracked here rather than mitigated. It is the first problem Phase 2 must solve, and the fix is the rootless CNI path in NET-02 |

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
- E-14: **A restore cannot require a host credential — OPEN, needs a ruling.**
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

- E-10: **Non-Linux hosts — OPEN.** Loopback ext4 plus rootless namespaces is
  Linux-only; the product vision is a cross-platform GUI, and D-01 (local
  compute) makes this a direct contradiction rather than a deferred concern.
  Options to cost: bundled VM, WSL2 + a macOS story, or a remote-engine fallback
  that partially reverses D-01. Ruling pending in `docs/DECISIONS.md`.
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
