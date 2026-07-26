//! ADR-0105 — BOM revision history integration tests.
//!
//! Pins the four properties the feature exists for:
//!
//! 1. **Retention + identity** — N edits yield N retained revisions,
//!    each with a monotonic number, an author, a reason, and its
//!    ORIGINAL lines still readable.
//! 2. **Diff** — added / removed / changed between two revisions is
//!    correct, and refuses loud rather than under-reporting.
//! 3. **The traceability link** — a released WO pins the revision it
//!    was built against, the pin is reproducible, and no later
//!    transition can overwrite it.
//! 4. **Attribution rides the audit chain** — one
//!    `mes.bom_revision_created` entry per revision, carrying the full
//!    line snapshot, written in the SAME tx as the business rows.
//!
//! Same in-memory DuckDB harness as `work_order_round_trip.rs`.

use rust_decimal::Decimal;
use std::str::FromStr;

use aberp_audit_ledger::{
    ensure_schema as ensure_audit_schema, Actor, BinaryHash, LedgerMeta, TenantId,
};
use aberp_inventory::{
    ensure_schema as ensure_inventory_schema, record_movement, ActorKind, MovementReason,
    MovementRefKind, RecordMovementContext, RecordMovementInputs,
};
use aberp_work_orders::{
    create_work_order, diff_bom_revisions, ensure_schema as ensure_wo_schema,
    list_active_bom_for_product, list_bom_lines_for_revision, list_bom_revisions, read_work_order,
    replace_bom_for_product, transition_work_order, BomLineInput, BomRevisionCreatedPayload,
    CreateWorkOrderInputs, RoutingOpInput, TransitionInputs, WoAction, WoWriteContext,
    WorkOrderError, WorkOrderState,
};
use duckdb::Connection;

const TEST_TENANT: &str = "ten_test_bom_revisions";

const PRODUCTS_SCHEMA_FOR_TESTS: &str = "
CREATE TABLE IF NOT EXISTS products (
    id               VARCHAR NOT NULL PRIMARY KEY,
    tenant_id        VARCHAR NOT NULL,
    name             VARCHAR NOT NULL,
    unit_kind        VARCHAR NOT NULL,
    unit_value       VARCHAR NOT NULL,
    currency         VARCHAR NOT NULL,
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
    aberp_qa::ensure_schema(&conn).unwrap();
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

fn ctx_for<'a>(meta: &'a LedgerMeta, login: &str) -> WoWriteContext<'a> {
    WoWriteContext {
        tenant: TEST_TENANT,
        actor: ActorKind::SpaOperator {
            operator_login: login.to_string(),
        },
        ledger_meta: meta,
        ledger_actor: Actor::from_local_cli("test-session".to_string(), login),
    }
}

fn line(component_id: &str, qty: &str) -> BomLineInput {
    BomLineInput {
        component_id: component_id.to_string(),
        qty_per_unit: Decimal::from_str(qty).unwrap(),
    }
}

/// Author one revision in its own committed tx.
fn author(
    conn: &mut Connection,
    meta: &LedgerMeta,
    login: &str,
    product_id: &str,
    lines: &[BomLineInput],
    reason: Option<&str>,
) -> aberp_work_orders::BomRevisionOutcome {
    let tx = conn.transaction().unwrap();
    let out =
        replace_bom_for_product(&tx, &ctx_for(meta, login), product_id, lines, reason).unwrap();
    tx.commit().unwrap();
    out
}

