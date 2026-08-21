//! Permanent regression tests for the defects Phase 1 shipped and then fixed.
//!
//! Each test here corresponds to a numbered requirement and a real incident.
//! None of them is `#[ignore]`d, and none of them skips silently — see
//! `tests/common/mod.rs` for why that distinction is the entire point.
//!
//! Run the whole suite:
//!
//! ```text
//! cargo test --test regression -- --test-threads=1
//! ```
//!
//! `--test-threads=1` is required: these attach loop devices, mount
//! filesystems and start containers, all of which are global host state.

mod common;

use std::time::{Duration, Instant};

use common::{extracted_plaintext, installed_helper_matches_built, require_host, unit_only, HostRequirements, TestProject};
use nemr_engine::containerd::client::ContainerdClient;
use nemr_engine::containerd::containers::StopOutcome;
use nemr_engine::engine::project;
use nemr_engine::engine::volume::{self, is_mounted, HelperOps, PrivilegedOps, Volume, VolumePaths, VolumeSize};

/// TEST-01 — the installed helper must be the one this suite is testing.
///
/// # The gap this closes
///
/// The PRIV-03 hardening shipped a helper that refused every attack in 13 unit
/// tests and via direct-invocation testing — and could not provision a single
/// volume, because the fd-based design was routed through `losetup`/`mount`
/// subprocesses that share neither the helper's fd table nor its column
/// vocabulary. Every test passed; the deployed artifact was non-functional.
///
/// That is the VOL-05 shape again — green while wrong — and the root cause was
/// that the tests exercised the rejection paths and pure logic, never a real
/// provision against the installed binary. This canary makes "green" require
/// that the installed helper is byte-identical to the built source, so a source
/// change that was not reinstalled (`sudo ./scripts/setup_test_host.sh`) fails
/// the suite loudly instead of testing a binary nobody runs.
#[test]
fn test_01_installed_helper_matches_built_source() {
    if unit_only() {
        return;
    }
    if !std::path::Path::new(HelperOps::DEFAULT_HELPER).exists() {
        panic!(
            "no privileged helper is installed at {}. These regression tests exercise the \
             installed helper, so it must be present.\n     fix: sudo ./scripts/setup_test_host.sh",
            HelperOps::DEFAULT_HELPER
        );
    }
    installed_helper_matches_built().unwrap_or_else(|reason| panic!("{reason}"));
}

/// PROC-06 — SIGTERM was silently discarded by `sleep infinity` as PID 1.
///
/// # The defect
///
/// PROC-01 originally specified `sleep infinity` as the container's supervisor.
/// Per `pid_namespaces(7)`, the kernel delivers a signal sent from an ancestor
/// namespace to a namespace's PID 1 **only if that process has installed a
/// handler for it**; SIGKILL and SIGSTOP are the only exceptions. `sleep`
/// installs no handlers at all — its `/proc/1/status` reports
/// `SigCgt: 0000000000000000` — so `stop`'s SIGTERM was dropped by the kernel
/// without reaching the process.
///
/// The visible effect was not an error. `nemr stop` still worked: it waited out
/// the full five-second grace period, escalated to SIGKILL, and reported
/// success. Measured on the reference host before the fix, every stop took
/// **6.4 to 6.8 seconds** and every container died of SIGKILL. Nothing in the
/// output said so.
///
/// # Why this asserts on the signal path
///
/// Asserting only that the task ends up stopped passes just as happily against
/// the broken version — the SIGKILL fallback does stop it. So the assertion is
/// on *how* it stopped: [`StopOutcome`] records whether the graceful path
/// succeeded, and this test requires `Graceful`. The elapsed-time bound is a
/// second, independent check on the same property.
#[test]
fn proc_06_stop_terminates_gracefully_without_escalating_to_sigkill() {
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "proc06", VolumeSize::Small).await;

        nemr_engine::engine::project::start(&client, &project.name)
            .await
            .expect("start");

        let started = Instant::now();
        let outcome = nemr_engine::engine::project::stop(&client, &project.name)
            .await
            .expect("stop");
        let elapsed = started.elapsed();

        assert_eq!(
            outcome,
            StopOutcome::Graceful,
            "stop escalated to SIGKILL after {elapsed:?}. PID 1 is ignoring SIGTERM, which \
             means it installs no handler for it — check config::SUPERVISOR_ARGS. A container \
             that can only be killed is a container that never gets to flush anything."
        );

        assert!(
            elapsed < Duration::from_secs(3),
            "stop took {elapsed:?}. A graceful stop should be near-instant; anything \
             approaching the five-second grace period means the signal is not being handled."
        );

        assert!(
            !nemr_engine::engine::project::is_running(&client, &project.name)
                .await
                .expect("is_running"),
            "the task must actually be gone after stop, not merely signalled"
        );
    });
}

