//! ADR-0099 H3 STEP 4d — the transitive-closure families on the shared Handle.
//!
//! STEP 4c migrated the fused work-order cluster; STEP 4d closed the write-fusion
//! closure it cross-touches: `material_inventory` (`inventory_balances` /
//! `inventory_reservations`) and `quote_intake_log` (the DEAL/pickup/refuse sagas
//! + the intake daemon). These pins drive the two NEW behaviours on ONE shared
//! `aberp_db::Handle` with the `SERVE_HANDLE_LIVE` tripwire ARMED:
//!
//!   A. **material_inventory rule-15 fusion** — `assign_heat_lot`'s
//!      `inventory_balances` UPDATE and its heat-lot audit ride ONE tx on ONE
//!      write guard (was a fresh `Connection::open` UPDATE + a SEPARATE fresh
//!      `Ledger::open` append). With the tripwire armed, any forked `Ledger::open`
//!      would PANIC; reaching the assertions proves the append rode the shared
//!      writer. The balance row + the audit chain are read back through the Handle.
//!
//!   B. **quote_intake_log Handle-coherence** — an intake row inserted through the
//!      Handle writer (as the daemon now does) is visible to a Handle reader
//!      (`already_intook`), and the DEAL-saga-style co-write of `quote_intake_log`
//!      + `inventory_balances` in ONE guard tx is atomic. A fresh-open reader would
//!      miss the WAL-resident insert; the Handle reader sees it.

use std::path::PathBuf;

use duckdb::{params, Connection};

use aberp_audit_ledger::serve_tripwire::{is_serve_handle_live, register_serve_handle};
use aberp_audit_ledger::{
    ensure_schema as ensure_audit_schema, BinaryHash, EventKind, Ledger, LedgerMeta, TenantId,
};

use aberp::material_inventory::{
    append_heat_lot_events, assign_heat_lot, ensure_schema as ensure_inventory_schema, read_balance,
};
use aberp::serve::open_tenant_handle;

const T: &str = "step4d_intake_material";

fn test_dir(label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir()
            .join("aberp-step4d")
            .join(format!("{}-{}", label, ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).expect("create test dir");
    dir
}

fn seed_balance(conn: &Connection, grade: &str) {
    conn.execute(
        "INSERT INTO inventory_balances (
            tenant_id, material_grade, on_hand_qty, reserved_qty,
            committed_qty, consumed_qty, unit_of_measure, last_updated
         ) VALUES (?1, ?2, 100.0, 0, 0, 0, 'kg', '2026-06-06T00:00:00Z')",
        params![T, grade],
    )
    .unwrap();
}

