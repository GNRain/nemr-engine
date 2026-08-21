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
//! - **Every managed path is resolved to a file descriptor, refusing any
//!   symlinked component**, and the privileged syscalls act on the descriptor
//!   (`/proc/self/fd/<n>`), never on a name that could be re-resolved. This
//!   closes the check-to-use races that let a caller redirect a privileged
//!   `mount`/`losetup`/`chown` off the managed directory. See `safe.rs`.
//! - **The whole mount sequence is serialised** under an advisory lock on the
//!   backing file, so two concurrent callers cannot each attach a separate loop
//!   device to the same image.
//!
//! # Usage
//!
//! ```text
//! nemr-volume mount   <name> <500MB|2GB|10GB>
//! nemr-volume unmount <name>
//! ```

mod loopdev;
mod safe;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

const MAX_NAME_LEN: usize = 32;
const VALID_SIZES: [&str; 3] = ["500MB", "2GB", "10GB"];

/// Interface version between the engine and this helper.
///
/// The engine checks this before invoking a privileged operation and refuses to
/// run against a helper whose protocol it does not understand, so a source
/// change that was never installed (`scripts/setup_test_host.sh`) is caught
/// rather than silently ignored. Bump it whenever the argument grammar or the
/// helper's guarantees change.
///
/// - 1: initial mount/unmount grammar (implicit; helpers without a `version`
///   subcommand predate the fd-based hardening).
/// - 2: symlink-safe fd-based mount/chown, backing-file flock, inode-identity
///   loop lookup.
const PROTOCOL_VERSION: u32 = 2;

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

    // `version` is answered before identifying the invoker: the engine calls it
    // as a preflight handshake and it touches nothing privileged, so it must
    // work even when run directly. Print a stable, parseable line.
    if args.first().map(String::as_str) == Some("version") {
        println!("nemr-volume protocol {PROTOCOL_VERSION}");
        return Ok(());
    }

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
            "unknown subcommand {other:?}; expected 'mount', 'unmount' or 'version'"
        )),
        None => Err(
            "usage: nemr-volume mount <name> <500MB|2GB|10GB> | nemr-volume unmount <name> \
             | nemr-volume version"
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
    home_dir_from_passwd(&passwd, uid)
}