/// PROC-06, static half — the supervisor must install a SIGTERM handler.
///
/// The integration test above is the real proof, but it needs a provisioned
/// host. This one needs nothing, so it runs everywhere including a bare CI
/// container, and it fails the moment someone reverts the supervisor to a
/// command that cannot trap anything.
///
/// It deliberately checks for the *mechanism* (a shell with a TERM trap) rather
/// than string-matching the exact command, so the command can be rewritten
/// without a spurious failure — but it cannot be rewritten back to a bare
/// `sleep`.
#[test]
fn proc_06_supervisor_installs_a_sigterm_handler() {
    let args = nemr_engine::config::SUPERVISOR_ARGS;
    let command = args.join(" ");

    assert!(
        command.contains("trap"),
        "the supervisor must install a signal handler, or the kernel will discard SIGTERM \
         sent to PID 1 (pid_namespaces(7)). Got: {command:?}"
    );
    assert!(
        command.contains("TERM"),
        "the supervisor's trap must cover TERM specifically, since that is what `stop` sends. \
         Got: {command:?}"
    );
    assert!(
        !command.starts_with("sleep "),
        "`sleep` installs no signal handlers (SigCgt: 0000000000000000), so SIGTERM to PID 1 \
         is dropped by the kernel and every `stop` degrades to a timeout plus SIGKILL. \
         This is the PROC-06 defect. Got: {command:?}"
    );
}

/// The provisioning success path, end to end through the *installed* helper.
///
/// This is the coverage whose absence let a non-functional helper pass CI. It
/// asserts the helper actually attaches a loop device, mounts an ext4
/// filesystem, and chowns it so the invoking user can write — i.e. every step
/// the fd-based ioctl rewrite touches — rather than only that attacks are
/// refused.
#[test]
fn vol_provision_mount_and_ownership_success_path() {
    if !require_host(HostRequirements::VOLUME) {
        return;
    }

    let name = common::unique_name("volok");
    common::purge(&name);

    let paths = VolumePaths::from_env().expect("HOME set");
    let volume = Volume::create(&name, VolumeSize::Small, paths, HelperOps::new())
        .unwrap_or_else(|e| panic!("provisioning must succeed against the installed helper: {e:#}"));
    let mount_point = volume.mount_point();

    // 1. The volume is genuinely mounted (loop attach + mount both worked).
    assert!(
        is_mounted(&mount_point),
        "the volume must be mounted after create — if this fails against a fresh helper, the \
         loop attach or mount step is broken"
    );

    // 2. The chown worked: the invoking user can write to the volume root. On a
    //    freshly formatted ext4 the root inode is root-owned, so without the
    //    PRIV-06 chown this write fails with EACCES — and the container (which
    //    maps to this uid) could not use its own volume.
    let marker = mount_point.join("provision-marker");
    std::fs::write(&marker, b"written by the invoking user")
        .unwrap_or_else(|e| panic!("the volume must be writable by the invoking user (PRIV-06 chown): {e}"));
    assert_eq!(std::fs::read(&marker).unwrap(), b"written by the invoking user");

    // 3. The quota is real: the filesystem's total does not exceed the request.
    let usage = volume::usage(&mount_point).expect("a mounted volume reports usage");
    assert!(
        usage.total <= VolumeSize::Small.bytes(),
        "quota must cap the filesystem at <= 500MB; got {} bytes",
        usage.total
    );

    // 4. Release: drop unmounts and detaches, leaving nothing mounted.
    drop(volume);
    assert!(
        !is_mounted(&mount_point),
        "the volume must be unmounted after the guard drops (loop detach + umount both worked)"
    );

    common::purge(&name);
}

