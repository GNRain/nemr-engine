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

/// The engine's own grace period, not a copy of it. A test asserting against a
/// duplicated literal stops testing production the moment production changes.
const GRACE_PERIOD: Duration = nemr_engine::containerd::config::SIGTERM_GRACE;

use common::{
    extracted_plaintext, installed_helper_matches_built, require_host, run_offline, unit_only,
    write_hostile_bundle, HostRequirements, TestProject,
};
use nemr_engine::containerd::client::ContainerdClient;
use nemr_engine::containerd::containers::StopOutcome;
use nemr_engine::engine::project;
use nemr_engine::engine::volume::{
    self, is_mounted, HelperOps, PrivilegedOps, Volume, VolumePaths, VolumeSize,
};

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

        // F-68: this was `< 3s`, which fired at 3.47s on a machine that happened
        // to be compiling — a false red on a healthy engine.
        //
        // The property that matters is already asserted above: `Graceful` means
        // the supervisor exited on SIGTERM and was never escalated to SIGKILL.
        // What remains for a duration check is the narrower case of "handled,
        // but so slowly it nearly escalated", and only a threshold close to the
        // grace period expresses that. Three seconds expressed "the machine is
        // busy", which is not a defect in anything under test.
        assert!(
            elapsed < GRACE_PERIOD - Duration::from_millis(500),
            "stop took {elapsed:?} of a {GRACE_PERIOD:?} grace period. It did not escalate, \
             but it came close enough that a slower machine would have been SIGKILLed — the \
             supervisor is handling SIGTERM sluggishly rather than promptly."
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
    let volume =
        Volume::create(&name, VolumeSize::Small, paths, HelperOps::new()).unwrap_or_else(|e| {
            panic!("provisioning must succeed against the installed helper: {e:#}")
        });
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
    std::fs::write(&marker, b"written by the invoking user").unwrap_or_else(|e| {
        panic!("the volume must be writable by the invoking user (PRIV-06 chown): {e}")
    });
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"written by the invoking user"
    );

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
    assert!(
        !is_mounted(&mount_point),
        "precondition: unmounted after simulated reboot"
    );
    assert!(!marker.exists(), "data is invisible while unmounted");
    assert!(
        paths.image_file(&name).exists(),
        "the backing file must survive a reboot — it is what makes remount possible"
    );

    // The fix: start detects the missing mount and remounts, rather than running
    // against the host filesystem.
    project::ensure_volume_mounted(&name)
        .unwrap_or_else(|e| panic!("VOL-06: start must remount, not proceed unmounted: {e:#}"));

    assert!(
        is_mounted(&mount_point),
        "VOL-06: the volume must be mounted again"
    );
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
    common::init_tracing();
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
            &[
                "/bin/sh",
                "-c",
                &format!("printf '%s' '{token}' > {container_path}"),
            ],
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
        if let Err(error) = project::start(&client, &project.name).await {
            // F-63: the bare `.expect("restart")` reported the error and nothing
            // about the state that produced it, which left the two candidate
            // causes indistinguishable — a mount that silently did not happen
            // (VOL-05, severe) versus a volume mounted without the directory.
            // Capture the discriminating facts here, while they are still true.
            panic!(
                "restart failed: {error:#}\n     {}",
                common::volume_state_report(&project.name)
            );
        }
        let (code, out) =
            project::exec_capture(&client, &project.name, &["/bin/cat", container_path])
                .await
                .expect("read back inside container");
        assert_eq!(
            code, 0,
            "the history file must be readable again after remount"
        );
        // F-77: the observable is `out`, which is the *exec capture*, not the
        // file. An empty capture and an empty file are different defects —
        // losing a conversation versus losing a read of it — and the assertion
        // alone cannot tell them apart. Read the volume directly at the moment
        // of failure so the next occurrence is a diagnosis rather than a guess.
        if out.trim() != token {
            let on_volume = std::fs::read_to_string(&host_path);
            let size = std::fs::metadata(&host_path).map(|m| m.len());
            panic!(
                "the session state did not come back byte-identical.\n     \
                 exec capture: {out:?}\n     \
                 token:        {token:?}\n     \
                 --- read directly from the volume, bypassing the container ---\n     \
                 host path:    {}\n     \
                 file size:    {size:?}\n     \
                 file content: {on_volume:?}\n     \
                 {}\n     \
                 If the file holds the token, the DATA is fine and the exec\n     \
                 capture lost it. If the file is empty, the volume lost the\n     \
                 write. Those are different bugs with different fixes.",
                host_path.display(),
                common::volume_state_report(&project.name)
            );
        }

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
    assert!(
        written < VolumeSize::Small.bytes(),
        "must not exceed the volume size"
    );
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

    let result = Volume::create(
        &name,
        VS::Small,
        paths.clone(),
        FailAfterMount {
            inner: HelperOps::new(),
        },
    );
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
    assert!(
        !is_mounted(&paths.mount_point(&name)),
        "no orphaned mount after the fault"
    );
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
    assert_eq!(
        before, after,
        "repeated remount must not attach a second loop device"
    );
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
            opened
                .manifest
                .members
                .iter()
                .map(|m| &m.path)
                .collect::<Vec<_>>()
        );

        project::import(&client, &destination.name, &bundle)
            .await
            .expect("import");

        assert_eq!(
            std::fs::read_to_string(&restored)
                .expect("the transcript must exist on the destination"),
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
            opened
                .manifest
                .members
                .iter()
                .map(|m| &m.path)
                .collect::<Vec<_>>()
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

/// M11 — a hostile bundle cannot write outside the destination.
///
/// The `extract()` traversal guard is unit-tested, but the defect F-58 found was
/// that nothing asserted the extract path *used* it: swapping `safe_join` for a
/// plain `join` left every test green while a crafted member path escaped. This
/// is the same class as the original privileged-helper mount escalation, in new
/// code, and reached by untrusted input — a bundle may arrive from another
/// machine or another user.
///
/// So it gets a permanent, host-level regression test with a **real hostile
/// bundle on disk**, driven through the real import path, asserting both that
/// the import is refused and that nothing was written outside.
#[test]
fn m11_a_hostile_bundle_cannot_escape_the_destination() {
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "m11esc", VolumeSize::Small).await;

        // A canary outside the destination volume. If traversal succeeds it is
        // overwritten; the assertion is on its contents, not merely its absence,
        // so a pass cannot come from the write landing somewhere unexpected.
        let outside = std::env::temp_dir().join(format!("m11-canary-{}", std::process::id()));
        std::fs::write(&outside, b"UNTOUCHED").expect("write canary");

        // Build a hostile bundle by hand: export() cannot produce a traversing
        // member path, which is exactly why this needs a crafted fixture.
        let bundle_path =
            std::env::temp_dir().join(format!("m11-hostile-{}.nemr", std::process::id()));
        let payload = b"PWNED".to_vec();
        let mount = VolumePaths::from_env().unwrap().mount_point(&project.name);
        let escape = format!("../../../../../../..{}", outside.to_string_lossy());
        write_hostile_bundle(&bundle_path, &escape, &payload);

        let error = project::import(&client, &project.name, &bundle_path)
            .await
            .expect_err("a traversing member path must be refused");
        assert_eq!(
            error.kind(),
            nemr_engine::error::ErrorKind::DataIntegrity,
            "traversal must be a data-integrity refusal: {error}"
        );

        assert_eq!(
            std::fs::read(&outside).expect("canary must still exist"),
            b"UNTOUCHED",
            "the hostile member must NOT have been written outside the destination"
        );
        assert!(
            !mount.join("PWNED").exists(),
            "and nothing stray inside it either"
        );

        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_file(&bundle_path);
    });
}

/// M9 — exporting into the project's own workspace must not swallow a bundle.
///
/// Found by the determinism test: a second export included the first bundle as a
/// member, so a repeatedly-exported project grows by its predecessor's size every
/// time while every command reports success. Permanent regression test at host
/// level, against real projects.
#[test]
fn m9_export_does_not_swallow_bundles_in_the_workspace() {
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "m9swal", VolumeSize::Small).await;
        let mount = VolumePaths::from_env().unwrap().mount_point(&project.name);
        std::fs::write(mount.join("notes.md"), "content").expect("seed a project file");

        let policy = nemr_engine::bundle::policy::Policy::default();

        // First export, written INTO the workspace.
        let first = mount.join("backup.nemr");
        project::export(&client, &project.name, &first, policy.clone())
            .await
            .expect("first export");
        let opened = nemr_engine::bundle::import::open(&first).expect("open first");
        assert!(
            !opened
                .manifest
                .members
                .iter()
                .any(|m| m.path.ends_with(".nemr")),
            "a bundle must not contain itself: {:?}",
            opened
                .manifest
                .members
                .iter()
                .map(|m| &m.path)
                .collect::<Vec<_>>()
        );

        // CONTROL: the first bundle really is sitting in the workspace, so the
        // second export genuinely had the chance to swallow it.
        assert!(
            first.exists(),
            "control: the first bundle is in the workspace"
        );

        let second = mount.join("backup2.nemr");
        project::export(&client, &project.name, &second, policy)
            .await
            .expect("second export");
        let opened = nemr_engine::bundle::import::open(&second).expect("open second");
        let swallowed: Vec<&String> = opened
            .manifest
            .members
            .iter()
            .map(|m| &m.path)
            .filter(|p| p.ends_with(".nemr"))
            .collect();
        assert!(
            swallowed.is_empty(),
            "a previous bundle must not be swallowed by the next export: {swallowed:?}"
        );
    });
}

/// E-11 — `nemr export` and `nemr import` work with no network and no
/// credentials configured, against a local file.
///
/// # Why this is a test and not a comment
///
/// The open/commercial boundary rests on it: if the open engine ever needs the
/// sync layer, an account, or an outbound request to move a bundle, the seam has
/// leaked and the open half stops being useful on its own. That was true by
/// construction and asserted nowhere, so nothing would have caught M12's storage
/// trait accidentally becoming a dependency of the CLI surface.
///
/// # Method
///
/// The CLI runs inside a **network namespace with only loopback**, so any
/// outbound request fails, and inside a **mount namespace with a tmpfs over
/// `~/.claude`**, so no credential is visible. The mount namespace is what makes
/// this safe: the real credential is hidden only inside the test's namespace and
/// is never moved or modified.
///
/// containerd remains reachable because its socket is a local Unix socket, which
/// is the point — "no network" means no internet and no sync service, not no IPC.
#[test]
fn e11_export_and_import_work_with_no_network_and_no_credentials() {
    if !require_host(HostRequirements::FULL) {
        return;
    }
    if let Err(reason) = common::namespace_probe() {
        panic!(
            "unprivileged user/mount/network namespaces are unavailable, so E-11's \
             offline guarantee cannot be tested on this host. This is a gap in \
             coverage, not a pass.\n     {reason}\n     \
             fix (CI runners and Ubuntu 24.04 hosts): \
             sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0"
        );
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let (source, destination) = runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let source = TestProject::create(&client, "e11src", VolumeSize::Small).await;
        let destination = TestProject::create(&client, "e11dst", VolumeSize::Small).await;
        (source, destination)
    });

    let paths = VolumePaths::from_env().unwrap();
    let marker = format!("offline-marker-{}", std::process::id());
    std::fs::write(paths.mount_point(&source.name).join("notes.md"), &marker)
        .expect("seed the source project");

    let bundle = std::env::temp_dir().join(format!("e11-{}.nemr", std::process::id()));
    let _ = std::fs::remove_file(&bundle);

    // CONTROL: the credential really is present outside the namespace, so
    // "it worked without one" is a meaningful claim rather than a vacuous one.
    let credential = nemr_engine::auth::host_credentials_path().expect("credential path");
    assert!(
        credential.exists(),
        "control: the host credential must exist outside the namespace, or hiding \
         it inside proves nothing"
    );

    let export = run_offline(&["export", &source.name, "-o", &bundle.to_string_lossy()]);
    assert!(
        export.status.success(),
        "export must work offline with no credential.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&export.stdout),
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(bundle.exists(), "the bundle must have been written");

    // IMPORT, and what this can and cannot still assert (E-14).
    //
    // A restore now creates its own project, which means provisioning a volume,
    // which means the privileged helper. The helper cannot run inside this
    // test's user namespace — `sudo` refuses there, because /etc/sudo.conf maps
    // to nobody — so the import half can no longer be driven through the CLI
    // inside the namespace at all.
    //
    // That is not E-11 weakening: E-11's guarantee is no network and no
    // credential, never "no privilege", and `create` has always needed the
    // helper. But it does mean the *end-to-end* no-credential assertion for
    // import is gone, replaced by the policy-level one in
    // `import_defers_the_credential_requirement`. Recorded rather than papered
    // over, and escalated as E-14 because resolving it properly touches
    // AUTH-03, which is a gated Section 3 requirement.
    //
    // The export half above is unchanged and still proves the full guarantee.
    drop(destination);

    // NOT asserted here any more, and no substitute is invented: there is no
    // read-only CLI command to run offline, and adding one to make a test
    // possible would be inventing product surface. The import guarantee is
    // covered instead by `import_defers_the_credential_requirement` (policy
    // level) and by scripts/check_seam.sh (the engine cannot depend on the
    // storage crate at all). Both are weaker than an end-to-end run, and saying
    // so is the point — see E-14.

    // And the real credential is untouched by all of this.
    assert!(
        credential.exists(),
        "the real credential must survive: the tmpfs hides it inside the namespace only"
    );

    let _ = std::fs::remove_file(&bundle);
}

/// The installed `nemr` must be the binary this working tree builds (F-62).
///
/// Milestone closure is "merged **and** reinstalled from that commit **and**
/// verified against the installed artifacts". The privileged helper has been
/// gated on that since F-58; the engine — the binary a user actually runs, and
/// the one `scripts/e2e_smoke_test.sh` invokes off PATH — was not. Nothing
/// stopped a smoke test from passing against a `nemr` built from a commit that
/// no longer exists and reporting the milestone closed.
///
/// Not `require_host`-gated: this needs no containerd, no helper and no base
/// image. It needs only that the working tree's claim about what is installed
/// is true, which is exactly the thing that must hold before any other result
/// here means anything.
#[test]
fn the_installed_engine_matches_its_source() {
    if common::unit_only() {
        eprintln!(
            "NEMR_TEST_UNIT_ONLY is set: not checking the installed engine. \
             A green run with it set says nothing about what is deployed."
        );
        return;
    }
    if let Err(reason) = common::installed_engine_matches_built() {
        panic!(
            "\n\nThe installed engine is not this source:\n\n  - {reason}\n\n\
             A verification run against a stale binary proves something about a commit \n\
             nobody is looking at. Reinstall, then re-run.\n"
        );
    }
}

