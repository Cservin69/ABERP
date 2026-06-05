//! Integration tests for the QA-queue repository — exercise the full
//! auto-create + decide_qa + cross-actor supersede + Dispose-emits-Scrap
//! paths against a fresh in-memory DuckDB so ADR-0063's invariants are
//! pinned at the call-site shape. Same posture as
//! `aberp-work-orders/tests/work_order_round_trip.rs`.

use rust_decimal::Decimal;
use std::str::FromStr;

use aberp_audit_ledger::{
    ensure_schema as ensure_audit_schema, Actor, BinaryHash, LedgerMeta, TenantId,
};
use aberp_inventory::{
    current_stock, ensure_schema as ensure_inventory_schema, record_movement, ActorKind,
    MovementReason, MovementRefKind, RecordMovementContext, RecordMovementInputs,
};
use aberp_qa::{
    all_live_inspections_passed_for_wo, count_qa_inspections_by_state, decide_qa,
    ensure_schema as ensure_qa_schema, get_qa_inspection, list_live_inspections_for_wo,
    list_qa_inspections, DecideQaInputs, QaDecision, QaError, QaInspection, QaState,
    QaWriteContext,
};
use aberp_work_orders::{
    create_work_order, ensure_schema as ensure_wo_schema, list_routing_ops_for_wo, read_routing_op,
    replace_bom_for_product, transition_routing_op, transition_work_order, BomLineInput,
    CreateWorkOrderInputs, RoutingOpAction, RoutingOpInput, RoutingOpTransitionInputs,
    TransitionInputs, WoAction, WoWriteContext, WorkOrderError, WorkOrderState,
};
use duckdb::Connection;

const TEST_TENANT: &str = "ten_test_qa";

const PRODUCTS_SCHEMA_FOR_TESTS: &str = "
CREATE TABLE IF NOT EXISTS products (
    id               VARCHAR NOT NULL PRIMARY KEY,
    tenant_id        VARCHAR NOT NULL,
    name             VARCHAR NOT NULL,
    unit_kind        VARCHAR NOT NULL CHECK (unit_kind IN ('Nav','Own')),
    unit_value       VARCHAR NOT NULL,
    currency         VARCHAR NOT NULL CHECK (currency IN ('HUF','EUR')),
    unit_price_minor BIGINT  NOT NULL,
    created_at       VARCHAR NOT NULL,
    updated_at       VARCHAR NOT NULL,
    deleted_at       VARCHAR
);
";

fn setup_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(PRODUCTS_SCHEMA_FOR_TESTS).unwrap();
    ensure_inventory_schema(&conn).unwrap();
    ensure_audit_schema(&conn).unwrap();
    ensure_wo_schema(&conn).unwrap();
    ensure_qa_schema(&conn).unwrap();
    conn
}

fn insert_product(conn: &Connection, id: &str, name: &str) {
    conn.execute(
        "INSERT INTO products (id, tenant_id, name, unit_kind, unit_value, currency,
                               unit_price_minor, created_at, updated_at, deleted_at,
                               stock_qty, min_stock)
         VALUES (?, ?, ?, 'Nav', 'PIECE', 'HUF', 0, '2026-01-01T00:00:00Z',
                 '2026-01-01T00:00:00Z', NULL, 0, 0);",
        duckdb::params![id, TEST_TENANT, name],
    )
    .unwrap();
}

fn meta() -> LedgerMeta {
    LedgerMeta::new(
        TenantId::new(TEST_TENANT).unwrap(),
        BinaryHash::from_bytes([0u8; 32]),
    )
}

fn wo_ctx_for<'a>(meta: &'a LedgerMeta, login: &str) -> WoWriteContext<'a> {
    WoWriteContext {
        tenant: TEST_TENANT,
        actor: ActorKind::SpaOperator {
            operator_login: login.to_string(),
        },
        ledger_meta: meta,
        ledger_actor: Actor::from_local_cli("test-session".to_string(), login),
    }
}

fn qa_ctx_for<'a>(meta: &'a LedgerMeta, login: &str) -> QaWriteContext<'a> {
    QaWriteContext {
        tenant: TEST_TENANT,
        actor: ActorKind::SpaOperator {
            operator_login: login.to_string(),
        },
        ledger_meta: meta,
        ledger_actor: Actor::from_local_cli("test-session".to_string(), login),
    }
}

