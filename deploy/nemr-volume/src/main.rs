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
//!   volume name and a size in bytes. Everything else is derived here.
//! - **The size is a number parsed by this program.** It stopped being a
//!   closed set of preset words in protocol 3, so this binary now parses
//!   attacker-controlled numeric input while running as root. Every parse,
//!   bound and rounding happens before any syscall, any path construction and
//!   any allocation derived from the value. See [`parse_size_bytes`].
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
//! nemr-volume mount     <name> <bytes>
//! nemr-volume unmount   <name>
//! nemr-volume normalize <bytes>
//! nemr-volume version
//! ```

mod loopdev;
mod safe;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

const MAX_NAME_LEN: usize = 32;

/// Smallest volume this helper will mount, in bytes — 64 MiB.
///
/// MEASURED, not guessed. Two things were measured on a host with the base
/// image installed:
///
/// 1. **What a session puts in the volume: nothing.** The volume is the
///    session's `/workspace`; the base image lives in containerd's own store,
///    not here. Three freshly created 500 MB volumes on the development host
///    were byte-for-byte identical to a bare `mkfs.ext4` of the same size
///    (50388992 bytes in use, all of it ext4's own metadata). So the working
///    set a new volume must hold is zero, and the binding constraint is
///    entirely ext4's overhead.
/// 2. **What ext4 costs at small sizes.** `mkfs.ext4` then `dumpe2fs`, usable
///    = (free blocks − reserved blocks) × block size:
///
///    | asked | usable | overhead |
///    |-------|--------|----------|
///    | 16 MiB | 10653696 | 37% |
///    | 32 MiB | 25534464 | 24% |
///    | 64 MiB | 55296000 | 18% |
///    | 500 MB | 447684608 | 15% |
///
/// 64 MiB is where the overhead stops dominating and 52.7 MiB is left for a
/// checkout — three times what the largest observed fresh session had written.
/// Below it a volume is mostly journal.
const MIN_BYTES: u64 = 64 * 1024 * 1024;

/// Largest volume this helper will mount, in bytes — 1 TiB.
///
/// A ceiling, not a capacity promise. The backing file is sparse, so an absurd
/// size costs no disk until it is written to — which is exactly why an
/// unbounded value is dangerous: `--size 1000000GB` would succeed quietly,
/// format, mount, and then fail with ENOSPC in the middle of somebody's work.
/// Free disk is checked by the caller against the real filesystem; this bound
/// is the backstop that keeps a typo from ever reaching `mkfs`.
const MAX_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

/// Interface version between the engine and this helper.
///
/// The engine checks this once per process, before its first privileged call,
/// and refuses to run against a helper whose protocol it does not speak — so a
/// source change that was never installed (`scripts/setup_test_host.sh`) is
/// caught rather than silently ignored. Bump it whenever the argument grammar
/// or the helper's guarantees change.
///
/// (Until protocol 3 that sentence was aspirational: nothing in the engine
/// read this number. `HelperOps::PROTOCOL` is the other half of it.)
///
/// - 1: initial mount/unmount grammar (implicit; helpers without a `version`
///   subcommand predate the fd-based hardening).
/// - 2: symlink-safe fd-based mount/chown, backing-file flock, inode-identity
///   loop lookup.
/// - 3: `mount` takes a size in BYTES instead of one of three preset words,
///   and `normalize` is added so a caller can ask what this helper will accept
///   and what a value rounds to before it allocates anything. The request
///   shape changed, so the version moves and every host needs the redeploy.
const PROTOCOL_VERSION: u32 = 3;

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
        Some("normalize") => {
            let size = require_arg(&args, 1, "size")?;
            reject_extra_args(&args, 2)?;
            cmd_normalize(&invoker, size)
        }
        Some(other) => Err(format!(
            "unknown subcommand {other:?}; expected 'mount', 'unmount', 'normalize' or 'version'"
        )),
        None => Err(USAGE.to_string()),
    }
}

const USAGE: &str = "usage: nemr-volume mount <name> <bytes> | nemr-volume unmount <name> \
                     | nemr-volume normalize <bytes> | nemr-volume version";

fn require_arg<'a>(args: &'a [String], index: usize, what: &str) -> Result<&'a str, String> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| format!("missing required argument: {what}"))
}

