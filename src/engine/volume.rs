//! Quota-bounded storage volume management (Section 3.6, VOL-01..VOL-05).
//!
//! A volume is a fixed-size sparse file, formatted ext4, attached to a loop
//! device and mounted. The mount is later bind-mounted into a project's
//! container as its working directory (Milestone 4).
//!
//! # Privilege split
//!
//! Measured at Milestone 3, and recorded in PRIV-02:
//!
//! | Step | Privileged? |
//! |---|---|
//! | Sparse file allocation | no |
//! | `mkfs.ext4` | no |
//! | `losetup` attach/detach | **yes** |
//! | `mount` / `umount` | **yes** |
//!
//! Everything unprivileged happens directly in this module. The privileged
//! steps go through [`PrivilegedOps`], whose production implementation shells
//! out to the root-owned helper described in PRIV-03. Formatting is
//! deliberately *not* behind that seam: it is the most destructive operation
//! involved, and keeping it unprivileged keeps it off the privileged surface
//! entirely.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Volume size presets (VOL-01).
///
/// A closed set rather than an arbitrary byte count: VOL-01 specifies presets,
/// and a fixed set keeps the privileged helper's input space small — it
/// accepts a preset name, never a caller-computed byte count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeSize {
    Small,
    Medium,
    Large,
}

impl VolumeSize {
    pub const DEFAULT: Self = Self::Medium;

    /// Size in bytes.
    pub fn bytes(self) -> u64 {
        match self {
            Self::Small => 500 * 1024 * 1024,
            Self::Medium => 2 * 1024 * 1024 * 1024,
            Self::Large => 10 * 1024 * 1024 * 1024,
        }
    }

    /// Canonical CLI spelling, e.g. `2GB`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Small => "500MB",
            Self::Medium => "2GB",
            Self::Large => "10GB",
        }
    }

    pub fn all() -> [Self; 3] {
        [Self::Small, Self::Medium, Self::Large]
    }
}

impl std::str::FromStr for VolumeSize {
    type Err = anyhow::Error;

    /// Parse the `--size` flag. Case-insensitive; accepts `500MB`/`2GB`/`10GB`.
    fn from_str(s: &str) -> Result<Self> {
        let normalised = s.trim().to_ascii_uppercase();
        Self::all()
            .into_iter()
            .find(|size| size.as_str() == normalised)
            .with_context(|| {
                let valid: Vec<&str> = Self::all().iter().map(|s| s.as_str()).collect();
                format!(
                    "unrecognised volume size {s:?}. Valid sizes: {}. \
                     Auto-expanding quotas are out of scope for Phase 1.",
                    valid.join(", ")
                )
            })
    }
}

impl std::fmt::Display for VolumeSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Maximum length of a volume name.
const MAX_NAME_LEN: usize = 32;

/// Validate a volume name.
///
/// Deliberately strict: this name is passed to the privileged helper, and it
/// is the *only* caller-controlled input that reaches a privileged code path.
/// The helper re-validates independently — it must not trust this function,
/// since the helper's whole purpose is to be the boundary — but rejecting
/// early gives a better error and keeps bad names out of engine state.
///
/// Rejects anything containing `/`, `.`, whitespace, or shell metacharacters,
/// so a name can never traverse or escape the managed directory.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("volume name must not be empty");
    }
    if name.len() > MAX_NAME_LEN {
        bail!(
            "volume name {name:?} is {} characters; maximum is {MAX_NAME_LEN}",
            name.len()
        );
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        bail!("volume name {name:?} must start with a lowercase letter or digit");
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
    {
        bail!(
            "volume name {name:?} contains {bad:?}; only lowercase letters, \
             digits and '-' are allowed"
        );
    }
    Ok(())
}

/// Filesystem layout for the engine's managed volumes.
///
/// Both directories live under the user's data directory: nothing here
/// requires root, and keeping them user-owned means the privileged helper can
/// derive every path it touches from a name alone.
#[derive(Debug, Clone)]
pub struct VolumePaths {
    root: PathBuf,
}

