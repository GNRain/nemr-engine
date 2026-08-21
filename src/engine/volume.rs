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
use std::os::unix::ffi::OsStrExt;
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
    /// `~/.local/share/nemr`.
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
            root: PathBuf::from(home).join(".local").join("share").join("nemr"),
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
    pub const DEFAULT_HELPER: &'static str = "/usr/local/libexec/nemr-volume";

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

/// Bytes used and total capacity of a mounted volume.
///
/// Reported from the filesystem itself rather than from the requested preset:
/// AC-6.1 compares `list` against actual state, and ext4 metadata means the
/// usable total is always somewhat below the size that was asked for. Quoting
/// the preset here would be quoting an intention, not a measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub used: u64,
    pub available: u64,
    pub total: u64,
}

impl Usage {
    /// Percentage full, using `df`'s definition.
    ///
    /// `df` computes `used / (used + available)`, **not** `used / total`. The
    /// difference is ext4's root-reserved blocks, which are neither used nor
    /// available to an ordinary caller. Dividing by `total` reports a smaller
    /// percentage than `df` for the same filesystem, and AC-6.1 cross-checks
    /// this output against `df`.
    pub fn percent(&self) -> f64 {
        let denominator = self.used + self.available;
        if denominator == 0 {
            0.0
        } else {
            (self.used as f64 / denominator as f64) * 100.0
        }
    }
}

/// Whether `path` is currently a mount point.
///
/// Checked against the kernel's mount table rather than inferred. This matters
/// because `statvfs` succeeds on a directory that is *not* a mount point and
/// reports the filesystem that directory sits on — so an unmounted volume
/// silently yields the host root filesystem's numbers. That produced a `list`
/// row reading "30.6GiB used of 2GB", which is the host disk, not the volume.
///
/// The mount-point field is **octal-unescaped** before comparison. The kernel
/// writes mountinfo field 5 with `\040` for space, `\011` for tab, `\012` for
/// newline and `\134` for backslash, so a raw comparison of a path containing
/// any of those reports a mounted volume as *unmounted*. Under a `$HOME` with a
/// space that is not cosmetic: `start` would take the remount path against an
/// already-mounted volume and stack a second loop device and ext4 mount over
/// the same backing bytes — the exact silent-corruption shape VOL-06 exists to
/// prevent. Same defect, same fix, as the privileged helper's parser.
pub fn is_mounted(mount_point: &Path) -> bool {
    let Ok(table) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    mountinfo_has_target(&table, mount_point)
}

/// The device backing `mount_point`, e.g. `/dev/loop19`, or `None` if it is not
/// a mount point.
///
/// This is the fact that makes VOL-05 legible. "Is something mounted here" and
/// "is the *project's volume* mounted here" are different questions; a working
/// directory backed by the host root device rather than a loop device is the
/// VOL-05 signature, and it is invisible unless the device is logged.
pub fn backing_device(mount_point: &Path) -> Option<String> {
    let table = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    mountinfo_device_for(&table, mount_point)
}

/// Source device for a mount target, parsed from a mountinfo table.
///
/// Split out for unit-testing without a live `/proc`. The source field sits
/// after the ` - ` separator (`… - <fstype> <source> <superopts>`), which is why
/// the optional-fields section has to be skipped rather than counted past.
fn mountinfo_device_for(table: &str, mount_point: &Path) -> Option<String> {
    let wanted = mount_point.as_os_str().as_bytes();
    for line in table.lines() {
        let Some(target) = line.split(' ').nth(4) else {
            continue;
        };
        if unescape_octal(target) != wanted {
            continue;
        }
        // Everything after " - " is: fstype, source, super options.
        if let Some((_, after)) = line.split_once(" - ") {
            if let Some(source) = after.split_whitespace().nth(1) {
                return Some(String::from_utf8_lossy(&unescape_octal(source)).into_owned());
            }
        }
    }
    None
}

/// Whether `table` (mountinfo contents) lists `mount_point` as a mount target.
///
/// Split out so the octal-unescaping logic is unit-testable without a live
/// `/proc/self/mountinfo`.
fn mountinfo_has_target(table: &str, mount_point: &Path) -> bool {
    let wanted = mount_point.as_os_str().as_bytes();
    table.lines().any(|line| {
        // Field 5 (1-indexed) is the mount point; optional fields follow it
        // until a " - " separator, so counting from the left is correct.
        line.split(' ')
            .nth(4)
            .is_some_and(|field| unescape_octal(field) == wanted)
    })
}