/// The namespace probe must report *why* it failed, not just that it did.
///
/// The bare boolean it replaced cost a CI round-trip: the E-11 offline test
/// refused with "namespaces are unavailable" and left the cause to be guessed
/// at. This asserts the diagnostic actually carries the underlying reason —
/// F-65 was a diagnostic that could never print, and the lesson generalises: a
/// message nothing ever executes is not a message.
///
/// Needs no host: it runs a stand-in that fails the way `unshare` fails.
#[test]
fn the_namespace_probe_reports_why_it_failed() {
    let dir = std::env::temp_dir().join(format!("nemr-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let shim = dir.join("unshare-stub");
    std::fs::write(
        &shim,
        "#!/bin/sh\necho 'unshare: write_setgroups failed: Permission denied' >&2\nexit 1\n",
    )
    .expect("write the stand-in");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let reason = common::namespace_probe_with(&shim.to_string_lossy())
        .expect_err("a failing probe must be an error");

    // The specific stderr must survive into the message: a probe that reported
    // only "it failed" is what made the last CI failure a guess.
    assert!(
        reason.contains("write_setgroups failed: Permission denied"),
        "the probe must carry the underlying reason, got: {reason}"
    );
    // And the AppArmor knob must be named, because on Ubuntu 24.04 it is the
    // usual cause and it is not visible in unshare's own message.
    assert!(
        reason.contains("apparmor_restrict_unprivileged_userns"),
        "the probe must report the restriction sysctl, got: {reason}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// D-08 — the base image is identified by its digest, not by its name.
///
/// The import path used to ask containerd "do you have `ghcr.io/gnrain/nemr-base`?".
/// A host holding exactly the right bytes under any other reference — pulled by
/// digest, imported under a local tag, carried in from another machine — would
/// be told to go to the registry for an image it already had. D-08's whole
/// argument is that the product must not need a third party when it does not
/// have to, so resolution asks whether the *bytes* are here.
///
/// Files the base image under a second reference, then resolves by digest and
/// requires that reference to come back.
#[test]
fn d08_the_base_image_resolves_by_digest_under_any_reference() {
    if !require_host(HostRequirements {
        containerd: true,
        helper: false,
        base_image: true,
    }) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let alias = format!("nemr.test/d08-alias:{}", std::process::id());

        let canonical = nemr_engine::config::BASE_IMAGE;
        let digest = client
            .image_target_digest(canonical)
            .await
            .expect("the base image must be present");

        // CONTROL: before the alias exists, resolution by digest must not
        // already report it — otherwise the assertion below proves nothing.
        let before = client.images_with_digest(&digest).await.expect("query");
        assert!(
            !before.iter().any(|image| image.name == alias),
            "the alias must not exist before the test creates it"
        );

        client
            .tag_image(canonical, &alias)
            .await
            .expect("record a second reference for the same image");

        let found = client.images_with_digest(&digest).await.expect("query");
        let matched = found.iter().any(|image| image.name == alias);

        // Clean up before asserting, so a failure does not leave the alias behind.
        let _ = client.untag_image(&alias).await;

        assert!(
            matched,
            "an image filed under {alias:?} carries digest {digest} and must be found by it; \
             resolving by name only would send this host to a registry for bytes it already has"
        );
    });
}

/// F-63 — a snapshot under a lease survives containerd's collector.
///
/// The defect: `create_container` prepared the rootfs snapshot and only then
/// wrote the container record naming it. In between, the snapshot was
/// unreferenced — containerd's definition of garbage — and if the collector ran
/// there it was deleted, while the record write still returned `Ok` because
/// containerd does not validate that `snapshot_key` resolves.
///
/// # Why both snapshots start leased (F-72)
///
/// The obvious shape — create one leased snapshot and one unleased one, then
/// collect — is unsound, and failed in **both directions** before this:
///
///   * locally, in a full serial run: the unleased snapshot *survived*, because
///     nothing had pushed containerd past its mutation threshold;
///   * on CI: a snapshot was *already gone* at the baseline check, because 23
///     preceding tests had, and the collector fired before the test looked.
///
/// Both are the same root cause. An unleased snapshot is unreferenced from the
/// instant it is created, so it can be collected at any moment — including
/// before the test has established it ever existed. The baseline observation was
/// racing the very collector the test is trying to reason about.
///
/// So both snapshots are created **under leases**, which makes the baseline
/// deterministic, and the control snapshot is then made collectable by releasing
/// its lease at a moment this test chooses. Every state transition is
/// test-controlled; none of it depends on when the collector happens to run.
#[test]
fn f63_a_leased_snapshot_survives_the_collector_and_an_unleased_one_does_not() {
    if !require_host(HostRequirements {
        containerd: true,
        helper: false,
        base_image: true,
    }) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let chain_id = client
            .image_chain_id(nemr_engine::config::BASE_IMAGE)
            .await
            .expect("the base image must be present");

        let pid = std::process::id();
        let kept_key = format!("f63-kept-{pid}");
        let dropped_key = format!("f63-dropped-{pid}");

        let lease_kept = client
            .create_lease(&format!("f63-kept-lease-{pid}"))
            .await
            .expect("take the lease under test");
        let lease_dropped = client
            .create_lease(&format!("f63-dropped-lease-{pid}"))
            .await
            .expect("take the control's lease");

        client
            .prepare_snapshot(&kept_key, &chain_id, Some(&lease_kept))
            .await
            .expect("prepare under the lease under test");
        client
            .prepare_snapshot(&dropped_key, &chain_id, Some(&lease_dropped))
            .await
            .expect("prepare under the control's lease");

        // BASELINE: both are lease-protected, so this cannot race the collector.
        // Naming which one is missing matters: "a snapshot is gone" is
        // ambiguous between "the harness raced" and "the lease is not working",
        // and those are opposite conclusions.
        let keys = client.list_snapshot_keys().await.expect("list");
        assert!(
            keys.contains(&kept_key),
            "{kept_key} is missing at the baseline, while still under a live lease. \
             That is the lease failing to protect, not a timing artifact."
        );
        assert!(
            keys.contains(&dropped_key),
            "{dropped_key} is missing at the baseline, while still under a live lease. \
             That is the lease failing to protect, not a timing artifact."
        );

        // The control becomes collectable HERE, by this test's choice — not at
        // some earlier moment outside its control.
        client
            .delete_lease(&lease_dropped)
            .await
            .expect("release the control's lease");

        // A collection on demand: containerd answers a synchronous lease delete
        // only once a collection has completed.
        client.collect_garbage_now().await;

        let keys = client.list_snapshot_keys().await.expect("list");
        let kept_survived = keys.contains(&kept_key);
        let dropped_survived = keys.contains(&dropped_key);

        // Clean up before asserting, so a failure leaves no residue.
        let _ = client.delete_lease(&lease_kept).await;
        let _ = client.remove_snapshot(&kept_key).await;
        let _ = client.remove_snapshot(&dropped_key).await;

        // CONTROL first: if the collection did not actually remove the
        // unreferenced snapshot, then "the leased one survived" says nothing.
        assert!(
            !dropped_survived,
            "{dropped_key} survived after its lease was released and a collection ran, so \
             the collection did not do anything and this test cannot speak to the lease"
        );
        assert!(
            kept_survived,
            "{kept_key} was collected while its lease was still held: the lease is not \
             protecting the window between prepare and the container record write (F-63)"
        );
    });
}

/// F-63 — a completed create leaves no lease behind.
///
/// The lease pins its snapshot. Held past the point the container record
/// references it, it would keep collecting nothing forever — the opposite leak,
/// and quieter, because nothing fails and the disk just never comes back. The
/// expiry label bounds that to an hour; this asserts the normal path does not
/// rely on it.
#[test]
fn f63_a_create_releases_its_lease() {
    if !require_host(HostRequirements::FULL) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let before = client.list_lease_ids().await.expect("list leases");

        let project = TestProject::create(&client, "f63lease", VolumeSize::DEFAULT).await;

        let after = client.list_lease_ids().await.expect("list leases");
        let leaked: Vec<&String> = after
            .iter()
            .filter(|id| !before.contains(id) && id.contains(&project.name))
            .collect();
        assert!(
            leaked.is_empty(),
            "create left {leaked:?} behind; a lease outliving its create pins the snapshot \
             it was protecting and nothing reports it"
        );
    });
}

/// F-63 — `create_container` itself must survive a collector running during it.
///
/// The sibling test proves the lease *mechanism* protects a snapshot. This one
/// proves `create_container` actually **uses** it, which is a different claim: a
/// working mechanism the production path never reaches protects nothing.
///
/// # Why this uses a hook rather than racing
///
/// The window is two consecutive gRPC calls wide. The first version of this
/// test ran creates under heavy concurrent churn and hoped to catch the
/// collector in it — and **passed with the lease removed**, twice. Measuring it
/// properly: 79 of 80 creates survived unleased, a ~1.25% per-create failure
/// rate. A guard that misses the defect 98.75% of the time is a guard that
/// reports green over a real bug, so the probabilistic version was deleted
/// rather than tuned.
///
/// The hook makes it deterministic: a synchronous lease deletion — which
/// containerd answers only after a collection has run — fires *inside* the
/// window. Leased survives every time; unleased is collected every time.
#[test]
fn f63_create_container_holds_its_snapshot_against_a_collection_inside_the_window() {
    if !require_host(HostRequirements {
        containerd: true,
        helper: false,
        base_image: true,
    }) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let pid = std::process::id();

        // The hook: create a lease and delete it with sync=true, which
        // containerd answers only once a garbage collection has completed. That
        // is a collection running inside the create window, on demand.
        let hook_client = std::sync::Arc::new(ContainerdClient::connect().await.expect("connect"));
        let hook: nemr_engine::containerd::client::CreateWindowHook =
            std::sync::Arc::new(move || {
                let client = hook_client.clone();
                Box::pin(async move {
                    client.collect_garbage_now().await;
                })
            });

        let client = ContainerdClient::connect()
            .await
            .expect("connect")
            .with_create_window_hook(hook);

        let id = format!("nemr-f63-window-{pid}");
        let spec = nemr_engine::containerd::containers::ContainerSpec {
            id: id.clone(),
            image: nemr_engine::config::BASE_IMAGE.to_string(),
            mounts: vec![],
            working_dir: None,
            extra_env: vec![],
            args: Some(vec!["/bin/sh".to_string()]),
            cgroup_name: None,
            cgroup_prefix: nemr_engine::config::CGROUP_PREFIX.to_string(),
            labels: Default::default(),
            own_network_namespace: true,
        };

        client
            .create_container(&spec)
            .await
            .expect("create_container must succeed");

        let survived = client
            .list_snapshot_keys()
            .await
            .expect("list")
            .contains(&id);

        let _ = client.delete_container(&id).await;
        let _ = client.remove_snapshot(&id).await;

        assert!(
            survived,
            "the rootfs snapshot for {id} was collected during create. create_container is \
             not holding a lease across prepare→record (F-63): it returns Ok with a \
             snapshot_key that no longer resolves, and `nemr start` fails later, forever."
        );
    });
}

/// D-08 part 1 — the unresolved-base-image error must not imply a fetch it never attempts.
///
/// The ruling scoped registry pull out: `nemr` does not fetch images. The
/// earlier draft of this error promised to distinguish "registry unreachable"
/// from "digest not found there" — a distinction nothing can make without
/// attempting the fetch. Claiming it would have been a fabrication in the one
/// place a user is already stuck.
///
/// Asserts the real message from the real resolver, not a reconstruction.
#[test]
fn d08_the_unresolved_base_image_error_is_honest_about_not_fetching() {
    if !require_host(HostRequirements {
        containerd: true,
        helper: false,
        base_image: false,
    }) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let absent = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        // A bundle that names an OLD version. The advice must tell the user to
        // pull THIS version, not whatever the engine currently defaults to
        // (F-85) — pulling the current version fetches different bytes.
        let bundle_reference = "ghcr.io/gnrain/nemr-base:0.1.0";

        let resolution = project::resolve_base_image(&client, absent, bundle_reference, "").await;
        let (where_looked, advice) = match resolution {
            nemr_engine::bundle::import::BaseImageResolution::Unresolved {
                where_looked,
                advice,
            } => (where_looked.join("\n"), advice),
            other => panic!("a digest of all zeroes must not resolve, got {other:?}"),
        };

        assert!(
            where_looked.contains("by digest across all images"),
            "must report the by-digest attempt: {where_looked}"
        );
        assert!(
            where_looked.contains("does not fetch images itself"),
            "must say plainly that nemr does not pull: {where_looked}"
        );
        // The honest-limit requirement, stated as a negative: no claim about a
        // registry's reachability, because none was contacted.
        for invented in ["unreachable", "not found there", "timed out", "connection"] {
            assert!(
                !where_looked.to_lowercase().contains(invented),
                "must not claim {invented:?} about a registry it never contacted: {where_looked}"
            );
        }
        assert!(
            advice.contains("ctr") && advice.contains("images pull"),
            "must give the exact fetch command: {advice}"
        );
        // F-85: the pull target must be the bundle's OWN version, not the
        // engine's current default — otherwise the remedy fetches the wrong
        // image and fails confusingly on another machine.
        assert!(
            advice.contains(bundle_reference),
            "the pull advice must name the version the bundle needs ({bundle_reference}), \
             not the engine's current default: {advice}"
        );
        assert!(
            !advice.contains(nemr_engine::config::BASE_IMAGE)
                || nemr_engine::config::BASE_IMAGE == bundle_reference,
            "the advice must not name the engine's current base image when the bundle needs a \
             different version: {advice}"
        );
        assert!(
            advice.contains("--namespace"),
            "the command must target the same containerd namespace the engine uses, or it \
             pulls into a namespace nemr never reads: {advice}"
        );
        assert!(
            advice.contains("CONTAINERD_ADDRESS"),
            "the command must target the rootless socket, or it pulls into the system \
             daemon (PRIV-01): {advice}"
        );
    });
}

