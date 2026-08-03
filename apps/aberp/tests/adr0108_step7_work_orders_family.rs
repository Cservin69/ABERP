//! ADR-0108 Step 7 Part D — **the work-orders / BOM family crosses.**
//!
//! (Ervin's brief called this "Part B"; B is partners and C is
//! products/inventory, both already on this branch. Named Part D rather than
//! silently renumbered.)
//!
//! Nine pins, in the order they defend:
//!
//! 1. all four tables cross and **no column drifts**, asserted against the
//!    fixture's own literals as well as through the gate;
//! 2. **`routings.est_cost_huf` — the tree's one money column that is R2 rather
//!    than R1 — is `TEXT` on the SQLite side, not `REAL` and not `INTEGER`**,
//!    and it crosses byte-verbatim including DuckDB's `DECIMAL(18,2)` trailing
//!    zeros;
//! 3. the two R2 quantities (`work_orders.qty_target`, `boms.qty_per_unit`)
//!    cross byte-verbatim too;
//! 4. **the disjunction sweep** — 4096 generated decimal renderings plus the
//!    adversarial table, every one either carried **byte-identically** or
//!    refused **loudly** naming the column and the row. Both arms are required
//!    to fire;
//! 5. a value the SQLite read path could not restore **fails the carry** rather
//!    than crossing as an opaque string;
//! 6. `work_orders.actual_machining_minutes` is `'real'` and bit-identical —
//!    §3.2 E's float, carried unchanged and *proved* unchanged;
//! 7. `ensure_work_orders_schema` lands both ladders and is idempotent (M8);
//! 8. the gate **hard-stops** per table when a table was not carried;
//! 9. the per-row equality arm is **shown to go red** on a single changed column
//!    on a single row — separately for the money column, for a quantity, and for
//!    an ordinary `TEXT` column that no fold and no `typeof` sweep can see.
//!
//! **This test pins no §3.4 fold, because this family owes none.** §3.4's seven
//! arithmetic sites contain no work-orders statement and §6.3's row for the
//! family says "row-by-row carry". The T-8 pending-fold register is unchanged by
//! this commit.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::migrate_billing::canonical_decimal;
use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};

const TENANT: &str = "test";

/// The family's four tables, in the order the migrator and the gate walk them.
const FAMILY: &[&str] = &["work_orders", "boms", "routings", "bom_revisions"];

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-workorders-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `(wo_id, wo_number, qty_target, state, actual_machining_minutes, bom_rev_id)`.
///
/// * **`wo-01`** — the ordinary released WO: a fractional `qty_target` that
///   renders with DuckDB's `DECIMAL(18,6)` trailing zeros (`"10.500000"`). It
///   must cross **verbatim**; re-rendering it here would make the migration's
///   trailing-zero behaviour depend on when the row happened to be written
///   (§3.2 C).
/// * **`wo-02`** — completed, with a **measured machining time that needs more
///   than the canonical quantity scale of 6** (`12.3456789`). This is the row
///   that proves §3.2 E's `REAL` call was the right one: carried as R2 it would
///   have *refused the migration*, on a column that is not money and carries no
///   exactness requirement.
/// * **`wo-03`** — the pre-ADR-0105 legacy row: `bom_rev_id` NULL, i.e.
///   "released against an unrevisioned BOM", the shape V003 deliberately does
///   **not** backfill and this migrator must not re-derive (B4).
/// * **`wo-04`** — on hold, with a non-ASCII `hold_reason` and no calibration
///   columns at all.
/// * **`wo-05`** — cancelled, `qty_target` at the widest scale the column
///   allows.
const WORK_ORDER_CASES: &[(&str, &str, &str, &str, Option<f64>, Option<&str>)] = &[
    (
        "wo-01",
        "WO-2026-0001",
        "10.5",
        "Released",
        None,
        Some("rev-01"),
    ),
    (
        "wo-02",
        "WO-2026-0002",
        "1",
        "Completed",
        Some(12.3456789),
        Some("rev-02"),
    ),
    ("wo-03", "WO-2026-0003", "250", "Started", Some(0.0), None),
    (
        "wo-04",
        "WO-2026-0004",
        "3.25",
        "OnHold",
        None,
        Some("rev-01"),
    ),
    (
        "wo-05",
        "WO-2026-0005",
        "0.000001",
        "Cancelled",
        Some(-0.0),
        None,
    ),
];

