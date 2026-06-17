//! S441 / ADR-0087 — qualified-timestamp authority abstraction.
//!
//! The audit chain is anchored by an RFC-3161 qualified timestamp at
//! login / heartbeat / logout (and at crash recovery). This module owns
//! the [`TimestampAuthority`] seam so the real NETLOCK endpoint is a
//! single-impl swap behind the trait (ADR-0087 §"TO-BE-CONFIRMED").
//!
//! Two impls ship:
//! - [`MockTimestampAuthority`] — deterministic HMAC-SHA256 over the
//!   payload with a fixed key. Conceptually RFC-3161-shaped (a
//!   `messageImprint` bound to a token); used in tests and dev builds.
//! - [`NetlockTsa`] — the real RFC-3161 client. Constructible so it
//!   compiles into the binary, but every method is `todo!()` pending
//!   NETLOCK onboarding. The expected request/response shape is documented
//!   inline.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The `tsa_identifier` the [`MockTimestampAuthority`] stamps into every
/// token + anchor row. The chain verifier dispatches on this string to
/// pick the authority that can verify a given anchor.
pub const MOCK_TSA_IDENTIFIER: &str = "mock-tsa";

/// The `tsa_identifier` a real NETLOCK qualified TSA would carry. Pinned
/// here so verify-dispatch and anchor rows agree on the production string
/// before the real client lands.
pub const NETLOCK_TSA_IDENTIFIER: &str = "netlock-qtsa";

/// A qualified-timestamp token over some payload. Same conceptual shape as
/// an RFC-3161 `TimeStampToken`: opaque verifiable bytes + when + who.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampToken {
    /// Opaque token bytes. For the mock this is the HMAC tag; for NETLOCK
    /// it is the DER-encoded RFC-3161 `TimeStampToken`.
    pub bytes: Vec<u8>,
    /// RFC3339 UTC instant the token was issued.
    pub issued_at_utc: String,
    /// Authority identifier — [`MOCK_TSA_IDENTIFIER`] or
    /// [`NETLOCK_TSA_IDENTIFIER`].
    pub tsa_identifier: String,
}

/// Failures from a timestamp authority. A `Network` failure is the
/// "NETLOCK unreachable" case ADR-0087 says must NEVER block an audit
/// write — callers queue the anchor `pending` and retry, they do not
/// propagate this into the append path.
#[derive(Debug, thiserror::Error)]
pub enum TsaError {
    #[error("timestamp authority unreachable: {0}")]
    Network(String),

    #[error("timestamp token failed to verify: {0}")]
    Verification(String),

    #[error("timestamp authority not yet implemented: {0}")]
    NotImplemented(&'static str),
}

/// A source of qualified timestamps over arbitrary payloads.
pub trait TimestampAuthority: Send + Sync {
    /// The identifier this authority stamps into its tokens.
    fn identifier(&self) -> &str;

    /// Timestamp `payload`, returning a verifiable token.
    fn timestamp(&self, payload: &[u8]) -> Result<TimestampToken, TsaError>;

    /// Verify a previously-issued token against the original payload.
    fn verify(&self, payload: &[u8], token: &TimestampToken) -> Result<(), TsaError>;
}

/// Deterministic HMAC-SHA256 timestamp authority for tests + dev builds.
///
/// `timestamp(payload)` returns `HMAC(key, payload)` as the token bytes;
/// the HMAC does NOT cover `issued_at_utc`, so a token over a fixed
/// payload is byte-deterministic (the issued-at wall clock is metadata,
/// not part of the imprint). `verify` recomputes the HMAC and compares in
/// constant time (via `hmac`'s built-in `verify_slice`).
#[derive(Debug, Clone)]
pub struct MockTimestampAuthority {
    key: Vec<u8>,
}

impl MockTimestampAuthority {
    /// A fixed deterministic key. Adequate for the test double; the real
    /// integrity comes from NETLOCK in production.
    pub fn new() -> Self {
        Self {
            key: b"aberp-mock-tsa-fixed-key-v1".to_vec(),
        }
    }

    fn mac(&self, payload: &[u8]) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(payload);
        mac
    }
}

impl Default for MockTimestampAuthority {
    fn default() -> Self {
        Self::new()
    }
}

impl TimestampAuthority for MockTimestampAuthority {
    fn identifier(&self) -> &str {
        MOCK_TSA_IDENTIFIER
    }

    fn timestamp(&self, payload: &[u8]) -> Result<TimestampToken, TsaError> {
        let tag = self.mac(payload).finalize().into_bytes();
        Ok(TimestampToken {
            bytes: tag.to_vec(),
            issued_at_utc: now_rfc3339(),
            tsa_identifier: MOCK_TSA_IDENTIFIER.to_string(),
        })
    }

    fn verify(&self, payload: &[u8], token: &TimestampToken) -> Result<(), TsaError> {
        if token.tsa_identifier != MOCK_TSA_IDENTIFIER {
            return Err(TsaError::Verification(format!(
                "token tsa_identifier {:?} is not the mock authority",
                token.tsa_identifier
            )));
        }
        self.mac(payload)
            .verify_slice(&token.bytes)
            .map_err(|_| TsaError::Verification("HMAC imprint mismatch".to_string()))
    }
}

