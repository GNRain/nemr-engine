//! The E-16 client-side key flow, in one place.
//!
//! The master key is derived, used, and dropped inside a single command's
//! lifetime; it never touches disk (`MasterKey` is `ZeroizeOnDrop` in
//! nemr-crypto). What persists locally is the same material the server holds —
//! the KDF salt/params and the sealed envelope — which is safe at rest by
//! construction: opening it needs the password.

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use nemr_crypto::{derive_root, Envelope, KdfParams, MasterKey, RootKey};

use crate::state::Account;

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(s: &str, what: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| anyhow!("invalid base64 in {what}"))
}

/// The KDF cost registration chooses for new accounts.
///
/// Production is nemr-crypto's default (OWASP: 19 MiB, t=2, p=1).
/// `NEMR_CLOUD_KDF_FAST=1` drops it to a fast profile — for the test suites
/// only, where hundreds of derivations would otherwise dominate the runtime. It
/// **announces itself on stderr** (F-92): a silent cost downgrade is the kind of
/// thing that survives into a real account, and the whole brute-force barrier is
/// this number.
pub fn registration_params() -> KdfParams {
    if std::env::var("NEMR_CLOUD_KDF_FAST").as_deref() == Ok("1") {
        eprintln!(
            "warning: NEMR_CLOUD_KDF_FAST=1 — registering with TEST key-derivation cost.\n\
             This account's password is cheap to brute-force offline. Do not use it for real data."
        );
        KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    } else {
        KdfParams::default()
    }
}

/// The floor the client accepts for **server-supplied** KDF parameters at login.
///
/// F-92: `/v1/auth/params` is unauthenticated, so a hostile or impersonated
/// server can answer a login with `m_cost: 8` and the client would dutifully
/// derive — and transmit — an auth key stretched at near-zero cost, handing over
/// a cheaply-crackable value derived from the real password. The client must not
/// take cost policy from an untrusted party without a floor of its own.
///
/// The floor is deliberately well below the OWASP default so raising server-side
/// cost never breaks existing clients, and `NEMR_CLOUD_KDF_FAST=1` lowers it for
/// the test suites (which register at that cost on purpose).
pub fn min_accepted_params() -> KdfParams {
    if std::env::var("NEMR_CLOUD_KDF_FAST").as_deref() == Ok("1") {
        KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    } else {
        KdfParams {
            m_cost: 8 * 1024,
            t_cost: 1,
            p_cost: 1,
        }
    }
}

/// Refuse server-supplied parameters weaker than [`min_accepted_params`].
pub fn check_params_floor(p: KdfParams) -> Result<KdfParams> {
    let floor = min_accepted_params();
    if p.m_cost < floor.m_cost || p.t_cost < floor.t_cost {
        bail!(
            "the server offered weak key-derivation parameters (m={} KiB, t={}); \n\
             this client requires at least m={} KiB, t={}.\n\
             Refusing: deriving your key at that cost would hand over a value that is \n\
             cheap to crack offline. Check that you are talking to the right server.",
            p.m_cost,
            p.t_cost,
            floor.m_cost,
            floor.t_cost
        );
    }
    Ok(p)
}

/// Read the password: `NEMR_CLOUD_PASSWORD` (automation), else prompt with echo
/// off. `confirm` prompts twice and insists they match (registration).
pub fn read_password(confirm: bool) -> Result<String> {
    if let Ok(p) = std::env::var("NEMR_CLOUD_PASSWORD") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    let p = rpassword::prompt_password("Password: ").context("reading the password")?;
    if p.is_empty() {
        bail!("an empty password is not accepted");
    }
    if confirm {
        let again = rpassword::prompt_password("Confirm password: ")?;
        if p != again {
            bail!("passwords do not match");
        }
    }
    Ok(p)
}

pub fn derive(password: &str, salt: &[u8], params: KdfParams) -> Result<RootKey> {
    derive_root(password.as_bytes(), salt, params)
        .map_err(|e| anyhow!("key derivation failed: {e}"))
}

/// Unwrap the master key from the stored account state and the password.
///
/// A wrong password surfaces as exactly that: the AEAD refuses, and there is
/// nothing else in this path that can fail the same way.
pub fn master_key(account: &Account, password: &str) -> Result<MasterKey> {
    let salt = unb64(&account.kdf_salt, "stored KDF salt")?;
    let params = KdfParams {
        m_cost: account.kdf_m_cost,
        t_cost: account.kdf_t_cost,
        p_cost: account.kdf_p_cost,
    };
    let root = derive(password, &salt, params)?;
    let envelope = Envelope::from_bytes(&unb64(&account.password_envelope, "stored envelope")?)
        .map_err(|e| anyhow!("stored envelope is malformed: {e}"))?;
    root.wrap_key()
        .open(&envelope)
        .map_err(|_| anyhow!("wrong password"))
}