/// VOL-06 — a volume lost to a reboot is remounted on start, never silently
/// fallen through to the host filesystem.
///
/// The reference defect: after a reboot the container record survives but the
/// mount and loop device do not, and `start` used to proceed against whatever
/// filesystem the mount-point directory happened to sit on — the host root, with
/// no quota and none of the project's data — reporting success the whole way.
///
/// This reproduces the post-reboot state exactly (unmount + detach via the same
/// helper a reboot's teardown is equivalent to) and asserts the volume is
/// remounted with its data and quota intact. It is not `#[ignore]`d: the whole
/// point is that the reference defect's guard runs on every pass.
#[test]
fn vol_06_start_remounts_a_volume_lost_to_reboot() {
    if !require_host(HostRequirements::VOLUME) {
        return;
    }

    let name = common::unique_name("vol06");
    common::purge(&name);

    let paths = VolumePaths::from_env().expect("HOME set");
    let volume = Volume::create(&name, VolumeSize::Small, paths.clone(), HelperOps::new())
        .unwrap_or_else(|e| panic!("setup: provisioning must succeed: {e:#}"));
    let mount_point = volume.mount_point();

    // A marker proves the *same* filesystem returns, not merely that something
    // is mounted.
    let marker = mount_point.join("vol06-marker");
    std::fs::write(&marker, b"before the simulated reboot").expect("write marker");
    let _ = volume.persist();

    // Reproduce the post-reboot state: mount and loop gone, backing file intact.
    HelperOps::new()
        .unmount_and_detach(&name)
        .expect("simulated reboot teardown");
    assert!(!is_mounted(&mount_point), "precondition: unmounted after simulated reboot");
    assert!(!marker.exists(), "data is invisible while unmounted");
    assert!(
        paths.image_file(&name).exists(),
        "the backing file must survive a reboot — it is what makes remount possible"
    );

    // The fix: start detects the missing mount and remounts, rather than running
    // against the host filesystem.
    project::ensure_volume_mounted(&name)
        .unwrap_or_else(|e| panic!("VOL-06: start must remount, not proceed unmounted: {e:#}"));

    assert!(is_mounted(&mount_point), "VOL-06: the volume must be mounted again");
    assert_eq!(
        std::fs::read(&marker).expect("marker must be back"),
        b"before the simulated reboot",
        "VOL-06: the SAME filesystem must return, data intact"
    );
    let usage = volume::usage(&mount_point).expect("mounted volume reports usage");
    assert!(
        usage.total <= VolumeSize::Small.bytes(),
        "VOL-06: quota must be back in force; got {} bytes",
        usage.total
    );

    common::purge(&name);
}

/// VOL-06 — a project whose backing file is gone must fail loudly, never
/// silently start against the host filesystem.
#[test]
fn vol_06_missing_backing_file_fails_loudly() {
    if !require_host(HostRequirements::VOLUME) {
        return;
    }

    let name = common::unique_name("vol06gone");
    common::purge(&name);

    let paths = VolumePaths::from_env().expect("HOME set");
    // Create the mount-point directory but no backing file — the state a lost
    // volume leaves behind.
    std::fs::create_dir_all(paths.mount_point(&name)).expect("create mount point");

    let error = project::ensure_volume_mounted(&name)
        .expect_err("a missing backing file must be an error, not a silent pass")
        .to_string();
    assert!(
        error.contains("no backing file"),
        "the error must say what is wrong: {error}"
    );

    common::purge(&name);
}