/// Decode mountinfo/`/proc/mounts` octal escapes (`\NNN`) into raw bytes.
fn unescape_octal(field: &str) -> Vec<u8> {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let octal = &bytes[i + 1..i + 4];
            if octal.iter().all(|b| (b'0'..=b'7').contains(b)) {
                out.push((octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0'));
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Query a mounted volume's usage.
///
/// Returns `None` when the volume is not mounted — after a host reboot, say,
/// which destroys mounts and loop devices while the container record survives —
/// so `list` reports "unmounted" instead of the host filesystem's figures.
pub fn usage(mount_point: &Path) -> Option<Usage> {
    if !is_mounted(mount_point) {
        return None;
    }

    let path = std::ffi::CString::new(mount_point.as_os_str().as_encoded_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };

    // SAFETY: `path` is a valid NUL-terminated string and `stat` is a valid,
    // correctly-sized statvfs for the duration of the call.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }

    let block = stat.f_frsize as u64;
    let total = stat.f_blocks as u64 * block;

    // Three quantities, and conflating them is easy:
    //   f_blocks — total blocks in the filesystem
    //   f_bfree  — blocks free, including ext4's root reserve
    //   f_bavail — blocks free to an unprivileged caller, excluding that reserve
    //
    // `df` reports Used as total - f_bfree and Avail as f_bavail, so the root
    // reserve counts as neither. Computing used as total - f_bavail instead
    // silently folds the reserve (5% of the filesystem by default) into "used",
    // which is what this originally did: it reported 190MiB/10% where `df`
    // inside the container said 73M/4% for the same volume.
    let used = total.saturating_sub(stat.f_bfree as u64 * block);
    let available = stat.f_bavail as u64 * block;

    Some(Usage { used, available, total })
}

/// Format a byte count for human reading.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

/// Audit log line (VOL-03, NFR-04).
///
/// Written to stderr so it is visible without a log file and cannot be
/// confused with a command's own stdout. Prefixed so an operator can
/// reconstruct what happened without reading Rust source.
fn audit(message: &str) {
    // VOL-03 / NFR-04: the audit trail. Emitted at `info` so it is on by
    // default without any flag — an operator must be able to reconstruct what
    // happened without knowing to enable anything.
    tracing::info!("[nemr:volume] {message}");
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
                "WARNING: the release helper for volume {:?} returned an error: {error:#}\n\
                 This is the *cleanup path* failing, which is not the same as a confirmed \
                 leak — the mount may never have been established (this runs on create's \
                 error path too). It does mean the release could not be confirmed. Verify \
                 with `losetup -a` and `grep {} /proc/self/mountinfo`; a startup \
                 reconciliation sweep will also reclaim it if it did leak.",
                self.name, self.name
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

    /// The mountinfo parser must decode the kernel's octal escapes, or a volume
    /// mounted under a path containing a space/tab/newline/backslash reads as
    /// unmounted — which sends `start` down the remount path and stacks a second
    /// loop device and ext4 mount over the same bytes (#2). This is the unit
    /// proof; it needs no `/proc`.
    /// VOL-05 (#30): `Usage::percent` uses df's definition — used/(used+available),
    /// NOT used/total — so the ext4 root reserve counts as neither. Pinned here
    /// because the smoke test's df cross-check is the only other guard, and a
    /// host-free unit test catches an arithmetic regression instantly.
    #[test]
    fn usage_percent_matches_df_definition() {
        // 100 used, 300 available, 500 total (100 reserved, neither used nor avail).
        let u = Usage { used: 100, available: 300, total: 500 };
        // df: 100 / (100 + 300) = 25%, NOT 100/500 = 20%.
        assert!((u.percent() - 25.0).abs() < 1e-9, "percent should be 25.0, got {}", u.percent());
    }

    #[test]
    fn usage_percent_is_zero_on_empty_filesystem() {
        let u = Usage { used: 0, available: 0, total: 0 };
        assert_eq!(u.percent(), 0.0, "an empty/degenerate fs must not divide by zero");
    }

    #[test]
    fn usage_percent_full_is_100() {
        let u = Usage { used: 400, available: 0, total: 500 };
        assert!((u.percent() - 100.0).abs() < 1e-9, "no space available reads as 100%");
    }

    #[test]
    fn is_mounted_decodes_octal_escaped_mount_points() {
        // A real mountinfo line for a mount point containing a space, exactly as
        // the kernel escapes it (\040), with two optional fields before " - ".
        let table = "301 29 7:19 / /home/john\\040doe/.local/share/nemr/mounts/p \
                     rw,relatime shared:277 master:2 - ext4 /dev/loop7 rw\n";

        assert!(
            mountinfo_has_target(table, Path::new("/home/john doe/.local/share/nemr/mounts/p")),
            "a mount point with a space must be recognised despite the \\040 escape"
        );
        assert!(
            !mountinfo_has_target(table, Path::new("/home/john doe/.local/share/nemr/mounts/other")),
            "a different path must not match"
        );
        // The naive (broken) comparison would have matched the escaped form:
        assert!(
            !mountinfo_has_target(table, Path::new("/home/john\\040doe/.local/share/nemr/mounts/p")),
            "the escaped literal must NOT match — that was the bug"
        );
    }

    /// The device parser backs the VOL-05 debug line. Field 5 is the target;
    /// the source sits after the " - " separator, so the optional-fields
    /// section has to be skipped rather than counted past — a fixed field index
    /// would read the wrong token whenever the optional count changes.
    #[test]
    fn mountinfo_device_is_read_after_the_separator() {
        let table = "301 29 7:19 / /home/u/.local/share/nemr/mounts/p rw,relatime \
shared:277 master:2 - ext4 /dev/loop7 rw\n"
            .replace(" \\\n", " ");
        assert_eq!(
            mountinfo_device_for(&table, Path::new("/home/u/.local/share/nemr/mounts/p")),
            Some("/dev/loop7".to_string()),
        );
        // A path that is not a mount point has no device — the VOL-05 condition.
        assert_eq!(
            mountinfo_device_for(&table, Path::new("/home/u/.local/share/nemr/mounts/other")),
            None,
        );
    }

    #[test]
    fn unescape_octal_covers_the_kernel_escapes() {
        assert_eq!(unescape_octal("plain"), b"plain");
        assert_eq!(unescape_octal(r"a\040b"), b"a b");
        assert_eq!(unescape_octal(r"a\011b"), b"a\tb");
        assert_eq!(unescape_octal(r"a\012b"), b"a\nb");
        assert_eq!(unescape_octal(r"a\134b"), b"a\\b");
    }

    #[test]
    fn paths_are_derived_under_the_managed_root() {
        let paths = VolumePaths {
            root: PathBuf::from("/tmp/nemr-test"),
        };
        assert_eq!(
            paths.image_file("demo"),
            PathBuf::from("/tmp/nemr-test/volumes/demo.img")
        );
        assert_eq!(
            paths.mount_point("demo"),
            PathBuf::from("/tmp/nemr-test/mounts/demo")
        );
    }
}
