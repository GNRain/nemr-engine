//! Symlink-safe path resolution and mount-table parsing for the privileged
//! helper.
//!
//! # Why this module exists
//!
//! The helper's managed directory (`~/.local/share/nemr/{volumes,mounts}`)
//! lives entirely inside the *invoking user's* home directory, which that user
//! owns and can rewrite at will. The user is untrusted (see the crate docs), so
//! the helper cannot trust any path underneath it: between the moment the
//! helper validates a path and the moment a privileged syscall re-opens it *by
//! name*, the user can swap any component for a symlink.
//!
//! The original helper validated the backing file with `symlink_metadata` and
//! then re-opened it by name at `losetup` and `mount` time. That is a classic
//! TOCTOU, and it was worse than theoretical:
//!
//! - The **mount point** was checked with `Path::is_dir`, which *follows*
//!   symlinks, and was then handed to `mount(8)` by name. A user who replaced
//!   `mounts/<name>` with a symlink to `/etc` got the helper to mount an
//!   attacker-authored ext4 over `/etc` — a straight local root escalation,
//!   demonstrated on the reference host.
//! - The **backing file** was re-opened by name after the symlink check, so a
//!   swap to `/dev/sda1` between check and `losetup` attached the host disk.
//! - The **chown** used `chown(2)` (follows symlinks) on a caller-controlled
//!   path, so a swap to a symlink-to-`/etc` chowned `/etc` to the caller.
//!
//! # The approach
//!
//! Resolve every managed path **component by component from `/`**, opening each
//! with `O_NOFOLLOW` so a symlinked component is refused rather than traversed,
//! and keeping the resulting file descriptor. All privileged syscalls then act
//! on the descriptor — via `/proc/self/fd/<n>`, which the kernel resolves to
//! the *pinned inode* regardless of what the path name points at now — so there
//! is no second name resolution to race. This is the same guarantee
//! `openat2(RESOLVE_NO_SYMLINKS)` gives; it is written out with `openat` so the
//! helper still builds on the reference host's kernel and needs no new syscall
//! wrapper.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

/// Open a path from `/`, refusing any symlinked component.
///
/// Each component is opened relative to the previous component's descriptor
/// with `O_NOFOLLOW`, so a symlink anywhere along the path — including the final
/// element — is an error, not something to follow. The returned descriptor
/// refers to a pinned inode; use [`proc_fd_path`] to name it for a syscall that
/// only takes a path, and the name cannot be redirected out from under you.
///
/// `final_flags` is OR-ed into the open of the last component (e.g. `O_RDWR`
/// for a backing file, `O_DIRECTORY | O_RDONLY` for a mount point). `O_NOFOLLOW`
/// and `O_CLOEXEC` are always added.
pub fn open_beneath(path: &Path, final_flags: libc::c_int) -> Result<OwnedFd, String> {
    let components = normal_components(path)?;
    if components.is_empty() {
        return Err(format!("{} has no path components", path.display()));
    }

    // Start from the real filesystem root, which the unprivileged user cannot
    // replace. Every step from here is relative to a descriptor, never a name.
    let mut parent = open_root()?;

    for (index, name) in components.iter().enumerate() {
        let is_last = index == components.len() - 1;
        let flags = if is_last {
            final_flags | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };

        let next = openat(&parent, name, flags).map_err(|errno| {
            if errno == libc::ELOOP {
                format!(
                    "refusing to follow a symlink at {:?} in {} — a component of a \
                     managed path is a symlink, which is how a privileged mount would \
                     be redirected off the managed directory",
                    name,
                    path.display()
                )
            } else {
                format!(
                    "cannot open {:?} in {}: {}",
                    name,
                    path.display(),
                    std::io::Error::from_raw_os_error(errno)
                )
            }
        })?;
        parent = next;
    }

    Ok(parent)
}

