//! S439 (ADR-0090) — NCR / CAPA quality workflow + open-NCR shipment gate e2e.
//!
//! Walks the defense quality journey the brief names
//! ([[customer-journey-e2e-gate]]): defense WO → part marked → NCR opened →
//! shipment BLOCKED → CAPA created + approved + verified → NCR closed → shipment
//! gate PASSES. Plus the refuse-Shipment invariants ([[trust-code-not-operator]]):
//! a defense dispatch whose WO has an Open/Contained NCR on a unit is blocked;
//! the non-defense path is unaffected; a closed NCR unblocks.
//!
//! The quality lifecycle functions open their own connections (audit lives on
//! the same file), so these tests use a file-backed DuckDB, not in-memory.

use duckdb::{params, Connection};

use aberp::part_marking::{
    data_matrix_payload, ensure_schema as ensure_part_schema, generate_part_uid, record_part_marks,
    PartMark,
};
use aberp::partners::{create_partner, CustomerType, PartnerInputs, PartnerKind};
use aberp::quality::{
    self, ensure_schema as ensure_quality_schema, CapaVerdict, NcrCategory, NcrSeverity, NcrState,
    NewCapa, NewNcr,
};
use aberp::serve::{resolve_open_ncr_gate, OpenNcrGate};

use aberp_audit_ledger::{ensure_schema as audit_ensure_schema, BinaryHash, TenantId};

const T: &str = "ncr_capa_gate_test";

struct Fixture {
    db_path: std::path::PathBuf,
    tenant: TenantId,
    hash: BinaryHash,
}

fn setup() -> Fixture {
    let dir = std::env::temp_dir()
        .join("aberp-ncr-gate-test")
        .join(ulid::Ulid::new().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("aberp.duckdb");
    {
        let conn = Connection::open(&db_path).unwrap();
        audit_ensure_schema(&conn).unwrap();
        ensure_part_schema(&conn).unwrap();
        ensure_quality_schema(&conn).unwrap();
        aberp_work_orders::ensure_schema(&conn).unwrap();
        aberp_dispatch::ensure_schema(&conn).unwrap();
        aberp::quote_pricing_jobs::ensure_schema(&conn).unwrap();
    }
    Fixture {
        db_path,
        tenant: TenantId::new(T).unwrap(),
        hash: BinaryHash::from_bytes([0u8; 32]),
    }
}

fn partner_inputs(name: &str, ct: CustomerType) -> PartnerInputs {
    PartnerInputs {
        display_name: name.to_string(),
        legal_name: name.to_string(),
        kind: PartnerKind::Customer,
        customer_vat_status: Default::default(),
        customer_type: ct,
        tax_number: None,
        eu_vat_number: None,
        address_street: None,
        address_postal_code: None,
        address_city: None,
        address_country: None,
        bank_account: None,
        contact_email: None,
        contact_phone: None,
    }
}

fn seed_wo(conn: &Connection, wo_id: &str, qty: &str) {
    conn.execute(
        "INSERT INTO work_orders (
            wo_id, tenant_id, wo_number, product_id, qty_target, state, created_at
         ) VALUES (?1, ?2, ?3, 'prd_1', ?4, 'completed', '2026-06-06T00:00:00Z')",
        params![wo_id, T, wo_id, qty],
    )
    .unwrap();
}

fn seed_dispatch(conn: &Connection, dsp_id: &str, wo_id: &str, partner_id: &str, st: &str) {
    conn.execute(
        "INSERT INTO dispatches (dsp_id, tenant_id, wo_id, partner_id, state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, '2026-06-06T00:00:00Z')",
        params![dsp_id, T, wo_id, partner_id, st],
    )
    .unwrap();
}

fn mark_units(conn: &Connection, wo_id: &str, n: u32) -> Vec<String> {
    let mut marks = Vec::new();
    for i in 1..=n {
        let part_uid = generate_part_uid();
        let serial = format!("SN-{i}");
        let payload = data_matrix_payload(&part_uid, &serial, None);
        marks.push(PartMark {
            wo_id: wo_id.to_string(),
            unit_index: i,
            part_uid,
            serial_number: serial,
            data_matrix_payload: payload,
            heat_lot_reference: None,
            marked_at_utc: "2026-06-16T00:00:00Z".to_string(),
            marked_by_operator: "op".to_string(),
        });
    }
    record_part_marks(conn, T, wo_id, &marks).unwrap();
    marks.into_iter().map(|m| m.part_uid).collect()
}

