//! `nemr-volume` — the privileged half of Nemr volume provisioning.
//!
//! Installed root-owned at a fixed path and invoked through a NOPASSWD sudo
//! rule scoped to exactly that path, with no argument wildcards (PRIV-03).
//!
//! # Threat model
//!
//! The invoking user is *not* trusted. They can run this binary as root with
//! arbitrary arguments and a partially controlled environment, so every
//! security property must hold inside this program:
//!
//! - **The caller never supplies a path, device, or UID.** They supply a
//!   volume name and a size preset. Everything else is derived here.
//! - **Names are validated before use.** The engine validates too, but that
//!   check is advisory — this one is the boundary.
//! - **No caller-controlled environment variable influences path
//!   construction.** In particular `XDG_DATA_HOME` and `HOME` are ignored;
//!   the invoking user's home directory comes from `/etc/passwd`.
//! - **Symlinks are refused.** The backing file must be a regular file, so a
//!   symlink cannot redirect `losetup` at a device the user should not reach.
//! - **Paths are re-checked after canonicalisation**, so no resolved path can
//!   escape the managed directory.
//!
//! # Usage
//!
//! ```text
//! nemr-volume mount   <name> <500MB|2GB|10GB>
//! nemr-volume unmount <name>
//! ```

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Absolute paths. Never resolved via `PATH`, which the caller could influence.
const LOSETUP: &str = "/usr/sbin/losetup";
const MOUNT: &str = "/usr/bin/mount";
const UMOUNT: &str = "/usr/bin/umount";

const MAX_NAME_LEN: usize = 32;
const VALID_SIZES: [&str; 3] = ["500MB", "2GB", "10GB"];

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nemr-volume: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let invoker = Invoker::from_sudo_env()?;

    match args.first().map(String::as_str) {
        Some("mount") => {
            let name = require_arg(&args, 1, "name")?;
            let size = require_arg(&args, 2, "size")?;
            reject_extra_args(&args, 3)?;
            cmd_mount(&invoker, name, size)
        }
        Some("unmount") => {
            let name = require_arg(&args, 1, "name")?;
            reject_extra_args(&args, 2)?;
            cmd_unmount(&invoker, name)
        }
        Some(other) => Err(format!(
            "unknown subcommand {other:?}; expected 'mount' or 'unmount'"
        )),
        None => Err(
            "usage: nemr-volume mount <name> <500MB|2GB|10GB> | nemr-volume unmount <name>"
                .to_string(),
        ),
    }
}

fn require_arg<'a>(args: &'a [String], index: usize, what: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("missing required argument: {what}"))
}

/// Refuse unexpected trailing arguments rather than ignoring them.
fn reject_extra_args(args: &[String], expected: usize) -> Result<(), String> {
    if args.len() > expected {
        return Err(format!(
            "unexpected extra argument {:?}",
            args[expected]
        ));
    }
    Ok(())
}

/// The unprivileged user who invoked us through sudo.
struct Invoker {
    uid: u32,
    gid: u32,
    home: PathBuf,
}

impl Invoker {
    /// Identify the caller from `SUDO_UID`/`SUDO_GID`.
    ///
    /// sudo sets these to the *real* invoking user and they cannot be forged
    /// by the caller — sudo overwrites them regardless of what the caller's
    /// environment contained. Refusing to run without them also means this
    /// binary does nothing if executed directly as root by mistake.
    fn from_sudo_env() -> Result<Self, String> {
        let uid: u32 = std::env::var("SUDO_UID")
            .map_err(|_| "SUDO_UID is not set; this helper must be invoked via sudo".to_string())?
            .parse()
            .map_err(|_| "SUDO_UID is not a valid uid".to_string())?;
        let gid: u32 = std::env::var("SUDO_GID")
            .map_err(|_| "SUDO_GID is not set; this helper must be invoked via sudo".to_string())?
            .parse()
            .map_err(|_| "SUDO_GID is not a valid gid".to_string())?;

        if uid == 0 {
            return Err("refusing to operate on behalf of root".to_string());
        }

        Ok(Self {
            uid,
            gid,
            home: home_dir_of(uid)?,
        })
    }

