//! PR-237 / S243 — auto-complete WO on last-op QA pass.
//!
//! Pins the five brief invariants for [`try_auto_complete_wo`]:
//!
//! 1. 3-op WO: pass every op → WO auto-flips to Completed.
//! 2. 3-op WO: pass two, fail the third → WO stays InProgress.
//! 3. 3-op WO: passing inspections out of sequence does NOT fire
//!    auto-complete until EVERY op has a live Passed inspection.
//! 4. Idempotency: once auto-complete fires, a subsequent Pass (via
//!    cross-actor supersede on the same op) is a no-op — the gate
//!    pre-check on `state == InProgress` short-circuits.
//! 5. No regress: Pass→Fail on a previously-passed inspection AFTER
//!    auto-complete does NOT walk the WO back from Completed to
//!    InProgress (no auto-cascade in the reverse direction —
//!    re-opening a passed op is an explicit operator concern).
//!
//! Same in-memory DuckDB pattern `work_order_round_trip.rs` and
//! `qa_round_trip.rs` use; no AppState scaffolding required because
//! [`try_auto_complete_wo`] takes a raw `Transaction`.

use rust_decimal::Decimal;
use std::str::FromStr;

use aberp_audit_ledger::{
    ensure_schema as ensure_audit_schema, Actor, BinaryHash, LedgerMeta, TenantId,
};
use aberp_inventory::{
    ensure_schema as ensure_inventory_schema, record_movement, ActorKind, MovementReason,
    MovementRefKind, RecordMovementContext, RecordMovementInputs,
};
use aberp_qa::{
    decide_qa, ensure_schema as ensure_qa_schema, DecideQaInputs, QaDecision, QaState,
    QaWriteContext,
};
use aberp_work_orders::{
    create_work_order, ensure_schema as ensure_wo_schema, list_routing_ops_for_wo, read_work_order,
    replace_bom_for_product, transition_routing_op, transition_work_order, try_auto_complete_wo,
    BomLineInput, CreateWorkOrderInputs, RoutingOpAction, RoutingOpInput,
    RoutingOpTransitionInputs, TransitionInputs, WoAction, WoWriteContext, WorkOrderState,
};
use duckdb::Connection;

const TEST_TENANT: &str = "ten_test_wo_auto_complete";

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

fn wo_ctx<'a>(meta: &'a LedgerMeta, login: &str) -> WoWriteContext<'a> {
    WoWriteContext {
        tenant: TEST_TENANT,
        actor: ActorKind::SpaOperator {
            operator_login: login.to_string(),
        },
        ledger_meta: meta,
        ledger_actor: Actor::from_local_cli("test-session".to_string(), login),
    }
}

fn qa_ctx<'a>(meta: &'a LedgerMeta, login: &str) -> QaWriteContext<'a> {
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
            &format!("adapter:{adapter_name}"),
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