/// M8 — session-critical state is relocated onto the portable volume.
///
/// WP-C1 measured that Claude Code writes its conversation history under
/// `/root/.claude/projects` on the ephemeral rootfs, which does not travel with
/// a volume export (docs/state-locality.md). M8 bind-mounts that subtree from
/// the volume. This test proves the relocation end to end, in the faithful
/// direction — a write *inside the container* to Claude Code's history path must
/// land on the volume, survive a stop and an actual unmount, and come back
/// readable after remount.
///
/// It does not use the real API (that is the smoke test's job and would be
/// flaky here); it proves the storage relocation deterministically. "Genuinely
/// gone when unmounted" is asserted directly, which is also the proof the state
/// is not cached on the rootfs — if it were, unmounting the volume would not
/// remove it.
#[test]
fn m8_session_state_lives_on_the_volume_and_vanishes_when_unmounted() {
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "m8", VolumeSize::Small).await;
        project::start(&client, &project.name).await.expect("start");

        let token = format!("history-token-{}", std::process::id());
        let container_path = "/root/.claude/projects/proof.jsonl";
        let host_path = VolumePaths::from_env()
            .unwrap()
            .mount_point(&project.name)
            .join(nemr_engine::config::VOLUME_STATE_PROJECTS)
            .join("proof.jsonl");

        // 1. Write to Claude Code's history path FROM INSIDE the container.
        let (code, _) = project::exec_capture(
            &client,
            &project.name,
            &["/bin/sh", "-c", &format!("printf '%s' '{token}' > {container_path}")],
        )
        .await
        .expect("write inside container");
        assert_eq!(code, 0, "the in-container write must succeed");

        // 2. It must have landed on the VOLUME, visible from the host.
        assert!(
            host_path.exists(),
            "a write to {container_path} must land on the volume at {} — the relocation bind \
             mount is missing or wrong",
            host_path.display()
        );
        assert_eq!(
            std::fs::read_to_string(&host_path).unwrap(),
            token,
            "the volume must hold exactly what the container wrote"
        );

        // 3. Stop, then actually unmount the volume.
        project::stop(&client, &project.name).await.expect("stop");
        HelperOps::new()
            .unmount_and_detach(&project.name)
            .expect("unmount the volume");

        // 4. Genuinely gone — not cached on the host, not on the rootfs (if it
        //    were on the rootfs, unmounting the volume would not remove it).
        assert!(
            !host_path.exists(),
            "with the volume unmounted the session state must be gone from {}; if it survives, \
             it was not really on the volume",
            host_path.display()
        );

        // 5. Remount (start) and read it back inside the container — sourced
        //    from the volume, intact.
        project::start(&client, &project.name).await.expect("restart");
        let (code, out) = project::exec_capture(
            &client,
            &project.name,
            &["/bin/cat", container_path],
        )
        .await
        .expect("read back inside container");
        assert_eq!(code, 0, "the history file must be readable again after remount");
        assert_eq!(
            out.trim(),
            token,
            "the session state must come back from the volume, byte-identical"
        );

        project::stop(&client, &project.name).await.ok();
    });
}