fn get_dispatch(conn: &Connection, dsp_id: &str) -> aberp_dispatch::Dispatch {
    aberp_dispatch::get_dispatch(conn, T, dsp_id)
        .unwrap()
        .expect("read seeded dispatch")
}

/// A fresh shared Handle on the fixture DB. Opened per call, AFTER any seed
/// connection has closed, so it observes the committed state coherently
/// (ADR-0099 H3 — the migrated quality/qc/purchasing fns route through this).
fn open_handle(fx: &Fixture) -> aberp_db::HandleArc {
    aberp::serve::open_tenant_handle(&fx.db_path, fx.tenant.clone()).unwrap()
}

fn open_ncr_on(fx: &Fixture, part_uids: &[String]) -> String {
    // `create_ncr` takes the passed-in write guard (its re-entrant-safe contract).
    let handle = open_handle(fx);
    let mut guard = handle.write().unwrap();
    quality::create_ncr(
        &mut guard,
        fx.tenant.clone(),
        fx.hash,
        "qa",
        NewNcr {
            severity: NcrSeverity::Major,
            category: NcrCategory::Workmanship,
            description: "surface finish out of spec".into(),
            affected_part_uids: part_uids.to_vec(),
            affected_wo_ids: vec![],
            affected_heat_lots: vec![],
            photos: vec![],
        },
    )
    .unwrap()
    .ncr_id
}

/// Open one Major/Workmanship NCR on `part_uid` through a PERSISTENT, already-open
/// Handle (the prod `state.db` shape) — the write guard is acquired from the
/// passed-in Handle and dropped when the closure returns, exactly as
/// `handle_create_ncr` does in serve.
fn new_open_ncr(handle: &aberp_db::HandleArc, fx: &Fixture, part_uid: &str, desc: &str) -> String {
    let mut guard = handle.write().unwrap();
    quality::create_ncr(
        &mut guard,
        fx.tenant.clone(),
        fx.hash,
        "qa",
        NewNcr {
            severity: NcrSeverity::Major,
            category: NcrCategory::Workmanship,
            description: desc.into(),
            affected_part_uids: vec![part_uid.to_string()],
            affected_wo_ids: vec![],
            affected_heat_lots: vec![],
            photos: vec![],
        },
    )
    .unwrap()
    .ncr_id
}

