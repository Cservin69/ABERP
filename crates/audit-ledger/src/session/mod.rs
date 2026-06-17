//! S441 / ADR-0086 + ADR-0087 + ADR-0088 — session signing + qualified-
//! timestamp anchoring for the audit chain.
//!
//! This module is the **structural floor** for the DÁP/NETLOCK Path-A
//! design. It lands the trait surfaces, the in-memory Ed25519 session key,
//! the deterministic mock timestamp authority, the `audit_ledger_anchors`
//! table, and the session lifecycle (open / heartbeat / close / service /
//! crash-recovery) operating against a [`crate::Ledger`]. Real DÁP OIDC and
//! NETLOCK RFC-3161 are stubbed (`todo!`) pending RP creds + onboarding.
//!
//! ## The signing preimage (ADR-0087)
//!
//! `event_sig` signs `prev_hash || kind.as_str() || subject || SHA-256(payload)`.
//! It deliberately does NOT fold into `entry_hash` (whose preimage is a
//! fixed-key canonical map — adding a field would break every legacy
//! entry, ADR-0087 §"Why the signature is a separate layer"). The two
//! integrity layers are decoupled; the signature chains to the link
//! structure via the `prev_hash` it covers.

pub mod anchors;
pub mod crypto;
pub mod tsa;

use sha2::{Digest, Sha256};

use crate::entry::{Actor, EntryHash, EventKind};
use crate::error::AppendError;
use crate::session::anchors::{Anchor, AnchorKind};
use crate::session::crypto::{SessionKey, ED25519_SIG_LEN};
use crate::session::tsa::TimestampAuthority;
use crate::storage::Ledger;

pub use crypto::CryptoError;
pub use tsa::{
    MockTimestampAuthority, NetlockTsa, TimestampToken, TsaError, MOCK_TSA_IDENTIFIER,
    NETLOCK_TSA_IDENTIFIER,
};

/// Compute the `event_sig` preimage for an entry.
///
/// `subject` is the deterministic S424 subject (`audit_summary::subject_of`)
/// — passed in by the app layer because that extractor lives in `apps/aberp`,
/// not in this library. Empty string when an entry has no subject.
pub fn event_sig_preimage(
    prev_hash: &EntryHash,
    kind: &EventKind,
    subject: &str,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + 16 + subject.len() + 32);
    out.extend_from_slice(prev_hash.as_bytes());
    out.extend_from_slice(kind.as_str().as_bytes());
    out.extend_from_slice(subject.as_bytes());
    out.extend_from_slice(&Sha256::digest(payload));
    out
}

/// The DÁP-attested operator behind an operator session (ADR-0086). Kept
/// minimal + digital-id-crate-free so this library does not depend on
/// `aberp-digital-id`; the app maps `DapIdentity` → this summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorIdentity {
    /// Stable gov.hu citizen identifier (`operator_dap_subject`).
    pub dap_subject: String,
    /// PID display name (surname + given name).
    pub display_name: String,
    /// RFC3339 UTC instant the identity was attested.
    pub attested_at_utc: String,
    /// `"dap"` for a real DÁP login, `"local_admin_fallback"` for the
    /// code-gated bypass (ADR-0086 §4) — the reduced assurance is visible
    /// in the chain itself.
    pub identity_source: String,
}

/// The service-key endorsement carried by a service session (ADR-0088).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEndorsement {
    /// Hex of the persisted service Ed25519 public key.
    pub service_pubkey_hex: String,
    /// `"pending"` until an operator login endorses it, then `"endorsed"`.
    /// Daemon events fired before any login are timestamp-anchored but
    /// only pending-endorsed (ADR-0088 §"Service-session lifecycle").
    pub endorsement_state: String,
}

/// What rooted a session's trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionIdentity {
    /// A DÁP-attested operator login (ADR-0086).
    Operator(OperatorIdentity),
    /// A long-lived per-tenant service session (ADR-0088).
    Service(ServiceEndorsement),
}

/// Everything the audit-write chokepoint needs to sign an entry under a
/// session: the per-login grouping id, the signing key, and the identity
/// that rooted it.
///
/// Note the `session_id` here is the ADR-0087 *per-login/per-service*
/// grouping — DISTINCT from `Actor.session_id` (a per-process id). The
/// collision is surfaced, not blended (ADR-0087 §"The session_id collision").
pub struct SessionContext {
    pub session_id: String,
    pub identity: SessionIdentity,
    key: SessionKey,
}

impl std::fmt::Debug for SessionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionContext")
            .field("session_id", &self.session_id)
            .field("identity", &self.identity)
            .field("pubkey", &self.key.pubkey_hex())
            .finish()
    }
}

