//! The sync client's core: what `login`, `register`, `logout` and `sessions`
//! *do*, with no prompting and no printing — callable by the CLI (which
//! prompts and prints around it) and by the UI's HTTP surface (which does
//! neither). One place, so the two never disagree: duplicating this logic
//! across the CLI and the daemon-side process would be the two-writers shape
//! WP A spent nine commits eliminating.
//!
//! Inputs come in as parameters — the password above all — and results go
//! out as values. Everything E-16 requires is unchanged: the master key is
//! derived here, in this process, and never written; the server receives what
//! it always received.
//!
//! `push`, `pull` and `release` are here too, with the engine behind
//! `EngineOps` (the CLI's subprocess, the UI's daemon client) and progress
//! reported through a callback, so the browser's pull IS the CLI's pull.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use nemr_crypto::{
    decrypt_bundle, encrypt_bundle, recovery_acknowledgement, MasterKey, RecoveryCode,
};
use rand::RngCore;
use serde_json::json;

use crate::api::{Api, LeaseLost, LeaseResponse, SessionEntry};
use crate::engine_cli::LocalProject;
use crate::keys::{self, b64, unb64};
use crate::state::{self, Account, LeaseState};

const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";

/// The server to talk to when none is named (E-19): `NEMR_SERVER_URL`, else
/// the server remembered from the last successful login or registration,
/// else the development default. A per-invocation environment beats a
/// persistent file; the flag and the page's field beat both, in the callers.
pub fn default_server() -> String {
    std::env::var("NEMR_SERVER_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(state::remembered_server)
        .unwrap_or_else(|| DEFAULT_SERVER.to_string())
}

/// Where the default came from, for a refusal that names its source.
pub fn default_server_source() -> &'static str {
    if std::env::var("NEMR_SERVER_URL").is_ok_and(|s| !s.is_empty()) {
        "NEMR_SERVER_URL"
    } else if state::remembered_server().is_some() {
        "remembered from your last login; pass --server or set NEMR_SERVER_URL to change it"
    } else {
        "the built-in default"
    }
}

/// Log in: derive the auth key from the password with the server's KDF
/// parameters (held to the floor, F-92), exchange it for a token, and store
/// the account. Returns what was stored.
pub fn login(server: &str, email: &str, password: &str) -> Result<Account> {
    let api = Api::new(server, None);
    let p = api.kdf_params(email)?;
    let salt = unb64(&p.kdf_salt, "server KDF salt")?;
    let params = keys::check_params_floor(nemr_crypto::KdfParams {
        m_cost: p.kdf_m_cost,
        t_cost: p.kdf_t_cost,
        p_cost: p.kdf_p_cost,
    })?;
    let root = keys::derive(password, &salt, params)?;
    finish_login(&api, server, email, root.auth_key().as_bytes())
}

fn finish_login(api: &Api, server: &str, email: &str, auth_key: &[u8; 32]) -> Result<Account> {
    let resp = api.login(email, &b64(auth_key))?;
    let account = Account {
        server: server.to_string(),
        email: email.to_string(),
        token: resp.token,
        kdf_salt: resp.kdf_salt,
        kdf_m_cost: resp.kdf_m_cost,
        kdf_t_cost: resp.kdf_t_cost,
        kdf_p_cost: resp.kdf_p_cost,
        password_envelope: resp.password_envelope,
    };
    state::save_account(&account)?;
    // Remembered past logout, so the address is typed once (E-19).
    state::remember_server(server)?;
    Ok(account)
}

/// A registration that has been created server-side and is waiting for the
/// recovery code to be typed back (E-16: recovery is not deferrable). Holds
/// everything needed to confirm — in memory, in this process, nowhere else —
/// so a mistyped code can be retried (F-92) without the account being
/// stranded.
pub struct RegistrationPending {
    pub server: String,
    pub email: String,
    /// The code, shown ONCE to the user by whoever drives this.
    pub recovery_code: RecoveryCode,
    recovery_salt: [u8; 16],
    params: nemr_crypto::KdfParams,
    recovery_envelope: nemr_crypto::Envelope,
    auth_key: [u8; 32],
}

