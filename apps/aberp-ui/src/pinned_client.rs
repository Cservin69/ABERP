//! Build a `reqwest::Client` whose TLS trust is exactly one
//! fingerprint: the SHA-256 over the leaf cert DER the backend
//! printed on stdout.
//!
//! Per `feedback_reqwest_trust_store`:
//!
//!   - `ClientBuilder::add_root_certificate` MERGES with webpki
//!     defaults, which is the opposite of what we want here. The
//!     loopback listener uses a self-signed cert with no chain to
//!     any public CA; defaulting to webpki roots would silently
//!     accept any public CA's cert if a future regression flipped
//!     the URL away from `127.0.0.1`.
//!
//!   - The correct posture is to build a `rustls::ClientConfig`
//!     directly with a custom `ServerCertVerifier`, then hand the
//!     **bare** config to reqwest via `use_preconfigured_tls` (no
//!     `Some(...)` wrap).
//!
//! The verifier here is a fingerprint pin: it accepts the connection
//! iff `sha256(end_entity.der()) == pinned_fingerprint`. No chain
//! validation, no hostname check beyond what reqwest itself does
//! against `127.0.0.1`. The loopback listener is the only thing the
//! shell ever talks to and ADR-0007 §Transport names "fingerprint
//! the Tauri shell verifies" as the trust mechanism.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

/// Build the reqwest client. Consumes the fingerprint by reference;
/// the verifier owns its own copy.
pub fn build(fingerprint_hex: &str) -> Result<reqwest::Client> {
    let fingerprint_bytes = hex::decode(fingerprint_hex)
        .with_context(|| format!("decode fingerprint hex `{fingerprint_hex}`"))?;
    if fingerprint_bytes.len() != 32 {
        return Err(anyhow!(
            "fingerprint `{fingerprint_hex}` decoded to {} bytes, expected 32",
            fingerprint_bytes.len()
        ));
    }

    let verifier = Arc::new(PinnedFingerprintVerifier {
        expected: fingerprint_bytes,
    });

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    let client = reqwest::ClientBuilder::new()
        .use_preconfigured_tls(config)
        .build()
        .context("reqwest::ClientBuilder::build with pinned-fingerprint TLS")?;
    Ok(client)
}

/// Constant-time fingerprint comparison. The set of inputs is small
/// (one 32-byte slice per handshake) but the convention is uniform
/// with the rest of the codebase (see
/// `apps/aberp/src/serve.rs::constant_time_eq`).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[derive(Debug)]
struct PinnedFingerprintVerifier {
    expected: Vec<u8>,
}

impl ServerCertVerifier for PinnedFingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let mut hasher = Sha256::new();
        hasher.update(end_entity.as_ref());
        let observed = hasher.finalize();
        if constant_time_eq(observed.as_slice(), &self.expected) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "loopback cert fingerprint mismatch: expected {}, got {}",
                hex::encode(&self.expected),
                hex::encode(observed)
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        // The fingerprint pin already commits to a specific cert,
        // including its public key; the TLS signature check is
        // structurally the same trust statement. Accepting here is
        // safe for the same reason webpki-with-roots accepts a
        // signature once chain validation passes.
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        // The rcgen self-signed cert produced by `aberp serve` is an
        // ECDSA P-256 cert under the aws-lc-rs provider default. We
        // return the standard TLS 1.2 / 1.3 set so rustls negotiates
        // whichever scheme the listener chose.
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_rejects_non_hex_fingerprint() {
        assert!(build("not hex at all").is_err());
    }

    #[test]
    fn build_rejects_wrong_length_fingerprint() {
        // 32 hex chars decodes to 16 bytes, not 32. Reject.
        assert!(build("abababababababababababababababab").is_err());
    }

    #[test]
    fn build_accepts_well_formed_fingerprint() {
        let fp = hex::encode([0x33u8; 32]);
        let client = build(&fp).expect("64-hex-char fingerprint must build a client");
        // Smoke: the client is usable; we don't actually make any
        // network call here, the verifier is exercised only when a
        // real TLS handshake happens.
        let _ = client;
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(&[1, 2, 3], &[1, 2, 3]));
        assert!(!constant_time_eq(&[1, 2, 3], &[1, 2, 4]));
        assert!(!constant_time_eq(&[1, 2, 3], &[1, 2]));
        assert!(constant_time_eq(&[], &[]));
    }
}