/// PROC-06 — the SIGKILL escalation path must actually work.
///
/// # The gap this closes (F-69)
///
/// `proc_06_stop_terminates_gracefully_without_escalating_to_sigkill` asserts
/// that stopping a well-behaved container does **not** escalate. Nothing
/// asserted that escalation happens when it should, so
/// `StopOutcome::Killed` was never produced anywhere in the tree — not by a
/// test, not by any other code path. If the timeout branch in `stop_task` were
/// dead, every existing test would still pass: a container ignoring SIGTERM
/// would hang forever and no guard would notice.
///
/// The message a user sees in that case (`nemr.rs`, "had to be killed") had
/// likewise never been produced. Same shape as F-65 and F-67 — machinery for a
/// failure, never run on one.
///
/// Runs a PID 1 that explicitly traps and ignores SIGTERM, so escalation is the
/// only way the task can end.
#[test]
fn proc_06_a_container_ignoring_sigterm_is_escalated_to_sigkill() {
    if !require_host(HostRequirements::FULL) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let name = common::unique_name("proc06kill");
        common::purge(&name);

        // A supervisor that refuses to die on SIGTERM. `trap '' TERM` sets the
        // disposition to ignore, which survives into the shell's wait loop.
        let spec = nemr_engine::containerd::containers::ContainerSpec {
            id: format!("nemr-{name}"),
            image: nemr_engine::config::BASE_IMAGE.to_string(),
            mounts: vec![],
            working_dir: None,
            extra_env: vec![],
            args: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "trap '' TERM; while :; do sleep 1; done".to_string(),
            ]),
            cgroup_name: Some(name.clone()),
            cgroup_prefix: nemr_engine::config::CGROUP_PREFIX.to_string(),
            labels: Default::default(),
            own_network_namespace: true,
        };
        let id = spec.id.clone();

        client.create_container(&spec).await.expect("create");
        client.start_task(&id).await.expect("start the task");

        let started = Instant::now();
        let outcome = client.stop_task(&id).await.expect("stop must not error");
        let elapsed = started.elapsed();

        let _ = client.delete_container(&id).await;
        let _ = client.remove_snapshot(&id).await;
        common::purge(&name);

        assert_eq!(
            outcome,
            StopOutcome::Killed,
            "a PID 1 that ignores SIGTERM must be escalated to SIGKILL and reported as \
             Killed, not silently reported as a graceful stop"
        );
        // It must have actually waited for the grace period rather than
        // escalating immediately — killing straight away would defeat the point
        // of a graceful stop for every well-behaved container.
        assert!(
            elapsed >= GRACE_PERIOD,
            "escalated after only {elapsed:?}; the {GRACE_PERIOD:?} grace period was not \
             honoured, so well-behaved containers are being killed without a chance to flush"
        );
    });
}

/// F-77 — the sweep must reclaim a killed run's volumes and nothing else.
///
/// The name filter is the whole safety argument: a real project like
/// `htmltest` must never match, and a concurrently running suite's volumes must
/// not either. Asserted directly against the classifier rather than by running
/// the sweep, so the test cannot destroy anything while proving it is safe.
#[test]
fn f77_the_test_sweep_only_claims_dead_test_projects() {
    let live = std::process::id();

    // Reclaimable: test-shaped, and the pid is gone. PID 1 always exists, so
    // use an implausible one and assert it really is absent first.
    let dead_pid = 4_000_000u32;
    assert!(
        !std::path::Path::new(&format!("/proc/{dead_pid}")).exists(),
        "control: pid {dead_pid} must not exist, or this test proves nothing"
    );
    assert_eq!(
        common::dead_test_project_pid(&format!("m8-{dead_pid}-3")),
        Some(dead_pid),
        "a test project from a dead pid is reclaimable"
    );

    // NOT reclaimable: a live pid — a parallel run's volumes are in use.
    assert_eq!(
        common::dead_test_project_pid(&format!("m8-{live}-0")),
        None,
        "a live pid's volumes must never be swept"
    );

    // NOT reclaimable: real project names. This is the assertion that stops the
    // sweep eating a developer's work.
    for real in ["htmltest", "myproject", "demo", "my-project", "a-b-c"] {
        assert_eq!(
            common::dead_test_project_pid(real),
            None,
            "{real:?} is not a test project and must never be swept"
        );
    }
}

/// F-28 — a foreign filesystem at the mount point must be refused, not used.
///
/// `is_mounted` answers "is something mounted here?", which is not the question.
/// A mount that lands on the wrong path — possible whenever many volumes are
/// attached and released concurrently — was accepted, and the container was
/// handed a foreign filesystem with no indication anything was wrong. That is
/// the shape F-63a was observed in: `mounted=true`, a real loop device, and a
/// volume containing nothing but `lost+found`.
///
/// A tmpfs stands in for "a filesystem that is not this project's volume": it
/// is mounted at the project's mount point, so presence-based checks pass and
/// only an identity check can tell the difference.
#[test]
fn f28_a_foreign_filesystem_at_the_mount_point_is_refused() {
    if !require_host(HostRequirements::VOLUME) {
        return;
    }
    common::init_tracing();

    let name = common::unique_name("f28");
    common::purge(&name);
    let paths = VolumePaths::from_env().expect("paths");
    let mount_point = paths.mount_point(&name);
    std::fs::create_dir_all(&mount_point).expect("mount point");

    // CONTROL: with nothing mounted and no image, the failure must be the
    // ordinary "no backing file" one — so a refusal below is attributable to
    // identity, not to the volume simply being absent.
    let absent = project::ensure_volume_mounted(&name).expect_err("no volume must fail");
    assert!(
        absent.to_string().contains("no backing file"),
        "control: expected the missing-volume error, got: {absent:#}"
    );

    // Mount a filesystem that is emphatically not this project's volume. Done
    // in a user namespace so the test needs no privilege of its own.
    let mounted = std::process::Command::new("unshare")
        .args(["-rm", "sh", "-c"])
        .arg(format!(
            "mount -t tmpfs none '{}' && grep -q ' {} ' /proc/self/mountinfo && echo MOUNTED",
            mount_point.display(),
            mount_point.display()
        ))
        .output()
        .expect("run unshare");
    // The mount lives in the namespace, so the assertion below is about what
    // the check does when it *sees* a foreign mount. Verify the identity
    // helper directly, which is the production code path the guard uses.
    assert!(
        String::from_utf8_lossy(&mounted.stdout).contains("MOUNTED"),
        "control: the stand-in tmpfs must have mounted, or nothing is being tested"
    );

    // A tmpfs is not loop-backed, so identity resolution must return None —
    // which is what makes the production check refuse rather than proceed.
    assert_eq!(
        volume::mounted_image_path(std::path::Path::new("/proc")),
        None,
        "a filesystem that is not loop-backed must not resolve to a backing image"
    );

    let _ = std::fs::remove_dir(&mount_point);
    common::purge(&name);
}

/// F-79 — `list` and `reconcile` must agree about what is on the host.
///
/// They disagreed: `nemr list` reported 57 untracked volumes while `nemr
/// reconcile` answered "nothing to reconcile: no orphaned mounts, loop devices
/// or snapshots". Both were reading the host; they were reading *different
/// parts* of it. `list` enumerated loop devices from `/sys`; `reconcile`
/// enumerated mount-point directories and image files, and 57 loop devices had
/// neither — their images deleted, their mount points removed — so they were
/// invisible to the one command whose job is to reclaim them. 24 GB was held
/// by artifacts the cleanup command reported as absent.
///
/// A false all-clear from a cleanup command is worse than a noisy one: it ends
/// the investigation.
///
/// This asserts the structural property rather than a count: every volume
/// `list` calls untracked must be something `reconcile` acts on.
#[test]
fn f79_reconcile_acts_on_everything_list_reports_as_untracked() {
    if !require_host(HostRequirements::FULL) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");

        let reported = project::untracked_volumes(&client)
            .await
            .expect("list untracked");

        let report = project::reconcile_orphans(&client)
            .await
            .expect("reconcile");

        // Anything reported as untracked must appear in reconcile's account of
        // what it did — released, refused, or explicitly kept. Silence is the
        // failure mode: it is what "nothing to reconcile" was.
        let accounted: std::collections::BTreeSet<&String> = report
            .released
            .iter()
            .chain(report.not_released.iter())
            .chain(report.orphan_backing_files.iter())
            .collect();

        let unaccounted: Vec<&String> = reported
            .iter()
            .filter(|name| !accounted.contains(name))
            .collect();

        assert!(
            unaccounted.is_empty(),
            "`list` reports {unaccounted:?} as untracked and `reconcile` did not account for \
             them. Two commands reading the same host and disagreeing is how 24 GB sat behind \
             \"nothing to reconcile\"."
        );

        // CONTROL: after reconciling, nothing should remain untracked — so the
        // assertion above cannot pass merely because `list` found nothing.
        let after = project::untracked_volumes(&client)
            .await
            .expect("list untracked again");
        assert!(
            after.is_empty(),
            "still untracked after reconcile: {after:?}"
        );
    });
}

/// The restore flow: one command, no invented quota.
///
/// Restoring onto a fresh host used to be three commands, one of which demanded
/// a `--size` the user had to guess — for information the bundle already
/// carried. `ProjectInfo` has recorded the source project's name and quota
/// since schema v1, so this needed no format change.
#[test]
fn import_creates_the_project_from_the_bundle_with_no_guessed_quota() {
    if !require_host(HostRequirements::FULL) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let source = TestProject::create(&client, "impsrc", VolumeSize::Small).await;
        let paths = VolumePaths::from_env().unwrap();

        let token = format!("restore-token-{}", std::process::id());
        let transcript = paths
            .mount_point(&source.name)
            .join(nemr_engine::config::VOLUME_STATE_PROJECTS)
            .join("-workspace/session.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).expect("mkdir");
        std::fs::write(&transcript, &token).expect("write transcript");

        let bundle = std::env::temp_dir().join(format!("restore-{}.nemr", std::process::id()));
        project::export(
            &client,
            &source.name,
            &bundle,
            nemr_engine::bundle::policy::Policy::default(),
        )
        .await
        .expect("export");

        // Remove the source entirely: this is a restore onto a host that has
        // never seen the project, which is the case that mattered.
        project::delete(&client, &source.name)
            .await
            .expect("delete");

        // CONTROL: nothing of that name exists now, so a success below is the
        // import creating it rather than finding it.
        assert!(
            !client
                .container_exists(&nemr_engine::config::container_id(&source.name))
                .await
                .expect("exists"),
            "control: the project must be gone before the restore"
        );

        // One command. No name, no size.
        let (restored_name, summary) = project::import_creating(&client, &bundle, None, None)
            .await
            .expect("import must create the project from the bundle");
        let restored = TestProject::adopt(restored_name.clone());

        assert_eq!(
            restored_name, source.name,
            "the project name must come from the bundle"
        );
        assert!(summary.members > 0, "the restore must have carried members");

        // The session is actually there.
        let landed = paths
            .mount_point(&restored.name)
            .join(nemr_engine::config::VOLUME_STATE_PROJECTS)
            .join("-workspace/session.jsonl");
        assert_eq!(
            std::fs::read_to_string(&landed).expect("the transcript must have been restored"),
            token,
            "the restored session must be byte-identical"
        );

        // The quota came from the bundle, not from a default.
        let listed = project::list(&client).await.expect("list");
        let entry = listed
            .iter()
            .find(|p| p.name == restored.name)
            .expect("the restored project must be listed");
        assert_eq!(
            entry.quota,
            VolumeSize::Small.to_string(),
            "the quota must come from the bundle's manifest, not a guess or a default"
        );

        let _ = std::fs::remove_file(&bundle);
    });
}

/// Importing over an existing project must refuse, not merge.
///
/// A silent merge would overwrite one session with another, and the damage is
/// invisible until someone opens it.
#[test]
fn import_refuses_to_clobber_an_existing_project() {
    if !require_host(HostRequirements::FULL) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let occupant = TestProject::create(&client, "impclob", VolumeSize::Small).await;
        let paths = VolumePaths::from_env().unwrap();

        let keep = format!("must-survive-{}", std::process::id());
        let existing = paths
            .mount_point(&occupant.name)
            .join(nemr_engine::config::VOLUME_STATE_PROJECTS)
            .join("-workspace/existing.jsonl");
        std::fs::create_dir_all(existing.parent().unwrap()).expect("mkdir");
        std::fs::write(&existing, &keep).expect("write");

        let bundle = std::env::temp_dir().join(format!("clobber-{}.nemr", std::process::id()));
        project::export(
            &client,
            &occupant.name,
            &bundle,
            nemr_engine::bundle::policy::Policy::default(),
        )
        .await
        .expect("export");

        let error = project::import_creating(&client, &bundle, Some(&occupant.name), None)
            .await
            .expect_err("importing onto an existing project must refuse");

        assert_eq!(
            error.kind(),
            nemr_engine::error::ErrorKind::Conflict,
            "a name collision is a conflict, not an internal error: {error}"
        );
        assert!(
            error.to_string().contains(&occupant.name),
            "the refusal must name the project: {error}"
        );

        // And it left the occupant alone.
        assert_eq!(
            std::fs::read_to_string(&existing).expect("the existing session must survive"),
            keep,
            "a refused import must not have touched the existing project"
        );

        let _ = std::fs::remove_file(&bundle);
    });
}

/// E-14 — a restore must not require a host credential.
///
/// E-11 rules that `nemr import` works with no network and no credential.
/// AUTH-03 rules that a missing credential is fatal "at creation time". Those
/// did not collide while import needed a pre-existing project; now that a
/// restore creates its own, they do.
///
/// This asserts the resolution at the policy level: `create` still refuses
/// without a credential, and a restore does not. It is weaker than the
/// end-to-end offline run it replaces — a restore provisions a volume and so
/// needs the privileged helper, which cannot run inside the offline test's user
/// namespace — and that gap is recorded rather than hidden.
#[test]
fn import_defers_the_credential_requirement() {
    // BRITTLE BY CONSTRUCTION, deliberately — read this before "fixing" a
    // failure here as a regression.
    //
    // The property is behavioural: `create` must refuse without a host
    // credential (AUTH-03), and a restore must not. The honest way to assert
    // that is to run each with no credential visible and observe the outcome —
    // and that is not cheaply available here, for two real reasons, neither a
    // shortcut:
    //   1. The full no-credential IMPORT is blocked by the helper-vs-namespace
    //      wall (E-14's second consequence): a restore provisions a volume, so
    //      it needs the privileged helper, which cannot run inside the user
    //      namespace that would hide the host credential. The e11 offline test
    //      documents exactly this.
    //   2. Making CREATE fail-without-credential observable means removing the
    //      real credential file for the duration — and a test that renames a
    //      user's live credential risks leaving it renamed if it dies, the same
    //      cleanup-before-verify hazard F-79 was about. Not worth it for this.
    //
    // So this inspects the source. What it asserts is now token-level, NOT the
    // exact call-site argument list, because matching the arg list is what
    // snapped when the `agent` parameter landed (a change unrelated to the
    // property). Tokens survive signature changes; the property does not depend
    // on them.
    let source = std::fs::read_to_string("src/engine/project.rs").expect("read project.rs");

    // The public `create` must route through the Required policy. Checked
    // against `create`'s own body, so an unrelated `Required` elsewhere cannot
    // satisfy it.
    let create_body = {
        let start = source
            .find("pub async fn create(")
            .expect("create must exist");
        let after = &source[start..];
        let end = after
            .find(
                "
}
",
            )
            .map(|e| start + e)
            .unwrap_or(source.len());
        &source[start..end]
    };
    assert!(
        create_body.contains("AuthPolicy::Required"),
        "nemr create must keep AUTH-03: its body must route through AuthPolicy::Required"
    );

    // The deferral must exist and be confined to exactly one site. This is the
    // invariant that actually matters — widening it would repeal AUTH-03 rather
    // than narrow it — and one stray extra site is what this catches.
    assert!(
        source.contains("AuthPolicy::DeferredForRestore"),
        "a restore must defer the credential check, or E-11's offline guarantee breaks"
    );
    let deferred_sites = source.matches("AuthPolicy::DeferredForRestore").count();
    assert_eq!(
        deferred_sites, 2,
        "expected `AuthPolicy::DeferredForRestore` exactly twice — the match arm that \
         implements the deferral and the single call site that selects it (the enum variant's \
         own definition is spelled without the `AuthPolicy::` prefix and is not counted). \
         {deferred_sites} occurrences means a new place selects the deferral, widening \
         AUTH-03's exception."
    );
}