fn seed_stock(conn: &mut Connection, meta: &LedgerMeta, product_id: &str, qty: &str) {
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

fn create_wo(conn: &mut Connection, meta: &LedgerMeta, wo_number: &str, qty: &str) -> String {
    let tx = conn.transaction().unwrap();
    let (wo, _ops) = create_work_order(
        &tx,
        &ctx_for(meta, "ervin"),
        CreateWorkOrderInputs {
            wo_number: wo_number.to_string(),
            product_id: "prd_widget".to_string(),
            qty_target: Decimal::from_str(qty).unwrap(),
            notes: None,
            routing_ops: vec![RoutingOpInput {
                op_name: "Turn".to_string(),
                est_time_min: Some(10),
                est_cost_huf: None,
            }],
            idempotency_key: format!("create-{wo_number}"),
            source_quote_id: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    wo.wo_id
}

fn release(
    conn: &mut Connection,
    meta: &LedgerMeta,
    wo_id: &str,
    key: &str,
) -> aberp_work_orders::WorkOrderTransitionOutcome {
    let tx = conn.transaction().unwrap();
    let out = transition_work_order(
        &tx,
        &ctx_for(meta, "ervin"),
        wo_id,
        TransitionInputs {
            action: WoAction::Release,
            reason: None,
            source_event_id: None,
            idempotency_key: key.to_string(),
            actual_machining_minutes: None,
        },
    )
    .unwrap();
    tx.commit().unwrap();
    out
}

// ─────────────────────────────────────────────────────────────────────
// 1. Retention + identity — ADR-0105 §2.2
// ─────────────────────────────────────────────────────────────────────

/// The headline property: a BOM edited N times yields N retained
/// revisions, numbered 1..N, each attributable, and each prior
/// revision's ORIGINAL lines still readable in full.
#[test]
fn three_edits_yield_three_retained_attributable_revisions() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    insert_product(&conn, "prd_bolt", "Bolt");
    insert_product(&conn, "prd_washer", "Washer");
    let meta = meta();

    let r1 = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2")],
        Some("initial release"),
    );
    let r2 = author(
        &mut conn,
        &meta,
        "kata",
        "prd_widget",
        &[line("prd_bar", "3"), line("prd_bolt", "4")],
        Some("added fastener; heavier bar"),
    );
    let r3 = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bolt", "4"), line("prd_washer", "4")],
        None,
    );

    // Three revisions, numbered monotonically, newest first.
    let revs = list_bom_revisions(&conn, TEST_TENANT, "prd_widget").unwrap();
    assert_eq!(revs.len(), 3, "expected 3 retained revisions");
    assert_eq!(
        revs.iter().map(|r| r.rev_number).collect::<Vec<_>>(),
        vec![3, 2, 1],
        "list must be newest-first with monotonic 1-based numbers"
    );

    // Each is attributable: author + timestamp always, reason when given.
    assert_eq!(revs[2].author, "ervin");
    assert_eq!(revs[2].reason.as_deref(), Some("initial release"));
    assert_eq!(revs[1].author, "kata");
    assert_eq!(
        revs[1].reason.as_deref(),
        Some("added fastener; heavier bar")
    );
    assert_eq!(revs[0].author, "ervin");
    assert_eq!(revs[0].reason, None, "no reason given must stay None");
    assert!(revs.iter().all(|r| !r.created_at.is_empty()));

    // Line counts recorded on the header match the authored sets.
    assert_eq!(
        revs.iter().map(|r| r.line_count).collect::<Vec<_>>(),
        vec![2, 2, 1]
    );

    // PRIOR VERSIONS INTACT — revision 1 still reads back exactly as
    // authored, even though it was superseded twice.
    let rev1_lines =
        list_bom_lines_for_revision(&conn, TEST_TENANT, &r1.revision.bom_rev_id).unwrap();
    assert_eq!(rev1_lines.len(), 1);
    assert_eq!(rev1_lines[0].component_id, "prd_bar");
    assert_eq!(
        rev1_lines[0].qty_per_unit,
        Decimal::from_str("2").unwrap(),
        "revision 1 must still hold its ORIGINAL qty, not the later 3"
    );
    assert!(
        rev1_lines[0].retired_at.is_some(),
        "a superseded line is retired, not deleted"
    );

    let rev2_lines =
        list_bom_lines_for_revision(&conn, TEST_TENANT, &r2.revision.bom_rev_id).unwrap();
    assert_eq!(rev2_lines.len(), 2);
    assert_eq!(
        rev2_lines
            .iter()
            .map(|l| l.component_id.as_str())
            .collect::<Vec<_>>(),
        vec!["prd_bar", "prd_bolt"]
    );

    // Only the newest revision is ACTIVE.
    let active = list_active_bom_for_product(&conn, TEST_TENANT, "prd_widget").unwrap();
    assert_eq!(active.len(), 2);
    assert!(
        active
            .iter()
            .all(|l| l.bom_rev_id.as_deref() == Some(r3.revision.bom_rev_id.as_str())),
        "every active line must belong to the newest revision"
    );

    // Nothing was DELETEd: 1 + 2 + 2 = 5 rows retained.
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM boms WHERE tenant_id = ? AND product_id = ?;",
            duckdb::params![TEST_TENANT, "prd_widget"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total, 5, "all five authored lines must be retained");
}

