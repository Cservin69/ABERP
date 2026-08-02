//! ADR-0108 Step 7 Part G — **the purchasing / purchase-order family crosses.**
//!
//! Parts B (partners), C (products/inventory), D (work-orders/BOM), E (QA/QC)
//! and F (dispatch) are already on this branch, so this is **Part G**.
//!
//! Eleven pins, in the order they defend:
//!
//! 1. all four tables cross and **no column drifts**, asserted against the
//!    fixture's own literals as well as through the gate;
//! 2. `po_number_state`'s key really is the composite `(tenant_id, year)` — one
//!    tenant with two years and one year under two tenants both survive, which a
//!    key built from either component alone would have collapsed;
//! 3. **§3.2 A's money crosses as `'integer'` with its value intact over the
//!    whole `i64` range**, §3.2 H's two booleans cross as `'integer'` with both
//!    arms intact, and every `TEXT` column as `'text'`;
//! 4. **the disjunction sweep** — every natural key in the source is either
//!    carried with a **byte-identical** round-trip or **refused loudly** naming
//!    the table and the key. Both arms are required to fire;
//! 5. the same disjunction proved through **real storage**: 256 generated PO
//!    lines seeded into a real DuckDB file, carried by the real migrator into a
//!    real SQLite file, and read back — text byte-for-byte, money and quantity
//!    value-for-value, booleans both ways;
//! 6. a duplicate natural key **fails the whole carry** rather than crossing as
//!    a row nobody compares — seeded on `po_number_state`, where a duplicate
//!    does not merely blind the gate but lets the allocator **re-issue a PO
//!    number**;
//! 7. `ensure_purchasing_schema` builds all four tables, is idempotent, declares
//!    every column with the §3.2 vocabulary, and `STRICT` refuses a float into a
//!    money column;
//! 8. the gate **hard-stops** per table when a table was not carried;
//! 9. the per-row equality arm is shown to go red on **every mutable column of
//!    every table, one column at a time** — including a **one-minor-unit** money
//!    change, which is the smallest drift this family can suffer;
//! 10. a source with none of the four tables is a legitimate shape and the gate
//!     says so out loud;
//! 11. the per-table presence hold works: a source missing one table still
//!     crosses the other three, and the missing one is **not** silently created.
//!
//! **This test pins no §3.4 fold, because this family owes none**, and no R2
//! canonical-decimal handling, because measurement says the family has no
//! `DECIMAL` and no `DOUBLE` column at all — its money is §3.2 A `INTEGER` minor
//! units and its quantities are §3.2 F `INTEGER` counts. It pins no M11-shaped
//! `LOWER()`/`LIKE` refusal either, because both patterns return zero hits
//! against `purchasing.rs`.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::migrate_dispatch::unique_natural_keys;
use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};

const TENANT: &str = "test";
/// The second tenant, which exists so that `po_number_state`'s key cannot be
/// mistaken for a single-column one.
const OTHER: &str = "other-tenant";

/// The family's four tables, in the order the migrator and the gate walk them.
const FAMILY: &[&str] = &[
    "purchase_orders",
    "purchase_order_lines",
    "purchase_order_receipts",
    "po_number_state",
];

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-purchasing-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// `(po_id, state, subtotal_minor, vat_rate_pct, vat_minor, total_minor,
///   populated, notes)`.
///
/// * **`po-01`** — `draft`: all five nullable columns are `NULL`, and `notes` is
///   the **empty string**. `notes` is `NOT NULL`, so `""` is a value and must
///   cross as `""` rather than as `NULL` — a distinction `STRICT` and the
///   `IS NOT NULL`-scoped `typeof` sweep are both structurally blind to.
/// * **`po-02`** — `issued_to_vendor`: every nullable column populated, and
///   `notes` carries the accented Hungarian characters an ASCII `LOWER()` would
///   fold plus a literal `%` and `_`. There is **no** `LOWER()` or `LIKE` site
///   in this family (measured, zero hits), so this is not a T-12 pin — it is
///   here so a T-12-shaped test has a value to key on if one becomes owed.
/// * **`po-03`** — `received` with **every money column zero**. Zero is a value:
///   a carry that dropped a column and left the SQLite default would produce 0
///   too, so the per-row arm has to see the same 0 on both sides rather than
///   infer it.
/// * **`po-90-min` / `po-91-max`** — `total_minor` at `i64::MIN` and `i64::MAX`.
///   DuckDB `BIGINT` and SQLite `INTEGER` are both 64-bit and §3.2 A says the
///   representation is unchanged; these two rows are what turns that claim into
///   a measurement. **They are ordered `min` before `max` deliberately**: the
///   gate's Σ is a `checked_add` fold in key order, so the running sum dips to
///   `i64::MIN + ε` and comes back rather than overflowing — a fixture with the
///   two swapped would make the gate fail on the fold instead of proving the
///   carry.
const PO_CASES: &[(&str, &str, i64, i64, i64, i64, bool, &str)] = &[
    ("po-01", "draft", 100_000, 27, 27_000, 127_000, false, ""),
    (
        "po-02",
        "issued_to_vendor",
        250_000,
        27,
        67_500,
        317_500,
        true,
        "Árvíztűrő tükörfúrógép — 100% átvéve _ raklap",
    ),
    ("po-03", "received", 0, 0, 0, 0, true, "nulla értékű PO"),
    ("po-90-min", "cancelled", 0, 0, 0, i64::MIN, false, "min"),
    ("po-91-max", "cancelled", 0, 0, 0, i64::MAX, false, "max"),
];

/// `(pol_id, po_id, seq, product_id, description, quantity, unit_price_minor,
///   line_total_minor, expected_heat_lot_required, received_quantity)`.
///
/// **Both arms of the §3.2 H boolean are present**, which is the whole point of
/// including a boolean in the fixture at all: a carry that bound `true` for
/// every row would still round-trip a fixture that only had `true`s.
///
/// `pol-03` carries an **empty description** and a **zero unit price**, so the
/// empty-string and zero-value arms run on this table too.
const POL_CASES: &[(
    &str,
    &str,
    i64,
    Option<&str>,
    &str,
    i64,
    i64,
    i64,
    bool,
    i64,
)] = &[
    (
        "pol-01",
        "po-01",
        0,
        None,
        "Rozsdamentes rúd Ø20",
        10,
        10_000,
        100_000,
        false,
        0,
    ),
    (
        "pol-02",
        "po-02",
        0,
        Some("prd-01"),
        "S355J2 lemez 10 mm",
        5,
        50_000,
        250_000,
        true,
        5,
    ),
    ("pol-03", "po-02", 1, None, "", 1, 0, 0, false, 0),
];

