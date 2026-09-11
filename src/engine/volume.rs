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

/// A volume's size, in bytes (VOL-01).
///
/// WAS a closed set of three presets. The reason given for the closed set was
/// that it "keeps the privileged helper's input space small — it accepts a
/// preset name, never a caller-computed byte count". That reason was sound and
/// it is now paid for differently: the helper parses the byte count itself,
/// under a grammar of nothing but ASCII digits, and bounds it before anything
/// is derived from the value (`deploy/nemr-volume`, protocol 3). The input
/// space is no smaller, so the parser is the thing that had to get stricter.
///
/// This type deliberately does NOT enforce the bounds. Parsing and policy are
/// separate: the bounds live in the helper, the one program that has to be
/// right about them, and reach the rest of the system through
/// [`PrivilegedOps::size_limits`]. A second copy here is a second copy to keep
/// in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VolumeSize(u64);

/// A kibibyte-based megabyte, which is what `500MB` has always meant here.
///
/// `500MB` was 500 × 1024 × 1024 from the first preset onwards. Switching to
/// SI megabytes now would silently change what an existing `--size 500MB`
/// means and what an existing project's recorded quota parses back to, so the
/// spelling keeps its meaning and the code says which one it is.
pub const MB: u64 = 1024 * 1024;
pub const GB: u64 = 1024 * 1024 * 1024;

impl VolumeSize {
    pub const DEFAULT: Self = Self(2 * GB);

    /// The three sizes that used to be the only ones.
    ///
    /// Kept as names because a great deal of the test suite says "the small
    /// one" and reads better for it, and because the page offers them as
    /// marks on its slider. They are not special to the engine any more: a
    /// volume may be any size between the helper's bounds, and nothing
    /// branches on these.
    pub const SMALL: Self = Self(500 * MB);
    pub const MEDIUM: Self = Self(2 * GB);
    pub const LARGE: Self = Self(10 * GB);

    pub const fn from_bytes(bytes: u64) -> Self {
        Self(bytes)
    }

    /// Size in bytes.
    pub fn bytes(self) -> u64 {
        self.0
    }

    /// The default size for a new project — the old clap default (2GB), now
    /// applied by the resolution layer so a missing `--size` and an explicit
    /// `--size 2GB` are distinguishable (WP-H).
    pub fn default_size() -> VolumeSize {
        Self::DEFAULT
    }

    /// The canonical spelling: whole gigabytes, else whole megabytes, else the
    /// exact byte count.
    ///
    /// MUST ROUND-TRIP through [`FromStr`] for every value, because this is
    /// what the bundle manifest records and what the daemon puts on the wire.
    /// A size that printed as `1GB` when it was 1 GB minus one block would
    /// come back as a different volume on import.
    pub fn as_str(self) -> String {
        if self.0 >= GB && self.0 % GB == 0 {
            format!("{}GB", self.0 / GB)
        } else if self.0 >= MB && self.0 % MB == 0 {
            format!("{}MB", self.0 / MB)
        } else {
            format!("{}B", self.0)
        }
    }
}

impl std::str::FromStr for VolumeSize {
    type Err = anyhow::Error;

    /// Parse a size. Case-insensitive, with an optional unit:
    ///
    /// - `2GB`, `1536MB` — kibibyte-based, as the presets always were
    /// - `67108864B`, `67108864` — an exact byte count
    ///
    /// NO BOUNDS ARE APPLIED. A number this refuses is a number that is not a
    /// size at all; a number that is out of range is refused by the helper,
    /// with the actual bound in the message. Two places that both refuse give
    /// two different messages for the same mistake.
    fn from_str(s: &str) -> Result<Self> {
        let t = s.trim();
        if t.is_empty() {
            bail!("a volume size is required, e.g. 2GB, 1536MB or a byte count");
        }
        let upper = t.to_ascii_uppercase();
        let (digits, unit) = match upper.strip_suffix("GB") {
            Some(d) => (d, GB),
            None => match upper.strip_suffix("MB") {
                Some(d) => (d, MB),
                None => (upper.strip_suffix('B').unwrap_or(&upper), 1),
            },
        };
        let digits = digits.trim();
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            bail!(
                "unrecognised volume size {s:?}. Give a whole number with an optional \
                 unit: 2GB, 1536MB, or a byte count like 2147483648."
            );
        }
        let n: u64 = digits
            .parse()
            .with_context(|| format!("volume size {s:?} does not fit in 64 bits"))?;
        n.checked_mul(unit)
            .map(VolumeSize)
            .with_context(|| format!("volume size {s:?} does not fit in 64 bits"))
    }
}

impl std::fmt::Display for VolumeSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// The helper's own answer to "what will you accept, and how do you round?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeBounds {
    pub min: u64,
    pub max: u64,
    pub block: u64,
}

