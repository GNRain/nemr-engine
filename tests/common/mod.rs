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
use nemr_engine::engine::volume::{self, HelperOps, PrivilegedOps, VolumePaths};

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

/// Reclaim volumes left by a previous run that was killed (F-77).
///
/// `TestProject`'s `Drop` cleans up however a test ends — **except** when the
/// process is killed outright, and an interrupted arm of a reproduction
/// experiment is exactly that. Each survivor is a fully-allocated 500 MB image
/// plus an attached loop device, and once the image is unlinked the kernel
/// keeps the inode alive, so the space cannot be reclaimed by deleting files.
/// About 140 accumulated on the reference host and filled the disk — which then
/// took out the ability to diagnose the disk filling.
///
/// So the suite bounds its own residue to one run's worth: before any
/// host-backed test, release what an earlier run left.
///
/// **Precision matters more than thoroughness here.** Test projects are named
/// `<prefix>-<pid>-<n>`. This only touches names of that shape whose pid is no
/// longer running — so a developer's real project (`htmltest`, `myproject`)
/// cannot match, and neither can a live parallel run's volumes.
fn sweep_dead_test_volumes() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Ok(paths) = VolumePaths::from_env() else {
            return;
        };
        let mut names = std::collections::BTreeSet::new();
        for dir in [paths.image_dir(), paths.mount_dir()] {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let raw = entry.file_name().to_string_lossy().into_owned();
                names.insert(raw.strip_suffix(".img").unwrap_or(&raw).to_string());
            }
        }

        let helper = HelperOps::new();
        let mut reclaimed = 0;
        for name in names {
            let Some(pid) = dead_test_project_pid(&name) else {
                continue;
            };
            let _ = pid;
            if let Err(error) = helper.unmount_and_detach(&name) {
                eprintln!("[nemr:test-sweep] could not release {name:?}: {error:#}");
                continue;
            }
            // F-79: verify the release BEFORE deleting the evidence of it.
            //
            // This previously deleted the image and the mount point on the
            // strength of the call returning Ok. When the helper could not
            // detach a loop device whose backing file was gone (F-77), that
            // removed the only two things `reconcile` enumerates — the image
            // file and the mount-point directory — leaving the device attached
            // and unfindable. 57 accumulated that way, holding 24 GB, with
            // `nemr reconcile` reporting "nothing to reconcile".
            //
            // The cleanup hid the mess it failed to clean. Assume-success in a
            // sweep is worse than assume-success anywhere else, because the
            // assumption destroys the trail.
            if let Some(device) = volume::attached_loop_device(&paths.image_file(&name)) {
                eprintln!(
                    "[nemr:test-sweep] {name:?} is still attached to /dev/loop{device} after \
                     release; leaving its image and mount point in place so `nemr reconcile` \
                     can still find it"
                );
                continue;
            }
            let _ = std::fs::remove_file(paths.image_file(&name));
            let _ = std::fs::remove_dir(paths.mount_point(&name));
            reclaimed += 1;
        }
        if reclaimed > 0 {
            eprintln!(
                "[nemr:test-sweep] reclaimed {reclaimed} volume(s) left by a killed run. \
                 TestProject::drop does not run under SIGKILL, so this bounds residue to \
                 one run's worth instead of letting it accumulate until the disk fills."
            );
        }
    });
}

/// The pid embedded in a `<prefix>-<pid>-<n>` test project name, if that pid is
/// no longer running.
///
/// Returns `None` for any name that is not of that shape — which is what keeps
/// a real project out of the sweep — and for a pid that is still alive, which
/// keeps a concurrently running suite's volumes out of it.
pub fn dead_test_project_pid(name: &str) -> Option<u32> {
    let mut parts = name.rsplitn(3, '-');
    let _counter: u32 = parts.next()?.parse().ok()?;
    let pid: u32 = parts.next()?.parse().ok()?;
    let prefix = parts.next()?;
    if prefix.is_empty() {
        return None;
    }
    if std::path::Path::new(&format!("/proc/{pid}")).exists() {
        return None;
    }
    Some(pid)
}