/// `(bom_line_id, product_id, component_id, qty_per_unit, retired, bom_rev_id)`.
///
/// A retired line is included on purpose: ADR-0062 §6 soft-retires BOM lines and
/// never deletes them, so a retired row is revision history and must cross like
/// any other.
const BOM_CASES: &[(&str, &str, &str, &str, bool, Option<&str>)] = &[
    ("bom-01", "prd-01", "prd-90", "2", false, Some("rev-01")),
    ("bom-02", "prd-01", "prd-91", "4.5", false, Some("rev-01")),
    ("bom-03", "prd-01", "prd-92", "0.125", true, Some("rev-01")),
    ("bom-04", "prd-02", "prd-90", "1", false, None),
];

/// `(routing_op_id, wo_id, sequence, op_name, est_time_min, est_cost_huf)`.
///
/// **`est_cost_huf` is the money column**, and the set is chosen so the gate has
/// something to lose: a scale-2 value, a whole-forint value, a negative (a
/// credit on a re-work op), a zero, and a NULL (no estimate captured).
const ROUTING_CASES: &[(&str, &str, i64, &str, Option<i64>, Option<&str>)] = &[
    ("rop-01", "wo-01", 1, "Sawing", Some(30), Some("12500.50")),
    ("rop-02", "wo-01", 2, "Turning", Some(120), Some("48000")),
    ("rop-03", "wo-01", 3, "Deburring", None, None),
    ("rop-04", "wo-02", 1, "Milling", Some(90), Some("-1500.25")),
    ("rop-05", "wo-03", 1, "Grinding", Some(0), Some("0")),
];

/// `(bom_rev_id, product_id, rev_number, author, reason, line_count)`.
const REVISION_CASES: &[(&str, &str, i64, &str, Option<&str>, i64)] = &[
    (
        "rev-01",
        "prd-01",
        1,
        "operator@example.test",
        Some("initial"),
        3,
    ),
    ("rev-02", "prd-01", 2, "mérnök@example.test", None, 3),
];