/// What the privileged helper will accept, and what it rounds to.
///
/// Carried rather than duplicated: every number here comes from the helper's
/// `normalize`, which is the program that enforces them. `free` comes from
/// `statvfs` on the volumes directory — the same call `nemr status` uses for a
/// mounted volume, so the two agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeLimits {
    pub min: u64,
    pub max: u64,
    pub block: u64,
    pub free: u64,
}

impl SizeLimits {
    pub fn new(bounds: SizeBounds, free: u64) -> Self {
        Self {
            min: bounds.min,
            max: bounds.max,
            block: bounds.block,
            free,
        }
    }

    /// Round a requested size the way the helper will, so a caller can show
    /// the real figure before it asks for confirmation.
    pub fn round(&self, size: VolumeSize) -> VolumeSize {
        if self.block == 0 {
            return size;
        }
        VolumeSize(size.bytes() - (size.bytes() % self.block))
    }

    /// Why this size cannot be used, if it cannot. Free disk is checked here
    /// too: the helper cannot check it (the engine allocates the backing file,
    /// not the helper) and a refusal at `mkfs` time is a refusal after the
    /// question was answered.
    pub fn refuse(&self, size: VolumeSize) -> Option<String> {
        let b = size.bytes();
        if b < self.min {
            return Some(format!(
                "{} is below the smallest volume nemr will make, {}",
                human_size(b),
                human_size(self.min)
            ));
        }
        if b > self.max {
            return Some(format!(
                "{} is above the largest volume nemr will make, {}",
                human_size(b),
                human_size(self.max)
            ));
        }
        if b > self.free {
            return Some(format!(
                "{} is more than the {} free on this disk",
                human_size(b),
                human_size(self.free)
            ));
        }
        None
    }
}

/// What a volume size may be on this host, and how much room there is.
///
/// Three facts from three owners, assembled in one place so nobody assembles
/// them twice: the bounds and the rounding from the helper, the free space
/// from `statvfs` on the volumes directory, and the default from the engine.
pub fn size_limits(ops: &impl PrivilegedOps, paths: &VolumePaths) -> Result<SizeLimits> {
    let bounds = ops.size_bounds()?;
    Ok(SizeLimits::new(bounds, free_bytes(&paths.image_dir())?))
}

/// Free space on the filesystem holding `path`, for an ordinary user.
///
/// `f_bavail`, not `f_bfree`: the difference is ext4's root-reserved blocks,
/// which the engine cannot allocate into. Quoting `f_bfree` would offer space
/// that `fallocate` then refuses.
pub fn free_bytes(path: &Path) -> Result<u64> {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("{} is not a usable path", path.display()))?;
    // SAFETY: a zeroed statvfs is a valid target and the pointer is a valid
    // NUL-terminated path for the duration of the call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        bail!(
            "cannot measure free space on {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