/// Refuse to run host-backed tests concurrently (F-71).
///
/// This module's own documentation has always said `--test-threads=1` is
/// **required**: these tests attach loop devices, mount filesystems, start
/// containers and drive containerd's garbage collector, all of which are global
/// host state. Nothing enforced it. `cargo test --test regression` — the obvious
/// invocation — runs them across `nproc` threads and produces failures that
/// belong to the harness rather than the engine.
///
/// That cost real time: F-63 was first observed in a parallel run, and "you ran
/// it in an unsupported mode" was a live explanation for it until a serial arm
/// ruled it out. A requirement stated only in prose is the same defect class as
/// a CI job named for a check it never runs (F-64) or a suite that reports ok
/// having run nothing (F-60): the rule exists, and nothing applies it.
///
/// `NEMR_TEST_ALLOW_PARALLEL=1` is a deliberate opt-out for concurrency
/// experiments — the loaded-parallel arms used to characterise F-63 need it. It
/// prints what it means, because a failure in that mode is not a defect unless
/// it also reproduces serially.
fn require_serial_execution() {
    if std::env::var_os("NEMR_TEST_ALLOW_PARALLEL").is_some() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            eprintln!(
                "NEMR_TEST_ALLOW_PARALLEL is set: host-backed tests may run concurrently. \
                 These tests share global host state, so a failure here is NOT a defect \
                 unless it also reproduces under --test-threads=1."
            );
        });
        return;
    }

    // libtest passes its own arguments through to the test binary, so the
    // binary's argv is an exact record of how it was invoked — no guessing.
    let serial_flag = std::env::args().any(|arg| arg == "--test-threads=1")
        || std::env::args()
            .zip(std::env::args().skip(1))
            .any(|(flag, value)| flag == "--test-threads" && value == "1");
    let serial_env = std::env::var("RUST_TEST_THREADS").is_ok_and(|value| value == "1");

    assert!(
        serial_flag || serial_env,
        "\n\nThis suite must run serially, and nothing was asserting it until F-71.\n\n\
         These tests attach loop devices, mount filesystems, start containers and drive \n\
         containerd's garbage collector — all global host state. Run in parallel they \n\
         produce failures that belong to the harness rather than the engine, which is \n\
         exactly how F-63 nearly got dismissed as a threading artifact.\n\n  \
         fix: cargo test --test regression -- --test-threads=1\n\n  \
         For a deliberate concurrency experiment, set NEMR_TEST_ALLOW_PARALLEL=1 and read \n  \
         its warning: a failure in that mode is not a defect unless it also reproduces \n  \
         serially.\n"
    );
}

/// Whether the developer explicitly opted out of host-backed tests.
///
/// ASKING IS ANNOUNCING. Every caller in this suite asks in order to skip, and
/// 28 of them returned early without ever reaching `require_host`'s message —
/// so they skipped in complete silence and cargo reported them as passed. The
/// announcement therefore lives here, at the question, not at one of the two
/// places that happened to answer it (the gate audit, 2026-09-09).
#[track_caller]
pub fn unit_only() -> bool {
    if std::env::var_os("NEMR_TEST_UNIT_ONLY").is_some() {
        announce_unit_only_skip();
        return true;
    }
    false
}

/// The same question without the announcement, for code that is deciding
/// something other than whether to skip.
pub fn unit_only_quiet() -> bool {
    std::env::var_os("NEMR_TEST_UNIT_ONLY").is_some()
}

/// Where a unit-only run records what it did not run.
///
/// A harness reads this instead of grepping the suite's output, which only
/// carries the skip lines under `--nocapture`.
pub fn unit_only_ledger() -> PathBuf {
    std::env::var_os("NEMR_TEST_SKIP_LEDGER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("unit-only-skips.txt"))
}

/// Announce a skipped test so it cannot be mistaken for a passed one.
///
/// TWO channels, deliberately.
///
/// 1. **Straight to the stderr file descriptor**, not `eprintln!`. libtest
///    captures the `print!`/`eprintln!` macros for a test that passes, and a
///    test that returns early *does* pass — so the old message was invisible in
///    every run that did not pass `--nocapture`, which is how 33 of 76
///    regression tests could quietly not run (the gate audit, 2026-09-09). A
///    direct `writeln!` on the handle bypasses the capture and always prints.
/// 2. **A ledger file**, appended once per skip, so a harness can count and
///    report them without parsing test output at all.
#[track_caller]
fn announce_unit_only_skip() {
    use std::io::Write;
    let at = std::panic::Location::caller();
    // Formatted first, then ONE write_all: `writeln!` straight at the handle
    // emits several write() calls, and parallel tests interleave them into
    // unreadable half-lines. The same reason the ledger below is written once.
    let line = format!(
        "\nNEMR-SKIP {at}: host-backed test NOT RUN (NEMR_TEST_UNIT_ONLY). \
         It is reported as passed by cargo; it checked nothing.\n"
    );
    let _ = std::io::stderr().write_all(line.as_bytes());
    let ledger = unit_only_ledger();
    if let Some(dir) = ledger.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Truncated once per process, then appended to: the file describes THIS run.
    static FRESH: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    FRESH.get_or_init(|| {
        let _ = std::fs::write(&ledger, "");
    });
    // ONE write_all of one buffer: tests run in parallel by default, and a
    // `writeln!` that formats into the file handle can issue several write()
    // calls, which interleave into corrupted lines. Measured: they did.
    let entry = format!("{at}\n");
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&ledger) {
        let _ = f.write_all(entry.as_bytes());
    }
}