impl VolumePaths {
    /// `~/.local/share/aihub`.
    ///
    /// Deliberately does **not** honour `XDG_DATA_HOME`, despite that being the
    /// conventional choice. The privileged helper derives this same path from
    /// the invoking user's `/etc/passwd` entry and ignores the environment
    /// entirely — it must, since a caller-controlled variable would let it be
    /// pointed anywhere. If the engine honoured `XDG_DATA_HOME` the two would
    /// disagree about where a volume lives whenever it was set, and the engine
    /// would allocate a backing file the helper would then refuse to find.
    /// Matching the helper's stricter rule keeps them in agreement by
    /// construction.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .context("HOME is not set; cannot locate volume storage")?;
        Ok(Self {
            root: PathBuf::from(home).join(".local").join("share").join("aihub"),
        })
    }

    pub fn image_dir(&self) -> PathBuf {
        self.root.join("volumes")
    }

    pub fn mount_dir(&self) -> PathBuf {
        self.root.join("mounts")
    }

    /// Backing file for `name`.
    pub fn image_file(&self, name: &str) -> PathBuf {
        self.image_dir().join(format!("{name}.img"))
    }

    /// Mount point for `name`.
    pub fn mount_point(&self, name: &str) -> PathBuf {
        self.mount_dir().join(name)
    }

    /// Create the managed directories if absent.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [self.image_dir(), self.mount_dir()] {
            fs::create_dir_all(&dir)
                .with_context(|| format!("failed to create {}", dir.display()))?;
        }
        Ok(())
    }
}

/// The privileged operations a volume needs (PRIV-02).
///
/// A trait rather than direct calls so that the fault-injection test required
/// by AC-3.4 can substitute an implementation that fails at a chosen step, and
/// assert that RAII cleanup (VOL-04) still releases everything. Injecting a
/// mid-mount failure against the real helper would otherwise mean deliberately
/// corrupting host state.
pub trait PrivilegedOps {
    /// Attach `image` to a free loop device and mount it at `mount_point`,
    /// chowning the mount into the caller's mapped subuid range (PRIV-06).
    ///
    /// One call, because PRIV-06 requires mount and chown to be atomic: a
    /// mounted-but-unchowned volume is unusable by the container, and exposing
    /// chown separately would permit re-owning arbitrary paths.
    fn attach_and_mount(&self, name: &str, size: VolumeSize) -> Result<()>;

    /// Unmount and detach. Must be idempotent: [`Volume::drop`] calls it on
    /// error paths where the mount may never have been established.
    fn unmount_and_detach(&self, name: &str) -> Result<()>;
}

/// Production implementation: shells out to the root-owned helper (PRIV-03).
///
/// Passes a name and a preset only — never a path, device or UID. Everything
/// else is derived inside the helper, which is the point of the design.
#[derive(Debug, Clone)]
pub struct HelperOps {
    helper_path: PathBuf,
}

impl HelperOps {
    pub const DEFAULT_HELPER: &'static str = "/usr/local/libexec/aihub-volume";

    pub fn new() -> Self {
        Self {
            helper_path: PathBuf::from(Self::DEFAULT_HELPER),
        }
    }

    fn run(&self, args: &[&str]) -> Result<()> {
        // VOL-03 / NFR-04: every privileged invocation is logged in full,
        // before it runs, with the fact of elevation stated explicitly.
        audit(&format!(
            "ELEVATED: sudo -n {} {}",
            self.helper_path.display(),
            args.join(" ")
        ));

        let output = Command::new("sudo")
            .arg("-n")
            .arg(&self.helper_path)
            .args(args)
            .output()
            .with_context(|| {
                format!(
                    "failed to execute {} via sudo. Is the helper installed? \
                     See deploy/README for installation.",
                    self.helper_path.display()
                )
            })?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        for line in stderr.lines().filter(|l| !l.is_empty()) {
            audit(&format!("helper: {line}"));
        }

        if !output.status.success() {
            bail!(
                "privileged helper failed ({}): {}",
                output.status,
                stderr.trim()
            );
        }
        Ok(())
    }
}

impl Default for HelperOps {
    fn default() -> Self {
        Self::new()
    }
}

impl PrivilegedOps for HelperOps {
    fn attach_and_mount(&self, name: &str, size: VolumeSize) -> Result<()> {
        self.run(&["mount", name, size.as_str()])
    }

    fn unmount_and_detach(&self, name: &str) -> Result<()> {
        self.run(&["unmount", name])
    }
}

/// Audit log line (VOL-03, NFR-04).
///
/// Written to stderr so it is visible without a log file and cannot be
/// confused with a command's own stdout. Prefixed so an operator can
/// reconstruct what happened without reading Rust source.
fn audit(message: &str) {
    eprintln!("[aihub:volume] {message}");
}