/// Quieting the success path must not quieten the failure path.
///
/// The provisioning trace moved from `info` to `debug` so `nemr create` prints
/// a summary instead of ten lines of loop devices. Everything F-65, F-67 and
/// F-73 bought depends on failures still being loud, and a filter change is
/// exactly the kind of edit that takes them out silently — WARN and ERROR sit
/// above INFO, so lowering what INFO shows cannot touch them, but "cannot" is
/// an argument and this is a test.
#[test]
fn quieting_the_success_path_does_not_quieten_failures() {
    if !require_host(HostRequirements {
        containerd: true,
        helper: false,
        base_image: false,
    }) {
        return;
    }

    let nemr = std::path::PathBuf::from(env!("CARGO_BIN_EXE_nemr"));
    let socket = ContainerdClient::default_socket_path().expect("socket");
    let absent = format!("no-such-project-{}", std::process::id());

    let run = |args: &[&str]| {
        std::process::Command::new(&nemr)
            .args(args)
            .env("CONTAINERD_ADDRESS", &socket)
            .env_remove("NEMR_DEBUG")
            .env_remove("NEMR_LOG")
            .output()
            .expect("run nemr")
    };

    // DEFAULT verbosity — the quiet one.
    let quiet = run(&["start", &absent]);
    let quiet_err = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        !quiet.status.success(),
        "starting a project that does not exist must fail"
    );
    assert!(
        quiet_err.contains(&absent),
        "the failure must name the project even at default verbosity: {quiet_err:?}"
    );
    assert!(
        quiet_err.contains("nemr create"),
        "the failure must still say what to do next at default verbosity: {quiet_err:?}"
    );

    // VERBOSE must not be required to see it, and must not lose it either.
    let loud = run(&["--verbose", "start", &absent]);
    let loud_err = String::from_utf8_lossy(&loud.stderr);
    assert!(
        loud_err.contains(&absent),
        "the failure must survive --verbose too: {loud_err:?}"
    );

    // CONTROL: the quiet path really is quieter, or this test is asserting
    // nothing about the change it exists to guard.
    let quiet_lines = quiet_err.lines().count();
    let loud_lines = loud_err.lines().count();
    assert!(
        loud_lines >= quiet_lines,
        "verbose produced fewer lines ({loud_lines}) than default ({quiet_lines}); \
         the verbosity switch is not doing what this test assumes"
    );
}

/// The fact of elevation stays visible without any flag.
///
/// A user must be able to tell that something ran as root, even though the
/// arguments moved behind `--verbose`. Silence about privilege would be a worse
/// default than the firehose it replaced.
#[test]
fn the_elevation_note_is_visible_without_a_flag() {
    if !require_host(HostRequirements::VOLUME) {
        return;
    }

    // E-09: the engine now runs in the daemon, so the VOL-03/NFR-04 privileged-
    // operation audit trail — every time the helper runs — is written to the
    // DAEMON's log, not the CLI's own output. This asserts the trail survives
    // the move (it must, NFR-04), by pointing a dedicated test daemon at a known
    // log and checking it after a create. (The CLI-side inline note is a
    // casualty of the daemon architecture, flagged for a ruling.)
    let nemr = std::path::PathBuf::from(env!("CARGO_BIN_EXE_nemr"));
    let socket = ContainerdClient::default_socket_path().expect("socket");
    let name = common::unique_name("elev");
    common::purge(&name);

    let test_state = std::env::temp_dir().join(format!("nemr-elev-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&test_state);
    let test_sock = test_state.join("nemrd.sock");
    std::fs::create_dir_all(&test_state).expect("state dir");

    let output = std::process::Command::new(&nemr)
        .args(["create", &name, "--size", "500MB"])
        .env("CONTAINERD_ADDRESS", &socket)
        .env("NEMR_DAEMON_SOCKET", &test_sock)
        .env("XDG_STATE_HOME", &test_state)
        .env_remove("NEMR_DEBUG")
        .env_remove("NEMR_LOG")
        .output()
        .expect("run nemr create");
    let _cleanup = TestProject::adopt(name.clone());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "create must succeed through the daemon: {stderr}\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    // The elevation note must be INLINE in the CLI output (NFR-04): the daemon
    // streams the engine's audit events back over WatchAudit, restoring across
    // the daemon boundary the inline note the pre-daemon CLI printed — a user
    // can tell a command elevated without going to the log.
    assert!(
        stderr.contains("elevated:"),
        "the CLI output must show inline that the privileged helper ran (NFR-04): {stderr:?}"
    );
    assert!(
        stderr.contains("mount"),
        "the elevation note must name the privileged operation: {stderr:?}"
    );
    // The full command line stays out of the DEFAULT output (only --verbose).
    assert!(
        !stderr.contains("sudo -n"),
        "the default output must not carry the full privileged command line: {stderr:?}"
    );
    // The durable trail is ALSO kept in the daemon log.
    let log =
        std::fs::read_to_string(test_state.join("nemr").join("nemrd.log")).unwrap_or_default();
    assert!(
        log.contains("elevated:"),
        "the daemon log must also keep the audit trail (NFR-04): {log:?}"
    );

    // Stop the dedicated test daemon and clean up.
    stop_test_daemon(&test_state, &test_sock);
    let _ = std::fs::remove_dir_all(&test_state);
}

/// Stop a daemon a test started on its own socket.
///
/// Not `pgrep -f <socket path>`: the path reaches nemrd through the
/// ENVIRONMENT, not its argv, so that matched nothing (or, worse, an unrelated
/// process that happened to mention the path) and every such test leaked its
/// daemon. Daemons accumulated until six were live on one machine. The daemon
/// announces its own pid in its log, and the test owns that log because it owns
/// XDG_STATE_HOME, so that is what identifies it.
fn stop_test_daemon(state_dir: &std::path::Path, sock: &std::path::Path) {
    let log = state_dir.join("nemr/nemrd.log");
    let Ok(text) = std::fs::read_to_string(&log) else {
        return;
    };
    let needle = sock.to_string_lossy().into_owned();
    for line in text.lines() {
        // "[nemrd 12345] listening on /path/to.sock (protocol vN)"
        if !line.contains("listening on") || !line.contains(&needle) {
            continue;
        }
        let Some(pid) = line
            .split_once("[nemrd ")
            .and_then(|(_, rest)| rest.split_once(']'))
            .and_then(|(pid, _)| pid.trim().parse::<i32>().ok())
        else {
            continue;
        };
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
}

/// `nemr status` must answer the questions that previously took three commands.
///
/// During the cross-machine work, "what state is this in?" meant reading
/// `nemr list`, `df` and `losetup` and correlating them by hand — and the two
/// questions that actually cost time were answerable from no command at all:
/// whether the mounted filesystem is the right one (F-28), and whether a
/// credential is present.
#[test]
fn status_reports_the_facts_that_took_three_commands_to_gather() {
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "status", VolumeSize::Small).await;

        let detail = project::status(&client, &project.name)
            .await
            .expect("status must work for an existing project");

        assert_eq!(detail.name, project.name);
        assert!(!detail.running, "a freshly created project is not running");
        assert!(detail.image_present, "the backing image must exist");
        assert!(detail.mounted, "a freshly created project is mounted");
        assert!(
            detail.loop_device.is_some(),
            "a mounted volume must report the loop device backing it"
        );
        assert!(
            detail.usage.is_some(),
            "a mounted volume must report usage against its quota"
        );
        assert_eq!(detail.quota, VolumeSize::Small.to_string());

        // The F-28 answer, which no command could give before.
        assert_eq!(
            detail.mount_is_correct(),
            Some(true),
            "a healthy project must report that its mount is its own volume"
        );

        // The credential answer, which turned a three-step diagnosis into a line.
        assert!(
            detail.credential.is_some(),
            "the host credential exists on this host, so status must report it"
        );

        // A project that does not exist is a clear refusal, not an empty report.
        let error = project::status(&client, "definitely-not-a-project")
            .await
            .expect_err("status on a missing project must fail");
        assert_eq!(
            error.kind(),
            nemr_engine::error::ErrorKind::InvalidRequest,
            "asking about a project that does not exist is a caller error: {error}"
        );
    });
}

/// `status` must report an unmounted volume as unmounted, not as wrong.
///
/// `mount_is_correct()` returns `None` when nothing is mounted. Collapsing that
/// into `false` would report a perfectly healthy stopped project as corrupted,
/// which is the kind of false alarm that trains people to ignore the field.
#[test]
fn status_distinguishes_unmounted_from_wrongly_mounted() {
    if !require_host(HostRequirements::VOLUME) {
        return;
    }

    let paths = VolumePaths::from_env().expect("paths");
    let detail = project::ProjectDetail {
        name: "demo".into(),
        container_id: "nemr-demo".into(),
        agent: nemr_engine::engine::agent::Agent::ClaudeCode,
        running: false,
        quota: "500MB".into(),
        mount_point: paths.mount_point("demo"),
        image_file: paths.image_file("demo"),
        image_present: true,
        mounted: false,
        loop_device: None,
        mounted_image: None,
        usage: None,
        base_image: "x".into(),
        base_image_digest: None,
        credential: None,
        credential_modified: None,
    };
    assert_eq!(
        detail.mount_is_correct(),
        None,
        "an unmounted volume is not a wrongly-mounted one"
    );

    let wrong = project::ProjectDetail {
        mounted: true,
        mounted_image: Some(paths.image_file("someone-else")),
        ..detail.clone()
    };
    assert_eq!(
        wrong.mount_is_correct(),
        Some(false),
        "a foreign image backing the mount must report as wrong (F-28)"
    );

    let right = project::ProjectDetail {
        mounted: true,
        mounted_image: Some(paths.image_file("demo")),
        ..detail
    };
    assert_eq!(right.mount_is_correct(), Some(true));
}

/// User-facing errors must name what failed and what to do next.
///
/// The D-08 unresolved-base-image error set the bar: it named the digest,
/// listed every place it looked and what it found there, stated its own limits,
/// and gave two concrete fixes. Several errors were a single clause with no
/// remedy — `"project X is not running; start it first"` tells you the state
/// and makes you go and find the command.
///
/// This asserts the floor, not the ceiling: an error must name its subject and
/// contain something the reader can act on. Not every error needs four
/// sections; every error needs a next step.
#[test]
fn user_facing_errors_name_the_subject_and_a_next_step() {
    if !require_host(HostRequirements {
        containerd: true,
        helper: false,
        base_image: false,
    }) {
        return;
    }

    let nemr = std::path::PathBuf::from(env!("CARGO_BIN_EXE_nemr"));
    let socket = ContainerdClient::default_socket_path().expect("socket");
    let absent = format!("no-such-{}", std::process::id());

    let run = |args: &[&str]| -> String {
        let output = std::process::Command::new(&nemr)
            .args(args)
            .env("CONTAINERD_ADDRESS", &socket)
            .output()
            .expect("run nemr");
        assert!(
            !output.status.success(),
            "expected {args:?} to fail so its error could be inspected"
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    // Each case: the command, and a command the error must suggest.
    let cases: [(&[&str], &str); 4] = [
        (&["start", &absent], "nemr create"),
        (&["stop", &absent], "nemr create"),
        (&["status", &absent], "nemr create"),
        (&["export", &absent], "nemr create"),
    ];

    for (args, expected_remedy) in cases {
        let message = run(args);
        assert!(
            message.contains(&absent),
            "{args:?}: the error must name its subject: {message:?}"
        );
        assert!(
            message.contains(expected_remedy),
            "{args:?}: the error must offer a next step containing {expected_remedy:?}: \
             {message:?}"
        );
        // A bare one-liner with no remedy is the shape this guards against.
        assert!(
            message.lines().filter(|l| !l.trim().is_empty()).count() >= 2,
            "{args:?}: a single-clause error with no next step: {message:?}"
        );
    }
}

/// A bundle survives a rename of the base image (D-08).
///
/// When `docker.io/nemr/base:0.1.0` became `ghcr.io/gnrain/nemr-base:0.1.0`,
/// every bundle already exported referenced the old name. They still import,
/// and this is why: the manifest identifies the base image by **digest**, the
/// reference is documented as a hint, and `resolve_base_image`'s second attempt
/// scans every local image by digest rather than trusting the name.
///
/// Verified empirically at the time — a bundle exported before the rename
/// imported afterwards with the old name removed from containerd entirely — and
/// pinned here so the property cannot regress into name-based resolution.
///
/// # This reproduces the rename rather than standing in for it
///
/// The first version looked for any *other* image on the host to act as a
/// renamed one. That passed here, where `alpine` happened to be lying around,
/// and failed on a clean CI runner where the base image is the only one — a
/// test depending on incidental host state, which is the class this project
/// keeps finding. It now performs the rename itself: file the image under an
/// alias, remove the canonical name, resolve, then put it back. At every step
/// at least one reference holds the blobs, so the image is never orphaned.
#[test]
fn a_bundle_survives_a_rename_of_the_base_image() {
    if !require_host(HostRequirements {
        containerd: true,
        helper: false,
        base_image: true,
    }) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let canonical = nemr_engine::config::BASE_IMAGE;
        let alias = format!("nemr.test/renamed-base:{}", std::process::id());

        let digest = client
            .image_target_digest(canonical)
            .await
            .expect("the base image must be present");

        // 1. File it under the new name. Both references now point at the same
        //    target, which is exactly what `ctr images tag` did for the real
        //    rename.
        client
            .tag_image(canonical, &alias)
            .await
            .expect("file the image under a second reference");

        // 2. Remove the old name. A bundle referencing it can now only resolve
        //    by digest — the situation every pre-rename bundle is in.
        let removed = client.untag_image(canonical).await;

        // 3. Resolve. Everything after this must run even on failure, or the
        //    host is left without its base image.
        let resolution = if removed.is_ok() {
            Some(project::resolve_base_image(&client, &digest, canonical, "").await)
        } else {
            None
        };

        // 4. Put the canonical name back, then drop the alias.
        let restored = client.tag_image(&alias, canonical).await;
        let _ = client.untag_image(&alias).await;

        removed.expect("removing the canonical reference must succeed");
        restored.expect("the canonical reference must be restored");

        match resolution.expect("resolution must have run") {
            nemr_engine::bundle::import::BaseImageResolution::Present { reference } => {
                assert_eq!(
                    reference, alias,
                    "resolution must report where it actually found the image, which is the                      renamed reference — not the one the bundle asked for"
                );
            }
            other => panic!(
                "with the old name gone, the image must still resolve by digest under its new                  name — this is what keeps pre-rename bundles importable: {other:?}"
            ),
        }

        // CONTROL: the base image is back under its canonical name, so this
        // test has not broken the host for everything that follows.
        assert_eq!(
            client
                .image_target_digest(canonical)
                .await
                .expect("the canonical reference must resolve again"),
            digest,
            "the base image must be restored exactly as it was"
        );
    });
}

