# Conformance Matrix

Traceability from every NEMR-SPEC-001 requirement to the code that implements it
and the test that proves it, plus the ledger of audit findings and their
disposition. Kept current from WP A onward.

**How to read the status column**

| Status | Meaning |
|---|---|
| ✅ done | Implemented and covered by a test that would catch a regression. |
| 🟡 partial | Implemented, but the test is a smoke-script step or missing; a dedicated test is owed. |
| 🔶 drifted | Code does something reasonable the spec no longer describes; resolution noted. |
| ⛔ absent | Not implemented (or a stub). |

**Test locations.** Unit tests live beside their code (`#[cfg(test)]`). Host-backed
regression tests are in `tests/regression.rs`, driven by `tests/common/mod.rs`,
which **refuses to skip** — a missing host facility fails the test with
instructions rather than silently passing (this is why the old `#[ignore]`d
VOL-06 suite never ran). The end-to-end script is `scripts/e2e_smoke_test.sh`.

**What CI proves, and what it does not.** All five CI jobs are green on a clean
Ubuntu runner, including the host-backed regression suite (10/10, hash-gated) and
the E2E smoke test. Two limits are deliberate and must not be blurred by a green
badge:

- CI runs the smoke test with `NEMR_SKIP_API=1`. It never contacts the Anthropic
  API. Only a local run exercises the round-trip.
- CI provisions a **placeholder** credential. That proves AUTH-02's mount
  mechanics — bind-mounted, read-only, at the path Claude Code expects — and
  proves nothing about whether a real credential authenticates.

So a CI pass is a strictly weaker claim than a local `verify_wp_a.sh` pass. The
smoke test prints which mode it ran in; treat "green in CI" and "works" as
different statements, because the gap between them is where this project's
defects have lived.

**Verification note.** Host-backed rows are proven against the *installed*
privileged helper, which `tests/common/mod.rs` enforces by refusing to run
unless `sha256(installed) == sha256(built)` (finding F-11 / TEST-01).

---

## A. Requirement conformance

### Storage volumes (VOL, Section 3.6)

| ID | Requirement | Implementation | Test | Status |
|---|---|---|---|---|
| VOL-01 | Fixed-size presets (500MB/2GB/10GB) | `volume.rs` `VolumeSize` | `size_presets_match_vol_01` | ✅ |
| VOL-02 | Sparse ext4 on a loop device, bind-mounted | `volume.rs` `allocate_sparse_file`/`format_ext4`; helper `loopdev::attach` + `safe::mount_ext4` | `vol_provision_mount_and_ownership_success_path` | ✅ |
| VOL-03 | Every mount/loop/format operation logged | `volume.rs::audit`, helper `audit` | smoke step 1–9 shows the log | ✅ |
| VOL-04 | RAII cleanup on error paths | `Volume: Drop`; `create` failure path | `ac_3_4_fault_injection_leaves_no_orphans` (ignored→host) | 🟡 (integration test still `#[ignore]` in volume.rs; superseded coverage in regression.rs owed) |
| VOL-05 | At-capacity ops fail clearly, no silent loss | ext4 ENOSPC surfaces; `usage()` uses `f_bavail`/`f_bfree` correctly | `ac_3_3_writing_past_capacity_fails_cleanly` (ignored) + smoke step 8 df cross-check | 🟡 (dedicated non-ignored ENOSPC + statvfs unit test owed — F-30) |
| VOL-06 | `start` verifies mount via mountinfo, remounts or fails loudly | `project.rs::ensure_volume_mounted`; `volume.rs::is_mounted` (octal-unescaped) | `vol_06_start_remounts_a_volume_lost_to_reboot`, `vol_06_missing_backing_file_fails_loudly`, `is_mounted_decodes_octal_escaped_mount_points` | ✅ |

### Privilege model (PRIV, Section 3.7)

| ID | Requirement | Implementation | Test | Status |
|---|---|---|---|---|
| PRIV-01 | Rootless containerd; no root-equiv group | `client.rs::default_socket_path` (rootless socket only) | smoke prereqs; runs entirely rootless | ✅ |
| PRIV-02 | losetup/mount privileged; mkfs/alloc not | `volume.rs` (alloc/mkfs unprivileged) + helper (loop/mount) | success-path test | ✅ |
| PRIV-03 | Single root-owned helper, wildcard-free grant, all validation inside | `deploy/nemr-volume/`, `deploy/sudoers.d/nemr-volume`; `safe::open_beneath` fd resolution | `symlinked_*_is_refused` (×3), `sudoers` grant shape | ✅ (was a **local root escalation**, F-01/F-02/F-04; fixed) |
| PRIV-04 | Elevated ops logged as elevated | helper `audit` (`[elevated] …`) | smoke shows `helper:` lines | ✅ |
| PRIV-05 | Split holds Phase 1; widen only via escalation | design; helper spawns zero subprocesses | — | ✅ |
| PRIV-06 | Mount+chown atomic; chown to invoker uid; no separate chown grant | helper `cmd_mount` (`fchown` on the mounted-root fd) | `vol_provision_mount_and_ownership_success_path` (user can write) | ✅ (was chown-follows-symlink escalation, F-05; fixed) |

