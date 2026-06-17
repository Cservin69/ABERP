//! S441 / ADR-0087 — Ed25519 software session key.
//!
//! A [`SessionKey`] is generated in process memory at session open
//! ([`SessionKey::fresh`]), used to sign every audit entry of that
//! session, and **never persisted** for operator sessions (it is
//! zeroized on drop by `ed25519-dalek`'s `ZeroizeOnDrop`). Service
//! sessions (ADR-0088) persist the *seed* in the OS keychain and rebuild
//! the key via [`SessionKey::from_seed`]; that persistence lives in the
//! app layer, not here.
//!
//! Rationale for software Ed25519 (not Secure Enclave): ADR-0087 §"Session
//! key" — the Enclave is P-256-only and needs a code-signing chain ABERP
//! does not yet hold; the session key's trust derives from the
//! login-endorsement bracket + qualified-timestamp anchor, not hardware
//! provenance. Revisit when Defense ships on Linux/Windows.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use zeroize::Zeroize;

/// Length of the Ed25519 secret seed / public key, in bytes.
pub const ED25519_KEY_LEN: usize = 32;
/// Length of an Ed25519 signature, in bytes.
pub const ED25519_SIG_LEN: usize = 64;

/// Failures from the session-key crypto layer.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    /// The OS CSPRNG (`getrandom`) could not produce a fresh seed.
    #[error("failed to read OS CSPRNG for a fresh session key: {0}")]
    Csprng(String),

    /// A stored / transmitted public key was not a valid Ed25519 point.
    #[error("invalid Ed25519 public key bytes")]
    BadPublicKey,

    /// The signature did not verify against the public key + preimage.
    #[error("Ed25519 signature verification failed")]
    BadSignature,

    /// A hex string (pubkey or signature) was malformed or the wrong
    /// length.
    #[error("malformed hex for {what}: {detail}")]
    BadHex { what: &'static str, detail: String },
}

/// An Ed25519 keypair held in process memory for the lifetime of one
/// session. The secret half is zeroized on drop.
pub struct SessionKey {
    signing: SigningKey,
}

impl SessionKey {
    /// Generate a fresh keypair from the OS CSPRNG. The 32-byte seed is
    /// zeroized immediately after the key is built; the secret then lives
    /// only inside `SigningKey` (itself `ZeroizeOnDrop`).
    pub fn fresh() -> Result<Self, CryptoError> {
        let mut seed = [0u8; ED25519_KEY_LEN];
        getrandom::getrandom(&mut seed).map_err(|e| CryptoError::Csprng(e.to_string()))?;
        let signing = SigningKey::from_bytes(&seed);
        seed.zeroize();
        Ok(Self { signing })
    }

    /// Rebuild a key from a persisted 32-byte seed (the ADR-0088 service
    /// key keychain-load path). The caller owns the seed's lifetime; we do
    /// not zeroize the borrowed slice (it is the caller's `Zeroizing`).
    pub fn from_seed(seed: &[u8; ED25519_KEY_LEN]) -> Self {
        Self {
            signing: SigningKey::from_bytes(seed),
        }
    }

    /// The 32-byte public key.
    pub fn pubkey_bytes(&self) -> [u8; ED25519_KEY_LEN] {
        self.signing.verifying_key().to_bytes()
    }

    /// Export the 32-byte secret seed — ONLY for ADR-0088 service-key
    /// keychain persistence (the service session spans process restarts and
    /// has no human to regenerate it). Operator session keys are in-memory
    /// and MUST NOT call this. The caller is responsible for wrapping the
    /// returned bytes in `Zeroizing` and not logging them.
    pub fn secret_seed_bytes(&self) -> [u8; ED25519_KEY_LEN] {
        self.signing.to_bytes()
    }