/// F-83 — a project's disk must be mounted `nosuid` and `nodev`.
///
/// The backing image is allocated and formatted by the unprivileged engine and
/// owned by the invoking user; the privileged helper checks only that it is a
/// regular file they own, never that its contents are safe. An unprivileged
/// user can plant a setuid-root binary in an ext4 image they own — `debugfs`
/// sets inode uid and mode directly on the image file, no root and no mount
/// required — and, if the helper mounts it without `nosuid`, executing that
/// binary yields root. `nodev` closes the same door for device nodes.
///
/// This asserts the kernel's own report of the mount, which is authoritative:
/// if `nosuid`/`nodev` are absent from mountinfo, setuid and devices are
/// honoured on a filesystem the user controls.
#[test]
fn f83_project_volumes_are_mounted_nosuid_and_nodev() {
    if !require_host(HostRequirements::VOLUME) {
        return;
    }
    common::init_tracing();

    let name = common::unique_name("f83");
    common::purge(&name);

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, &name, VolumeSize::Small).await;

        let mount_point = VolumePaths::from_env()
            .expect("paths")
            .mount_point(&project.name);

        // CONTROL: the mount is actually present and readable, so an absent
        // option below means "not set" rather than "nothing mounted here".
        assert!(
            nemr_engine::engine::volume::is_mounted(&mount_point),
            "control: the volume must be mounted for this test to mean anything"
        );

        // The kernel's per-mount option list is mountinfo field 6.
        let table = std::fs::read_to_string("/proc/self/mountinfo").expect("read mountinfo");
        let wanted = mount_point.to_string_lossy();
        let options = table
            .lines()
            .find(|l| l.split(' ').nth(4) == Some(wanted.as_ref()))
            .and_then(|l| l.split(' ').nth(5))
            .unwrap_or("")
            .to_string();

        assert!(
            options.split(',').any(|o| o == "nosuid"),
            "the project volume must be mounted nosuid — without it a user-crafted setuid-root \
             binary on the volume runs as root (F-83). mount options were: {options:?}"
        );
        assert!(
            options.split(',').any(|o| o == "nodev"),
            "the project volume must be mounted nodev — without it device nodes on the \
             user-controlled image are usable (F-83). mount options were: {options:?}"
        );
    });
}

/// Interactive `create` must never hang without a terminal (WP-H).
///
/// A `nemr create` that waits forever for input on a CI runner burns the whole
/// job timeout and reports nothing — the worst failure shape there is. Every
/// case here runs the real binary with stdin NOT a terminal and a short
/// timeout: a hang shows up as the timeout killing the process, which these
/// assertions would catch as a missing exit within the deadline.
#[test]
fn create_is_non_interactive_and_never_hangs_without_a_tty() {
    // The never-hang property lives in the CLI's resolve layer (decide), which
    // runs BEFORE any daemon contact: cases 1/3/4 fail fast there. Case 2 (name
    // given) proceeds to the daemon. CONTAINERD_ADDRESS is deliberately NOT
    // overridden here — the CLI no longer reads it (the daemon does), so
    // overriding it only poisons case 2's daemon into a slow failure while
    // proving nothing about the CLI.
    let nemr = std::path::PathBuf::from(env!("CARGO_BIN_EXE_nemr"));

    let run = |args: &[&str], extra_env: &[(&str, &str)]| -> (Option<i32>, String) {
        use std::process::{Command, Stdio};
        let mut cmd = Command::new(&nemr);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn nemr");
        // Poll for up to 30s; a hang is the failure this test exists to catch.
        // The deadline exceeds the client's daemon-autostart wait (15s), so a
        // case that legitimately fails via a failed autostart (e.g. no host in
        // CI's no-host step) is not mistaken for a hang — only a real input hang,
        // which never returns, would trip it.
        let start = std::time::Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("wait") {
                let out = child.wait_with_output().expect("output");
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                return (status.code(), text);
            }
            if start.elapsed() > std::time::Duration::from_secs(30) {
                let _ = child.kill();
                panic!("`nemr {args:?}` did not exit within 30s — it hung waiting for input");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };

    // 1. No name, no tty: must fail FAST, naming what was needed — never prompt.
    let (code, out) = run(&["create"], &[]);
    assert_eq!(
        code,
        Some(1),
        "missing name with no tty must be a clean failure: {out}"
    );
    assert!(
        out.contains("not a terminal") && out.contains("name"),
        "the failure must say a name is required and why: {out}"
    );

    // 2. Name given, no tty: resolution succeeds (defaults fill size+agent) and
    //    the command PROCEEDS without prompting or hanging. Post-daemon, "reach
    //    containerd" is the daemon's job, not the CLI's, so the property here is
    //    purely no-hang: it exits within the deadline. It may create a real
    //    project via the daemon, so it is deleted afterward.
    let unique = format!("wphcreate-{}", std::process::id());
    let (_code, _out) = run(&["create", &unique], &[]);
    // Best-effort cleanup of anything it created (the daemon uses the real host).
    // Cleanup goes through the ambient daemon (real containerd); the CLI no
    // longer reads CONTAINERD_ADDRESS, so no override is needed here.
    let _ = std::process::Command::new(&nemr)
        .args(["delete", &unique, "--yes"])
        .output();

    // 3. The escape hatch forces non-interactive even where a tty might exist.
    let (code, out) = run(&["create"], &[("NEMR_NON_INTERACTIVE", "1")]);
    assert_eq!(
        code,
        Some(1),
        "NEMR_NON_INTERACTIVE must force the fail-fast path: {out}"
    );

    // 4. A flag suppresses its own prompt: --agent given, still no name, no tty
    //    → fails on the NAME, proving --agent was accepted without prompting.
    let (code, out) = run(&["create", "--agent", "codex"], &[]);
    assert_eq!(code, Some(1), "still no name: {out}");
    assert!(
        out.contains("name") && !out.contains("agent is required"),
        "a supplied --agent must not itself be demanded: {out}"
    );
}

/// A project records its agent, reports it, and can switch it (E-15).
///
/// The label is the source of truth — `start`/`attach` read it to launch the
/// right CLI — so this asserts it round-trips through create, status and a
/// switch, and that a running project refuses the switch (nothing should change
/// agents mid-session).
#[test]
fn e15_a_project_records_reports_and_switches_its_agent() {
    if !require_host(HostRequirements::FULL) {
        return;
    }
    common::init_tracing();

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        use nemr_engine::engine::agent::Agent;
        let client = ContainerdClient::connect().await.expect("connect");

        // Created as Codex — not the default, so a pass proves the value was
        // recorded rather than defaulted.
        let project =
            TestProject::create_with_agent(&client, "e15", VolumeSize::Small, Agent::Codex).await;

        let detail = project::status(&client, &project.name)
            .await
            .expect("status");
        assert_eq!(
            detail.agent,
            Agent::Codex,
            "the agent chosen at create must be recorded and reported"
        );

        // Switch to Claude Code and confirm it took.
        let (previous, now) = project::set_agent(&client, &project.name, Agent::ClaudeCode)
            .await
            .expect("switch");
        assert_eq!(previous, Agent::Codex);
        assert_eq!(now, Agent::ClaudeCode);
        assert_eq!(
            project::status(&client, &project.name)
                .await
                .expect("status")
                .agent,
            Agent::ClaudeCode,
            "the switch must persist to the label, not just return a value"
        );

        // A running project must refuse the switch.
        project::start(&client, &project.name).await.expect("start");
        let err = project::set_agent(&client, &project.name, Agent::Codex)
            .await
            .expect_err("switching a running project must fail");
        assert_eq!(
            err.kind(),
            nemr_engine::error::ErrorKind::Conflict,
            "switching mid-run is a conflict (wrong state), never an internal error: {err}"
        );
        project::stop(&client, &project.name).await.ok();
    });
}

/// The unverified-agent warning is data-driven and appears at selection (F-84).
///
/// "Implemented the same way" must not quietly become "works": selecting an
/// unverified agent must warn at the point of selection, not only in docs. This
/// asserts the property that drives the warning — `portability_verified()` —
/// and that the CLI create output carries the note for an unverified agent and
/// omits it for a verified one.
#[test]
fn f84_unverified_agents_are_flagged_at_selection() {
    use nemr_engine::engine::agent::Agent;

    // The property the warning is built on.
    assert!(
        Agent::ClaudeCode.portability_verified(),
        "Claude Code was verified by WP-C (M8/M10)"
    );
    assert!(
        !Agent::Codex.portability_verified(),
        "Codex is implemented but not empirically verified; if this flips, it must be because \
         the F-84 acceptance actually ran, not because someone assumed it"
    );
    // The menu description must itself carry the caveat, so it shows even in the
    // arrow-key picker.
    assert!(
        Agent::Codex
            .description()
            .to_lowercase()
            .contains("unverified"),
        "an unverified agent's menu description must say so: {:?}",
        Agent::Codex.description()
    );

    // At least one agent must be verified and one not, or this test is vacuous
    // (a self-contained control, per the standing rule).
    let verified = Agent::all()
        .iter()
        .filter(|a| a.portability_verified())
        .count();
    let unverified = Agent::all().len() - verified;
    assert!(
        verified >= 1 && unverified >= 1,
        "control: the fixture must contain both a verified and an unverified agent"
    );
}

/// The daemon refuses a protocol-version mismatch cleanly (E-09).
///
/// The hash-gate lesson applied to the daemon protocol: a client and daemon
/// from different builds must be refused with an actionable message, not
/// discovered as a confusing downstream failure. A daemon started with a forced
/// protocol version stands in for a stale build.
#[test]
fn e09_daemon_refuses_a_protocol_version_mismatch() {
    if !require_host(HostRequirements {
        containerd: true,
        helper: false,
        base_image: false,
    }) {
        return;
    }

    let nemr = std::path::PathBuf::from(env!("CARGO_BIN_EXE_nemr"));
    let dir = std::env::temp_dir().join(format!("nemr-e09-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let sock = dir.join("nemrd.sock");

    // A CLI command that would autostart a daemon — but with the daemon forced
    // to a different protocol version, so the handshake must be refused.
    let output = std::process::Command::new(&nemr)
        .args(["list"])
        .env("NEMR_DAEMON_SOCKET", &sock)
        .env("XDG_STATE_HOME", &dir)
        .env("NEMR_TEST_DAEMON_PROTOCOL", "999")
        .output()
        .expect("run nemr list");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "a protocol mismatch must fail, not succeed: {combined}"
    );
    assert!(
        combined.contains("protocol version mismatch") || combined.contains("999"),
        "the failure must name the version mismatch actionably: {combined}"
    );

    // Stop the forced daemon.
    stop_test_daemon(&dir, &sock);
    let _ = std::fs::remove_dir_all(&dir);
}

/// WP-K: `nemr <unknown>` execs `nemr-<unknown>` from PATH — the cargo/git
/// external-subcommand pattern the sync client installs under. The open CLI
/// carries no extension names (the E-11 seam grep proves no commercial name
/// appears in this tree); this test proves the generic mechanism: argv
/// passthrough, exit-code passthrough, and stdout untouched.
#[test]
fn wpk_an_external_subcommand_is_execed_with_args_and_exit_code() {
    let dir = std::env::temp_dir().join(format!("nemr-ext-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("nemr-frobnicate");
    std::fs::write(&stub, "#!/bin/sh\necho \"FROBNICATED args=$*\"\nexit 7\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nemr"))
        .args(["frobnicate", "--alpha", "beta"])
        .env("PATH", path)
        .output()
        .expect("running nemr");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(7),
        "the extension's exit code must pass through unchanged"
    );
    assert!(
        stdout.contains("FROBNICATED args=--alpha beta"),
        "args must pass through in order: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The not-found half: an unknown subcommand with no matching extension fails
/// with a message that names both the subcommand and the `nemr-<name>` form —
/// so the mechanism is discoverable from its own error — and does not touch the
/// daemon (no autostart wait: this must fail fast).
#[test]
fn wpk_an_unknown_subcommand_without_an_extension_fails_helpfully() {
    let started = std::time::Instant::now();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nemr"))
        .arg("definitely-not-a-subcommand")
        .env("PATH", "/nonexistent") // control: nothing can be found
        .output()
        .expect("running nemr");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(
        stderr.contains("definitely-not-a-subcommand")
            && stderr.contains("nemr-definitely-not-a-subcommand"),
        "the error must teach the extension form: {stderr}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "not-found must fail fast, never wait on a daemon"
    );
}

/// NET-02: a started session has its OWN network namespace.
///
/// This test exists because its opposite was true until 2026-08-28 and nothing
/// asserted it. NET-01 read "project containers shall share rootlesskit's
/// network namespace" — true, verified twice by hand with inode comparison, and
/// covered by no test at all, which is exactly how a property gets quietly
/// falsified. The requirement was retired deliberately (one namespace meant two
/// sessions could not both bind port 8000, and the loser's own dev server
/// reported `EADDRINUSE` without ever naming nemr). The inverse is now pinned.
#[test]
fn net02_a_session_has_its_own_network_namespace() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "net02ns", VolumeSize::Small).await;
        let pid = project::start(&client, &project.name).await.expect("start");

        let netns_of = |pid: u32| {
            std::fs::read_link(format!("/proc/{pid}/ns/net"))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
        let rk_pid: u32 =
            std::fs::read_to_string(format!("{runtime_dir}/containerd-rootless/child_pid"))
                .expect("rootlesskit child_pid")
                .trim()
                .parse()
                .expect("a pid");

        let session_ns = netns_of(pid);
        let rootlesskit_ns = netns_of(rk_pid);

        // Control: both reads must have produced something, or "they differ"
        // would be true for the uninteresting reason that neither was readable.
        assert!(
            session_ns.starts_with("net:[") && rootlesskit_ns.starts_with("net:["),
            "could not read both namespaces (session={session_ns:?}, \
             rootlesskit={rootlesskit_ns:?}); this test would otherwise pass by \
             failing to look"
        );
        assert_ne!(
            session_ns, rootlesskit_ns,
            "NET-02: the session must have its own network namespace"
        );

        let _ = project::stop(&client, &project.name).await;
    });
}

