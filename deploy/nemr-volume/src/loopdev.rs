//! Loop-device attach/detach driven entirely by ioctls on descriptors the
//! helper already holds — no `losetup` subprocess.
//!
//! # Why not shell out to `losetup`
//!
//! The hardened design resolves the backing file to a descriptor with
//! `O_NOFOLLOW` and never re-opens it by name, so a symlink swap has nothing to
//! redirect. Passing that descriptor to a `losetup` child as `/proc/self/fd/N`
//! defeats the whole thing twice over:
//!
//! - `/proc/self` resolves against the **child's** pid, not the helper's, so
//!   `/proc/self/fd/N` names an entry in the child's fd table, not ours.
//! - Rust opens files `O_CLOEXEC` by default, so fd N is closed across the
//!   `exec` regardless — the child's fd N does not exist.
//!
//! The result was a helper that refused every attack and could not provision a
//! single volume: `losetup: /proc/self/fd/4: failed to set up loop device: No
//! such file or directory`. Parsing `losetup --list` output for the detach path
//! had a second bug in the same spirit — invented column names — so every
//! unmount's detach step failed too.
//!
//! Driving the loop device through ioctls on the held descriptor removes the
//! subprocess, the second path resolution, and the CLI-vocabulary fragility all
//! at once. The backing file is identified by its `(device, inode)` pair read
//! straight from the kernel via `LOOP_GET_STATUS64`, which cannot be spoofed by
//! a rename and is immune to the `(deleted)` and whitespace-in-path problems the
//! string parse had.

use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;

// ioctl numbers from <linux/loop.h>. Stable kernel ABI.
const LOOP_CLR_FD: libc::c_ulong = 0x4C01;
const LOOP_GET_STATUS64: libc::c_ulong = 0x4C05;
const LOOP_CONFIGURE: libc::c_ulong = 0x4C0A;
const LOOP_CTL_GET_FREE: libc::c_ulong = 0x4C82;

const LO_NAME_SIZE: usize = 64;
const LO_KEY_SIZE: usize = 32;

/// `struct loop_info64` from <linux/loop.h>.
#[repr(C)]
#[derive(Clone, Copy)]
struct LoopInfo64 {
    lo_device: u64,
    lo_inode: u64,
    lo_rdevice: u64,
    lo_offset: u64,
    lo_sizelimit: u64,
    lo_number: u32,
    lo_encrypt_type: u32,
    lo_encrypt_key_size: u32,
    lo_flags: u32,
    lo_file_name: [u8; LO_NAME_SIZE],
    lo_crypt_name: [u8; LO_NAME_SIZE],
    lo_encrypt_key: [u8; LO_KEY_SIZE],
    lo_init: [u64; 2],
}

impl Default for LoopInfo64 {
    fn default() -> Self {
        // SAFETY: an all-zero LoopInfo64 is a valid, empty descriptor.
        unsafe { std::mem::zeroed() }
    }
}

/// `struct loop_config` from <linux/loop.h>, the argument to `LOOP_CONFIGURE`.
#[repr(C)]
struct LoopConfig {
    fd: u32,
    block_size: u32,
    info: LoopInfo64,
    reserved: [u64; 8],
}