/// `(por_id, pol_id, received_quantity, inspection_pass, inspection_notes,
///   heat_lot_assigned, ncr_id)`.
///
/// **Both arms of `inspection_pass` are present**, and it is the family's
/// load-bearing boolean: `false` is what mints the S439 auto-NCR, so a column
/// that crossed as the string `"false"` — which every Rust `bool` read would
/// then see as `false` — would turn every *passing* incoming inspection into a
/// failure.
///
/// `por-02` is the failed one, so it is the only row with a non-NULL `ncr_id`,
/// and `por-01` is the only one with a non-NULL `heat_lot_assigned`. Between
/// them every nullable column on this table is exercised in both states.
const POR_CASES: &[(&str, &str, i64, bool, &str, Option<&str>, Option<&str>)] = &[
    ("por-01", "pol-02", 5, true, "", Some("HL-2026-0001"), None),
    (
        "por-02",
        "pol-03",
        0,
        false,
        "Méret eltérés — 100% selejt",
        None,
        Some("ncr-01"),
    ),
];

/// `(tenant_id, year, next_number, updated_at)`.
///
/// **`2026` appears under two tenants and `test` appears under two years**,
/// which is the point: the key is the composite `(tenant_id, year)`, and a
/// migrator that keyed its `BTreeMap` on either component alone would collapse
/// two buckets onto one entry and every other check would stay green. Pin 2
/// asserts exactly that.
const PO_NUMBER_CASES: &[(&str, i64, i64, &str)] = &[
    (OTHER, 2026, 1, "2026-01-02T08:00:00Z"),
    (TENANT, 2025, 12, "2025-12-31T23:59:59Z"),
    (TENANT, 2026, 4, "2026-02-01T09:00:00Z"),
];

/// How many generated PO lines pin 5 pushes through real storage.
const SWEPT_ROWS: usize = 256;

/// A deterministic xorshift, so a failure is reproducible from the file alone.
fn rng(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

/// A `TEXT` value across the shapes a purchasing record actually holds — ULIDs,
/// material descriptions, accented operator notes, delivery-note numbers — plus
/// the ones that break a naive bind: quotes, a literal `%`, a newline, and the
/// empty string.
///
/// **No interior NUL**: neither engine's `TEXT` can hold one, so a sweep that
/// generated one would be pinning a shared limitation rather than the carry.
fn swept_text(i: usize, state: &mut u64) -> String {
    let n = rng(state);
    match i % 8 {
        0 => String::new(),
        1 => format!("01J{:016X}", n),
        2 => format!("Árvíztűrő {} tükörfúrógép", n % 1000),
        3 => format!("it's a \"quoted\" 100% value {}", n % 97),
        4 => format!("line one\nline two\ttab {}", n % 89),
        5 => "日本語のテキスト — 汎用".to_string(),
        6 => format!("{}", n),
        _ => format!("SZL-{}-{:04}", 2020 + (n % 10), n % 9999),
    }
}

/// A money or quantity value for the sweep, bounded so that 256 of them cannot
/// overflow the gate's `checked_add` fold. Both signs are generated: a credit
/// note against a PO is a negative line total, and the carry must not assume
/// money is non-negative.
fn swept_i64(state: &mut u64) -> i64 {
    (rng(state) % 1_000_001) as i64 - 500_000
}

/// Seed a DEV-shaped DuckDB through the **real** `ensure_schema`, so the SQLite
/// side is compared against the schema the product actually builds rather than
/// against a hand-written copy of it.
///
/// One of them, because one module builds all four tables in one batch —
/// unlike Parts C, D, E and F, where two or three modules did.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");

    {
        let conn = duckdb::Connection::open(&db).unwrap();
        aberp::purchasing::ensure_schema(&conn).unwrap();

        for (po_id, state, subtotal, vat_pct, vat, total, populated, notes) in PO_CASES {
            conn.execute(
                "INSERT INTO purchase_orders
                   (po_id, tenant_id, po_number, vendor_partner_id, currency, subtotal_minor,
                    vat_rate_pct, vat_minor, total_minor, state, vendor_avl_status, issued_at_utc,
                    expected_delivery_utc, notes, requested_by_operator, approved_by_operator,
                    approved_at_utc, created_at_utc)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    po_id,
                    TENANT,
                    format!("PO-2026-{}", &po_id[3..]),
                    "prt-vendor-01",
                    "HUF",
                    subtotal,
                    vat_pct,
                    vat,
                    total,
                    state,
                    populated.then_some("approved"),
                    populated.then_some("2026-02-01T10:00:00Z"),
                    populated.then_some("2026-03-01"),
                    notes,
                    "operator@example.test",
                    populated.then_some("manager@example.test"),
                    populated.then_some("2026-02-01T09:30:00Z"),
                    "2026-02-01T09:00:00Z",
                ],
            )
            .unwrap();
        }

        for (pol_id, po_id, seq, product_id, description, qty, unit, total, heat, received) in
            POL_CASES
        {
            insert_line(
                &conn,
                TENANT,
                pol_id,
                po_id,
                *seq,
                *product_id,
                description,
                *qty,
                *unit,
                *total,
                *heat,
                *received,
            );
        }

        for (por_id, pol_id, received, pass, notes, heat_lot, ncr_id) in POR_CASES {
            conn.execute(
                "INSERT INTO purchase_order_receipts
                   (por_id, po_id, pol_id, tenant_id, received_at_utc, received_by_operator,
                    delivery_note_number, received_quantity, inspection_pass, inspection_notes,
                    heat_lot_assigned, ncr_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    por_id,
                    "po-02",
                    pol_id,
                    TENANT,
                    "2026-02-10T14:00:00Z",
                    "raktaros@example.test",
                    "SZL-2026-0042",
                    received,
                    pass,
                    notes,
                    heat_lot,
                    ncr_id,
                ],
            )
            .unwrap();
        }

        for (tenant, year, next, updated) in PO_NUMBER_CASES {
            conn.execute(
                "INSERT INTO po_number_state (tenant_id, year, next_number, updated_at)
                 VALUES (?, ?, ?, ?)",
                duckdb::params![tenant, year, next, updated],
            )
            .unwrap();
        }

        // The swept rows, on their own PO so the fixture's own cases stay
        // readable in a failure message.
        let mut state = 0x5EED_1108_0607_u64 ^ 0x9E37_79B9_7F4A_7C15;
        for i in 0..SWEPT_ROWS {
            insert_line(
                &conn,
                TENANT,
                &format!("pol-sweep-{i:04}"),
                "po-sweep",
                i as i64,
                Some(&swept_text(i, &mut state)),
                &swept_text(i + 3, &mut state),
                swept_i64(&mut state),
                swept_i64(&mut state),
                swept_i64(&mut state),
                i % 2 == 0,
                swept_i64(&mut state),
            );
        }

        conn.close().unwrap();
    }

    seed_ledger(&db);
    db
}

