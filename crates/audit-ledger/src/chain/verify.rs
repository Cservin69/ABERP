//! Full-chain integrity verification.
//!
//! [`verify_chain`] walks a sequence of [`crate::entry::Entry`] values and
//! checks four invariants:
//!
//! 1. Order — `seq` starts at 1 and advances by 1 contiguously.
//! 2. Chain link — `entry[N].prev_hash == entry[N-1].entry_hash`, or the
//!    tenant genesis hash for N=1.
//! 3. Per-entry integrity — `entry[N].entry_hash` equals the SHA-256 of
//!    the canonical-encoded entry minus `entry_hash` itself.
//! 4. Loud failure — the first divergence identifies the first tampered
//!    or out-of-order entry; the verifier does not continue past it.

use crate::chain::compute::compute_entry_hash;
use crate::chain::genesis::genesis_hash;
use crate::entry::{Entry, TenantId};
use crate::error::VerifyError;

/// Verify a sequence of entries against the per-tenant genesis hash.
///
/// Returns `Ok(count)` on success (number of entries walked) or a
/// [`VerifyError`] describing the first divergence. ADR-0007 §"Fail loud"
/// applies: a tampered chain returns the precise `seq` and reason, not a
/// generic "verification failed".
pub fn verify_chain<'a, I>(tenant: &TenantId, entries: I) -> Result<u64, VerifyError>
where
    I: IntoIterator<Item = &'a Entry>,
{
    let mut expected_seq: u64 = 1;
    let mut prev_hash = genesis_hash(tenant);
    let mut count: u64 = 0;

    for entry in entries {
        // 1. Order check — contiguous from seq=1 upward.
        if entry.seq.as_u64() != expected_seq {
            return Err(VerifyError::OutOfOrder {
                expected: expected_seq,
                found: entry.seq.as_u64(),
            });
        }

        // 2. Chain link check.
        if entry.prev_hash != prev_hash {
            return Err(VerifyError::ChainBroken {
                seq: entry.seq.as_u64(),
            });
        }

        // 3. Per-entry integrity.
        let recomputed = compute_entry_hash(entry);
        if recomputed != entry.entry_hash {
            return Err(VerifyError::TamperedAt {
                seq: entry.seq.as_u64(),
            });
        }

        // 4. Advance.
        prev_hash = entry.entry_hash;
        expected_seq = expected_seq
            .checked_add(1)
            .expect("audit-ledger sequence overflow during verify");
        count += 1;
    }

    Ok(count)
}

/// S441 / ADR-0087 — verdict of the EXTENDED chain verification (base hash
/// chain + per-entry signatures + qualified-timestamp anchors). Maps to
/// ADR-0087's three reported states: *chain intact + fully anchored* /
/// *chain intact + anchors pending* / (hard failure via `Err`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainVerdict {
    /// Entries whose hash chain verified (the base [`verify_chain`] count).
    pub entries_verified: u64,
    /// Entries carrying a valid `event_sig`.
    pub signatures_verified: u64,
    /// Anchors whose qualified-timestamp token verified.
    pub anchors_anchored: u64,
    /// Anchors queued pending (TSA outage) or whose authority cannot be
    /// verified in this build (e.g. a real NETLOCK token, stubbed here) —
    /// a flagged non-failure state, not a tamper.
    pub anchors_pending: u64,
    /// `true` iff the chain is intact AND every anchor is fully anchored.
    pub fully_anchored: bool,
}

/// Extended verification (ADR-0087 §"Chain verification extends").
///
/// Runs the base [`verify_chain`] (hash chain + prev-link), then:
/// - **(a) per-entry signatures** — every entry with a non-null
///   `session_id` must carry a valid `event_sig` under its `session_pubkey`
///   over `prev_hash || kind || subject || SHA-256(payload)`. The anti-strip
///   membership rule: an entry whose session is *anchored* but carries no
///   signature is a failure.
/// - **(b) anchor verification** — each anchor's token is verified against
///   the authority returned by `tsa_for(anchor.tsa_identifier)` (mock
///   anchors → `MockTimestampAuthority`; production → NETLOCK). An anchor
///   with no live authority or a pending token is counted pending, not
///   failed.
///
/// `subject_of` reconstructs the same subject the signer used (the app's
/// `audit_summary::subject_of`); for session-lifecycle events it returns
/// `None` (→ `""`), matching the signer.
pub fn verify_chain_signed<'a>(
    tenant: &TenantId,
    entries: &[Entry],
    anchors: &[crate::session::anchors::Anchor],
    subject_of: impl Fn(&Entry) -> Option<String>,
    tsa_for: impl Fn(&str) -> Option<&'a dyn crate::session::tsa::TimestampAuthority>,
) -> Result<ChainVerdict, VerifyError> {
    use crate::session::anchors::{anchor_preimage, TsaStatus};
    use crate::session::crypto::SessionKey;
    use crate::session::event_sig_preimage;
    use crate::session::tsa::TimestampToken;
    use std::collections::HashSet;

    // Base hash chain.
    let entries_verified = verify_chain(tenant, entries.iter())?;

    // (a) signatures + anti-strip membership.
    let anchored_sessions: HashSet<&str> = anchors.iter().map(|a| a.session_id.as_str()).collect();
    let mut signatures_verified = 0u64;
    for entry in entries {
        match (&entry.session_id, &entry.session_pubkey, &entry.event_sig) {
            (Some(_), Some(pubkey_hex), Some(sig_hex)) => {
                let subject = subject_of(entry).unwrap_or_default();
                let preimage =
                    event_sig_preimage(&entry.prev_hash, &entry.kind, &subject, &entry.payload);
                SessionKey::verify_hex(pubkey_hex, &preimage, sig_hex).map_err(|_| {
                    VerifyError::SignatureInvalid {
                        seq: entry.seq.as_u64(),
                    }
                })?;
                signatures_verified += 1;
            }
            (Some(sid), _, _) => {
                // session_id set but no signature: a strip is a failure only
                // if the session is anchored (a real signed session).
                if anchored_sessions.contains(sid.as_str()) {
                    return Err(VerifyError::SignatureMissingInSignedSession {
                        seq: entry.seq.as_u64(),
                    });
                }
            }
            (None, _, _) => { /* legacy / unsigned entry — allowed */ }
        }
    }

    // (b) anchors.
    let mut anchors_anchored = 0u64;
    let mut anchors_pending = 0u64;
    for a in anchors {
        match (a.tsa_status, &a.timestamp_token_bytes) {
            (TsaStatus::Anchored, Some(bytes)) => match tsa_for(&a.tsa_identifier) {
                Some(tsa) => {
                    let preimage = anchor_preimage(
                        a.kind,
                        &a.tenant_id,
                        &a.session_id,
                        &a.chain_head_hash_at_anchor,
                        &a.created_at_utc,
                    );
                    let token = TimestampToken {
                        bytes: bytes.clone(),
                        issued_at_utc: a.created_at_utc.clone(),
                        tsa_identifier: a.tsa_identifier.clone(),
                    };
                    tsa.verify(&preimage, &token)
                        .map_err(|_| VerifyError::AnchorTampered {
                            anchor_id: a.id.clone(),
                        })?;
                    anchors_anchored += 1;
                }
                // No live authority for this identifier (real NETLOCK token
                // in a build without the client) — flagged pending.
                None => anchors_pending += 1,
            },
            // Pending/failed token, or anchored-but-NULL token.
            _ => anchors_pending += 1,
        }
    }

    Ok(ChainVerdict {
        entries_verified,
        signatures_verified,
        anchors_anchored,
        anchors_pending,
        fully_anchored: anchors_pending == 0,
    })
}
