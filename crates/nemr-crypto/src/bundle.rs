//! Bundle encryption under the master key.
//!
//! Whole-bundle for this pass (D-11 storage shape). The AEAD format carries a
//! version byte, so per-chunk encryption (D-06) can arrive later without
//! reinterpreting existing ciphertext. A distinct AAD keeps a bundle ciphertext
//! from being opened as an envelope even under the same key.

use crate::aead;
use crate::envelope::MasterKey;
use crate::CryptoError;

/// Associated data binding a ciphertext to "this is a bundle, format v1".
const AAD_BUNDLE: &[u8] = b"nemr/bundle/v1";

/// Encrypt a whole bundle under the master key. The output is what the storage
/// backend holds; the server cannot read it.
pub fn encrypt_bundle(mk: &MasterKey, plaintext: &[u8]) -> Vec<u8> {
    aead::seal(mk.key(), AAD_BUNDLE, plaintext)
}

/// Decrypt a bundle. [`CryptoError::Decrypt`] on a wrong key or tampering.
pub fn decrypt_bundle(mk: &MasterKey, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    aead::open(mk.key(), AAD_BUNDLE, ciphertext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_round_trips_byte_identically() {
        let mk = MasterKey::generate();
        let plaintext = b"a real bundle would be tar.zst bytes; this stands in".repeat(64);
        let ct = encrypt_bundle(&mk, &plaintext);
        assert_ne!(ct, plaintext, "ciphertext must not equal plaintext");
        assert_eq!(decrypt_bundle(&mk, &ct).unwrap(), plaintext);
    }

    #[test]
    fn an_empty_bundle_still_round_trips() {
        let mk = MasterKey::generate();
        let ct = encrypt_bundle(&mk, b"");
        assert_eq!(decrypt_bundle(&mk, &ct).unwrap(), b"");
    }

    #[test]
    fn a_different_master_key_cannot_decrypt() {
        let mk = MasterKey::generate();
        let other = MasterKey::generate();
        let ct = encrypt_bundle(&mk, b"secret source code");
        assert_eq!(decrypt_bundle(&other, &ct), Err(CryptoError::Decrypt));
    }

    #[test]
    fn a_tampered_bundle_is_rejected() {
        let mk = MasterKey::generate();
        let mut ct = encrypt_bundle(&mk, b"secret source code");
        let mid = ct.len() / 2;
        ct[mid] ^= 0x01;
        assert_eq!(decrypt_bundle(&mk, &ct), Err(CryptoError::Decrypt));
    }

    #[test]
    fn a_bundle_ciphertext_cannot_be_opened_as_an_envelope() {
        // The AAD binds the ciphertext to its purpose: even holding the master
        // key, opening a bundle blob through the envelope path must fail. This
        // is what stops a confused-deputy swap between the two.
        let mk = MasterKey::generate();
        let ct = encrypt_bundle(&mk, &[0u8; 32]);
        // Envelope::from_bytes would reject on length; go straight to the AEAD
        // layer with the envelope AAD to prove the AAD itself is the guard.
        let wrong_aad = crate::aead::open(mk.key(), b"nemr/envelope/v1", &ct);
        assert_eq!(wrong_aad, Err(CryptoError::Decrypt));
    }
}