/// Begin a registration: generate the E-16 material, create the account.
/// The account exists after this and is unusable until confirmed.
pub fn register_begin(server: &str, email: &str, password: &str) -> Result<RegistrationPending> {
    let api = Api::new(server, None);
    let params = keys::registration_params();
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let root = keys::derive(password, &salt, params)?;
    let mk = MasterKey::generate();
    let password_envelope = root.wrap_key().seal(&mk);

    let recovery_code = RecoveryCode::generate();
    let mut recovery_salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut recovery_salt);
    let recovery_root = nemr_crypto::derive_root(recovery_code.as_secret(), &recovery_salt, params)
        .map_err(|e| anyhow!("deriving the recovery key: {e}"))?;
    let recovery_envelope = recovery_root.recovery_wrap_key().seal(&mk);

    api.register(&json!({
        "email": email,
        "kdf_salt": b64(&salt),
        "kdf_m_cost": params.m_cost,
        "kdf_t_cost": params.t_cost,
        "kdf_p_cost": params.p_cost,
        "auth_key": b64(root.auth_key().as_bytes()),
        "password_envelope": b64(&password_envelope.to_bytes()),
        "recovery_salt": b64(&recovery_salt),
        "recovery_m_cost": params.m_cost,
        "recovery_t_cost": params.t_cost,
        "recovery_p_cost": params.p_cost,
        "recovery_envelope": b64(&recovery_envelope.to_bytes()),
        "recovery_ack_hash": b64(&recovery_acknowledgement(&mk)),
    }))?;

    Ok(RegistrationPending {
        server: server.to_string(),
        email: email.to_string(),
        recovery_code,
        recovery_salt,
        params,
        recovery_envelope,
        auth_key: *root.auth_key().as_bytes(),
    })
}

/// Does the typed code open the recovery envelope? The confirmation is real,
/// not a string compare: the master key is recovered THROUGH the envelope.
/// `Ok(None)` is a wrong code — ask again; the pending registration is intact.
pub fn register_check_code(pending: &RegistrationPending, typed: &str) -> Option<MasterKey> {
    RecoveryCode::parse(typed.trim()).ok().and_then(|code| {
        nemr_crypto::derive_root(code.as_secret(), &pending.recovery_salt, pending.params)
            .ok()
            .and_then(|root| {
                root.recovery_wrap_key()
                    .open(&pending.recovery_envelope)
                    .ok()
            })
    })
}

/// Confirm with a code that opened the envelope, then log straight in so a
/// registration ends in a usable state.
pub fn register_confirm(pending: &RegistrationPending, recovered: &MasterKey) -> Result<Account> {
    let api = Api::new(&pending.server, None);
    api.confirm_recovery(&pending.email, &b64(&recovery_acknowledgement(recovered)))?;
    finish_login(&api, &pending.server, &pending.email, &pending.auth_key)
}

/// The message for a registration abandoned before confirmation.
pub fn unconfirmed_message() -> &'static str {
    "recovery was not confirmed, so the account is registered but NOT usable, \
     and the code shown is gone with this attempt. Register again with the \
     same email: an unconfirmed account is replaced by the new registration, \
     and a new code is shown."
}

/// What logout did: the server-side revocation's outcome, and whether there
/// was anything to log out of.
pub struct LogoutReport {
    pub was_logged_in: bool,
    /// `Some(error)` if the server could not be told; local state is cleared
    /// regardless, because an unreachable server must not trap the user.
    pub revoke_failed: Option<String>,
}

pub fn logout() -> Result<LogoutReport> {
    match state::load_account() {
        Ok(account) => {
            let api = Api::new(&account.server, Some(account.token.clone()));
            let revoke_failed = api.logout().err().map(|e| format!("{e:#}"));
            state::delete_account()?;
            Ok(LogoutReport {
                was_logged_in: true,
                revoke_failed,
            })
        }
        Err(_) => Ok(LogoutReport {
            was_logged_in: false,
            revoke_failed: None,
        }),
    }
}

/// Where a session lives, from this machine's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Local,
    Remote,
    Both,
}

impl Where {
    pub fn as_str(self) -> &'static str {
        match self {
            Where::Local => "local",
            Where::Remote => "remote",
            Where::Both => "both",
        }
    }
}

