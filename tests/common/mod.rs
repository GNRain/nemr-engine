//! Shared harness for the regression suite.
//!
//! # Why these tests refuse rather than skip
//!
//! Every test in `tests/` guards a defect that already reached a user once.
//! A test that quietly skips when its prerequisites are absent is worse than
//! no test at all: the suite reports green, nobody notices coverage vanished,
//! and the defect walks back in. So [`require_host`] **fails** with an
//! actionable message instead of skipping.
//!
//! The escape hatch is explicit and narrow: `NEMR_TEST_UNIT_ONLY=1` opts out
//! of host-backed tests for a developer without a provisioned machine. CI does
//! not set it, so CI always runs the full suite (see `.github/workflows/ci.yml`).
//! Making the opt-out an environment variable a human has to type — rather than
//! an `#[ignore]` attribute that silently applies to everyone — is the whole
//! point: `#[ignore]` is how the VOL-06 regression suite came to exist without
//! ever running.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use nemr_engine::containerd::client::ContainerdClient;
use nemr_engine::engine::volume::{HelperOps, PrivilegedOps, VolumePaths};

/// Host facilities a regression test needs.
pub struct HostRequirements {
    /// Needs rootless containerd answering on its socket.
    pub containerd: bool,
    /// Needs the root-owned privileged helper plus its sudoers grant.
    pub helper: bool,
    /// Needs the base image present in containerd's content store.
    pub base_image: bool,
}

impl HostRequirements {
    /// Everything a full project lifecycle needs.
    pub const FULL: Self = Self {
        containerd: true,
        helper: true,
        base_image: true,
    };

    /// Volume-only tests: loop devices and mounts, no containers.
    pub const VOLUME: Self = Self {
        containerd: false,
        helper: true,
        base_image: false,
    };
}

/// Whether the developer explicitly opted out of host-backed tests.
pub fn unit_only() -> bool {
    std::env::var_os("NEMR_TEST_UNIT_ONLY").is_some()
}