impl SessionContext {
    /// Build a context from an already-minted key (service sessions load
    /// the key from the keychain; operator sessions mint a fresh one — see
    /// [`SessionContext::fresh_operator`]).
    pub fn new(session_id: String, identity: SessionIdentity, key: SessionKey) -> Self {
        Self {
            session_id,
            identity,
            key,
        }
    }

    /// Mint a fresh operator session: new ULID + fresh in-memory key.
    pub fn fresh_operator(identity: OperatorIdentity) -> Result<Self, AppendError> {
        let key = SessionKey::fresh().map_err(|e| AppendError::Crypto(e.to_string()))?;
        Ok(Self::new(
            format!("ses_{}", ulid::Ulid::new()),
            SessionIdentity::Operator(identity),
            key,
        ))
    }

    pub fn pubkey_hex(&self) -> String {
        self.key.pubkey_hex()
    }

    /// Sign an `event_sig` preimage with this session's key.
    pub fn sign(&self, preimage: &[u8]) -> [u8; ED25519_SIG_LEN] {
        self.key.sign(preimage)
    }
}

// ──────────────────────────────────────────────────────────────────────
// Session lifecycle — orchestrates signed appends + anchors on a Ledger.
//
// These are the operations the boot path (operator login, service-session
// startup, heartbeat actor, crash recovery) and the e2e gate drive. Each
// signed event routes through `Ledger::append_signed` (the chokepoint);
// each anchor through `Ledger::take_anchor`.
// ──────────────────────────────────────────────────────────────────────

/// Open an operator session: emit `SessionOpened` (signed) + a `LoginOpen`
/// anchor binding the operator identity to the session key.
pub fn open_operator_session(
    ledger: &mut Ledger,
    tsa: &dyn TimestampAuthority,
    actor: Actor,
    identity: OperatorIdentity,
) -> Result<(SessionContext, Anchor), AppendError> {
    let ctx = SessionContext::fresh_operator(identity.clone())?;
    let payload = serde_json::to_vec(&serde_json::json!({
        "session_id": ctx.session_id,
        "session_pubkey": ctx.pubkey_hex(),
        "operator_dap_subject": identity.dap_subject,
        "identity_source": identity.identity_source,
        "opened_at_utc": tsa::now_rfc3339(),
    }))?;
    ledger.append_signed(
        EventKind::SessionOpened,
        // S424 `subject_of` returns None for session-lifecycle payloads, so
        // the signer's subject must be "" to match the verifier (ADR-0087).
        "",
        payload,
        actor,
        None,
        Some(&ctx),
    )?;
    let anchor = ledger.take_anchor(tsa, &ctx.session_id, AnchorKind::LoginOpen)?;
    Ok((ctx, anchor))
}

/// Open the per-tenant service session (ADR-0088): emit
/// `ServiceSessionOpened` (signed by the service key) + a `ServiceOpen`
/// anchor. Runs at binary startup BEFORE any daemon can fire.
pub fn open_service_session(
    ledger: &mut Ledger,
    tsa: &dyn TimestampAuthority,
    actor: Actor,
    service_key: SessionKey,
) -> Result<(SessionContext, Anchor), AppendError> {
    let endorsement = ServiceEndorsement {
        service_pubkey_hex: service_key.pubkey_hex(),
        endorsement_state: "pending".to_string(),
    };
    let ctx = SessionContext::new(
        format!("svc_{}", ulid::Ulid::new()),
        SessionIdentity::Service(endorsement.clone()),
        service_key,
    );
    let payload = serde_json::to_vec(&serde_json::json!({
        "session_id": ctx.session_id,
        "service_pubkey": endorsement.service_pubkey_hex,
        "opened_at_utc": tsa::now_rfc3339(),
    }))?;
    ledger.append_signed(
        EventKind::ServiceSessionOpened,
        "",
        payload,
        actor,
        None,
        Some(&ctx),
    )?;
    let anchor = ledger.take_anchor(tsa, &ctx.session_id, AnchorKind::ServiceOpen)?;
    Ok((ctx, anchor))
}

/// First operator login endorses the service key (ADR-0088): emit
/// `ServiceSessionEndorsed` (signed by the OPERATOR session) + a
/// `ServiceEndorse` anchor. Links the service pubkey to the DÁP-attested
/// operator, giving daemon events a human root of trust.
pub fn endorse_service_session(
    ledger: &mut Ledger,
    tsa: &dyn TimestampAuthority,
    actor: Actor,
    operator: &SessionContext,
    service_pubkey_hex: &str,
    operator_dap_subject: &str,
) -> Result<Anchor, AppendError> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "service_pubkey": service_pubkey_hex,
        "operator_dap_subject": operator_dap_subject,
        "endorsed_at_utc": tsa::now_rfc3339(),
    }))?;
    ledger.append_signed(
        EventKind::ServiceSessionEndorsed,
        "",
        payload,
        actor,
        None,
        Some(operator),
    )?;
    ledger.take_anchor(tsa, &operator.session_id, AnchorKind::ServiceEndorse)
}

