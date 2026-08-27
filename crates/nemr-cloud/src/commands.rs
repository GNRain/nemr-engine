//! The client commands: register, login, logout, sessions, push, pull,
//! release, and the internal lease-heartbeat holder.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use nemr_crypto::{
    decrypt_bundle, encrypt_bundle, recovery_acknowledgement, MasterKey, RecoveryCode,
};
use rand::RngCore;
use serde_json::json;

use crate::api::{Api, LeaseLost, LeaseResponse};
use crate::engine_cli;
use crate::keys::{self, b64, unb64};
use crate::state::{self, Account, LeaseState};

const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn resolve_server(flag: Option<String>) -> String {
    flag.or_else(|| std::env::var("NEMR_SERVER_URL").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SERVER.to_string())
}

fn resolve_email(flag: Option<String>) -> Result<String> {
    if let Some(e) = flag.or_else(|| std::env::var("NEMR_CLOUD_EMAIL").ok()) {
        if !e.is_empty() {
            return Ok(e);
        }
    }
    eprint!("Email: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading email")?;
    let email = line.trim().to_string();
    if email.is_empty() {
        bail!("an email is required");
    }
    Ok(email)
}

// --- register ---------------------------------------------------------------

pub fn register(server: Option<String>, email: Option<String>) -> Result<()> {
    let server = resolve_server(server);
    let email = resolve_email(email)?;
    let password = keys::read_password(true)?;
    let api = Api::new(&server, None);

    // The full E-16 material, generated client-side. The server receives only
    // what it can store without being able to read anything: the auth key (it
    // hashes), public salts/params, sealed envelopes, and a one-way ack hash.
    let params = keys::registration_params();
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let root = keys::derive(&password, &salt, params)?;
    let mk = MasterKey::generate();
    let password_envelope = root.wrap_key().seal(&mk);

    let recovery_code = RecoveryCode::generate();
    let mut recovery_salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut recovery_salt);
    // The recovery code is raw bytes, not text — derive over the bytes.
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

    // Recovery is not deferrable (E-16): show the code once, and the account
    // stays unusable until the user proves they stored it by typing it back.
    println!();
    println!("Your recovery code — the ONLY way back in if you forget your password:");
    println!();
    println!("    {}", recovery_code.display());
    println!();
    println!("Store it now (password manager, paper — not this machine).");
    println!("A forgotten password with no recovery code means your data is");
    println!("unrecoverable, permanently: the server cannot read it (E-16).");
    println!();
    // Piped stdout is block-buffered: flush so a driver (or a human paging
    // output) sees the code before we block waiting for it to be typed back.
    std::io::stdout().flush().ok();

    // Retry rather than abort (F-92). The account row already exists at this
    // point, so a single-shot confirm that exits on a typo strands the user:
    // re-running `register` hits "email already registered", and the code was
    // shown once and is now gone from the screen. The code is still in memory
    // here, so ask again — and if the user gives up, say exactly how to finish.
    let mut recovered = None;
    for attempt in 1..=5 {
        eprint!("Type the recovery code to confirm you stored it: ");
        std::io::stderr().flush().ok();
        let mut typed = String::new();
        if std::io::stdin()
            .read_line(&mut typed)
            .context("reading the recovery code")?
            == 0
        {
            break; // stdin closed
        }
        // The confirmation is real, not a string compare: recover the master
        // key THROUGH the recovery envelope with the typed code and prove it
        // matches.
        let opened = RecoveryCode::parse(typed.trim()).ok().and_then(|code| {
            nemr_crypto::derive_root(code.as_secret(), &recovery_salt, params)
                .ok()
                .and_then(|root| root.recovery_wrap_key().open(&recovery_envelope).ok())
        });
        match opened {
            Some(mk) => {
                recovered = Some(mk);
                break;
            }
            None if attempt < 5 => {
                eprintln!("that code does not open the recovery envelope — try again.");
                eprintln!("(it is the code printed above, hyphens and case do not matter)");
            }
            None => {}
        }
    }
    let Some(recovered) = recovered else {
        bail!(
            "recovery was not confirmed, so the account is registered but NOT usable.\n\
             The recovery code above is the only copy — store it, then finish with:\n\
             \n    nemr login\n\n\
             and re-run the confirmation. Registering again with this email will be\n\
             refused because the account now exists."
        );
    };
    api.confirm_recovery(&email, &b64(&recovery_acknowledgement(&recovered)))?;
    println!("Recovery confirmed. Account is active.");

    // Log straight in so `register` ends in a usable state.
    finish_login(&api, &server, &email, root.auth_key().as_bytes())
}