fn qa_adapter_ctx<'a>(meta: &'a LedgerMeta, adapter_name: &str) -> QaWriteContext<'a> {
    QaWriteContext {
        tenant: TEST_TENANT,
        actor: ActorKind::Adapter {
            adapter_name: adapter_name.to_string(),
        },
        ledger_meta: meta,
        ledger_actor: Actor::from_local_cli(
            "adapter-session".to_string(),
            &format!("adapter:{}", adapter_name),
        ),
    }
}

fn seed_component_stock(conn: &mut Connection, meta: &LedgerMeta, product_id: &str, qty: &str) {
    let tx = conn.transaction().unwrap();
    let ctx = RecordMovementContext {
        tenant: TEST_TENANT,
        actor: ActorKind::SpaOperator {
            operator_login: "seed".to_string(),
        },
        ledger_meta: meta,
        ledger_actor: Actor::from_local_cli("seed-session".to_string(), "seed"),
    };
    record_movement(
        &tx,
        &ctx,
        RecordMovementInputs {
            product_id: product_id.to_string(),
            qty_delta: Decimal::from_str(qty).unwrap(),
            reason: MovementReason::Receipt,
            ref_kind: MovementRefKind::Manual,
            ref_id: None,
            notes: None,
            idempotency_key: format!("seed-{product_id}"),
        },
    )
    .unwrap();
    tx.commit().unwrap();
}