/// A byte count as a person reads it.
///
/// Lives here rather than in `project` because sizes are this module's
/// vocabulary and three places now need to print one.
pub fn human_size(bytes: u64) -> String {
    const G: u64 = 1024 * 1024 * 1024;
    const M: u64 = 1024 * 1024;
    const K: u64 = 1024;
    if bytes >= G {
        format!("{:.1}GiB", bytes as f64 / G as f64)
    } else if bytes >= M {
        format!("{:.1}MiB", bytes as f64 / M as f64)
    } else if bytes >= K {
        format!("{:.1}KiB", bytes as f64 / K as f64)
    } else {
        format!("{bytes}B")
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
pub fn validate_name(name: &str) -> std::result::Result<(), crate::error::Error> {
    let invalid = |reason: &str| crate::error::Error::InvalidName {
        name: name.to_string(),
        reason: reason.to_string(),
    };
    if name.is_empty() {
        return Err(invalid("must not be empty"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(invalid(&format!(
            "{} characters; maximum is {MAX_NAME_LEN}",
            name.len()
        )));
    }
    if !name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(invalid("must start with a lowercase letter or digit"));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
    {
        return Err(invalid(&format!(
            "contains {bad:?}; only lowercase letters, digits and '-' are allowed"
        )));
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
        let home =
            std::env::var_os("HOME").context("HOME is not set; cannot locate volume storage")?;
        Ok(Self {
            root: PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("nemr"),
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

    /// What the helper will accept, asked of the helper.
    ///
    /// On the privileged trait because the helper is where the bounds are
    /// enforced, and a bound the engine keeps its own copy of is a bound that
    /// drifts. Nothing here is privileged — the helper answers it without
    /// touching anything — but the answer must come from that program.
    fn size_bounds(&self) -> Result<SizeBounds>;
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

    /// The helper protocol this engine speaks.
    ///
    /// The helper's own source has said since protocol 2 that "the engine
    /// checks this before invoking a privileged operation". IT DID NOT — the
    /// only thing that ever read the number was `setup_test_host.sh`, which
    /// prints it for a human. Nothing enforced it, so a host running a stale
    /// helper found out through whatever the first mismatched argument
    /// happened to do. Protocol 3 changes the size argument from a word to a
    /// number, which is exactly the kind of change that must not be discovered
    /// that way, so the claim is now true.
    pub const PROTOCOL: u32 = 3;

    pub fn new() -> Self {
        Self {
            helper_path: PathBuf::from(Self::DEFAULT_HELPER),
        }
    }

    fn run(&self, args: &[&str]) -> Result<()> {
        self.run_capturing(args).map(|_| ())
    }

    /// Refuse a helper this engine does not speak to, ONCE per process.
    ///
    /// Once because it costs a `sudo` round trip and the installed binary
    /// cannot change under a running command; before the first privileged
    /// call because the alternative is a mismatch surfacing as whatever the
    /// stale helper made of an argument it did not understand.
    fn check_protocol(&self) -> Result<()> {
        static CHECKED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
        CHECKED
            .get_or_init(|| self.read_protocol().map_err(|e| e.to_string()))
            .clone()
            .map_err(anyhow::Error::msg)
    }

    fn read_protocol(&self) -> Result<()> {
        let output = Command::new("sudo")
            .arg("-n")
            .arg(&self.helper_path)
            .arg("version")
            .output()
            .with_context(|| {
                format!(
                    "failed to execute {} via sudo. Is the helper installed? \
                     See deploy/README for installation.",
                    self.helper_path.display()
                )
            })?;
        let line = String::from_utf8_lossy(&output.stdout);
        let found: Option<u32> = line
            .split_whitespace()
            .last()
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|_| output.status.success());
        match found {
            Some(v) if v == Self::PROTOCOL => Ok(()),
            Some(v) => bail!(
                "the privileged helper speaks protocol {v}; this nemr speaks {}.\n\
                 The helper is root-owned and is not updated by installing nemr.\n\
                 Reinstall it: sudo ./scripts/setup_test_host.sh",
                Self::PROTOCOL
            ),
            None => bail!(
                "the privileged helper at {} did not report a protocol version.\n\
                 It predates the version handshake, or it is not the helper.\n\
                 Reinstall it: sudo ./scripts/setup_test_host.sh",
                self.helper_path.display()
            ),
        }
    }

    fn run_capturing(&self, args: &[&str]) -> Result<String> {
        self.check_protocol()?;
        // VOL-03 / NFR-04: every privileged invocation is logged in full,
        // before it runs, with the fact of elevation stated explicitly.
        audit_elevated(
            args.first().copied().unwrap_or("run"),
            &format!("sudo -n {} {}", self.helper_path.display(), args.join(" ")),
        );

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
            // The helper's own stderr is the useful part — it names the exact
            // syscall or path that failed — so it leads. The remedies below are
            // the three things that are actually wrong when this fires, in the
            // order they occur in practice.
            bail!(
                "the privileged helper failed.\n\
                 command:  {} {}\n\
                 exit:     {}\n\
                 it said:  {}\n\n\
                 Most likely, in order:\n  \
                 1. the helper is out of date — reinstall: sudo ./scripts/setup_test_host.sh\n  \
                 2. the sudoers grant is missing — check: sudo -n {} version\n  \
                 3. the host cannot provide what was asked (loop devices exhausted, \
                 no space) — check: losetup -a | wc -l, df -h",
                self.helper_path.display(),
                args.join(" "),
                output.status,
                if stderr.trim().is_empty() {
                    "<nothing>"
                } else {
                    stderr.trim()
                },
                self.helper_path.display(),
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl Default for HelperOps {
    fn default() -> Self {
        Self::new()
    }
}

impl PrivilegedOps for HelperOps {
    fn attach_and_mount(&self, name: &str, size: VolumeSize) -> Result<()> {
        // The byte count, not a preset word — and the helper checks it against
        // the backing file's real length before it mounts anything.
        self.run(&["mount", name, &size.bytes().to_string()])
    }

    fn unmount_and_detach(&self, name: &str) -> Result<()> {
        self.run(&["unmount", name])
    }

    fn size_bounds(&self) -> Result<SizeBounds> {
        parse_bounds(&self.run_capturing(&["normalize"])?)
    }
}

/// Read `key=value` lines from the helper's `normalize`.
///
/// Strict: a missing key is an error, never a default. A default here would be
/// this program quietly deciding a bound the helper is supposed to own, which
/// is the whole thing the round trip exists to prevent.
fn parse_bounds(stdout: &str) -> Result<SizeBounds> {
    let get = |key: &str| -> Result<u64> {
        stdout
            .lines()
            .find_map(|l| l.trim().strip_prefix(&format!("{key}=")))
            .with_context(|| {
                format!(
                    "the privileged helper did not report {key:?}. It is probably older than \
                     protocol 3 — reinstall: sudo ./scripts/setup_test_host.sh"
                )
            })?
            .trim()
            .parse::<u64>()
            .with_context(|| format!("the helper reported a {key} that is not a number"))
    };
    Ok(SizeBounds {
        min: get("min")?,
        max: get("max")?,
        block: get("block")?,
    })
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
/// Whether the filesystem mounted at a project's mount point is that project's
/// own volume (F-28).
///
/// Pure, so the decision that gates `start` is testable without mounting
/// anything — the production path and its guard are then the same code, which
/// is the F-58 lesson.
///
/// `actual` is what [`mounted_image_path`] resolved: `None` means the mount is
/// not loop-backed and therefore not a volume this engine provisioned.
pub fn mount_identity(expected: &Path, actual: Option<&Path>) -> Result<(), MountIdentityError> {
    match actual {
        Some(actual) if actual == expected => Ok(()),
        Some(actual) => Err(MountIdentityError::WrongVolume {
            actual: actual.to_path_buf(),
        }),
        None => Err(MountIdentityError::NotLoopBacked),
    }
}

/// Why a mount point does not hold the project's own volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountIdentityError {
    /// A loop-backed filesystem, but backed by a different image.
    WrongVolume { actual: std::path::PathBuf },
    /// Not loop-backed at all, so not a volume this engine provisioned.
    NotLoopBacked,
}

/// The backing image path of whatever filesystem is mounted at `mount_point`.
///
/// The inverse of [`attached_loop_device`], and the missing half of the
/// mount check (F-28). `is_mounted` answers "is *something* mounted here?";
/// this answers "is it *ours*?". Without it, a mount landing on the wrong path
/// — which is a live possibility whenever many volumes are attached and
/// released concurrently — is indistinguishable from the right one, and the
/// container is handed a foreign, usually empty, filesystem.
pub fn mounted_image_path(mount_point: &Path) -> Option<std::path::PathBuf> {
    const LOOP_MAJOR: u32 = 7;
    let table = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let wanted = mount_point.as_os_str().as_bytes();

    let mut device = None;
    for line in table.lines() {
        let mut fields = line.split(' ');
        let dev = fields.nth(2)?;
        let target = fields.nth(1)?;
        if unescape_octal(target) == wanted {
            device = Some(dev.to_string());
            break;
        }
    }
    let device = device?;
    let (major, minor) = device.split_once(':')?;
    if major.parse::<u32>().ok()? != LOOP_MAJOR {
        return None;
    }
    let backing =
        std::fs::read_to_string(format!("/sys/block/loop{minor}/loop/backing_file")).ok()?;
    let backing = backing.trim_end();
    Some(std::path::PathBuf::from(
        backing.strip_suffix(" (deleted)").unwrap_or(backing),
    ))
}

/// The loop device still attached to `image_path`, if any — including when the
/// file has been deleted (F-77).
///
/// Readable without privilege, so a *report* about whether a release actually
/// happened does not depend on the thing that does the releasing.
pub fn attached_loop_device(image_path: &Path) -> Option<u32> {
    let expected = image_path.to_string_lossy();
    let deleted = format!("{expected} (deleted)");
    for entry in std::fs::read_dir("/sys/block").ok()?.flatten() {
        let Ok(backing) = std::fs::read_to_string(entry.path().join("loop/backing_file")) else {
            continue;
        };
        let backing = backing.trim_end();
        if backing == expected || backing == deleted {
            let name = entry.file_name();
            let digits = name.to_str()?.strip_prefix("loop")?;
            return digits.parse().ok();
        }
    }
    None
}

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

/// How a mount's propagation bears on whether a mount made at that point is
/// visible to the rootless container runtime (F-128).
///
/// runc runs inside rootlesskit's mount namespace, which is started
/// `--propagation=rslave` — a slave of the host's mounts. A mount the privileged
/// helper makes in the host namespace reaches runc only if it *propagates* into
/// that slave, and it propagates only if the host-side mount is `shared` (part
/// of a peer group the slave receives from). If it is private, the mount is
/// invisible inside the container: the M8 session-state bind sources
/// (`.nemr-state/{projects,sessions}`) do not exist for runc, and the task fails
/// to start with an opaque `open …/.nemr-state/projects: no such file or
/// directory`. This was the WSL2 spike's dominant failure (E-10): WSL2's `/init`
/// leaves `/` private where a standard systemd host makes it rshared, so twelve
/// project-starting tests died at the mount step before any of their real
/// behaviour ran. The engine depends on `/` (hence its mounts) being rshared;
/// this makes that dependency legible instead of a runc riddle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountPropagation {
    /// `shared:` — the mount reaches the rslave container namespace. Required.
    Shared,
    /// No `shared:` tag — a mount here does NOT reach the container; runc will
    /// not see the bind sources under it. The WSL2 failure.
    Private,
    /// No mountinfo line for the target — the mount is absent, nothing to judge.
    /// Not a failure: `ensure_volume_mounted` checks presence; this judges only
    /// a mount that is present.
    Unknown,
}

/// The propagation of the mount at `mount_point`, read from this process's mount
/// namespace — the same namespace the privileged helper mounts into and the one
/// runc's is slaved to, so the answer is the one that governs visibility into
/// the container.
pub fn mount_propagation(mount_point: &Path) -> MountPropagation {
    match std::fs::read_to_string("/proc/self/mountinfo") {
        Ok(table) => mountinfo_propagation(&table, mount_point),
        Err(_) => MountPropagation::Unknown,
    }
}

/// Propagation of a mount target, parsed from a mountinfo table (F-128).
///
/// The propagation tags (`shared:N`, `master:N`, `propagate_from:N`,
/// `unbindable`) are *optional fields*: they sit after field 6 (mount options)
/// and before the ` - ` separator, in any number including zero. Only a
/// `shared:` tag means events propagate to the peer group runc's namespace is
/// slaved to; `master:` alone is a slave that receives but does not re-emit, so
/// it does not carry a mount onward to a further slave. Split out to be
/// unit-testable without a live `/proc` — the WSL2 case (a private `/`) cannot
/// be reproduced on a host whose `/` is already rshared.
fn mountinfo_propagation(table: &str, mount_point: &Path) -> MountPropagation {
    let wanted = mount_point.as_os_str().as_bytes();
    for line in table.lines() {
        let Some((left, _)) = line.split_once(" - ") else {
            continue;
        };
        let mut fields = left.split(' ');
        // Field 5 (index 4) is the mount point.
        if fields.nth(4).map(unescape_octal).as_deref() != Some(wanted) {
            continue;
        }
        // What remains is field 6 (options) then the optional tags; skip the
        // options and look for a `shared:` among the tags.
        let shared = fields.skip(1).any(|tag| tag.starts_with("shared:"));
        return if shared {
            MountPropagation::Shared
        } else {
            MountPropagation::Private
        };
    }
    MountPropagation::Unknown
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

    Some(Usage {
        used,
        available,
        total,
    })
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
/// The audit trail (VOL-03 / NFR-04), at `debug`.
///
/// It used to be `info`, so every command printed its full provisioning trace.
/// That was right while WP A was auditing the privileged path and wrong as a
/// default: a user running `nemr create` was shown ten lines of loop devices
/// and mount points to say "your project is ready".
///
/// The trail is not lost — `--verbose`, `NEMR_DEBUG` and `NEMR_LOG` all show it
/// in full, and the *fact* of elevation stays visible by default via
/// [`audit_elevated`]. What changed is that reconstructing a run is now
/// something you ask for rather than something you scroll past.
fn audit(message: &str) {
    // `nemr_audit` lets the daemon's audit Layer (E-09) classify this event
    // structurally rather than by matching the message text.
    tracing::debug!(nemr_audit = "trace", "[nemr:volume] {message}");
}

/// The one thing that stays visible by default: something ran as root.
///
/// A user must be able to tell that privilege was used without having to enable
/// anything, even though they do not see the arguments. The full command line —
/// which is the audit record — is emitted alongside at `debug`.
fn audit_elevated(operation: &str, full_command: &str) {
    // The elevation event is the one a user must be able to see without going
    // to look (NFR-04): `nemr_audit = "elevated"` marks it for the daemon's
    // audit stream, which surfaces it inline in the CLI.
    tracing::info!(
        nemr_audit = "elevated",
        nemr_op = operation,
        "[nemr] elevated: {operation} (via the privileged helper)"
    );
    tracing::debug!(
        nemr_audit = "trace",
        "[nemr:volume] ELEVATED: {full_command}"
    );
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
                "volume {name:?} already exists at {}\n\
             A volume is never reformatted in place — that would destroy whatever \
             is on it. Remove the project and its volume:\n    \
             nemr delete {name}\n\
             Or, if there is no project and only a stray image, remove the file \
             deliberately.",
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
            io::ErrorKind::NotFound => {
                anyhow::anyhow!("mkfs.ext4 not found. Install e2fsprogs (see PREREQUISITES.md).")
            }
            _ => anyhow::Error::from(error),
        })
        .with_context(|| format!("failed to run mkfs.ext4 on {}", path.display()))?;

    if !output.status.success() {
        bail!(
            "could not format the volume as ext4.\n\
             image:    {}\n\
             exit:     {}\n\
             it said:  {}\n\n\
             The image file was allocated but is unusable, so it is left in place \
             rather than silently removed. Remove it before retrying:\n    \
             rm {}",
            path.display(),
            output.status,
            {
                let e = String::from_utf8_lossy(&output.stderr);
                if e.trim().is_empty() {
                    "<nothing>".to_string()
                } else {
                    e.trim().to_string()
                }
            },
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three named sizes keep the meanings they always had. `500MB` has
    /// been 500 MiB since the first preset; an SI megabyte here would quietly
    /// resize every existing project's recorded quota on the next parse.
    #[test]
    fn the_named_sizes_keep_their_old_meanings() {
        assert_eq!(VolumeSize::SMALL.bytes(), 500 * 1024 * 1024);
        assert_eq!(VolumeSize::MEDIUM.bytes(), 2 * 1024 * 1024 * 1024);
        assert_eq!(VolumeSize::LARGE.bytes(), 10 * 1024 * 1024 * 1024);
        assert_eq!(VolumeSize::DEFAULT, VolumeSize::MEDIUM);
    }

    #[test]
    fn size_parses_case_insensitively() {
        assert_eq!("2GB".parse::<VolumeSize>().unwrap(), VolumeSize::MEDIUM);
        assert_eq!("2gb".parse::<VolumeSize>().unwrap(), VolumeSize::MEDIUM);
        assert_eq!(" 500mb ".parse::<VolumeSize>().unwrap(), VolumeSize::SMALL);
    }

    /// The point of SPEC 1.153: a size that is none of the three.
    #[test]
    fn any_size_parses_not_only_the_three() {
        assert_eq!("7GB".parse::<VolumeSize>().unwrap().bytes(), 7 * GB);
        assert_eq!("1536MB".parse::<VolumeSize>().unwrap().bytes(), 1536 * MB);
        assert_eq!("64MB".parse::<VolumeSize>().unwrap().bytes(), 64 * MB);
        // A bare byte count, which is what the helper speaks and what a
        // volume's own file length reports back.
        assert_eq!(
            "67108864".parse::<VolumeSize>().unwrap().bytes(),
            67_108_864
        );
        assert_eq!(
            "67108864B".parse::<VolumeSize>().unwrap().bytes(),
            67_108_864
        );
    }

    /// EVERY size must survive a round trip through its own spelling: this is
    /// what the bundle manifest records and what the daemon puts on the wire,
    /// so a value that prints as something else is a volume that comes back a
    /// different size on import.
    #[test]
    fn every_size_round_trips_through_its_own_spelling() {
        for bytes in [
            64 * MB,
            500 * MB,
            1536 * MB,
            2 * GB,
            7 * GB,
            10 * GB,
            // Not a whole MB: the case that forces the byte spelling.
            2 * GB + 4096,
            67_108_864 + 1,
            1,
        ] {
            let size = VolumeSize::from_bytes(bytes);
            let back: VolumeSize = size.as_str().parse().unwrap_or_else(|e| {
                panic!(
                    "{bytes} printed as {:?} and did not parse back: {e}",
                    size.as_str()
                )
            });
            assert_eq!(back, size, "{bytes} printed as {:?}", size.as_str());
        }
    }

    #[test]
    fn something_that_is_not_a_size_is_rejected() {
        for bad in [
            "", "  ", "GB", "MB", "-1GB", "2.5GB", "2 GB x", "two GB", "0x10",
        ] {
            assert!(
                bad.parse::<VolumeSize>().is_err(),
                "{bad:?} must be rejected"
            );
        }
        let error = "two GB".parse::<VolumeSize>().unwrap_err().to_string();
        assert!(
            error.contains("2GB") && error.contains("byte count"),
            "the refusal must show the shape it wants: {error}"
        );
    }

    /// The bounds are the helper's, and the engine applies them without
    /// keeping a copy: these are all built from a `SizeLimits` handed in.
    #[test]
    fn a_size_out_of_range_is_refused_naming_the_figure() {
        let limits = SizeLimits {
            min: 64 * MB,
            max: 1024 * GB,
            block: 4096,
            free: 100 * GB,
        };
        assert_eq!(limits.refuse(VolumeSize::from_bytes(2 * GB)), None);
        let below = limits.refuse(VolumeSize::from_bytes(MB)).unwrap();
        assert!(below.contains("64.0MiB"), "{below}");
        let above = limits.refuse(VolumeSize::from_bytes(2048 * GB)).unwrap();
        assert!(above.contains("1024.0GiB"), "{above}");
        // Free disk, which neither bound covers — and the figure is the one
        // the user has to stay under.
        let full = limits.refuse(VolumeSize::from_bytes(200 * GB)).unwrap();
        assert!(full.contains("100.0GiB") && full.contains("free"), "{full}");
    }

    #[test]
    fn rounding_matches_the_helper_and_never_rounds_up() {
        let limits = SizeLimits {
            min: 64 * MB,
            max: 1024 * GB,
            block: 4096,
            free: 100 * GB,
        };
        assert_eq!(limits.round(VolumeSize::from_bytes(4096)).bytes(), 4096);
        assert_eq!(limits.round(VolumeSize::from_bytes(4097)).bytes(), 4096);
        assert_eq!(limits.round(VolumeSize::from_bytes(8191)).bytes(), 4096);
        // Whole MB and GB are already block multiples, so the prompt path
        // never changes what was typed.
        for size in [VolumeSize::SMALL, VolumeSize::MEDIUM, VolumeSize::LARGE] {
            assert_eq!(limits.round(size), size);
        }
    }

    /// The bounds must come from the helper's own output, and a helper that
    /// does not report one is an error rather than a default — a default here
    /// would be the engine quietly deciding a bound it does not enforce.
    #[test]
    fn the_bounds_are_read_from_the_helper_and_never_defaulted() {
        let good = "min=67108864\nmax=1099511627776\nblock=4096\n";
        assert_eq!(
            parse_bounds(good).unwrap(),
            SizeBounds {
                min: 67_108_864,
                max: 1_099_511_627_776,
                block: 4096
            }
        );
        for missing in [
            "max=1\nblock=4096\n",
            "min=1\nblock=4096\n",
            "min=1\nmax=2\n",
            "",
            "min=sixty\nmax=2\nblock=4096\n",
        ] {
            let error = parse_bounds(missing).unwrap_err().to_string();
            assert!(
                error.contains("helper"),
                "the refusal must point at the helper: {error}"
            );
        }
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
        let u = Usage {
            used: 100,
            available: 300,
            total: 500,
        };
        // df: 100 / (100 + 300) = 25%, NOT 100/500 = 20%.
        assert!(
            (u.percent() - 25.0).abs() < 1e-9,
            "percent should be 25.0, got {}",
            u.percent()
        );
    }

    #[test]
    fn usage_percent_is_zero_on_empty_filesystem() {
        let u = Usage {
            used: 0,
            available: 0,
            total: 0,
        };
        assert_eq!(
            u.percent(),
            0.0,
            "an empty/degenerate fs must not divide by zero"
        );
    }

    #[test]
    fn usage_percent_full_is_100() {
        let u = Usage {
            used: 400,
            available: 0,
            total: 500,
        };
        assert!(
            (u.percent() - 100.0).abs() < 1e-9,
            "no space available reads as 100%"
        );
    }

    #[test]
    fn is_mounted_decodes_octal_escaped_mount_points() {
        // A real mountinfo line for a mount point containing a space, exactly as
        // the kernel escapes it (\040), with two optional fields before " - ".
        let table = "301 29 7:19 / /home/john\\040doe/.local/share/nemr/mounts/p \
                     rw,relatime shared:277 master:2 - ext4 /dev/loop7 rw\n";

        assert!(
            mountinfo_has_target(
                table,
                Path::new("/home/john doe/.local/share/nemr/mounts/p")
            ),
            "a mount point with a space must be recognised despite the \\040 escape"
        );
        assert!(
            !mountinfo_has_target(
                table,
                Path::new("/home/john doe/.local/share/nemr/mounts/other")
            ),
            "a different path must not match"
        );
        // The naive (broken) comparison would have matched the escaped form:
        assert!(
            !mountinfo_has_target(
                table,
                Path::new("/home/john\\040doe/.local/share/nemr/mounts/p")
            ),
            "the escaped literal must NOT match — that was the bug"
        );
    }

    /// The device parser backs the VOL-05 debug line.
    ///
    /// Field 5 is the target; the source sits after the " - " separator, so the
    /// optional-fields section must be **skipped**, not counted past. The
    /// previous version tested a single line shape with two optional fields, so
    /// a fixed-index implementation (`split(' ').nth(10)`) passed it while
    /// returning the wrong token on this host's real mountinfo, which has one
    /// optional field (F-58). Every optional-field count is now covered.
    #[test]
    fn mountinfo_device_is_read_after_the_separator_at_any_optional_count() {
        // Real shapes: zero, one and two optional fields, each a different device.
        let table = concat!(
            "20 25 0:19 / /zero rw,nosuid - tmpfs tmpfs-zero rw\n",
            "24 29 0:22 / /one rw,nosuid shared:7 - sysfs sysfs-one rw\n",
            "301 29 7:19 / /two rw,relatime shared:277 master:2 - ext4 /dev/loop7 rw\n",
        );
        for (mount_point, expected) in [
            ("/zero", "tmpfs-zero"),
            ("/one", "sysfs-one"),
            ("/two", "/dev/loop7"),
        ] {
            assert_eq!(
                mountinfo_device_for(table, Path::new(mount_point)),
                Some(expected.to_string()),
                "{mount_point} must resolve past its optional fields"
            );
        }
        // A path that is not a mount point has no device — the VOL-05 condition.
        assert_eq!(mountinfo_device_for(table, Path::new("/absent")), None);
    }

    /// The parser must agree with this host's actual /proc/self/mountinfo.
    #[test]
    fn mountinfo_device_matches_this_host() {
        let Ok(table) = std::fs::read_to_string("/proc/self/mountinfo") else {
            return;
        };
        // Control: the root mount always exists, so a None here means the parser
        // is broken rather than that the mount is absent.
        assert!(
            mountinfo_device_for(&table, Path::new("/")).is_some(),
            "the parser must find a device for / on a real mountinfo"
        );
    }

    /// F-28 — the identity decision that gates `start`.
    #[test]
    fn mount_identity_accepts_only_the_projects_own_image() {
        let expected = Path::new("/v/demo.img");

        assert!(mount_identity(expected, Some(expected)).is_ok());

        assert_eq!(
            mount_identity(expected, Some(Path::new("/v/other.img"))),
            Err(MountIdentityError::WrongVolume {
                actual: Path::new("/v/other.img").to_path_buf()
            }),
            "a different image must be refused: accepting it hands the container someone \
             else's volume, silently"
        );

        assert_eq!(
            mount_identity(expected, None),
            Err(MountIdentityError::NotLoopBacked),
            "a mount that is not loop-backed is not a nemr volume"
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

    /// F-128 — the propagation guard that turns the WSL2 mount failure from an
    /// opaque runc error into a named refusal. The parser *is* the guard; these
    /// cases go red if it misreads the optional-fields section, which is exactly
    /// where the `shared:` tag lives.
    #[test]
    fn mountinfo_propagation_reads_the_shared_tag_at_any_optional_count() {
        let mp = "/home/u/.local/share/nemr/mounts/p";
        // Shared: the invariant a standard systemd host provides and nemr needs.
        let shared = format!("301 29 7:19 / {mp} rw,relatime shared:277 - ext4 /dev/loop7 rw\n");
        assert_eq!(
            mountinfo_propagation(&shared, Path::new(mp)),
            MountPropagation::Shared
        );
        // Shared with a trailing master tag (a shared mount that also receives).
        let shared_master =
            format!("301 29 7:19 / {mp} rw,relatime shared:277 master:2 - ext4 /dev/loop7 rw\n");
        assert_eq!(
            mountinfo_propagation(&shared_master, Path::new(mp)),
            MountPropagation::Shared
        );
        // The WSL2 case: no optional tags at all. A mount made here does not
        // reach rootlesskit's rslave namespace, so runc cannot see the M8 bind
        // sources — the exact twelve-test failure the spike found.
        let private = format!("301 29 7:19 / {mp} rw,relatime - ext4 /dev/loop7 rw\n");
        assert_eq!(
            mountinfo_propagation(&private, Path::new(mp)),
            MountPropagation::Private
        );
        // `master:` alone is a slave, not shared — it receives but does not
        // re-emit, so it does not carry a mount onward to a further slave.
        let slave = format!("301 29 7:19 / {mp} rw,relatime master:2 - ext4 /dev/loop7 rw\n");
        assert_eq!(
            mountinfo_propagation(&slave, Path::new(mp)),
            MountPropagation::Private
        );
        // Absent target: nothing to judge.
        assert_eq!(
            mountinfo_propagation(&shared, Path::new("/home/u/.local/share/nemr/mounts/other")),
            MountPropagation::Unknown
        );
    }

    /// Octal-escaped mount points must decode before propagation is read, or a
    /// `$HOME` with a space reads the wrong line — the same defect the F-58/F-28
    /// parsers fixed, here for propagation.
    #[test]
    fn mountinfo_propagation_decodes_escaped_mount_points() {
        let table =
            "301 29 7:19 / /home/john\\040doe/mounts/p rw,relatime shared:277 - ext4 /dev/loop7 rw\n";
        assert_eq!(
            mountinfo_propagation(table, Path::new("/home/john doe/mounts/p")),
            MountPropagation::Shared
        );
    }

    /// Control: the parser must agree with this host's real mount namespace.
    /// `/` always has a mountinfo line, so a decisive (non-`Unknown`) answer
    /// proves the live `/proc` read path works. A shared `/` here is *why* the
    /// twelve tests pass on the reference host and failed on WSL2.
    #[test]
    fn mount_propagation_is_decisive_for_this_host_root() {
        if std::fs::read_to_string("/proc/self/mountinfo").is_err() {
            return;
        }
        assert_ne!(mount_propagation(Path::new("/")), MountPropagation::Unknown);
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