/// Open the mount point's *parent* directory descriptor and return it together
/// with the final component name.
///
/// The parent is resolved with the same no-symlink guarantee. The caller mounts
/// onto `openat(parent, name, ...)` and then chowns the mounted root via the
/// same `(parent, name)` pair — `openat` crosses into the mount, so the chown
/// lands on the mounted filesystem's root rather than the directory underneath
/// it, and it still cannot be redirected because `parent` is a pinned inode.
pub fn open_parent_and_name(path: &Path) -> Result<(OwnedFd, CString), String> {
    let components = normal_components(path)?;
    let (last, parents) = components
        .split_last()
        .ok_or_else(|| format!("{} has no final component", path.display()))?;

    let mut parent = open_root()?;
    for name in parents {
        parent = openat(
            &parent,
            name,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
        .map_err(|errno| {
            format!(
                "cannot open parent component {:?} of {}: {}",
                name,
                path.display(),
                std::io::Error::from_raw_os_error(errno)
            )
        })?;
    }
    Ok((parent, last.clone()))
}

/// Open a directory descriptor relative to `parent` for its child `name`,
/// refusing a symlink. Used post-mount to reach the mounted root for `fchown`.
pub fn openat_dir(parent: &OwnedFd, name: &CString) -> Result<OwnedFd, String> {
    openat(
        parent,
        name,
        libc::O_DIRECTORY | libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    )
    .map_err(|errno| {
        format!(
            "cannot open mounted directory {name:?}: {}",
            std::io::Error::from_raw_os_error(errno)
        )
    })
}

/// The `/proc/self/fd/<n>` name for a descriptor, resolved **in this process**.
///
/// A syscall made by the helper itself and handed this path resolves it to the
/// descriptor's pinned inode, so it is immune to a concurrent rename or symlink
/// swap of the original path. This only holds in-process: `/proc/self` resolves
/// against the calling process's pid, so handing this path to a *child* process
/// names an entry in the child's fd table, not the helper's — which is exactly
/// why the mount and loop operations are done via direct syscalls here rather
/// than by spawning `mount`/`losetup`.
pub fn proc_fd_path(fd: &impl AsRawFd) -> String {
    format!("/proc/self/fd/{}", fd.as_raw_fd())
}

/// The `/proc/self/fd/<parent>/<name>` path for a child of a pinned directory.
///
/// Names a directory entry relative to a descriptor whose ancestors cannot be
/// re-resolved, without holding a descriptor open *into* the child — which
/// matters for `umount`, where an open descriptor into the mount would make the
/// unmount fail `EBUSY`. Safe because the parent is pinned and, while the child
/// is a mount point, it cannot be renamed out from under us (renaming a mount
/// point is itself `EBUSY`).
pub fn proc_child_path(parent: &impl AsRawFd, name: &CString) -> String {
    format!(
        "/proc/self/fd/{}/{}",
        parent.as_raw_fd(),
        name.to_string_lossy()
    )
}

/// `mount(2)`, called in-process so a `/proc/self/fd` target resolves against
/// this process. `source` is a `/dev/loopN` device path (root-owned, not
/// caller-controlled); `target` is a `/proc/self/fd/...` path pinned to a
/// validated descriptor.
pub fn mount_ext4(source: &str, target: &str) -> Result<(), String> {
    let source = CString::new(source).map_err(|_| "source has an interior NUL".to_string())?;
    let target = CString::new(target).map_err(|_| "target has an interior NUL".to_string())?;
    let fstype = c"ext4";
    // MS_NOSUID | MS_NODEV harden the mount against the backing image, which is
    // the one thing here the caller fully controls (F-83).
    //
    // The image is allocated and formatted by the UNPRIVILEGED engine, owned by
    // the invoking user, and this helper checks only that it is a regular file
    // they own — never that its contents are trustworthy. Without these flags a
    // user can craft an ext4 image containing a setuid-root binary (no root
    // needed: `debugfs` sets inode uid/mode directly on an image file they own),
    // hand it to this helper by project name, and get it mounted with setuid
    // honoured — a local privilege escalation. `nosuid` makes setuid/setgid bits
    // inert; `nodev` stops device nodes on the image being usable.
    //
    // Exec is deliberately NOT disabled: the mount is the container's /workspace
    // and running project code from it is the whole point. Setuid binaries and
    // device nodes in a project workspace are attack surface, not a feature.
    let flags = libc::MS_NOSUID | libc::MS_NODEV;
    // SAFETY: all pointers are valid NUL-terminated strings for the call; data
    // is NULL, meaning ext4 defaults.
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(format!(
            "mount({} -> {}) failed: {}",
            source.to_string_lossy(),
            target.to_string_lossy(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// `umount2(2)`, called in-process.
pub fn umount(target: &str) -> Result<(), String> {
    let c_target = CString::new(target).map_err(|_| "target has an interior NUL".to_string())?;
    // SAFETY: c_target is a valid NUL-terminated string for the call.
    let rc = unsafe { libc::umount2(c_target.as_ptr(), 0) };
    if rc != 0 {
        return Err(format!(
            "umount({target}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// `fstat` a descriptor.
pub fn fstat(fd: &impl AsRawFd) -> Result<libc::stat, String> {
    // SAFETY: zeroed stat is a valid target; fd is valid for the call.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), &mut st) };
    if rc != 0 {
        return Err(format!("fstat failed: {}", std::io::Error::last_os_error()));
    }
    Ok(st)
}

/// `fstatvfs` a descriptor: the filesystem the descriptor lives on.
///
/// On the descriptor, never on a path, for the same reason as everything else
/// here — the caller owns every directory this program touches, so a name
/// resolved twice is a name that can change between the two.
pub fn fstatvfs(fd: &impl AsRawFd) -> Result<libc::statvfs, String> {
    // SAFETY: zeroed statvfs is a valid target; fd is valid for the call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut st) };
    if rc != 0 {
        return Err(format!(
            "fstatvfs failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(st)
}

/// `fchown` a descriptor. Used instead of `chown(path)` so the ownership change
/// cannot be redirected to a symlink target.
pub fn fchown(fd: &impl AsRawFd, uid: u32, gid: u32) -> Result<(), String> {
    let rc = unsafe { libc::fchown(fd.as_raw_fd(), uid, gid) };
    if rc != 0 {
        return Err(format!(
            "fchown failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Take an advisory exclusive lock on a descriptor's open file, held until the
/// returned guard is dropped.
///
/// Serialises the whole check-then-mount sequence so two concurrent
/// `nemr start`s of the same project cannot both pass the "already mounted"
/// check and each attach a *separate* loop device to the same backing file —
/// which stacks two independent read-write ext4 superblocks over one set of
/// bytes and corrupts the filesystem.
///
/// The lock is taken on the **backing file's own descriptor**. Two callers
/// operating on the same project open the same inode, and `flock` competes at
/// the open-file level across independent opens of one inode, so the lock is
/// exactly scoped to "operations on this backing file" without needing a
/// separate lock directory — and it needs no privilege beyond opening a file
/// the caller already owns. `flock` releases automatically if the helper dies.
pub struct FileLock<'fd> {
    fd: &'fd OwnedFd,
}

impl<'fd> FileLock<'fd> {
    pub fn acquire(fd: &'fd OwnedFd) -> Result<Self, String> {
        // SAFETY: fd is a valid, open descriptor for the lifetime of the borrow.
        let rc = unsafe { libc::flock(fd.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(format!(
                "cannot lock the backing file: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self { fd })
    }
}

impl Drop for FileLock<'_> {
    fn drop(&mut self) {
        // Best-effort release; the kernel also releases on close/exit.
        unsafe { libc::flock(self.fd.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Whether `mount_point` is a mount target, read from `/proc/self/mountinfo`.
///
/// Unlike the original parser this **unescapes** the mount-point field before
/// comparing. The kernel writes mountinfo field 5 with octal escapes for space
/// (`\040`), tab (`\011`), newline (`\012`) and backslash (`\134`), so a raw
/// string comparison reports every volume under a home directory containing any
/// of those characters as *unmounted* — which would send `start` down the
/// remount path against an already-mounted volume and stack a second mount.
pub fn is_mounted(mount_point: &Path) -> bool {
    let Ok(table) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    is_mounted_in_table(&table, mount_point)
}

/// Whether `table` (mountinfo contents) lists `mount_point` as a mount target.
///
/// Split out so the production comparison and its test are the *same* code.
/// Previously the test reimplemented the comparison in a local helper, so this
/// loop — which decides the remount and detach paths — was covered by nothing
/// and could have been inverted with the suite staying green (F-58).
pub fn is_mounted_in_table(table: &str, mount_point: &Path) -> bool {
    let wanted = mount_point.as_os_str().as_bytes();
    let mut found = false;
    for target in mountinfo_targets(table) {
        if target == wanted {
            found = true;
            break;
        }
    }
    found
}

/// The source device (major, minor) of `mount_point`, from a mountinfo table.
///
/// # Why this exists (F-77)
///
/// `unmount` located the loop device by the backing file's inode. When the
/// backing file has been **deleted** — which is the normal state after a
/// `delete`, and the state every interrupted test run leaves behind — there is
/// no inode to look up, so the helper unmounted and reported
/// "nothing to detach by inode" while leaving the loop device attached.
///
/// The code claimed "a detached-and-deleted device auto-clears once its mount
/// is released". Measured on a real host: it does not. Seven loop devices were
/// still attached to deleted images after their mounts were released, each
/// pinning a fully-allocated 500 MB file that `rm` could not reclaim because
/// the kernel still held the inode open. That is why deleting the `.img` files
/// freed no space.
///
/// mountinfo field 3 is the source device as `major:minor`, and for a loop
/// device major is 7 and minor is the device number — so the mount table itself
/// says which loop to detach, with no dependence on the backing file existing.
pub fn mount_source_device(table: &str, mount_point: &Path) -> Option<(u32, u32)> {
    let wanted = mount_point.as_os_str().as_bytes();
    for line in table.lines() {
        let mut fields = line.split(' ');
        let device = fields.nth(2)?;
        let target = fields.nth(1)?;
        if unescape_octal(target) != wanted {
            continue;
        }
        let (major, minor) = device.split_once(':')?;
        return Some((major.parse().ok()?, minor.parse().ok()?));
    }
    None
}

/// The loop device number backing `mount_point`, if it is a loop mount.
///
/// Major 7 is the loop block-device major; anything else is not a loop mount
/// and must not be detached, so this returns `None` rather than guessing.
pub fn loop_number_for_mount(mount_point: &Path) -> Option<u32> {
    const LOOP_MAJOR: u32 = 7;
    let table = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    match mount_source_device(&table, mount_point) {
        Some((LOOP_MAJOR, minor)) => Some(minor),
        _ => None,
    }
}

/// Iterator over the (unescaped) mount-point targets in a mountinfo table.
///
/// Field 5 (1-indexed) is the mount point. Optional fields follow it until a
/// ` - ` separator, so counting from the left to field 5 is correct regardless
/// of how many optional fields are present.
pub fn mountinfo_targets(table: &str) -> impl Iterator<Item = Vec<u8>> + '_ {
    table.lines().filter_map(|line| {
        let field = line.split(' ').nth(4)?;
        Some(unescape_octal(field))
    })
}

/// Decode mountinfo/`/proc/mounts` octal escapes into raw bytes.
///
/// Only `\040`, `\011`, `\012` and `\134` appear in practice, but any
/// three-digit octal escape is decoded so the function is correct for the whole
/// format rather than the common cases.
pub fn unescape_octal(field: &str) -> Vec<u8> {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let octal = &bytes[i + 1..i + 4];
            if octal.iter().all(|b| (b'0'..=b'7').contains(b)) {
                let value = (octal[0] - b'0') * 64 + (octal[1] - b'0') * 8 + (octal[2] - b'0');
                out.push(value);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

// --- low-level helpers -----------------------------------------------------

fn open_root() -> Result<OwnedFd, String> {
    let root = c"/";
    // SAFETY: "/" is a valid path; the returned fd is owned.
    let raw = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(format!(
            "cannot open /: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn openat(parent: &OwnedFd, name: &CString, flags: libc::c_int) -> Result<OwnedFd, libc::c_int> {
    // SAFETY: parent is a valid directory descriptor; name is a valid C string.
    let raw = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO));
    }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Split a path into its normal components as C strings, rejecting anything
/// that is not a plain name (`.`, `..`, or a non-root prefix).
///
/// The managed paths are built internally from a validated name and fixed
/// segments, so they are always well-formed; this is defence in depth against a
/// future caller that constructs a path differently.
fn normal_components(path: &Path) -> Result<Vec<CString>, String> {
    let mut out = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => {
                let c = CString::new(part.as_bytes())
                    .map_err(|_| format!("path component {part:?} contains a NUL"))?;
                out.push(c);
            }
            other => {
                return Err(format!(
                    "refusing to resolve a path with a non-literal component {other:?}: {}",
                    path.display()
                ))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nemr-safe-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The reference escalation: a symlinked mount point must be refused, not
    /// followed. This is the unit-level proof of the `/etc`-shadowing fix.
    #[test]
    fn symlinked_final_component_is_refused() {
        let root = scratch("final-symlink");
        let target = root.join("elsewhere");
        fs::create_dir(&target).unwrap();
        let attack = root.join("mountpoint");
        symlink(&target, &attack).unwrap();

        let error = open_beneath(&attack, libc::O_DIRECTORY | libc::O_RDONLY)
            .expect_err("a symlinked mount point must be refused");
        assert!(
            error.contains("symlink"),
            "error should explain the refusal: {error}"
        );
    }

    /// A symlinked *intermediate* component (e.g. `mounts` itself swapped for a
    /// link) must also be refused — the escalation does not require the final
    /// element to be the link.
    #[test]
    fn symlinked_intermediate_component_is_refused() {
        let root = scratch("mid-symlink");
        let real = root.join("real");
        fs::create_dir_all(real.join("child")).unwrap();
        let link = root.join("mounts");
        symlink(&real, &link).unwrap();

        let attack = link.join("child");
        let error = open_beneath(&attack, libc::O_DIRECTORY | libc::O_RDONLY)
            .expect_err("a symlinked intermediate component must be refused");
        assert!(error.contains("symlink"), "got: {error}");
    }

    /// A real directory path resolves, and the descriptor refers to it.
    #[test]
    fn real_directory_resolves() {
        let root = scratch("real-dir");
        let dir = root.join("volumes");
        fs::create_dir(&dir).unwrap();

        let fd = open_beneath(&dir, libc::O_DIRECTORY | libc::O_RDONLY)
            .expect("a real directory should resolve");
        let st = fstat(&fd).unwrap();
        assert_eq!(st.st_mode & libc::S_IFMT, libc::S_IFDIR);
    }

    /// A regular backing file resolves and fstat reports it as one.
    #[test]
    fn regular_file_resolves_and_stats() {
        let root = scratch("regfile");
        let file = root.join("vol.img");
        fs::write(&file, b"backing").unwrap();

        let fd = open_beneath(&file, libc::O_RDWR).expect("a regular file should resolve");
        let st = fstat(&fd).unwrap();
        assert_eq!(st.st_mode & libc::S_IFMT, libc::S_IFREG);
        assert_eq!(st.st_uid, unsafe { libc::getuid() });
    }

    /// A symlinked backing file (the losetup-redirect attack) is refused.
    #[test]
    fn symlinked_backing_file_is_refused() {
        let root = scratch("img-symlink");
        let decoy = root.join("decoy");
        fs::write(&decoy, b"decoy").unwrap();
        let img = root.join("vol.img");
        symlink(&decoy, &img).unwrap();

        let error =
            open_beneath(&img, libc::O_RDWR).expect_err("a symlinked backing file must be refused");
        assert!(error.contains("symlink"), "got: {error}");
    }

    #[test]
    fn octal_unescape_decodes_the_kernel_escapes() {
        assert_eq!(unescape_octal("plain"), b"plain");
        assert_eq!(unescape_octal(r"a\040b"), b"a b"); // space
        assert_eq!(unescape_octal(r"a\011b"), b"a\tb"); // tab
        assert_eq!(unescape_octal(r"a\012b"), b"a\nb"); // newline
        assert_eq!(unescape_octal(r"a\134b"), b"a\\b"); // backslash
        assert_eq!(
            unescape_octal(r"/home/John\040Doe/x"),
            b"/home/John Doe/x".to_vec()
        );
    }

    /// F-77 — the mount table names the loop device, so a deleted backing file
    /// does not strand it.
    ///
    /// Real mountinfo lines from a host that had leaked seven loop devices this
    /// way. Field 3 is `major:minor`; major 7 is the loop major.
    #[test]
    fn the_loop_device_is_recovered_from_the_mount_table() {
        let table = "\
1259 29 7:20 / /home/nemr/.local/share/nemr/mounts/enospc-207576-19 rw,relatime shared:1 - ext4 /dev/loop20 rw
1260 29 7:23 / /home/nemr/.local/share/nemr/mounts/htmltest rw,relatime - ext4 /dev/loop23 rw
";
        assert_eq!(
            mount_source_device(
                table,
                Path::new("/home/nemr/.local/share/nemr/mounts/enospc-207576-19")
            ),
            Some((7, 20))
        );
        assert_eq!(
            mount_source_device(
                table,
                Path::new("/home/nemr/.local/share/nemr/mounts/htmltest")
            ),
            Some((7, 23))
        );
        // CONTROL: a path that is not in the table must not resolve to a device.
        // Detaching a loop device on a mistaken match would destroy an unrelated
        // filesystem, so a miss has to be a miss.
        assert_eq!(
            mount_source_device(
                table,
                Path::new("/home/nemr/.local/share/nemr/mounts/absent")
            ),
            None
        );
    }

    /// A mount point containing a space must still resolve.
    ///
    /// Same escaping trap as `is_mounted`: the kernel writes `\040` for a space,
    /// so a raw comparison silently fails to find the device and the loop leaks.
    #[test]
    fn the_loop_lookup_unescapes_the_mount_point() {
        let table = "49 29 7:19 / /home/john\\040doe/mnt rw shared:277 - ext4 /dev/loop19 rw\n";
        assert_eq!(
            mount_source_device(table, Path::new("/home/john doe/mnt")),
            Some((7, 19))
        );
    }

    #[test]
    fn mountinfo_field_five_is_read_past_optional_fields() {
        // A line with two optional fields (shared:277 master:2) before " - ".
        let table =
            "49 29 7:19 / /home/john\\040doe/mnt rw shared:277 master:2 - ext4 /dev/loop0 rw\n";
        let targets: Vec<Vec<u8>> = mountinfo_targets(table).collect();
        assert_eq!(targets, vec![b"/home/john doe/mnt".to_vec()]);
        // Calls the PRODUCTION comparison, not a local reimplementation.
        assert!(is_mounted_in_table(table, Path::new("/home/john doe/mnt")));
        assert!(!is_mounted_in_table(
            table,
            Path::new("/home/john doe/other")
        ));
    }
}