/// The content hash is a function of the component set only — same set
/// in a different line ORDER hashes identically; a changed quantity
/// does not.
#[test]
fn content_hash_tracks_material_content_not_line_order() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    insert_product(&conn, "prd_bolt", "Bolt");
    let meta = meta();

    let a = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2"), line("prd_bolt", "5")],
        None,
    );
    // Same set, reversed order.
    let b = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bolt", "5"), line("prd_bar", "2")],
        None,
    );
    // One quantity changed.
    let c = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2"), line("prd_bolt", "6")],
        None,
    );

    assert_eq!(
        a.revision.content_hash, b.revision.content_hash,
        "line order must not change the content hash"
    );
    assert_ne!(
        a.revision.content_hash, c.revision.content_hash,
        "a changed quantity MUST change the content hash"
    );
    // Re-authoring an identical set still mints a new revision — an
    // operator save is a real event even when nothing changed
    // (ADR-0105 §2.7: no hidden suppression).
    assert_eq!(b.revision.rev_number, 2);
    assert_ne!(a.revision.bom_rev_id, b.revision.bom_rev_id);
}

/// ADR-0105 §2.6 — a component may appear at most once per BOM, so
/// the component-keyed diff is well defined. Refused loud, not
/// silently collapsed.
#[test]
fn duplicate_component_in_one_bom_is_refused_loud() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    let meta = meta();

    let tx = conn.transaction().unwrap();
    let err = replace_bom_for_product(
        &tx,
        &ctx_for(&meta, "ervin"),
        "prd_widget",
        &[line("prd_bar", "2"), line("prd_bar", "3")],
        None,
    )
    .unwrap_err();
    assert!(
        matches!(&err, WorkOrderError::Validation(m) if m.contains("prd_bar")
            && m.contains("at most once")),
        "expected a loud duplicate-component validation error, got {err:?}"
    );
    drop(tx);

    // Nothing was written — no revision, no lines.
    let revs = list_bom_revisions(&conn, TEST_TENANT, "prd_widget").unwrap();
    assert!(revs.is_empty(), "a refused author must mint no revision");
}

// ─────────────────────────────────────────────────────────────────────
// 2. Diff — ADR-0105 §2.6
// ─────────────────────────────────────────────────────────────────────