/// One row of the session list: the server's index merged with the local
/// projects — the list D-03's lease UX was designed around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub name: String,
    pub agent: String,
    pub location: Where,
    pub running: bool,
    pub size_bytes: Option<i64>,
    pub updated_at_unix: Option<i64>,
    pub last_machine: Option<String>,
    pub has_bundle: bool,
    /// Held right now, per the server (SPEC 1.108).
    pub held_by: Option<String>,
    pub lease_expires_at_unix: Option<i64>,
    /// E-21: `Some(false)` on a machine that has never logged in.
    pub credential_present: Option<bool>,
}

/// Merge the server's index with the local projects. Pure; `local` is
/// `None` when the engine could not be asked (the list still works from the
/// server alone — that is the whole product on a machine without the engine).
pub fn merge_rows(remote: &[SessionEntry], local: Option<&[LocalProject]>) -> Vec<SessionRow> {
    let mut names: Vec<String> = remote.iter().map(|s| s.name.clone()).collect();
    if let Some(local) = local {
        for p in local {
            if !names.contains(&p.name) {
                names.push(p.name.clone());
            }
        }
    }
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let r = remote.iter().find(|s| s.name == name);
            let l = local.and_then(|list| list.iter().find(|p| p.name == name));
            let location = match (l.is_some(), r.is_some()) {
                (true, true) => Where::Both,
                (true, false) => Where::Local,
                (false, true) => Where::Remote,
                (false, false) => unreachable!("name came from one of the lists"),
            };
            SessionRow {
                agent: l
                    .map(|p| p.agent.clone())
                    .or_else(|| r.map(|s| s.agent.clone()))
                    .unwrap_or_default(),
                location,
                running: l.is_some_and(|p| p.running),
                size_bytes: r
                    .and_then(|s| s.ciphertext_bytes)
                    .or_else(|| l.filter(|p| p.usage_known).map(|p| p.used_bytes as i64))
                    .or_else(|| r.map(|s| s.size_bytes).filter(|n| *n > 0)),
                updated_at_unix: r.map(|s| s.updated_at_unix),
                last_machine: r.and_then(|s| s.last_machine.clone()),
                has_bundle: r.is_some_and(|s| s.has_bundle),
                held_by: r.and_then(|s| s.held_by.clone()),
                lease_expires_at_unix: r.and_then(|s| s.lease_expires_at_unix),
                credential_present: l.and_then(|p| p.credential_present),
                name,
            }
        })
        .collect()
}

/// The list: the server's index for the logged-in account, merged with
/// whatever local view the caller has.
pub fn sessions(local: Option<&[LocalProject]>) -> Result<Vec<SessionRow>> {
    let account = state::load_account()?;
    let api = Api::new(&account.server, Some(account.token.clone()));
    let remote = api.sessions()?;
    Ok(merge_rows(&remote, local))
}

/// Does this password open the stored envelope? The same check `push` and
/// `pull` make when they derive the master key, callable **before** taking
/// an action that a later refusal cannot undo — the UI's stop-and-push
/// stops the session first, and a typo must not cost the user a running
/// session for a push that was never going to happen.
pub fn verify_password(password: &str) -> Result<()> {
    let account = state::load_account()?;
    keys::master_key(&account, password).map(|_| ())
}

/// Is anyone logged in on this machine, and as whom?
pub fn whoami() -> Option<Account> {
    state::load_account().ok()
}

// --- the engine, as the core sees it ------------------------------------------

/// How the sync core reaches the engine. The CLI drives `nemr` as a
/// subprocess (`engine_cli`); the UI asks the daemon over its socket through
/// `nemr-daemon-api` (`daemon`). The core sees three verbs and never learns
/// which.
pub trait EngineOps: Send + Sync {
    fn list(&self) -> Result<Vec<LocalProject>>;
    /// Export `name` to `dest` (absolute; the daemon resolves relative paths
    /// in its own cwd).
    fn export(&self, name: &str, dest: &Path) -> Result<()>;
    /// Import a bundle as `name`; the engine creates the project and returns
    /// the name it created.
    fn import(&self, bundle: &Path, name: &str) -> Result<String>;
}

/// The lease is held by another machine. Typed so each driver can offer its
/// own way to take over — the CLI its flag, the page its button.
#[derive(Debug)]
pub struct HeldElsewhere {
    pub session: String,
    pub holder: String,
    pub expires_in_secs: i64,
}