/// Seed a DEV-shaped DuckDB through the **real** `ensure_schema`, so the SQLite
/// side is compared against the schema the product actually builds rather than
/// against a hand-written copy of it.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");

    {
        let conn = duckdb::Connection::open(&db).unwrap();
        // V001 + V002 + V003 in one call — the family's single construction
        // path, and the reason its schema is compared statement for statement.
        aberp_work_orders::ensure_schema(&conn).unwrap();

        for (wo_id, wo_number, qty, state, minutes, rev) in WORK_ORDER_CASES {
            conn.execute(
                "INSERT INTO work_orders
                   (wo_id, tenant_id, wo_number, product_id, qty_target, state, created_at,
                    released_at, started_at, completed_at, cancelled_at, hold_reason, notes,
                    source_quote_id, actual_machining_minutes, bom_rev_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    wo_id,
                    TENANT,
                    wo_number,
                    if *wo_id == "wo-05" {
                        "prd-02"
                    } else {
                        "prd-01"
                    },
                    qty,
                    state,
                    "2026-01-01T00:00:00Z",
                    (*state != "Cancelled").then_some("2026-02-01T00:00:00Z"),
                    matches!(*state, "Started" | "Completed").then_some("2026-02-02T00:00:00Z"),
                    (*state == "Completed").then_some("2026-02-03T00:00:00Z"),
                    (*state == "Cancelled").then_some("2026-02-04T00:00:00Z"),
                    (*state == "OnHold").then_some("Anyaghiány — Árvíztűrő tükörfúrógép"),
                    (*wo_id == "wo-01").then_some("first batch"),
                    minutes.map(|_| "quote-77"),
                    minutes,
                    rev,
                ],
            )
            .unwrap();
        }

        for (bom_line_id, product_id, component_id, qty, retired, rev) in BOM_CASES {
            conn.execute(
                "INSERT INTO boms
                   (bom_line_id, tenant_id, product_id, component_id, qty_per_unit, created_at,
                    retired_at, bom_rev_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    bom_line_id,
                    TENANT,
                    product_id,
                    component_id,
                    qty,
                    "2026-01-01T00:00:00Z",
                    retired.then_some("2026-03-01T00:00:00Z"),
                    rev,
                ],
            )
            .unwrap();
        }

        for (routing_op_id, wo_id, sequence, op_name, est_time, est_cost) in ROUTING_CASES {
            conn.execute(
                "INSERT INTO routings
                   (routing_op_id, tenant_id, wo_id, sequence, op_name, est_time_min,
                    est_cost_huf, state, started_at, completed_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    routing_op_id,
                    TENANT,
                    wo_id,
                    sequence,
                    op_name,
                    est_time,
                    est_cost,
                    if *sequence == 1 { "Active" } else { "Pending" },
                    (*sequence == 1).then_some("2026-02-02T00:00:00Z"),
                    None::<&str>,
                ],
            )
            .unwrap();
        }

        for (bom_rev_id, product_id, rev_number, author, reason, line_count) in REVISION_CASES {
            conn.execute(
                "INSERT INTO bom_revisions
                   (bom_rev_id, tenant_id, product_id, rev_number, created_at, author, reason,
                    line_count, content_hash)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    bom_rev_id,
                    TENANT,
                    product_id,
                    rev_number,
                    "2026-01-01T00:00:00Z",
                    author,
                    reason,
                    line_count,
                    format!("fnv-{bom_rev_id}"),
                ],
            )
            .unwrap();
        }
        conn.close().unwrap();
    }

    seed_ledger(&db);
    db
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
    assert_eq!(out.work_orders.work_orders, WORK_ORDER_CASES.len() as u64);
    assert_eq!(out.work_orders.boms, BOM_CASES.len() as u64);
    assert_eq!(out.work_orders.routings, ROUTING_CASES.len() as u64);
    assert_eq!(out.work_orders.bom_revisions, REVISION_CASES.len() as u64);
    (dir, db, lite)
}

/// One column of one row on the SQLite side, as the storage class SQLite chose.
fn sqlite_text(lite: &Path, sql: &str) -> Option<String> {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
        .unwrap()
}

fn sqlite_typeof(lite: &Path, table: &str, col: &str, key_col: &str, key: &str) -> String {
    let conn = aberp_db::sqlite::open_hardened(lite).unwrap();
    conn.query_row(
        &format!("SELECT typeof({col}) FROM {table} WHERE {key_col} = ?"),
        [key],
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
fn the_work_orders_bom_family_crosses_with_zero_drift() {
    let (_dir, db, lite) = crossed("family");

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops.is_empty(),
        "the family must cross clean: {:?}",
        r.hard_stops
    );
    for table in FAMILY {
        assert!(
            r.checks
                .iter()
                .any(|c| c.contains(&format!("every {table} column round-trips with ZERO drift"))),
            "the gate must report {table}'s ZERO-drift check; it reported {:?}",
            r.checks
        );
    }

    // The four row counts, re-read from SQLite rather than taken from the carry.
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    for (table, want) in [
        ("work_orders", WORK_ORDER_CASES.len()),
        ("boms", BOM_CASES.len()),
        ("routings", ROUTING_CASES.len()),
        ("bom_revisions", REVISION_CASES.len()),
    ] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, want as i64, "{table} row count on the SQLite side");
    }
    drop(conn);

    // The non-ASCII hold reason and the legacy NULL `bom_rev_id` — the two rows
    // whose *shape* the migration must not normalise away.
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT hold_reason FROM work_orders WHERE wo_id = 'wo-04'"
        )
        .as_deref(),
        Some("Anyaghiány — Árvíztűrő tükörfúrógép")
    );
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT bom_rev_id FROM work_orders WHERE wo_id = 'wo-03'"
        ),
        None,
        "a pre-ADR-0105 WO stays unrevisioned; a backfill here would fabricate traceability"
    );
    // A soft-retired BOM line is history and crosses like any other.
    assert_eq!(
        sqlite_text(
            &lite,
            "SELECT retired_at FROM boms WHERE bom_line_id = 'bom-03'"
        )
        .as_deref(),
        Some("2026-03-01T00:00:00Z")
    );
}