/// A diff across two revisions reports exactly what was added,
/// removed, and re-quantified.
#[test]
fn diff_between_two_revisions_reports_added_removed_and_changed() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    insert_product(&conn, "prd_bolt", "Bolt");
    insert_product(&conn, "prd_washer", "Washer");
    let meta = meta();

    // rev1: bar×2, bolt×4      rev2: bar×3, washer×1
    //   → bar CHANGED 2→3, bolt REMOVED, washer ADDED.
    let r1 = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2"), line("prd_bolt", "4")],
        None,
    );
    let r2 = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "3"), line("prd_washer", "1")],
        None,
    );

    let from = list_bom_lines_for_revision(&conn, TEST_TENANT, &r1.revision.bom_rev_id).unwrap();
    let to = list_bom_lines_for_revision(&conn, TEST_TENANT, &r2.revision.bom_rev_id).unwrap();
    let diff = diff_bom_revisions(&from, &to).unwrap();

    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.added[0].component_id, "prd_washer");
    assert_eq!(diff.added[0].qty_per_unit, Decimal::from_str("1").unwrap());

    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.removed[0].component_id, "prd_bolt");
    assert_eq!(
        diff.removed[0].qty_per_unit,
        Decimal::from_str("4").unwrap()
    );

    assert_eq!(diff.changed.len(), 1);
    assert_eq!(diff.changed[0].component_id, "prd_bar");
    assert_eq!(diff.changed[0].qty_from, Decimal::from_str("2").unwrap());
    assert_eq!(diff.changed[0].qty_to, Decimal::from_str("3").unwrap());

    assert!(!diff.is_empty());

    // Reversing the arguments mirrors added/removed and flips the
    // change direction — the diff is directional, not a set XOR.
    let back = diff_bom_revisions(&to, &from).unwrap();
    assert_eq!(back.added[0].component_id, "prd_bolt");
    assert_eq!(back.removed[0].component_id, "prd_washer");
    assert_eq!(back.changed[0].qty_from, Decimal::from_str("3").unwrap());
    assert_eq!(back.changed[0].qty_to, Decimal::from_str("2").unwrap());

    // A revision against itself is an empty diff.
    let same = diff_bom_revisions(&from, &from).unwrap();
    assert!(same.is_empty(), "self-diff must be empty");
}

/// A legacy revision carrying a duplicate component cannot be diffed
/// meaningfully; the diff refuses rather than under-reporting the
/// change (the author-time gate cannot retroactively clean old rows).
#[test]
fn diff_refuses_loud_on_a_duplicate_component_side() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    let meta = meta();

    let good = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2")],
        None,
    );
    let good_lines =
        list_bom_lines_for_revision(&conn, TEST_TENANT, &good.revision.bom_rev_id).unwrap();

    // A hand-built duplicate-bearing side, as a pre-ADR-0105 row set
    // could be.
    let mut dup = good_lines.clone();
    dup.push(good_lines[0].clone());

    let err = diff_bom_revisions(&dup, &good_lines).unwrap_err();
    assert!(
        matches!(&err, WorkOrderError::Validation(m) if m.contains("duplicate component")),
        "expected a loud duplicate-side refusal, got {err:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// 3. The traceability link — ADR-0105 §2.5
// ─────────────────────────────────────────────────────────────────────

/// **The load-bearing test.** A released WO records WHICH BOM revision
/// it was built against; a later BOM edit does not retro-change it,
/// and the pinned revision still reads back as it was at Release.
#[test]
fn released_wo_pins_the_bom_revision_and_a_later_edit_cannot_move_it() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    insert_product(&conn, "prd_bolt", "Bolt");
    let meta = meta();
    seed_stock(&mut conn, &meta, "prd_bar", "100");
    seed_stock(&mut conn, &meta, "prd_bolt", "100");

    // Revision 1: bar×2. Release WO-1 against it.
    let r1 = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2")],
        Some("as-designed"),
    );
    let wo1 = create_wo(&mut conn, &meta, "WO-0001", "5");
    let out1 = release(&mut conn, &meta, &wo1, "rel-1");
    assert_eq!(out1.wo.state, WorkOrderState::Released);
    assert_eq!(
        out1.wo.bom_rev_id.as_deref(),
        Some(r1.revision.bom_rev_id.as_str()),
        "Release must pin the revision it consumed"
    );
    assert!(
        out1.warnings.is_empty(),
        "a revisioned BOM must release without a legacy warning, got {:?}",
        out1.warnings
    );

    // Revision 2 supersedes it: bar×3 + bolt×1.
    let r2 = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "3"), line("prd_bolt", "1")],
        Some("ECO-14"),
    );

    // WO-1's pin is UNMOVED by the later edit.
    let wo1_after = read_work_order(&conn, TEST_TENANT, &wo1).unwrap().unwrap();
    assert_eq!(
        wo1_after.bom_rev_id.as_deref(),
        Some(r1.revision.bom_rev_id.as_str()),
        "a BOM edit after Release must NOT retro-change what the batch was built to"
    );

    // The pin is REPRODUCIBLE: resolving it re-derives the exact
    // as-built BOM, not today's.
    let as_built =
        list_bom_lines_for_revision(&conn, TEST_TENANT, wo1_after.bom_rev_id.as_ref().unwrap())
            .unwrap();
    assert_eq!(as_built.len(), 1);
    assert_eq!(as_built[0].component_id, "prd_bar");
    assert_eq!(as_built[0].qty_per_unit, Decimal::from_str("2").unwrap());

    // A WO released AFTER the edit pins the NEW revision — two batches
    // of the same product are distinguishable by what they were built to.
    let wo2 = create_wo(&mut conn, &meta, "WO-0002", "5");
    let out2 = release(&mut conn, &meta, &wo2, "rel-2");
    assert_eq!(
        out2.wo.bom_rev_id.as_deref(),
        Some(r2.revision.bom_rev_id.as_str())
    );
    assert_ne!(out1.wo.bom_rev_id, out2.wo.bom_rev_id);
}