/// One `purchase_order_lines` row.
#[allow(clippy::too_many_arguments)]
fn insert_line(
    conn: &duckdb::Connection,
    tenant: &str,
    pol_id: &str,
    po_id: &str,
    seq: i64,
    product_id: Option<&str>,
    description: &str,
    quantity: i64,
    unit_price_minor: i64,
    line_total_minor: i64,
    expected_heat_lot_required: bool,
    received_quantity: i64,
) {
    conn.execute(
        "INSERT INTO purchase_order_lines
           (pol_id, po_id, tenant_id, seq, product_id, description, quantity, unit_price_minor,
            currency, line_total_minor, expected_heat_lot_required, received_quantity)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        duckdb::params![
            pol_id,
            po_id,
            tenant,
            seq,
            product_id,
            description,
            quantity,
            unit_price_minor,
            "HUF",
            line_total_minor,
            expected_heat_lot_required,
            received_quantity,
        ],
    )
    .unwrap();
}

/// The audit chain + mirror + tamper-evidence layer the Step-4 gate turns on.
fn seed_ledger(db: &Path) {
    {
        let mut ledger = Ledger::open(
            db,
            TenantId::new(TENANT.to_string()).unwrap(),
            BinaryHash::from_bytes([7u8; 32]),
        )
        .unwrap();
        for i in 0..3 {
            ledger
                .append(
                    EventKind::DbAutoRecovered,
                    format!(r#"{{"n":{i}}}"#).into_bytes(),
                    Actor::test_only(),
                    None,
                )
                .unwrap();
        }
        ledger
            .sync_mirror(&aberp_audit_ledger::mirror_path_for(db))
            .unwrap();
    }
    let conn = duckdb::Connection::open(db).unwrap();
    conn.execute_batch(
        "UPDATE audit_ledger
            SET session_id = 'sess-7',
                session_pubkey = 'pubkey-hex',
                event_sig = 'sig-' || CAST(seq AS VARCHAR);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO audit_ledger_anchors
           (id, tenant_id, session_id, kind, chain_head_hash_at_anchor,
            timestamp_token_bytes, tsa_identifier, tsa_status, created_at_utc)
         VALUES ('anc-1', ?, 'sess-7', 'session_close', 'deadbeef', ?, 'tsa.example', 'ok',
                 '2026-07-31T00:00:00Z')",
        duckdb::params![TENANT, vec![7u8; 8]],
    )
    .unwrap();
    conn.close().unwrap();
}

/// Migrate a freshly-seeded fixture and return `(dir, duckdb, sqlite)`.
fn crossed(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch(tag);
    let db = seed(&dir);
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.purchasing.purchase_orders, PO_CASES.len() as u64);
    assert_eq!(
        out.purchasing.purchase_order_lines,
        (POL_CASES.len() + SWEPT_ROWS) as u64
    );
    assert_eq!(
        out.purchasing.purchase_order_receipts,
        POR_CASES.len() as u64
    );
    assert_eq!(out.purchasing.po_number_state, PO_NUMBER_CASES.len() as u64);
    (dir, db, lite)
}

fn sqlite_text(lite: &Path, sql: &str) -> Option<String> {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
        .unwrap()
}

fn sqlite_i64(lite: &Path, sql: &str) -> i64 {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap()
}

fn sqlite_bool(lite: &Path, sql: &str) -> bool {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(sql, [], |r| r.get::<_, bool>(0)).unwrap()
}