// ---------------------------------------------------------------------------
// 2 + 3. The money column, and the two quantities
// ---------------------------------------------------------------------------

/// **`routings.est_cost_huf` is `TEXT`, and it crosses verbatim.**
///
/// This is the tree's one money column that does not follow R1 (§3.2 B's
/// conservative call: HUF has no subunit, the Rust type is `Option<Decimal>`,
/// and the value never reaches NAV, the PDF or the ledger). The exception is
/// only safe if the column really is exact, so the pin is on the storage class
/// **and** on the bytes.
#[test]
fn the_one_r2_money_column_is_text_and_crosses_verbatim() {
    let (_dir, _db, lite) = crossed("money");

    for (id, _, _, _, _, est_cost) in ROUTING_CASES {
        let got = sqlite_text(
            &lite,
            &format!("SELECT est_cost_huf FROM routings WHERE routing_op_id = '{id}'"),
        );
        match est_cost {
            None => assert_eq!(got, None, "{id}: a captured-no-estimate row stays NULL"),
            Some(want) => {
                // DuckDB's DECIMAL(18,2) read-back renders with the declared
                // scale; whatever it renders must cross byte-identically.
                let d: String = {
                    let conn = duckdb::Connection::open(&_db).unwrap();
                    conn.query_row(
                        &format!(
                            "SELECT CAST(est_cost_huf AS VARCHAR) FROM routings \
                             WHERE routing_op_id = '{id}'"
                        ),
                        [],
                        |r| r.get(0),
                    )
                    .unwrap()
                };
                assert_eq!(
                    got.as_deref(),
                    Some(d.as_str()),
                    "{id}: byte-verbatim carry"
                );
                assert_eq!(
                    rust_decimal::Decimal::from_str_exact(&d).unwrap(),
                    rust_decimal::Decimal::from_str_exact(want).unwrap(),
                    "{id}: and the value is the one the fixture wrote"
                );
                assert_eq!(
                    sqlite_typeof(&lite, "routings", "est_cost_huf", "routing_op_id", id),
                    "text",
                    "{id}: money on a REAL is F-6a's class arriving through §3.2 B's exception"
                );
            }
        }
    }
}