/// The pin is stamped ONCE, at Release. Subsequent transitions
/// (Start / Hold / Resume) must not clear or re-stamp it.
#[test]
fn later_transitions_do_not_overwrite_the_pin() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    let meta = meta();
    seed_stock(&mut conn, &meta, "prd_bar", "100");

    let r1 = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2")],
        None,
    );
    let wo = create_wo(&mut conn, &meta, "WO-0003", "2");
    release(&mut conn, &meta, &wo, "rel-3");

    for (action, key) in [
        (WoAction::Start, "start-3"),
        (WoAction::Hold, "hold-3"),
        (WoAction::Resume, "resume-3"),
    ] {
        let tx = conn.transaction().unwrap();
        transition_work_order(
            &tx,
            &ctx_for(&meta, "ervin"),
            &wo,
            TransitionInputs {
                action,
                reason: Some("t".to_string()),
                source_event_id: None,
                idempotency_key: key.to_string(),
                actual_machining_minutes: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();

        let after = read_work_order(&conn, TEST_TENANT, &wo).unwrap().unwrap();
        assert_eq!(
            after.bom_rev_id.as_deref(),
            Some(r1.revision.bom_rev_id.as_str()),
            "the {action:?} transition must leave the Release pin untouched"
        );
    }
}

/// A WO that has not been released carries no pin — `None` here means
/// "not yet built", not "unknown".
#[test]
fn unreleased_wo_carries_no_pin() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    let meta = meta();
    author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2")],
        None,
    );
    let wo = create_wo(&mut conn, &meta, "WO-0004", "1");
    let read = read_work_order(&conn, TEST_TENANT, &wo).unwrap().unwrap();
    assert_eq!(read.state, WorkOrderState::Created);
    assert_eq!(read.bom_rev_id, None);
}

/// ADR-0105 §2.4 — a pre-ADR-0105 BOM (rows with no `bom_rev_id`) is
/// NOT retro-attributed. The Release proceeds but warns loudly and
/// pins nothing rather than fabricating a revision.
#[test]
fn legacy_unrevisioned_bom_releases_with_a_loud_warning_and_no_pin() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    let meta = meta();
    seed_stock(&mut conn, &meta, "prd_bar", "100");

    // Simulate a row written before the revision store existed.
    conn.execute(
        "INSERT INTO boms (bom_line_id, tenant_id, product_id, component_id,
                           qty_per_unit, created_at, retired_at, bom_rev_id)
         VALUES ('bml_legacy', ?, 'prd_widget', 'prd_bar', 2,
                 '2026-01-01T00:00:00Z', NULL, NULL);",
        duckdb::params![TEST_TENANT],
    )
    .unwrap();

    let wo = create_wo(&mut conn, &meta, "WO-0005", "3");
    let out = release(&mut conn, &meta, &wo, "rel-5");

    assert_eq!(out.wo.state, WorkOrderState::Released);
    assert_eq!(
        out.wo.bom_rev_id, None,
        "a legacy BOM must pin NOTHING rather than a fabricated revision"
    );
    assert!(
        out.warnings
            .iter()
            .any(|w| w.contains("unrevisioned") && w.contains("re-save")),
        "expected a loud unrevisioned-BOM warning, got {:?}",
        out.warnings
    );

    // Re-authoring starts the history at revision 1 — nothing is
    // back-dated for the rows that had no author.
    let r1 = author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2")],
        Some("adopting revision control"),
    );
    assert_eq!(r1.revision.rev_number, 1);
}