/// VOL-05 / AC-3.3 — writing past the quota fails with a clear ENOSPC, never a
/// silent short write. Migrated from a `#[ignore]`d volume.rs test into the
/// non-skippable suite.
#[test]
fn vol_write_past_quota_fails_with_enospc() {
    use std::io::Write;
    if !require_host(HostRequirements::VOLUME) {
        return;
    }
    let name = common::unique_name("enospc");
    common::purge(&name);
    let paths = VolumePaths::from_env().unwrap();
    let volume = Volume::create(&name, VolumeSize::Small, paths, HelperOps::new())
        .unwrap_or_else(|e| panic!("provision: {e:#}"));

    let target = volume.mount_point().join("filler.bin");
    let mut file = std::fs::File::create(&target).expect("volume writable");
    let chunk = vec![0u8; 4 * 1024 * 1024];
    let mut written: u64 = 0;
    let error = loop {
        match file.write_all(&chunk).and_then(|()| file.flush()) {
            Ok(()) => {
                written += chunk.len() as u64;
                assert!(
                    written < VolumeSize::Small.bytes() * 2,
                    "wrote {written} bytes into a {}-byte volume without hitting a limit — the \
                     quota is not enforced",
                    VolumeSize::Small.bytes()
                );
            }
            Err(e) => break e,
        }
    };
    assert_eq!(
        error.raw_os_error(),
        Some(28),
        "expected ENOSPC (28) at the quota, got {error:?}"
    );
    assert!(written < VolumeSize::Small.bytes(), "must not exceed the volume size");
    drop(volume);
    common::purge(&name);
}

/// VOL-04 / AC-3.4 — a fault after a successful mount must leave no orphaned
/// loop device or mount (RAII cleanup). Migrated from `#[ignore]`.
#[test]
fn vol_fault_injection_leaves_no_orphans() {
    use nemr_engine::engine::volume::VolumeSize as VS;
    if !require_host(HostRequirements::VOLUME) {
        return;
    }

    // Wraps the real helper but fails *after* the mount succeeds — host state is
    // genuinely live at that point, so cleanup is load-bearing, not cosmetic.
    struct FailAfterMount {
        inner: HelperOps,
    }
    impl PrivilegedOps for FailAfterMount {
        fn attach_and_mount(&self, name: &str, size: VS) -> anyhow::Result<()> {
            self.inner.attach_and_mount(name, size)?;
            anyhow::bail!("injected fault: failure after mount succeeded")
        }
        fn unmount_and_detach(&self, name: &str) -> anyhow::Result<()> {
            self.inner.unmount_and_detach(name)
        }
    }

    let name = common::unique_name("faultinj");
    common::purge(&name);
    let paths = VolumePaths::from_env().unwrap();

    let result = Volume::create(&name, VS::Small, paths.clone(), FailAfterMount { inner: HelperOps::new() });
    // CONTROL: assert the failure is the INJECTED one, not an earlier failure in
    // create (name validation, sparse allocation, mkfs). Without this, an early
    // failure would leave nothing mounted and the no-orphan assertions below
    // would pass vacuously — the suite's only RAII-cleanup coverage silently
    // ceasing to exercise cleanup while staying green (F-56 class).
    let error = match result {
        Ok(_) => panic!("the injected fault must fail creation"),
        Err(e) => e,
    };
    assert!(
        format!("{error:#}").contains("injected fault"),
        "the failure must be the injected one, or this test proves nothing: {error:#}"
    );

    // No residue: the mount genuinely happened, then failed — Drop must release it.
    assert!(!is_mounted(&paths.mount_point(&name)), "no orphaned mount after the fault");
    let loop_attached = std::process::Command::new("losetup")
        .arg("-a")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("{name}.img")))
        .unwrap_or(false);
    assert!(!loop_attached, "no orphaned loop device after the fault");
    common::purge(&name);
}

/// VOL-06 — remount is idempotent: two `ensure_volume_mounted` calls must not
/// stack a second loop device. Migrated from `#[ignore]`.
#[test]
fn vol_remount_is_idempotent() {
    if !require_host(HostRequirements::VOLUME) {
        return;
    }
    let name = common::unique_name("idem");
    common::purge(&name);
    let paths = VolumePaths::from_env().unwrap();
    let volume = Volume::create(&name, VolumeSize::Small, paths.clone(), HelperOps::new())
        .unwrap_or_else(|e| panic!("provision: {e:#}"));
    let mount_point = volume.mount_point();
    let _ = volume.persist();

    let device_of = || {
        std::process::Command::new("losetup")
            .args(["-j", &paths.image_file(&name).to_string_lossy()])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
            .unwrap_or(0)
    };
    let before = device_of();
    project::ensure_volume_mounted(&name).unwrap();
    project::ensure_volume_mounted(&name).unwrap();
    let after = device_of();

    assert!(is_mounted(&mount_point), "still mounted");
    assert_eq!(before, after, "repeated remount must not attach a second loop device");
    assert_eq!(after, 1, "exactly one loop device backs the image");
    common::purge(&name);
}

