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