fn sqlite_typeof(lite: &Path, table: &str, col: &str, where_clause: &str) -> String {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(
        &format!("SELECT typeof({col}) FROM {table} WHERE {where_clause}"),
        [],
        |r| r.get::<_, String>(0),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// 1. The headline
// ---------------------------------------------------------------------------

/// All four tables cross, the gate passes, and every column read back from
/// SQLite is the value DuckDB held.
///
/// The read-back is done here as well as inside the gate: the gate compares the
/// two sides against each other, whereas the assertions below compare SQLite
/// against the literal constants the fixture was built from — so two sides that
/// were wrong in the same way would still be caught.
#[test]
fn the_purchasing_family_crosses_with_zero_drift() {
    let (_dir, db, lite) = crossed("headline");

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops.is_empty(),
        "the purchasing family must cross clean: {:?}",
        r.hard_stops
    );
    for table in FAMILY {
        assert!(
            r.checks
                .iter()
                .any(|c| c.contains(&format!("every {table} column round-trips with ZERO drift"))),
            "the ZERO-drift check for {table} must be emitted; checks: {:?}",
            r.checks
        );
    }

    // --- purchase_orders, against the fixture's own literals ---
    for (po_id, state, subtotal, vat_pct, vat, total, populated, notes) in PO_CASES {
        let get = |col: &str| {
            sqlite_text(
                &lite,
                &format!("SELECT {col} FROM purchase_orders WHERE po_id = '{po_id}'"),
            )
        };
        let num = |col: &str| {
            sqlite_i64(
                &lite,
                &format!("SELECT {col} FROM purchase_orders WHERE po_id = '{po_id}'"),
            )
        };
        assert_eq!(get("state").as_deref(), Some(*state));
        assert_eq!(get("currency").as_deref(), Some("HUF"));
        // §3.2 A — money, exact, including both i64 extremes.
        assert_eq!(num("subtotal_minor"), *subtotal, "subtotal on {po_id}");
        assert_eq!(num("vat_rate_pct"), *vat_pct, "vat_rate_pct on {po_id}");
        assert_eq!(num("vat_minor"), *vat, "vat_minor on {po_id}");
        assert_eq!(num("total_minor"), *total, "total_minor on {po_id}");
        // The nullable columns, in both states.
        assert_eq!(get("vendor_avl_status").is_some(), *populated);
        assert_eq!(get("issued_at_utc").is_some(), *populated);
        assert_eq!(get("approved_by_operator").is_some(), *populated);
        // The empty string is a value, not a NULL. `po-01` is the row that
        // distinguishes them and the `typeof` sweep cannot.
        assert_eq!(get("notes").as_deref(), Some(*notes), "notes on {po_id}");
    }

    // --- purchase_order_lines, against the fixture's own literals ---
    for (pol_id, po_id, seq, product_id, description, qty, unit, total, heat, received) in POL_CASES
    {
        let at =
            |col: &str| format!("SELECT {col} FROM purchase_order_lines WHERE pol_id = '{pol_id}'");
        assert_eq!(sqlite_text(&lite, &at("po_id")).as_deref(), Some(*po_id));
        assert_eq!(
            sqlite_text(&lite, &at("product_id")).as_deref(),
            *product_id
        );
        assert_eq!(
            sqlite_text(&lite, &at("description")).as_deref(),
            Some(*description)
        );
        assert_eq!(sqlite_i64(&lite, &at("seq")), *seq);
        assert_eq!(sqlite_i64(&lite, &at("quantity")), *qty);
        assert_eq!(sqlite_i64(&lite, &at("unit_price_minor")), *unit);
        assert_eq!(sqlite_i64(&lite, &at("line_total_minor")), *total);
        assert_eq!(sqlite_i64(&lite, &at("received_quantity")), *received);
        // §3.2 H — both arms.
        assert_eq!(
            sqlite_bool(&lite, &at("expected_heat_lot_required")),
            *heat,
            "the heat-lot flag on {pol_id}"
        );
    }

    // --- purchase_order_receipts ---
    for (por_id, pol_id, received, pass, notes, heat_lot, ncr_id) in POR_CASES {
        let at = |col: &str| {
            format!("SELECT {col} FROM purchase_order_receipts WHERE por_id = '{por_id}'")
        };
        assert_eq!(sqlite_text(&lite, &at("pol_id")).as_deref(), Some(*pol_id));
        assert_eq!(sqlite_i64(&lite, &at("received_quantity")), *received);
        assert_eq!(
            sqlite_text(&lite, &at("inspection_notes")).as_deref(),
            Some(*notes)
        );
        assert_eq!(
            sqlite_text(&lite, &at("heat_lot_assigned")).as_deref(),
            *heat_lot
        );
        assert_eq!(sqlite_text(&lite, &at("ncr_id")).as_deref(), *ncr_id);
        // The family's load-bearing boolean: `false` is what mints the auto-NCR.
        assert_eq!(
            sqlite_bool(&lite, &at("inspection_pass")),
            *pass,
            "inspection_pass on {por_id}"
        );
    }

    // --- po_number_state ---
    for (tenant, year, next, updated) in PO_NUMBER_CASES {
        let at = |col: &str| {
            format!(
                "SELECT {col} FROM po_number_state WHERE tenant_id = '{tenant}' AND year = {year}"
            )
        };
        assert_eq!(sqlite_i64(&lite, &at("next_number")), *next);
        assert_eq!(
            sqlite_text(&lite, &at("updated_at")).as_deref(),
            Some(*updated)
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The composite key
// ---------------------------------------------------------------------------

/// **One tenant with two years, and one year under two tenants — all three
/// buckets survive.**
///
/// A migrator that keyed its `BTreeMap` on `tenant_id` alone would collapse
/// `test`'s 2025 and 2026 buckets; one that keyed on `year` alone would collapse
/// `test`'s and `other-tenant`'s 2026 buckets. In either case every other check
/// stays green: the row count matches, the `typeof` sweep passes, and the
/// surviving row is compared twice. This is the pin that says the key is both.
///
/// And the stakes are not abstract. `reserve_po_number` (`purchasing.rs:569`)
/// reads this bucket with `rows.next()` — it takes the *first* row and advances
/// that one. Two rows for one `(tenant, year)` means two counters issuing the
/// same `PO-YYYY-NNNN`, the shape v2.33.4 fixed on invoice numbers.
#[test]
fn the_composite_key_really_is_composite() {
    let (_dir, _db, lite) = crossed("composite");

    let bucket = |tenant: &str, year: i64| {
        sqlite_i64(
            &lite,
            &format!(
                "SELECT next_number FROM po_number_state \
                 WHERE tenant_id = '{tenant}' AND year = {year}"
            ),
        )
    };
    assert_eq!(bucket(TENANT, 2025), 12, "test's 2025 bucket must survive");
    assert_eq!(
        bucket(TENANT, 2026),
        4,
        "test's 2026 bucket must survive ALONGSIDE its 2025 one"
    );
    assert_eq!(
        bucket(OTHER, 2026),
        1,
        "other-tenant's 2026 bucket must survive ALONGSIDE test's 2026 one"
    );

    assert_eq!(
        sqlite_i64(
            &lite,
            "SELECT count(*) FROM po_number_state WHERE year = 2026"
        ),
        2,
        "two tenants hold a 2026 bucket; a year-only key would have left one"
    );
    assert_eq!(
        sqlite_i64(
            &lite,
            &format!("SELECT count(*) FROM po_number_state WHERE tenant_id = '{TENANT}'")
        ),
        2,
        "one tenant holds two years; a tenant-only key would have left one"
    );
}

// ---------------------------------------------------------------------------
// 3. §3.2 A, F and H — the family's numbers
// ---------------------------------------------------------------------------

/// Money and counts cross as `'integer'` with their values intact over the whole
/// `i64` range; the two booleans cross as `'integer'` with both arms intact;
/// every other column crosses as `'text'`.
///
/// A `'text'` money column would order and compare lexicographically and would
/// silently coerce to `REAL` in any later SQL arithmetic — the exact class R1
/// exists to close. A `'text'` boolean would be a `"true"` that every Rust
/// comparison reads as `false`.
#[test]
fn money_counts_and_booleans_cross_as_integer_and_everything_else_as_text() {
    let (_dir, db, lite) = crossed("typing");

    for col in ["subtotal_minor", "vat_rate_pct", "vat_minor", "total_minor"] {
        assert_eq!(
            sqlite_typeof(&lite, "purchase_orders", col, "po_id = 'po-01'"),
            "integer",
            "{col} is §3.2 A/F money-or-count and must be INTEGER"
        );
    }
    // The extremes, exact.
    assert_eq!(
        sqlite_i64(
            &lite,
            "SELECT total_minor FROM purchase_orders WHERE po_id = 'po-91-max'"
        ),
        i64::MAX,
        "§3.2 A says the representation is unchanged; i64::MAX is what makes that a measurement"
    );
    assert_eq!(
        sqlite_i64(
            &lite,
            "SELECT total_minor FROM purchase_orders WHERE po_id = 'po-90-min'"
        ),
        i64::MIN
    );

    assert_eq!(
        sqlite_typeof(
            &lite,
            "purchase_order_lines",
            "expected_heat_lot_required",
            "pol_id = 'pol-02'"
        ),
        "integer",
        "§3.2 H: BOOLEAN → INTEGER"
    );
    assert_eq!(
        sqlite_typeof(
            &lite,
            "purchase_order_receipts",
            "inspection_pass",
            "por_id = 'por-02'"
        ),
        "integer"
    );
    assert_eq!(
        sqlite_typeof(&lite, "purchase_orders", "notes", "po_id = 'po-01'"),
        "text",
        "the empty string is TEXT, not NULL"
    );
    assert_eq!(
        sqlite_typeof(&lite, "po_number_state", "year", "year = 2025"),
        "integer"
    );

    // And the gate says the same thing over EVERY row rather than the one this
    // test picked, plus the Σ folds §6.3 requires per money column.
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    for want in [
        "typeof(purchase_orders.total_minor) = 'integer'",
        "typeof(purchase_order_lines.expected_heat_lot_required) = 'integer'",
        "typeof(purchase_order_receipts.inspection_pass) = 'integer'",
        "Σ purchase_orders.subtotal_minor",
        "Σ purchase_orders.vat_minor",
        "Σ purchase_orders.total_minor",
        "Σ purchase_order_lines.unit_price_minor",
        "Σ purchase_order_lines.line_total_minor",
    ] {
        assert!(
            r.checks.iter().any(|c| c.contains(want)),
            "the gate must emit {want:?}; checks: {:?}",
            r.checks
        );
    }
}

// ---------------------------------------------------------------------------
// 4 + 5. The disjunction: exact round-trip OR a loud refusal
// ---------------------------------------------------------------------------

/// **Every natural key is either accepted and round-trips byte-identically, or
/// refused loudly naming the table and the key. Both arms are required to
/// fire.**
///
/// This family has no R2 column and no float, so the disjunction is not about a
/// *value* representation — it is about *identity*. `unique_natural_keys` is the
/// only refusal in the family, and on `po_number_state` it is the only thing
/// between a duplicated allocator bucket and a green gate.
///
/// The adversarial table below is measured, not asserted from the docs:
///
/// | input | arm | why |
/// |---|---|---|
/// | distinct keys | accept | the ordinary case |
/// | an adjacent duplicate | **refuse** | `ORDER BY` puts duplicates next to each other |
/// | a non-adjacent duplicate | **refuse** | a `BTreeSet`, not a peek at the previous row |
/// | keys differing only in the year | accept | the second component is load-bearing |
/// | keys differing only in the tenant | accept | the first component is load-bearing |
/// | `2025` vs `20250` | accept | the join is `#`-delimited, not a concatenation |
/// | `""` vs a real key | accept | the empty key is a key, not a wildcard |
#[test]
fn every_carried_natural_key_either_round_trips_or_is_refused() {
    let mut accepted = 0usize;
    let mut refused = 0usize;

    let cases: &[(&[&str], bool)] = &[
        (&["test#2025", "test#2026", "other#2026"], true),
        (&["test#2026", "test#2026"], false),
        (&["test#2025", "test#2026", "z#9", "test#2025"], false),
        (&["test#2025", "test#2026", "test#2027"], true),
        (&["test#2026", "other#2026"], true),
        (&["test#2025", "test#20250"], true),
        (&["", "test#2026"], true),
        (&["", ""], false),
        // The three ULID-keyed tables' shape, for completeness: one refusal
        // function serves all four tables.
        (&["test#po-01", "test#po-02"], true),
        (&["test#po-01", "test#po-01"], false),
    ];

    for (keys, ok) in cases {
        let owned: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
        match unique_natural_keys(&owned, "po_number_state") {
            Ok(()) => {
                assert!(ok, "{keys:?} must have been refused");
                accepted += 1;
            }
            Err(e) => {
                assert!(!ok, "{keys:?} must have been accepted, got {e}");
                let msg = e.to_string();
                assert!(
                    msg.contains("po_number_state"),
                    "must name the table: {msg}"
                );
                // The duplicated key itself must be in the message — an operator
                // needs the row, not a count.
                let dup = keys
                    .iter()
                    .enumerate()
                    .find(|(i, k)| keys[..*i].contains(k))
                    .map(|(_, k)| *k)
                    .unwrap();
                assert!(
                    msg.contains(&format!("natural key {dup}")),
                    "must name the duplicated key {dup:?}: {msg}"
                );
                refused += 1;
            }
        }
    }

    // **Both arms fired.** A disjunction test in which one arm never runs is a
    // single-arm test wearing a disjunction's name.
    assert!(accepted >= 6, "the accept arm must fire: {accepted}");
    assert!(refused >= 3, "the refuse arm must fire: {refused}");
}

/// The same disjunction proved through **real storage**: 256 generated PO lines
/// seeded into a real DuckDB file, carried by the real migrator into a real
/// SQLite file, and read back — text byte-for-byte, money and quantity
/// value-for-value, booleans both ways.
///
/// This is the pin the unit test above cannot be: it exercises DuckDB's
/// `VARCHAR`/`BIGINT`/`BOOLEAN` reads, the `ToSql` binds, `STRICT`'s acceptance,
/// and SQLite's read-back — four layers where a value could be normalised,
/// truncated, re-encoded or coerced without any in-memory check noticing.
#[test]
fn every_swept_row_survives_real_storage_byte_for_byte() {
    type Swept = (i64, Option<String>, Option<String>, i64, i64, i64, bool);
    const SQL: &str = "SELECT seq, product_id, description, quantity, unit_price_minor, \
                       line_total_minor, expected_heat_lot_required FROM purchase_order_lines \
                       WHERE po_id = 'po-sweep' ORDER BY seq ASC";

    let (_dir, db, lite) = crossed("sweep");

    let duck: Vec<Swept> = {
        let conn = duckdb::Connection::open(&db).unwrap();
        let mut stmt = conn.prepare(SQL).unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(stmt);
        conn.close().unwrap();
        rows
    };
    assert_eq!(duck.len(), SWEPT_ROWS, "the sweep must actually be seeded");

    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let mut stmt = conn.prepare(SQL).unwrap();
    let lite_rows: Vec<Swept> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, bool>(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(lite_rows.len(), SWEPT_ROWS);
    for (i, (d, l)) in duck.iter().zip(lite_rows.iter()).enumerate() {
        assert_eq!(d.0, l.0, "seq drifted on swept row {i}");
        assert_eq!(
            d.1.as_deref().map(str::as_bytes),
            l.1.as_deref().map(str::as_bytes),
            "product_id is not byte-identical on swept row {i}: {:?} vs {:?}",
            d.1,
            l.1
        );
        assert_eq!(
            d.2.as_deref().map(str::as_bytes),
            l.2.as_deref().map(str::as_bytes),
            "description is not byte-identical on swept row {i}: {:?} vs {:?}",
            d.2,
            l.2
        );
        assert_eq!(d.3, l.3, "quantity drifted on swept row {i}");
        assert_eq!(d.4, l.4, "unit_price_minor drifted on swept row {i}");
        assert_eq!(d.5, l.5, "line_total_minor drifted on swept row {i}");
        assert_eq!(d.6, l.6, "the heat-lot flag drifted on swept row {i}");
    }
    // The empty string really was swept, and it came back as `""` and not as
    // `NULL` — the distinction `STRICT` and the typeof sweep are both blind to.
    assert!(
        lite_rows.iter().any(|r| r.1.as_deref() == Some("")),
        "the sweep must include an empty-string product_id"
    );
    // Negative money really was swept: a credit line against a PO is a negative
    // line total, and a carry that assumed money is non-negative would pass a
    // fixture that only had positives.
    assert!(
        lite_rows.iter().any(|r| r.5 < 0),
        "the sweep must include a negative line total"
    );
    // And both boolean arms really were swept.
    assert!(lite_rows.iter().any(|r| r.6));
    assert!(lite_rows.iter().any(|r| !r.6));
}

// ---------------------------------------------------------------------------
// 6. The refusal, through the real migrator
// ---------------------------------------------------------------------------

/// **A duplicate natural key fails the whole carry.**
///
/// Reachable from a real DuckDB source precisely because no purchasing table has
/// a `PRIMARY KEY`, a `UNIQUE` or an index (`purchasing.rs:169–171`). It is
/// seeded on `po_number_state` because that is the table where a duplicate does
/// more than blind the gate: `reserve_po_number` takes the first matching row,
/// so two rows for one `(tenant, year)` are two counters issuing the same PO
/// number.
#[test]
fn a_duplicate_natural_key_fails_the_migration() {
    let dir = scratch("dupkey");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        // Same (tenant_id, year) as the fixture's 2026 bucket, with a different
        // counter — two allocators claiming one year.
        conn.execute(
            "INSERT INTO po_number_state (tenant_id, year, next_number, updated_at)
             VALUES (?, ?, ?, ?)",
            duckdb::params![TENANT, 2026_i64, 99_i64, "2026-02-02T09:00:00Z"],
        )
        .unwrap();
        conn.close().unwrap();
    }

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a duplicate natural key must fail the migration");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("po_number_state"),
        "must name the table: {msg}"
    );
    assert!(
        msg.contains(&format!("{TENANT}#2026")),
        "must name the duplicated key: {msg}"
    );
    assert!(
        msg.contains("no PRIMARY KEY"),
        "must say why nothing stopped it: {msg}"
    );
}

/// The same refusal on a ULID-keyed table, so the guard is not shown only on the
/// one table whose key is genuinely composite.
#[test]
fn a_duplicate_po_id_also_fails_the_migration() {
    let dir = scratch("duppo");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute(
            "INSERT INTO purchase_orders
               (po_id, tenant_id, po_number, vendor_partner_id, currency, subtotal_minor,
                vat_rate_pct, vat_minor, total_minor, state, vendor_avl_status, issued_at_utc,
                expected_delivery_utc, notes, requested_by_operator, approved_by_operator,
                approved_at_utc, created_at_utc)
             VALUES ('po-01', ?, 'PO-2026-XXXX', 'prt-vendor-02', 'EUR', 1, 27, 0, 1, 'draft',
                     NULL, NULL, NULL, 'duplicate', 'op', NULL, NULL, '2026-02-01T09:00:00Z')",
            duckdb::params![TENANT],
        )
        .unwrap();
        conn.close().unwrap();
    }

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a duplicate po_id must fail the migration");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("purchase_orders"),
        "must name the table: {msg}"
    );
    assert!(
        msg.contains(&format!("{TENANT}#po-01")),
        "must name the duplicated key: {msg}"
    );
}

// ---------------------------------------------------------------------------
// 7. The schema
// ---------------------------------------------------------------------------

/// `ensure_purchasing_schema` builds all four tables, is idempotent, and
/// declares every column with the §3.2 vocabulary.
#[test]
fn ensure_purchasing_schema_builds_all_four_tables_and_is_idempotent() {
    let dir = scratch("schema");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    aberp::migrate_purchasing::ensure_purchasing_schema(&conn).unwrap();
    // Twice: `CREATE TABLE IF NOT EXISTS` is the posture the source schema uses,
    // and a boot that re-runs it must be a no-op rather than an error.
    aberp::migrate_purchasing::ensure_purchasing_schema(&conn).unwrap();

    for table in FAMILY {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{table} must exist exactly once");

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sql.contains("STRICT"), "{table} must be STRICT: {sql}");
        for banned in ["VARCHAR", "DECIMAL", "DOUBLE", "BOOLEAN", "BIGINT"] {
            assert!(
                !sql.contains(banned),
                "{table} must not declare {banned}: {sql}"
            );
        }
        // `purchasing.rs:169-171`'s posture: no surrogate key, no CHECK, no
        // DEFAULT, no index. Porting any of them here would fork the two
        // schemas and delete `unique_natural_keys`'s reason to exist.
        for banned in ["PRIMARY KEY", "UNIQUE", "CHECK", "DEFAULT"] {
            assert!(
                !sql.contains(banned),
                "the DuckDB {table} declares no {banned}: {sql}"
            );
        }
        let idx: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = ? \
                 AND sql IS NOT NULL",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            idx, 0,
            "the DuckDB {table} has no index; creating one here would fork the two schemas"
        );
    }

    // A STRICT table must refuse a float into a money column — the one typing
    // guarantee this family gets from the engine rather than from Rust.
    let bad = conn.execute(
        "INSERT INTO purchase_orders (po_id, tenant_id, po_number, vendor_partner_id, currency, \
         subtotal_minor, vat_rate_pct, vat_minor, total_minor, state, notes, \
         requested_by_operator, created_at_utc) \
         VALUES ('x', 't', 'n', 'v', 'HUF', 1.5, 27, 0, 0, 'draft', '', 'op', 'now')",
        [],
    );
    assert!(
        bad.is_err(),
        "STRICT must refuse a non-integral REAL in an INTEGER money column"
    );
}

