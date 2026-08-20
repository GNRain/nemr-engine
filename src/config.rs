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

/// Session-critical Claude Code state, relocated onto the portable volume (M8).
///
/// WP-C1 measured that Claude Code writes its conversation history and session
/// state under `/root/.claude/` on the **rootfs snapshot**, which does not
/// travel with a volume export — so the whole premise of a portable session was
/// unmet (see `docs/state-locality.md`). These two subtrees are bind-mounted
/// from the volume so that history lives on the layer that travels.
///
/// The relocation is deliberately **surgical** rather than a blanket
/// `CLAUDE_CONFIG_DIR` override: `/root/.claude.json` carries machine and
/// account identity (`machineID`, `oauthAccount`) and `/root/.claude/.credentials.json`
/// is the credential, and a whole-directory move would drag both onto the
/// exportable volume, violating D-02. Only the history subtrees move; secrets
/// and identity stay on the rootfs by construction.
pub const CONTAINER_CLAUDE_PROJECTS: &str = "/root/.claude/projects";
pub const CONTAINER_CLAUDE_SESSIONS: &str = "/root/.claude/sessions";

/// Directory on the volume that holds the relocated session state. A dotted
/// name so it does not clutter the user's `/workspace` listing, and a single
/// parent so the bundle's exclusion/inclusion policy (D-06) can reason about
/// `.nemr-state/**` as one unit.
pub const VOLUME_STATE_DIR: &str = ".nemr-state";
pub const VOLUME_STATE_PROJECTS: &str = ".nemr-state/projects";
pub const VOLUME_STATE_SESSIONS: &str = ".nemr-state/sessions";

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