// --- login / logout ---------------------------------------------------------

pub fn login(server: Option<String>, email: Option<String>) -> Result<()> {
    let server = resolve_server(server);
    let email = resolve_email(email)?;
    let password = keys::read_password(false)?;
    let api = Api::new(&server, None);

    let p = api.kdf_params(&email)?;
    let salt = unb64(&p.kdf_salt, "server KDF salt")?;
    // The params come from an unauthenticated endpoint: hold them to a floor
    // before stretching the real password with them (F-92).
    let params = keys::check_params_floor(nemr_crypto::KdfParams {
        m_cost: p.kdf_m_cost,
        t_cost: p.kdf_t_cost,
        p_cost: p.kdf_p_cost,
    })?;
    let root = keys::derive(&password, &salt, params)?;
    finish_login(&api, &server, &email, root.auth_key().as_bytes())
}

fn finish_login(api: &Api, server: &str, email: &str, auth_key: &[u8; 32]) -> Result<()> {
    let resp = api.login(email, &b64(auth_key))?;
    state::save_account(&Account {
        server: server.to_string(),
        email: email.to_string(),
        token: resp.token,
        kdf_salt: resp.kdf_salt,
        kdf_m_cost: resp.kdf_m_cost,
        kdf_t_cost: resp.kdf_t_cost,
        kdf_p_cost: resp.kdf_p_cost,
        password_envelope: resp.password_envelope,
    })?;
    println!("logged in as {email} ({server})");
    Ok(())
}

pub fn logout() -> Result<()> {
    match state::load_account() {
        Ok(account) => {
            // Revoke server-side first; a local-only logout leaves a live token
            // on the server for its whole TTL. Best-effort: an unreachable
            // server must not trap the user in a logged-in state.
            let api = Api::new(&account.server, Some(account.token.clone()));
            if let Err(e) = api.logout() {
                eprintln!("warning: could not revoke the token server-side: {e:#}");
                eprintln!("         (it expires on its own; local state is cleared regardless)");
            }
            state::delete_account()?;
            println!("logged out");
        }
        Err(_) => println!("not logged in"),
    }
    Ok(())
}

// --- sessions ---------------------------------------------------------------

