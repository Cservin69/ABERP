//! Audit payloads + write helpers for the quote-intake daemon.
//!
//! S256 / PR-245 reworked the emission policy. The v1 (S210) posture
//! emitted a single conditional `QuoteIntakePollCompleted` only when a
//! cycle saw work — which left the Settings → Quote Intake panel
//! reading "No daemon cycle has emitted an audit entry yet" on a
//! healthy-but-idle daemon. The v2 vocabulary:
//!
//! - [`EventKind::QuoteIntakePollAttempted`] — written EVERY cycle,
//!   regardless of outcome (the per-cycle heartbeat). Reuses the
//!   [`QuoteIntakePollPayload`] shape unchanged.
//! - [`EventKind::QuoteIntakeRowAdded`] — one per quote freshly staged,
//!   carrying the customer's source `quote_id` for end-to-end tracing.
//! - [`EventKind::QuoteIntakePollFailed`] — written when the storefront
//!   HTTP call fails, carrying a STRUCTURED closed-vocab
//!   [`PollFailureReason`] (dashboardable) alongside the heartbeat.
//!
//! `QuoteIntakePollCompleted` (v1) is retained in the EventKind enum for
//! parsing pre-S256 rows but is no longer emitted.

use duckdb::Transaction;
use serde::{Deserialize, Serialize};

use aberp_audit_ledger::{append_in_tx, Actor, EventKind, LedgerMeta};

use crate::error::QuoteIntakeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollTrigger {
    Daemon,
    Manual,
}

impl PollTrigger {
    pub fn as_audit_str(self) -> &'static str {
        match self {
            PollTrigger::Daemon => "daemon",
            PollTrigger::Manual => "manual",
        }
    }
}

/// Closed-vocab classification of a cycle-aborting HTTP failure. Stored
/// as a snake-case string on the [`QuoteIntakePollFailedPayload`] so a
/// future dashboard can `GROUP BY reason` without parsing free text
/// (brief §A.3 + the adversarial-review note on the reason schema).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollFailureReason {
    /// Transport-layer failure (DNS, connect, TLS, timeout).
    Transport,
    /// 401 — bearer rotated/invalid. The daemon PAUSES on this reason
    /// rather than hammering (brief §A.5).
    Unauthorized,
    /// 503 — sister service not ready.
    ServiceUnavailable,
    /// Any other non-2xx HTTP status.
    UnexpectedStatus,
    /// 2xx body that failed to parse as the expected quote envelope.
    Parse,
}

impl PollFailureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PollFailureReason::Transport => "transport",
            PollFailureReason::Unauthorized => "unauthorized",
            PollFailureReason::ServiceUnavailable => "service_unavailable",
            PollFailureReason::UnexpectedStatus => "unexpected_status",
            PollFailureReason::Parse => "parse",
        }
    }

    /// Map a transport/list error onto a structured reason. Returns
    /// `None` for errors that are NOT cycle-aborting (per-row mapping /
    /// storage / config / disabled) — those don't produce a
    /// `QuoteIntakePollFailed` entry.
    pub fn from_error(err: &QuoteIntakeError) -> Option<Self> {
        match err {
            QuoteIntakeError::Transport(_) => Some(PollFailureReason::Transport),
            QuoteIntakeError::Unauthorized => Some(PollFailureReason::Unauthorized),
            QuoteIntakeError::ServiceUnavailable => Some(PollFailureReason::ServiceUnavailable),
            QuoteIntakeError::UnexpectedStatus { .. } => Some(PollFailureReason::UnexpectedStatus),
            QuoteIntakeError::Parse(_) => Some(PollFailureReason::Parse),
            QuoteIntakeError::Storage(_)
            | QuoteIntakeError::Mapping { .. }
            | QuoteIntakeError::Config(_)
            | QuoteIntakeError::Disabled => None,
        }
    }
}