/// Take a heartbeat anchor over the current chain head + emit
/// `TimestampAnchorTaken` (or `TimestampAnchorDelayed` if the TSA was
/// unreachable and the anchor queued pending). Driven by the heartbeat
/// actor every `audit_anchor_heartbeat_seconds`.
pub fn heartbeat(
    ledger: &mut Ledger,
    tsa: &dyn TimestampAuthority,
    actor: Actor,
    session: &SessionContext,
) -> Result<Anchor, AppendError> {
    let anchor = ledger.take_anchor(tsa, &session.session_id, AnchorKind::Heartbeat)?;
    let kind = match anchor.tsa_status {
        anchors::TsaStatus::Anchored => EventKind::TimestampAnchorTaken,
        _ => EventKind::TimestampAnchorDelayed,
    };
    let payload = serde_json::to_vec(&serde_json::json!({
        "session_id": session.session_id,
        "anchor_id": anchor.id,
        "anchor_kind": anchor.kind.as_str(),
        "tsa_status": anchor.tsa_status.as_str(),
        "chain_head_hash": anchor.chain_head_hash_at_anchor,
    }))?;
    ledger.append_signed(kind, "", payload, actor, None, Some(session))?;
    Ok(anchor)
}

/// Close an operator session cleanly: emit `SessionClosed` (signed) + a
/// `LogoutClose` anchor.
pub fn close_operator_session(
    ledger: &mut Ledger,
    tsa: &dyn TimestampAuthority,
    actor: Actor,
    session: &SessionContext,
) -> Result<Anchor, AppendError> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "session_id": session.session_id,
        "closed_at_utc": tsa::now_rfc3339(),
    }))?;
    ledger.append_signed(
        EventKind::SessionClosed,
        "",
        payload,
        actor,
        None,
        Some(session),
    )?;
    ledger.take_anchor(tsa, &session.session_id, AnchorKind::LogoutClose)
}

/// Close the service session at graceful shutdown: emit
/// `ServiceSessionClosed` (signed by the service key) + a `ServiceClose`
/// anchor.
pub fn close_service_session(
    ledger: &mut Ledger,
    tsa: &dyn TimestampAuthority,
    actor: Actor,
    session: &SessionContext,
) -> Result<Anchor, AppendError> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "session_id": session.session_id,
        "closed_at_utc": tsa::now_rfc3339(),
    }))?;
    ledger.append_signed(
        EventKind::ServiceSessionClosed,
        "",
        payload,
        actor,
        None,
        Some(session),
    )?;
    ledger.take_anchor(tsa, &session.session_id, AnchorKind::ServiceClose)
}

/// A recovered orphan session (ADR-0087 §"Crash recovery").
#[derive(Debug, Clone)]
pub struct RecoveredSession {
    pub orphan_session_id: String,
    pub recovery_anchor: Anchor,
}

/// On boot, detect operator/service sessions opened in a previous run with
/// no clean close. For each: emit `SessionCrashRecovered` (signed by the
/// NEW boot session's key) + take a `LogoutClose` anchor over the recovered
/// chain tail to close it. The missing clean logout is an audit-noted
/// irregularity, not a chain break.
pub fn recover_crashed_sessions(
    ledger: &mut Ledger,
    tsa: &dyn TimestampAuthority,
    actor: Actor,
    boot_session: &SessionContext,
) -> Result<Vec<RecoveredSession>, AppendError> {
    let orphans = ledger.open_sessions_without_close()?;
    let mut recovered = Vec::new();
    for orphan_session_id in orphans {
        // The boot session is itself "open with no close" — never recover
        // ourselves (we close cleanly at shutdown).
        if orphan_session_id == boot_session.session_id {
            continue;
        }
        let payload = serde_json::to_vec(&serde_json::json!({
            "orphan_session_id": orphan_session_id,
            "recovered_by_session": boot_session.session_id,
            "recovered_at_utc": tsa::now_rfc3339(),
        }))?;
        ledger.append_signed(
            EventKind::SessionCrashRecovered,
            "",
            payload,
            actor.clone(),
            None,
            Some(boot_session),
        )?;
        // Close the orphan by anchoring a LogoutClose under ITS session_id.
        let recovery_anchor =
            ledger.take_anchor(tsa, &orphan_session_id, AnchorKind::LogoutClose)?;
        recovered.push(RecoveredSession {
            orphan_session_id,
            recovery_anchor,
        });
    }
    Ok(recovered)
}
