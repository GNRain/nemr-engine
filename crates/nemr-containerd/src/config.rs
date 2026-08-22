//! Containerd-level defaults for the wrapper.
//!
//! These moved out of the engine's `config.rs` when the wrapper became its own
//! crate: they describe how the wrapper talks to containerd (runtime,
//! snapshotter, namespace), not anything product-specific. Product-specific
//! choices — the cgroup prefix, image reference, project naming — stay in the
//! caller and reach the wrapper through [`crate::containers::ContainerSpec`].

/// Runtime containerd hands containers to. Never invoked directly (SPEC §3.1).
pub const RUNTIME: &str = "io.containerd.runc.v2";

/// Snapshotter used for container rootfs. `overlayfs` works rootless on kernel
/// 5.11+; `native` is the fallback if a host reports otherwise.
pub const SNAPSHOTTER: &str = "overlayfs";

/// How long `stop_task` waits for a task to exit on SIGTERM before escalating
/// to SIGKILL (PROC-06).
///
/// Public because the regression test asserts against it. It previously carried
/// its own literal, which is the drift this project keeps finding: a test
/// comparing production behaviour against a number that can silently diverge
/// from it is a test of nothing in particular.
pub const SIGTERM_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
