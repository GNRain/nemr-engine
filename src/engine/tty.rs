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
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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

/// Tracks which private terminal modes the container's programs have turned on.
///
/// # Why tracking, rather than resetting everything
///
/// A full-screen program interrupted mid-run never emits its own restores, and
/// the modes it set — alternate screen, hidden cursor, mouse reporting — live
/// in the *local* terminal emulator, so they outlive the container. Something
/// has to undo them.
///
/// The obvious approach, emitting every disable sequence unconditionally on
/// exit, is actively harmful. `\e[?1049l` does not merely leave the alternate
/// screen: it **restores the cursor position saved when the buffer was
/// switched**. Sent when the program never switched, it moves the cursor to a
/// stale or default position. That was the cause of a report of `logout`
/// followed by a screenful of blank lines and a cursor stranded far below it.
///
/// So the output stream is watched, and only modes observed to be *still on*
/// when the session ends are turned off.
#[derive(Default)]
pub struct ModeTracker {
    state: ScanState,
    params: Vec<u16>,
    digits: String,
    private: bool,
    /// Modes set with `\e[?Nh` and not yet cleared.
    enabled: std::collections::BTreeSet<u16>,
    /// The cursor is hidden. Tracked separately because its polarity is
    /// inverted: `\e[?25h` *shows* the cursor.
    cursor_hidden: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum ScanState {
    #[default]
    Ground,
    Escape,
    Csi,
}

impl ModeTracker {
    /// Private modes worth undoing. Others are left alone: guessing at modes a
    /// program manages itself risks doing more harm than the leak.
    const TRACKED: [u16; 8] = [
        47,   // alternate screen (legacy)
        1000, // mouse click reporting
        1002, // mouse drag reporting
        1003, // all-motion mouse reporting
        1006, // SGR mouse encoding
        1047, // alternate screen, no cursor save
        1049, // alternate screen with cursor save/restore
        2004, // bracketed paste
    ];

    /// Feed output bytes on their way to the terminal.
    ///
    /// Incremental: a sequence split across two reads is still recognised,
    /// because the parse state persists between calls.
    pub fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            // ESC restarts an escape sequence from any state (ECMA-48). Without
            // this, an ESC arriving mid-CSI — a truncated or malformed sequence,
            // which a killed full-screen program can easily emit — would be
            // swallowed as an ordinary parameter byte and the following real
            // sequence missed, so a mode left on would never be restored.
            if byte == 0x1b {
                self.state = ScanState::Escape;
                continue;
            }
            match self.state {
                ScanState::Ground => {}
                ScanState::Escape => {
                    if byte == b'[' {
                        self.state = ScanState::Csi;
                        self.params.clear();
                        self.digits.clear();
                        self.private = false;
                    } else {
                        self.state = ScanState::Ground;
                    }
                }
                ScanState::Csi => match byte {
                    b'?' => self.private = true,
                    b'0'..=b'9' => self.digits.push(byte as char),
                    b';' => self.take_param(),
                    b'h' | b'l' => {
                        self.take_param();
                        if self.private {
                            let set = byte == b'h';
                            for mode in std::mem::take(&mut self.params) {
                                self.record(mode, set);
                            }
                        }
                        self.state = ScanState::Ground;
                    }
                    _ => self.state = ScanState::Ground,
                },
            }
        }
    }

    fn take_param(&mut self) {
        if let Ok(value) = self.digits.parse::<u16>() {
            self.params.push(value);
        }
        self.digits.clear();
    }

    fn record(&mut self, mode: u16, set: bool) {
        if mode == 25 {
            self.cursor_hidden = !set;
            return;
        }
        if !Self::TRACKED.contains(&mode) {
            return;
        }
        if set {
            self.enabled.insert(mode);
        } else {
            self.enabled.remove(&mode);
        }
    }

    /// Sequences that undo what is still set, or empty if nothing is.
    pub fn restore_sequence(&self) -> String {
        let mut restore = String::new();

        // Alternate-screen modes first: leaving the buffer repositions the
        // cursor, so anything else would be undone by it.
        for mode in [1049u16, 1047, 47] {
            if self.enabled.contains(&mode) {
                restore.push_str(&format!("\x1b[?{mode}l"));
            }
        }
        for mode in [1000u16, 1002, 1003, 1006, 2004] {
            if self.enabled.contains(&mode) {
                restore.push_str(&format!("\x1b[?{mode}l"));
            }
        }
        if self.cursor_hidden {
            restore.push_str("\x1b[?25h");
        }
        if !restore.is_empty() {
            // Only reset attributes if something else needed undoing; a clean
            // session should emit nothing at all.
            restore.push_str("\x1b[0m");
        }
        restore
    }
}