/// A provisioned volume.
///
/// Holds the mount open for its lifetime. [`Drop`] releases it (VOL-04) —
/// including on error paths, which is the entire reason this is a guard type
/// rather than a set of free functions.
pub struct Volume<P: PrivilegedOps> {
    name: String,
    size: VolumeSize,
    paths: VolumePaths,
    ops: P,
    /// Whether the privileged mount succeeded, and so needs releasing.
    mounted: bool,
}

impl<P: PrivilegedOps> Volume<P> {
    /// Create and mount a new volume (VOL-01, VOL-02).
    ///
    /// Ordering is deliberate: everything unprivileged happens first, so a
    /// failure in allocation or formatting never leaves a loop device or mount
    /// behind — there is nothing privileged to clean up yet.
    pub fn create(name: &str, size: VolumeSize, paths: VolumePaths, ops: P) -> Result<Self> {
        validate_name(name)?;
        paths.ensure_dirs()?;

        let image = paths.image_file(name);
        if image.exists() {
            bail!(
                "volume {name:?} already exists at {}. Delete it first.",
                image.display()
            );
        }

        allocate_sparse_file(&image, size)?;

        // From here on a failure must remove the backing file, or a retry hits
        // the "already exists" check above against a half-built volume.
        let mut volume = Self {
            name: name.to_string(),
            size,
            paths,
            ops,
            mounted: false,
        };

        if let Err(error) = volume.format_and_mount() {
            // Order matters, and getting it wrong leaks a loop device.
            //
            // The privileged side identifies a volume's loop device by looking
            // up its backing file *by path*. Unlinking the file first leaves
            // any attached device permanently unfindable — `losetup -a` still
            // shows it, marked "(deleted)", with nothing able to detach it.
            //
            // So release the privileged resources first (explicit drop), and
            // only then remove the file we own.
            drop(volume);
            let _ = fs::remove_file(&image);
            audit(&format!(
                "create failed for {name:?}, released resources and removed backing file: {error:#}"
            ));
            return Err(error);
        }

        Ok(volume)
    }

    fn format_and_mount(&mut self) -> Result<()> {
        let image = self.paths.image_file(&self.name);
        format_ext4(&image)?;

        let mount_point = self.paths.mount_point(&self.name);
        fs::create_dir_all(&mount_point)
            .with_context(|| format!("failed to create mount point {}", mount_point.display()))?;

        // Take responsibility for cleanup *before* initiating the privileged
        // operation, not after it reports success. A partial failure — mounted
        // but not chowned, say — must still be released by Drop, and if we only
        // set this on the success path such a failure would leak the mount and
        // loop device. `unmount_and_detach` is idempotent precisely so this is
        // safe when the operation failed before doing anything (VOL-04).
        self.mounted = true;
        self.ops.attach_and_mount(&self.name, self.size)?;

        audit(&format!(
            "mounted volume {:?} ({}) at {}",
            self.name,
            self.size,
            mount_point.display()
        ));
        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn size(&self) -> VolumeSize {
        self.size
    }

    /// Path the volume is mounted at — the container's working directory.
    pub fn mount_point(&self) -> PathBuf {
        self.paths.mount_point(&self.name)
    }

    pub fn image_file(&self) -> PathBuf {
        self.paths.image_file(&self.name)
    }

    /// Whether the privileged mount is currently held.
    pub fn is_mounted(&self) -> bool {
        self.mounted
    }

    /// Give up ownership of the mount, leaving it in place.
    ///
    /// The guard exists so a *failed* provisioning run releases everything
    /// (VOL-04). A successful one is the opposite case: the volume must outlive
    /// the guard, because a project's container is about to depend on it. This
    /// marks the mount as no longer owned, so `Drop` becomes a no-op, and
    /// returns the mount point.
    ///
    /// Call only once the volume is genuinely committed to. Anything that can
    /// still fail should happen before this, so the failure path still cleans
    /// up.
    pub fn persist(mut self) -> PathBuf {
        let mount_point = self.paths.mount_point(&self.name);
        self.mounted = false;
        audit(&format!(
            "volume {:?} persisted at {}; no longer released on drop",
            self.name,
            mount_point.display()
        ));
        mount_point
    }
}

impl<P: PrivilegedOps> Drop for Volume<P> {
    /// Release the mount and loop device (VOL-04).
    ///
    /// Errors are logged, not propagated — `Drop` cannot fail, and panicking
    /// here would abort during unwind. An operator needs to see a leaked loop
    /// device in the log, which is what NFR-04 asks for.
    fn drop(&mut self) {
        if !self.mounted {
            return;
        }
        match self.ops.unmount_and_detach(&self.name) {
            Ok(()) => {
                self.mounted = false;
                audit(&format!("released volume {:?}", self.name));
            }
            Err(error) => audit(&format!(
                "WARNING: failed to release volume {:?}: {error:#}. \
                 Check `losetup -a` and `mount` for orphaned resources.",
                self.name
            )),
        }
    }
}

/// Allocate the backing file as a sparse file (VOL-02).
///
/// Unprivileged: `set_len` on a fresh file produces a sparse allocation, so a
/// 2 GB volume costs no disk until written to.
fn allocate_sparse_file(path: &Path, size: VolumeSize) -> Result<()> {
    audit(&format!(
        "allocating sparse file {} ({} = {} bytes)",
        path.display(),
        size,
        size.bytes()
    ));

    let file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("failed to create backing file {}", path.display()))?;

