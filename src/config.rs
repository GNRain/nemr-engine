//! Paths, defaults and constants.
//!
//! Single place for values that would otherwise be scattered as literals
//! across the engine. Anything a later milestone might need to change should
//! be here rather than inline at a call site.

/// containerd namespace the engine operates in.
///
/// Matches `ctr`'s default so engine-created resources stay inspectable with
/// the stock CLI — several acceptance criteria cross-check against `ctr`.
pub const NAMESPACE: &str = "default";

/// Base image produced by Milestone 2.
pub const BASE_IMAGE: &str = "docker.io/nemr/base:0.1.0";

/// Snapshotter used for container rootfs.
///
/// `overlayfs` works rootless on kernel 5.11+ and was confirmed `ok` on the
/// reference host at Milestone 1. `native` is the fallback if a host reports
/// otherwise.
pub const SNAPSHOTTER: &str = "overlayfs";

/// Runtime containerd hands containers to. Never invoked directly (Section 3.1).
pub const RUNTIME: &str = "io.containerd.runc.v2";

/// Prefix for engine-created container IDs.
///
/// Namespacing engine containers means `ctr containers list` shows at a glance
/// which records belong to Nemr, and a project named `demo` cannot collide
/// with an unrelated container of the same name.
pub const CONTAINER_PREFIX: &str = "nemr-";

/// Container ID for a project.
pub fn container_id(project: &str) -> String {
    format!("{CONTAINER_PREFIX}{project}")
}

/// Working directory inside the container; the project volume is mounted here.
pub const CONTAINER_WORKDIR: &str = "/workspace";

/// Where Claude Code looks for its credentials inside the container.
///
/// The base image runs as root, so `HOME` is `/root`.
pub const CONTAINER_CREDENTIALS: &str = "/root/.claude/.credentials.json";

/// Prefix used in the systemd cgroup scope name, `user.slice:<prefix>:<id>`.
pub const CGROUP_PREFIX: &str = "nemr";

/// PID 1 inside a project container (Section 3.8, PROC-01/PROC-03/PROC-06).
///
/// A supervisor, not a shell. Written explicitly into the runtime spec so the
/// process model cannot drift with the base image's `CMD`. Interactive shells
/// are separate execs (PROC-02).
///
/// # Why not `sleep infinity`
///
/// The obvious supervisor is `sleep infinity`, and it was the original choice.
/// It is wrong as PID 1. Per `pid_namespaces(7)`, the kernel delivers a signal
/// sent from an ancestor namespace to a namespace's PID 1 **only if that process
/// has installed a handler for it** (SIGKILL and SIGSTOP excepted). `sleep`
/// installs none — its `/proc/1/status` shows `SigCgt: 0000000000000000` — so
/// `stop`'s SIGTERM was discarded by the kernel, and every stop waited out the
/// full grace period and then SIGKILLed. Measured before the fix: 6.4–6.8s per
/// stop, every one a kill. That is the PROC-06 defect.
///
/// This supervisor is a shell that traps SIGTERM and exits on it, so PID 1
/// actually handles the signal and `stop` completes in milliseconds. `wait $!`
/// on a backgrounded `sleep` keeps the shell interruptible: a bare `sleep` in
/// the foreground would not return control to the trap until it finished. The
/// image ships `/bin/sh` (dash), where this is portable; measured exit latency
/// on the base image is ~26ms.
pub const SUPERVISOR_ARGS: [&str; 3] = [
    "/bin/sh",
    "-c",
    "trap 'exit 0' TERM; while :; do sleep 2147483647 & wait $!; done",
];