/// ADR-0099 H3 STEP 2b — PROD-TOPOLOGY coherence pin for the defense open-NCR
/// shipment gate (the fail-open the step-2 adversarial found).
///
/// Step 2 moved quality's WRITERS onto the shared `aberp_db::Handle` (checkpoint
/// disabled → NCR writes stay WAL-resident) but left the gate's NCR READ on a
/// fresh `Connection::open`. This test runs the PROD topology — ONE persistent
/// Handle held live as `state.db` — and pins the exact contrast, deterministically:
///
///   * FIX — the gate reads NCRs through the LIVE Handle (`state.db.read()`), so
///     it SEES a WAL-resident NCR and BLOCKS the defense shipment (does NOT fail
///     open), even after an interleaved fresh opener has torn the on-disk tail.
///   * BUG — the pre-fix pattern (the gate's NCR read on a fresh `Connection::open`)
///     reads the torn on-disk state and MISSES that NCR → the gate would return
///     `Pass` = FAILS OPEN, shipping a WO with an unresolved Open NCR.
///
/// The tear is real and deterministic: a fresh opener's checkpoint-on-close folds
/// the Handle's uncheckpointed WAL and desyncs it, so a LATER Handle write (NCR-B
/// below) is silently lost on disk while remaining live in the Handle's own cache
/// (`SELECT` via `Handle::read` sees it; a fresh `Connection::open` does not).
///
/// This is deliberately NOT the serial open→drop→read shape of the sibling tests
/// above: dropping the writer Handle folds the WAL to disk and MASKS the tear.
#[test]
fn open_ncr_gate_reads_through_live_handle_pre_fix_fresh_open_fails_open() {
    use aberp::part_marking::list_part_marks;
    use aberp::quality::{list_ncrs, open_ncr_ids_blocking_part_uids, NcrFilter};

    let fx = setup();

    // Seed the fresh-open families (partner/WO/dispatch/parts) on a conn that
    // DROPS *before* the Handle opens — in prod these are written by their own
    // fresh opens; the gate reads them via the fresh `conn`, never the Handle
    // (the Handle would be Q2-blind to their post-boot writes). Two independent
    // WOs/dispatches so each gate result hinges on exactly one NCR.
    let uid_a;
    let uid_b;
    {
        let conn = Connection::open(&fx.db_path).unwrap();
        let buyer =
            create_partner(&conn, T, &partner_inputs("Def Co", CustomerType::Defense)).unwrap();
        seed_wo(&conn, "wo-a", "1");
        seed_dispatch(&conn, "dsp-a", "wo-a", &buyer.id, "drafted");
        seed_wo(&conn, "wo-b", "1");
        seed_dispatch(&conn, "dsp-b", "wo-b", &buyer.id, "drafted");
        uid_a = mark_units(&conn, "wo-a", 1).remove(0);
        uid_b = mark_units(&conn, "wo-b", 1).remove(0);
    }

    // ── PROD TOPOLOGY: ONE persistent Handle == state.db, held live throughout ──
    let handle = open_handle(&fx);

    // NCR-A on wo-a's unit, written THROUGH the live Handle (WAL-resident,
    // uncheckpointed) — this is the prior WAL write the interleaved tear folds.
    let ncr_a = new_open_ncr(&handle, &fx, &uid_a, "surface finish out of spec");

    // An interleaved fresh `Connection::open` reader + close — the pre-fix reader
    // shape, and also what every not-yet-migrated dispatch/part fresh opener does
    // in prod. Its checkpoint-on-close folds + desyncs the Handle's WAL tail.
    {
        let tearing = Connection::open(&fx.db_path).unwrap();
        let _ = list_ncrs(&tearing, T, &NcrFilter::default()).unwrap();
    }

    // NCR-B on wo-b's unit, written THROUGH the live Handle AFTER the tear. It is
    // live in the Handle's cache but silently lost on disk (asserted at the end).
    let ncr_b = new_open_ncr(&handle, &fx, &uid_b, "second defect");

    // ── THE GATE INSTANT (process still live) — same fresh `conn` for the
    // fresh-open-family reads; the two paths differ ONLY in where NCRs are read. ──
    let conn = Connection::open(&fx.db_path).unwrap();
    let dsp_b = get_dispatch(&conn, "dsp-b");

    // FIX: NCRs read through the LIVE Handle → sees NCR-B → BLOCKS (not fail-open).
    match resolve_open_ncr_gate(&conn, &handle, T, &dsp_b).unwrap() {
        OpenNcrGate::Blocked {
            work_order_id,
            blocking_ncr_ids,
            ..
        } => {
            assert_eq!(work_order_id, "wo-b");
            assert_eq!(
                blocking_ncr_ids,
                vec![ncr_b.clone()],
                "the live-Handle gate must SEE the WAL-resident NCR-B and block"
            );
        }
        other => panic!("defense shipment FAILED OPEN through the live Handle: {other:?}"),
    }

    // PRE-FIX: replicate the gate's NCR read on the fresh `conn`. It reads the torn
    // on-disk state, MISSES NCR-B, and the pure blocking check comes back empty —
    // i.e. `resolve_open_ncr_gate` would return `Pass` = FAIL OPEN. This is the
    // exact code path this step removes; it fails only because of the fix.
    let part_uids_b: Vec<String> = list_part_marks(&conn, T, "wo-b")
        .unwrap()
        .into_iter()
        .map(|m| m.part_uid)
        .collect();
    let stale_ncrs = list_ncrs(&conn, T, &NcrFilter::default()).unwrap();
    let pre_fix_blocking = open_ncr_ids_blocking_part_uids(&stale_ncrs, &part_uids_b);
    assert!(
        pre_fix_blocking.is_empty(),
        "PRE-FIX: a fresh-open gate read misses the WAL-torn-lost NCR-B → the gate \
         would return Pass = FAIL OPEN. (If this ever becomes non-empty the tear \
         hazard no longer reproduces and this pin must be revisited.)"
    );
    drop(conn);
    drop(handle); // end the process; checkpoint-on-shutdown is disabled (F-A)

    // The durable on-disk truth: NCR-A survived (folded by the tear) but NCR-B was
    // silently lost — the fail-open the Handle read is immune to.
    let disk = Connection::open(&fx.db_path).unwrap();
    let on_disk = |id: &str| -> i64 {
        disk.query_row(
            "SELECT COUNT(*) FROM ncrs WHERE ncr_id = ?1",
            params![id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(on_disk(&ncr_a), 1, "the pre-tear NCR-A is durable");
    assert_eq!(
        on_disk(&ncr_b),
        0,
        "the post-tear NCR-B is silently lost on disk — a fresh-open gate read fails open"
    );
}

/// A defense dispatch whose WO has a unit referenced by an Open NCR is BLOCKED;
/// resolving + closing that NCR unblocks it.
#[test]
fn defense_dispatch_blocked_by_open_ncr_then_unblocked_when_closed() {
    let fx = setup();
    let conn = Connection::open(&fx.db_path).unwrap();
    let buyer = create_partner(&conn, T, &partner_inputs("Def Co", CustomerType::Defense)).unwrap();
    seed_wo(&conn, "wo-def", "2");
    seed_dispatch(&conn, "dsp-def", "wo-def", &buyer.id, "drafted");
    let uids = mark_units(&conn, "wo-def", 2);
    drop(conn);

    // Open an NCR on unit 1 → defense gate BLOCKS, naming the NCR.
    let ncr_id = open_ncr_on(&fx, &[uids[0].clone()]);
    let conn = Connection::open(&fx.db_path).unwrap();
    let dispatch = get_dispatch(&conn, "dsp-def");
    match resolve_open_ncr_gate(&conn, &open_handle(&fx), T, &dispatch).unwrap() {
        OpenNcrGate::Blocked {
            work_order_id,
            customer_type,
            blocking_ncr_ids,
        } => {
            assert_eq!(work_order_id, "wo-def");
            assert_eq!(customer_type, "defense");
            assert_eq!(blocking_ncr_ids, vec![ncr_id.clone()]);
        }
        other => panic!("expected Blocked, got {other:?}"),
    }
    drop(conn);

    // Contained still blocks (brief §4: Open OR Contained).
    quality::transition_ncr(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &ncr_id,
        NcrState::Contained,
        "",
    )
    .unwrap();
    let conn = Connection::open(&fx.db_path).unwrap();
    assert!(matches!(
        resolve_open_ncr_gate(&conn, &open_handle(&fx), T, &get_dispatch(&conn, "dsp-def"))
            .unwrap(),
        OpenNcrGate::Blocked { .. }
    ));
    drop(conn);

    // Drive to close with a verified CAPA → gate PASSES.
    quality::transition_ncr(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &ncr_id,
        NcrState::UnderInvestigation,
        "",
    )
    .unwrap();
    quality::transition_ncr(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &ncr_id,
        NcrState::CorrectionApplied,
        "",
    )
    .unwrap();
    let capa = quality::create_capa(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        NewCapa {
            ncr_id: ncr_id.clone(),
            corrective_action_text: "re-polish".into(),
            preventive_action_text: "tighten op sheet".into(),
            responsible_operator: "qa".into(),
            target_close_date: "2026-07-01".into(),
        },
    )
    .unwrap();
    quality::approve_capa(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &capa.capa_id,
    )
    .unwrap();
    quality::review_capa_effectiveness(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &capa.capa_id,
        CapaVerdict::Verified,
        "holds",
    )
    .unwrap();
    let closed = quality::transition_ncr(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &ncr_id,
        NcrState::Closed,
        "done",
    )
    .unwrap();
    assert_eq!(closed.state, NcrState::Closed);

    let conn = Connection::open(&fx.db_path).unwrap();
    assert_eq!(
        resolve_open_ncr_gate(&conn, &open_handle(&fx), T, &get_dispatch(&conn, "dsp-def"))
            .unwrap(),
        OpenNcrGate::Pass,
        "closed NCR no longer blocks shipment"
    );
}

/// The COMMERCIAL path is unaffected: an Industrial buyer's dispatch ships even
/// with an Open NCR on its part.
#[test]
fn non_defense_dispatch_unaffected_by_open_ncr() {
    let fx = setup();
    let conn = Connection::open(&fx.db_path).unwrap();
    let buyer = create_partner(
        &conn,
        T,
        &partner_inputs("Ind Co", CustomerType::Industrial),
    )
    .unwrap();
    seed_wo(&conn, "wo-ind", "1");
    seed_dispatch(&conn, "dsp-ind", "wo-ind", &buyer.id, "drafted");
    let uids = mark_units(&conn, "wo-ind", 1);
    drop(conn);

    open_ncr_on(&fx, &uids);
    let conn = Connection::open(&fx.db_path).unwrap();
    assert_eq!(
        resolve_open_ncr_gate(&conn, &open_handle(&fx), T, &get_dispatch(&conn, "dsp-ind"))
            .unwrap(),
        OpenNcrGate::Pass,
        "non-defense path is never gated by NCRs"
    );
}

/// An NCR on a DIFFERENT part UID does not block a WO whose units are clean.
#[test]
fn open_ncr_on_other_part_does_not_block() {
    let fx = setup();
    let conn = Connection::open(&fx.db_path).unwrap();
    let buyer = create_partner(&conn, T, &partner_inputs("Def Co", CustomerType::Defense)).unwrap();
    seed_wo(&conn, "wo-def", "1");
    seed_dispatch(&conn, "dsp-def", "wo-def", &buyer.id, "drafted");
    mark_units(&conn, "wo-def", 1);
    drop(conn);

    // NCR references an unrelated UID.
    open_ncr_on(&fx, &["dp-0000000000000000000000000Z".to_string()]);
    let conn = Connection::open(&fx.db_path).unwrap();
    assert_eq!(
        resolve_open_ncr_gate(&conn, &open_handle(&fx), T, &get_dispatch(&conn, "dsp-def"))
            .unwrap(),
        OpenNcrGate::Pass
    );
}

/// Full quality loop fires every NCR/CAPA EventKind exactly where expected.
#[test]
fn full_loop_fires_all_quality_events() {
    let fx = setup();
    let ncr_id = open_ncr_on(&fx, &["dp-AAAAAAAAAAAAAAAAAAAAAAAAAA".to_string()]);
    quality::transition_ncr(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &ncr_id,
        NcrState::Contained,
        "",
    )
    .unwrap();
    quality::transition_ncr(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &ncr_id,
        NcrState::UnderInvestigation,
        "",
    )
    .unwrap();
    quality::transition_ncr(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &ncr_id,
        NcrState::CorrectionApplied,
        "",
    )
    .unwrap();
    let capa = quality::create_capa(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        NewCapa {
            ncr_id: ncr_id.clone(),
            corrective_action_text: "c".into(),
            preventive_action_text: "p".into(),
            responsible_operator: "qa".into(),
            target_close_date: "2026-07-01".into(),
        },
    )
    .unwrap();
    quality::approve_capa(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &capa.capa_id,
    )
    .unwrap();
    quality::review_capa_effectiveness(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &capa.capa_id,
        CapaVerdict::Verified,
        "ok",
    )
    .unwrap();
    quality::close_capa(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &capa.capa_id,
    )
    .unwrap();
    quality::transition_ncr(
        &open_handle(&fx),
        fx.tenant.clone(),
        fx.hash,
        "qa",
        &ncr_id,
        NcrState::Closed,
        "done",
    )
    .unwrap();

    let conn = Connection::open(&fx.db_path).unwrap();
    let count = |kind: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM audit_ledger WHERE kind = ?1",
            params![kind],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(count("ncr.created"), 1);
    assert_eq!(
        count("ncr.state_changed"),
        4,
        "contained, under_inv, corr_applied, closed"
    );
    assert_eq!(count("ncr.closed"), 1);
    assert_eq!(count("capa.created"), 1);
    assert_eq!(count("capa.approved"), 1);
    assert_eq!(count("capa.effectiveness_reviewed"), 1);
    assert_eq!(count("capa.closed"), 1);
}