    file.set_len(size.bytes())
        .with_context(|| format!("failed to size backing file {}", path.display()))?;

    Ok(())
}

/// Format the backing file as ext4 (VOL-02).
///
/// Unprivileged — see the module-level privilege table. `-F` is required
/// because the target is a regular file rather than a block device.
fn format_ext4(path: &Path) -> Result<()> {
    audit(&format!("formatting {} as ext4", path.display()));

    let output = Command::new("mkfs.ext4")
        .arg("-q")
        .arg("-F")
        .arg(path)
        .output()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => anyhow::anyhow!(
                "mkfs.ext4 not found. Install e2fsprogs (see PREREQUISITES.md)."
            ),
            _ => anyhow::Error::from(error),
        })
        .with_context(|| format!("failed to run mkfs.ext4 on {}", path.display()))?;

    if !output.status.success() {
        bail!(
            "mkfs.ext4 failed on {} ({}): {}",
            path.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_presets_match_vol_01() {
        assert_eq!(VolumeSize::Small.bytes(), 500 * 1024 * 1024);
        assert_eq!(VolumeSize::Medium.bytes(), 2 * 1024 * 1024 * 1024);
        assert_eq!(VolumeSize::Large.bytes(), 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn size_parses_case_insensitively() {
        assert_eq!("2GB".parse::<VolumeSize>().unwrap(), VolumeSize::Medium);
        assert_eq!("2gb".parse::<VolumeSize>().unwrap(), VolumeSize::Medium);
        assert_eq!(" 500mb ".parse::<VolumeSize>().unwrap(), VolumeSize::Small);
    }

    #[test]
    fn unknown_size_is_rejected_with_valid_options() {
        let error = "7GB".parse::<VolumeSize>().unwrap_err().to_string();
        assert!(error.contains("500MB"), "error should list valid sizes: {error}");
        assert!(error.contains("10GB"), "error should list valid sizes: {error}");
    }

    #[test]
    fn valid_names_accepted() {
        for name in ["a", "my-project", "proj2", "a-b-c-1"] {
            validate_name(name).unwrap_or_else(|e| panic!("{name:?} should be valid: {e}"));
        }
    }

    /// The traversal cases that motivated the PRIV-03 helper design. A name
    /// reaching the privileged boundary must never be able to escape the
    /// managed directory.
    #[test]
    fn traversal_and_injection_names_rejected() {
        for name in [
            "../etc",
            "..",
            "a/b",
            "/absolute",
            "with space",
            "semi;colon",
            "dollar$sign",
            "back\\slash",
            "new\nline",
            "UPPER",
            "trailing.",
        ] {
            assert!(
                validate_name(name).is_err(),
                "{name:?} must be rejected before reaching the privileged helper"
            );
        }
    }

    #[test]
    fn empty_and_overlong_names_rejected() {
        assert!(validate_name("").is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN + 1)).is_err());
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN)).is_ok());
    }

    #[test]
    fn paths_are_derived_under_the_managed_root() {
        let paths = VolumePaths {
            root: PathBuf::from("/tmp/aihub-test"),
        };
        assert_eq!(
            paths.image_file("demo"),
            PathBuf::from("/tmp/aihub-test/volumes/demo.img")
        );
        assert_eq!(
            paths.mount_point("demo"),
            PathBuf::from("/tmp/aihub-test/mounts/demo")
        );
    }
}

