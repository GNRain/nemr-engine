//! Argon2id password stretching and HKDF domain separation.
//!
//! `password + salt ──Argon2id──▶ root`, then `root` is expanded by HKDF-SHA256
//! into purpose-bound subkeys. The Argon2id step is the brute-force barrier; the
//! HKDF step is what keeps the auth path from leaking the wrap key.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::envelope::{AuthKey, WrapKey};
use crate::{CryptoError, SymKey};

/// HKDF `info` labels. Distinct per purpose so no subkey derives another; the
/// version suffix lets a future scheme coexist without ambiguity.
const INFO_AUTH: &[u8] = b"nemr/kdf/auth/v1";
const INFO_WRAP: &[u8] = b"nemr/kdf/wrap/v1";
const INFO_RECOVERY_WRAP: &[u8] = b"nemr/kdf/recovery-wrap/v1";

/// Argon2id cost parameters.
///
/// Stored alongside the user (the salt and these numbers are public) so a login
/// on any machine reproduces the same derivation. Recorded per-user rather than
/// hard-coded so the cost can be raised for new accounts without invalidating
/// old ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_cost: u32,
    /// Iterations (time cost).
    pub t_cost: u32,
    /// Parallelism (lanes).
    pub p_cost: u32,
}

impl Default for KdfParams {
    /// OWASP's Argon2id recommendation as of 2024: 19 MiB, 2 iterations, 1 lane.
    /// The memory floor is what makes GPU/ASIC brute force expensive; iterations
    /// add cost on top. These are the numbers justified in SPEC §crypto.
    fn default() -> Self {
        Self {
            m_cost: 19 * 1024,
            t_cost: 2,
            p_cost: 1,
        }
    }
}

impl KdfParams {
    fn build(self) -> Result<Argon2<'static>, CryptoError> {
        let params = Params::new(self.m_cost, self.t_cost, self.p_cost, Some(32))
            .map_err(|e| CryptoError::Kdf(e.to_string()))?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
    }
}

/// The 32-byte Argon2id output, before HKDF separation.
///
/// Never sent anywhere and never used as an AEAD key directly — only expanded
/// into [`AuthKey`] and [`WrapKey`], which are domain-separated from each other.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RootKey([u8; 32]);

/// Run Argon2id over the password and salt.
///
/// `salt` is the per-user KDF salt (public). It must be the same on every login
/// or the derivation will not reproduce.
pub fn derive_root(
    password: &[u8],
    salt: &[u8],
    params: KdfParams,
) -> Result<RootKey, CryptoError> {
    let argon = params.build()?;
    let mut out = [0u8; 32];
    argon
        .hash_password_into(password, salt, &mut out)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(RootKey(out))
}

impl RootKey {
    /// The value the client sends to the server to authenticate. Domain-separated
    /// from the wrap key, so the server learning it reveals nothing about the
    /// key that opens envelopes.
    pub fn auth_key(&self) -> AuthKey {
        AuthKey(self.expand(INFO_AUTH))
    }

    /// The key that wraps the master key. Never leaves the client.
    pub fn wrap_key(&self) -> WrapKey {
        WrapKey(self.expand(INFO_WRAP))
    }

    /// The key that wraps the master key in the *recovery* envelope, derived
    /// from the recovery code rather than the password. Domain-separated from
    /// the password wrap key so the two envelopes are independent.
    pub fn recovery_wrap_key(&self) -> WrapKey {
        WrapKey(self.expand(INFO_RECOVERY_WRAP))
    }

    fn expand(&self, info: &[u8]) -> SymKey {
        // HKDF with no salt: the input keying material (the Argon2id root) is
        // already uniformly random over 256 bits, so an extract salt adds
        // nothing. `expand` with a distinct `info` is the separation.
        let hk = Hkdf::<Sha256>::new(None, &self.0);
        let mut out = [0u8; 32];
        hk.expand(info, &mut out)
            .expect("32 bytes is within HKDF-SHA256's output limit");
        SymKey(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cheap params so the suite stays fast; production uses KdfParams::default().
    fn fast() -> KdfParams {
        KdfParams {
            m_cost: 8,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn derivation_is_deterministic_for_the_same_inputs() {
        let a = derive_root(b"correct horse", b"salt-sixteen-byt", fast()).unwrap();
        let b = derive_root(b"correct horse", b"salt-sixteen-byt", fast()).unwrap();
        assert_eq!(a.auth_key().as_bytes(), b.auth_key().as_bytes());
        assert_eq!(a.wrap_key().0.as_bytes(), b.wrap_key().0.as_bytes());
    }

    #[test]
    fn a_different_salt_yields_a_different_root() {
        let a = derive_root(b"pw", b"salt-sixteen-byt", fast()).unwrap();
        let b = derive_root(b"pw", b"different-16-byte", fast()).unwrap();
        assert_ne!(a.auth_key().as_bytes(), b.auth_key().as_bytes());
    }

    #[test]
    fn a_different_password_yields_a_different_root() {
        let a = derive_root(b"pw one", b"salt-sixteen-byt", fast()).unwrap();
        let b = derive_root(b"pw two", b"salt-sixteen-byt", fast()).unwrap();
        assert_ne!(a.wrap_key().0.as_bytes(), b.wrap_key().0.as_bytes());
    }

    /// The load-bearing separation: from one root, the auth key and the wrap key
    /// must differ. If they were equal, a server that receives the auth key
    /// could open every envelope — the exact failure E-16 exists to prevent.
    #[test]
    fn auth_and_wrap_keys_are_domain_separated() {
        let root = derive_root(b"pw", b"salt-sixteen-byt", fast()).unwrap();
        assert_ne!(
            root.auth_key().as_bytes(),
            root.wrap_key().0.as_bytes(),
            "auth_key must never equal wrap_key, or the server could decrypt"
        );
    }

    #[test]
    fn parameters_below_the_argon2_floor_are_rejected_not_ignored() {
        // m_cost of 0 is invalid; it must surface as an error, never silently
        // fall back to a weaker derivation.
        let bad = KdfParams {
            m_cost: 0,
            t_cost: 1,
            p_cost: 1,
        };
        assert!(matches!(
            derive_root(b"pw", b"salt-sixteen-byt", bad),
            Err(CryptoError::Kdf(_))
        ));
    }
}
