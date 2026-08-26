//! One AEAD path, used for both envelopes and bundles.
//!
//! Format: `version || nonce(24) || ciphertext+tag`. The caller supplies the
//! associated data, which binds a ciphertext to its purpose — an envelope and a
//! bundle sealed under the same key are not interchangeable because their AAD
//! differs.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::{CryptoError, SymKey};

pub(crate) const VERSION: u8 = 1;
pub(crate) const NONCE_LEN: usize = 24;
pub(crate) const TAG_LEN: usize = 16;

/// Seal `plaintext` under `key` with a fresh random nonce and the given AAD.
pub(crate) fn seal(key: &SymKey, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("XChaCha20-Poly1305 encryption is infallible for valid inputs");
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
    out.push(VERSION);
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ct);
    out
}

/// Open a blob produced by [`seal`]. Returns [`CryptoError::Decrypt`] if the key
/// is wrong or the ciphertext was tampered with — the two are indistinguishable.
pub(crate) fn open(key: &SymKey, aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if blob.len() < 1 + NONCE_LEN + TAG_LEN || blob[0] != VERSION {
        return Err(CryptoError::Malformed {
            what: "sealed blob",
        });
    }
    let nonce = XNonce::from_slice(&blob[1..1 + NONCE_LEN]);
    let ct = &blob[1 + NONCE_LEN..];
    let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
    cipher
        .decrypt(nonce, Payload { msg: ct, aad })
        .map_err(|_| CryptoError::Decrypt)
}