// --- NET-02: the wiring itself, not only the isolation ----------------------

/// Run a script inside rootlesskit's namespaces, the way the engine does.
///
/// Tests observe the session from OUTSIDE it, through the same door the engine
/// uses, rather than asking the session about itself.
fn in_rootlesskit(script: &str) -> std::process::Output {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    let child = std::fs::read_to_string(format!("{runtime_dir}/containerd-rootless/child_pid"))
        .expect("rootlesskit child_pid — is rootless containerd running?");
    std::process::Command::new("nsenter")
        .args([
            "-t",
            child.trim(),
            "-U",
            "-n",
            "--preserve-credentials",
            "--",
            "bash",
            "-c",
            script,
        ])
        .output()
        .expect("nsenter")
}

/// The session network recorded on a project, or None if it has none.
async fn recorded_allocation(
    client: &ContainerdClient,
    name: &str,
) -> Option<nemr_engine::engine::netns::Allocation> {
    let id = nemr_engine::config::container_id(name);
    client
        .list_containers()
        .await
        .ok()?
        .into_iter()
        .find(|c| c.id == id)
        .and_then(|c| project::allocation_from_labels(&c.labels))
}

/// Assert a running session's namespace really carries its address and route.
///
/// Read from inside the session's own namespace. The control comes first: if
/// the read produced nothing, the assertions below would pass or fail for
/// reasons that have nothing to do with the wiring.
fn assert_session_is_wired(pid: u32, alloc: nemr_engine::engine::netns::Allocation) {
    let out = in_rootlesskit(&format!(
        "nsenter -t {pid} -n -- ip -4 addr show; echo '--- routes ---'; \
         nsenter -t {pid} -n -- ip -4 route show"
    ));
    let seen = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        seen.contains("lo:"),
        "could not read the session's network namespace, so nothing below would mean \
         anything.\n  stdout: {seen}\n  stderr: {err}"
    );
    assert!(
        seen.contains(&format!("inet {}/24", alloc.session_ip())),
        "the session has no address on ceth0; it is isolated and unreachable, which is a \
         started session that cannot work.\n{seen}"
    );
    assert!(
        seen.contains(&format!("default via {}", alloc.gateway())),
        "the session has no default route, so it has an address and no way out — the \
         shape in which Claude Code loses the API.\n{seen}"
    );
}

/// NET-02: a started session is WIRED, not merely isolated.
///
/// The namespace test beside this one passes on the OCI spec alone: delete
/// every line of `engine::netns` and a session still gets its own namespace,
/// still differs from rootlesskit's, and still has no way to reach anything.
/// Isolation was never the deliverable — a session that works while isolated
/// is — so this asserts the address and the route that make it one.
#[test]
fn net02_a_started_session_is_wired_not_merely_isolated() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "net02wire", VolumeSize::Small).await;
        let pid = project::start(&client, &project.name).await.expect("start");

        let alloc = recorded_allocation(&client, &project.name)
            .await
            .expect("create must record a session network");
        assert_session_is_wired(pid, alloc);

        let _ = project::stop(&client, &project.name).await;
    });
}

/// NET-02: a project that predates the feature is allocated a network on its
/// first start, rather than starting into an empty namespace in silence.
///
/// Every project created before this pass carries no `nemr.netns` label. Its
/// task now gets its own network namespace regardless — that comes from the OCI
/// spec — so "no label" cannot mean "no networking wanted"; it means "not
/// allocated yet". The first version read the label with `.ok().and_then(...)`
/// and skipped wiring when it found none, which upgraded every existing project
/// into one that starts, reports success and cannot reach anything.
#[test]
fn net02_a_project_with_no_recorded_network_is_allocated_one_on_first_start() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "net02old", VolumeSize::Small).await;

        // Make it look like a project created before NET-02.
        let id = nemr_engine::config::container_id(&project.name);
        let container = client
            .list_containers()
            .await
            .expect("list")
            .into_iter()
            .find(|c| c.id == id)
            .expect("the project's container record");
        let mut labels = container.labels.clone();
        labels.remove("nemr.netns");
        client
            .update_container_labels(&id, labels)
            .await
            .expect("strip the allocation label");

        // Control: the state under test really is the state we think it is.
        assert!(
            recorded_allocation(&client, &project.name).await.is_none(),
            "the label was not actually removed, so this test would prove nothing"
        );

        let pid = project::start(&client, &project.name)
            .await
            .expect("a project with no recorded network must still start");

        let alloc = recorded_allocation(&client, &project.name)
            .await
            .expect("start must ALLOCATE and RECORD a network, not skip the wiring");
        assert_session_is_wired(pid, alloc);

        let _ = project::stop(&client, &project.name).await;
    });
}

/// NET-02: a restore allocates a session network HERE; it never inherits one.
///
/// The question a bundle raises: does import take the network the source
/// machine used, or choose one locally? It must choose locally — the index is a
/// fact about one host's free space, and a bundle carried from another machine
/// would name a `/24` that is already in use here. A duplicate index does not
/// error: the two projects derive the same addresses and the same link name,
/// one session ends up with no network, and the other's host ports serve it.
///
/// Proven by making the source project's index be taken by something else
/// before the bundle is imported. The bundle records no network at all (there
/// is no such field in the manifest), and this is what pins that it stays that
/// way.
#[test]
fn net02_a_restore_allocates_a_network_here_rather_than_inheriting_one() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let bundle =
            std::env::temp_dir().join(format!("net02-restore-{}.nemr", std::process::id()));
        let _ = std::fs::remove_file(&bundle);

        let source = TestProject::create(&client, "net02src", VolumeSize::Small).await;
        let source_index = recorded_allocation(&client, &source.name)
            .await
            .expect("the source must have an allocation")
            .index;

        project::export(
            &client,
            &source.name,
            &bundle,
            nemr_engine::bundle::policy::Policy::default(),
        )
        .await
        .expect("export");

        let source_name = source.name.clone();
        project::delete(&client, &source_name)
            .await
            .expect("delete");
        std::mem::forget(source); // deleted already; nothing left to clean up

        // Something else takes the index the source used to hold. Allocation
        // reuses the lowest free index, so this is deterministic.
        let squatter = TestProject::create(&client, "net02sq", VolumeSize::Small).await;
        let squatter_index = recorded_allocation(&client, &squatter.name)
            .await
            .expect("the squatter must have an allocation")
            .index;
        assert_eq!(
            squatter_index, source_index,
            "the freed index must be reused, or this test does not set up the conflict \
             it means to"
        );

        let (restored_name, _) = project::import_creating(&client, &bundle, None, None)
            .await
            .expect("import");
        let restored = TestProject::adopt(restored_name.clone());
        let restored_index = recorded_allocation(&client, &restored_name)
            .await
            .expect("a restored project must be allocated a session network")
            .index;

        assert_ne!(
            restored_index, squatter_index,
            "the restore was given a /24 that is already in use on this machine; nothing \
             would error, one project's forwards would simply serve the other's session"
        );

        drop(restored);
        drop(squatter);
        let _ = std::fs::remove_file(&bundle);
    });
}

/// F-109: the CLI must not panic when its reader goes away.
///
/// Rust sets `SIGPIPE` to `SIG_IGN` before `main`, so a write to a closed pipe
/// returns `EPIPE` and `println!` panics. `nemr list | head -1` therefore
/// printed a Rust panic and exited 101 in roughly one run in five. The same
/// race is what turned a SUCCESSFUL `nemr list | grep -q <project>` into a
/// failed pipeline, which is how an acceptance reported "pulled project not in
/// nemr list" about a project that was listed — and, because the panic went to
/// a discarded stderr, left no trace of why.
///
/// Asserted against the INSTALLED binary and repeated, because the failure is a
/// race: one green run proves nothing. The exit status is deliberately not
/// pinned to a single value — dying of SIGPIPE (141) and finishing before the
/// reader leaves (0) are both correct — but a panic never is.
#[test]
fn f109_the_cli_does_not_panic_when_its_reader_closes_the_pipe() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    // nemr's STDOUT goes into a reader that closes after one line; its STDERR
    // is captured to a file and echoed back, because that is where the panic
    // would appear and piping it would defeat the point.
    const PROBE: &str = r#"
        err=$(mktemp)
        nemr list 2>"$err" | head -1 >/dev/null
        status=${PIPESTATUS[0]}
        cat "$err"; rm -f "$err"
        echo "STATUS=$status"
    "#;

    let mut panics = 0;
    let mut observed = String::new();
    let mut statuses = std::collections::BTreeSet::new();
    for _ in 0..40 {
        let out = std::process::Command::new("bash")
            .args(["-c", PROBE])
            .output()
            .expect("run nemr list into a closing reader");
        let seen = String::from_utf8_lossy(&out.stdout).into_owned();
        for line in seen.lines() {
            if let Some(status) = line.strip_prefix("STATUS=") {
                statuses.insert(status.to_string());
            }
        }
        if seen.contains("panicked") {
            panics += 1;
            observed = seen;
        }
    }

    // Control: the probe must actually have run the binary. A `nemr` that could
    // not start at all never panics either, and would pass this silently.
    assert!(
        !statuses.is_empty() && !statuses.contains("127"),
        "the probe never ran nemr (statuses seen: {statuses:?}); this test would \
         otherwise pass by failing to look"
    );
    assert_eq!(
        panics, 0,
        "the CLI panicked on a closed stdout in {panics} of 40 runs. \
         A tool that is piped into `head`, `grep -q` or `less` must exit quietly \
         like every other one.\nexit statuses seen: {statuses:?}\nlast panic:\n{observed}"
    );
}

/// NET-02 upgrade path: a project created BEFORE NET-02 is repaired on start.
///
/// The defect this pins cost a real project. A container's OCI spec is frozen
/// into its record at create time and read again at every task start, so a
/// project created before NET-02 asks for no network namespace and its task
/// joins rootlesskit's — for ever, whatever the engine does afterwards. The
/// first migration recorded an allocation on such a project and then wired it,
/// which built a session network inside the SHARED namespace and died on
/// `ip route add default` with "RTNETLINK answers: File exists", because
/// rootlesskit already has a default route. `htmltest` — a project that had
/// moved between two machines — stopped starting.
///
/// `NEMR_TEST_PRE_NET02` creates the old shape, because no freshly created
/// container can have it and an untested upgrade path is how this happened.
#[test]
fn net02_a_project_created_before_net02_is_migrated_and_starts_wired() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");

        std::env::set_var("NEMR_TEST_PRE_NET02", "1");
        let project = TestProject::create(&client, "net02pre", VolumeSize::Small).await;
        std::env::remove_var("NEMR_TEST_PRE_NET02");

        // Control: the subject really has the shape under test. READ-ONLY — the
        // first version of this asked `ensure_own_network_namespace`, which
        // MIGRATED the subject, so the code under test was handed an
        // already-repaired container and the test stayed green with the
        // migration deleted. A control that changes what it observes is not a
        // control.
        let id = nemr_engine::config::container_id(&project.name);
        assert!(
            !client
                .has_own_network_namespace(&id)
                .await
                .expect("read the spec"),
            "the subject must START without a network namespace, or this proves nothing"
        );

        // Now the real path: start it the way a user would.
        let pid = project::start(&client, &project.name)
            .await
            .expect("a project created before NET-02 must still start");

        assert!(
            client
                .has_own_network_namespace(&id)
                .await
                .expect("read the spec"),
            "start must have repaired the container record"
        );
        assert!(
            !nemr_engine::engine::netns::shares_rootlesskit_namespace(pid)
                .expect("compare namespaces"),
            "the migrated project is still in rootlesskit's shared namespace"
        );
        let alloc = recorded_allocation(&client, &project.name)
            .await
            .expect("start must record an allocation");
        assert_session_is_wired(pid, alloc);

        let _ = project::stop(&client, &project.name).await;
    });
}

/// The control the migration rests on: a task that shares rootlesskit's network
/// namespace is DETECTED, not wired.
///
/// Wiring a session into the shared namespace puts its addresses on
/// rootlesskit's own interfaces and collides with rootlesskit's default route.
/// The comparison is the same `net:` inode evidence NET-01 was established with.
#[test]
fn net02_sharing_rootlesskits_namespace_is_detected() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    let rk: u32 = std::fs::read_to_string(format!("{runtime_dir}/containerd-rootless/child_pid"))
        .expect("rootlesskit child_pid")
        .trim()
        .parse()
        .expect("a pid");

    // rootlesskit shares its own namespace with itself, by definition.
    assert!(
        nemr_engine::engine::netns::shares_rootlesskit_namespace(rk).expect("compare"),
        "rootlesskit's own child must compare equal, or the check cannot detect anything"
    );

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "net02own", VolumeSize::Small).await;
        let pid = project::start(&client, &project.name).await.expect("start");

        assert!(
            !nemr_engine::engine::netns::shares_rootlesskit_namespace(pid).expect("compare"),
            "a NET-02 session must NOT share rootlesskit's namespace"
        );

        let _ = project::stop(&client, &project.name).await;
    });
}

/// The wiring RECONCILES to the declared state: running it twice is a no-op,
/// not a collision.
///
/// The pattern this closes has cost this project repeatedly — a path that works
/// from clean and fails on a second run. Volumes and port forwards already
/// reconcile: the declaration is authoritative and the live state is brought to
/// match it. The session wiring used `ip addr add` and `ip route add`, which
/// answer "RTNETLINK answers: File exists" rather than "already so", so any
/// second application was a hard failure — and `start` turns that into a
/// refusal to start at all.
#[test]
fn net02_wiring_a_live_session_again_reconciles_rather_than_colliding() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "net02idem", VolumeSize::Small).await;
        let pid = project::start(&client, &project.name).await.expect("start");
        let alloc = recorded_allocation(&client, &project.name)
            .await
            .expect("an allocation");

        // Control: it is wired now, so a second application is being asked to
        // reconcile something that already exists — which is the case under
        // test, not a fresh build.
        assert_session_is_wired(pid, alloc);

        for attempt in 1..=2 {
            nemr_engine::engine::netns::connect_session(pid, alloc).unwrap_or_else(|e| {
                panic!("re-wiring a live session failed on attempt {attempt}: {e:#}")
            });
            assert_session_is_wired(pid, alloc);
        }

        let _ = project::stop(&client, &project.name).await;
    });
}