/// M10 — a bundle exported from one project imports into another with the
/// session intact.
///
/// The acceptance is *continuity*, not file presence. This test proves the
/// storage half deterministically: a transcript written on the source appears
/// byte-identical at the path Claude Code reads on the destination, into a
/// project that provably had no history of its own. The live-API half — that
/// `claude --continue` actually recalls it — is exercised manually and by the
/// smoke test, because an API round-trip inside a regression test would make
/// the suite flaky and credential-dependent.
///
/// The control matters: the destination is asserted empty *before* the import,
/// so a pass cannot come from the destination having had the content already.
#[test]
fn m10_bundle_round_trip_carries_the_session_to_another_project() {
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let source = TestProject::create(&client, "m10src", VolumeSize::Small).await;
        let destination = TestProject::create(&client, "m10dst", VolumeSize::Small).await;

        let paths = VolumePaths::from_env().unwrap();
        let source_root = paths.mount_point(&source.name);
        let destination_root = paths.mount_point(&destination.name);

        // Write a transcript on the source, where Claude Code would put it.
        let token = format!("bundle-token-{}", std::process::id());
        let transcript = source_root
            .join(nemr_engine::config::VOLUME_STATE_PROJECTS)
            .join("-workspace/session.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).expect("mkdir");
        std::fs::write(&transcript, &token).expect("write transcript");

        // Control: the destination must have no transcript of its own, or a
        // pass would prove nothing.
        let restored = destination_root
            .join(nemr_engine::config::VOLUME_STATE_PROJECTS)
            .join("-workspace/session.jsonl");
        assert!(
            !restored.exists(),
            "control: the destination must start without this transcript"
        );

        let bundle = std::env::temp_dir().join(format!("m10-{}.nemr", std::process::id()));
        let _ = std::fs::remove_file(&bundle);

        project::export(
            &client,
            &source.name,
            &bundle,
            nemr_engine::bundle::policy::Policy::default(),
        )
        .await
        .expect("export");

        // The bundle must actually contain the session, not merely exist.
        let opened = nemr_engine::bundle::import::open(&bundle).expect("open bundle");
        assert!(
            opened
                .manifest
                .members
                .iter()
                .any(|m| m.path.ends_with("session.jsonl") && m.is_session_critical()),
            "the transcript must be a session-critical member: {:?}",
            opened.manifest.members.iter().map(|m| &m.path).collect::<Vec<_>>()
        );

        project::import(&client, &destination.name, &bundle)
            .await
            .expect("import");

        assert_eq!(
            std::fs::read_to_string(&restored).expect("the transcript must exist on the destination"),
            token,
            "the session must arrive byte-identical at the path Claude Code reads"
        );

        let _ = std::fs::remove_file(&bundle);
    });
}

