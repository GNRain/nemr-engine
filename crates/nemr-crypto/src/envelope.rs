//! The master key, the envelopes that wrap it, and the recovery code.
//!
//! An *envelope* is the master key sealed under a wrapping key with an AEAD. The
//! server stores envelopes as opaque blobs; only a client holding the wrapping
//! key (from the password, or from the recovery code) can open one.

use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::aead;
use crate::{CryptoError, SymKey};

/// Associated data binding an envelope's ciphertext to its purpose and version.
/// A blob sealed as an envelope cannot be opened as a bundle, or vice versa,
/// even under the same key.
const AAD_ENVELOPE: &[u8] = b"nemr/envelope/v1";

/// Domain prefix for the recovery acknowledgement hash.
const ACK_DOMAIN: &[u8] = b"nemr/recovery-ack/v1";

// master key (32) + Poly1305 tag (16).
const SEALED_MK_LEN: usize = 32 + aead::TAG_LEN;
const ENVELOPE_TOTAL_LEN: usize = 1 + aead::NONCE_LEN + SEALED_MK_LEN;

/// The value the client sends to the server to authenticate.
///
/// Domain-separated from the wrap key, so a server holding this cannot open an
/// envelope. It is exposed as bytes precisely because it is meant to travel.
pub struct AuthKey(pub(crate) SymKey);

impl AuthKey {
    /// The bytes to transmit to the server. This is the one key that is meant
    /// to leave the client.
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

/// The key that wraps the master key. Never leaves the client, and has no method
/// to expose its bytes — it can only seal and open.
pub struct WrapKey(pub(crate) SymKey);

impl WrapKey {
    /// Seal the master key into an envelope.
    pub fn seal(&self, mk: &MasterKey) -> Envelope {
        Envelope(aead::seal(&self.0, AAD_ENVELOPE, mk.0.as_bytes()))
    }

    /// Open an envelope, recovering the master key. Fails as [`CryptoError::Decrypt`]
    /// if the key is wrong or the envelope was tampered with — the two are
    /// indistinguishable on purpose.
    pub fn open(&self, envelope: &Envelope) -> Result<MasterKey, CryptoError> {
        let bytes = aead::open(&self.0, AAD_ENVELOPE, &envelope.0)?;
        if bytes.len() != 32 {
            return Err(CryptoError::Malformed { what: "envelope" });
        }
        let mut mk = [0u8; 32];
        mk.copy_from_slice(&bytes);
        Ok(MasterKey(SymKey(mk)))
    }
}

/// The random 256-bit data key. The server never sees it. It encrypts bundles
/// and is itself only ever stored wrapped in an [`Envelope`].
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct MasterKey(pub(crate) SymKey);

impl MasterKey {
    /// Draw a fresh master key from the OS CSPRNG. Called once, at registration.
    pub fn generate() -> Self {
        let mut k = [0u8; 32];
        OsRng.fill_bytes(&mut k);
        MasterKey(SymKey(k))
    }

    /// The AEAD key for bundle encryption. Crate-internal: a bundle is encrypted
    /// only through [`crate::encrypt_bundle`], never by handing out raw bytes.
    pub(crate) fn key(&self) -> &SymKey {
        &self.0
    }
}

/// A sealed master key. Opaque bytes as far as the server is concerned.
#[derive(Clone, PartialEq, Eq)]
pub struct Envelope(Vec<u8>);

impl Envelope {
    /// The wire/storage form: `version || nonce || ciphertext+tag`.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.clone()
    }

    /// Parse a stored envelope, checking only the framing — not the key.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() != ENVELOPE_TOTAL_LEN || bytes[0] != aead::VERSION {
            return Err(CryptoError::Malformed { what: "envelope" });
        }
        Ok(Envelope(bytes.to_vec()))
    }
}

/// The confirm-before-usable proof.
///
/// `SHA-256(domain || MK)`. Sent to the server at registration; at confirmation
/// the client recovers the master key from the *recovery* envelope and recomputes
/// this. A match proves the recovery code actually recovers the same master key —
/// so recovery is known to work before the account is usable — without ever
/// revealing the master key to the server (SHA-256 is one-way).
pub fn recovery_acknowledgement(mk: &MasterKey) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(ACK_DOMAIN);
    h.update(mk.0.as_bytes());
    h.finalize().into()
}

// --- Recovery code --------------------------------------------------------

/// 128 bits of entropy, shown to the user once and never stored in plaintext by
/// anyone. Crockford base32 for transcription (no I/L/O/U, case-insensitive).
const RECOVERY_ENTROPY_BYTES: usize = 16;
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RecoveryCode([u8; RECOVERY_ENTROPY_BYTES]);