    /// Hex-encode the public key for storage in the `session_pubkey`
    /// column.
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.pubkey_bytes())
    }

    /// Sign a preimage, returning the raw 64-byte signature.
    pub fn sign(&self, preimage: &[u8]) -> [u8; ED25519_SIG_LEN] {
        self.signing.sign(preimage).to_bytes()
    }

    /// Verify a signature against a public key + preimage. Free function
    /// shape (per the brief) so verification needs no live `SessionKey`,
    /// only the persisted `session_pubkey`.
    pub fn verify(
        pubkey: &[u8; ED25519_KEY_LEN],
        preimage: &[u8],
        sig: &[u8; ED25519_SIG_LEN],
    ) -> Result<(), CryptoError> {
        let vk = VerifyingKey::from_bytes(pubkey).map_err(|_| CryptoError::BadPublicKey)?;
        let signature = Signature::from_bytes(sig);
        vk.verify(preimage, &signature)
            .map_err(|_| CryptoError::BadSignature)
    }

    /// Verify using a hex-encoded pubkey + hex-encoded signature (the form
    /// persisted on `Entry`). Used by the extended chain verifier.
    pub fn verify_hex(pubkey_hex: &str, preimage: &[u8], sig_hex: &str) -> Result<(), CryptoError> {
        let pubkey = hex32(pubkey_hex, "session_pubkey")?;
        let sig = hex64(sig_hex, "event_sig")?;
        Self::verify(&pubkey, preimage, &sig)
    }
}

/// Manual `Debug` — never prints the secret half (CLAUDE.md rule 12: fail
/// loud, but do not leak key material in a log line).
impl std::fmt::Debug for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKey")
            .field("pubkey", &self.pubkey_hex())
            .field("secret", &"<zeroized-on-drop>")
            .finish()
    }
}

fn hex32(s: &str, what: &'static str) -> Result<[u8; 32], CryptoError> {
    let bytes = hex::decode(s).map_err(|e| CryptoError::BadHex {
        what,
        detail: e.to_string(),
    })?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::BadHex {
            what,
            detail: format!("expected 32 bytes, got {}", bytes.len()),
        })
}

fn hex64(s: &str, what: &'static str) -> Result<[u8; 64], CryptoError> {
    let bytes = hex::decode(s).map_err(|e| CryptoError::BadHex {
        what,
        detail: e.to_string(),
    })?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::BadHex {
            what,
            detail: format!("expected 64 bytes, got {}", bytes.len()),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test 14 — SessionKey signs / verifies; tampered payload fails verify.
    #[test]
    fn sign_then_verify_round_trips() {
        let key = SessionKey::fresh().unwrap();
        let msg = b"prev_hash||kind||subject||payload_hash";
        let sig = key.sign(msg);
        SessionKey::verify(&key.pubkey_bytes(), msg, &sig).expect("genuine signature verifies");
    }

    #[test]
    fn tampered_payload_fails_verify() {
        let key = SessionKey::fresh().unwrap();
        let sig = key.sign(b"original");
        let err = SessionKey::verify(&key.pubkey_bytes(), b"tampered", &sig).unwrap_err();
        assert_eq!(err, CryptoError::BadSignature);
    }

    #[test]
    fn foreign_pubkey_fails_verify() {
        let signer = SessionKey::fresh().unwrap();
        let other = SessionKey::fresh().unwrap();
        let sig = signer.sign(b"msg");
        let err = SessionKey::verify(&other.pubkey_bytes(), b"msg", &sig).unwrap_err();
        assert_eq!(err, CryptoError::BadSignature);
    }

    #[test]
    fn from_seed_is_deterministic() {
        let seed = [7u8; 32];
        let a = SessionKey::from_seed(&seed);
        let b = SessionKey::from_seed(&seed);
        assert_eq!(a.pubkey_bytes(), b.pubkey_bytes());
        // Two fresh keys differ (the seed is randomised).
        let f = SessionKey::fresh().unwrap();
        assert_ne!(a.pubkey_bytes(), f.pubkey_bytes());
    }

    #[test]
    fn hex_round_trip_verifies() {
        let key = SessionKey::fresh().unwrap();
        let msg = b"hello";
        let sig_hex = hex::encode(key.sign(msg));
        SessionKey::verify_hex(&key.pubkey_hex(), msg, &sig_hex).expect("hex form verifies");
    }
}
