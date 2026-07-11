//! ADR-0099 H3 — `create_ncr` re-entrancy proof (the atomic quality + qc +
//! purchasing migration).
//!
//! # What this proves
//!
//! `quality::create_ncr` is re-entrantly reached from THREE contexts that each
//! already hold the shared `aberp_db::Handle` write guard:
//!
//!   1. `purchasing::record_receipt` — a failed incoming inspection auto-spawns a
//!      SupplierIssue NCR (S439) while record_receipt holds the guard;
//!   2. `qc_inspection::record_manual_inspection` — a failing verdict auto-spawns a
//!      Workmanship NCR while that fn holds the guard;
//!   3. the direct operator path — which acquires the guard and calls create_ncr.
//!
//! Because `create_ncr` now takes the PASSED-IN guard (never re-acquires
//! `db.write()`), none of these nest a second acquire of the non-reentrant writer
//! mutex. This test drives all three with BOTH tripwires armed and asserts none
//! fire:
//!
//!   * the writer-mutex re-entrancy tripwire (`Handle::write`/`read`
//!     `assert_not_reentrant`) — always compiled in debug/test; a nested acquire
//!     on the guard-holding thread PANICS;
//!   * `SERVE_HANDLE_LIVE` (`register_serve_handle`) — while a serve Handle is
//!     registered on the path, ANY independent audit open (`Ledger::open` /
//!     `append_reopen`) PANICS. The migrated fns append via `append_in_tx` on the
//!     shared guard, so nothing independent opens.
//!
//! A regression that re-introduced a self-opened `Ledger`/`Connection` in
//! `create_ncr` (or any of the three call sites) would panic HERE.

use aberp::purchasing::{
    create_po, ensure_schema as ensure_po_schema, record_receipt, transition_po, NewPo, NewPoLine,
    NewReceipt, PoState, ReceiptLineInput,
};
use aberp::qc_inspection::{record_manual_inspection, ManualInspectionRequest};
use aberp::quality::{create_ncr, NcrCategory, NcrSeverity, NewNcr};
use aberp::serve::open_tenant_handle;

use aberp::avl_vendors::{create_vendor, ensure_schema as ensure_avl_schema, VendorInputs};
use aberp_audit_ledger::serve_tripwire::{is_serve_handle_live, register_serve_handle};
use aberp_audit_ledger::{ensure_schema as audit_ensure_schema, BinaryHash, TenantId};
use aberp_qa::{
    create_inspection_plan, ensure_schema as ensure_qa_schema, NewInspectionPlan, QcSource, Verdict,
};

const T: &str = "reentrancy_proof";