/// A — assign_heat_lot's UPDATE + heat-lot audit on ONE guard tx, tripwire armed.
#[test]
fn assign_heat_lot_fuses_update_and_audit_on_one_guard_no_fork() {
    let dir = test_dir("heat-lot-fusion");
    let db_path = dir.join("aberp.duckdb");
    let tenant = TenantId::new(T).unwrap();
    let hash = BinaryHash::from_bytes([0u8; 32]);

    // Schemas + a balance row on a fresh conn BEFORE the Handle opens (Q1).
    {
        let conn = Connection::open(&db_path).unwrap();
        ensure_audit_schema(&conn).unwrap();
        ensure_inventory_schema(&conn).unwrap();
        seed_balance(&conn, "Ti-6Al-4V");
    }

    let db = open_tenant_handle(&db_path, tenant.clone()).unwrap();
    let _tripwire = register_serve_handle(&db_path);
    assert!(is_serve_handle_live(&db_path));

    // The rule-15 fusion: UPDATE + append_in_tx on ONE guard/tx. A forked
    // `Ledger::open` for the audit would trip SERVE_HANDLE_LIVE and PANIC.
    let meta = LedgerMeta::new(tenant.clone(), hash);
    {
        let mut guard = db.write().unwrap();
        let tx = guard.transaction().unwrap();
        let assignment =
            assign_heat_lot(&tx, T, "Ti-6Al-4V", "HEAT-4D-1", "file:///mtr.pdf", "ervin")
                .expect("assign_heat_lot on the guard tx");
        let appended =
            append_heat_lot_events(&tx, &meta, &assignment).expect("append heat-lot audit in tx");
        assert_eq!(appended, 2, "an MTR URL was recorded → 2 audit events");
        tx.commit().unwrap();
    }

    // The balance row carries the heat lot, read back through the Handle.
    let bal = {
        let conn = db.read().unwrap();
        read_balance(&conn, T, "Ti-6Al-4V")
            .expect("read balance")
            .expect("balance row exists")
    };
    assert_eq!(bal.heat_lot_number.as_deref(), Some("HEAT-4D-1"));

    // The audit chain verifies, read through the Handle (a fresh `Ledger::open`
    // here would trip the armed tripwire). Both heat-lot events are present.
    let ledger = Ledger::from_connection(db.read().unwrap(), tenant.clone(), hash);
    ledger.verify_chain().expect("audit chain verifies");
    let kinds: Vec<EventKind> = Ledger::from_connection(db.read().unwrap(), tenant, hash)
        .entries()
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&EventKind::MaterialHeatLotAssigned));
    assert!(kinds.contains(&EventKind::MaterialMtrUploaded));
    assert!(is_serve_handle_live(&db_path));
}

/// B — quote_intake_log written through the Handle is Handle-coherent; a
/// DEAL-saga-style co-write of quote_intake_log + inventory_balances in ONE guard
/// tx is atomic and both land.
#[test]
fn quote_intake_log_and_inventory_cowrite_atomic_on_one_guard() {
    use aberp_quote_intake::log_table;

    let dir = test_dir("intake-inventory-cowrite");
    let db_path = dir.join("aberp.duckdb");
    let tenant = TenantId::new(T).unwrap();

    {
        let conn = Connection::open(&db_path).unwrap();
        ensure_audit_schema(&conn).unwrap();
        ensure_inventory_schema(&conn).unwrap();
        log_table::ensure_schema(&conn).unwrap();
        seed_balance(&conn, "6061-T6");
    }

    let db = open_tenant_handle(&db_path, tenant.clone()).unwrap();
    let _tripwire = register_serve_handle(&db_path);

    // Insert an intake row through the Handle writer (as the daemon now does),
    // co-written with an inventory_balances UPDATE in ONE tx (DEAL-saga shape).
    let now = time::OffsetDateTime::parse(
        "2026-06-17T12:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    {
        let mut guard = db.write().unwrap();
        let tx = guard.transaction().unwrap();
        log_table::insert_intake(
            &tx,
            T,
            "q-4d-1",
            "inv_4d_1",
            "2026-06-17T11:59:00Z",
            now,
            "{}",
            "{}",
        )
        .expect("insert_intake on the guard tx");
        tx.execute(
            "UPDATE inventory_balances SET committed_qty = committed_qty + 1
             WHERE tenant_id = ?1 AND material_grade = ?2",
            params![T, "6061-T6"],
        )
        .expect("co-write inventory_balances in the same tx");
        tx.commit().unwrap();
    }

    // A Handle reader sees the WAL-resident intake row (a fresh open would miss it).
    let seen = {
        let conn = db.read().unwrap();
        log_table::already_intook(&conn, T, "q-4d-1").expect("already_intook read")
    };
    assert!(
        seen.is_some(),
        "the Handle-written intake row is visible to a Handle reader"
    );

    // The co-written inventory row committed atomically.
    let bal = {
        let conn = db.read().unwrap();
        read_balance(&conn, T, "6061-T6").unwrap().unwrap()
    };
    assert_eq!(bal.committed_qty, 1.0);
    assert!(is_serve_handle_live(&db_path));
}