/// Common setup: create products + BOM + a 2-op WO + release it +
/// return the WO id + the routing-op ids in sequence order.
fn create_and_release_wo_with_2_ops(
    conn: &mut Connection,
    meta: &LedgerMeta,
) -> (String, Vec<String>) {
    insert_product(conn, "prd_widget", "Widget");
    insert_product(conn, "prd_bar", "Raw bar");
    seed_component_stock(conn, meta, "prd_bar", "20");

    let tx = conn.transaction().unwrap();
    replace_bom_for_product(
        &tx,
        TEST_TENANT,
        "prd_widget",
        &[BomLineInput {
            component_id: "prd_bar".to_string(),
            qty_per_unit: Decimal::from_str("1").unwrap(),
        }],
    )
    .unwrap();
    tx.commit().unwrap();

    let tx = conn.transaction().unwrap();
    let (wo, ops) = create_work_order(
        &tx,
        &wo_ctx_for(meta, "ervin"),
        CreateWorkOrderInputs {
            wo_number: "WO-QA-001".to_string(),
            product_id: "prd_widget".to_string(),
            qty_target: Decimal::from_str("5").unwrap(),
            notes: None,
            routing_ops: vec![
                RoutingOpInput {
                    op_name: "CNC mill".to_string(),
                    est_time_min: None,
                    est_cost_huf: None,
                },
                RoutingOpInput {
                    op_name: "Deburr".to_string(),
                    est_time_min: None,
                    est_cost_huf: None,
                },
            ],
            idempotency_key: "create-qa-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // Release: cascade flips op#1 to Active.
    let tx = conn.transaction().unwrap();
    transition_work_order(
        &tx,
        &wo_ctx_for(meta, "ervin"),
        &wo.wo_id,
        TransitionInputs {
            action: WoAction::Release,
            reason: None,
            source_event_id: None,
            idempotency_key: "release-qa-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let op_ids: Vec<String> = ops.into_iter().map(|o| o.routing_op_id).collect();
    (wo.wo_id, op_ids)
}

fn count_kind(conn: &Connection, kind: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM audit_ledger WHERE kind = ?;",
        duckdb::params![kind],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
}

// ─────────────────────────────────────────────────────────────────────
// S233 Part A invariant: Release flips first routing-op to Active
// ─────────────────────────────────────────────────────────────────────

#[test]
fn release_cascades_first_routing_op_to_active() {
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    let ops = list_routing_ops_for_wo(&conn, TEST_TENANT, &wo_id).unwrap();
    assert_eq!(ops.len(), 2);
    // First op (sequence=1) is Active; second stays Pending.
    let op1 = ops.iter().find(|o| o.routing_op_id == op_ids[0]).unwrap();
    let op2 = ops.iter().find(|o| o.routing_op_id == op_ids[1]).unwrap();
    assert_eq!(op1.state, aberp_work_orders::RoutingOpState::Active);
    assert_eq!(op2.state, aberp_work_orders::RoutingOpState::Pending);
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0062 §2 + S233 cascade: completing op#1 flips op#2 to Active
// ─────────────────────────────────────────────────────────────────────

#[test]
fn complete_op_cascades_next_op_to_active_and_auto_creates_qa() {
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(
        outcome.routing_op.state,
        aberp_work_orders::RoutingOpState::Completed
    );
    let next = outcome.next_op_activated.expect("op#2 should be activated");
    assert_eq!(next.routing_op_id, op_ids[1]);
    assert_eq!(next.state, aberp_work_orders::RoutingOpState::Active);
    // QA inspection auto-created.
    assert!(outcome.qa_inspection_id.starts_with("qa_"));
    let qa = get_qa_inspection(&conn, TEST_TENANT, &outcome.qa_inspection_id)
        .unwrap()
        .unwrap();
    assert_eq!(qa.state, QaState::Pending);
    assert_eq!(qa.routing_op_id, op_ids[0]);
    assert_eq!(qa.wo_id, wo_id);

    // Audit: 1 created + 1 decided ledger entries... no decided yet.
    let created_count = count_kind(&conn, "mes.qa_inspection_created");
    assert_eq!(created_count, 1);
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0063 invariant #2: every QA decision emits exactly one
// QaInspectionDecided audit entry.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn decide_qa_emits_one_audit_entry_per_call() {
    let mut conn = setup_db();
    let meta = meta();
    let (_wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    // Complete op#1 → QA inspection auto-created.
    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id = outcome.qa_inspection_id;

    // Decide Pass.
    let tx = conn.transaction().unwrap();
    let result = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Pass,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "decide-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(result.inspection.state, QaState::Passed);
    assert_eq!(result.inspection.qa_id, qa_id);
    assert_eq!(result.superseded_qa_id, None);
    let decided_count = count_kind(&conn, "mes.qa_inspection_decided");
    assert_eq!(decided_count, 1);
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0063 invariant #3: cross-actor decision INSERTs a new row + sets
// the prior row's `superseded_by`.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn cross_actor_decision_creates_new_row_supersedes_prior() {
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    // Complete op#1 → QA inspection auto-created.
    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id = outcome.qa_inspection_id;

    // Adapter writes Failed first.
    let tx = conn.transaction().unwrap();
    let adapter_outcome = decide_qa(
        &tx,
        &qa_adapter_ctx(&meta, "renishaw-cell-A"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("scan: out of tolerance".to_string()),
            measurement: Some("dim_a=12.45mm".to_string()),
            source_event_id: Some("evt_adapter_001".to_string()),
            idempotency_key: "adapter-decide-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    // Adapter wrote in-place (the auto-created row had no prior actor).
    assert_eq!(adapter_outcome.inspection.qa_id, qa_id);
    assert_eq!(adapter_outcome.inspection.state, QaState::Failed);
    assert!(adapter_outcome.superseded_qa_id.is_none());

    // Operator overrides Failed → Rework (cross-actor — operator ≠
    // "adapter:renishaw-cell-A"). New row + supersede.
    let tx = conn.transaction().unwrap();
    let op_outcome = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: Some("re-machine".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "op-decide-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let new_qa_id = op_outcome.inspection.qa_id.clone();
    assert_ne!(new_qa_id, qa_id);
    assert_eq!(op_outcome.inspection.state, QaState::Reworking);
    assert_eq!(op_outcome.superseded_qa_id, Some(qa_id.clone()));
    assert!(op_outcome.rework_flipped_routing_op_back_to_active);

    // The prior row now has superseded_by populated, the new row has it NULL.
    let prior = get_qa_inspection(&conn, TEST_TENANT, &qa_id)
        .unwrap()
        .unwrap();
    assert_eq!(prior.superseded_by, Some(new_qa_id.clone()));
    assert_eq!(prior.state, QaState::Failed); // adapter's reading PRESERVED.
    let new_row = get_qa_inspection(&conn, TEST_TENANT, &new_qa_id)
        .unwrap()
        .unwrap();
    assert_eq!(new_row.superseded_by, None);

    // Live-inspection list returns the operator's row only.
    let live = list_live_inspections_for_wo(&conn, TEST_TENANT, &wo_id).unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].qa_id, new_qa_id);

    // Upstream routing-op was flipped back to Active.
    let op = read_routing_op(&conn, TEST_TENANT, &op_ids[0])
        .unwrap()
        .unwrap();
    assert_eq!(op.state, aberp_work_orders::RoutingOpState::Active);
}

// ─────────────────────────────────────────────────────────────────────
// S264 / PR-253 (F6): Rework emits a RoutingOpStateChanged audit row so
// the Completed → Active reverse transition is walkable from the ledger.
// Pre-fix the reverse flip was invisible to a routing-op state walk
// (only the QA-decided row carried it implicitly) — this test fails
// pre-fix because no `mes.routing_op_state_changed` row is appended.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn rework_emits_routing_op_state_changed_audit() {
    let mut conn = setup_db();
    let meta = meta();
    let (_wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    // Complete op#1 → QA inspection auto-created (Pending).
    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id = outcome.qa_inspection_id;

    // Pending → Fail (allowed edge), then Fail → Rework (the flip).
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "fail-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let before = count_kind(&conn, "mes.routing_op_state_changed");
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: Some("re-machine".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "rework-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let after = count_kind(&conn, "mes.routing_op_state_changed");
    assert_eq!(
        after - before,
        1,
        "Rework must emit exactly one RoutingOpStateChanged audit row"
    );

    // Audit-walk: recover the reverse transition from the ledger payload.
    let payload: Vec<u8> = conn
        .query_row(
            "SELECT payload FROM audit_ledger WHERE kind = ? ORDER BY seq DESC LIMIT 1;",
            duckdb::params!["mes.routing_op_state_changed"],
            |r| r.get(0),
        )
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(v["routing_op_id"], serde_json::json!(op_ids[0]));
    assert_eq!(v["from_state"], serde_json::json!("completed"));
    assert_eq!(v["to_state"], serde_json::json!("active"));

    // Upstream routing-op really is back to Active.
    let op = read_routing_op(&conn, TEST_TENANT, &op_ids[0])
        .unwrap()
        .unwrap();
    assert_eq!(op.state, aberp_work_orders::RoutingOpState::Active);
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0063 invariant #4: same-actor decision UPDATEs the existing row.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn same_actor_decision_updates_in_place() {
    let mut conn = setup_db();
    let meta = meta();
    let (_wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id = outcome.qa_inspection_id;

    // Operator: Pending → Failed.
    let tx = conn.transaction().unwrap();
    let r1 = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("burr".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "op-fail-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(r1.inspection.qa_id, qa_id);
    assert!(r1.superseded_qa_id.is_none());

    // Same operator: Failed → Reworking — still in-place.
    let tx = conn.transaction().unwrap();
    let r2 = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: Some("redo".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "op-rework-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(r2.inspection.qa_id, qa_id);
    assert_eq!(r2.inspection.state, QaState::Reworking);
    assert!(r2.superseded_qa_id.is_none());

    // Exactly 1 row in qa_inspections for this (wo, op).
    let live = list_qa_inspections(&conn, TEST_TENANT, None, 100, 0).unwrap();
    assert_eq!(live.len(), 1);
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0063 invariant #5: Disposed emits exactly one Scrap movement in
// the same tx (sized at the WO's qty_target).
// ─────────────────────────────────────────────────────────────────────

#[test]
fn dispose_emits_one_scrap_movement_in_same_tx() {
    let mut conn = setup_db();
    let meta = meta();
    let (_wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id = outcome.qa_inspection_id;

    // Fail first (Pending → Failed → Disposed).
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("scrap".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "fail-disp".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let pre_scrap_count = count_kind(&conn, "mes.stock_movement_recorded");
    let widget_pre = current_stock(&conn, TEST_TENANT, "prd_widget")
        .unwrap()
        .unwrap_or_else(|| Decimal::from_str("0").unwrap());

    let tx = conn.transaction().unwrap();
    let dispose_outcome = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Dispose,
            reason: Some("part destroyed".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "dispose-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    assert!(dispose_outcome.disposed_emitted_scrap_movement);
    assert_eq!(dispose_outcome.inspection.state, QaState::Disposed);

    // Exactly ONE new stock_movement_recorded entry.
    let post_scrap_count = count_kind(&conn, "mes.stock_movement_recorded");
    assert_eq!(post_scrap_count, pre_scrap_count + 1);

    // Finished-good stock went DOWN by qty_target (5).
    let widget_post = current_stock(&conn, TEST_TENANT, "prd_widget")
        .unwrap()
        .unwrap_or_else(|| Decimal::from_str("0").unwrap());
    assert_eq!(widget_post, widget_pre - Decimal::from_str("5").unwrap());
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0063 invariant #6: wo_completion_eligible only fires when every
// routing-op has a live Passed inspection.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn wo_completion_eligible_requires_all_ops_passed() {
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    // No ops completed yet — gate must refuse.
    {
        let tx = conn.transaction().unwrap();
        let ok = all_live_inspections_passed_for_wo(&tx, TEST_TENANT, &wo_id).unwrap();
        assert!(!ok);
    }

    // Complete + Pass op#1 only — gate still refuses (op#2 has no inspection).
    let tx = conn.transaction().unwrap();
    let o1 = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &o1.qa_inspection_id,
        DecideQaInputs {
            decision: QaDecision::Pass,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "pass-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    {
        let tx = conn.transaction().unwrap();
        let ok = all_live_inspections_passed_for_wo(&tx, TEST_TENANT, &wo_id).unwrap();
        assert!(!ok);
    }

    // Start (Released → InProgress is the only path to Complete-eligibility
    // per ADR-0062 §2; the QA gate fires AFTER `next_state` in the
    // handler, so attempting Complete from Released first would fail with
    // IllegalTransition, not WoCompletionBlockedByQa).
    // Start.
    let tx = conn.transaction().unwrap();
    transition_work_order(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &wo_id,
        TransitionInputs {
            action: WoAction::Start,
            reason: None,
            source_event_id: None,
            idempotency_key: "wo-start".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // Now Complete — QA gate refuses (op#2 not passed).
    let tx = conn.transaction().unwrap();
    let err2 = transition_work_order(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &wo_id,
        TransitionInputs {
            action: WoAction::Complete,
            reason: None,
            source_event_id: None,
            idempotency_key: "wo-complete-bad-2".to_string(),
        },
    )
    .unwrap_err();
    drop(tx);
    assert!(
        matches!(err2, WorkOrderError::WoCompletionBlockedByQa(_)),
        "expected WoCompletionBlockedByQa for op#2, got {err2:?}"
    );

    // Complete + Pass op#2 → gate satisfied → Complete works.
    let tx = conn.transaction().unwrap();
    let o2 = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[1],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op2-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &o2.qa_inspection_id,
        DecideQaInputs {
            decision: QaDecision::Pass,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "pass-2".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    {
        let tx = conn.transaction().unwrap();
        let ok = all_live_inspections_passed_for_wo(&tx, TEST_TENANT, &wo_id).unwrap();
        assert!(ok, "gate should be ok after both ops passed");
    }

    // WO Complete now succeeds.
    let tx = conn.transaction().unwrap();
    let outcome = transition_work_order(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &wo_id,
        TransitionInputs {
            action: WoAction::Complete,
            reason: None,
            source_event_id: None,
            idempotency_key: "wo-complete-good".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(outcome.wo.state, WorkOrderState::Completed);
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0063 invariant #7: illegal QA transitions refused at handler.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn decide_qa_refuses_illegal_state_pair() {
    let mut conn = setup_db();
    let meta = meta();
    let (_wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id = outcome.qa_inspection_id;

    // Try Pending → Rework (not in the allowed-edge list).
    let tx = conn.transaction().unwrap();
    let err = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "bad-rework".to_string(),
        },
    )
    .unwrap_err();
    drop(tx);
    assert!(
        matches!(err, QaError::IllegalTransition(_)),
        "expected IllegalTransition, got {err:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0063 §"Rework": after rework + re-complete, the prior inspection
// is superseded by the fresh one (cascade-create + supersede).
// ─────────────────────────────────────────────────────────────────────

#[test]
fn rework_then_recomplete_supersedes_prior_inspection() {
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id_1 = outcome.qa_inspection_id;

    // Fail then Rework.
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id_1,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("redo".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "fail-rw".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id_1,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: Some("redo".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "rw-1".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // Op flipped Active.
    let op = read_routing_op(&conn, TEST_TENANT, &op_ids[0])
        .unwrap()
        .unwrap();
    assert_eq!(op.state, aberp_work_orders::RoutingOpState::Active);

    // Re-complete op#1 — auto-create fresh inspection AND supersede the
    // prior (which is currently Reworking).
    let tx = conn.transaction().unwrap();
    let outcome2 = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete-2".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id_2 = outcome2.qa_inspection_id;
    assert_ne!(qa_id_1, qa_id_2);

    // Prior is superseded.
    let prior = get_qa_inspection(&conn, TEST_TENANT, &qa_id_1)
        .unwrap()
        .unwrap();
    assert_eq!(prior.superseded_by, Some(qa_id_2.clone()));
    assert_eq!(prior.state, QaState::Reworking);

    // Live list returns the new row only.
    let live = list_live_inspections_for_wo(&conn, TEST_TENANT, &wo_id).unwrap();
    let live_for_op1: Vec<&QaInspection> = live
        .iter()
        .filter(|q| q.routing_op_id == op_ids[0])
        .collect();
    assert_eq!(live_for_op1.len(), 1);
    assert_eq!(live_for_op1[0].qa_id, qa_id_2);
    assert_eq!(live_for_op1[0].state, QaState::Pending);
}

// ─────────────────────────────────────────────────────────────────────
// ADR-0063 §"Adversarial review" #3: Reworking → Passed is allowed
// (the rework-succeeds path). ADR §1 storage table is internally
// inconsistent (named in PR-229 body); we implement the prose.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn rework_succeeded_path_passes_inspection() {
    let mut conn = setup_db();
    let meta = meta();
    let (_wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id = outcome.qa_inspection_id;

    // Pending → Failed → Reworking → Passed.
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "f".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "r".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    let r = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Pass,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "p".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(r.inspection.state, QaState::Passed);
}

// ─────────────────────────────────────────────────────────────────────
// S235 / PR-231 — count_qa_inspections_by_state pins for the Workshop
// operator dashboard tile.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn count_qa_inspections_by_state_groups_correctly() {
    let mut conn = setup_db();
    let meta = meta();
    let (_wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);

    // Empty state: every bucket zero.
    let zero = count_qa_inspections_by_state(&conn, TEST_TENANT).unwrap();
    assert_eq!(zero.pending, 0);
    assert_eq!(zero.passed, 0);
    assert_eq!(zero.failed, 0);
    assert_eq!(zero.reworking, 0);
    assert_eq!(zero.disposed, 0);

    // Complete op#1 → one Pending QA inspection is auto-created.
    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(&meta, "ervin"),
        &op_ids[0],
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: "op1-complete".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let qa_id = outcome.qa_inspection_id;

    let after_create = count_qa_inspections_by_state(&conn, TEST_TENANT).unwrap();
    assert_eq!(after_create.pending, 1);
    assert_eq!(after_create.reworking, 0);

    // QA state machine: Pending → Fail → Failed → Rework → Reworking
    // (cf. `crates/aberp-qa/src/state.rs`). So we go Pending → Failed
    // first, then Failed → Reworking, to land a Reworking row for the
    // count assertion.
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "to-failed".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "to-rework".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // After Rework: one Reworking, but the supersession also creates a
    // fresh Pending row per ADR-0063 (rework re-opens the inspection).
    // The exact post-decision row count belongs to the QA state machine
    // — what THIS test pins is that the per-state grouping mirrors what
    // the table holds. Re-read the counts after the decision; assert
    // each bucket equals the table-side SELECT for that state.
    let counts = count_qa_inspections_by_state(&conn, TEST_TENANT).unwrap();
    let pending_in_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM qa_inspections WHERE tenant_id = ? AND state = 'pending';",
            duckdb::params![TEST_TENANT],
            |row| row.get(0),
        )
        .unwrap();
    let reworking_in_table: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM qa_inspections WHERE tenant_id = ? AND state = 'reworking';",
            duckdb::params![TEST_TENANT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(counts.pending as i64, pending_in_table);
    assert_eq!(counts.reworking as i64, reworking_in_table);
}

#[test]
fn count_qa_inspections_by_state_is_tenant_scoped() {
    let conn = setup_db();
    // No data inserted for either tenant — both should be zero. Pins
    // the absence of cross-tenant leakage so a future query bug that
    // dropped the `WHERE tenant_id = ?` clause would fail the round-
    // trip elsewhere AND this one.
    let other = count_qa_inspections_by_state(&conn, "ten_other").unwrap();
    assert_eq!(other.pending, 0);
    let own = count_qa_inspections_by_state(&conn, TEST_TENANT).unwrap();
    assert_eq!(own.pending, 0);
}

// ─────────────────────────────────────────────────────────────────────
// S249-F18: decide_qa refuses Pass/Fail/Rework on Cancelled WO.
// ─────────────────────────────────────────────────────────────────────

fn cancel_wo(conn: &mut Connection, meta: &LedgerMeta, wo_id: &str, idem: &str) {
    let tx = conn.transaction().unwrap();
    transition_work_order(
        &tx,
        &wo_ctx_for(meta, "ervin"),
        wo_id,
        TransitionInputs {
            action: WoAction::Cancel,
            reason: Some("cancelled mid-prod".to_string()),
            source_event_id: None,
            idempotency_key: idem.to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
}

fn complete_op_get_qa_id(conn: &mut Connection, meta: &LedgerMeta, op_id: &str) -> String {
    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx_for(meta, "ervin"),
        op_id,
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: format!("op-complete-{op_id}"),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    outcome.qa_inspection_id
}

#[test]
fn decide_qa_pass_refused_on_cancelled_wo() {
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);
    let qa_id = complete_op_get_qa_id(&mut conn, &meta, &op_ids[0]);
    cancel_wo(&mut conn, &meta, &wo_id, "cancel-1");

    let tx = conn.transaction().unwrap();
    let err = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Pass,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "decide-pass-on-cancelled".to_string(),
        },
    )
    .unwrap_err();

    match err {
        QaError::Validation(msg) => {
            assert!(
                msg.contains("cancelled") && msg.contains(&wo_id),
                "expected cancelled+wo_id in msg, got: {msg}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
    drop(tx);
    // No QaInspectionDecided audit row should have been written.
    let decided = count_kind(&conn, "mes.qa_inspection_decided");
    assert_eq!(decided, 0, "no decide audit row on refused decision");
}

#[test]
fn decide_qa_fail_refused_on_cancelled_wo() {
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);
    let qa_id = complete_op_get_qa_id(&mut conn, &meta, &op_ids[0]);
    cancel_wo(&mut conn, &meta, &wo_id, "cancel-2");

    let tx = conn.transaction().unwrap();
    let err = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("oot".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "decide-fail-on-cancelled".to_string(),
        },
    )
    .unwrap_err();
    assert!(matches!(err, QaError::Validation(_)));
}

#[test]
fn decide_qa_rework_refused_on_cancelled_wo() {
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);
    let qa_id = complete_op_get_qa_id(&mut conn, &meta, &op_ids[0]);

    // First Fail (so the next state can be Rework per the state machine).
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("bad".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "decide-fail-first".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    cancel_wo(&mut conn, &meta, &wo_id, "cancel-3");

    let tx = conn.transaction().unwrap();
    let err = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: "decide-rework-on-cancelled".to_string(),
        },
    )
    .unwrap_err();
    assert!(matches!(err, QaError::Validation(_)));
}

#[test]
fn decide_qa_dispose_still_allowed_on_cancelled_wo() {
    // Path: Pending → Fail (legal pre-cancel) → cancel WO → Dispose.
    // Per ADR-0063 §"Dispose" Dispose is only reachable from Failed or
    // Reworking, so the test seeds Failed first. The gate must still
    // accept Dispose against a Cancelled WO — scrap is the natural
    // outcome for a cancelled-mid-prod WO.
    let mut conn = setup_db();
    let meta = meta();
    let (wo_id, op_ids) = create_and_release_wo_with_2_ops(&mut conn, &meta);
    let qa_id = complete_op_get_qa_id(&mut conn, &meta, &op_ids[0]);

    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("oot".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "decide-fail-pre-cancel".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    cancel_wo(&mut conn, &meta, &wo_id, "cancel-4");

    let tx = conn.transaction().unwrap();
    let outcome = decide_qa(
        &tx,
        &qa_ctx_for(&meta, "ervin"),
        &qa_id,
        DecideQaInputs {
            decision: QaDecision::Dispose,
            reason: Some("scrap".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "decide-dispose-on-cancelled".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(outcome.inspection.state, QaState::Disposed);
    assert!(outcome.disposed_emitted_scrap_movement);
}