// ---------------------------------------------------------------------------
// 8 + 9. The gate's teeth
// ---------------------------------------------------------------------------

/// The gate **hard-stops** per table when a table was not carried.
///
/// Mutation-shaped: this is what the gate does if a future edit drops one arm of
/// the carry from `carry_purchasing`.
#[test]
fn the_gate_hard_stops_per_table_when_a_table_was_not_carried() {
    for table in FAMILY {
        let (_dir, db, lite) = crossed("notcarried");
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            conn.execute_batch(&format!("DROP TABLE {table}")).unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.starts_with(table) && s.contains("exists in DuckDB but NOT in SQLite")),
            "dropping {table} must be named by the gate; it reported {:?}",
            r.hard_stops
        );
    }
}

/// **A single changed column on a single row reds the gate — for every mutable
/// column of `purchase_orders`, one column at a time.** A gate that has never
/// been shown to fail is not a gate (ADR-0107 §4.1).
///
/// `tenant_id` and `po_id` are excluded because they *are* the key; mutating one
/// produces a missing-row hard stop instead, which the last block below pins
/// separately.
///
/// **The money columns are mutated by ONE minor unit**, not by a large amount.
/// One forint is the smallest drift this family can suffer and the one a
/// tolerance-shaped comparison would wave through; it is also enough to move the
/// Σ fold, so both arms of the gate are exercised on the same mutation.
#[test]
fn a_single_changed_column_on_a_purchase_order_reds_the_gate() {
    /// `(column, the row it is populated on, the value to set)`.
    const MUTABLE: &[(&str, &str, &str)] = &[
        ("po_number", "po-01", "'TAMPERED'"),
        ("vendor_partner_id", "po-01", "'TAMPERED'"),
        ("currency", "po-01", "'EUR'"),
        ("state", "po-01", "'TAMPERED'"),
        ("notes", "po-02", "'TAMPERED'"),
        ("requested_by_operator", "po-01", "'TAMPERED'"),
        ("created_at_utc", "po-01", "'TAMPERED'"),
        // The nullable ones, cleared — the value → NULL arm.
        ("vendor_avl_status", "po-02", "NULL"),
        ("issued_at_utc", "po-02", "NULL"),
        ("expected_delivery_utc", "po-02", "NULL"),
        ("approved_by_operator", "po-02", "NULL"),
        ("approved_at_utc", "po-02", "NULL"),
        // §3.2 A / F — one minor unit, one percentage point.
        ("subtotal_minor", "po-01", "100001"),
        ("vat_rate_pct", "po-01", "28"),
        ("vat_minor", "po-01", "27001"),
        ("total_minor", "po-01", "127001"),
    ];

    for (col, row, set) in MUTABLE {
        let (_dir, db, lite) = crossed("drift-po");
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            // The mutation must actually change a row; a no-op UPDATE would make
            // the assertion below vacuous.
            let before = sqlite_i64(
                &lite,
                &format!(
                    "SELECT count(*) FROM purchase_orders WHERE po_id = '{row}' \
                     AND {col} IS NOT NULL"
                ),
            );
            assert_eq!(before, 1, "{col} must be populated on {row} to be mutated");
            conn.execute_batch(&format!(
                "UPDATE purchase_orders SET {col} = {set} WHERE po_id = '{row}'"
            ))
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains(&format!("purchase order {TENANT}#{row}: {col}"))),
            "the gate must name the composite key and the column {col}; it reported {:?}",
            r.hard_stops
        );
        assert!(
            !r.checks
                .iter()
                .any(|c| c.contains("every purchase_orders column round-trips with ZERO drift")),
            "the ZERO-drift check must NOT be emitted alongside a drift hard stop"
        );
    }

    // The other direction: `po-01`'s NULLs become values. A comparison written
    // as "both non-null and different" would miss every one of these.
    for col in ["vendor_avl_status", "issued_at_utc", "approved_at_utc"] {
        let (_dir, db, lite) = crossed("drift-po-value");
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            conn.execute_batch(&format!(
                "UPDATE purchase_orders SET {col} = 'INVENTED' WHERE po_id = 'po-01'"
            ))
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains(&format!("purchase order {TENANT}#po-01: {col}"))),
            "a NULL that became a value must red the gate on {col}: {:?}",
            r.hard_stops
        );
    }

    // The empty string turning into a NULL — the drift `STRICT` and the typeof
    // sweep are both structurally blind to.
    //
    // It has to be pinned on a **nullable** column, and that is a finding rather
    // than a detail: the first draft of this test cleared `purchase_orders.notes`
    // and the mutation could not be applied at all, because `notes` is
    // `NOT NULL` on **both** engines. A `""`/`NULL` confusion is unreachable
    // there — the DDL already forbids one of the two states. Where it IS
    // reachable is a nullable column that legitimately holds `""`, and the sweep
    // seeds exactly one: `product_id` on every eighth swept line.
    let (_dir, db, lite) = crossed("drift-po-empty");
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT product_id FROM purchase_order_lines WHERE pol_id = 'pol-sweep-0000'"
        )
        .as_deref(),
        Some(""),
        "the pin needs a nullable column actually holding the empty string"
    );
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(
            "UPDATE purchase_order_lines SET product_id = NULL WHERE pol_id = 'pol-sweep-0000'",
        )
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains(&format!("PO line {TENANT}#pol-sweep-0000: product_id"))),
        "\"\" → NULL must red the gate; nothing else in the set can see it: {:?}",
        r.hard_stops
    );

    // And a mutated KEY component surfaces as a missing row rather than as a
    // column drift — the one shape the loop above cannot produce.
    let (_dir, db, lite) = crossed("drift-po-key");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch("UPDATE purchase_orders SET po_id = 'po-99' WHERE po_id = 'po-01'")
            .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains(&format!("purchase order {TENANT}#po-01 is missing"))),
        "a mutated key component must surface as a missing row: {:?}",
        r.hard_stops
    );
}