/// The real NETLOCK RFC-3161 qualified TSA client — STUB.
///
/// Constructible so it links into the binary, but every method panics with
/// a `todo!` until NETLOCK account onboarding completes (ADR-0087
/// §"TO-BE-CONFIRMED": the RFC-3161 endpoint, auth, and qualified-TSA cert
/// chain came from marketing copy, not a developer-portal read).
///
/// ## Expected request shape (RFC 3161 §2.4.1 `TimeStampReq`)
/// `POST <netlock-tsa-url>` with `Content-Type: application/timestamp-query`
/// and a DER-encoded `TimeStampReq`:
/// ```text
/// TimeStampReq ::= SEQUENCE {
///   version        INTEGER { v1(1) },
///   messageImprint MessageImprint,   -- { SHA-256 OID, SHA-256(payload) }
///   reqPolicy      TSAPolicyId OPTIONAL,
///   nonce          INTEGER OPTIONAL,  -- random, echoed back, replay guard
///   certReq        BOOLEAN DEFAULT FALSE  -- TRUE: include the TSA cert chain
/// }
/// ```
/// The `payload` argument is the bytes to anchor (e.g.
/// `operator_dap_subject || tenant || session_id || session_pubkey ||
/// login_at_utc`); the imprint is `SHA-256(payload)`.
///
/// ## Expected response shape (RFC 3161 §2.4.2 `TimeStampResp`)
/// `Content-Type: application/timestamp-reply`, DER `TimeStampResp`:
/// ```text
/// TimeStampResp ::= SEQUENCE {
///   status         PKIStatusInfo,     -- granted(0) / rejection(2)
///   timeStampToken TimeStampToken OPTIONAL  -- a CMS SignedData (the qualified seal)
/// }
/// ```
/// On `granted`, the DER `timeStampToken` becomes
/// [`TimestampToken::bytes`]; `issued_at_utc` is the `genTime` from the
/// `TSTInfo`. `verify` validates the CMS signature against the NETLOCK
/// qualified-TSA cert chain and confirms `messageImprint == SHA-256(payload)`
/// and (if sent) the echoed `nonce`.
#[derive(Debug, Clone)]
pub struct NetlockTsa {
    /// The configured endpoint URL (filled from config at construction so
    /// the stub still records the gap concretely).
    _endpoint: String,
}

impl NetlockTsa {
    /// Construct against a configured endpoint. Does NOT contact NETLOCK
    /// (no network at construction); the `todo!` fires only on use.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            _endpoint: endpoint.into(),
        }
    }
}

impl TimestampAuthority for NetlockTsa {
    fn identifier(&self) -> &str {
        NETLOCK_TSA_IDENTIFIER
    }

    fn timestamp(&self, _payload: &[u8]) -> Result<TimestampToken, TsaError> {
        todo!(
            "real NETLOCK RFC 3161 endpoint — pending account onboarding. \
             Build a DER TimeStampReq {{ version=1, messageImprint=SHA-256(payload), \
             nonce=<random>, certReq=true }}, POST application/timestamp-query to \
             the configured NETLOCK qualified-TSA URL, parse the TimeStampResp, and \
             return its DER timeStampToken as TimestampToken.bytes with genTime as \
             issued_at_utc (ADR-0087)."
        )
    }

    fn verify(&self, _payload: &[u8], _token: &TimestampToken) -> Result<(), TsaError> {
        todo!(
            "real NETLOCK RFC 3161 verification — pending account onboarding. \
             Parse the CMS SignedData timeStampToken, validate its signature against \
             the NETLOCK qualified-TSA certificate chain, and confirm \
             messageImprint == SHA-256(payload) (ADR-0087 §'Chain verification \
             extends' (b))."
        )
    }
}

/// Current UTC instant as RFC3339. Isolated so the anchor + token paths
/// agree on the format.
pub(crate) fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test 15 — Mock TSA round-trip: timestamps payload, verifies; tampered fails.
    #[test]
    fn mock_tsa_round_trips() {
        let tsa = MockTimestampAuthority::new();
        let payload = b"chain-head-hash-bytes";
        let token = tsa.timestamp(payload).unwrap();
        assert_eq!(token.tsa_identifier, MOCK_TSA_IDENTIFIER);
        tsa.verify(payload, &token).expect("genuine token verifies");
    }

    #[test]
    fn mock_tsa_tampered_payload_fails() {
        let tsa = MockTimestampAuthority::new();
        let token = tsa.timestamp(b"original").unwrap();
        let err = tsa.verify(b"tampered", &token).unwrap_err();
        assert!(matches!(err, TsaError::Verification(_)));
    }

    #[test]
    fn mock_tsa_is_deterministic_on_payload() {
        let tsa = MockTimestampAuthority::new();
        let a = tsa.timestamp(b"same").unwrap();
        let b = tsa.timestamp(b"same").unwrap();
        // Token bytes (the imprint) are deterministic; only issued_at may differ.
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    #[should_panic(expected = "pending account onboarding")]
    fn netlock_timestamp_is_todo() {
        let _ = NetlockTsa::new("https://example.invalid/tsa").timestamp(b"x");
    }
}