/// Refuse unexpected trailing arguments rather than ignoring them.
fn reject_extra_args(args: &[String], expected: usize) -> Result<(), String> {
    if args.len() > expected {
        return Err(format!("unexpected extra argument {:?}", args[expected]));
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

    fn volumes_dir(&self) -> PathBuf {
        self.managed_root().join("volumes")
    }

    fn image_file(&self, name: &str) -> PathBuf {
        self.volumes_dir().join(format!("{name}.img"))
    }

    fn mount_point(&self, name: &str) -> PathBuf {
        self.managed_root().join("mounts").join(name)
    }
}

/// Look up a uid's home directory in `/etc/passwd`.
fn home_dir_of(uid: u32) -> Result<PathBuf, String> {
    let passwd =
        fs::read_to_string("/etc/passwd").map_err(|e| format!("cannot read /etc/passwd: {e}"))?;
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

/// Parse a caller-supplied size into bytes (PRIV-03).
///
/// THIS IS THE BOUNDARY. The engine and the CLI both validate too, and both
/// checks are conveniences; this one runs as root and must assume the caller
/// ran neither.
///
/// The accepted grammar is deliberately the narrowest thing that can express a
/// byte count: **one or more ASCII digits, nothing else**. Everything a looser
/// parser would have to reason about is refused by construction rather than by
/// a rule that has to be got right:
///
/// - `+5`, `-5` — a sign is not a digit. `u64::from_str` accepts a leading
///   `+`, so this does not delegate to it for that check.
/// - `0x40`, `0o100`, `0b1` — a prefix is not a digit. There is no radix here;
///   a value is decimal or it is refused.
/// - ` 5`, `5 `, `5\n` — whitespace is not a digit. Nothing is trimmed, so a
///   caller cannot smuggle a value past a bound with padding.
/// - `5MB`, `5e9`, `5.0` — a unit, an exponent and a point are not digits.
///   Units are the CLI's vocabulary, not this program's.
/// - `99999999999999999999999` — overflow is refused as overflow, by
///   `checked_mul`/`checked_add` on each digit, so nothing wraps into a small
///   in-range number.
/// - Unicode digits (`٥`, `５`) — `is_ascii_digit`, not `is_numeric`.
///
/// Only then are the bounds applied, and only then does anything derived from
/// the value exist. No syscall, no path, no allocation runs before this
/// returns.
fn parse_size_bytes(size: &str) -> Result<u64, String> {
    if size.is_empty() {
        return Err("size must not be empty; give a whole number of bytes".to_string());
    }
    if size.len() > 20 {
        // 2^64 - 1 is 20 digits. Anything longer cannot be a u64 and is
        // refused before the accumulate loop rather than by it.
        return Err(format!(
            "size is {} characters; a byte count is at most 20 digits",
            size.len()
        ));
    }
    let mut bytes: u64 = 0;
    for c in size.chars() {
        if !c.is_ascii_digit() {
            return Err(format!(
                "size {size:?} contains {c:?}; a size is a whole number of bytes, \
                 digits only — no sign, no unit, no whitespace, no 0x prefix"
            ));
        }
        bytes = bytes
            .checked_mul(10)
            .and_then(|b| b.checked_add(u64::from(c as u8 - b'0')))
            .ok_or_else(|| format!("size {size:?} does not fit in 64 bits"))?;
    }
    if bytes < MIN_BYTES {
        return Err(format!(
            "size {bytes} is below the minimum {MIN_BYTES} ({} MiB): \
             ext4 overhead would leave almost nothing usable",
            MIN_BYTES / (1024 * 1024)
        ));
    }
    if bytes > MAX_BYTES {
        return Err(format!(
            "size {bytes} is above the maximum {MAX_BYTES} ({} GiB): \
             the backing file is sparse, so an absurd size fails later and \
             further from the cause than it does here",
            MAX_BYTES / (1024 * 1024 * 1024)
        ));
    }
    Ok(bytes)
}

/// Round a validated size DOWN to a whole number of filesystem blocks.
///
/// Down, never up: rounding up would hand back more than the caller checked
/// against free disk, and a size that grew between the check and the
/// allocation is the same class of surprise as a TOCTOU. A tail shorter than
/// one block is unusable anyway — ext4 allocates in whole blocks.
///
/// A block size that is zero or not a power of two is refused rather than
/// worked around: it means `statvfs` answered something this program does not
/// understand, and guessing at that point is how a rounding bug becomes a
/// sizing bug.
fn round_down_to_block(bytes: u64, block: u64) -> Result<u64, String> {
    if block == 0 || !block.is_power_of_two() {
        return Err(format!(
            "the filesystem reported a block size of {block}, which is not a power of two"
        ));
    }
    let rounded = bytes - (bytes % block);
    if rounded < MIN_BYTES {
        return Err(format!(
            "size {bytes} rounds down to {rounded}, below the minimum {MIN_BYTES}"
        ));
    }
    Ok(rounded)
}

/// Audit line (PRIV-04, NFR-04).
///
/// Every privileged action states what it did and that it was elevated, so an
/// operator can reconstruct events without reading source. Goes to stderr; the
/// engine captures and re-logs these.
fn audit(message: &str) {
    eprintln!("[elevated] {message}");
}

/// Answer what this helper will accept, and what a size rounds to — without
/// doing anything.
///
/// The engine allocates the backing file itself (unprivileged), so it has to
/// know the rounded length BEFORE it allocates; asking afterwards would mean
/// either a second allocation or a file that differs from what was agreed.
/// This is that question, answered by the program that enforces the answer,
/// from the real filesystem rather than from a constant the two halves would
/// have to keep in step.
///
/// Touches nothing: it opens the volumes directory read-only to ask `statvfs`
/// for its block size, and prints. Output is `key=value` lines so a caller
/// parses it without a format to keep in step either.
fn cmd_normalize(invoker: &Invoker, size: &str) -> Result<(), String> {
    let requested = parse_size_bytes(size)?;

    // Through the same symlink-refusing resolver as everything else. It is
    // read-only and unprivileged in effect, but a resolver that is only used
    // on the dangerous paths is a resolver somebody will forget to use on the
    // next one.
    let dir_fd = safe::open_beneath(&invoker.volumes_dir(), libc::O_RDONLY)?;
    let vfs = safe::fstatvfs(&dir_fd)?;
    // f_frsize is the fragment size — the unit f_blocks/f_bavail count in, and
    // the one that matters for how much of a file is a whole block. f_bsize is
    // a preferred I/O size and on some filesystems is not the allocation unit.
    let block = vfs.f_frsize as u64;
    let rounded = round_down_to_block(requested, block)?;

    println!("min={MIN_BYTES}");
    println!("max={MAX_BYTES}");
    println!("block={block}");
    println!("requested={requested}");
    println!("bytes={rounded}");
    Ok(())
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
    // PARSED AND BOUNDED FIRST, before a path is built from the name or a
    // descriptor is opened. A refusal here has touched nothing.
    let size_bytes = parse_size_bytes(size)?;

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
    // THE SIZE ARGUMENT MEANS SOMETHING NOW. Under protocol 2 it was a preset
    // word used only in the audit line, so a caller could claim any of the
    // three and nothing checked. A byte count can be checked against the file
    // that is about to be mounted, and is: the two must agree exactly, which
    // makes the audit line a statement about the mount rather than about the
    // argument.
    let actual = image_stat.st_size as u64;
    if actual != size_bytes {
        return Err(format!(
            "{} is {actual} bytes but the request says {size_bytes}; \
             the size must be the backing file's own length",
            image_path.display()
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
        "provisioning volume {name:?} ({size_bytes} bytes) for uid {} at {}",
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
    // F-77: capture the loop device from the mount table BEFORE unmounting.
    // Afterwards the mount is gone and, if the backing file was deleted, there
    // is nothing left that says which loop device to release.
    let loop_from_mount = safe::loop_number_for_mount(&mount_path);

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
        None => match loop_from_mount {
            // F-77: the backing file is gone, so inode lookup cannot work — but
            // the mount table named the device before we unmounted it. Without
            // this the loop device stays attached to a deleted, fully-allocated
            // image and the space is never reclaimed; `rm` on the image reports
            // success and frees nothing, because the kernel still holds the
            // inode open.
            //
            // Scope: this detaches a loop device that was the source of a mount
            // point under the managed directory, whose name has already been
            // validated. It is the same loop lifecycle this helper already owns
            // (it attached the device in the first place), not a new capability.
            Some(number) => {
                loopdev::detach(number)?;
                audit(&format!(
                    "backing file {} is gone; detached /dev/loop{number} identified from the \
                     mount table before unmounting",
                    image_path.display()
                ));
            }
            // Last resort: the volume may already be unmounted, so the mount
            // table says nothing either. /sys reports the backing path even
            // after the file is unlinked, and the match is anchored to this
            // project's exact image path.
            None => match loopdev::find_by_backing_path(&image_path)? {
                Some(number) => {
                    loopdev::detach(number)?;
                    audit(&format!(
                        "backing file {} is gone and it was not mounted; detached \
                         /dev/loop{number} found by backing path in /sys",
                        image_path.display()
                    ));
                }
                None => audit(&format!(
                    "backing file {} is gone and no loop device references it; nothing to detach",
                    image_path.display()
                )),
            },
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_injection_names() {
        for name in [
            "../etc", "..", ".", "a/b", "/abs", "a b", "a;b", "a$b", "a\\b", "a\nb", "UPPER",
            "-lead", "a.img", "",
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

    /// THE BOUNDARY TEST. Every one of these goes through the helper's own
    /// parser, not through the CLI — the CLI's validation is a convenience and
    /// this program must be correct with no caller at all.
    #[test]
    fn a_malformed_size_is_refused_before_anything_is_derived_from_it() {
        for size in [
            "",            // nothing
            " ",           // whitespace only
            " 67108864",   // leading space
            "67108864 ",   // trailing space
            "67108864\n",  // trailing newline
            "+67108864",   // leading plus — u64::from_str would accept this
            "-67108864",   // negative
            "0x4000000",   // hex prefix
            "0o400000000", // octal prefix
            "0b1",         // binary prefix
            "6_7108864",   // digit separator
            "67108864.0",  // a point
            "6.7e7",       // an exponent
            "64MB",        // a unit: the CLI's vocabulary, not this one
            "64 MB",
            "２００００００００", // full-width digits: is_ascii_digit, not is_numeric
            "٦٧١٠٨٨٦٤",           // arabic-indic digits
            "67108864;rm -rf /",
            "$(echo 67108864)",
            "99999999999999999999999999", // longer than 20 digits
            "18446744073709551616",       // u64::MAX + 1, exactly 20 digits
        ] {
            let error = parse_size_bytes(size)
                .expect_err(&format!("{size:?} must be refused"))
                .to_string();
            assert!(!error.is_empty(), "{size:?} was refused without saying why");
        }
    }

    #[test]
    fn an_out_of_range_size_is_refused_and_the_bound_is_named() {
        for size in ["0", "1", "1024", &(MIN_BYTES - 1).to_string()] {
            let error = parse_size_bytes(size).expect_err(&format!("{size:?} is below MIN"));
            assert!(
                error.contains(&MIN_BYTES.to_string()),
                "the refusal must name the minimum: {error}"
            );
        }
        for size in [
            &(MAX_BYTES + 1).to_string(),
            &u64::MAX.to_string(),
            "18446744073709551615",
        ] {
            let error = parse_size_bytes(size).expect_err(&format!("{size:?} is above MAX"));
            assert!(
                error.contains(&MAX_BYTES.to_string()),
                "the refusal must name the maximum: {error}"
            );
        }
    }

    #[test]
    fn a_size_in_range_is_accepted_exactly() {
        for bytes in [MIN_BYTES, MIN_BYTES + 1, 524_288_000, MAX_BYTES] {
            assert_eq!(parse_size_bytes(&bytes.to_string()), Ok(bytes));
        }
        // No overflow anywhere on the way to a legitimate large value.
        assert_eq!(parse_size_bytes("1099511627776"), Ok(MAX_BYTES));
    }

    /// The bounds themselves, asserted rather than described — MIN was
    /// measured and MAX was chosen, and both are load-bearing.
    #[test]
    fn the_bounds_are_what_was_measured() {
        assert_eq!(MIN_BYTES, 67_108_864, "64 MiB");
        assert_eq!(MAX_BYTES, 1_099_511_627_776, "1 TiB");
        assert!(MIN_BYTES < MAX_BYTES);
        assert_eq!(MIN_BYTES % 4096, 0, "MIN must survive its own rounding");
        assert_eq!(MAX_BYTES % 4096, 0);
    }

    #[test]
    fn rounding_goes_down_and_never_below_the_minimum() {
        assert_eq!(round_down_to_block(MIN_BYTES, 4096), Ok(MIN_BYTES));
        assert_eq!(round_down_to_block(MIN_BYTES + 1, 4096), Ok(MIN_BYTES));
        assert_eq!(round_down_to_block(MIN_BYTES + 4095, 4096), Ok(MIN_BYTES));
        assert_eq!(
            round_down_to_block(MIN_BYTES + 4096, 4096),
            Ok(MIN_BYTES + 4096)
        );
        // Never up: what comes back is never more than what went in.
        for bytes in [MIN_BYTES + 1, MIN_BYTES + 1234, 1_234_567_890] {
            assert!(round_down_to_block(bytes, 4096).unwrap() <= bytes);
        }
        // A block size this program does not understand is refused, not
        // guessed around.
        assert!(round_down_to_block(MIN_BYTES, 0).is_err());
        assert!(round_down_to_block(MIN_BYTES, 4095).is_err());
        // Rounding must not be a way past the minimum.
        assert!(round_down_to_block(MIN_BYTES, 1024 * 1024 * 1024).is_err());
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
        let error =
            home_dir_from_passwd(PASSWD, 1001).expect_err("a non-absolute home must be refused");
        assert!(error.contains("absolute"), "error should say why: {error}");
    }

    #[test]
    fn extra_arguments_are_refused() {
        let args: Vec<String> = ["mount", "a", "67108864", "extra"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(reject_extra_args(&args, 3).is_err());
    }
}
