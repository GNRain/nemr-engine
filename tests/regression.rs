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

use common::{require_host, HostRequirements, TestProject};
use nemr_engine::containerd::client::ContainerdClient;
use nemr_engine::containerd::containers::StopOutcome;
use nemr_engine::engine::volume::VolumeSize;

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