/// Per-cycle heartbeat payload. Unchanged from S210 — reused verbatim
/// for the v2 [`EventKind::QuoteIntakePollAttempted`] kind so the
/// Settings panel reader keeps the same shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuoteIntakePollPayload {
    pub idempotency_key: String,
    pub trigger: String,
    pub fetched_count: u32,
    pub created_count: u32,
    pub skipped_duplicate_count: u32,
    pub writeback_retried_count: u32,
    pub writeback_failed_count: u32,
    pub failed_count: u32,
    /// S256 — number of quotes that failed mapping and were staged as
    /// `error`-state rows this cycle (instead of being silently dropped).
    #[serde(default)]
    pub errored_count: u32,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

impl QuoteIntakePollPayload {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("JSON serialization of audit payload cannot fail")
    }
}

/// One-per-quote arrival payload. Carries the customer's `quote_id`
/// (the storefront source-of-truth reference) so an arrival is
/// traceable end-to-end, plus the minted `invoice_id` and `intake_at`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuoteIntakeRowAddedPayload {
    pub idempotency_key: String,
    pub quote_id: String,
    pub invoice_id: String,
    /// RFC3339 instant the row was staged.
    pub intake_at: String,
}

impl QuoteIntakeRowAddedPayload {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("JSON serialization of audit payload cannot fail")
    }
}

/// Structured failure payload for a cycle-aborting HTTP error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuoteIntakePollFailedPayload {
    pub idempotency_key: String,
    pub trigger: String,
    /// Closed-vocab failure class (snake_case).
    pub reason: String,
    /// HTTP status when the reason is `unexpected_status`; `None`
    /// otherwise.
    #[serde(default)]
    pub status: Option<u16>,
    /// Operator-readable detail (scrubbed of bearer bytes by the
    /// transport layer). Never carries the token.
    #[serde(default)]
    pub detail: Option<String>,
    pub elapsed_ms: u64,
}

impl QuoteIntakePollFailedPayload {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("JSON serialization of audit payload cannot fail")
    }
}

/// The v2 per-cycle heartbeat kind (see module docs). Kept for callers
/// that want the on-disk string without constructing an `EventKind`.
pub fn audit_kind_string() -> &'static str {
    EventKind::QuoteIntakePollAttempted.as_str()
}

/// Write the per-cycle [`EventKind::QuoteIntakePollAttempted`] heartbeat.
/// Always called — no `should_emit` gate — so the Settings panel always
/// has a "last cycle" to show once the daemon has run at least once.
pub fn write_poll_audit_entry(
    tx: &Transaction<'_>,
    meta: &LedgerMeta,
    actor: Actor,
    payload: &QuoteIntakePollPayload,
) -> Result<(), QuoteIntakeError> {
    append_in_tx(
        tx,
        meta,
        EventKind::QuoteIntakePollAttempted,
        payload.to_bytes(),
        actor,
        Some(payload.idempotency_key.clone()),
    )
    .map(|_| ())
    .map_err(|e| QuoteIntakeError::Storage(format!("append QuoteIntakePollAttempted entry: {e}")))
}

/// Write one [`EventKind::QuoteIntakeRowAdded`] entry for a freshly
/// staged quote.
pub fn write_row_added_entry(
    tx: &Transaction<'_>,
    meta: &LedgerMeta,
    actor: Actor,
    payload: &QuoteIntakeRowAddedPayload,
) -> Result<(), QuoteIntakeError> {
    append_in_tx(
        tx,
        meta,
        EventKind::QuoteIntakeRowAdded,
        payload.to_bytes(),
        actor,
        Some(payload.idempotency_key.clone()),
    )
    .map(|_| ())
    .map_err(|e| QuoteIntakeError::Storage(format!("append QuoteIntakeRowAdded entry: {e}")))
}