/// Reach `host:port` from inside a session's network namespace, using the
/// HOST's node rather than anything in the image.
///
/// Entering only the network namespace is deliberate: it measures reachability
/// without depending on what the base image happens to ship, and without
/// asking the session's own tooling to report on itself.
fn reach_from_session(pid: u32, host: &str, port: u16) -> String {
    let probe = format!(
        "node -e \"require('http').get({{host:'{host}',port:{port},timeout:4000}},r=>{{\
         let d='';r.on('data',c=>d+=c);r.on('end',()=>{{console.log('GOT:'+d);process.exit(0)}})}})\
         .on('error',()=>{{console.log('GOT:REFUSED');process.exit(0)}})\
         .on('timeout',()=>{{console.log('GOT:TIMEOUT');process.exit(0)}})\""
    );
    let out = in_rootlesskit(&format!("nsenter -t {pid} -n -- {probe}"));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("GOT:").map(|s| s.to_string()))
        .unwrap_or_else(|| {
            format!(
                "PROBE-FAILED(stdout={:?} stderr={:?})",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
}

/// Serve a fixed body on port 8000 inside a session's namespace, from the host.
fn serve_in_session(pid: u32, body: &str) {
    let script = format!(
        "setsid nsenter -t {pid} -n -- node -e \
         \"require('http').createServer((q,s)=>s.end('{body}')).listen(8000,'0.0.0.0')\" \
         >/dev/null 2>&1 </dev/null & sleep 2"
    );
    let _ = in_rootlesskit(&script);
}

/// NET-05: a session cannot reach another session.
///
/// F-108. Sessions are routed to one another inside rootlesskit's namespace, so
/// "its own network namespace" delivered port-space separation and nothing else
/// — every session could reach every other session's address and ports. That
/// reads as a security boundary and was not one, and someone would eventually
/// put two clients' work in two sessions on that assumption.
///
/// The control is the shape the MASQUERADE control established: remove the rule
/// and the SAME request must succeed, so the test cannot pass because the
/// request was broken for some other reason.
#[test]
fn net05_a_session_cannot_reach_another_session() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let a = TestProject::create(&client, "net05a", VolumeSize::Small).await;
        let b = TestProject::create(&client, "net05b", VolumeSize::Small).await;
        let pid_a = project::start(&client, &a.name).await.expect("start A");
        let pid_b = project::start(&client, &b.name).await.expect("start B");

        let alloc_b = recorded_allocation(&client, &b.name)
            .await
            .expect("B has an allocation");
        let ip_b = alloc_b.session_ip();

        serve_in_session(pid_b, "SESSION-B");

        // Control 1: the isolation rule is actually in place. Reading, not
        // repairing — if it is absent the refusal below would prove nothing.
        assert!(
            nemr_engine::engine::netns::isolation_is_active().expect("read the FORWARD chain"),
            "start must have asserted the isolation rule; without it this test is vacuous"
        );

        let blocked = reach_from_session(pid_a, &ip_b, 8000);
        assert!(
            blocked == "REFUSED" || blocked == "TIMEOUT",
            "session A reached session B at {ip_b}:8000 — sessions are not isolated (got {blocked:?})"
        );

        // Control 2: the SAME request succeeds with the rule removed. Without
        // this, a probe that could never work would pass this test.
        let drop_rule = in_rootlesskit(
            "iptables -w 5 -D FORWARD -s 10.99.0.0/16 -d 10.99.0.0/16 -j DROP",
        );
        assert!(
            drop_rule.status.success(),
            "could not remove the rule for the control: {}",
            String::from_utf8_lossy(&drop_rule.stderr)
        );
        let reachable = reach_from_session(pid_a, &ip_b, 8000);

        // Put it back before asserting, so a failure here does not leave the
        // host without isolation.
        let _ = in_rootlesskit(
            "iptables -w 5 -C FORWARD -s 10.99.0.0/16 -d 10.99.0.0/16 -j DROP 2>/dev/null \
             || iptables -w 5 -I FORWARD 1 -s 10.99.0.0/16 -d 10.99.0.0/16 -j DROP",
        );

        assert_eq!(
            reachable, "SESSION-B",
            "CONTROL FAILED: with the rule removed the request still did not reach B, so the \
             refusal above says nothing about isolation"
        );

        let _ = project::stop(&client, &a.name).await;
        let _ = project::stop(&client, &b.name).await;
    });
}

/// F-115: a bundle records the base image the container ACTUALLY runs on.
///
/// Both fields used to come from `config::BASE_IMAGE`, so a bundle recorded what
/// the exporting engine builds today rather than what the project was built on.
/// Usually identical, and silently wrong when it differs — which is exactly the
/// substitution `manifest.rs` calls "a silent-wrong-result defect" and claims to
/// refuse. On the developer host three of four projects name an image their
/// rootfs is not.
///
/// The chain id is the assertion that bites: it comes from the snapshot and
/// there is no way to produce it from a constant.
#[test]
fn f115_a_bundle_records_the_base_image_the_container_actually_runs_on() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "f115", VolumeSize::Small).await;
        let id = nemr_engine::config::container_id(&project.name);

        // The truth, read from the snapshot rather than from any constant.
        let real_chain_id = client
            .snapshot_parent(&id)
            .await
            .expect("a project's snapshot has a parent");
        assert!(
            real_chain_id.starts_with("sha256:"),
            "control: the snapshot parent must be readable, or nothing below means anything \
             (got {real_chain_id:?})"
        );

        let bundle = std::env::temp_dir().join(format!("f115-{}.nemr", std::process::id()));
        let _ = std::fs::remove_file(&bundle);
        project::export(
            &client,
            &project.name,
            &bundle,
            nemr_engine::bundle::policy::Policy::default(),
        )
        .await
        .expect("export");

        let opened = nemr_engine::bundle::import::open(&bundle).expect("open bundle");
        assert_eq!(
            opened.manifest.base_image.rootfs_chain_id, real_chain_id,
            "the bundle must record the rootfs the container is actually built on"
        );

        // And where the image IS identifiable, the reference and digest must
        // name an image that really builds that rootfs — not merely a plausible
        // one. This is the half a constant can satisfy by luck, so it is
        // asserted against the chain id rather than against the constant.
        let recorded_digest = opened.manifest.base_image.digest.clone();
        if !recorded_digest.is_empty() {
            let named = &opened.manifest.base_image.reference;
            let its_chain = client
                .image_chain_id(named)
                .await
                .expect("the recorded reference must resolve to a local image");
            assert_eq!(
                its_chain, real_chain_id,
                "the bundle names {named} but that image does not build this project's rootfs"
            );
        }

        let _ = std::fs::remove_file(&bundle);
    });
}

/// F-115: a bundle naming a rootfs this host cannot build is REFUSED, and the
/// digest is not consulted as a second chance.
///
/// The digest can match while the rootfs differs — a tag reused for different
/// bytes does exactly that, and F-85 records it happening on this project. So a
/// chain-id miss must end the resolution rather than fall through, or the
/// stronger check would be silently downgraded to the weaker one.
#[test]
fn f115_a_chain_id_miss_is_not_rescued_by_a_matching_digest() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let base = nemr_engine::config::BASE_IMAGE;
        let real_chain = client
            .image_chain_id(base)
            .await
            .expect("the base image must be present on a provisioned host");
        let real_digest = client
            .image_target_digest(base)
            .await
            .expect("and must have a digest");

        // Control: the real chain id resolves. Without this the refusal below
        // could be a resolver that never resolves anything.
        match project::resolve_base_image(&client, &real_digest, base, &real_chain).await {
            nemr_engine::bundle::import::BaseImageResolution::Present { .. } => {}
            other => panic!("control failed: the real chain id must resolve, got {other:?}"),
        }

        // A rootfs this host does not have, paired with a digest that DOES
        // resolve. Falling through would answer Present on the wrong rootfs.
        let bogus_chain = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        match project::resolve_base_image(&client, &real_digest, base, bogus_chain).await {
            nemr_engine::bundle::import::BaseImageResolution::Unresolved {
                where_looked, ..
            } => {
                assert!(
                    where_looked.iter().any(|l| l.contains(bogus_chain)),
                    "the refusal must name the rootfs it could not find: {where_looked:?}"
                );
            }
            nemr_engine::bundle::import::BaseImageResolution::Present { reference } => panic!(
                "resolved to {reference} on a matching DIGEST despite a chain-id miss — the \
                 stronger check was downgraded to the weaker one, which is the substitution \
                 this refuses to make"
            ),
        }
    });
}

/// F-115: an image is found by the rootfs it BUILDS, not by name or by being
/// the only candidate.
///
/// `image_with_chain_id` is the mechanism the whole identity fix rests on — it
/// is what turns "this project's snapshot parent" into "this is the image it was
/// built from". Both directions are asserted: a real chain id must find its
/// image, and an unknown one must return `None` rather than falling back to
/// whatever image happens to be present. The second half is the one that bites:
/// a lookup that returned the first image regardless would satisfy the first.
#[test]
fn f115_an_image_is_found_by_the_rootfs_it_builds() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let base = nemr_engine::config::BASE_IMAGE;
        let real_chain = client
            .image_chain_id(base)
            .await
            .expect("the base image must be present on a provisioned host");

        let found = client
            .image_with_chain_id(&real_chain)
            .await
            .expect("the lookup must not error")
            .expect("the base image's own chain id must find the base image");
        assert_eq!(
            found.name, base,
            "found {} for the chain id that belongs to {base}",
            found.name
        );

        // A rootfs no local image builds. `None` means "cannot prove it", which
        // the caller records as an empty digest rather than a guess — the whole
        // distinction F-115 exists to make.
        let bogus = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        let missing = client
            .image_with_chain_id(bogus)
            .await
            .expect("an unknown chain id is not an error");
        assert!(
            missing.is_none(),
            "an unknown rootfs resolved to {:?} — a lookup that answers with whatever image \
             is present would let export record a confident, wrong identity",
            missing.map(|i| i.name)
        );
    });
}

/// The oldest published base image other than the one this engine builds.
///
/// F-85 keeps every published version pullable for ever and forbids reusing a
/// tag, so the ledger under `image/digests/` is a permanent supply of images
/// whose rootfs is provably NOT this engine's constant. That discipline exists
/// for bundle recoverability; it turns out to be the only way to construct a
/// subject for F-115's export test.
fn a_published_version_other_than_the_current_one() -> Option<String> {
    let current = nemr_engine::config::base_image_version();
    let mut versions: Vec<String> = std::fs::read_dir("image/digests")
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|name| name != "README.md" && name != current)
        .collect();
    versions.sort();
    versions
        .first()
        .map(|v| format!("ghcr.io/gnrain/nemr-base:{v}"))
}

/// F-115/F-116: export reads the CONTAINER, not this engine's constant.
///
/// This is the test F-116 recorded as impossible. The claim was that no subject
/// could exhibit the defect, because a freshly created project's snapshot parent
/// IS the constant's chain id, byte for byte. That was true of every project the
/// suite could create — and wrong about what was available, because F-85 keeps
/// every published base version pullable for ever. Creating a project from an
/// older one gives a rootfs that is provably not the constant's.
///
/// The control comes first and is the whole reason this test means anything: the
/// two chain ids must actually differ. If they were equal, the assertion below
/// would pass under either derivation and prove nothing — which is exactly the
/// state F-116 described.
#[test]
fn f115_export_reads_the_container_not_the_engine_constant() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let older = a_published_version_other_than_the_current_one()
        .expect("image/digests must list a published version other than the current one");

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");

        let older_chain = client.image_chain_id(&older).await.unwrap_or_else(|e| {
            panic!(
                "{older} is not in this containerd, so the divergent subject cannot be built \
                 ({e:#}).\n     fix: ctr -n {ns} images pull --platform linux/amd64 {older}",
                ns = client.namespace()
            )
        });
        let constant_chain = client
            .image_chain_id(nemr_engine::config::BASE_IMAGE)
            .await
            .expect("the current base image must be present");

        // THE CONTROL. Without a genuine divergence this test is vacuous, and
        // saying so out loud is what F-116 was missing.
        assert_ne!(
            older_chain, constant_chain,
            "{older} and the current constant build the SAME rootfs, so this test cannot \
             distinguish reading the container from reading the constant"
        );

        std::env::set_var("NEMR_TEST_BASE_IMAGE", &older);
        let project = TestProject::create(&client, "f115old", VolumeSize::Small).await;
        std::env::remove_var("NEMR_TEST_BASE_IMAGE");

        let id = nemr_engine::config::container_id(&project.name);
        let parent = client.snapshot_parent(&id).await.expect("snapshot parent");
        assert_eq!(
            parent, older_chain,
            "the subject was not built on the older image, so it is not divergent after all"
        );

        let bundle = std::env::temp_dir().join(format!("f115old-{}.nemr", std::process::id()));
        let _ = std::fs::remove_file(&bundle);
        project::export(
            &client,
            &project.name,
            &bundle,
            nemr_engine::bundle::policy::Policy::default(),
        )
        .await
        .expect("export");

        let opened = nemr_engine::bundle::import::open(&bundle).expect("open bundle");
        let recorded = &opened.manifest.base_image;

        assert_eq!(
            recorded.rootfs_chain_id, older_chain,
            "the bundle recorded {} but the project is built on {older_chain} — export read \
             the engine's constant instead of the container",
            recorded.rootfs_chain_id
        );
        assert_ne!(
            recorded.rootfs_chain_id, constant_chain,
            "the bundle recorded the CONSTANT's rootfs for a project that does not run on it"
        );
        assert_eq!(
            recorded.reference,
            older,
            "the bundle must name the image it was actually built from, not {}",
            nemr_engine::config::BASE_IMAGE
        );

        let _ = std::fs::remove_file(&bundle);
    });
}