#[test]
fn create_ncr_call_sites_never_nest_a_second_writer_acquire() {
    let dir = std::env::temp_dir()
        .join("aberp-reentrancy-proof")
        .join(ulid::Ulid::new().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("aberp.duckdb");
    let tenant = TenantId::new(T).unwrap();
    let hash = BinaryHash::from_bytes([0u8; 32]);

    // Schemas — seeded on a fresh connection BEFORE the shared Handle opens.
    {
        let conn = duckdb::Connection::open(&db_path).unwrap();
        audit_ensure_schema(&conn).unwrap();
        ensure_po_schema(&conn).unwrap();
        ensure_avl_schema(&conn).unwrap();
        aberp::quality::ensure_schema(&conn).unwrap();
        ensure_qa_schema(&conn).unwrap();
    }

    // Open the ONE shared Handle, then ARM both tripwires by registering it as
    // serve-live (mirrors serve boot: Handle open, then tripwire arm).
    let db = open_tenant_handle(&db_path, tenant.clone()).unwrap();
    let _tripwire = register_serve_handle(&db_path);
    assert!(
        is_serve_handle_live(&db_path),
        "SERVE_HANDLE_LIVE must be armed for this proof to mean anything"
    );

    // Seed an approved AVL vendor through the shared writer (no independent open).
    {
        let guard = db.write().unwrap();
        create_vendor(
            &guard,
            T,
            &VendorInputs {
                partner_id: "ptn_approved".into(),
                approved_status: "approved".into(),
                approval_categories: vec![],
                approved_until_utc: None,
                screening_notes: String::new(),
            },
            "qa",
        )
        .unwrap();
    }

    // ── Call site 1: purchasing::record_receipt → create_ncr (failed inspection).
    let po = create_po(
        &db,
        tenant.clone(),
        hash,
        "buyer",
        NewPo {
            vendor_partner_id: "ptn_approved".into(),
            currency: "EUR".into(),
            vat_rate_pct: 27,
            expected_delivery_utc: None,
            notes: String::new(),
            lines: vec![NewPoLine {
                product_id: None,
                description: "316L bar stock".into(),
                quantity: 10,
                unit_price_minor: 5000,
                expected_heat_lot_required: false,
            }],
        },
    )
    .unwrap();
    let issued = transition_po(
        &db,
        tenant.clone(),
        hash,
        "buyer",
        &po.po_id,
        PoState::IssuedToVendor,
        Some("manager"),
    )
    .unwrap();
    assert_eq!(issued.state, PoState::IssuedToVendor);
    let pol_id = {
        let conn = db.read().unwrap();
        aberp::purchasing::list_po_lines(&conn, T, &po.po_id).unwrap()[0]
            .pol_id
            .clone()
    };
    let after = record_receipt(
        &db,
        tenant.clone(),
        hash,
        "receiver",
        &po.po_id,
        NewReceipt {
            delivery_note_number: "DN-1".into(),
            lines: vec![ReceiptLineInput {
                pol_id,
                received_quantity: 4,
                inspection_pass: false, // FAIL → auto-NCR under record_receipt's guard
                inspection_notes: "surface pitting".into(),
                heat_lot: None,
            }],
        },
    )
    .expect("record_receipt with a failed inspection must NOT panic (re-entrancy)");
    assert_eq!(after.state, PoState::PartiallyReceived);

    // ── Call site 2: qc_inspection::record_manual_inspection → create_ncr (Major).
    let plan_id = {
        let guard = db.write().unwrap();
        create_inspection_plan(
            &guard,
            T,
            NewInspectionPlan {
                product_id: "prd_1".into(),
                feature_name: "Bore Ø".into(),
                nominal_value: 10.0,
                upper_tol: 0.010,
                lower_tol: -0.010,
                units: "mm".into(),
                optional_probe_cycle_id: None,
                enabled: true,
            },
        )
        .unwrap()
        .plan_id
    };
    let now = time::OffsetDateTime::parse(
        "2026-06-17T12:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let result = record_manual_inspection(
        &db,
        tenant.clone(),
        hash,
        "ervin",
        now,
        86400,
        ManualInspectionRequest {
            plan_id,
            actual_value: 10.025, // 1.5× half-width → Major → auto-NCR under its guard
            source: QcSource::Manual,
            units: None,
            source_event_id: None,
            probe_serial: None,
            last_calibration_at: None,
            wo_id: None,
            part_uid: None,
            heat_lot: None,
        },
    )
    .expect("record_manual_inspection with a Major verdict must NOT panic (re-entrancy)");
    assert_eq!(result.inspection.verdict, Verdict::Major);
    assert!(
        result.auto_ncr.is_some(),
        "a Major verdict auto-spawns an NCR"
    );

    // ── Call site 3: the direct operator path — acquire the guard, then create_ncr.
    {
        let mut guard = db.write().unwrap();
        let ncr = create_ncr(
            &mut guard,
            tenant.clone(),
            hash,
            "qa",
            NewNcr {
                severity: NcrSeverity::Minor,
                category: NcrCategory::Workmanship,
                description: "direct operator NCR".into(),
                affected_part_uids: vec![],
                affected_wo_ids: vec![],
                affected_heat_lots: vec![],
                photos: vec![],
            },
        )
        .expect("direct create_ncr under a held guard must NOT panic");
        assert!(ncr.ncr_id.starts_with("ncr_"));
    }

    // Reaching here (not a `SERVE_HANDLE_LIVE tripwire` / re-entrancy panic) IS the
    // proof: three auto-/direct create_ncr paths ran on the shared writer with the
    // tripwire armed and none forked the ledger or nested a second acquire.
    assert!(is_serve_handle_live(&db_path));
}
