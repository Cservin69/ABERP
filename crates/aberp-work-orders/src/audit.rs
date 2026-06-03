//! Audit-ledger payloads for the three Work Order kinds per
//! ADR-0062 §4.
//!
//! - [`WorkOrderCreatedPayload`] → `mes.work_order_created`
//! - [`WorkOrderStateChangedPayload`] → `mes.work_order_state_changed`
//! - [`RoutingOpStateChangedPayload`] → `mes.routing_op_state_changed`
//!
//! All three round-trip through `serde_json`; the closed-vocab enums
//! re-use the `rename_all = "snake_case"` from [`crate::types`].
//!
//! Why three not one — per ADR-0062 §4: the create-vs-transition
//! split mirrors the Stage 1 `InvoiceDraftCreated` vs `InvoiceState*`
//! pattern. The routing-op state changes are a separate kind so
//! future operations-dashboard projections can glob
//! `mes.routing_op_*` without sweeping WO-level events.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::types::{RoutingOpState, WorkOrderState};

/// `mes.work_order_created` payload. ONE entry per WO at create time;
/// carries the full snapshot so the future operations-dashboard
/// projection can build its initial state from a single ledger pass.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkOrderCreatedPayload {
    /// `wo_<ULID>` per ADR-0062 §1.
    pub wo_id: String,
    /// Operator-visible WO number (e.g. `WO-2026-0042`).
    pub wo_number: String,
    /// `prd_<ULID>` — the finished good being produced.
    pub product_id: String,
    /// Quantity of finished units to produce.
    pub qty_target: Decimal,
    /// `rop_<ULID>`s of the routing operations created with the WO,
    /// in sequence order. The future operations-dashboard projection
    /// uses this to wire WO → routing without a re-query.
    pub routing_op_ids: Vec<String>,
    /// Human-readable operator attribution string per ADR-0062 §3 /
    /// the inventory module's [`aberp_inventory::ActorKind::as_operator_string`]
    /// posture.
    pub actor: String,
    /// F8 idempotency key — the client-supplied retry token.
    pub idempotency_key: String,
}

impl WorkOrderCreatedPayload {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("JSON serialization of WorkOrderCreatedPayload cannot fail")
    }
}

/// `mes.work_order_state_changed` payload. ONE entry per state
/// transition. `source_event_id` cross-references an upstream adapter
/// event ULID when the transition was driven by an adapter
/// (e.g. a barcode scan); `None` for SPA-button-driven transitions
/// per ADR-0062 §4 + invariant 7.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkOrderStateChangedPayload {
    pub wo_id: String,
    pub from_state: WorkOrderState,
    pub to_state: WorkOrderState,
    /// Optional operator-supplied reason (used by Cancel / Hold to
    /// record `hold_reason` or a cancel justification).
    pub reason: Option<String>,
    /// Human-readable operator attribution string.
    pub actor: String,
    /// **Load-bearing** per ADR-0062 §4. `Some(ULID)` when an adapter
    /// event drove the transition; `None` for SPA button presses.
    /// The route handler MUST pass an explicit `None` rather than
    /// derive it — a buggy handler that silently omits this would
    /// break the audit story (ADR-0062 §"Adversarial review" #8).
    pub source_event_id: Option<String>,
    /// F8 idempotency key.
    pub idempotency_key: String,
}

impl WorkOrderStateChangedPayload {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self)
            .expect("JSON serialization of WorkOrderStateChangedPayload cannot fail")
    }
}

/// `mes.routing_op_state_changed` payload. ONE entry per per-op
/// transition. Carries the parent `wo_id` so SPA filters by WO can
/// pull these alongside the WO-level events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingOpStateChangedPayload {
    pub routing_op_id: String,
    pub wo_id: String,
    pub from_state: RoutingOpState,
    pub to_state: RoutingOpState,
    pub actor: String,
    pub idempotency_key: String,
}