pub fn sessions() -> Result<()> {
    let account = state::load_account()?;
    let api = Api::new(&account.server, Some(account.token.clone()));
    let remote = api.sessions()?;

    // Local projects, best-effort: sync must still show the server list on a
    // machine where the engine is absent or its daemon cannot start.
    let local = match engine_cli::list_projects() {
        Ok(list) => Some(list),
        Err(e) => {
            eprintln!("note: local projects unavailable ({e:#});");
            eprintln!("      showing the server index only.");
            None
        }
    };

    let mut names: Vec<String> = remote.iter().map(|s| s.name.clone()).collect();
    if let Some(local) = &local {
        for p in local {
            if !names.contains(&p.name) {
                names.push(p.name.clone());
            }
        }
    }
    names.sort();

    if names.is_empty() {
        println!("no sessions anywhere. Create one with: nemr create <name> --size 2GB");
        return Ok(());
    }

    println!(
        "{:<18} {:<8} {:<7} {:<9} {:<12} LAST MACHINE",
        "NAME", "AGENT", "WHERE", "SIZE", "UPDATED"
    );
    for name in &names {
        let r = remote.iter().find(|s| &s.name == name);
        let l = local
            .as_ref()
            .and_then(|list| list.iter().find(|p| &p.name == name));
        // A session that exists remotely but not locally is a normal state —
        // that is the whole product — so WHERE says it plainly.
        let wher = match (l.is_some(), r.is_some()) {
            (true, true) => "both",
            (true, false) => "local",
            (false, true) => "remote",
            (false, false) => unreachable!("name came from one of the lists"),
        };
        let agent = l
            .map(|p| p.agent.clone())
            .or_else(|| r.map(|s| s.agent.clone()))
            .unwrap_or_default();
        let size = r
            .and_then(|s| s.ciphertext_bytes)
            .map(human_bytes)
            .or_else(|| {
                l.filter(|p| p.usage_known)
                    .map(|p| human_bytes(p.used_bytes as i64))
            })
            .or_else(|| r.map(|s| s.size_bytes).filter(|n| *n > 0).map(human_bytes))
            .unwrap_or_else(|| "-".into());
        let updated = r
            .map(|s| ago(s.updated_at_unix))
            .unwrap_or_else(|| "-".into());
        let machine = r
            .and_then(|s| s.last_machine.clone())
            .unwrap_or_else(|| "-".into());
        println!("{name:<18} {agent:<8} {wher:<7} {size:<9} {updated:<12} {machine}");
    }
    Ok(())
}