/// Assert the host can run this test, or fail with instructions.
///
/// Returns `false` when the caller should return early because the developer
/// opted out via `NEMR_TEST_UNIT_ONLY`. Otherwise it either returns `true` or
/// panics with a message that says exactly what is missing and how to fix it.
#[track_caller]
pub fn require_host(requirements: HostRequirements) -> bool {
    if unit_only_quiet() {
        announce_unit_only_skip();
        return false;
    }

    // After the unit-only return, deliberately: the serial requirement exists
    // because these tests share global host state, and a unit-only run touches
    // none of it. CI's unit job runs without the flag and is right to.
    require_serial_execution();
    sweep_dead_test_volumes();

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

    let installed_hash = sha256_of(&installed).map_err(|e| {
        format!(
            "cannot hash the installed helper {}: {e}",
            installed.display()
        )
    })?;
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

/// Newest source file of the engine and everything it is built from — and
/// nothing else (F-90).
///
/// The list is the engine's actual dependency closure, not the workspace:
///
/// - `src/`, `proto/` — the engine crate itself, and the daemon's public
///   interface. Before F-90 neither `proto/` nor the build script was scanned
///   and a proto edit left the gate green; the build script has since moved
///   into `crates/nemr-daemon-api/`, which is scanned below.
/// - `crates/nemr-containerd/` and `crates/nemr-daemon-api/` — the two
///   workspace crates the engine links (the wrapper, and the daemon API with
///   its proto build).
///   Before F-90 this was `crates/` wholesale, which swept the COMMERCIAL
///   crates the engine must never depend on (`check_seam.sh` proves it does
///   not), so editing a sync-server test reported the engine stale. A gate
///   that cries wolf teaches people to reinstall past it without reading, and
///   then it stops working on the day the drift is real. The helper's gate
///   learned this same lesson from the other direction — see
///   `newest_helper_source`.
/// - `Cargo.toml` — the engine's own manifest (it is the workspace root
///   package), where its dependency versions live.
///
/// `Cargo.lock` is deliberately **excluded**: the lockfile is workspace-wide,
/// so a commercial crate adding a dependency touches it and would re-open the
/// same false positive. The residual gap — a bare `cargo update` that changes
/// only the lock — is accepted and named: the next `install_engine.sh` still
/// rebuilds from the updated lock and the hash gate re-arms.
fn newest_engine_source() -> Result<Option<(PathBuf, std::time::SystemTime)>, String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(newest_under(&[
        root.join("src"),
        root.join("proto"),
        root.join("crates/nemr-containerd"),
        root.join("crates/nemr-daemon-api"),
        root.join("Cargo.toml"),
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
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
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

    let installed_hash = sha256_of(&installed).map_err(|e| {
        format!(
            "cannot hash the installed engine {}: {e}",
            installed.display()
        )
    })?;
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

    // The daemon is installed beside the CLI by install_engine.sh and every
    // command runs through it — an unverified nemrd is a bigger hole than an
    // unverified nemr, yet until F-90 nothing hashed it. Same rule, same fix.
    let installed_daemon = installed
        .parent()
        .map(|dir| dir.join("nemrd"))
        .unwrap_or_else(|| PathBuf::from("nemrd"));
    let built_daemon = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/nemrd");
    if !installed_daemon.exists() || !built_daemon.exists() {
        return Err(format!(
            "the daemon is not installed/built ({} / {})\n     \
             fix: ./scripts/install_engine.sh",
            installed_daemon.display(),
            built_daemon.display()
        ));
    }
    let installed_daemon_hash = sha256_of(&installed_daemon)
        .map_err(|e| format!("cannot hash {}: {e}", installed_daemon.display()))?;
    let built_daemon_hash = sha256_of(&built_daemon)
        .map_err(|e| format!("cannot hash {}: {e}", built_daemon.display()))?;
    if installed_daemon_hash != built_daemon_hash {
        return Err(format!(
            "the installed nemrd does not match the built source:\n     \
             installed {} = {installed_daemon_hash}\n     \
             built     {} = {built_daemon_hash}\n     \
             fix: ./scripts/install_engine.sh",
            installed_daemon.display(),
            built_daemon.display()
        ));
    }
    Ok(())
}

/// Path to the helper crate's release binary, relative to this crate's root.
pub fn built_helper_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("deploy/nemr-volume/target/release/nemr-volume")
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// Ensure a published base-image `reference` is present in containerd, pulling
/// it if not — so a test that needs a prior version as a fixture provisions its
/// own, rather than depending on which host script happened to run last.
///
/// `setup_host.sh` pulls only the current version; `ci_provision_host.sh` also
/// pulls a prior one. The F-115 export test needs the prior one as a divergent
/// subject, and on a host provisioned by `setup_host.sh` it panicked on exactly
/// that gap (the WSL2 spike's f115 failure). A test's fixture is the test's
/// responsibility, not the provisioner's.
///
/// nemr itself never pulls (D-08 part 1); this is TEST provisioning, the same
/// `ctr images pull` `ci_provision_host.sh` runs, which is why it shells to
/// `ctr` rather than asking the engine client for something it refuses to do.
/// F-85 guarantees a published tag names one set of bytes for ever, so the
/// pulled fixture is the recorded one, not "whatever the registry has today".
pub fn ensure_base_image_present(reference: &str) {
    // Already there? Read, don't repair — the same presence check the suite
    // uses for the current image.
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let present = runtime.block_on(async {
        let Ok(client) = ContainerdClient::connect().await else {
            return false;
        };
        client
            .list_images()
            .await
            .map(|images| images.iter().any(|image| image.name == reference))
            .unwrap_or(false)
    });
    if present {
        return;
    }

    eprintln!("    fixture {reference} is not in containerd; pulling it (test provisioning)");
    let status = Command::new("ctr")
        .args([
            "-n",
            "default",
            "images",
            "pull",
            "--platform",
            "linux/amd64",
            reference,
        ])
        .status()
        .unwrap_or_else(|e| panic!("could not run `ctr` to pull the fixture {reference}: {e}"));
    assert!(
        status.success(),
        "failed to pull the base-image fixture {reference}. It is needed as a divergent \
         subject (F-115/F-116) and setup_host.sh pulls only the current version. \
         Manually: ctr -n default images pull --platform linux/amd64 {reference}"
    );
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

        nemr_engine::engine::project::create(
            client,
            &name,
            size,
            nemr_engine::engine::agent::Agent::default_agent(),
        )
        .await
        .unwrap_or_else(|error| panic!("failed to create test project {name:?}: {error:#}"));

        Self { name }
    }

    /// Create a test project running a specific agent (E-15).
    pub async fn create_with_agent(
        client: &ContainerdClient,
        prefix: &str,
        size: nemr_engine::engine::volume::VolumeSize,
        agent: nemr_engine::engine::agent::Agent,
    ) -> Self {
        let name = unique_name(prefix);
        purge(&name);
        nemr_engine::engine::project::create(client, &name, size, agent)
            .await
            .unwrap_or_else(|error| panic!("failed to create test project {name:?}: {error:#}"));
        Self { name }
    }
}

impl TestProject {
    /// Take ownership of a project this test did not create, so it is still
    /// cleaned up when the test ends.
    ///
    /// Needed where the *engine* chooses the name — a restore takes it from the
    /// bundle — and the test must still guarantee the volume is released
    /// however the test exits.
    pub fn adopt(name: String) -> Self {
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
/// Projects no automated path may EVER delete (F-123). Mirrors
/// `NEMR_PROTECTED_SUBJECTS` in scripts/lib/proc.sh — one rule, two enforcement
/// points, and a test asserts the lists agree so they cannot drift.
///
/// "Experiments use disposable subjects" was a convention, and convention
/// failed: a teardown check counted a running project's link as a leak, and
/// the cleanup acted on a hardcoded echo printed beneath a task listing that
/// said RUNNING — against the one project designated irreplaceable. A
/// protected subject must not depend on every future check being correct.
// 2026-09-06: `htmltest` retired — the artifact is gone (deleted deliberately
// to reclaim disk; no bundle survives on this host). Empty until something
// irreplaceable exists again; the guard and its own test stay armed.
pub const PROTECTED_SUBJECTS: &[&str] = &[];

/// Is this name protected from automated teardown?
///
/// `NEMR_TEST_EXTRA_PROTECTED` adds one name for the guard's own test: the
/// refusal cannot be proven against the REAL protected subject — proving
/// destructiveness against the thing being protected is the incident again —
/// so the test protects a disposable name and shows purge spares it.
pub fn is_protected(name: &str) -> bool {
    if PROTECTED_SUBJECTS.contains(&name) {
        return true;
    }
    std::env::var("NEMR_TEST_EXTRA_PROTECTED").is_ok_and(|extra| extra == name)
}

pub fn purge(name: &str) {
    // The guard runs FIRST, before any handle to containerd or the helper is
    // even acquired. Loud, not silent: a purge that reaches this line has a
    // bug upstream — it computed a protected name where a disposable one
    // belongs — and hiding that would defer the bug to a worse moment. Not a
    // panic, because purge runs in Drop and a panic during unwind aborts the
    // whole test binary, taking every other test's cleanup with it.
    if is_protected(name) {
        eprintln!(
            "[nemr:test] REFUSED to purge {name:?}: protected subject (F-123). \
             The calling test has a bug — it passed a protected name to a teardown path."
        );
        return;
    }

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

                    // NET-02: release the session network before the record
                    // that names it. Going straight at the host is deliberate
                    // (above), but the veth and the NAT rule are host state
                    // too, and skipping them leaked one of each per purged
                    // test — which then failed the NET-02 acceptance's teardown
                    // check for reasons that had nothing to do with the code
                    // under test. Read the label first: after
                    // `delete_container` there is nothing left to read it from.
                    let alloc = client
                        .list_containers()
                        .await
                        .ok()
                        .and_then(|cs| cs.into_iter().find(|c| c.id == id))
                        .and_then(|c| {
                            nemr_engine::engine::project::allocation_from_labels(&c.labels)
                        });

                    let _ = client.stop_task(&id).await;
                    let _ = client.delete_container(&id).await;

                    if let Some(alloc) = alloc {
                        nemr_engine::engine::netns::disconnect_session(alloc);
                    }
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
    append(
        &mut builder,
        "manifest.json",
        &serde_json::to_vec(&manifest).unwrap(),
    );
    append(
        &mut builder,
        "chunks/0000.zst",
        &zstd::encode_all(payload, 3).unwrap(),
    );
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

/// Install a tracing subscriber for the regression suite, once per process.
///
/// WP B added structured lifecycle logging, but nothing in `tests/` ever
/// initialised a subscriber — that happens in `nemr`'s `main`. So every
/// `tracing::debug!` the engine emits was discarded in exactly the place
/// failures get diagnosed: `NEMR_LOG=... cargo test` produced no engine logs at
/// all. Found while diagnosing F-63, where `ensure_volume_mounted`'s
/// decision-point log was the fact needed and was not there.
///
/// Off unless `NEMR_LOG` is set, so ordinary runs are unchanged.
pub fn init_tracing() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Ok(filter) = std::env::var("NEMR_LOG") else {
            return;
        };
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .with_test_writer()
            .with_target(true)
            .try_init();
    });
}

/// Everything needed to tell "the volume was not mounted" apart from "the
/// volume was mounted but the directory was missing".
///
/// F-63 turned on exactly that distinction and the failure carried neither
/// fact: the panic said `.nemr-state/projects: no such file or directory` and
/// left open whether the mount had silently not happened (a VOL-05 violation,
/// severe) or the volume was mounted and genuinely lacked the directory (a test
/// artifact). Captured at the moment of failure, not reconstructed afterwards.
pub fn volume_state_report(name: &str) -> String {
    let Ok(paths) = VolumePaths::from_env() else {
        return "  <cannot resolve VolumePaths>".into();
    };
    let mount_point = paths.mount_point(name);
    let image = paths.image_file(name);

    let listing = match std::fs::read_dir(&mount_point) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            if names.is_empty() {
                "<empty>".to_string()
            } else {
                names.join(", ")
            }
        }
        Err(e) => format!("<unreadable: {e}>"),
    };

    format!(
        "  mount point:   {}\n                is_mounted:    {}\n                backing dev:   {}\n                image file:    {} ({})\n                dir contents:  {}\n                .nemr-state/projects exists: {}\n                mounted image: {:?}   <-- must equal the image file above (F-28)",
        mount_point.display(),
        volume::is_mounted(&mount_point),
        volume::backing_device(&mount_point).unwrap_or_else(|| "<none>".into()),
        image.display(),
        if image.exists() { "present" } else { "MISSING" },
        listing,
        mount_point.join(".nemr-state/projects").exists(),
        volume::mounted_image_path(&mount_point),
    )
}

