//! Paths, defaults and constants.
//!
//! Single place for values that would otherwise be scattered as literals
//! across the engine. Anything a later milestone might need to change should
//! be here rather than inline at a call site.

// The containerd namespace, runtime and snapshotter moved to the
// `nemr-containerd` wrapper crate's `config` when the wrapper was extracted:
// they describe how the wrapper talks to containerd, not anything about the
// product. Only product-level constants remain here.

/// Base image produced by Milestone 2.
///
/// # Why not `docker.io/...` (D-08)
///
/// It used to read `docker.io/nemr/base:0.1.0`. Nothing about the image is
/// Docker — `docker.io/` is simply the registry containerd fills in for a name
/// with no host, and nothing was ever pushed there. But on a project whose
/// NFR-01 forbids Docker at every layer, with a CI gate enforcing it, having
/// Docker's registry in the name of the primary artifact was misleading every
/// time anyone read it, and cost real reasoning twice.
///
/// GHCR per D-08: the org exists, CI already builds the image, and packages are
/// free for public repos — no new vendor and no Docker Hub rate limits.
///
/// # Versioning is load-bearing (F-85)
///
/// The version tag identifies a specific set of bytes and must NEVER be reused
/// for different ones. **Any content change to the image — a new agent, a new
/// package, an edited Dockerfile — is a version bump**, and every published
/// version stays published, because a bundle records the digest it was built
/// from and can only be restored where that exact image is available.
///
/// `0.1.0` was the single-agent image (`sha256:2be53736…`); `0.2.0` added Codex
/// (`sha256:39c5ade9…`); `0.3.0` added `curl` and `iproute2`
/// (`sha256:749c092d…`), so a session can diagnose its own network from inside. Reusing `0.1.0` for the two-agent image — which an
/// earlier pass did — left bundles referencing the old digest unrecoverable
/// while the D-08 error's pull advice fetched the wrong bytes: a success signal
/// over the wrong artifact. Every published version's digest is recorded under
/// `image/digests/<version>`, and the build refuses to produce a digest that
/// disagrees with the recorded one for its version.
pub const BASE_IMAGE: &str = "ghcr.io/gnrain/nemr-base:0.3.0";

/// The version tag of [`BASE_IMAGE`] — the part after the last `:`.
///
/// The version now lives in exactly one place: the constant above. Every shell
/// consumer reads it through `scripts/lib/base_image.sh`, the probe binaries
/// take it by env var from the same helper, and the drift guard in
/// `check_base_image_versioning.sh` goes red if a `nemr-base:<digits>` literal
/// reappears in a script or probe — proven by leaving one consumer hardcoded
/// (F-124). A bump edits this file; everything moves.
///
/// This sentence was false for the first two weeks this function existed: it
/// described the world the function was written to create, while five files
/// hardcoded the version independently and this function had no callers — an
/// intention recorded as a fact (F-124's ledger note). It became true and the
/// comment changed in the SAME commit as the reality, which is the only way a
/// comment like this stays honest: prose has no mutation test.
pub fn base_image_version() -> &'static str {
    BASE_IMAGE.rsplit(':').next().unwrap_or(BASE_IMAGE)
}

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

/// The container's Claude Code home directory, `/root/.claude`. Since F-14 the
/// host's dedicated credential directory is bound HERE (a directory bind, rw),
/// not just the credential file: Claude Code rewrites `.credentials.json` by
/// writing a temp file and renaming it over the target, which a single-file
/// bind cannot follow. See [`crate::auth::host_credential_dir`].
pub const CONTAINER_CLAUDE_DIR: &str = "/root/.claude";

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