### Authentication (AUTH, Section 3.5)

| ID | Requirement | Implementation | Test | Status |
|---|---|---|---|---|
| AUTH-01 | Credentials not baked into the image | `image/Dockerfile` (no creds) | smoke step 4 | ✅ |
| AUTH-02 | Host `~/.claude/.credentials.json` bind-mounted read-only | `auth.rs`, `project.rs::create` (`BindMount::read_only`) | smoke step 4 (readable + read-only) | 🔶 (narrowed from whole `~/.claude` to the file, SPEC 1.14) — WP-C1 confirmed it is the **only** secret and stays a host bind-mount off both portable layers (D-02). **F-12** open: a file bind-mount pins an inode, so an atomic-replace credential rotation on the host is invisible in-container. |
| AUTH-03 | Missing credentials → clear, actionable failure | `auth.rs::resolve_credentials` | `missing_credentials_error_is_actionable` | ✅ |

### State locality & relocation (WP-C, new — pending Section 3 promotion)

| ID | Requirement | Implementation | Test | Status |
|---|---|---|---|---|
| STATE-01 (C1) | Determine empirically where session state lives | `docs/state-locality.md` (measured both layers) | experiment (evidence in doc) | ✅ history on rootfs, project files on volume; credential is a host bind-mount |
| M8 (C2) | Relocate session-critical state onto the portable volume | `project.rs::session_state_mounts`, `create`; `config.rs` state paths | `m8_session_state_lives_on_the_volume_and_vanishes_when_unmounted` + real-API `--continue` recall | ✅ surgical (history subtrees only; credential + `.claude.json` identity stay off the volume, D-02) |

### Process model (PROC, Section 3.8)

| ID | Requirement | Implementation | Test | Status |
|---|---|---|---|---|
| PROC-01 | PID 1 is a long-lived supervisor | `config.rs::SUPERVISOR_ARGS` | `proc_06_*` | ✅ |
| PROC-02 | `attach` is a task exec with a fresh TTY per call | `project.rs::attach`, `ModeTracker` (`tty.rs`) | smoke step 3–5; `mode_tracker_tests` (11) | 🔶 **E-07**: a pty is allocated only when stdin is a terminal (a scripted attach must be pipe-backed to terminate). Drift from PROC-02 as written; awaiting Section 3.8 amendment. `ModeTracker` now unit-tested (F-40 closed; found+fixed an ESC-restart parser gap). |
| PROC-03 | Supervisor written explicitly into the runtime spec | `project.rs::create` (`args`) | — | ✅ |
| PROC-04 | `stop` terminates the task incl. live execs | `containers.rs::stop_task` (SIGKILL-all) | — | 🟡 (exec-kill path untested — F-35) |
| PROC-06 *(new)* | Supervisor must trap SIGTERM; `stop` terminates gracefully, not by timeout→SIGKILL | `config.rs::SUPERVISOR_ARGS`, `StopOutcome` | `proc_06_stop_terminates_gracefully_without_escalating_to_sigkill`, `proc_06_supervisor_installs_a_sigterm_handler` | ✅ |

### Network (NET, Section 3.9)

| ID | Requirement | Implementation | Test | Status |
|---|---|---|---|---|
| NET-01 | Share rootlesskit's netns (host-net equivalent) | `containers.rs::oci_spec` (no netns entry) | smoke API round-trip | ✅ |
| NET-02 | Per-project net isolation out of scope | — | — | ✅ (documented) |

### Non-functional (NFR, Section 5)

