//! Re-binding the host's *current* credential into a running session (F-12).
//!
//! A file bind mount pins the inode it was made from. The host's Claude Code
//! rewrites `~/.claude/.credentials.json` by rename on every refresh, so after a
//! host-side refresh a running session still sees the previous file — whose
//! refresh token has been rotated away — and its next refresh fails. The
//! session's own writes go in place (a rename over a mount point cannot
//! succeed), so the *other* direction was never the problem.
//!
//! The repair is a mount operation inside the task's mount namespace: detach
//! the stale bind, then move a detached clone of the host's current file onto
//! the mount point. Two facts shape the implementation, both established by
//! hand before this was written (docs/e13-token-spike.md, Part D):
//!
//! - The stale bind's root dentry is **unlinked** (the host renamed over it),
//!   and the kernel refuses to mount on top of an unlinked dentry (`ENOENT`
//!   from `lock_mount`). So the stale bind must be detached first; the
//!   underlying rootfs placeholder is then the mount point again.
//! - The host path is not visible inside the task's namespace, so the source
//!   travels as a file descriptor: `open_tree(OPEN_TREE_CLONE)` inside
//!   rootlesskit's namespaces (where the host home is visible and we hold
//!   `CAP_SYS_ADMIN`), then `setns` into the task's mount namespace, then
//!   `move_mount`. `/proc/self/fd` does not work there: `/proc` inside the
//!   task's mount namespace is the container's.
//!
//! Joining a user namespace requires a **single-threaded** process, and the
//! daemon is not one, so the sequence runs in a child: `nemrd __rebind …`,
//! dispatched before the daemon's runtime exists.

use std::ffi::CString;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::{bail, Context, Result};

// From <linux/mount.h>. Stable ABI since 5.2; spelled here so the build does
// not depend on which `libc` release first exported them.
const OPEN_TREE_CLONE: libc::c_uint = 1;
const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x4;

fn errno(what: &str) -> anyhow::Error {
    let e = std::io::Error::last_os_error();
    anyhow::anyhow!("{what}: {e}")
}

fn setns(file: &std::fs::File, nstype: libc::c_int, what: &str) -> Result<()> {
    if unsafe { libc::setns(file.as_raw_fd(), nstype) } != 0 {
        return Err(errno(what));
    }
    Ok(())
}

/// The syscall sequence. **Run only in a fresh, single-threaded process.**
pub fn rebind_in_this_process(task_pid: u32, host: &Path, container: &Path) -> Result<()> {
    let child: u32 = crate::engine::netns::rootlesskit_child_pid_for_reads()?
        .trim()
        .parse()
        .context("rootlesskit child_pid is not a pid")?;
    // Every namespace handle first, while /proc is still the host's.
    let open_ns = |pid: u32, ns: &str| {
        std::fs::File::open(format!("/proc/{pid}/ns/{ns}"))
            .with_context(|| format!("opening /proc/{pid}/ns/{ns}"))
    };
    let rk_user = open_ns(child, "user")?;
    let rk_mnt = open_ns(child, "mnt")?;
    let task_mnt = open_ns(task_pid, "mnt")?;
    let host_c = CString::new(host.as_os_str().as_encoded_bytes())?;
    let target_c = CString::new(container.as_os_str().as_encoded_bytes())?;

    setns(
        &rk_user,
        libc::CLONE_NEWUSER,
        "joining rootlesskit's user namespace",
    )?;
    setns(
        &rk_mnt,
        libc::CLONE_NEWNS,
        "joining rootlesskit's mount namespace",
    )?;
    let tree = unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            libc::AT_FDCWD,
            host_c.as_ptr(),
            OPEN_TREE_CLONE,
        )
    };
    if tree < 0 {
        return Err(errno("open_tree(host credential, OPEN_TREE_CLONE)"));
    }
    setns(
        &task_mnt,
        libc::CLONE_NEWNS,
        "joining the task's mount namespace",
    )?;
    // Detach whatever is on the mount point. EINVAL means nothing was mounted
    // there (the rootfs placeholder is bare) — then there is nothing stale.
    if unsafe { libc::umount2(target_c.as_ptr(), libc::MNT_DETACH) } != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EINVAL) {
            bail!("detaching the stale credential bind: {e}");
        }
    }
    let moved = unsafe {
        libc::syscall(
            libc::SYS_move_mount,
            tree as libc::c_int,
            c"".as_ptr(),
            libc::AT_FDCWD,
            target_c.as_ptr(),
            MOVE_MOUNT_F_EMPTY_PATH,
        )
    };
    if moved != 0 {
        return Err(errno("move_mount(current credential -> session)"));
    }
    Ok(())
}

/// Entry point for `nemrd __rebind <task pid> <host path> <container path>`.
pub fn rebind_main(args: &[String]) -> Result<()> {
    let [pid, host, container] = args else {
        bail!("usage: nemrd __rebind <task pid> <host path> <container path>");
    };
    rebind_in_this_process(
        pid.parse().context("task pid")?,
        Path::new(host),
        Path::new(container),
    )
}

/// The binary that understands `__rebind`: `nemrd`, and only `nemrd`.
///
/// Found by this order: `NEMR_REBIND_HELPER` (the test suite points it at the
/// built daemon), the running executable when it *is* nemrd, else `nemrd`
/// beside the running executable (the CLI's autostart convention). Never the
/// bare current executable: the first version did that, and inside the test
/// harness `current_exe()` is the test binary, which read `__rebind` as a
/// test-name filter, ran nothing, and exited 0 — a "success" that re-bound
/// nothing. The regression test caught it; the post-condition check in
/// `project::rebind_credential` now would too.
fn helper_binary() -> Result<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os("NEMR_REBIND_HELPER") {
        return Ok(std::path::PathBuf::from(explicit));
    }
    let exe = std::env::current_exe().context("locating the running executable")?;
    if exe.file_name().is_some_and(|n| n == "nemrd") {
        return Ok(exe);
    }
    let sibling = exe.with_file_name("nemrd");
    if sibling.exists() {
        return Ok(sibling);
    }
    bail!(
        "no nemrd binary to run the re-bind in (looked beside {}; set NEMR_REBIND_HELPER)",
        exe.display()
    )
}

/// Re-bind the host's current credential into the task, in a child process.
///
/// The child is this same binary (`nemrd`) invoked with `__rebind`, so nothing
/// new is installed and the syscalls run single-threaded as `setns` requires.
/// Blocking; call from a blocking context.
pub fn rebind_in_child(task_pid: u32, host: &Path, container: &Path) -> Result<()> {
    let exe = helper_binary()?;
    let out = std::process::Command::new(&exe)
        .arg("__rebind")
        .arg(task_pid.to_string())
        .arg(host)
        .arg(container)
        .output()
        .with_context(|| format!("spawning {} __rebind", exe.display()))?;
    if !out.status.success() {
        bail!(
            "re-binding the credential into task {task_pid} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}