impl RoutingOpStateChangedPayload {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self)
            .expect("JSON serialization of RoutingOpStateChangedPayload cannot fail")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn sample_created() -> WorkOrderCreatedPayload {
        WorkOrderCreatedPayload {
            wo_id: "wo_01H8WORK1234567890ABCDEFG".to_string(),
            wo_number: "WO-2026-0001".to_string(),
            product_id: "prd_01H8PROD234567890ABCDEFGH".to_string(),
            qty_target: Decimal::from_str("10").unwrap(),
            routing_op_ids: vec!["rop_01H8OP1".to_string(), "rop_01H8OP2".to_string()],
            actor: "ervin".to_string(),
            idempotency_key: "01H8IDEM00000000000000000".to_string(),
        }
    }

    fn sample_state_changed() -> WorkOrderStateChangedPayload {
        WorkOrderStateChangedPayload {
            wo_id: "wo_01H8WORK1234567890ABCDEFG".to_string(),
            from_state: WorkOrderState::Created,
            to_state: WorkOrderState::Released,
            reason: None,
            actor: "ervin".to_string(),
            source_event_id: None,
            idempotency_key: "01H8IDEM00000000000000001".to_string(),
        }
    }

    fn sample_op_changed() -> RoutingOpStateChangedPayload {
        RoutingOpStateChangedPayload {
            routing_op_id: "rop_01H8OP1".to_string(),
            wo_id: "wo_01H8WORK1234567890ABCDEFG".to_string(),
            from_state: RoutingOpState::Pending,
            to_state: RoutingOpState::Active,
            actor: "ervin".to_string(),
            idempotency_key: "01H8IDEM00000000000000002".to_string(),
        }
    }

    #[test]
    fn created_payload_round_trips() {
        let p = sample_created();
        let back: WorkOrderCreatedPayload = serde_json::from_slice(&p.to_bytes()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn state_changed_payload_round_trips() {
        let p = sample_state_changed();
        let back: WorkOrderStateChangedPayload = serde_json::from_slice(&p.to_bytes()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn state_changed_payload_with_source_event_id_round_trips() {
        let mut p = sample_state_changed();
        p.source_event_id = Some("evt_01H8ADAPTER".to_string());
        let back: WorkOrderStateChangedPayload = serde_json::from_slice(&p.to_bytes()).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn op_changed_payload_round_trips() {
        let p = sample_op_changed();
        let back: RoutingOpStateChangedPayload = serde_json::from_slice(&p.to_bytes()).unwrap();
        assert_eq!(back, p);
    }

    /// Per ADR-0062 §"Surfaced conflict 1" the closed-vocab states
    /// MUST serialize as snake_case so SQL `WHERE payload->>'to_state'
    /// = 'in_progress'` queries match byte-for-byte.
    #[test]
    fn payloads_use_snake_case_state_tokens() {
        let mut p = sample_state_changed();
        p.from_state = WorkOrderState::InProgress;
        p.to_state = WorkOrderState::OnHold;
        let v: serde_json::Value = serde_json::from_slice(&p.to_bytes()).unwrap();
        assert_eq!(v["from_state"].as_str(), Some("in_progress"));
        assert_eq!(v["to_state"].as_str(), Some("on_hold"));

        let mut op = sample_op_changed();
        op.to_state = RoutingOpState::Completed;
        let v: serde_json::Value = serde_json::from_slice(&op.to_bytes()).unwrap();
        assert_eq!(v["to_state"].as_str(), Some("completed"));
    }

    /// `source_event_id: None` MUST serialize as a literal JSON null,
    /// not be omitted — the future audit-evidence consumer relies on
    /// the field being present (and `null`) to distinguish "no upstream
    /// adapter" from "field missing because the writer forgot to add
    /// it" per ADR-0062 §"Adversarial review" #8.
    #[test]
    fn source_event_id_none_serializes_as_null_not_omitted() {
        let p = sample_state_changed();
        assert!(p.source_event_id.is_none());
        let v: serde_json::Value = serde_json::from_slice(&p.to_bytes()).unwrap();
        assert!(v["source_event_id"].is_null());
    }
}