// ─────────────────────────────────────────────────────────────────────
// 4. Attribution rides the hash-chained audit ledger — ADR-0105 §2.3
// ─────────────────────────────────────────────────────────────────────

/// Every revision writes exactly one `mes.bom_revision_created` entry
/// carrying the FULL line snapshot, in the same commit as the business
/// rows (CLAUDE.md rule 15).
#[test]
fn each_revision_appends_one_audit_entry_with_the_full_snapshot() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    insert_product(&conn, "prd_bar", "Bar");
    insert_product(&conn, "prd_bolt", "Bolt");
    let meta = meta();

    author(
        &mut conn,
        &meta,
        "ervin",
        "prd_widget",
        &[line("prd_bar", "2")],
        Some("initial"),
    );
    let r2 = author(
        &mut conn,
        &meta,
        "kata",
        "prd_widget",
        &[line("prd_bar", "3"), line("prd_bolt", "1")],
        Some("ECO-9"),
    );

    let mut stmt = conn
        .prepare(
            "SELECT payload FROM audit_ledger
             WHERE kind = 'mes.bom_revision_created' ORDER BY seq ASC;",
        )
        .unwrap();
    let payloads: Vec<Vec<u8>> = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(payloads.len(), 2, "one audit entry per authored revision");

    let p1: BomRevisionCreatedPayload = serde_json::from_slice(&payloads[0]).unwrap();
    assert_eq!(p1.rev_number, 1);
    assert_eq!(p1.prior_rev_id, None);
    assert_eq!(p1.actor, "ervin");
    assert_eq!(p1.reason.as_deref(), Some("initial"));
    assert_eq!(p1.lines.len(), 1);
    assert_eq!(p1.lines[0].component_id, "prd_bar");
    assert_eq!(p1.lines[0].qty_per_unit, Decimal::from_str("2").unwrap());

    let p2: BomRevisionCreatedPayload = serde_json::from_slice(&payloads[1]).unwrap();
    assert_eq!(p2.rev_number, 2);
    assert_eq!(
        p2.prior_rev_id.as_deref(),
        Some(p1.bom_rev_id.as_str()),
        "each revision must name the one it supersedes"
    );
    assert_eq!(p2.actor, "kata");
    assert_eq!(p2.bom_rev_id, r2.revision.bom_rev_id);
    assert_eq!(p2.content_hash, r2.revision.content_hash);
    // Full snapshot — the ledger can attest the BOM without the tables.
    assert_eq!(p2.line_count, 2);
    assert_eq!(
        p2.lines
            .iter()
            .map(|l| l.component_id.as_str())
            .collect::<Vec<_>>(),
        vec!["prd_bar", "prd_bolt"]
    );
}

/// A refused author must leave NO audit entry — the business rows and
/// the ledger append share one transaction, so a rollback takes both.
#[test]
fn a_refused_author_leaves_no_audit_entry() {
    let mut conn = setup_db();
    insert_product(&conn, "prd_widget", "Widget");
    let meta = meta();

    // Unknown product → refused before anything is written.
    let tx = conn.transaction().unwrap();
    let err = replace_bom_for_product(
        &tx,
        &ctx_for(&meta, "ervin"),
        "prd_does_not_exist",
        &[line("prd_bar", "2")],
        None,
    )
    .unwrap_err();
    assert!(matches!(err, WorkOrderError::ProductNotFound(_)));
    tx.rollback().unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_ledger WHERE kind = 'mes.bom_revision_created';",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}