/// Whether this process's stdin is a terminal.
///
/// Decides whether an attach session allocates a pty. A pty is what makes an
/// interactive shell behave, but it also changes how end-of-input works: a pty
/// has no EOF, so a scripted `echo cmd | nemr attach` can never tell the shell
/// its input has finished. Without a terminal, stdin is an ordinary pipe and
/// closing it produces a real EOF, which shells handle correctly.
pub fn stdin_is_terminal() -> bool {
    // SAFETY: stdin's descriptor is valid for the duration of the call.
    unsafe { libc::isatty(std::io::stdin().as_raw_fd()) == 1 }
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
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("failed to open FIFO {}", path.display()))?;

    // Clear O_NONBLOCK: it was only needed to guarantee the open itself did not
    // block. The proxy threads want ordinary blocking reads.
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is valid and owned by `file`.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags != -1 {
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    }
    Ok(file)
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

/// Copy from a FIFO to `to`, stopping once `stop` is set and no data remains.
///
/// # Why this cannot simply read until EOF
///
/// [`open_fifo`] opens `O_RDWR` to avoid the blocking-open deadlock, which
/// means **this process holds a write end of the FIFO itself**. A FIFO reports
/// EOF only when every write end is closed, so a plain read loop never sees
/// EOF here even after the container's process exits and the shim closes its
/// side — it blocks forever on a pipe nobody will ever write to again.
///
/// That is not theoretical: it deadlocked `attach` on exit. The exec had
/// finished and `wait_exec` had returned, but joining the output pump blocked
/// indefinitely, leaving the CLI hung.
///
/// So the loop polls with a timeout and checks `stop` when idle. Once the
/// caller sets `stop`, any buffered output is drained first — a poll timeout
/// with the flag set is what ends it, so no trailing bytes are lost.
pub fn pump_until_stopped<W: Write>(
    from: File,
    mut to: W,
    stop: Arc<AtomicBool>,
    tracker: Option<Arc<std::sync::Mutex<ModeTracker>>>,
) {
    const POLL_TIMEOUT_MS: libc::c_int = 100;

    let mut from = from;
    let fd = from.as_raw_fd();

    // SAFETY: `fd` is valid and owned by `from` for the whole function.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags != -1 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    let mut buffer = [0u8; 8192];
    loop {
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };

        // SAFETY: `poll_fd` is a valid, correctly-sized pollfd.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, POLL_TIMEOUT_MS) };

        if ready > 0 && (poll_fd.revents & libc::POLLIN) != 0 {
            match from.read(&mut buffer) {
                Ok(0) => {
                    // A genuine EOF, which happens if every writer including
                    // ours has closed. Nothing more is coming.
                    break;
                }
                Ok(n) => {
                    if let Some(tracker) = &tracker {
                        if let Ok(mut tracker) = tracker.lock() {
                            tracker.observe(&buffer[..n]);
                        }
                    }
                    if to.write_all(&buffer[..n]).is_err() || to.flush().is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        } else if stop.load(Ordering::Relaxed) {
            // Idle and asked to stop: everything buffered has been drained.
            break;
        }
    }
}

#[cfg(test)]
mod mode_tracker_tests {
    use super::ModeTracker;

    fn restore_after(chunks: &[&[u8]]) -> String {
        let mut t = ModeTracker::default();
        for c in chunks {
            t.observe(c);
        }
        t.restore_sequence()
    }

    /// A clean session — no private modes left on — restores nothing. This is
    /// the whole point of tracking rather than blindly resetting: a well-behaved
    /// program should cost zero restore bytes.
    #[test]
    fn clean_session_restores_nothing() {
        assert_eq!(restore_after(&[b"hello world\n\x1b[0mnormal text"]), "");
    }

    /// A program that entered the alternate screen and never left it (killed
    /// mid-run) must have it turned off, or the user's terminal is stuck on a
    /// dead frame.
    #[test]
    fn alt_screen_left_on_is_restored() {
        let r = restore_after(&[b"\x1b[?1049h drawing..."]);
        assert!(
            r.contains("\x1b[?1049l"),
            "must leave the alternate screen: {r:?}"
        );
        assert!(r.ends_with("\x1b[0m"), "must reset attributes after: {r:?}");
    }

    /// Entered and left cleanly — nothing to undo. Emitting `\e[?1049l` here
    /// would restore a stale cursor position; that was a real reported bug.
    #[test]
    fn alt_screen_entered_and_left_restores_nothing() {
        assert_eq!(restore_after(&[b"\x1b[?1049h\x1b[?1049l"]), "");
    }

    /// Cursor polarity is inverted: `\e[?25l` hides, `\e[?25h` shows. A hidden
    /// cursor left behind must be shown again.
    #[test]
    fn hidden_cursor_is_shown_again() {
        let r = restore_after(&[b"\x1b[?25l"]);
        assert!(r.contains("\x1b[?25h"), "must show the cursor: {r:?}");
    }

    #[test]
    fn cursor_hidden_then_shown_restores_nothing() {
        assert_eq!(restore_after(&[b"\x1b[?25l\x1b[?25h"]), "");
    }

    /// A sequence split across two reads must still be recognised — the parse
    /// state persists between `observe` calls. This is the property that makes
    /// it safe to feed arbitrary read() chunks.
    #[test]
    fn sequence_split_across_reads_is_recognised() {
        let r = restore_after(&[b"\x1b[?10", b"49h rest"]);
        assert!(
            r.contains("\x1b[?1049l"),
            "split sequence must be parsed: {r:?}"
        );
    }

    /// Modes the tracker does not manage are left alone — guessing at a mode a
    /// program manages itself risks more harm than the leak.
    #[test]
    fn untracked_mode_is_ignored() {
        // 12 (cursor blink) is not in TRACKED.
        assert_eq!(restore_after(&[b"\x1b[?12h"]), "");
    }

    #[test]
    fn mouse_modes_left_on_are_disabled() {
        let r = restore_after(&[b"\x1b[?1000h\x1b[?1006h"]);
        assert!(
            r.contains("\x1b[?1000l") && r.contains("\x1b[?1006l"),
            "mouse off: {r:?}"
        );
    }

    /// Alternate-screen restore must come first: leaving the buffer repositions
    /// the cursor, so any other restore emitted before it would be undone.
    #[test]
    fn alt_screen_is_restored_before_mouse() {
        let r = restore_after(&[b"\x1b[?1049h\x1b[?1000h"]);
        let alt = r.find("\x1b[?1049l").expect("alt present");
        let mouse = r.find("\x1b[?1000l").expect("mouse present");
        assert!(alt < mouse, "alt-screen must restore before mouse: {r:?}");
    }

    /// Malformed CSI (no final byte, garbage params) must not panic and must not
    /// wedge the parser against a following valid sequence.
    #[test]
    fn malformed_csi_does_not_panic_or_wedge() {
        let r = restore_after(&[b"\x1b[?99999999999", b"\x1b[?1049h"]);
        assert!(r.contains("\x1b[?1049l"), "recovers after garbage: {r:?}");
    }
}