/// D-02, asserted against a real bundle rather than the policy unit.
///
/// The credential must not appear in a bundle exported from a real project. The
/// policy test alone was once satisfied by a path that could not occur, so this
/// one greps the actual bytes.
#[test]
fn m9_a_real_bundle_contains_no_credential() {
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "m9cred", VolumeSize::Small).await;

        let bundle = std::env::temp_dir().join(format!("m9cred-{}.nemr", std::process::id()));
        let _ = std::fs::remove_file(&bundle);
        project::export(
            &client,
            &project.name,
            &bundle,
            nemr_engine::bundle::policy::Policy::default(),
        )
        .await
        .expect("export");

        // Assert on EXTRACTED plaintext, never the compressed bundle bytes
        // (F-57: a raw grep degrades to a no-op once zstd actually compresses).
        let opened = nemr_engine::bundle::import::open(&bundle).expect("open");
        assert!(
            !opened
                .manifest
                .members
                .iter()
                .any(|m| m.path.contains(".credentials.json")),
            "no bundle member may reference the credential file (D-02)"
        );

        let plaintext = extracted_plaintext(&bundle);
        // And the host's real credential contents must not appear either.
        if let Ok(host_credential) = std::fs::read_to_string(
            nemr_engine::auth::host_credentials_path().expect("credential path"),
        ) {
            for line in host_credential.lines().filter(|l| l.len() > 24) {
                assert!(
                    !plaintext.contains(line.trim()),
                    "a line of the host credential appeared in the bundle (D-02)"
                );
            }
        }

        let _ = std::fs::remove_file(&bundle);
    });
}

/// F-55 — MCP configuration travels, and identity does not.
///
/// # Why there is no bind-mount here
///
/// F-54 rules that MCP configuration must travel while machine and account
/// identity must not. The obvious implementation was to bind-mount a filtered
/// `.claude.json` from the volume, which would have put identity fields *on*
/// the exportable layer and defended them with an export-time filter.
///
/// Claude Code makes that unnecessary. It natively supports project-scoped MCP
/// configuration in `.mcp.json` at the project root — verified by
/// `claude mcp list` discovering a server declared there — and the project root
/// *is* the volume. So MCP configuration travels as an ordinary project file,
/// while `machineID` and `oauthAccount` stay in `/root/.claude.json` on the
/// rootfs and never reach the volume at all.
///
/// That is the difference between D-02 held by policy and D-02 held by
/// structure: no filter can fail open on a field that was never there. It is
/// also why this needed no M8 bind-mount change and therefore did not
/// invalidate D-06.
///
/// This test carries a **control**: it asserts a known-present string IS found
/// by the same search that reports identity absent, so a pass cannot come from
/// the search being broken (the F-56 lesson).
#[test]
fn f55_mcp_config_travels_and_identity_does_not() {
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "f55", VolumeSize::Small).await;

        let mount = VolumePaths::from_env().unwrap().mount_point(&project.name);
        // Project-scoped MCP configuration, exactly where Claude Code reads it.
        let marker = "mcp-marker-travels";
        std::fs::write(
            mount.join(".mcp.json"),
            format!(r#"{{"mcpServers":{{"{marker}":{{"command":"echo"}}}}}}"#),
        )
        .expect("write .mcp.json");

        let bundle = std::env::temp_dir().join(format!("f55-{}.nemr", std::process::id()));
        let _ = std::fs::remove_file(&bundle);
        project::export(
            &client,
            &project.name,
            &bundle,
            nemr_engine::bundle::policy::Policy::default(),
        )
        .await
        .expect("export");

        let opened = nemr_engine::bundle::import::open(&bundle).expect("open");

        // MCP configuration must be a session-critical member.
        assert!(
            opened
                .manifest
                .members
                .iter()
                .any(|m| m.path == ".mcp.json" && m.is_session_critical()),
            "MCP configuration must travel (F-54): {:?}",
            opened.manifest.members.iter().map(|m| &m.path).collect::<Vec<_>>()
        );

        // Search the EXTRACTED plaintext, not the compressed bundle (F-57).
        let plaintext = extracted_plaintext(&bundle);

        // CONTROL: the same search must find something known to be present, or
        // the identity assertions below prove nothing.
        assert!(
            plaintext.contains(marker),
            "control: the marker must be findable in the extracted plaintext, or \
             the absence assertions below are vacuous"
        );

        // And identity must be absent — held structurally, since these fields
        // live on the rootfs and never reach the volume.
        for identity in ["machineID", "oauthAccount"] {
            assert!(
                !plaintext.contains(identity),
                "{identity} must never appear in a bundle (F-54)"
            );
        }

        let _ = std::fs::remove_file(&bundle);
    });
}
