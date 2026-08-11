//! Terminal handling for interactive attach sessions (Section 3.8, PROC-02).
//!
//! Attaching means giving the user's terminal to a process inside the
//! container. Three things have to happen for that to feel like a normal shell:
//!
//! 1. The local terminal goes into **raw mode**, so keystrokes reach the
//!    container instead of being line-buffered and echoed locally. Without it,
//!    Ctrl-C would kill `nemr` rather than the process inside, and nothing
//!    interactive works.
//! 2. Bytes are proxied both ways through the FIFOs containerd's shim opens.
//! 3. Window size is sent on start and on every `SIGWINCH`, or the process
//!    believes the terminal is whatever size it was when it started.
//!
//! Raw mode is a global change to the user's terminal, so [`RawMode`] is a
//! guard: `Drop` restores the original settings on every path, including panic
//! and error. Leaving a terminal in raw mode leaves the user with an
//! apparently-broken shell that needs a blind `reset`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result};

/// Restores the terminal to its original settings when dropped.
pub struct RawMode {
    fd: i32,
    original: libc::termios,
}

impl RawMode {
    /// Put stdin into raw mode, returning a guard that restores it.
    ///
    /// Returns `Ok(None)` when stdin is not a terminal — piping into `attach`
    /// is legitimate for scripted use, and there is nothing to make raw.
    pub fn enable() -> Result<Option<Self>> {
        let fd = std::io::stdin().as_raw_fd();

        // SAFETY: `fd` is a valid file descriptor for the lifetime of the call.
        if unsafe { libc::isatty(fd) } != 1 {
            return Ok(None);
        }

        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `original` is a valid, correctly-sized termios.
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to read terminal settings");
        }

        let mut raw = original;
        // SAFETY: `raw` is a valid termios initialised from tcgetattr.
        unsafe { libc::cfmakeraw(&mut raw) };

        // SAFETY: same.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to put the terminal into raw mode");
        }

        Ok(Some(Self { fd, original }))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: `fd` and `original` were valid when the guard was created,
        // and stdin outlives it.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

/// Current terminal size as `(width, height)`, if stdin is a terminal.
pub fn window_size() -> Option<(u32, u32)> {
    let fd = std::io::stdin().as_raw_fd();
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };

    // SAFETY: `size` is a valid winsize; TIOCGWINSZ writes exactly that.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } != 0 {
        return None;
    }
    if size.ws_col == 0 || size.ws_row == 0 {
        return None;
    }
    Some((size.ws_col as u32, size.ws_row as u32))
}

/// Create a FIFO, replacing any stale one at the same path.
pub fn make_fifo(path: &Path) -> Result<()> {
    let _ = std::fs::remove_file(path);

    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .context("FIFO path contains an interior NUL")?;

    // SAFETY: `c_path` is a valid NUL-terminated string for the call's duration.
    if unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to create FIFO {}", path.display()));
    }
    Ok(())
}

/// Open a FIFO read-write.
///
/// Opening a FIFO read-only blocks until a writer appears, and write-only until
/// a reader does. Since both ends are opened by different processes at
/// unpredictable times, doing either invites a deadlock. `O_RDWR` on a FIFO
/// never blocks on Linux, which is the standard way around this.
pub fn open_fifo(path: &Path) -> Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("failed to open FIFO {}", path.display()))
        .and_then(|file| {
            // Clear O_NONBLOCK: it was only needed to guarantee the open itself
            // did not block. The proxy threads want ordinary blocking reads.
            let fd = file.as_raw_fd();
            // SAFETY: `fd` is valid and owned by `file`.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags != -1 {
                unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
            }
            Ok(file)
        })
}

/// Copy bytes from `from` to `to` until EOF, flushing as it goes.
///
/// Flushing per chunk matters for an interactive session: buffered output would
/// make a shell prompt appear only once enough bytes accumulated.
pub fn pump<R: Read, W: Write>(mut from: R, mut to: W) {
    let mut buffer = [0u8; 8192];
    loop {
        match from.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buffer[..n]).is_err() || to.flush().is_err() {
                    break;
                }
            }
        }
    }
}