fn human_bytes(n: i64) -> String {
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

fn ago(unix: i64) -> String {
    let d = (now_unix() - unix).max(0);
    if d < 60 {
        format!("{d}s ago")
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else {
        format!("{}d ago", d / 86400)
    }
}

// --- the lease, client side --------------------------------------------------

/// Ensure this machine holds the session's lease, returning the live fence.
///
/// A healthy hold already maintained by our heartbeat holder is REUSED — a
/// fresh acquire would advance the fence and kill our own holder mid-flight.
/// Otherwise acquire; a lease held by another machine is refused with its
/// holder named, unless `take_over`.
fn ensure_lease(api: &Api, name: &str, take_over: bool) -> Result<LeaseResponse> {
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
        eprintln!(
            "took over the lease from {} (its next write will be refused)",
            resp.holder
        );
        return Ok(taken);
    }
    bail!(
        "session {name:?} is held by {} (lease expires in {}s).\n\
         Take it over with --take-over — the other machine will be locked out of writing.",
        resp.holder,
        (resp.expires_at_unix - now_unix()).max(0)
    )
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

    let exe = std::env::current_exe().context("locating our own binary")?;
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

pub fn push(name: &str, release_after: bool, take_over: bool) -> Result<()> {
    let account = state::load_account()?;
    let password = keys::read_password(false)?;
    let mk = keys::master_key(&account, &password)?;
    let api = Api::new(&account.server, Some(account.token.clone()));

    let locals = engine_cli::list_projects()?;
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

    let lease = ensure_lease(&api, name, take_over)?;

    // Export through the open CLI, encrypt here, upload ciphertext. The
    // plaintext bundle exists only inside this tempdir, briefly — the same
    // artifact a manual `nemr export` produces, on the user's own disk.
    let tmp = private_tempdir()?;
    let bundle_path = tmp.path().join(format!("{name}.nemr"));
    engine_cli::export(name, &bundle_path)?;
    let plaintext = std::fs::read(&bundle_path).context("reading the exported bundle")?;
    let ciphertext = encrypt_bundle(&mk, &plaintext);

    let result = api.upload_bundle(name, &lease.holder, lease.fence, ciphertext);
    match result {
        Ok(info) => {
            println!(
                "pushed {name:?}: {} plaintext -> {} ciphertext (encrypted client-side; the server cannot read it)",
                human_bytes(plaintext.len() as i64),
                info["bytes"].as_i64().map(human_bytes).unwrap_or_default(),
            );
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
    }

    if release_after {
        stop_holder(name);
        api.release_lease(name, &lease.holder, lease.fence)?;
        state::delete_lease(name);
        println!("lease released");
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
    Ok(())
}

pub fn pull(name: &str, take_over: bool) -> Result<()> {
    let account = state::load_account()?;
    let password = keys::read_password(false)?;
    let mk = keys::master_key(&account, &password)?;
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
    let lease = ensure_lease(&api, name, take_over)?;

    let ciphertext = api.download_bundle(name)?;
    let plaintext = decrypt_bundle(&mk, &ciphertext).map_err(|_| {
        anyhow!("the downloaded bundle does not decrypt — wrong key or corrupted ciphertext")
    })?;

    let tmp = private_tempdir()?;
    let bundle_path = tmp.path().join(format!("{name}.nemr"));
    std::fs::write(&bundle_path, &plaintext).context("writing the decrypted bundle")?;
    let out = engine_cli::import(&bundle_path)?;
    print!("{out}");

    ensure_holder(name, &lease)?;
    println!(
        "holding the lease as {} (heartbeating in the background)",
        lease.holder
    );
    Ok(())
}

pub fn release(name: &str) -> Result<()> {
    let account = state::load_account()?;
    let api = Api::new(&account.server, Some(account.token));
    let Some(lease) = state::load_lease(name) else {
        println!("no lease state for {name:?} on this machine");
        return Ok(());
    };
    stop_holder(name);
    match api.release_lease(name, &lease.holder, lease.fence) {
        Ok(()) => println!("released the lease on {name:?}"),
        Err(e) if e.downcast_ref::<LeaseLost>().is_some() => {
            println!("lease on {name:?} was already lost (taken over or expired)");
        }
        Err(e) => return Err(e),
    }
    state::delete_lease(name);
    Ok(())
}

// --- the heartbeat holder -----------------------------------------------------

/// The detached renewal loop: the client-side half of D-03's "heartbeat to
/// renew". It refuses to outlive its lease:
///
/// - a refused renewal (409) means taken over or expired → mark `lost`, exit;
/// - a TTL's worth of failed transport means we cannot KNOW we still hold it →
///   same verdict, because "no successful heartbeat within TTL" IS lease-lost
///   (the rule the restarted-daemon case is built on);
///
/// After either, writing requires a fresh acquire — and the server's fence
/// refuses the stale credentials regardless of what this process does.
pub fn hold(name: &str, holder: &str, fence: i64, interval_ms: u64) -> Result<()> {
    let account = state::load_account()?;
    let api = Api::new(&account.server, Some(account.token));
    let mut expires_at = now_unix() + (interval_ms as i64 * 3) / 1000;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        match api.heartbeat(name, holder, fence) {
            Ok(resp) => {
                expires_at = resp.expires_at_unix;
                let _ = state::save_lease(
                    name,
                    &LeaseState {
                        holder: holder.to_string(),
                        fence,
                        expires_at_unix: expires_at,
                        ttl_seconds: (interval_ms as i64 * 3) / 1000,
                        status: "held".into(),
                        holder_pid: Some(std::process::id()),
                    },
                );
            }
            Err(e) => {
                let lost = e.downcast_ref::<LeaseLost>().is_some();
                let past_ttl = now_unix() > expires_at;
                if lost || past_ttl {
                    let _ = state::save_lease(
                        name,
                        &LeaseState {
                            holder: holder.to_string(),
                            fence,
                            expires_at_unix: expires_at,
                            ttl_seconds: (interval_ms as i64 * 3) / 1000,
                            status: "lost".into(),
                            holder_pid: None,
                        },
                    );
                    // Exit non-zero: this process will not renew a lease it
                    // cannot prove it holds.
                    std::process::exit(1);
                }
                // Transient transport trouble inside the TTL: keep trying.
            }
        }
    }
}