/// The same, over every mutable column of the other three tables — including
/// **both booleans**, flipped in both directions.
///
/// Included rather than assumed-by-symmetry: the booleans are the columns the
/// `typeof` sweep is blindest to, since `0` and `1` are both `'integer'`, and
/// `inspection_pass` is the one whose value decides whether an auto-NCR exists.
#[test]
fn a_single_changed_column_on_the_other_tables_reds_the_gate() {
    /// `(table, noun, key column, key value, column, value to set)`.
    const MUTABLE: &[(&str, &str, &str, &str, &str, &str)] = &[
        // purchase_order_lines
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "po_id",
            "'TAMPERED'",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "seq",
            "9",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "product_id",
            "NULL",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "description",
            "'TAMPERED'",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "quantity",
            "6",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "unit_price_minor",
            "50001",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "currency",
            "'EUR'",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "line_total_minor",
            "250001",
        ),
        // §3.2 H, both directions: pol-02 is `true`, pol-01 is `false`.
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "expected_heat_lot_required",
            "0",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-01",
            "expected_heat_lot_required",
            "1",
        ),
        (
            "purchase_order_lines",
            "PO line",
            "pol_id",
            "pol-02",
            "received_quantity",
            "4",
        ),
        // purchase_order_receipts
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-01",
            "po_id",
            "'TAMPERED'",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-01",
            "pol_id",
            "'TAMPERED'",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-01",
            "received_at_utc",
            "'TAMPERED'",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-01",
            "received_by_operator",
            "'TAMPERED'",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-01",
            "delivery_note_number",
            "'TAMPERED'",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-02",
            "inspection_notes",
            "'TAMPERED'",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-01",
            "heat_lot_assigned",
            "NULL",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-02",
            "ncr_id",
            "NULL",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-01",
            "received_quantity",
            "4",
        ),
        // The family's load-bearing boolean, both directions.
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-01",
            "inspection_pass",
            "0",
        ),
        (
            "purchase_order_receipts",
            "PO receipt",
            "por_id",
            "por-02",
            "inspection_pass",
            "1",
        ),
        // po_number_state — `updated_at` and the counter itself. `tenant_id` and
        // `year` are the key.
        (
            "po_number_state",
            "PO number bucket",
            "year",
            "2025",
            "next_number",
            "13",
        ),
        (
            "po_number_state",
            "PO number bucket",
            "year",
            "2025",
            "updated_at",
            "'TAMPERED'",
        ),
    ];

    for (table, noun, key_col, key_val, col, set) in MUTABLE {
        let (_dir, db, lite) = crossed("drift-other");
        let quoted = if key_col == &"year" {
            key_val.to_string()
        } else {
            format!("'{key_val}'")
        };
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            let before = sqlite_i64(
                &lite,
                &format!(
                    "SELECT count(*) FROM {table} WHERE tenant_id = '{TENANT}' \
                     AND {key_col} = {quoted} AND {col} IS NOT NULL"
                ),
            );
            assert_eq!(
                before, 1,
                "{col} must be populated on {table}.{key_col} = {key_val} to be mutated"
            );
            conn.execute_batch(&format!(
                "UPDATE {table} SET {col} = {set} \
                 WHERE tenant_id = '{TENANT}' AND {key_col} = {quoted}"
            ))
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains(&format!("{noun} {TENANT}#{key_val}: {col}"))),
            "the gate must name {noun} {TENANT}#{key_val} and the column {col}; it reported {:?}",
            r.hard_stops
        );
        assert!(
            !r.checks
                .iter()
                .any(|c| c.contains(&format!("every {table} column round-trips with ZERO drift"))),
            "the ZERO-drift check for {table} must NOT be emitted alongside a drift hard stop"
        );
    }
}