/// Integration tests exercising real loop devices and mounts.
///
/// Marked `#[ignore]` because they need the privileged helper installed
/// (`deploy/` + PREREQUISITES.md) and they mutate host state. Run explicitly:
///
/// ```text
/// cargo test --lib -- --ignored --nocapture --test-threads=1
/// ```
///
/// `--test-threads=1` matters: these attach loop devices and assert on global
/// host state, so they must not interleave.
#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::process::Command;

    /// Host inspection: is anything still attached or mounted for `name`?
    ///
    /// Deliberately queries the host directly rather than engine state — AC-3.2
    /// asks for verification by host inspection, and engine state could agree
    /// with itself while the host leaked.
    fn host_residue(paths: &VolumePaths, name: &str) -> (bool, bool, bool) {
        let image = paths.image_file(name);
        let mount_point = paths.mount_point(name);

        let file_exists = image.exists();

        // Scan the whole loop table rather than querying by path with
        // `losetup -j`. A device whose backing file has been unlinked is still
        // attached but no longer matches its path — `losetup -a` reports it as
        // "/path/to/file.img (deleted)". Probing by path cannot see that, which
        // is how an orphaned device once passed this check.
        let loop_attached = Command::new("losetup")
            .arg("-a")
            .output()
            .map(|o| {
                let table = String::from_utf8_lossy(&o.stdout).to_string();
                let needle = image.to_string_lossy().to_string();
                table.lines().any(|line| line.contains(&needle))
            })
            .unwrap_or(false);

        let mount_present = fs::read_to_string("/proc/self/mountinfo")
            .map(|table| {
                let target = mount_point.to_string_lossy().to_string();
                table
                    .lines()
                    .any(|l| l.split(' ').nth(4).is_some_and(|m| m == target))
            })
            .unwrap_or(false);

        (file_exists, loop_attached, mount_present)
    }

    fn assert_no_residue(paths: &VolumePaths, name: &str, context: &str) {
        let (file, loop_dev, mount) = host_residue(paths, name);
        assert!(!file, "{context}: backing file still present");
        assert!(!loop_dev, "{context}: loop device still attached");
        assert!(!mount, "{context}: mount entry still present");
    }

    /// Remove a volume completely: unmount, detach, delete backing file and
    /// mount point. Explicit rather than relying on Drop, so deletion is
    /// itself under test (AC-3.2).
    fn destroy(paths: &VolumePaths, name: &str) {
        let _ = HelperOps::new().unmount_and_detach(name);
        let _ = fs::remove_file(paths.image_file(name));
        let _ = fs::remove_dir(paths.mount_point(name));
    }

    /// AC-3.1: creation at a specified size is correctly capped, per df.
    #[test]
    #[ignore]
    fn ac_3_1_volume_is_capped_at_requested_size() {
        let paths = VolumePaths::from_env().unwrap();
        let name = "ac31-capped";
        destroy(&paths, name);

        let volume =
            Volume::create(name, VolumeSize::Small, paths.clone(), HelperOps::new()).unwrap();

        let df = Command::new("df")
            .args(["-B1", "--output=size,used,avail,target"])
            .arg(volume.mount_point())
            .output()
            .unwrap();
        let df = String::from_utf8_lossy(&df.stdout);
        println!("--- df of {} ---\n{df}", volume.mount_point().display());

        let total_bytes: u64 = df
            .lines()
            .nth(1)
            .and_then(|l| l.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .expect("df should report a size");

        let requested = VolumeSize::Small.bytes();
        // ext4 metadata (journal, inode tables, reserved blocks) consumes part
        // of the device, so the usable filesystem is smaller than the request —
        // never larger. That upper bound is the cap AC-3.1 is about.
        assert!(
            total_bytes <= requested,
            "filesystem ({total_bytes}) must not exceed the requested {requested}"
        );
        assert!(
            total_bytes > requested / 2,
            "filesystem ({total_bytes}) is implausibly small for a {requested}-byte request"
        );

        let du = Command::new("du")
            .args(["-h", "--apparent-size"])
            .arg(volume.image_file())
            .output()
            .unwrap();
        let du_actual = Command::new("du").arg("-h").arg(volume.image_file()).output().unwrap();
        println!(
            "--- du: apparent={} actual={} (sparse) ---",
            String::from_utf8_lossy(&du.stdout).trim(),
            String::from_utf8_lossy(&du_actual.stdout).trim()
        );

        drop(volume);
        destroy(&paths, name);
        assert_no_residue(&paths, name, "AC-3.1 cleanup");
    }

    /// AC-3.2: deletion leaves no file, loop device, or mount entry.
    #[test]
    #[ignore]
    fn ac_3_2_deletion_leaves_no_residue() {
        let paths = VolumePaths::from_env().unwrap();
        let name = "ac32-residue";
        destroy(&paths, name);

        let volume =
            Volume::create(name, VolumeSize::Small, paths.clone(), HelperOps::new()).unwrap();
        let (file, loop_dev, mount) = host_residue(&paths, name);
        assert!(file && loop_dev && mount, "volume should be fully live while held");
        println!("while live: file={file} loop={loop_dev} mount={mount}");

        drop(volume);
        destroy(&paths, name);

        let (file, loop_dev, mount) = host_residue(&paths, name);
        println!("after delete: file={file} loop={loop_dev} mount={mount}");
        assert_no_residue(&paths, name, "AC-3.2");
    }

    /// AC-3.3: create → mount → write past capacity → enforced failure → delete.
    #[test]
    #[ignore]
    fn ac_3_3_writing_past_capacity_fails_cleanly() {
        use std::io::Write;

        let paths = VolumePaths::from_env().unwrap();
        let name = "ac33-capacity";
        destroy(&paths, name);

        let volume =
            Volume::create(name, VolumeSize::Small, paths.clone(), HelperOps::new()).unwrap();

        // Write in chunks until the filesystem refuses. VOL-05: this must be a
        // clear error, never a silent short write.
        let target = volume.mount_point().join("filler.bin");
        let mut file = fs::File::create(&target).expect("volume should be writable");
        let chunk = vec![0u8; 4 * 1024 * 1024];
        let mut written: u64 = 0;
        let error = loop {
            match file.write_all(&chunk).and_then(|()| file.flush()) {
                Ok(()) => {
                    written += chunk.len() as u64;
                    assert!(
                        written < VolumeSize::Small.bytes() * 2,
                        "wrote {written} bytes into a {} byte volume without hitting a limit — \
                         the quota is not being enforced",
                        VolumeSize::Small.bytes()
                    );
                }
                Err(e) => break e,
            }
        };

        println!("wrote {written} bytes, then failed with: {error}");
        println!("error kind: {:?}, raw os error: {:?}", error.kind(), error.raw_os_error());

        // ENOSPC (28) is the enforced-quota signal. An explicit error rather
        // than a truncated write is what VOL-05 requires.
        assert_eq!(
            error.raw_os_error(),
            Some(28),
            "expected ENOSPC when exceeding the quota, got {error:?}"
        );
        assert!(
            written < VolumeSize::Small.bytes(),
            "should not have written more than the volume holds"
        );

        drop(file);
        drop(volume);
        destroy(&paths, name);
        assert_no_residue(&paths, name, "AC-3.3 cleanup");
    }

    /// Wraps the real helper but fails *after* a successful mount, simulating a
    /// failure partway through provisioning (AC-3.4).
    struct FailAfterMount {
        inner: HelperOps,
    }

    impl PrivilegedOps for FailAfterMount {
        fn attach_and_mount(&self, name: &str, size: VolumeSize) -> Result<()> {
            self.inner.attach_and_mount(name, size)?;
            // Host state is now real: loop device attached, filesystem mounted.
            bail!("injected fault: simulated failure after mount succeeded")
        }

        fn unmount_and_detach(&self, name: &str) -> Result<()> {
            self.inner.unmount_and_detach(name)
        }
    }

    /// AC-3.4: a fault mid-provisioning leaves no orphaned resources (VOL-04).
    ///
    /// This is the case that makes RAII load-bearing rather than decorative:
    /// the mount genuinely happened, then provisioning failed. Without Drop
    /// taking responsibility before the privileged call, the loop device and
    /// mount would both leak.
    #[test]
    #[ignore]
    fn ac_3_4_fault_injection_leaves_no_orphans() {
        let paths = VolumePaths::from_env().unwrap();
        let name = "ac34-faultinj";
        destroy(&paths, name);

        let result = Volume::create(
            name,
            VolumeSize::Small,
            paths.clone(),
            FailAfterMount { inner: HelperOps::new() },
        );

        let error = result.err().expect("injected fault should fail creation");
        println!("create failed as injected: {error:#}");

        let (file, loop_dev, mount) = host_residue(&paths, name);
        println!("after failed create: file={file} loop={loop_dev} mount={mount}");
        assert_no_residue(&paths, name, "AC-3.4 (RAII cleanup after injected fault)");

        destroy(&paths, name);
    }
}
