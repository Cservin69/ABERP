//! S354 / PR-42 (U16) — operator accept-on-behalf crypto + validation.
//!
//! ## Why this exists
//!
//! When a customer accepts a quote off-channel (phone / e-mail / in
//! person) instead of clicking the unique DEAL link, there was NO path
//! to mark the quote accepted: the storefront's ADR-0005 typed-ACCEPT
//! scheme deliberately *refuses* `approved` over a plain Bearer (that is
//! the whole point of the customer-owned accept token). The quote sat at
//! `quoted` until the 30-day link expired — an operator dead-end
//! (audit U16, `docs/findings/s346-audit-quote-workflow.md`).
//!
//! The fix (ADR-0072 + the ABERP-site ADR-0005 amendment) adds a SECOND,
//! distinct accept path: ABERP POSTs `status: "operator_accepted"` to the
//! storefront over its Bearer **plus** an HMAC signature over the bound
//! fields. The storefront permits the transition only when both the
//! Bearer AND the HMAC validate — so an operator-accept is provably
//! ABERP-originated and cannot be replayed against any other signed
//! surface.
//!
//! ## The shared secret
//!
//! The HMAC key is the storefront **Bearer** shared secret (the same
//! token ABERP presents on `POST /api/quotes/{id}/priced` and reads from
//! [`crate::storefront_credential::StorefrontCredentialSnapshot::bearer`];
//! on the storefront it is `ABERP_SITE_ADMIN_TOKEN`). ABERP does not
//! possess the storefront's customer-token `QUOTE_STATUS_SIGNING_KEY`, so
//! the Bearer is the only secret shared between the two services — and
//! signing with it is what binds the operator-accept fields to ABERP's
//! identity. Because the Bearer alone already authenticates the request,
//! the HMAC is not an additional *authentication* factor; its job is to
//! (a) bind the semantic fields `{quote_id, channel, accepted_at_ms,
//! operator_user_id}` so they cannot be tampered independently of the
//! token, and (b) gate the otherwise-forbidden `operator_accepted`
//! transition behind an explicit signed proof (see ADR-0072 §Security).
//!
//! Everything here is PURE (no I/O, no clock) so the signature is
//! deterministic and unit-testable with fixed inputs.

use sha2::{Digest, Sha256};

/// Off-channel acceptance medium the operator records. Closed vocab —
/// the storefront mirrors this exact set.
pub const CHANNEL_PHONE: &str = "phone";
pub const CHANNEL_EMAIL: &str = "email";
pub const CHANNEL_IN_PERSON: &str = "in_person";
pub const CHANNEL_OTHER: &str = "other";

/// The closed channel vocabulary, in display order.
pub const ACCEPT_CHANNELS: [&str; 4] = [
    CHANNEL_PHONE,
    CHANNEL_EMAIL,
    CHANNEL_IN_PERSON,
    CHANNEL_OTHER,
];

/// `true` iff `channel` is one of [`ACCEPT_CHANNELS`].
pub fn is_valid_channel(channel: &str) -> bool {
    ACCEPT_CHANNELS.contains(&channel)
}

/// Canonical HMAC message. MUST match the storefront verifier
/// byte-for-byte (ABERP-site `operatorAcceptCanonicalMessage`). The
/// literal `operator_accept` is the domain separator (mirrors
/// ADR-0005's `"status"` / `"accept"` markers) so an operator-accept
/// signature can never be replayed as a status- or accept-token.
///
/// `accepted_at_ms` is rendered as a plain decimal integer (no grouping,
/// no sign for the realistic positive epoch) so both languages format it
/// identically.
pub fn operator_accept_canonical(
    quote_id: &str,
    channel: &str,
    accepted_at_ms: i64,
    operator_user_id: &str,
) -> String {
    format!("{quote_id}|operator_accept|{channel}|{accepted_at_ms}|{operator_user_id}")
}

/// Lowercase-hex HMAC-SHA256 over [`operator_accept_canonical`] keyed by
/// the storefront Bearer secret. The wire field the storefront verifies.
pub fn operator_accept_hmac_hex(
    secret: &[u8],
    quote_id: &str,
    channel: &str,
    accepted_at_ms: i64,
    operator_user_id: &str,
) -> String {
    let msg = operator_accept_canonical(quote_id, channel, accepted_at_ms, operator_user_id);
    hex::encode(hmac_sha256(secret, msg.as_bytes()))
}