| ID | Requirement | Implementation | Test | Status |
|---|---|---|---|---|
| NFR-01 | No Docker at any layer, incl. transitively | `scripts/check_docker_free.sh` (CLI/socket, transitive deps, build path) + `deny.toml` bans | CI `docker-free` job; **negative-tested** three ways (socket path, CLI invocation, real `bollard` dep) | ✅ (F-24 closed — now enforced by the pipeline, not memory) |
| NFR-02 | create/start in low single-digit seconds | — | — | 🟡 (measured nowhere — F-45, WP-B benchmark harness) |
| NFR-03 | No orphaned loops/mounts/containerd resources after delete | recoverable `delete` ordering + `reconcile_orphans` | live reconcile test; smoke step 9 | ✅ (cluster F-03/F-10/F-15/F-20/F-23 fixed; exec-record leak on SIGKILLed attach still open — F-18) |
| NFR-04 | Destructive ops logged auditably | `audit` via `tracing` (info, on by default); `NEMR_DEBUG=1` adds decision points | smoke output; VOL-05 debug demo in README | ✅ |
| NFR-05 | No silent privilege escalation; documented | helper via explicit sudo; `setup_test_host.sh` | — | ✅ |

### Acceptance criteria (AC)

AC-1.x (connectivity), AC-2.x (image build), AC-3.1–3.5 (volume), AC-4.x
(create), AC-5.x (start/attach/stop), AC-6.x (list/delete), AC-7.x (E2E) — all
demonstrated historically and by the smoke test. Owed: **AC-3.x volume tests are
still `#[ignore]`d in `volume.rs`** (F-09, F-16); **AC-4.2/AC-5.3 error paths
have no automated test** (F-48); **AC-7.1 "clean host" needs the helper install
scripted** — done via `setup_test_host.sh`.

---

## B. Audit findings ledger

41 confirmed by an adversarial audit (16 agents, find→refute). IDs are stable;
disposition tracked here.

**Fixed on `wp-a-hardening`**

| ID | Sev | Requirement | Finding | Fixed by |
|---|---|---|---|---|
| F-01 | crit | PRIV-03 | Mount point checked with symlink-following `is_dir()` → mount attacker ext4 over `/etc` = root | fd-based `open_beneath` + in-process mount |
| F-02 | crit | PRIV-03 | Backing file re-opened by name after check (TOCTOU) → attach `/dev/sda1` | pinned fd + `/proc/self/fd` |
| F-05 | high | PRIV-06 | `chown` follows symlinks → chown `/etc` to caller | `fchown` on mounted-root fd |
| F-06 | high | VOL-06 | Concurrent `start` double-attaches/double-mounts one image → corruption | backing-file flock + `--nooverlap` intent |
| F-07 | high | VOL-05/06 | `is_mounted` compares raw path to octal-escaped mountinfo → mounted reads unmounted | octal-unescape (engine + helper) |
| F-03/F-10/F-20 | high | NFR-03 | `delete` destroys record before releasing volume → unrecoverable orphan | reorder: record removed last + idempotent |
| F-15/F-23 | med | NFR-03 | Orphan loop/snapshot after crash, no sweep | `reconcile_orphans` + `nemr reconcile` |
| F-17 | high | PROC-04/06 | `sleep infinity` ignores SIGTERM → every stop 6.4s→SIGKILL, reported success | trapping supervisor + `StopOutcome` |
| F-11 (TEST-01) | — | — | Helper passed 13 tests while unable to provision (rejection-only coverage) | hash gate + success-path test |
| F-19 | med | NFR-03 | No engine↔helper version handshake | `version` subcommand (protocol 2) |
| F-30-loop | med | NFR-03 | Loop lookup by path string breaks on `(deleted)`/whitespace | inode-identity via `LOOP_GET_STATUS64` |
| F-30 | med | VOL-05 | statvfs `f_bfree`/`f_bavail` + `Usage::percent` untested | 3 `usage_percent_*` unit tests (df definition) |
| F-40 | med | PROC-02 | `ModeTracker` untested; ESC-restart parser gap | 11 `mode_tracker_tests` + ESC-restart fix |
| F-09/F-16 | high | VOL-06/AC-3 | AC-3.x + VOL-06 tests `#[ignore]`d in volume.rs | migrated to the non-skippable suite; zero ignored tests remain |
| F-24 | med | NFR-01 | No CI — Docker-freeness/lint/licensing unenforced | `.github/workflows/ci.yml` + `check_docker_free.sh` (negative-tested) + `deny.toml` |
| F-37 | med | — | base-image build never scripted (README prose only) | `scripts/build_base_image.sh`, run and verified (200.5 MiB, same digest) |
| F-53 | high | NFR-01/supply chain | **RUSTSEC-2026-0258** — `h2` unbounded empty DATA frames, reachable via `tonic` → `containerd-client` | found by the new `cargo deny` gate on its first run; `h2` 0.4.15 → 0.4.18 |
| **F-54** | high | AUTH-02 / D-02 | `.claude.json` mixes portable config (MCP servers) with machine/account identity (`machineID`, `oauthAccount`) — a file-level include/exclude either leaks identity or drops MCP config | **Ruled 2026-08-21:** field-level **allowlist**; unrecognised fields stay and are logged. Implemented in M9's exclusion policy. |