/// Attach `backing_fd` to a free loop device and return its number.
///
/// Uses `LOOP_CONFIGURE` (kernel 5.8+), which sets the backing descriptor and
/// the device info in one atomic ioctl. The kernel takes its own reference to
/// the backing file, so the device outlives the helper's descriptor. The device
/// is not marked autoclear: it persists until [`detach`] or until its mount is
/// released, matching the previous `losetup` behaviour.
pub fn attach(backing_fd: &OwnedFd) -> Result<u32, String> {
    let control = File::open("/dev/loop-control")
        .map_err(|e| format!("cannot open /dev/loop-control: {e}"))?;

    // GET_FREE then CONFIGURE can race another process onto the same device;
    // CONFIGURE returns EBUSY, so retry with a fresh free number a bounded
    // number of times.
    for _ in 0..64 {
        // SAFETY: control is a valid descriptor; GET_FREE takes no argument.
        let number = unsafe { libc::ioctl(control.as_raw_fd(), LOOP_CTL_GET_FREE) };
        if number < 0 {
            return Err(format!(
                "LOOP_CTL_GET_FREE failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        let path = format!("/dev/loop{number}");
        let device = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(&path)
            .map_err(|e| format!("cannot open {path}: {e}"))?;

        let mut config: LoopConfig = LoopConfig {
            fd: backing_fd.as_raw_fd() as u32,
            block_size: 0,
            info: LoopInfo64::default(),
            reserved: [0; 8],
        };

        // SAFETY: device is a valid loop device fd; config is a valid,
        // correctly-sized loop_config for the whole call.
        let rc = unsafe {
            libc::ioctl(
                device.as_raw_fd(),
                LOOP_CONFIGURE,
                &mut config as *mut LoopConfig,
            )
        };
        if rc == 0 {
            return Ok(number as u32);
        }

        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EBUSY) {
            // Someone took this device between GET_FREE and CONFIGURE. Retry.
            continue;
        }
        return Err(format!("LOOP_CONFIGURE on {path} failed: {err}"));
    }

    Err("could not find a free loop device after 64 attempts".to_string())
}

/// The loop device number currently backed by the file at `backing_fd`, if any.
///
/// Compares the `(device, inode)` identity the kernel records against the file
/// we hold open, so it matches regardless of the backing file's path — even if
/// that path has been unlinked or contains whitespace.
pub fn find_by_backing(backing_fd: &impl AsRawFd) -> Result<Option<u32>, String> {
    let st = crate::safe::fstat(backing_fd)?;

    let entries = std::fs::read_dir("/sys/block")
        .map_err(|e| format!("cannot read /sys/block: {e}"))?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(digits) = name.strip_prefix("loop") else {
            continue;
        };
        let Ok(number) = digits.parse::<u32>() else {
            continue;
        };

        let path = format!("/dev/loop{number}");
        let Ok(device) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(&path)
        else {
            continue;
        };

        let mut info = LoopInfo64::default();
        // SAFETY: device is valid; info is a valid, correctly-sized target.
        let rc =
            unsafe { libc::ioctl(device.as_raw_fd(), LOOP_GET_STATUS64, &mut info as *mut _) };
        if rc != 0 {
            // ENXIO for an unbound device, etc. — not the one we want.
            continue;
        }

        if info.lo_inode == st.st_ino && info.lo_device == st.st_dev {
            return Ok(Some(number));
        }
    }

    Ok(None)
}

/// Detach loop device `number` (`LOOP_CLR_FD`).
///
/// The device must not be mounted; the caller unmounts first. A device whose
/// backing file was unlinked auto-clears once its last mount is gone, so a
/// missing device here is not an error.
pub fn detach(number: u32) -> Result<(), String> {
    let path = format!("/dev/loop{number}");
    let device = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(device) => device,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot open {path}: {e}")),
    };

    // SAFETY: device is a valid loop device descriptor.
    let rc = unsafe { libc::ioctl(device.as_raw_fd(), LOOP_CLR_FD) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        // ENXIO: already detached. Idempotent.
        if err.raw_os_error() == Some(libc::ENXIO) {
            return Ok(());
        }
        return Err(format!("LOOP_CLR_FD on {path} failed: {err}"));
    }
    Ok(())
}

/// The `/dev/loopN` path for a device number, for use as a mount source.
///
/// A device path under `/dev` is root-owned and not caller-controlled, so it is
/// safe to name directly — unlike anything under the managed directory.
pub fn device_path(number: u32) -> String {
    format!("/dev/loop{number}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ioctl structs are a binary ABI contract with the kernel. A wrong
    /// field type or a missing field would be undetectable at runtime except as
    /// corruption, so pin the sizes against the kernel's own definitions:
    /// `loop_info64` is 232 bytes and `loop_config` is 304 bytes on 64-bit
    /// Linux. This runs without root and fails the build the instant the layout
    /// drifts.
    #[test]
    fn ioctl_struct_layout_matches_the_kernel() {
        assert_eq!(
            std::mem::size_of::<LoopInfo64>(),
            232,
            "loop_info64 layout does not match <linux/loop.h>"
        );
        assert_eq!(
            std::mem::size_of::<LoopConfig>(),
            304,
            "loop_config layout does not match <linux/loop.h>"
        );
    }
}