/// Parse a passwd table for `uid`'s home directory.
///
/// Split out so the security property — the home comes from passwd, never from
/// a caller-controlled `HOME`/`XDG_DATA_HOME`, and must be absolute — is
/// testable. Previously the only test hand-built an `Invoker` and never reached
/// this function, so the derivation could have been rewritten to read the
/// environment with the suite staying green (F-58).
fn home_dir_from_passwd(passwd: &str, uid: u32) -> Result<PathBuf, String> {
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

/// Attach the backing file to a loop device, mount it, and hand ownership to
/// the invoking user — as one operation (PRIV-06).
///
/// # Security
///
/// Every path this touches lives inside the invoking user's home directory,
/// which the (untrusted) caller owns. So nothing is re-opened by name after
/// validation: the backing file and the mount point are each resolved once, to
/// a file descriptor, refusing any symlinked component ([`safe::open_beneath`]),
/// and `losetup`/`mount`/`fchown` all act on `/proc/self/fd/<n>`, which the
/// kernel resolves to the pinned inode. A concurrent symlink swap therefore has
/// nothing to redirect. The whole sequence runs under a `/run/nemr` flock so two
/// concurrent callers cannot each attach a separate loop device to the same
/// backing file. See src/safe.rs for the full rationale.
fn cmd_mount(invoker: &Invoker, name: &str, size: &str) -> Result<(), String> {
    validate_name(name)?;
    validate_size(size)?;

    let image_path = invoker.image_file(name);
    let mount_path = invoker.mount_point(name);

    // Resolve the backing file to a pinned descriptor, refusing a symlink
    // anywhere in the path. O_RDWR because the loop device must be writable for
    // an ext4 read-write mount.
    let image_fd = safe::open_beneath(&image_path, libc::O_RDWR)?;

    // Serialise the whole check-then-mount against a concurrent start of the
    // same project (double-attach / double-mount corruption). The lock is on
    // the backing file itself, so it needs no privileged lock directory.
    let _lock = safe::FileLock::acquire(&image_fd)?;

    let image_stat = safe::fstat(&image_fd)?;
    if image_stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(format!("{} is not a regular file", image_path.display()));
    }
    if image_stat.st_uid != invoker.uid {
        return Err(format!(
            "{} is owned by uid {}, not the invoking user {}",
            image_path.display(),
            image_stat.st_uid,
            invoker.uid
        ));
    }

    // Resolve the mount point's parent to a pinned descriptor, again refusing
    // symlinks. The child is opened relative to it, so the mount target cannot
    // be redirected out of the managed directory (this is the /etc-shadowing
    // escalation's fix).
    let (parent_fd, child) = safe::open_parent_and_name(&mount_path)?;
    let mount_dir_fd = safe::openat_dir(&parent_fd, &child)?;
    let mount_dir_stat = safe::fstat(&mount_dir_fd)?;
    if mount_dir_stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(format!(
            "mount point {} is not a directory; the engine creates it before calling",
            mount_path.display()
        ));
    }
    if safe::is_mounted(&mount_path) {
        return Err(format!("{} is already mounted", mount_path.display()));
    }

    audit(&format!(
        "provisioning volume {name:?} ({size}) for uid {} at {}",
        invoker.uid,
        mount_path.display()
    ));

    // Attach the loop device to the pinned backing-file descriptor, in-process
    // via ioctl — no subprocess, so no second path resolution and no fd table
    // to lose across an exec.
    let loop_number = loopdev::attach(&image_fd)?;
    let device = loopdev::device_path(loop_number);
    audit(&format!("attached {} to {device}", image_path.display()));

    // Mount onto the pinned mount-point descriptor, in-process. `/proc/self/fd`
    // resolves against the helper here, so even if the caller swaps the
    // mount-point name for a symlink now, the target still resolves to the inode
    // we validated. The loop device path is root-owned, not caller-controlled.
    let mount_target = safe::proc_fd_path(&mount_dir_fd);
    if let Err(error) = safe::mount_ext4(&device, &mount_target) {
        audit(&format!("mount failed, detaching {device}"));
        let _ = loopdev::detach(loop_number);
        return Err(error);
    }
    audit(&format!("mounted {device} at {}", mount_path.display()));

    // PRIV-06. A freshly formatted ext4 has a root-owned root inode; inside the
    // rootless container's user namespace host uid 0 is unmapped and appears as
    // nobody, so the container could not write to its own volume. Handing the
    // mount to the invoking user's uid makes it appear as uid 0 inside the
    // container. Re-open the mount point through the pinned parent descriptor so
    // the fchown lands on the *mounted* root (openat crosses into the mount) and
    // cannot be redirected — chown-by-path here was itself an escalation.
    let chown_result = safe::openat_dir(&parent_fd, &child)
        .and_then(|mounted_root_fd| safe::fchown(&mounted_root_fd, invoker.uid, invoker.gid));
    if let Err(error) = chown_result {
        audit("chown failed, unwinding mount and loop device");
        let _ = safe::umount(&safe::proc_child_path(&parent_fd, &child));
        let _ = loopdev::detach(loop_number);
        return Err(format!(
            "mounted but failed to chown to {}:{}: {error}. Mount and loop device were released.",
            invoker.uid, invoker.gid
        ));
    }
    audit(&format!(
        "chowned {} to {}:{} (maps to root inside the container)",
        mount_path.display(),
        invoker.uid,
        invoker.gid
    ));

    Ok(())
}