/// Whether unprivileged user, mount and network namespaces are usable here,
/// and if not, **why**.
///
/// Reported rather than silently skipped: a host without them cannot verify
/// E-11's offline guarantee, and that is a coverage gap to surface.
///
/// The bare boolean this replaced cost a CI round-trip: the offline test
/// refused, said only "namespaces are unavailable", and left the actual cause
/// to be guessed at. `unshare` writes a specific reason to stderr — on Ubuntu 24.04 it is
/// normally `kernel.apparmor_restrict_unprivileged_userns`, but "normally" is
/// not a diagnosis. Surfacing the real message means the next failure is read
/// rather than inferred.
pub fn namespace_probe() -> Result<(), String> {
    namespace_probe_with("unshare")
}

/// The probe, parameterised on the binary so its failure path is testable.
///
/// Taking the command as an argument rather than mutating `PATH` keeps the test
/// free of process-global state — and the point of the parameter is that the
/// *diagnostic* gets exercised, not just the happy path. F-65 was a diagnostic
/// that could never print; a diagnostic nothing ever runs is the same bug
/// waiting to happen.
pub fn namespace_probe_with(command: &str) -> Result<(), String> {
    match Command::new(command).args(["-rmn", "true"]).output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim();
            let restriction =
                std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
                    .map(|value| format!("apparmor_restrict_unprivileged_userns={}", value.trim()))
                    .unwrap_or_else(|_| "apparmor_restrict_unprivileged_userns=<absent>".into());
            Err(format!(
                "`unshare -rmn true` failed ({}): {}\n     {restriction}",
                output.status,
                if detail.is_empty() {
                    "no stderr"
                } else {
                    detail
                }
            ))
        }
        Err(e) => Err(format!("cannot run `unshare`: {e}")),
    }
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
            "{} && CONTAINERD_ADDRESS='{socket}' '{}' {}",
            offline_masking(&home),
            binary.display(),
            quoted.join(" ")
        ))
        .output()
        .expect("run nemr offline")
}