**Open — this branch / next (WP-A remnant + WP-C follow-up)**

| ID | Sev | Requirement | Finding | Plan |
|---|---|---|---|---|
| F-18 | med | NFR-03 | SIGKILLed attach leaks its containerd exec record | reap stale execs in `reconcile_orphans` |
| **F-58** | high | multiple | **Guard-test audit: 10 confirmed F-56 siblings.** A 10-agent adversarial audit applied the new rule ("would this test pass with the guarded code deleted?") across the suite. Four rated *guards nothing outright*: the kernel-5.8 floor test compared tuple literals to tuple literals; the extraction-ordering test re-implemented the production sort and asserted on its own output; the schema-refusal test called the helper directly while `open()`'s call could be deleted; the F-54 filter tests exercise a code path no real export reaches. Six weaker: fault-injection accepted any error, the helper-hash gate is not source-aware, `mountinfo_device_for` tested one optional-field count, and others. | **4 high fixed and each proven to go red with its guarded code disabled**; remainder tracked below. |
| **F-57** | high | D-02 / F-54 | **Secret-absence guards degraded with size.** Four tests asserted "the secret must not appear in the bundle" by grepping the raw `.nemr` bytes. Chunks are zstd-compressed, so a marker is findable only while content is small enough to store near-verbatim — measured: visible in a 34 B bundle, **invisible in a 241 KiB one**. The guards therefore passed on fixtures and stopped guarding at realistic sizes: they degrade exactly when it matters. A direct F-56 sibling, found by applying the new guard-test rule. | **Fixed:** all four assert on *extracted plaintext* via a shared helper; each carries a positive control; the credential guard was proven to go red with the filter disabled and green with it restored. |
| **F-56** | high | D-02 / M9 | **Guard test guarded the wrong thing.** `credentials_never_travel_under_any_policy` passed while asserting on `root/.claude/.credentials.json` (the container's path); a real export walks the volume, whose layout is `.nemr-state/…`, so the checked path cannot occur. D-02 held **structurally** (credential is a host bind-mount), not because the test enforced it — false assurance, invisible because green. The sixth instance of the recurring green-over-wrong pattern and a new variant. | **Fixed in M9:** filter matches the credential filename anywhere; tests assert against the measured layout. Rule added to `.claude/loop.md`; siblings audited. |
| **F-55** | high | F-54 / M9 | **MCP configuration cannot travel.** F-54 rules that it must, but `.claude.json` lives on the **rootfs**, not the portable volume, so a volume-only export cannot reach it. `filter_claude_json` is correct and currently unreachable in a real export. | Needs an M8 bind-mount change (relocate a portable `.claude.json` onto the volume), which invalidates D-06 until re-verified. **Raised for the M9/M10 review.** |
| F-28 | med | VOL-06 | `ensure_volume_mounted` accepts *any* fs at the mount point (identity not checked) | verify the mount is backed by the project's loop device |
| F-35 | med | PROC-04 | exec-kill on stop untested | add regression test |
| F-12 | high | AUTH-02 | Credential file bind-mount pins an inode → host rotation invisible in-container | WP-C follow-up (mount a dir, or re-resolve) |

**Open — deferred to WP-B/C/D (tracked, not lost)**

| ID | Sev | Requirement | Finding | Owner |
|---|---|---|---|---|
| F-45 | low | NFR-02 | create/start latency measured nowhere | WP-B benchmark harness |
| F-22/F-34 | med | VOL-05 | `list` trusts labels blind; destroyed volume shown as merely "unmounted" | WP-A/B follow-up |
| F-25 | med | NFR-01 | `BASE_IMAGE` in a squattable Docker Hub namespace | pin by digest (WP-B reproducible build) |
| F-46 | low | PROC-02 | `std::process::exit` in attach/delete skips destructors | WP-B error-model unification |
| F-49 | low | NFR-01 | `~/.docker` created by buildctl's vendored telemetry (not Docker) — audit tripwire | document |

**Refuted / no-change** (12 findings): kept in the audit record; not reproduced
against the code (e.g. names are validated in two places, killing several
weird-name scenarios). Available in the workflow journal.

---

## Maintenance

Update this file in the same commit as any change that adds, moves, or resolves
a requirement or finding. The regression suite plus `nemr reconcile` are the
runtime guards; this document is the map. See `.claude/loop.md` for the
regression cadence.