/// RFC 2104 HMAC-SHA256. Hand-rolled on `sha2` to avoid pulling the
/// `hmac` crate for one call site (same construction as
/// `aberp-digital-id`'s mock signer, which is `pub(crate)` there).
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const SHA256_BLOCK_SIZE: usize = 64;

    let mut block_key = [0u8; SHA256_BLOCK_SIZE];
    if key.len() > SHA256_BLOCK_SIZE {
        let digest = Sha256::digest(key);
        block_key[..digest.len()].copy_from_slice(&digest);
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; SHA256_BLOCK_SIZE];
    let mut opad = [0x5cu8; SHA256_BLOCK_SIZE];
    for i in 0..SHA256_BLOCK_SIZE {
        ipad[i] ^= block_key[i];
        opad[i] ^= block_key[i];
    }

    let inner = {
        let mut h = Sha256::new();
        h.update(ipad);
        h.update(message);
        h.finalize()
    };
    let outer = {
        let mut h = Sha256::new();
        h.update(opad);
        h.update(inner);
        h.finalize()
    };

    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED_QID: &str = "00000000-0000-0000-0000-000000000001";
    const FIXED_SECRET: &[u8] = b"unit-test-bearer-secret";
    // A FIXED timestamp so the signature is reproducible run-to-run
    // (brief: "HMAC signature stable across runs given same inputs — use
    // a fixed timestamp in tests").
    const FIXED_TS_MS: i64 = 1_780_000_000_000;

    #[test]
    fn s354_channels_are_validated_against_the_closed_vocab() {
        for c in ACCEPT_CHANNELS {
            assert!(is_valid_channel(c), "{c} must be valid");
        }
        for bad in ["", "Phone", "approved", "sms", "in person", "PHONE"] {
            assert!(!is_valid_channel(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn s354_canonical_message_is_domain_separated_and_field_ordered() {
        let m = operator_accept_canonical(FIXED_QID, CHANNEL_PHONE, FIXED_TS_MS, "ervin");
        assert_eq!(
            m,
            format!("{FIXED_QID}|operator_accept|phone|{FIXED_TS_MS}|ervin")
        );
    }

    #[test]
    fn s354_hmac_is_stable_across_runs_for_fixed_inputs() {
        // The same inputs must yield the same hex digest every call —
        // no clock, no randomness leaks in. Two computations agree, and
        // the value is pinned so a future refactor that changes the
        // construction trips this test.
        let a =
            operator_accept_hmac_hex(FIXED_SECRET, FIXED_QID, CHANNEL_PHONE, FIXED_TS_MS, "ervin");
        let b =
            operator_accept_hmac_hex(FIXED_SECRET, FIXED_QID, CHANNEL_PHONE, FIXED_TS_MS, "ervin");
        assert_eq!(a, b, "HMAC must be deterministic");
        assert_eq!(a.len(), 64, "SHA-256 hex is 64 chars");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Pinned vector — recompute only if the canonical format or the
        // construction intentionally changes (which is a wire-breaking
        // change the storefront must mirror).
        assert_eq!(
            a,
            // Shared cross-impl vector — the ABERP-site vitest
            // `operator-accept.spec.ts` asserts the SAME hex for the SAME
            // inputs, proving the two HMAC implementations agree.
            "66c8b4f0b6b44c01b580a6c079464f8b957a56e9ba0e667e074591e541c1a749",
            "pinned HMAC vector drifted — storefront verifier must match"
        );
    }

    #[test]
    fn s354_hmac_changes_when_any_bound_field_changes() {
        let base =
            operator_accept_hmac_hex(FIXED_SECRET, FIXED_QID, CHANNEL_PHONE, FIXED_TS_MS, "ervin");
        // Every bound field must perturb the digest (binding proof).
        assert_ne!(
            base,
            operator_accept_hmac_hex(FIXED_SECRET, FIXED_QID, CHANNEL_EMAIL, FIXED_TS_MS, "ervin"),
            "channel must bind"
        );
        assert_ne!(
            base,
            operator_accept_hmac_hex(
                FIXED_SECRET,
                FIXED_QID,
                CHANNEL_PHONE,
                FIXED_TS_MS + 1,
                "ervin"
            ),
            "timestamp must bind"
        );
        assert_ne!(
            base,
            operator_accept_hmac_hex(FIXED_SECRET, FIXED_QID, CHANNEL_PHONE, FIXED_TS_MS, "anna"),
            "operator must bind"
        );
        assert_ne!(
            base,
            operator_accept_hmac_hex(
                b"other-secret",
                FIXED_QID,
                CHANNEL_PHONE,
                FIXED_TS_MS,
                "ervin"
            ),
            "secret must bind"
        );
    }
}