/// The shell that hides this machine's Claude login inside the namespace.
///
/// ONE definition, used by `run_offline` and by the control that proves the
/// hiding works — otherwise the control tests something the real call does not
/// do. It masks the path the ENGINE reads (F-14:
/// `~/.local/share/nemr/host-credential`) and, for good measure, the host's own
/// `~/.claude`, which is a different file the engine never reads.
///
/// Masking `~/.claude` alone was the whole of this for two days after F-14
/// moved the credential, so `e11_export_and_import_work_with_no_network_and_no_credentials`
/// silently stopped establishing the "no credential" half of its own name. It
/// passed either way, which is why nothing caught it (the gate audit, 2026-09-09).
fn offline_masking(home: &str) -> String {
    let engine_credential_dir = format!("{home}/.local/share/nemr/host-credential");
    // mkdir first: mount fails on a missing directory, and a host that has
    // never run the engine has neither of these.
    format!(
        "mkdir -p '{engine_credential_dir}' '{home}/.claude' && \
         mount -t tmpfs none '{engine_credential_dir}' && \
         mount -t tmpfs none '{home}/.claude'"
    )
}

/// Run an arbitrary shell command under exactly the masking `run_offline` uses.
///
/// This exists so a control can ask "is the credential actually hidden in
/// there?" of the same namespace the real call runs in.
pub fn run_offline_probe(shell: &str) -> std::process::Output {
    let home = std::env::var("HOME").expect("HOME");
    Command::new("unshare")
        .args(["-rmn", "bash", "-c"])
        .arg(format!("{} && {shell}", offline_masking(&home)))
        .output()
        .expect("run a probe offline")
}