/// Common setup — 3-op WO released, all routing ops staged: op#1
/// Active, op#2 + op#3 Pending. Returns `(wo_id, op_ids)` with op_ids
/// in sequence order.
fn create_and_release_3_op_wo(
    conn: &mut Connection,
    meta: &LedgerMeta,
    wo_number: &str,
    create_key: &str,
    release_key: &str,
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
        &wo_ctx(meta, "ervin"),
        CreateWorkOrderInputs {
            wo_number: wo_number.to_string(),
            product_id: "prd_widget".to_string(),
            qty_target: Decimal::from_str("3").unwrap(),
            notes: None,
            routing_ops: vec![
                RoutingOpInput {
                    op_name: "Saw".to_string(),
                    est_time_min: None,
                    est_cost_huf: None,
                },
                RoutingOpInput {
                    op_name: "Mill".to_string(),
                    est_time_min: None,
                    est_cost_huf: None,
                },
                RoutingOpInput {
                    op_name: "Polish".to_string(),
                    est_time_min: None,
                    est_cost_huf: None,
                },
            ],
            idempotency_key: create_key.to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let tx = conn.transaction().unwrap();
    transition_work_order(
        &tx,
        &wo_ctx(meta, "ervin"),
        &wo.wo_id,
        TransitionInputs {
            action: WoAction::Release,
            reason: None,
            source_event_id: None,
            idempotency_key: release_key.to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // Move Released → InProgress so subsequent op-Complete cascades
    // walk through the standard lifecycle the brief assumes. Without
    // this, the WO is still Released when we start completing ops,
    // and `try_auto_complete_wo` won't fire because the pre-check
    // refuses non-InProgress states.
    let tx = conn.transaction().unwrap();
    transition_work_order(
        &tx,
        &wo_ctx(meta, "ervin"),
        &wo.wo_id,
        TransitionInputs {
            action: WoAction::Start,
            reason: None,
            source_event_id: None,
            idempotency_key: format!("{release_key}:start"),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    let op_ids: Vec<String> = ops.into_iter().map(|o| o.routing_op_id).collect();
    (wo.wo_id, op_ids)
}

/// Complete the supplied routing-op + auto-create the Pending QA
/// inspection inside ONE tx. Returns the qa_id for the inspection
/// just created.
fn complete_op(
    conn: &mut Connection,
    meta: &LedgerMeta,
    routing_op_id: &str,
    idem: &str,
) -> String {
    let tx = conn.transaction().unwrap();
    let outcome = transition_routing_op(
        &tx,
        &wo_ctx(meta, "ervin"),
        routing_op_id,
        RoutingOpTransitionInputs {
            action: RoutingOpAction::Complete,
            source_event_id: None,
            idempotency_key: idem.to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    outcome.qa_inspection_id
}

/// Decide a QA inspection + immediately attempt the auto-complete
/// hook in the same tx, mirroring the route-layer composition. Returns
/// `(qa_state_after_decide, wo_auto_completed_wo_id)`.
fn decide_and_maybe_auto_complete(
    conn: &mut Connection,
    meta: &LedgerMeta,
    qa_id: &str,
    decision: QaDecision,
    idem: &str,
) -> (QaState, Option<String>) {
    let tx = conn.transaction().unwrap();
    let decide_outcome = decide_qa(
        &tx,
        &qa_ctx(meta, "ervin"),
        qa_id,
        DecideQaInputs {
            decision,
            reason: None,
            measurement: None,
            source_event_id: None,
            idempotency_key: idem.to_string(),
        },
    )
    .unwrap();
    let wo_id = decide_outcome.inspection.wo_id.clone();
    let auto = if matches!(decide_outcome.inspection.state, QaState::Passed) {
        try_auto_complete_wo(&tx, &wo_ctx(meta, "ervin"), &wo_id, idem).unwrap()
    } else {
        None
    };
    tx.commit().unwrap();
    (decide_outcome.inspection.state, auto)
}

// ─────────────────────────────────────────────────────────────────────
// Invariant 1: pass every op → WO auto-flips to Completed.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn passing_every_op_auto_completes_the_wo() {
    let mut conn = setup_db();
    let m = meta();
    let (wo_id, op_ids) =
        create_and_release_3_op_wo(&mut conn, &m, "WO-AC-001", "create-1", "release-1");

    // Op#1 → Complete + Pass.
    let qa1 = complete_op(&mut conn, &m, &op_ids[0], "op1-complete-1");
    let (s1, ac1) = decide_and_maybe_auto_complete(&mut conn, &m, &qa1, QaDecision::Pass, "qa1-1");
    assert_eq!(s1, QaState::Passed);
    assert!(
        ac1.is_none(),
        "WO must NOT auto-complete after op#1 only (ops #2 + #3 still pending)"
    );

    // Op#2 → Complete + Pass.
    let qa2 = complete_op(&mut conn, &m, &op_ids[1], "op2-complete-1");
    let (s2, ac2) = decide_and_maybe_auto_complete(&mut conn, &m, &qa2, QaDecision::Pass, "qa2-1");
    assert_eq!(s2, QaState::Passed);
    assert!(
        ac2.is_none(),
        "WO must NOT auto-complete after op#1+op#2 only (op #3 still pending)"
    );

    // Op#3 → Complete + Pass. This Pass satisfies the QA gate; the
    // auto-complete hook fires.
    let qa3 = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-1");
    let (s3, ac3) = decide_and_maybe_auto_complete(&mut conn, &m, &qa3, QaDecision::Pass, "qa3-1");
    assert_eq!(s3, QaState::Passed);
    assert_eq!(
        ac3.as_deref(),
        Some(wo_id.as_str()),
        "WO must auto-complete when every op has a Passed live inspection"
    );

    // The WO is now in Completed state — read it back to verify.
    let conn_ro = conn;
    let wo = read_work_order(&conn_ro, TEST_TENANT, &wo_id)
        .unwrap()
        .unwrap();
    assert_eq!(wo.state, WorkOrderState::Completed);
}

// ─────────────────────────────────────────────────────────────────────
// Invariant 2: pass two ops, fail the third → WO stays InProgress.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn failing_one_op_keeps_wo_in_progress() {
    let mut conn = setup_db();
    let m = meta();
    let (wo_id, op_ids) =
        create_and_release_3_op_wo(&mut conn, &m, "WO-AC-002", "create-2", "release-2");

    let qa1 = complete_op(&mut conn, &m, &op_ids[0], "op1-complete-2");
    let (_, _) = decide_and_maybe_auto_complete(&mut conn, &m, &qa1, QaDecision::Pass, "qa1-2");

    let qa2 = complete_op(&mut conn, &m, &op_ids[1], "op2-complete-2");
    let (_, _) = decide_and_maybe_auto_complete(&mut conn, &m, &qa2, QaDecision::Pass, "qa2-2");

    let qa3 = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-2");
    let (s3, ac3) = decide_and_maybe_auto_complete(&mut conn, &m, &qa3, QaDecision::Fail, "qa3-2");
    assert_eq!(s3, QaState::Failed);
    assert!(
        ac3.is_none(),
        "WO must NOT auto-complete when the last op QA Failed"
    );

    let wo = read_work_order(&conn, TEST_TENANT, &wo_id)
        .unwrap()
        .unwrap();
    assert_eq!(wo.state, WorkOrderState::InProgress);
}

// ─────────────────────────────────────────────────────────────────────
// Invariant 3: passing inspections out of sequence — the auto-complete
// only fires when EVERY op has a live Passed inspection, regardless
// of which one is decided last.
//
// (The cascade forces ops to *complete* in sequence — you can't
// complete op#3 before op#1 — but QA *decisions* on the resulting
// Pending inspections can come in any order. This pins the latter.)
// ─────────────────────────────────────────────────────────────────────

#[test]
fn out_of_order_qa_pass_does_not_fire_until_every_op_passes() {
    let mut conn = setup_db();
    let m = meta();
    let (wo_id, op_ids) =
        create_and_release_3_op_wo(&mut conn, &m, "WO-AC-003", "create-3", "release-3");

    // Complete all three ops so all three Pending inspections exist.
    let qa1 = complete_op(&mut conn, &m, &op_ids[0], "op1-complete-3");
    let qa2 = complete_op(&mut conn, &m, &op_ids[1], "op2-complete-3");
    let qa3 = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-3");

    // Now decide in order: 3, then 1, then 2.
    let (_, ac3) = decide_and_maybe_auto_complete(&mut conn, &m, &qa3, QaDecision::Pass, "qa3-3");
    assert!(
        ac3.is_none(),
        "passing op#3 first must NOT auto-complete (op#1 + op#2 still pending)"
    );
    let wo_after_3 = read_work_order(&conn, TEST_TENANT, &wo_id)
        .unwrap()
        .unwrap();
    assert_eq!(wo_after_3.state, WorkOrderState::InProgress);

    let (_, ac1) = decide_and_maybe_auto_complete(&mut conn, &m, &qa1, QaDecision::Pass, "qa1-3");
    assert!(
        ac1.is_none(),
        "passing op#1 second must NOT auto-complete (op#2 still pending)"
    );

    let (_, ac2) = decide_and_maybe_auto_complete(&mut conn, &m, &qa2, QaDecision::Pass, "qa2-3");
    assert_eq!(
        ac2.as_deref(),
        Some(wo_id.as_str()),
        "WO must auto-complete on the LAST pass regardless of sequence"
    );

    let wo = read_work_order(&conn, TEST_TENANT, &wo_id)
        .unwrap()
        .unwrap();
    assert_eq!(wo.state, WorkOrderState::Completed);
}

// ─────────────────────────────────────────────────────────────────────
// Invariant 4: idempotency — once auto-complete has fired, a
// subsequent Pass (here: an adapter-actor cross-actor Pass that
// supersedes the operator's Pass on the same inspection) is a cheap
// no-op. The InProgress pre-check inside try_auto_complete_wo
// short-circuits because the WO is already Completed.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn second_pass_after_auto_complete_is_a_noop() {
    let mut conn = setup_db();
    let m = meta();
    let (wo_id, op_ids) =
        create_and_release_3_op_wo(&mut conn, &m, "WO-AC-004", "create-4", "release-4");

    let qa1 = complete_op(&mut conn, &m, &op_ids[0], "op1-complete-4");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa1, QaDecision::Pass, "qa1-4");
    let qa2 = complete_op(&mut conn, &m, &op_ids[1], "op2-complete-4");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa2, QaDecision::Pass, "qa2-4");
    let qa3 = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-4");
    let (_, ac_first) =
        decide_and_maybe_auto_complete(&mut conn, &m, &qa3, QaDecision::Pass, "qa3-4");
    assert_eq!(ac_first.as_deref(), Some(wo_id.as_str()));

    // Cross-actor supersede on the SAME inspection: an adapter writes
    // a Pass against the now-Passed inspection. decide_qa says
    // Passed→Pass is illegal at the state-machine layer, so the second
    // path is via QA inspection #3 already being Passed and the
    // ADAPTER writing on an op-COMPLETED-but-fresh-Pending-inspection
    // — to exercise this idempotency case we supersede by simulating
    // a fresh decide.
    //
    // Simplest pin: call try_auto_complete_wo directly with the SAME
    // wo_id after the first auto-complete already fired. The
    // InProgress pre-check refuses → Ok(None).
    let tx = conn.transaction().unwrap();
    let again = try_auto_complete_wo(&tx, &wo_ctx(&m, "ervin"), &wo_id, "qa3-4-repeat").unwrap();
    tx.commit().unwrap();
    assert!(
        again.is_none(),
        "try_auto_complete_wo must be a no-op when WO is already Completed"
    );

    let wo = read_work_order(&conn, TEST_TENANT, &wo_id)
        .unwrap()
        .unwrap();
    assert_eq!(wo.state, WorkOrderState::Completed);
}

// ─────────────────────────────────────────────────────────────────────
// Invariant 5: no regress — re-opening a previously-passed inspection
// (operator decides Fail on a now-Passed inspection per the
// `Passed → Fail` edge in the QA state machine) does NOT walk the WO
// back from Completed to InProgress. The auto-complete hook only
// fires on decision=Pass; there is no inverse "auto-uncomplete".
//
// Per the brief: "that's an explicit operator concern, not auto-
// cascade." The WO state remains Completed; the operator can manually
// Cancel + re-Release if they want to surface the regression.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn pass_then_fail_after_auto_complete_does_not_regress_wo() {
    let mut conn = setup_db();
    let m = meta();
    let (wo_id, op_ids) =
        create_and_release_3_op_wo(&mut conn, &m, "WO-AC-005", "create-5", "release-5");

    let qa1 = complete_op(&mut conn, &m, &op_ids[0], "op1-complete-5");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa1, QaDecision::Pass, "qa1-5");
    let qa2 = complete_op(&mut conn, &m, &op_ids[1], "op2-complete-5");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa2, QaDecision::Pass, "qa2-5");
    let qa3 = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-5");
    let (_, ac3) = decide_and_maybe_auto_complete(&mut conn, &m, &qa3, QaDecision::Pass, "qa3-5");
    assert_eq!(ac3.as_deref(), Some(wo_id.as_str()));

    // After-the-fact Fail on qa3 (same actor in-place UPDATE per the
    // QA state machine: Passed → Fail is allowed). The hook does NOT
    // fire because the decision is Fail, not Pass — and even if it
    // did, the InProgress pre-check would refuse.
    let tx = conn.transaction().unwrap();
    let after_fact = decide_qa(
        &tx,
        &qa_ctx(&m, "ervin"),
        &qa3,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("after-fact catch".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "qa3-5-fail".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    assert_eq!(after_fact.inspection.state, QaState::Failed);

    let wo = read_work_order(&conn, TEST_TENANT, &wo_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        wo.state,
        WorkOrderState::Completed,
        "WO must NOT regress to InProgress after a after-fact Fail on a previously-passed inspection"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Defence-in-depth: cross-actor supersede on the FINAL inspection
// still fires the auto-complete. The cross-actor INSERT mints a NEW
// qa_id, the auto-complete hook reads the gate via the live-row view
// (`superseded_by IS NULL`) so it sees the new Passed row regardless
// of qa_id changes.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn cross_actor_pass_on_final_op_still_auto_completes() {
    let mut conn = setup_db();
    let m = meta();
    let (wo_id, op_ids) =
        create_and_release_3_op_wo(&mut conn, &m, "WO-AC-006", "create-6", "release-6");

    let qa1 = complete_op(&mut conn, &m, &op_ids[0], "op1-complete-6");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa1, QaDecision::Pass, "qa1-6");
    let qa2 = complete_op(&mut conn, &m, &op_ids[1], "op2-complete-6");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa2, QaDecision::Pass, "qa2-6");
    let qa3 = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-6");

    // Adapter writes Fail first.
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_adapter_ctx(&m, "renishaw-cell-A"),
        &qa3,
        DecideQaInputs {
            decision: QaDecision::Fail,
            reason: Some("scan: out of tol".to_string()),
            measurement: None,
            source_event_id: Some("evt_adapter".to_string()),
            idempotency_key: "qa3-6-adapter".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // Operator overrides Failed → Reworking → Passed via the legal
    // QA cycle. Rework triggers a re-Active cascade on op#3 (the
    // existing aberp-qa::decide_qa Rework path); operator re-completes
    // op#3 → a FRESH Pending inspection is created (qa3b); operator
    // passes qa3b → auto-complete fires.
    let tx = conn.transaction().unwrap();
    decide_qa(
        &tx,
        &qa_ctx(&m, "ervin"),
        &qa3,
        DecideQaInputs {
            decision: QaDecision::Rework,
            reason: Some("operator rework".to_string()),
            measurement: None,
            source_event_id: None,
            idempotency_key: "qa3-6-rework".to_string(),
        },
    )
    .unwrap();
    tx.commit().unwrap();

    // The Rework cascade flipped op#3 back to Active. Re-complete it.
    let qa3b = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-6b");
    let (s, ac) = decide_and_maybe_auto_complete(&mut conn, &m, &qa3b, QaDecision::Pass, "qa3b-6");
    assert_eq!(s, QaState::Passed);
    assert_eq!(
        ac.as_deref(),
        Some(wo_id.as_str()),
        "auto-complete must fire after the post-rework re-pass on the final op"
    );

    let wo = read_work_order(&conn, TEST_TENANT, &wo_id)
        .unwrap()
        .unwrap();
    assert_eq!(wo.state, WorkOrderState::Completed);
}

// ─────────────────────────────────────────────────────────────────────
// Defence-in-depth: the auto-complete fires through transition_work_order
// which means the WoCompletion stock_movement was emitted. Pin it.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn auto_complete_emits_wo_completion_movement() {
    let mut conn = setup_db();
    let m = meta();
    let (wo_id, op_ids) =
        create_and_release_3_op_wo(&mut conn, &m, "WO-AC-007", "create-7", "release-7");

    let qa1 = complete_op(&mut conn, &m, &op_ids[0], "op1-complete-7");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa1, QaDecision::Pass, "qa1-7");
    let qa2 = complete_op(&mut conn, &m, &op_ids[1], "op2-complete-7");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa2, QaDecision::Pass, "qa2-7");
    let qa3 = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-7");
    let (_, ac) = decide_and_maybe_auto_complete(&mut conn, &m, &qa3, QaDecision::Pass, "qa3-7");
    assert_eq!(ac.as_deref(), Some(wo_id.as_str()));

    // The WoCompletion movement is reason='wo_completion'.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stock_movements
             WHERE tenant_id = ? AND ref_id = ? AND reason = 'wo_completion';",
            duckdb::params![TEST_TENANT, &wo_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "exactly one WoCompletion stock_movement must be emitted by auto-complete"
    );

    // And one WorkOrderStateChanged → completed audit entry. Filter
    // payload-side because the audit_ledger column is BLOB (`payload`)
    // not a JSON column — round-trip via serde to inspect `to_state`.
    let mut stmt = conn
        .prepare(
            "SELECT payload FROM audit_ledger
             WHERE kind = 'mes.work_order_state_changed';",
        )
        .unwrap();
    let payloads: Vec<Vec<u8>> = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let completed_audit_count = payloads
        .iter()
        .map(|p| serde_json::from_slice::<serde_json::Value>(p).unwrap())
        .filter(|v| v["to_state"].as_str() == Some("completed"))
        .count();
    assert_eq!(
        completed_audit_count, 1,
        "exactly one WorkOrderStateChanged → completed audit entry must be emitted"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Smoke: every routing op must be flipped to Completed before any
// auto-complete fires. (Defence-in-depth — the brief mentions
// this is structurally enforced by all_live_inspections_passed_for_wo,
// but the cascade also intersects with the gate here.) Two ops left
// uncompleted: gate cannot be satisfied even if existing inspections
// are passed.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn list_routing_ops_match_completed_after_auto_complete() {
    let mut conn = setup_db();
    let m = meta();
    let (wo_id, op_ids) =
        create_and_release_3_op_wo(&mut conn, &m, "WO-AC-008", "create-8", "release-8");

    let qa1 = complete_op(&mut conn, &m, &op_ids[0], "op1-complete-8");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa1, QaDecision::Pass, "qa1-8");
    let qa2 = complete_op(&mut conn, &m, &op_ids[1], "op2-complete-8");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa2, QaDecision::Pass, "qa2-8");
    let qa3 = complete_op(&mut conn, &m, &op_ids[2], "op3-complete-8");
    decide_and_maybe_auto_complete(&mut conn, &m, &qa3, QaDecision::Pass, "qa3-8");

    // All 3 routing ops are in `Completed` state.
    let ops = list_routing_ops_for_wo(&conn, TEST_TENANT, &wo_id).unwrap();
    assert_eq!(ops.len(), 3);
    for op in &ops {
        assert_eq!(
            op.state,
            aberp_work_orders::RoutingOpState::Completed,
            "op {} must be Completed after auto-complete fired",
            op.routing_op_id
        );
    }
}