    /// Managed root for this user's volumes.
    ///
    /// Derived from `/etc/passwd`, never from `HOME` or `XDG_DATA_HOME`: those
    /// are caller-controlled and would let the caller point privileged
    /// operations anywhere on the filesystem.
    fn managed_root(&self) -> PathBuf {
        self.home.join(".local").join("share").join("nemr")
    }

    fn image_file(&self, name: &str) -> PathBuf {
        self.managed_root().join("volumes").join(format!("{name}.img"))
    }

    fn mount_point(&self, name: &str) -> PathBuf {
        self.managed_root().join("mounts").join(name)
    }
}

/// Look up a uid's home directory in `/etc/passwd`.
fn home_dir_of(uid: u32) -> Result<PathBuf, String> {
    let passwd = fs::read_to_string("/etc/passwd")
        .map_err(|e| format!("cannot read /etc/passwd: {e}"))?;

    for line in passwd.lines() {
        // name:passwd:uid:gid:gecos:home:shell
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 {
            continue;
        }
        if fields[2].parse::<u32>() == Ok(uid) {
            let home = PathBuf::from(fields[5]);
            if !home.is_absolute() {
                return Err(format!("home directory for uid {uid} is not absolute"));
            }
            return Ok(home);
        }
    }
    Err(format!("no /etc/passwd entry for uid {uid}"))
}

/// Validate a volume name (PRIV-03).
///
/// Independent of the engine's identical check. The engine's exists to give a
/// good error early; this one is the security boundary and must not assume the
/// caller ran the other.
///
/// The character set excludes `/`, `.`, whitespace and every shell
/// metacharacter, so a validated name cannot traverse directories or alter the
/// meaning of a constructed path.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("volume name must not be empty".to_string());
    }
    if name.len() > MAX_NAME_LEN {
        return Err(format!(
            "volume name is {} characters; maximum is {MAX_NAME_LEN}",
            name.len()
        ));
    }
    let first = name.chars().next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err("volume name must start with a lowercase letter or digit".to_string());
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
    {
        return Err(format!(
            "volume name contains {bad:?}; only lowercase letters, digits and '-' are allowed"
        ));
    }
    Ok(())
}

fn validate_size(size: &str) -> Result<(), String> {
    if VALID_SIZES.contains(&size) {
        Ok(())
    } else {
        Err(format!(
            "unrecognised size {size:?}; valid sizes: {}",
            VALID_SIZES.join(", ")
        ))
    }
}

/// Audit line (PRIV-04, NFR-04).
///
/// Every privileged action states what it did and that it was elevated, so an
/// operator can reconstruct events without reading source. Goes to stderr; the
/// engine captures and re-logs these.
fn audit(message: &str) {
    eprintln!("[elevated] {message}");
}

/// Confirm `path` is a regular file inside `managed_root`, and return its
/// canonical form.
///
/// `symlink_metadata` does not follow links, so a symlink is rejected outright
/// rather than followed to a device node. Canonicalising and re-checking the
/// prefix afterwards catches anything the name validation did not.
fn resolve_regular_file(path: &Path, managed_root: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;

    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; refusing to operate on it",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }

    let canonical = path
        .canonicalize()
        .map_err(|e| format!("cannot canonicalise {}: {e}", path.display()))?;

    // Defence in depth: the name is already validated, so this should be
    // unreachable. It costs nothing and would catch a mistake elsewhere.
    if !canonical.starts_with(managed_root) {
        return Err(format!(
            "{} resolves outside the managed directory {}",
            canonical.display(),
            managed_root.display()
        ));
    }
    Ok(canonical)
}