impl std::fmt::Display for HeldElsewhere {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session {:?} is held by {} (lease expires in {}s). Taking it over locks the other machine out of writing.",
            self.session, self.holder, self.expires_in_secs
        )
    }
}
impl std::error::Error for HeldElsewhere {}

/// The binary that runs the detached heartbeat holder: this one. Overridable
/// for tests whose "own binary" is a test harness (the F-12 re-bind lesson:
/// a helper spawned as the test binary reads its verb as a test filter).
fn holder_binary() -> Result<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("NEMR_CLOUD_HOLDER_BIN") {
        return Ok(std::path::PathBuf::from(p));
    }
    std::env::current_exe().context("locating our own binary")
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn human_bytes(n: i64) -> String {
    let n = n.max(0) as f64;
    if n >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1}GiB", n / (1024.0 * 1024.0 * 1024.0))
    } else if n >= 1024.0 * 1024.0 {
        format!("{:.1}MiB", n / (1024.0 * 1024.0))
    } else if n >= 1024.0 {
        format!("{:.1}KiB", n / 1024.0)
    } else {
        format!("{n:.0}B")
    }
}

// --- the lease, client side --------------------------------------------------

/// Ensure this machine holds the session's lease, returning the live fence.
///
/// A healthy hold already maintained by our heartbeat holder is REUSED — a
/// fresh acquire would advance the fence and kill our own holder mid-flight.
/// Otherwise acquire; a lease held by another machine is refused with its
/// holder named, unless `take_over`.
fn ensure_lease(
    api: &Api,
    name: &str,
    take_over: bool,
    report: &mut dyn FnMut(&str),
) -> Result<LeaseResponse> {
    let me = state::holder_identity();

    if let Some(lease) = state::load_lease(name) {
        let holder_alive = lease
            .holder_pid
            .is_some_and(|pid| holder_process_is_ours(pid, name, lease.fence));
        if lease.status == "held"
            && lease.holder == me
            && lease.expires_at_unix > now_unix() + 2
            && holder_alive
        {
            return Ok(LeaseResponse {
                granted: true,
                holder: lease.holder,
                fence: lease.fence,
                expires_at_unix: lease.expires_at_unix,
                ttl_seconds: lease.ttl_seconds,
            });
        }
    }

    // We are about to take a NEW fence. Any holder still running carries the
    // old one, and the server would keep honouring its heartbeats (it matches
    // holder+fence, and this machine's holder string is unchanged) — so retire
    // it BEFORE acquiring rather than leaving two holders racing (F-92).
    stop_holder(name);

    let resp = api.acquire_lease(name, &me)?;
    if resp.granted {
        return Ok(resp);
    }
    if take_over {
        let taken = api.takeover_lease(name, &me)?;
        report(&format!(
            "took over the lease from {} (its next write will be refused)",
            resp.holder
        ));
        return Ok(taken);
    }
    // Typed, so each driver can offer its own way to take over (the CLI's
    // flag, the page's button) without the core knowing either.
    Err(HeldElsewhere {
        session: name.to_string(),
        holder: resp.holder,
        expires_in_secs: (resp.expires_at_unix - now_unix()).max(0),
    }
    .into())
}

/// Is `pid` one of OUR holder processes for this session AND this fence?
///
/// Checked before any kill: destroying a PID without verifying what it is would
/// be the F-79 shape. The match is on **exact argv elements**, not a substring
/// of the joined cmdline (F-92): PIDs are reused, and a substring test matches
/// any holder whose session name merely contains ours (`proj` inside
/// `proj-backup`), so the wrong process could be signalled. The fence is part of
/// the identity because a holder on a stale fence is precisely NOT the holder we
/// think we have.
fn holder_process_is_ours(pid: u32, session: &str, fence: i64) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    // /proc cmdline is NUL-separated argv with a trailing NUL.
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let has = |needle: &str| argv.iter().any(|a| a == needle);
    has("__hold") && has(session) && has(&fence.to_string())
}

