//! Where the daemon listens and the CLI connects (E-09).
//!
//! A Unix domain socket under `$XDG_RUNTIME_DIR`: filesystem permissions
//! authenticate, so there is no port, no TCP, nothing network-facing. If E-10's
//! remote engine ever happens, only this path changes; the service does not.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// The daemon's socket path: `$XDG_RUNTIME_DIR/nemr/nemrd.sock`.
///
/// Under `$XDG_RUNTIME_DIR` (per-user, mode 0700, cleared on logout) rather
/// than a world-writable /tmp, so the socket is reachable only by the user who
/// owns the session — the same trust boundary the rootless containerd socket
/// beside it already relies on.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("NEMR_DAEMON_SOCKET") {
        return Ok(PathBuf::from(explicit));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context(
        "XDG_RUNTIME_DIR is not set, so the nemrd socket cannot be located. It is set by a \
         normal login session; a bare shell may lack it.",
    )?;
    Ok(PathBuf::from(runtime).join("nemr").join("nemrd.sock"))
}
