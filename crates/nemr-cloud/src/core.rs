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
//! `push` and `pull` stay in `commands` for now; they move here when the
//! UI's pull-and-start and stop-and-push steps are built, in the same shape.

use anyhow::{anyhow, Result};
use nemr_crypto::{recovery_acknowledgement, MasterKey, RecoveryCode};
use rand::RngCore;
use serde_json::json;

use crate::api::{Api, SessionEntry};
use crate::engine_cli::LocalProject;
use crate::keys::{self, b64, unb64};
use crate::state::{self, Account};

const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";

/// The server to talk to when none is named: `NEMR_SERVER_URL`, else the
/// development default.
pub fn default_server() -> String {
    std::env::var("NEMR_SERVER_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_SERVER.to_string())
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

/// Is anyone logged in on this machine, and as whom?
pub fn whoami() -> Option<Account> {
    state::load_account().ok()
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