/// Write one [`EventKind::QuoteIntakePollFailed`] entry for a
/// cycle-aborting HTTP error.
pub fn write_poll_failed_entry(
    tx: &Transaction<'_>,
    meta: &LedgerMeta,
    actor: Actor,
    payload: &QuoteIntakePollFailedPayload,
) -> Result<(), QuoteIntakeError> {
    append_in_tx(
        tx,
        meta,
        EventKind::QuoteIntakePollFailed,
        payload.to_bytes(),
        actor,
        Some(payload.idempotency_key.clone()),
    )
    .map(|_| ())
    .map_err(|e| QuoteIntakeError::Storage(format!("append QuoteIntakePollFailed entry: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(fetched: u32, err: Option<String>) -> QuoteIntakePollPayload {
        QuoteIntakePollPayload {
            idempotency_key: "ulid-xxx".to_string(),
            trigger: "daemon".to_string(),
            fetched_count: fetched,
            created_count: 0,
            skipped_duplicate_count: 0,
            writeback_retried_count: 0,
            writeback_failed_count: 0,
            failed_count: 0,
            errored_count: 0,
            elapsed_ms: 12,
            error: err,
        }
    }

    #[test]
    fn audit_kind_string_matches_attempted_event_kind() {
        assert_eq!(
            audit_kind_string(),
            EventKind::QuoteIntakePollAttempted.as_str()
        );
    }

    #[test]
    fn poll_payload_round_trips_through_bytes() {
        let p = sample(2, None);
        let bytes = p.to_bytes();
        let back: QuoteIntakePollPayload = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn poll_payload_back_compat_without_errored_count() {
        // A pre-S256 on-disk payload has no `errored_count` field; it
        // must still decode (defaulting to 0) so historical rows parse.
        let raw = r#"{"idempotency_key":"k","trigger":"daemon","fetched_count":1,
            "created_count":1,"skipped_duplicate_count":0,"writeback_retried_count":0,
            "writeback_failed_count":0,"failed_count":0,"elapsed_ms":5,"error":null}"#;
        let p: QuoteIntakePollPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(p.errored_count, 0);
        assert_eq!(p.created_count, 1);
    }

    #[test]
    fn row_added_payload_round_trips() {
        let p = QuoteIntakeRowAddedPayload {
            idempotency_key: "k".to_string(),
            quote_id: "4d8a5409-1789-4090-8497-2e5276c46220".to_string(),
            invoice_id: "inv_01ABC".to_string(),
            intake_at: "2026-06-05T10:00:00Z".to_string(),
        };
        let back: QuoteIntakeRowAddedPayload = serde_json::from_slice(&p.to_bytes()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn poll_failed_payload_round_trips() {
        let p = QuoteIntakePollFailedPayload {
            idempotency_key: "k".to_string(),
            trigger: "daemon".to_string(),
            reason: PollFailureReason::Unauthorized.as_str().to_string(),
            status: None,
            detail: Some("re-paste bearer".to_string()),
            elapsed_ms: 9,
        };
        let back: QuoteIntakePollFailedPayload = serde_json::from_slice(&p.to_bytes()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn poll_trigger_audit_strings() {
        assert_eq!(PollTrigger::Daemon.as_audit_str(), "daemon");
        assert_eq!(PollTrigger::Manual.as_audit_str(), "manual");
    }

    #[test]
    fn failure_reason_strings_are_closed_vocab() {
        assert_eq!(PollFailureReason::Transport.as_str(), "transport");
        assert_eq!(PollFailureReason::Unauthorized.as_str(), "unauthorized");
        assert_eq!(
            PollFailureReason::ServiceUnavailable.as_str(),
            "service_unavailable"
        );
        assert_eq!(
            PollFailureReason::UnexpectedStatus.as_str(),
            "unexpected_status"
        );
        assert_eq!(PollFailureReason::Parse.as_str(), "parse");
    }

    #[test]
    fn failure_reason_maps_only_cycle_aborting_errors() {
        assert_eq!(
            PollFailureReason::from_error(&QuoteIntakeError::Unauthorized),
            Some(PollFailureReason::Unauthorized)
        );
        assert_eq!(
            PollFailureReason::from_error(&QuoteIntakeError::Transport("dns".into())),
            Some(PollFailureReason::Transport)
        );
        assert_eq!(
            PollFailureReason::from_error(&QuoteIntakeError::UnexpectedStatus { status: 500 }),
            Some(PollFailureReason::UnexpectedStatus)
        );
        // Per-row / non-cycle-aborting errors do NOT produce a poll-failed entry.
        assert_eq!(
            PollFailureReason::from_error(&QuoteIntakeError::Mapping {
                quote_id: "q".into(),
                message: "no email".into()
            }),
            None
        );
        assert_eq!(
            PollFailureReason::from_error(&QuoteIntakeError::Storage("locked".into())),
            None
        );
    }
}
