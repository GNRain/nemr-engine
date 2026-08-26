//! E-16 — client-side key derivation and bundle encryption.
//!
//! The server stores ciphertext it cannot read. This crate is the client half
//! that makes that true: it derives keys from the user's password, wraps a
//! random data key the server never sees, and encrypts bundles under it.
//!
//! # The key hierarchy (E-16, Option B)
//!
//! ```text
//!   password + salt ──Argon2id──▶ root
//!                                  ├─HKDF(info="…/auth/v1")─▶ auth_key   (sent to the server)
//!                                  └─HKDF(info="…/wrap/v1")─▶ wrap_key   (never leaves the client)
//!
//!   MK ← 32 random bytes                          (the data key; the server never sees it)
//!   password_envelope = AEAD_seal(wrap_key, MK)   (stored server-side, opaque)
//!
//!   recovery_code ← random, shown once
//!   recovery_envelope = AEAD_seal(HKDF(Argon2id(recovery_code), "…/recovery-wrap/v1"), MK)
//!
//!   bundle_ciphertext = AEAD_seal(MK, plaintext)  (what the storage backend holds)
//! ```
//!
//! A **random master key wrapped by a password-derived envelope** is what makes
//! every future login method — GitHub OAuth, per-device caching, recovery — a
//! new envelope over the *same* MK, with no re-encryption of any bundle. That is
//! why E-16 chose it over deriving the data key from the password directly.
//!
//! # Why these primitives (no invented constructions)
//!
//! - **Argon2id** is the password-stretching barrier. Its cost is what a thief
//!   of the server database must pay per password guess; the salt is public
//!   (the server must return it at login), so Argon2id — not secrecy of the
//!   salt — is the whole defence. Parameters are stated in [`KdfParams`].
//! - **HKDF-SHA256** provides *domain separation*: `auth_key` and `wrap_key`
//!   come from the same root but different `info` labels, so a server that sees
//!   `auth_key` cannot derive `wrap_key`, and therefore cannot open an envelope.
//!   This is the property that lets authentication travel through the server
//!   while the data key does not.
//! - **XChaCha20-Poly1305** (AEAD) wraps the MK and encrypts bundles. Its
//!   192-bit nonce is safe to draw at random, so there is no nonce counter to
//!   persist or to get wrong across machines.
//!
//! # D-02 is a different secret
//!
//! D-02 governs Anthropic's credential — someone else's secret, per-device,
//! which never travels. E-16 governs the user's own bundle key, which *must*
//! travel, as ciphertext the server cannot open. Same word "key", opposite
//! requirement; they do not conflict.

mod aead;
mod bundle;
mod envelope;
mod kdf;

pub use bundle::{decrypt_bundle, encrypt_bundle};
pub use envelope::{recovery_acknowledgement, AuthKey, Envelope, MasterKey, RecoveryCode, WrapKey};
pub use kdf::{derive_root, KdfParams, RootKey};

/// Everything that can go wrong in this crate.
///
/// Deliberately coarse: a caller must never learn *why* a decryption failed —
/// "wrong key" and "tampered ciphertext" are the same [`CryptoError::Decrypt`]
/// so a padding-oracle-style probe learns nothing. The distinctions that are
/// safe to surface (a malformed envelope, an unparseable recovery code) are
/// their own variants.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    /// AEAD open failed: the key was wrong or the ciphertext was tampered with.
    /// The two are indistinguishable on purpose.
    #[error("decryption failed")]
    Decrypt,
    /// A stored blob was too short or had an unknown version byte.
    #[error("malformed {what}")]
    Malformed { what: &'static str },
    /// The recovery code was not the expected length or alphabet.
    #[error("invalid recovery code")]
    InvalidRecoveryCode,
    /// Argon2id rejected the parameters (e.g. memory below its floor).
    #[error("key derivation failed: {0}")]
    Kdf(String),
}

/// A 256-bit AEAD key produced by [`kdf`] and consumed by [`envelope`].
///
/// Kept out of the public surface as a bare `[u8; 32]` so callers cannot build
/// one from arbitrary bytes and bypass derivation.
#[derive(Clone, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub(crate) struct SymKey([u8; 32]);

impl SymKey {
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
