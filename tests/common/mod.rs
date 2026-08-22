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

    if requirements.base_image && !base_image_present() {
        missing.push(format!(
            "base image {} not found in containerd\n     \
             fix: build and import it per README, \"Base image (Milestone 2)\"",
            nemr_engine::config::BASE_IMAGE
        ));
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

    // F-58: hashing installed-vs-built is not enough. Neither hash is tied to
    // the *source*, so "helper edited but never rebuilt" leaves both binaries
    // identically stale and the gate green — contradicting this gate's own
    // promise that a source change which was not reinstalled fails loudly.
    // Nothing else rebuilds the helper: only scripts/setup_test_host.sh does,
    // and the suite never runs it.
    //
    // So compare the built artifact against the source that produced it. A
    // source file newer than the binary means the binary is stale, whatever its
    // hash agrees with.
    if let Some((newest, mtime)) = newest_helper_source()? {
        let built_mtime = std::fs::metadata(&built)
            .and_then(|m| m.modified())
            .map_err(|e| format!("cannot stat {}: {e}", built.display()))?;
        if mtime > built_mtime {
            return Err(format!(
                "the helper's source is newer than its built binary:\n     \
                 {} is newer than {}\n     \
                 The installed helper cannot be the source under test.\n     \
                 fix: sudo ./scripts/setup_test_host.sh",
                newest.display(),
                built.display()
            ));
        }
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

/// Newest source file of the helper crate, with its mtime.
///
/// `deploy/nemr-volume` is its **own** workspace with its own lockfile, so the
/// outer workspace's manifests are deliberately not included: they do not
/// determine this binary, and counting them made every ordinary edit to the
/// engine report the helper as stale.
fn newest_helper_source() -> Result<Option<(PathBuf, std::time::SystemTime)>, String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("deploy/nemr-volume");
    Ok(newest_under(&[
        root.join("src"),
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
    ]))
}

/// Newest source file of the engine and everything it is built from.
///
/// `crates/` is included because the engine links them: a change in
/// `crates/nemr-containerd` changes the `nemr` binary just as surely as a change
/// in `src/`, and a gate watching only `src/` would call a stale install
/// current. The workspace manifests are included here because here they do
/// determine the binary.
fn newest_engine_source() -> Result<Option<(PathBuf, std::time::SystemTime)>, String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(newest_under(&[
        root.join("src"),
        root.join("crates"),
        root.join("Cargo.toml"),
        root.join("Cargo.lock"),
    ]))
}

/// Newest file among `paths`, recursing into directories.
///
/// Takes an explicit list rather than inferring a crate layout: each gate must
/// state exactly what determines its binary, because a gate that guesses too
/// widely goes red on unrelated edits and a gate that guesses too narrowly goes
/// green on a stale one. `target/` directories are skipped — build output is
/// newer than its own source by construction.
fn newest_under(paths: &[PathBuf]) -> Option<(PathBuf, std::time::SystemTime)> {
    let mut stack: Vec<PathBuf> = paths.iter().filter(|p| p.exists()).cloned().collect();
    let mut newest: Option<(PathBuf, std::time::SystemTime)> = None;
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&path) else {
                continue;
            };
            stack.extend(entries.flatten().map(|entry| entry.path()));
            continue;
        }
        let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(_, best)| mtime > *best) {
            newest = Some((path, mtime));
        }
    }
    newest
}

/// Where `nemr` is installed. Overridable so a developer with a different
/// prefix can still be gated rather than silently exempt.
pub fn installed_engine_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NEMR_INSTALLED_BIN") {
        return PathBuf::from(path);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/bin/nemr")
}

/// Path to the engine's release binary.
pub fn built_engine_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/nemr")
}