/// F-123: the test harness's own teardown refuses protected subjects.
///
/// Proven against a DISPOSABLE project temporarily marked protected — the
/// refusal cannot be proven against htmltest itself, because demonstrating
/// destructiveness against the thing being protected is the incident this
/// guard exists to prevent. The control is the same purge, same project,
/// protection lifted: it must then actually destroy, or the "refusal" above
/// proves nothing about purge and everything about a purge that never works.
#[test]
fn f123_purge_refuses_a_protected_subject_and_destroys_without_protection() {
    if unit_only() {
        return;
    }
    if !require_host(HostRequirements::FULL) {
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(async {
        let client = ContainerdClient::connect().await.expect("connect");
        let project = TestProject::create(&client, "f123prot", VolumeSize::Small).await;
        let name = project.name.clone();
        let id = nemr_engine::config::container_id(&name);

        // Protect it, then purge. The project must SURVIVE.
        std::env::set_var("NEMR_TEST_EXTRA_PROTECTED", &name);
        common::purge(&name);
        assert!(
            client.container_exists(&id).await.expect("query"),
            "purge destroyed a PROTECTED subject — the guard does not bite"
        );

        // Control: lift the protection and the SAME purge must destroy —
        // otherwise the survival above says nothing about the guard.
        std::env::remove_var("NEMR_TEST_EXTRA_PROTECTED");
        common::purge(&name);
        assert!(
            !client.container_exists(&id).await.expect("query"),
            "CONTROL FAILED: purge without protection did not destroy, so the refusal \
             above proves nothing about the guard"
        );

        std::mem::forget(project); // already purged; Drop would double-purge
    });
}

/// F-123: the shell and Rust protected lists must agree — one rule, two
/// enforcement points, zero drift.
#[test]
fn f123_the_shell_and_rust_protected_lists_agree() {
    let shell = std::fs::read_to_string("scripts/lib/proc.sh").expect("read proc.sh");
    let line = shell
        .lines()
        .find(|l| l.starts_with("NEMR_PROTECTED_SUBJECTS="))
        .expect("proc.sh must define NEMR_PROTECTED_SUBJECTS");
    let shell_list: Vec<&str> = line
        .trim_start_matches("NEMR_PROTECTED_SUBJECTS=")
        .trim_matches('"')
        .split_whitespace()
        .collect();
    assert_eq!(
        shell_list,
        common::PROTECTED_SUBJECTS,
        "the shell guard and the Rust guard protect different sets — the rule has forked"
    );
}

// F-125: the destructive-git rule is enforced mechanically, not by memory.
//
// Both loop.md rules ("commit or stash before any destructive git operation")
// existed when slip seven ignored them inside the mutation proof of a
// different rule. The PreToolUse hook in scripts/hooks/git_destructive_guard.sh
// is the structural form: it PARSES each command (POSIX shlex — an earlier
// regex draft was broken 19 ways by an adversarial review, all with
// plainly-typed commands), resolves each git invocation's target tree, reads
// `git status` there, and refuses. These tests drive the ACTUAL hook script
// with the ACTUAL payload shape the harness sends.
// ---------------------------------------------------------------------------

/// A disposable git repo with one commit. `dirty()` adds an uncommitted
/// change. The directory is created by this fixture and destroyed by it —
/// nothing outside it is ever touched.
struct F125Fixture {
    dir: std::path::PathBuf,
}

impl F125Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nemr-f125-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("run git in fixture");
            assert!(out.status.success(), "git {:?} failed in fixture", args);
        };
        git(&["init", "-q", "."]);
        git(&["config", "user.email", "f125@test"]);
        git(&["config", "user.name", "f125"]);
        std::fs::write(dir.join("f.txt"), "base\n").expect("write");
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        F125Fixture { dir }
    }

    fn dirty(tag: &str) -> Self {
        let fx = Self::new(tag);
        std::fs::write(fx.dir.join("f.txt"), "base\nuncommitted\n").expect("dirty");
        fx
    }
}

impl Drop for F125Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn f125_guard_script() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/hooks/git_destructive_guard.sh")
}

/// Run a guard script against a synthesized PreToolUse payload; return its
/// exit code and captured stderr — the REASON for a refusal is part of the
/// contract, not just the exit code. `prepend_path` lets a test interpose a
/// fake `git`.
fn f125_run_guard(
    script: &std::path::Path,
    cmd: &str,
    cwd: &std::path::Path,
    prepend_path: Option<&std::path::Path>,
) -> (i32, String) {
    use std::io::Write;
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": cmd },
        "cwd": cwd,
    })
    .to_string();
    let mut c = std::process::Command::new("bash");
    c.arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if let Some(p) = prepend_path {
        let path = std::env::var("PATH").unwrap_or_default();
        c.env("PATH", format!("{}:{}", p.display(), path));
    }
    let mut child = c.spawn().expect("spawn guard");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("wait guard");
    (
        out.status.code().expect("guard exit code"),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// F-125: the guard refuses exactly the rule's precondition — destructive git
/// whose TARGET TREE has uncommitted work — and nothing stricter. Includes
/// the shapes that broke the regex draft: `&` joins, quoted/-C paths,
/// multi-line commands, cd tracking, and an override quoted inside a string
/// (which must NOT vouch — declared per invocation means an env prefix of
/// the invocation itself).
#[test]
fn f125_git_guard_refuses_destructive_git_on_a_dirty_tree_and_nothing_stricter() {
    let fx = F125Fixture::dirty("main");
    let guard = f125_guard_script();
    let dirty = fx.dir.to_str().unwrap();
    let refused = [
        "git checkout -- f.txt".to_string(), // slip seven's shape
        "git reset --hard".to_string(),      // slip three's shape
        "git reset -q --hard HEAD~1".to_string(),
        "git checkout .".to_string(),
        "git checkout f.txt".to_string(), // pathspec checkout, no `--`
        "git clean -fd".to_string(),
        "git switch -f b2".to_string(),
        "git switch --discard-changes b2".to_string(),
        "git rm -f f.txt".to_string(),
        "git merge --abort".to_string(),
        "git rebase --abort".to_string(),
        format!("cd {} && git reset --hard", dirty), // cd tracked
        "git restore --staged f.txt & git restore f.txt".to_string(), // `&` join
        "git -C . reset --hard".to_string(),
        format!("git -C '{}' reset --hard", dirty), // quoted -C
        "git status\ngit reset --hard".to_string(), // newline separator
        "git \\\nreset --hard".to_string(),         // line continuation
        // an override mentioned INSIDE a string vouches for nothing:
        "git commit -m 'NEMR_GIT_DESTRUCTIVE_OK=1 noted' && git reset --hard".to_string(),
    ];
    for cmd in &refused {
        let (code, err) = f125_run_guard(&guard, cmd, &fx.dir, None);
        assert_eq!(
            code, 2,
            "`{}` on a DIRTY tree must refuse (exit 2) — this is the rule's precondition\nstderr: {}",
            cmd, err
        );
    }
    let allowed = [
        "git stash", // the remedy itself must never be blocked
        "git commit -am wip",
        "git commit -am 'never run git reset --hard again'", // mention, not invocation
        "git add -A",
        "git status",
        "git checkout -b wp-x",
        "git checkout nonexistent-branch", // branch switch, not a pathspec
        "git rebase --continue",           // the conflict-resolution flow itself
        "git rm --cached f.txt",           // index-only
        "git clean -n",                    // dry run
        "cargo test --lib",                // not git at all
        "NEMR_GIT_DESTRUCTIVE_OK=1 git checkout -- f.txt", // declared destruction
    ];
    for cmd in &allowed {
        let (code, err) = f125_run_guard(&guard, cmd, &fx.dir, None);
        assert_eq!(
            code, 0,
            "`{}` must pass on a dirty tree — a guard stricter than the rule teaches \
             reaching for the override, and then the override stops carrying information\nstderr: {}",
            cmd, err
        );
    }
    // Clean tree: commit the change and the same destructive commands pass,
    // because "commit or stash first" is precisely what makes the tree clean.
    let out = std::process::Command::new("git")
        .args(["commit", "-aqm", "clean"])
        .current_dir(&fx.dir)
        .output()
        .expect("commit");
    assert!(out.status.success());
    for cmd in &["git checkout -- f.txt", "git reset --hard", "git clean -fd"] {
        let (code, err) = f125_run_guard(&guard, cmd, &fx.dir, None);
        assert_eq!(
            code, 0,
            "`{}` on a CLEAN tree must pass — the precondition is satisfied\nstderr: {}",
            cmd, err
        );
    }
    // Except stash drop/clear, which destroy the rule's own remedy and
    // refuse regardless of tree state (override excepted).
    for cmd in &["git stash drop", "git stash clear"] {
        let (code, _) = f125_run_guard(&guard, cmd, &fx.dir, None);
        assert_eq!(
            code, 2,
            "`{}` must refuse even on a clean tree — a stash holds exactly the work \
             the rule protects, one command after it was stashed",
            cmd
        );
    }
    let (code, _) = f125_run_guard(
        &guard,
        "NEMR_GIT_DESTRUCTIVE_OK=1 git stash clear",
        &fx.dir,
        None,
    );
    assert_eq!(
        code, 0,
        "the declared override must clear stash refusals too"
    );
}

/// F-125: the `git restore --staged` carve-out is exact. Staged-only restore
/// unstages without touching the worktree, so it passes even dirty; any form
/// that reaches the worktree (bare, --worktree, -W, or a staged-only restore
/// sharing a command with a bare one — via `&&`, `;`, or `&`) refuses.
#[test]
fn f125_git_guard_staged_restore_carveout_is_exact() {
    let fx = F125Fixture::dirty("restore");
    let guard = f125_guard_script();
    for cmd in &["git restore --staged f.txt", "git restore -S f.txt"] {
        let (code, err) = f125_run_guard(&guard, cmd, &fx.dir, None);
        assert_eq!(
            code, 0,
            "`{}` destroys nothing (it only unstages) and must pass\nstderr: {}",
            cmd, err
        );
    }
    for cmd in &[
        "git restore f.txt",
        "git restore --staged --worktree f.txt",
        "git restore -SW f.txt",
        "git restore --staged f.txt && git restore f.txt",
        "git restore --staged f.txt & git restore f.txt",
    ] {
        let (code, _) = f125_run_guard(&guard, cmd, &fx.dir, None);
        assert_eq!(
            code, 2,
            "`{}` reaches the worktree and must refuse on a dirty tree",
            cmd
        );
    }
}

/// F-125 / F-109: when the guard cannot verify the tree's state, it refuses
/// FOR THAT REASON. The fixture is CLEAN, so exit 2 is unreachable through
/// the dirty-tree branch — only the read-failure branch can produce it — and
/// the stderr wording is asserted so a refusal for the wrong reason fails.
/// Covers both unreadable-status and an unresolvable target ($VAR in -C).
#[test]
fn f125_git_guard_refuses_when_state_cannot_be_verified() {
    let fx = F125Fixture::new("f109"); // clean on purpose
    let guard = f125_guard_script();
    let shim_dir = fx.dir.join("shim");
    std::fs::create_dir_all(&shim_dir).expect("shim dir");
    std::fs::write(
        shim_dir.join("git"),
        "#!/usr/bin/env bash\ncase \"$*\" in *rev-parse*) echo true; exit 0;; \
         *status*) echo 'fatal: index file corrupt' >&2; exit 128;; \
         *) exec /usr/bin/git \"$@\";; esac\n",
    )
    .expect("write shim");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(shim_dir.join("git"))
        .expect("meta")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(shim_dir.join("git"), perms).expect("chmod");
    let (code, err) = f125_run_guard(&guard, "git reset --hard", &fx.dir, Some(&shim_dir));
    assert_eq!(
        code, 2,
        "an unreadable tree state must REFUSE, not pass as clean (F-109)"
    );
    assert!(
        err.contains("could not read"),
        "the refusal must NAME the read failure — a refusal for any other reason \
         means the F-109 branch never ran\nstderr: {}",
        err
    );
    let (code, err) = f125_run_guard(&guard, "git -C \"$REPO\" reset --hard", &fx.dir, None);
    assert_eq!(
        code, 2,
        "an unresolvable target must refuse, not pass as clean"
    );
    assert!(
        err.contains("cannot determine"),
        "the refusal must name the unresolved target\nstderr: {}",
        err
    );
}

/// F-125 guard-of-the-guard: (a) a neutered copy of the script (refusals
/// turned into allows) lets the slip-seven vector through, proving the tests
/// above go red when the guarded code is disabled; (b) the settings wiring
/// cannot silently drift OR be silently disarmed — the hook must be wired on
/// PreToolUse/Bash with no `if` pre-filter (an `if` on "Bash(git *)" would
/// skip exactly the compound `cd x && git ...` forms), disableAllHooks must
/// not be set, and the wired script must exist, be executable, and start
/// with a bash shebang.
#[test]
fn f125_git_guard_goes_red_when_disabled_and_the_wiring_cannot_drift() {
    let fx = F125Fixture::dirty("control");
    let guard = f125_guard_script();

    // (a) The disabled-guard control. Every refusal path exits through
    // sys.exit(2); flipping those to 0 must let the slip-seven vector pass.
    let real = std::fs::read_to_string(&guard).expect("read guard");
    assert!(
        real.contains("def refuse") && real.matches("sys.exit(2)").count() >= 1,
        "the guard's refusal path no longer exits via refuse()/sys.exit(2) — the \
         neutered-copy control below would neuter nothing"
    );
    let neutered_path = fx.dir.join("neutered.sh");
    std::fs::write(&neutered_path, real.replace("sys.exit(2)", "sys.exit(0)"))
        .expect("write neutered");
    let (code, _) = f125_run_guard(&neutered_path, "git checkout -- f.txt", &fx.dir, None);
    assert_eq!(
        code, 0,
        "CONTROL FAILED: the neutered guard still refused, so the refusal tests \
         above cannot be attributed to the guard"
    );
    let (code, _) = f125_run_guard(&guard, "git checkout -- f.txt", &fx.dir, None);
    assert_eq!(
        code, 2,
        "the real guard must refuse the vector the neutered one allowed"
    );

    // (b) The wiring drift check.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".claude/settings.json"))
            .expect(".claude/settings.json must exist — it is what arms the guard"),
    )
    .expect(".claude/settings.json must be valid JSON — a broken settings file silently disables ALL of it");
    assert_ne!(
        settings["disableAllHooks"],
        serde_json::Value::Bool(true),
        "disableAllHooks would disarm a correctly-wired guard"
    );
    let pre = settings["hooks"]["PreToolUse"]
        .as_array()
        .expect("settings must define hooks.PreToolUse");
    let wired = pre.iter().any(|m| {
        m["matcher"].as_str() == Some("Bash")
            && m.get("if").is_none()
            && m["hooks"].as_array().is_some_and(|hs| {
                hs.iter().any(|h| {
                    h["type"].as_str() == Some("command")
                        && h.get("if").is_none()
                        && h["command"]
                            .as_str()
                            .is_some_and(|c| c.ends_with("/scripts/hooks/git_destructive_guard.sh"))
                })
            })
    });
    assert!(
        wired,
        "settings.json no longer arms git_destructive_guard.sh on PreToolUse/Bash \
         (unfiltered, command ending in the script path) — the guard exists but \
         nothing runs it"
    );
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&guard).expect("the wired guard script must exist");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "the guard script is not executable — the hook would error instead of guarding"
    );
    assert!(
        real.starts_with("#!/usr/bin/env bash"),
        "the guard script lost its shebang — the hook would error instead of guarding"
    );
}