impl RecoveryCode {
    /// Draw a fresh recovery code from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut b = [0u8; RECOVERY_ENTROPY_BYTES];
        OsRng.fill_bytes(&mut b);
        RecoveryCode(b)
    }

    /// The transcribable form, grouped in fives for readability
    /// (`ABCDE-FGHJK-…`).
    pub fn display(&self) -> String {
        let raw = base32_encode(&self.0);
        raw.as_bytes()
            .chunks(5)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Parse a code the user typed back. Hyphens, spaces and case are ignored;
    /// Crockford's I/L→1 and O→0 substitutions are applied so a transcription
    /// slip does not lock the user out.
    pub fn parse(input: &str) -> Result<Self, CryptoError> {
        let bytes = base32_decode(input)?;
        if bytes.len() != RECOVERY_ENTROPY_BYTES {
            return Err(CryptoError::InvalidRecoveryCode);
        }
        let mut b = [0u8; RECOVERY_ENTROPY_BYTES];
        b.copy_from_slice(&bytes);
        Ok(RecoveryCode(b))
    }

    /// The bytes to feed Argon2id when deriving the recovery wrap key.
    pub fn as_secret(&self) -> &[u8] {
        &self.0
    }
}

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for &byte in data {
        buffer = (buffer << 8) | byte as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = (buffer >> bits) & 0x1f;
            out.push(CROCKFORD[idx as usize] as char);
        }
    }
    if bits > 0 {
        let idx = (buffer << (5 - bits)) & 0x1f;
        out.push(CROCKFORD[idx as usize] as char);
    }
    out
}

fn base32_decode(input: &str) -> Result<Vec<u8>, CryptoError> {
    let mut buffer = 0u32;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for ch in input.chars() {
        if ch == '-' || ch == ' ' {
            continue;
        }
        let v = decode_symbol(ch).ok_or(CryptoError::InvalidRecoveryCode)?;
        buffer = (buffer << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Ok(out)
}

fn decode_symbol(ch: char) -> Option<u8> {
    let c = ch.to_ascii_uppercase();
    match c {
        // Crockford's forgiving mappings for common transcription slips.
        'O' => Some(0),
        'I' | 'L' => Some(1),
        _ => CROCKFORD
            .iter()
            .position(|&s| s as char == c)
            .map(|p| p as u8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap_key() -> WrapKey {
        // A raw key for unit tests; production keys come from the KDF.
        WrapKey(SymKey([7u8; 32]))
    }

    #[test]
    fn seal_then_open_recovers_the_master_key() {
        let mk = MasterKey::generate();
        let wk = wrap_key();
        let env = wk.seal(&mk);
        let recovered = wk.open(&env).unwrap();
        assert_eq!(recovered.0.as_bytes(), mk.0.as_bytes());
    }

    #[test]
    fn a_wrong_wrap_key_cannot_open_the_envelope() {
        let mk = MasterKey::generate();
        let env = wrap_key().seal(&mk);
        let wrong = WrapKey(SymKey([8u8; 32]));
        assert!(matches!(wrong.open(&env), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn a_tampered_envelope_is_rejected() {
        let mk = MasterKey::generate();
        let wk = wrap_key();
        let mut bytes = wk.seal(&mk).to_bytes();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01; // flip a ciphertext/tag bit
        let env = Envelope::from_bytes(&bytes).unwrap();
        assert!(matches!(wk.open(&env), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn two_seals_of_the_same_key_differ_because_the_nonce_is_random() {
        let mk = MasterKey::generate();
        let wk = wrap_key();
        assert_ne!(
            wk.seal(&mk).to_bytes(),
            wk.seal(&mk).to_bytes(),
            "a reused nonce would make these equal and leak equality of plaintexts"
        );
    }

    #[test]
    fn a_truncated_envelope_is_malformed_not_a_decrypt_error() {
        let mk = MasterKey::generate();
        let bytes = wrap_key().seal(&mk).to_bytes();
        assert!(matches!(
            Envelope::from_bytes(&bytes[..bytes.len() - 1]),
            Err(CryptoError::Malformed { .. })
        ));
    }

    #[test]
    fn recovery_acknowledgement_matches_only_the_same_master_key() {
        let mk = MasterKey::generate();
        let same = mk.clone();
        let other = MasterKey::generate();
        assert_eq!(
            recovery_acknowledgement(&mk),
            recovery_acknowledgement(&same)
        );
        assert_ne!(
            recovery_acknowledgement(&mk),
            recovery_acknowledgement(&other)
        );
    }

    #[test]
    fn recovery_code_round_trips_through_display_and_parse() {
        let code = RecoveryCode::generate();
        let shown = code.display();
        assert!(shown.contains('-'), "should be grouped for readability");
        let parsed = RecoveryCode::parse(&shown).unwrap();
        assert_eq!(parsed.as_secret(), code.as_secret());
    }

    #[test]
    fn recovery_code_parsing_is_forgiving_of_case_and_spacing() {
        let code = RecoveryCode::generate();
        let shown = code.display().to_lowercase().replace('-', " ");
        assert_eq!(
            RecoveryCode::parse(&shown).unwrap().as_secret(),
            code.as_secret()
        );
    }

    #[test]
    fn a_garbage_recovery_code_is_rejected() {
        // '!' is outside the alphabet.
        assert!(matches!(
            RecoveryCode::parse("!!!!"),
            Err(CryptoError::InvalidRecoveryCode)
        ));
    }
}