/// Is the installed `nemr` the binary this working tree builds? (F-62)
///
/// The helper has had this gate since F-58; the engine has not, and the engine
/// is the binary a user actually runs. Milestone closure here means "merged
/// **and** reinstalled from that commit **and** verified against the installed
/// artifacts" — a rule nothing enforced. `scripts/e2e_smoke_test.sh` invokes
/// whatever `nemr` is on PATH, so a smoke test could pass against a binary
/// built from a commit that no longer exists and report the milestone closed.
///
/// Two checks, for the two ways staleness happens:
///
///   1. installed hash != built hash — built but never installed;
///   2. a source file newer than the installed binary — edited and never built,
///      which leaves both binaries identically stale and hash-equal (the exact
///      hole F-58 found in the helper's gate).
///
/// The hash comparison requires the install to be a **copy of
/// `target/release/nemr`**, not a `cargo install` — `cargo install` rebuilds in
/// its own target directory and produces a different (larger) binary from
/// identical source, so hashes would never agree. See README.
pub fn installed_engine_matches_built() -> Result<(), String> {
    let installed = installed_engine_path();
    let built = built_engine_path();

    if !installed.exists() {
        return Err(format!(
            "nemr is not installed at {}\n     \
             fix: ./scripts/install_engine.sh",
            installed.display()
        ));
    }
    if !built.exists() {
        return Err(format!(
            "the engine's release binary is not built at {}\n     \
             fix: ./scripts/install_engine.sh",
            built.display()
        ));
    }

    if let Some((newest, mtime)) = newest_engine_source()? {
        let installed_mtime = std::fs::metadata(&installed)
            .and_then(|m| m.modified())
            .map_err(|e| format!("cannot stat {}: {e}", installed.display()))?;
        if mtime > installed_mtime {
            return Err(format!(
                "the engine's source is newer than the installed binary:\n     \
                 {} is newer than {}\n     \
                 The installed nemr cannot be the source under test.\n     \
                 fix: ./scripts/install_engine.sh",
                newest.display(),
                installed.display()
            ));
        }
    }

    let installed_hash = sha256_of(&installed)
        .map_err(|e| format!("cannot hash the installed engine {}: {e}", installed.display()))?;
    let built_hash = sha256_of(&built)
        .map_err(|e| format!("cannot hash the built engine {}: {e}", built.display()))?;
    if installed_hash != built_hash {
        return Err(format!(
            "the installed nemr does not match the built source:\n     \
             installed {} = {installed_hash}\n     \
             built     {} = {built_hash}\n     \
             fix: ./scripts/install_engine.sh",
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

/// Extract a bundle to a scratch directory and return every file's contents
/// concatenated, for content assertions.
///
/// # Why not grep the bundle file
///
/// F-57: a bundle's chunks are zstd-compressed, so searching the `.nemr` bytes
/// finds a plaintext string only when the content was small enough that zstd
/// stored it near-verbatim. Measured: a marker in a 34-byte bundle is findable
/// in the raw file; the same marker in a 241 KiB bundle is not. Every
/// "the secret must not appear in the bundle" test written that way therefore
/// passes on small fixtures and stops guarding anything at realistic sizes —
/// it degrades precisely when it matters.
///
/// Assertions about what a bundle does or does not contain must run against the
/// extracted plaintext, which is also what an importer or an attacker actually
/// sees.
pub fn extracted_plaintext(bundle_path: &Path) -> String {
    let bundle = nemr_engine::bundle::import::open(bundle_path)
        .unwrap_or_else(|e| panic!("open bundle {}: {e}", bundle_path.display()));
    let dir = std::env::temp_dir().join(format!(
        "nemr-extract-{}-{}",
        std::process::id(),
        unique_name("x")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    bundle.extract(&dir).expect("extract bundle");

    let mut combined = String::new();
    let mut stack = vec![dir.clone()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                combined.push_str(&text);
                combined.push('\n');
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    combined
}

/// Write a bundle whose single member has an arbitrary (possibly hostile) path.
///
/// `export()` cannot produce a traversing member path — which is precisely why a
/// traversal regression test needs a crafted fixture rather than a unit test on
/// the path-joining helper alone (F-58).
pub fn write_hostile_bundle(destination: &Path, member_path: &str, payload: &[u8]) {
    use sha2::{Digest, Sha256};
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();

    let manifest = serde_json::json!({
        "schema_version": 1,
        "engine_version": "0.1.0",
        "created_at": "0",
        "project": { "name": "hostile", "quota": "500MB", "content_bytes": payload.len() },
        "base_image": {
            "reference": nemr_engine::config::BASE_IMAGE,
            "digest": local_base_image_digest().unwrap_or_default(),
        },
        "chunks": [{
            "index": 0,
            "sha256": hex(&Sha256::digest(payload)),
            "compressed_bytes": 0,
            "plain_bytes": payload.len(),
        }],
        "members": [{
            "path": member_path,
            "class": "session-critical",
            "mode": 33188,
            "size": payload.len(),
            "sha256": hex(&Sha256::digest(payload)),
            "span": { "offset": 0, "length": payload.len() },
        }],
        "excluded": [],
    });

    let file = std::fs::File::create(destination).expect("create hostile bundle");
    let mut builder = tar::Builder::new(file);
    let append = |builder: &mut tar::Builder<std::fs::File>, name: &str, bytes: &[u8]| {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        builder.append_data(&mut header, name, bytes).unwrap();
    };
    append(&mut builder, "manifest.json", &serde_json::to_vec(&manifest).unwrap());
    append(&mut builder, "chunks/0000.zst", &zstd::encode_all(payload, 3).unwrap());
    builder.finish().expect("finish hostile bundle");
}

/// The base image digest on this host, so a hostile bundle passes the base-image
/// check and reaches the extraction path under test.
pub fn local_base_image_digest() -> Option<String> {
    // On a dedicated thread: callers are already inside a runtime, and nesting
    // `Runtime::new().block_on` inside one panics.
    std::thread::spawn(|| {
        let runtime = tokio::runtime::Runtime::new().ok()?;
        runtime.block_on(async {
            let client = ContainerdClient::connect().await.ok()?;
            client
                .image_target_digest(nemr_engine::config::BASE_IMAGE)
                .await
                .ok()
        })
    })
    .join()
    .ok()
    .flatten()
}

/// Whether unprivileged user, mount and network namespaces are usable here.
///
/// Reported rather than silently skipped: a host without them cannot verify
/// E-11's offline guarantee, and that is a coverage gap to surface.
pub fn namespaces_available() -> bool {
    Command::new("unshare")
        .args(["-rmn", "true"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run the installed `nemr` CLI with **no network** and **no credential**.
///
/// A network namespace with only loopback makes any outbound request fail; a
/// mount namespace with a tmpfs over `~/.claude` hides the credential *inside
/// the namespace only*, so the real one is never moved or modified. containerd
/// stays reachable through its local Unix socket, which is the intended
/// reading of "no network": no internet and no sync service, not no IPC.
pub fn run_offline(args: &[&str]) -> std::process::Output {
    let home = std::env::var("HOME").expect("HOME");
    let socket = ContainerdClient::default_socket_path()
        .expect("containerd socket path")
        .to_string_lossy()
        .into_owned();
    // CARGO_BIN_EXE_ rather than a hardcoded target/debug path: cargo guarantees
    // this points at the binary built for *this* test run. The hardcoded path
    // was correct only by coincidence — under `cargo test --release` the fresh
    // binary lands in target/release and the debug one goes stale, so the
    // offline tests would have gone green against a binary from an older commit.
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_nemr"));
    let quoted: Vec<String> = args.iter().map(|a| format!("'{a}'")).collect();

    Command::new("unshare")
        .args(["-rmn", "bash", "-c"])
        .arg(format!(
            "mount -t tmpfs none '{home}/.claude' && \
             CONTAINERD_ADDRESS='{socket}' '{}' {}",
            binary.display(),
            quoted.join(" ")
        ))
        .output()
        .expect("run nemr offline")
}