/// The two R2 quantities cross byte-verbatim, trailing zeros included.
#[test]
fn the_r2_quantities_cross_verbatim_with_their_trailing_zeros() {
    let (_dir, db, lite) = crossed("qty");

    for (table, key_col, key, col) in [
        ("work_orders", "wo_id", "wo-01", "qty_target"),
        ("boms", "bom_line_id", "bom-02", "qty_per_unit"),
    ] {
        let duck: String = {
            let conn = duckdb::Connection::open(&db).unwrap();
            conn.query_row(
                &format!("SELECT CAST({col} AS VARCHAR) FROM {table} WHERE {key_col} = '{key}'"),
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(
            duck.contains('.') && duck.ends_with('0'),
            "the fixture must actually exercise DuckDB's trailing zeros, got {duck:?}"
        );
        assert_eq!(
            sqlite_text(
                &lite,
                &format!("SELECT {col} FROM {table} WHERE {key_col} = '{key}'")
            )
            .as_deref(),
            Some(duck.as_str()),
            "{table}.{col} must cross verbatim — re-rendering would make the migration's \
             trailing-zero behaviour depend on when the row was written (§3.2 C)"
        );
        assert_eq!(
            sqlite_typeof(&lite, table, col, key_col, key),
            "text",
            "{table}.{col} is R2"
        );
    }
}

// ---------------------------------------------------------------------------
// 4 + 5. The disjunction
// ---------------------------------------------------------------------------

/// **Every value the carry meets either round-trips byte-identically or is
/// refused loudly.** Both arms are required to fire.
///
/// A deterministic sweep rather than a random one: the generator is a fixed
/// xorshift over a fixed seed, so a failure is reproducible from the test name
/// alone and CI cannot go green-then-red on the same code.
///
/// The property is a **disjunction**, and the second arm is the point. A test
/// that only asserted the happy arm would pass on a carry that quietly
/// re-rendered every value — normalising `"10.500000"` to `"10.5"` and making
/// the migration's output depend on the row's write date — or on one that
/// carried an unparseable string through as an opaque blob for the SQLite read
/// path to trip over later.
#[test]
fn every_carried_decimal_either_round_trips_byte_identically_or_is_refused() {
    let mut carried = 0usize;
    let mut refused = 0usize;

    // The adversarial table first: the shapes a money/quantity column actually
    // takes, and the shapes it must never accept.
    let named: &[(&str, bool)] = &[
        ("0", true),
        ("-0", true),
        ("0.00", true),
        ("1", true),
        ("10.5", true),
        ("10.500000", true),
        ("-1500.25", true),
        ("0.000001", true),
        ("79228162514264337593543950335", true), // Decimal::MAX
        ("", false),
        ("NaN", false),
        ("inf", false),
        ("-Infinity", false),
        ("1.2.3", false),
        ("12 500", false),
        // MEASURED, and it is a carry rather than a refusal: `Decimal::from_str`
        // accepts exponent form, so `canonical_decimal` does too and the string
        // crosses verbatim. That is the correct answer and not a hole — the
        // property R2 needs is that the SQLite read path can restore the value,
        // and `Decimal::from_str("1e6")` is exactly 1000000. It is also
        // unreachable from a real source: DuckDB's `CAST(DECIMAL AS VARCHAR)`
        // never emits exponent form. Left in the table so the next reader sees
        // the answer measured rather than assumed.
        ("1e6", true),
        ("79228162514264337593543950336", false), // one past Decimal::MAX
        // MEASURED, and the most important line in this table. `canonical_decimal`
        // is built on `Decimal::from_str`, which **rounds** past 28 significant
        // digits rather than erroring — unlike `Decimal::from_str_exact`, which
        // `migrate_products::canonical_decimal_from_f64` uses and which does
        // error. So the two R2 validators in this migrator differ in strictness.
        //
        // It carries, and for this family that is correct rather than merely
        // tolerated: the string is stored verbatim, so the SQLite read path
        // applies the *same* `from_str` rounding DuckDB's reader already applied,
        // and both sides see one value. Exposure is zero besides — a
        // `DECIMAL(18,6)` / `DECIMAL(18,2)` source has 18 digits and cannot reach
        // 28. It stops being zero only for a family whose R2 source is not a
        // `DECIMAL` column; §9 carries the row.
        ("0.12345678901234567890123456789012", true),
    ];
    for (raw, ok) in named {
        match canonical_decimal(raw, "rop-01", "routings.est_cost_huf") {
            Ok(out) => {
                assert!(*ok, "{raw:?} should have been refused, got {out:?}");
                assert_eq!(
                    &out, raw,
                    "{raw:?} must cross BYTE-IDENTICALLY, not re-rendered"
                );
                carried += 1;
            }
            Err(e) => {
                assert!(!*ok, "{raw:?} should have crossed: {e}");
                let msg = e.to_string();
                assert!(
                    msg.contains("rop-01"),
                    "the refusal must name the row: {msg}"
                );
                assert!(
                    msg.contains("routings.est_cost_huf"),
                    "the refusal must name the column: {msg}"
                );
                refused += 1;
            }
        }
    }

    // 4096 generated renderings across the magnitudes and scales a money or
    // quantity column actually takes, plus deliberate corruptions.
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for i in 0..4096u32 {
        let mag = (next() % 1_000_000_000) as i128;
        let scale = (next() % 7) as u32;
        let neg = next() % 2 == 0;
        let mut raw = if scale == 0 {
            format!("{mag}")
        } else {
            let unit = 10i128.pow(scale);
            format!(
                "{}.{:0width$}",
                mag / unit,
                mag % unit,
                width = scale as usize
            )
        };
        if neg {
            raw.insert(0, '-');
        }
        // Every 16th value is corrupted in a way a DECIMAL column could never
        // produce but a hand-edited or re-typed one could — so the refusal arm
        // is exercised on generated input too, not only on the named table.
        if i % 16 == 0 {
            raw.push_str(match next() % 4 {
                0 => " HUF",
                1 => ",00",
                2 => "..",
                _ => "e3",
            });
        }
        let key = format!("row-{i}");
        match canonical_decimal(&raw, &key, "boms.qty_per_unit") {
            Ok(out) => {
                assert_eq!(out, raw, "value {i} ({raw:?}) must cross byte-identically");
                carried += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains(&key),
                    "value {i}: refusal must name the row: {msg}"
                );
                assert!(
                    msg.contains("boms.qty_per_unit"),
                    "value {i}: refusal must name the column: {msg}"
                );
                refused += 1;
            }
        }
    }

    // **Both arms, required.** A sweep in which nothing was ever refused proves
    // only that the happy path exists.
    //
    // Measured on the fixed seed: **3923 carried, 191 refused.** The refusal
    // count is ~3/4 of the 256 corrupted values rather than all of them, because
    // one of the four corruptions is the `e3` exponent suffix, which `Decimal`
    // reads (see the named table above). The thresholds sit below the measured
    // numbers rather than on them, so a `rust_decimal` parse change shows up as
    // a real signal instead of as churn.
    assert!(carried > 3_800, "the carry arm must fire: {carried}");
    assert!(refused > 150, "the REFUSAL arm must fire: {refused}");
}

/// A value the SQLite read path could not restore **fails the whole carry**,
/// rather than crossing as an opaque string.
///
/// The mutation is applied to the DuckDB side through a widened column, because
/// a `DECIMAL` column cannot itself hold a non-decimal — which is exactly why
/// the refusal exists: the column's declared type is not the guard (§3.1's
/// corrected note), the Rust-side validation is.
///
/// ⚠ The index has to come off first. DuckDB refuses
/// `ALTER COLUMN … SET DATA TYPE` on a table any index depends on
/// (`Dependency Error: Cannot alter entry "routings"`), which is the same
/// class as `[[duckdb-alter-column-type-check-constraint]]` and is why the
/// widen is three statements rather than one.
#[test]
fn a_decimal_the_read_path_could_not_restore_fails_the_migration() {
    let dir = scratch("refuse");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP INDEX routings_tenant_wo_sequence_idx;
             ALTER TABLE routings ALTER COLUMN est_cost_huf SET DATA TYPE VARCHAR;
             UPDATE routings SET est_cost_huf = '12 500 Ft' WHERE routing_op_id = 'rop-01';",
        )
        .unwrap();
        conn.close().unwrap();
    }

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a value the read path cannot restore must FAIL the carry");
    let msg = format!("{err:#}");
    assert!(msg.contains("rop-01"), "must name the row: {msg}");
    assert!(
        msg.contains("routings.est_cost_huf"),
        "must name the column: {msg}"
    );
    assert!(
        msg.contains("canonical decimal string"),
        "must say what the rule is, not just that parsing failed: {msg}"
    );
}