/// Assert the host can run this test, or fail with instructions.
///
/// Returns `false` when the caller should return early because the developer
/// opted out via `NEMR_TEST_UNIT_ONLY`. Otherwise it either returns `true` or
/// panics with a message that says exactly what is missing and how to fix it.
pub fn require_host(requirements: HostRequirements) -> bool {
    if unit_only() {
        eprintln!(
            "NEMR_TEST_UNIT_ONLY is set: skipping a host-backed regression test. \
             CI does not set this, and a green run with it set proves nothing about \
             the defects these tests guard."
        );
        return false;
    }

    let mut missing = Vec::new();

    if requirements.containerd {
        match ContainerdClient::default_socket_path() {
            Ok(path) if path.exists() => {}
            Ok(path) => missing.push(format!(
                "rootless containerd socket not found at {}\n     \
                 fix: systemctl --user start containerd-rootless.service",
                path.display()
            )),
            Err(error) => missing.push(format!("cannot locate the containerd socket: {error:#}")),
        }
    }

    if requirements.helper {
        let helper = PathBuf::from(HelperOps::DEFAULT_HELPER);
        if !helper.exists() {
            missing.push(format!(
                "privileged helper not installed at {}\n     \
                 fix: sudo ./scripts/setup_test_host.sh",
                helper.display()
            ));
        } else if !sudo_grant_works() {
            missing.push(format!(
                "cannot invoke {} via `sudo -n`\n     \
                 fix: install deploy/sudoers.d/nemr-volume (see ./scripts/setup_test_host.sh)",
                helper.display()
            ));
        } else if let Err(reason) = installed_helper_matches_built() {
            // TEST-01: a host-backed test must exercise the *installed* helper,
            // not a locally-built one that was never deployed. If they differ,
            // the deployed artifact is not what this suite is proving anything
            // about — the exact gap that let a helper which could not provision
            // a single volume pass 13 unit tests. Refuse to run rather than
            // report a green result against a stale binary.
            missing.push(reason);
        }
    }

    if requirements.base_image {
        if !base_image_present() {
            missing.push(format!(
                "base image {} not found in containerd\n     \
                 fix: build and import it per README, \"Base image (Milestone 2)\"",
                nemr_engine::config::BASE_IMAGE
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "\n\nThis regression test needs host facilities that are not present:\n\n  - {}\n\n\
         These tests are deliberately not `#[ignore]`d: each one guards a defect that already \n\
         reached a user, and a silently-skipped guard is how the VOL-06 regression suite came \n\
         to exist without ever running. Provision the host, or set NEMR_TEST_UNIT_ONLY=1 to \n\
         run only the tests that need nothing (CI never sets it).\n",
        missing.join("\n  - ")
    );

    true
}

/// Does the NOPASSWD grant actually work right now?
///
/// A bare invocation prints usage and exits 1, so "it ran at all" is the
/// signal, not a zero exit status. `sudo -n` never prompts, so a missing grant
/// fails immediately rather than blocking the suite on a password prompt.
fn sudo_grant_works() -> bool {
    Command::new("sudo")
        .args(["-n", HelperOps::DEFAULT_HELPER])
        .output()
        .map(|output| {
            let text = String::from_utf8_lossy(&output.stderr).to_lowercase();
            text.contains("usage")
        })
        .unwrap_or(false)
}

/// Whether the installed helper is byte-identical to the crate's built release
/// artifact.
///
/// This is the enforcement behind "the suite passes against the *installed*
/// helper, verified by hash". A stale installed helper — source changed but
/// `setup_test_host.sh` not re-run — makes every host-backed assertion a
/// statement about a binary nobody is running.
pub fn installed_helper_matches_built() -> Result<(), String> {
    let installed = PathBuf::from(HelperOps::DEFAULT_HELPER);
    let built = built_helper_path();

    if !built.exists() {
        return Err(format!(
            "the helper's release binary is not built at {}\n     \
             fix: (cd deploy/nemr-volume && cargo build --release) then sudo ./scripts/setup_test_host.sh",
            built.display()
        ));
    }

    let installed_hash = sha256_of(&installed)
        .map_err(|e| format!("cannot hash the installed helper {}: {e}", installed.display()))?;
    let built_hash = sha256_of(&built)
        .map_err(|e| format!("cannot hash the built helper {}: {e}", built.display()))?;

    if installed_hash != built_hash {
        return Err(format!(
            "the installed helper does not match the built source:\n     \
             installed {} = {installed_hash}\n     \
             built     {} = {built_hash}\n     \
             fix: sudo ./scripts/setup_test_host.sh",
            installed.display(),
            built.display()
        ));
    }
    Ok(())
}

/// Path to the helper crate's release binary, relative to this crate's root.
pub fn built_helper_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("deploy/nemr-volume/target/release/nemr-volume")
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

fn base_image_present() -> bool {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return false;
    };
    runtime.block_on(async {
        let Ok(client) = ContainerdClient::connect().await else {
            return false;
        };
        client
            .list_images()
            .await
            .map(|images| {
                images
                    .iter()
                    .any(|image| image.name == nemr_engine::config::BASE_IMAGE)
            })
            .unwrap_or(false)
    })
}

/// A unique project name for one test.
///
/// Carries the process id and a per-process counter, so concurrent test
/// binaries cannot collide and a leaked project is traceable to the run that
/// made it. Kept within the 32-character limit `validate_name` enforces.
pub fn unique_name(prefix: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{n}", std::process::id())
}

/// A project that deletes itself when the test ends, however it ends.
///
/// Without this a failing assertion leaves a loop device and a mount behind,
/// and the next run inherits them — which is exactly the class of cross-run
/// contamination the smoke test was criticised for.
pub struct TestProject {
    pub name: String,
}

impl TestProject {
    /// Create a project, or fail the test with the underlying error.
    pub async fn create(
        client: &ContainerdClient,
        prefix: &str,
        size: nemr_engine::engine::volume::VolumeSize,
    ) -> Self {
        let name = unique_name(prefix);
        // Clear any residue from an earlier interrupted run before creating.
        purge(&name);

        nemr_engine::engine::project::create(client, &name, size)
            .await
            .unwrap_or_else(|error| panic!("failed to create test project {name:?}: {error:#}"));

        Self { name }
    }

    pub fn mount_point(&self) -> PathBuf {
        VolumePaths::from_env()
            .expect("HOME must be set")
            .mount_point(&self.name)
    }

    pub fn image_file(&self) -> PathBuf {
        VolumePaths::from_env()
            .expect("HOME must be set")
            .image_file(&self.name)
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        purge(&self.name);
    }
}

/// Remove every trace of a project, tolerating any of it already being gone.
///
/// Deliberately does not go through `project::delete`: this runs on the
/// failure path, where the engine's own delete may be the thing under test or
/// may itself be broken. It goes straight at the host.
pub fn purge(name: &str) {
    // Run the containerd cleanup on a dedicated thread with its own runtime.
    // `purge` is called from inside `TestProject::create`, which is itself
    // driven by a runtime, and nesting `Runtime::new().block_on` inside a live
    // runtime panics ("Cannot start a runtime from within a runtime").
    let owned = name.to_string();
    let _ = std::thread::spawn(move || {
        if let Ok(runtime) = tokio::runtime::Runtime::new() {
            runtime.block_on(async {
                if let Ok(client) = ContainerdClient::connect().await {
                    let id = nemr_engine::config::container_id(&owned);
                    let _ = client.stop_task(&id).await;
                    let _ = client.delete_container(&id).await;
                }
            });
        }
    })
    .join();

    let _ = HelperOps::new().unmount_and_detach(name);

    if let Ok(paths) = VolumePaths::from_env() {
        let _ = std::fs::remove_file(paths.image_file(name));
        let _ = std::fs::remove_dir(paths.mount_point(name));
    }
}