// ---------------------------------------------------------------------------
// 10 + 11. The legitimate and the partial shapes
// ---------------------------------------------------------------------------

/// A source with none of the family's tables is a legitimate shape, and the gate
/// says so out loud rather than staying silent.
#[test]
fn a_source_without_the_family_reports_the_absence_rather_than_staying_silent() {
    let dir = scratch("absent");
    let db = dir.join("aberp.duckdb");
    seed_ledger(&db);

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.purchasing, Default::default());

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a source in which no purchase order was ever raised is legitimate: {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("purchasing family absent on BOTH sides")),
        "the absence must be REPORTED, not silent; checks: {:?}",
        r.checks
    );
}

/// **The per-table presence hold works: a source missing one table still crosses
/// the other three, and the missing one is not silently created.**
///
/// Unlike Parts C–F, a partial source is not a shape this product can produce —
/// one module builds all four tables in one batch. The hold is here because it
/// is what makes the gate's per-table hard stop above meaningful: a carry that
/// created an empty SQLite table for one the source lacks would manufacture
/// exactly the asymmetry the gate exists to detect.
#[test]
fn a_source_missing_one_table_still_crosses_the_other_three() {
    let dir = scratch("partial");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch("DROP TABLE purchase_order_receipts;")
            .unwrap();
        conn.close().unwrap();
    }

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.purchasing.purchase_orders, PO_CASES.len() as u64);
    assert_eq!(out.purchasing.purchase_order_receipts, 0);
    assert_eq!(out.purchasing.po_number_state, PO_NUMBER_CASES.len() as u64);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a symmetric absence is not an asymmetry: {:?}",
        r.hard_stops
    );

    // And the absent table really is absent, not silently created.
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' \
             AND name = 'purchase_order_receipts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "creating an empty purchase_order_receipts the source does not have would manufacture \
         the asymmetry the gate exists to detect"
    );
}