// ---------------------------------------------------------------------------
// 6. The one float
// ---------------------------------------------------------------------------

/// **`work_orders.actual_machining_minutes` is `'real'` and bit-identical.**
///
/// §3.2 E carries it `DOUBLE` → `REAL` unchanged, and Part C's five rule-7
/// columns did **not** set a precedent for it: they were one half of a measured
/// divergence with an exact counterpart, and this one has none. `wo-02` is the
/// row that makes the call concrete — `12.3456789` needs scale 7, so carrying
/// this column as R2 would have *refused the migration* on an ordinary measured
/// duration.
#[test]
fn the_one_non_money_float_stays_real_and_bit_identical() {
    let (_dir, db, lite) = crossed("float");

    for (wo_id, _, _, _, minutes, _) in WORK_ORDER_CASES {
        let got: Option<f64> = {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            conn.query_row(
                "SELECT actual_machining_minutes FROM work_orders WHERE wo_id = ?",
                [wo_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        match minutes {
            None => assert_eq!(got, None, "{wo_id}"),
            Some(want) => {
                assert_eq!(got, Some(*want), "{wo_id}: DOUBLE → REAL must be bit-exact");
                assert_eq!(
                    sqlite_typeof(
                        &lite,
                        "work_orders",
                        "actual_machining_minutes",
                        "wo_id",
                        wo_id
                    ),
                    "real",
                    "{wo_id}: §3.2 E keeps this a float"
                );
            }
        }
    }

    // And the value that makes the §3.2 E call load-bearing: it is beyond the
    // canonical quantity scale of 6, so an R2 carry would have refused it.
    let duck: f64 = {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.query_row(
            "SELECT actual_machining_minutes FROM work_orders WHERE wo_id = 'wo-02'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(
        canonical_decimal(&format!("{duck}"), "wo-02", "x").is_ok(),
        "sanity: it IS a decimal"
    );
    assert!(
        rust_decimal::Decimal::from_str_exact(&format!("{duck}"))
            .unwrap()
            .scale()
            > 6,
        "the fixture must actually exercise a duration beyond the canonical quantity scale"
    );
}

// ---------------------------------------------------------------------------
// 7. The schema
// ---------------------------------------------------------------------------

/// `ensure_work_orders_schema` lands both ladders and is idempotent (M8).
///
/// The `boms_tenant_rev_idx` assertion is the specific one: V003 creates that
/// index on a column V003 itself adds, so a migrator that ran the index before
/// the `ensure_columns` post-condition would be relying on statement order
/// instead of on proof.
#[test]
fn ensure_work_orders_schema_lands_both_ladders_and_is_idempotent() {
    let dir = scratch("schema");
    let lite = dir.join("schema.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    for _ in 0..3 {
        aberp::migrate_work_orders::ensure_work_orders_schema(&conn).expect("idempotent");
    }

    for (table, col, want) in [
        ("work_orders", "qty_target", "TEXT"),
        ("work_orders", "source_quote_id", "TEXT"),
        ("work_orders", "actual_machining_minutes", "REAL"),
        ("work_orders", "bom_rev_id", "TEXT"),
        ("boms", "qty_per_unit", "TEXT"),
        ("boms", "bom_rev_id", "TEXT"),
        ("routings", "est_cost_huf", "TEXT"),
        ("routings", "sequence", "INTEGER"),
        ("routings", "est_time_min", "INTEGER"),
        ("bom_revisions", "rev_number", "INTEGER"),
        ("bom_revisions", "line_count", "INTEGER"),
        ("bom_revisions", "content_hash", "TEXT"),
    ] {
        let got: String = conn
            .query_row(
                &format!("SELECT type FROM pragma_table_info('{table}') WHERE name = '{col}'"),
                [],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("{table}.{col} must exist after the ladders: {e}"));
        assert_eq!(got, want, "{table}.{col}");
    }

    // Every table is STRICT.
    for table in FAMILY {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert!(sql.contains("STRICT"), "{table} must be STRICT: {sql}");
    }

    // The V003 index that keys on a ladder column.
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
            ["boms_tenant_rev_idx"],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "V003's boms_tenant_rev_idx must land");
}

// ---------------------------------------------------------------------------
// 8 + 9. The gate's teeth
// ---------------------------------------------------------------------------

/// The gate **hard-stops** per table when a table was not carried.
///
/// Mutation-shaped: this is what the gate does if a future edit drops one arm of
/// the carry from `carry_work_orders`.
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

/// **A single changed column on a single row reds the gate — for each of the
/// three column classes in this family.** A gate that has never been shown to
/// fail is not a gate (ADR-0107 §4.1).
///
/// The three mutations are chosen so nothing else can catch them:
///
/// * `routings.est_cost_huf` on `rop-01` — money, and the row count and the
///   `typeof` sweep are blind to a wrong figure;
/// * `boms.qty_per_unit` on `bom-01` and `bom-02` **swapped so `Σ` is
///   unchanged** — the per-row arm is then the *only* thing between a rewritten
///   BOM and a green gate;
/// * `work_orders.wo_number` on `wo-01` — an ordinary `TEXT` column that no fold
///   touches at all, which is why the arm compares every column rather than the
///   "important" ones.
#[test]
fn a_single_changed_column_on_a_single_row_reds_the_gate() {
    // --- money ---
    let (_dir, db, lite) = crossed("drift-money");
    assert!(
        reconcile(&db, &lite, TENANT).unwrap().hard_stops.is_empty(),
        "the fixture must be green before it is mutated"
    );
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(
            "UPDATE routings SET est_cost_huf = '12500.51' WHERE routing_op_id = 'rop-01'",
        )
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("routing op rop-01: est_cost_huf")),
        "the gate must name the row AND the column; it reported {:?}",
        r.hard_stops
    );
    assert!(
        !r.checks
            .iter()
            .any(|c| c.contains("every routings column round-trips with ZERO drift")),
        "the ZERO-drift check must NOT be emitted alongside a drift hard stop"
    );

    // --- a quantity, rewritten so the Σ fold cannot see it ---
    let (_dir, db, lite) = crossed("drift-sum");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(
            "UPDATE boms SET qty_per_unit = '4.5' WHERE bom_line_id = 'bom-01';
             UPDATE boms SET qty_per_unit = '2' WHERE bom_line_id = 'bom-02';",
        )
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.checks.iter().any(|c| c.contains("Σ boms.qty_per_unit")),
        "the Σ fold must still agree — that is what makes this mutation the interesting one: {:?}",
        r.checks
    );
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("bom line bom-01: qty_per_unit")),
        "the per-row arm must catch a swap the Σ fold cannot see; it reported {:?}",
        r.hard_stops
    );

    // --- an ordinary TEXT column no fold touches ---
    let (_dir, db, lite) = crossed("drift-text");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(
            "UPDATE work_orders SET wo_number = 'WO-2026-9999' WHERE wo_id = 'wo-01'",
        )
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("work order wo-01: wo_number")),
        "no fold and no typeof sweep can see this; the per-row arm must: {:?}",
        r.hard_stops
    );
}

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
    assert_eq!(out.work_orders, Default::default());

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a source without work orders is legitimate: {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("work-orders/BOM family absent on BOTH sides")),
        "the absence must be REPORTED, not silent; checks: {:?}",
        r.checks
    );
}

/// **A pre-ADR-0105 source has three of the four tables, and that is a
/// legitimate shape** — which is why presence is held per table rather than per
/// family here.
///
/// Without this, a per-family answer would say "the family is present" and then
/// hard-stop on `bom_revisions`, refusing to migrate a database whose only sin
/// is being older than V003.
#[test]
fn a_pre_adr0105_source_without_bom_revisions_still_crosses() {
    let dir = scratch("prerev");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch(
            "UPDATE work_orders SET bom_rev_id = NULL;
             UPDATE boms SET bom_rev_id = NULL;
             DROP TABLE bom_revisions;",
        )
        .unwrap();
        conn.close().unwrap();
    }

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.work_orders.work_orders, WORK_ORDER_CASES.len() as u64);
    assert_eq!(out.work_orders.bom_revisions, 0);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a pre-V003 source is legitimate, not an asymmetry: {:?}",
        r.hard_stops
    );
    // And the absent table really is absent on both sides, not silently created.
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'bom_revisions'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "creating an empty table the source does not have would manufacture the asymmetry the \
         gate exists to detect"
    );
}