/// Ensure a heartbeat holder is running for exactly this (session, fence), and
/// record the lease state.
///
/// Idempotent by design (F-92): if a holder is already live on this same fence
/// it is kept — spawning a second would leak the first, whose heartbeats the
/// server would keep honouring because they carry identical credentials. A
/// holder on any *other* fence is retired first.
fn ensure_holder(name: &str, lease: &LeaseResponse) -> Result<()> {
    if let Some(existing) = state::load_lease(name) {
        if existing.fence == lease.fence
            && existing
                .holder_pid
                .is_some_and(|pid| holder_process_is_ours(pid, name, lease.fence))
        {
            // Already held by a live holder on this fence: refresh the recorded
            // expiry and keep the process.
            return state::save_lease(
                name,
                &LeaseState {
                    holder: lease.holder.clone(),
                    fence: lease.fence,
                    expires_at_unix: lease.expires_at_unix,
                    ttl_seconds: lease.ttl_seconds,
                    status: "held".into(),
                    holder_pid: existing.holder_pid,
                },
            );
        }
        stop_holder(name);
    }

    let exe = holder_binary()?;
    // Heartbeat at a third of the lease's FULL TTL, which the server reports —
    // never at a third of what happens to remain on a partly-elapsed lease,
    // which on a reused hold would collapse to a hot loop (F-92).
    //
    // The floor clamps the INTERVAL, never the TTL: clamping the TTL upward
    // would make the holder renew more slowly than the lease actually expires,
    // eating the 3x safety margin and letting a live session's lease lapse
    // under load. TTL/3 is the margin; 200ms only stops a pathological spin.
    let interval_ms = ((lease.ttl_seconds.max(1) as u64 * 1000) / 3).max(200);

    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__hold")
        .arg(name)
        .arg("--holder")
        .arg(&lease.holder)
        .arg("--fence")
        .arg(lease.fence.to_string())
        .arg("--interval-ms")
        .arg(interval_ms.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Detach: survive this CLI's exit and its terminal.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn().context("spawning the lease heartbeat holder")?;

    state::save_lease(
        name,
        &LeaseState {
            holder: lease.holder.clone(),
            fence: lease.fence,
            expires_at_unix: lease.expires_at_unix,
            ttl_seconds: lease.ttl_seconds,
            status: "held".into(),
            holder_pid: Some(child.id()),
        },
    )
}

/// Stop our holder process for a session, verifying it is ours first, and wait
/// for it to actually go — a release that races its own holder's next heartbeat
/// would re-create the state it just cleared.
fn stop_holder(name: &str) {
    let Some(lease) = state::load_lease(name) else {
        return;
    };
    let Some(pid) = lease.holder_pid else {
        return;
    };
    if !holder_process_is_ours(pid, name, lease.fence) {
        return;
    }
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    for _ in 0..100 {
        if !holder_process_is_ours(pid, name, lease.fence) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

// --- push / pull / release ---------------------------------------------------

/// A temp directory only this user can enter, for the **plaintext** bundle.
///
/// F-92: `tempfile::tempdir()` honours the process umask — measured 0775 with a
/// 0664 file on the reference host — so the decrypted session, the very thing
/// E-16 exists to keep private, sat world-readable in `/tmp` for the length of a
/// push or pull. Any local user could read it. The mode is set on the directory
/// **before** anything is written into it, so there is no window where the
/// bundle exists under a permissive mode.
fn private_tempdir() -> Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().context("creating a temp directory")?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .context("restricting the temp directory to this user")?;
    Ok(dir)
}

/// What a push did, for the driver to render (the CLI says it all through
/// `report`; the UI's stop-and-push reads these).
#[allow(dead_code)]
pub struct PushReport {
    pub plaintext_bytes: usize,
    pub ciphertext_bytes: i64,
    pub released: bool,
    pub holder: String,
}

/// `nemr push`: export through the engine, encrypt here, upload under the
/// lease's fence. `report` receives the CLI's progress lines as they happen.
pub fn push(
    name: &str,
    password: &str,
    release_after: bool,
    take_over: bool,
    engine: &dyn EngineOps,
    report: &mut dyn FnMut(&str),
) -> Result<PushReport> {
    let account = state::load_account()?;
    let mk = keys::master_key(&account, password)?;
    let api = Api::new(&account.server, Some(account.token.clone()));

    let locals = engine.list()?;
    let project = locals
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| anyhow!("no local project {name:?} — see `nemr list`"))?;
    if project.running {
        bail!("project {name:?} is running; stop it first so the volume is quiescent: nemr stop {name}");
    }

    // The index entry first, so the lease has a session row to hang off.
    api.upsert_session(&json!({
        "name": name,
        "agent": project.agent,
        "size_bytes": project.used_bytes,
        "last_machine": state::holder_identity(),
    }))?;

    let lease = ensure_lease(&api, name, take_over, report)?;
    report(&format!("holding the lease as {}", lease.holder));

    // Export through the engine, encrypt here, upload ciphertext. The
    // plaintext bundle exists only inside this tempdir, briefly — the same
    // artifact a manual `nemr export` produces, on the user's own disk.
    let tmp = private_tempdir()?;
    let bundle_path = tmp.path().join(format!("{name}.nemr"));
    report("exporting the session");
    engine.export(name, &bundle_path)?;
    let plaintext = std::fs::read(&bundle_path).context("reading the exported bundle")?;
    report(&format!(
        "encrypting {} client-side",
        human_bytes(plaintext.len() as i64)
    ));
    let ciphertext = encrypt_bundle(&mk, &plaintext);
    let ciphertext_len = ciphertext.len() as i64;

    report(&format!(
        "uploading {} of ciphertext",
        human_bytes(ciphertext_len)
    ));
    let result = api.upload_bundle(name, &lease.holder, lease.fence, ciphertext);
    let ciphertext_bytes = match result {
        Ok(info) => {
            let bytes = info["bytes"].as_i64().unwrap_or(ciphertext_len);
            report(&format!(
                "pushed {name:?}: {} plaintext -> {} ciphertext (encrypted client-side; the server cannot read it)",
                human_bytes(plaintext.len() as i64),
                human_bytes(bytes),
            ));
            bytes
        }
        Err(e) => {
            if let Some(lost) = e.downcast_ref::<LeaseLost>() {
                // The server refused the write: this machine no longer holds the
                // lease. Do not retry, do not take over silently — say so.
                state::save_lease(
                    name,
                    &LeaseState {
                        holder: lease.holder.clone(),
                        fence: lease.fence,
                        expires_at_unix: lease.expires_at_unix,
                        ttl_seconds: lease.ttl_seconds,
                        status: "lost".into(),
                        holder_pid: None,
                    },
                )?;
                bail!(
                    "not writing: {lost}\n\
                     Another machine holds this session now. Its work would be overwritten.\n\
                     If you are sure, re-run with --take-over."
                );
            }
            return Err(e);
        }
    };

    if release_after {
        stop_holder(name);
        api.release_lease(name, &lease.holder, lease.fence)?;
        state::delete_lease(name);
        report("lease released");
    } else {
        // Unconditionally, because ensure_holder is idempotent per fence. The
        // old fence-blind "only if no live holder" guard was the defect (F-92):
        // when a re-acquire advanced the fence while the previous holder was
        // still alive, the guard saw a live process and skipped — leaving the
        // NEW fence recorded nowhere, no one heartbeating it, and a later
        // release sending a stale fence that the server refused, stranding the
        // lease held until its TTL ran out.
        ensure_holder(name, &lease)?;
    }
    Ok(PushReport {
        plaintext_bytes: plaintext.len(),
        ciphertext_bytes,
        released: release_after,
        holder: lease.holder,
    })
}

/// What a pull did, for the driver to render.
pub struct PullReport {
    /// The name the engine created (from the manifest).
    pub imported: String,
    pub holder: String,
    pub plaintext_bytes: usize,
}

/// `nemr pull`: take the lease, download, decrypt here, import through the
/// engine, keep the lease held. `report` receives the progress lines.
pub fn pull(
    name: &str,
    password: &str,
    take_over: bool,
    engine: &dyn EngineOps,
    report: &mut dyn FnMut(&str),
) -> Result<PullReport> {
    let account = state::load_account()?;
    let mk = keys::master_key(&account, password)?;
    let api = Api::new(&account.server, Some(account.token.clone()));

    let sessions = api.sessions()?;
    let session = sessions
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow!("no session {name:?} on the server — see `nemr sessions`"))?;
    if !session.has_bundle {
        bail!("session {name:?} has no uploaded bundle yet (push it from the machine that has it)");
    }

    // Take the lease BEFORE materializing anything: pulling is this machine
    // claiming the session (D-03).
    let lease = ensure_lease(&api, name, take_over, report)?;
    report(&format!("holding the lease as {}", lease.holder));

    report(&format!(
        "downloading {} of ciphertext",
        session
            .ciphertext_bytes
            .map(human_bytes)
            .unwrap_or_else(|| "the bundle".into())
    ));
    let ciphertext = api.download_bundle(name)?;
    report("decrypting client-side");
    let plaintext = decrypt_bundle(&mk, &ciphertext).map_err(|_| {
        anyhow!("the downloaded bundle does not decrypt — wrong key or corrupted ciphertext")
    })?;

    let tmp = private_tempdir()?;
    let bundle_path = tmp.path().join(format!("{name}.nemr"));
    std::fs::write(&bundle_path, &plaintext).context("writing the decrypted bundle")?;
    report(&format!(
        "importing {} into the engine",
        human_bytes(plaintext.len() as i64)
    ));
    let imported = engine.import(&bundle_path, name)?;
    report(&format!("imported as {imported:?}"));

    ensure_holder(name, &lease)?;
    report(&format!(
        "holding the lease as {} (heartbeating in the background)",
        lease.holder
    ));
    Ok(PullReport {
        imported,
        holder: lease.holder,
        plaintext_bytes: plaintext.len(),
    })
}

