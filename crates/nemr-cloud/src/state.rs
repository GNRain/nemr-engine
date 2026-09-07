//! Client-side state: the account file and per-session lease files.
//!
//! Lives under `$XDG_STATE_HOME/nemr/cloud` (fallback `~/.local/state`), the
//! same convention as the daemon's log. What is stored is deliberately limited:
//!
//! - the bearer token (secret-ish: it authenticates, but cannot decrypt);
//! - the KDF salt + parameters and the **password envelope** — which the server
//!   also stores, and whose whole design is to be safe at rest: opening it
//!   needs the password. The master key itself is NEVER written to disk
//!   (E-16); every data command re-derives it from the password.
//!
//! Files are 0600: the token alone must not leak to other local users.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir)
        .join("nemr/cloud")
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Account {
    pub server: String,
    pub email: String,
    pub token: String,
    /// base64url, no padding — same encoding the wire uses.
    pub kdf_salt: String,
    pub kdf_m_cost: u32,
    pub kdf_t_cost: u32,
    pub kdf_p_cost: u32,
    pub password_envelope: String,
}

fn account_path() -> PathBuf {
    state_dir().join("account.json")
}

pub fn save_account(account: &Account) -> Result<()> {
    write_private(&account_path(), &serde_json::to_vec_pretty(account)?)
}

pub fn load_account() -> Result<Account> {
    let path = account_path();
    let bytes = std::fs::read(&path)
        .with_context(|| format!("not logged in (no {}). Run: nemr login", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("corrupt {}", path.display()))
}

pub fn delete_account() -> Result<()> {
    let path = account_path();
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

/// The server of the last successful login or registration, remembered
/// across `logout` (E-19): `account.json` dies with the session, so without
/// this the address was retyped after every logout. Public data (a URL),
/// still 0600 like everything under the state directory.
fn server_url_path() -> PathBuf {
    state_dir().join("server-url")
}

pub fn remember_server(url: &str) -> Result<()> {
    write_private(&server_url_path(), url.trim().as_bytes())
}

pub fn remembered_server() -> Option<String> {
    std::fs::read_to_string(server_url_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The UI's launch URL (token in the fragment), 0600 — readable by this user
/// only, the same trust the daemon's socket relies on. Overwritten on every
/// `nemr ui`; the token inside is single-use anyway.
pub fn save_ui_url(url: &str) -> Result<()> {
    write_private(&state_dir().join("ui-url"), url.as_bytes())
}

/// Per-session lease state, written by push/pull and by the heartbeat holder.
///
/// `status` is the client-side verdict the holder maintains: `held` while
/// renewals succeed, `lost` the moment one is refused. Push consults it before
/// writing, and the server checks the fence regardless — two independent gates.
#[derive(Serialize, Deserialize, Clone)]
pub struct LeaseState {
    pub holder: String,
    pub fence: i64,
    pub expires_at_unix: i64,
    /// The lease policy's full TTL (F-92): heartbeat pacing comes from this,
    /// never from what remains of a partly-elapsed lease.
    #[serde(default)]
    pub ttl_seconds: i64,
    pub status: String, // held | lost | released
    /// PID of the heartbeat holder process, if one was spawned.
    pub holder_pid: Option<u32>,
}

fn lease_path(session: &str) -> PathBuf {
    state_dir().join(format!("leases/{session}.json"))
}

pub fn save_lease(session: &str, lease: &LeaseState) -> Result<()> {
    write_private(&lease_path(session), &serde_json::to_vec_pretty(lease)?)
}

pub fn load_lease(session: &str) -> Option<LeaseState> {
    let bytes = std::fs::read(lease_path(session)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn delete_lease(session: &str) {
    let _ = std::fs::remove_file(lease_path(session));
}

/// This machine's holder identity in leases: `NEMR_CLOUD_HOLDER` override
/// (tests simulate two machines with it), else the hostname — which is what a
/// takeover prompt shows the other human ("held by <hostname>").
pub fn holder_identity() -> String {
    if let Ok(h) = std::env::var("NEMR_CLOUD_HOLDER") {
        if !h.is_empty() {
            return h;
        }
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown-machine".into())
}

/// Write a file 0600, creating parent directories, atomically enough for our
/// purposes (write to a sibling temp name, then rename).
fn write_private(path: &PathBuf, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path
        .parent()
        .context("state path has no parent directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(bytes)?;
    f.flush()?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