/// Run a command with a fixed environment, returning trimmed stdout.
fn run_command(program: &str, args: &[&str]) -> Result<String, String> {
    audit(&format!("{program} {}", args.join(" ")));

    let output = Command::new(program)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .output()
        .map_err(|e| format!("failed to execute {program}: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "{program} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Loop device currently backing `image`, if any.
///
/// Scans the full loop table instead of using `losetup -j <path>`. If the
/// backing file has been unlinked, `-j` matches nothing while the device stays
/// attached — the kernel reports it as `/path/to/file.img (deleted)`. Matching
/// on that prefix lets us still detach it, so an interrupted provisioning run
/// cannot strand a loop device permanently (NFR-03).
///
/// The path is derived internally from a validated name, never supplied by the
/// caller, so this cannot be steered at an unrelated device.
fn loop_device_for(image: &Path) -> Result<Option<String>, String> {
    let table = run_command(LOSETUP, &["-l", "-O", "NAME,BACK-FILE", "-n"])?;
    let wanted = image.to_string_lossy().to_string();

    for line in table.lines() {
        let mut fields = line.split_whitespace();
        let (Some(device), Some(back_file)) = (fields.next(), fields.next()) else {
            continue;
        };
        // A deleted backing file is reported as "/path/to/file.img (deleted)",
        // so the marker lands in its own whitespace-separated token and the
        // path compares equal either way.
        if back_file == wanted {
            return Ok(Some(device.to_string()));
        }
    }
    Ok(None)
}

fn is_mounted(mount_point: &Path) -> bool {
    // Read the kernel's mount table directly rather than shelling out, so this
    // works identically on error paths where a command might fail.
    fs::read_to_string("/proc/self/mountinfo")
        .map(|table| {
            let target = mount_point.to_string_lossy().to_string();
            table
                .lines()
                .any(|line| line.split(' ').nth(4).is_some_and(|m| m == target))
        })
        .unwrap_or(false)
}

/// Attach the backing file to a loop device, mount it, and hand ownership to
/// the invoking user — as one operation (PRIV-06).
fn cmd_mount(invoker: &Invoker, name: &str, size: &str) -> Result<(), String> {
    validate_name(name)?;
    validate_size(size)?;

    let managed_root = invoker.managed_root();
    let image = invoker.image_file(name);
    let mount_point = invoker.mount_point(name);

    let image = resolve_regular_file(&image, &managed_root)?;

    // The backing file must belong to the invoker. Without this, a user could
    // ask us to attach another user's file.
    let owner = fs::metadata(&image)
        .map_err(|e| format!("cannot stat {}: {e}", image.display()))?
        .uid();
    if owner != invoker.uid {
        return Err(format!(
            "{} is owned by uid {owner}, not the invoking user {}",
            image.display(),
            invoker.uid
        ));
    }

    if !mount_point.is_dir() {
        return Err(format!(
            "mount point {} does not exist; the engine creates it before calling",
            mount_point.display()
        ));
    }
    if is_mounted(&mount_point) {
        return Err(format!("{} is already mounted", mount_point.display()));
    }

    audit(&format!(
        "provisioning volume {name:?} ({size}) for uid {} at {}",
        invoker.uid,
        mount_point.display()
    ));

    let device = run_command(
        LOSETUP,
        &["--find", "--show", &image.to_string_lossy()],
    )?;
    audit(&format!("attached {} to {device}", image.display()));

    let mount_target = mount_point.to_string_lossy().to_string();
    if let Err(error) = run_command(MOUNT, &[&device, &mount_target]) {
        // Do not leak the loop device if mounting fails (NFR-03).
        audit(&format!("mount failed, detaching {device}"));
        let _ = run_command(LOSETUP, &["-d", &device]);
        return Err(error);
    }
    audit(&format!("mounted {device} at {mount_target}"));

    // PRIV-06. A freshly formatted ext4 has a root-owned root inode; inside the
    // rootless container's user namespace host uid 0 is unmapped and appears as
    // nobody, so the container could not write to its own volume. Handing the
    // mount to the invoking user's uid makes it appear as uid 0 — root — inside
    // the container, because the namespace maps host uid <invoker> to 0.
    //
    // The uid is taken from SUDO_UID, never from an argument: accepting a
    // caller-supplied uid would turn this into a general-purpose chown.
    if let Err(error) = std::os::unix::fs::chown(&mount_point, Some(invoker.uid), Some(invoker.gid))
    {
        // A mounted-but-unchowned volume is useless to the container and would
        // be an orphaned mount plus loop device if we simply returned here
        // (NFR-03). Unwind both before reporting.
        audit(&format!("chown failed, unwinding mount and loop device"));
        let _ = run_command(UMOUNT, &[&mount_target]);
        let _ = run_command(LOSETUP, &["-d", &device]);
        return Err(format!(
            "mounted at {mount_target} but failed to chown to {}:{}: {error}. \
             Mount and loop device were released.",
            invoker.uid, invoker.gid
        ));
    }
    audit(&format!(
        "chowned {mount_target} to {}:{} (maps to root inside the container)",
        invoker.uid, invoker.gid
    ));

    Ok(())
}

/// Unmount and detach. Idempotent: the engine's `Drop` calls this on error
/// paths where the mount may never have been established (VOL-04).
fn cmd_unmount(invoker: &Invoker, name: &str) -> Result<(), String> {
    validate_name(name)?;

    let mount_point = invoker.mount_point(name);
    let image = invoker.image_file(name);
    let mount_target = mount_point.to_string_lossy().to_string();

    if is_mounted(&mount_point) {
        run_command(UMOUNT, &[&mount_target])?;
        audit(&format!("unmounted {mount_target}"));
    } else {
        audit(&format!("{mount_target} was not mounted; nothing to unmount"));
    }

    // Detach by looking the device up from the image, rather than trusting a
    // caller-supplied device name.
    match loop_device_for(&image)? {
        Some(device) => {
            run_command(LOSETUP, &["-d", &device])?;
            audit(&format!("detached {device}"));
        }
        None => audit(&format!(
            "no loop device attached to {}; nothing to detach",
            image.display()
        )),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_injection_names() {
        for name in [
            "../etc", "..", ".", "a/b", "/abs", "a b", "a;b", "a$b", "a\\b", "a\nb", "UPPER", "-lead",
            "a.img", "",
        ] {
            assert!(validate_name(name).is_err(), "{name:?} must be rejected");
        }
    }

    #[test]
    fn accepts_plain_names() {
        for name in ["a", "1", "my-project", "proj-2"] {
            assert!(validate_name(name).is_ok(), "{name:?} should be accepted");
        }
    }

    #[test]
    fn name_length_is_bounded() {
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN)).is_ok());
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn only_preset_sizes_accepted() {
        for size in VALID_SIZES {
            assert!(validate_size(size).is_ok());
        }
        for size in ["1TB", "2gb", "", "2GB;rm", "999999GB"] {
            assert!(validate_size(size).is_err(), "{size:?} must be rejected");
        }
    }

    /// Paths must derive from the passwd home, never from the environment.
    #[test]
    fn paths_derive_from_invoker_home() {
        let invoker = Invoker {
            uid: 1000,
            gid: 1000,
            home: PathBuf::from("/home/someone"),
        };
        assert_eq!(
            invoker.image_file("demo"),
            PathBuf::from("/home/someone/.local/share/nemr/volumes/demo.img")
        );
        assert_eq!(
            invoker.mount_point("demo"),
            PathBuf::from("/home/someone/.local/share/nemr/mounts/demo")
        );
    }

    #[test]
    fn extra_arguments_are_refused() {
        let args: Vec<String> = ["mount", "a", "2GB", "extra"].iter().map(|s| s.to_string()).collect();
        assert!(reject_extra_args(&args, 3).is_err());
    }
}