/// What `release` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOutcome {
    NoLeaseHere,
    Released,
    AlreadyLost,
}

pub fn release(name: &str) -> Result<ReleaseOutcome> {
    let account = state::load_account()?;
    let api = Api::new(&account.server, Some(account.token));
    let Some(lease) = state::load_lease(name) else {
        return Ok(ReleaseOutcome::NoLeaseHere);
    };
    stop_holder(name);
    let outcome = match api.release_lease(name, &lease.holder, lease.fence) {
        Ok(()) => ReleaseOutcome::Released,
        Err(e) if e.downcast_ref::<LeaseLost>().is_some() => ReleaseOutcome::AlreadyLost,
        Err(e) => return Err(e),
    };
    state::delete_lease(name);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(name: &str, held_by: Option<&str>) -> SessionEntry {
        SessionEntry {
            name: name.into(),
            agent: "claude-code".into(),
            size_bytes: 10,
            last_machine: Some("laptop".into()),
            has_bundle: true,
            ciphertext_bytes: Some(1234),
            updated_at_unix: 1_800_000_000,
            held_by: held_by.map(String::from),
            lease_expires_at_unix: held_by.map(|_| 1_800_000_600),
        }
    }
    fn local(name: &str, running: bool) -> LocalProject {
        LocalProject {
            name: name.into(),
            agent: "claude-code".into(),
            running,
            usage_known: true,
            used_bytes: 500,
            credential_present: None,
        }
    }

    /// The three states a session can be in from this machine, sorted by name,
    /// and the holder carried through — the list D-03's UX rests on.
    #[test]
    fn rows_are_marked_local_remote_or_both_and_carry_the_holder() {
        let rows = merge_rows(
            &[remote("b-remote", Some("desktop")), remote("c-both", None)],
            Some(&[local("a-local", true), local("c-both", false)]),
        );
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["a-local", "b-remote", "c-both"]);
        assert_eq!(rows[0].location, Where::Local);
        assert!(rows[0].running);
        assert_eq!(rows[1].location, Where::Remote);
        assert_eq!(
            rows[1].held_by.as_deref(),
            Some("desktop"),
            "open elsewhere must show"
        );
        assert_eq!(rows[2].location, Where::Both);
        assert_eq!(rows[2].held_by, None);
        assert_eq!(
            rows[2].size_bytes,
            Some(1234),
            "the server's ciphertext size wins when both exist"
        );
    }

    /// No engine: the server's list alone still renders — remote rows, none
    /// running, sizes from the server.
    #[test]
    fn without_a_local_view_the_server_list_still_renders() {
        let rows = merge_rows(&[remote("x", None)], None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].location, Where::Remote);
        assert!(!rows[0].running);
        assert_eq!(rows[0].size_bytes, Some(1234));
    }
}