/// Unmount and detach. Idempotent: the engine's `Drop` calls this on error
/// paths where the mount may never have been established (VOL-04).
///
/// Takes the same backing-file lock as [`cmd_mount`] (when the file still
/// exists), and finds the loop device by the backing file's (device, inode)
/// identity rather than its path, so it still detaches a device whose backing
/// file was unlinked or whose path contains whitespace.
fn cmd_unmount(invoker: &Invoker, name: &str) -> Result<(), String> {
    validate_name(name)?;

    let mount_path = invoker.mount_point(name);
    let image_path = invoker.image_file(name);

    // Take the same backing-file lock as mount when the file still exists, so an
    // unmount cannot race a concurrent mount of the same project. On the delete
    // path the file may already be gone; then there is nothing to race.
    let lock_fd = safe::open_beneath(&image_path, libc::O_RDONLY).ok();
    let _lock = lock_fd.as_ref().map(safe::FileLock::acquire).transpose()?;

    // Unmount via the pinned parent descriptor so we cannot be tricked into
    // unmounting a path the caller has since redirected — and without holding a
    // descriptor *into* the mount, which would make umount fail EBUSY. The
    // parent's ancestors are pinned; the mount-point name cannot be renamed
    // while it is a mount point. Tolerate the parent being gone (cleanup paths).
    if safe::is_mounted(&mount_path) {
        match safe::open_parent_and_name(&mount_path) {
            Ok((parent_fd, child)) => {
                let target = safe::proc_child_path(&parent_fd, &child);
                safe::umount(&target)?;
                audit(&format!("unmounted {}", mount_path.display()));
            }
            Err(error) => {
                // The mount table says it is mounted but the path no longer
                // resolves cleanly. Report rather than umounting a name we
                // cannot vouch for.
                return Err(format!(
                    "{} is mounted but its path no longer resolves safely ({error}); \
                     refusing to unmount a path that may have been redirected",
                    mount_path.display()
                ));
            }
        }
    } else {
        audit(&format!(
            "{} was not mounted; nothing to unmount",
            mount_path.display()
        ));
    }

    // Detach the loop device, found by the backing file's (device, inode)
    // identity via ioctl — robust to an unlinked backing file and to whitespace
    // in the path, both of which broke the old string-parse of `losetup --list`.
    // The file may already be gone (delete unlinks it); then there is nothing to
    // look up, and a detached-and-deleted device auto-clears once its mount is
    // released. Reuse the descriptor already opened for the lock.
    match lock_fd.as_ref() {
        Some(image_fd) => match loopdev::find_by_backing(image_fd)? {
            Some(number) => {
                loopdev::detach(number)?;
                audit(&format!("detached /dev/loop{number}"));
            }
            None => audit(&format!(
                "no loop device attached to {}; nothing to detach",
                image_path.display()
            )),
        },
        None => audit(&format!(
            "backing file {} is gone; nothing to detach by inode",
            image_path.display()
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

    /// The home directory must come from passwd, never from the environment,
    /// and must be absolute.
    ///
    /// Exercises `home_dir_from_passwd` itself. The pre-existing
    /// `paths_derive_from_invoker_home` hand-builds an `Invoker`, so it never
    /// reaches the derivation and would stay green if this were rewritten to
    /// read `HOME` (F-58).
    #[test]
    fn home_is_derived_from_passwd_not_the_environment() {
        const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n\
                              nemr:x:1000:1000:Nemr:/home/nemr:/bin/bash\n";
        // Poison the environment: the derivation must ignore it entirely.
        std::env::set_var("HOME", "/tmp/attacker-controlled");
        std::env::set_var("XDG_DATA_HOME", "/tmp/attacker-controlled");

        assert_eq!(
            home_dir_from_passwd(PASSWD, 1000).unwrap(),
            PathBuf::from("/home/nemr"),
            "the home must come from passwd, not from HOME"
        );
        assert_eq!(
            home_dir_from_passwd(PASSWD, 0).unwrap(),
            PathBuf::from("/root")
        );
        assert!(
            home_dir_from_passwd(PASSWD, 4242).is_err(),
            "an unknown uid must be refused, not defaulted"
        );
    }

    /// A relative home in passwd must be refused: joining onto it would resolve
    /// against the helper's working directory, which the caller can influence.
    #[test]
    fn a_relative_home_is_refused() {
        const PASSWD: &str = "bad:x:1001:1001:Bad:relative/path:/bin/sh\n";
        let error = home_dir_from_passwd(PASSWD, 1001)
            .expect_err("a non-absolute home must be refused");
        assert!(error.contains("absolute"), "error should say why: {error}");
    }

    #[test]
    fn extra_arguments_are_refused() {
        let args: Vec<String> = ["mount", "a", "2GB", "extra"].iter().map(|s| s.to_string()).collect();
        assert!(reject_extra_args(&args, 3).is_err());
    }
}
