//! ADR-0108 Step 7 Part C — **the products/inventory family crosses**, and the
//! rule-7 quantity divergence is resolved rather than carried.
//!
//! Eight pins, in the order they defend:
//!
//! 1. the family crosses and **no column drifts**, across all four tables,
//!    asserted against the fixture's own literals as well as through the gate;
//! 2. **the rule-7 resolution**: the five columns that were `DOUBLE` are `TEXT`
//!    on the SQLite side, holding the canonical decimal string, and each one
//!    converts back to the identical `f64`;
//! 3. **the property sweep** — 4096 generated quantities plus the adversarial
//!    table, every one either carried byte-identically or refused **loudly**;
//! 4. a `DOUBLE` that cannot be expressed at the canonical quantity scale
//!    **fails the carry** instead of being rounded into it;
//! 5. `ensure_products_schema` lands both ladders and is idempotent (M8);
//! 6. `STRICT` refuses a float into `products.unit_price_minor` (R1) **and**
//!    into a rule-7 quantity column is impossible to do accidentally, because
//!    the column is `TEXT` — so the pin is on the money column and on the
//!    `typeof` sweep that catches the quantity case;
//! 7. the gate **hard-stops** per table when a table was not carried;
//! 8. the per-row equality arm is **shown to go red** on a single changed
//!    quantity on a single row — and separately on a `stock_movements.qty_delta`
//!    that differs only in a digit the row count, the `Σ` fold and the `typeof`
//!    sweep are all blind to.
//!
//! **This test does not pin §3.4's three cache-rebuild folds, and that is on
//! purpose.** Nothing here makes `aberp_inventory::repository`'s
//! `SUM(qty_delta)` run against SQLite; those statements still go to DuckDB,
//! where `DECIMAL` is a real decimal type. See the module docs of
//! `aberp::migrate_products` and §9's row.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::migrate_products::canonical_decimal_from_f64;
use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};

const TENANT: &str = "test";

/// The family's four tables, in the order the migrator and the gate walk them.
const FAMILY: &[&str] = &[
    "products",
    "stock_movements",
    "inventory_balances",
    "inventory_reservations",
];

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-products-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `(id, name, unit_price_minor, stock_qty, min_stock, bin_location)`.
///
/// * **`prd-01`** — the ordinary case, a fractional stock quantity that renders
///   with DuckDB's `DECIMAL(18,6)` trailing zeros (`"12.500000"`). It must cross
///   **verbatim**: re-rendering it here would make the migration's trailing-zero
///   behaviour depend on when the row happened to be written (§3.2 C).
/// * **`prd-02`** — a **negative** stock quantity. Real, and the sign is the
///   thing a lexicographic `TEXT` comparison gets most wrong, which is why the
///   low-stock predicate had to move to Rust in the first place.
/// * **`prd-03`** — the pre-inventory-ladder row: `stock_qty` / `min_stock` /
///   `bin_location` all NULL, the shape `V001__inventory.sql` exists to add and
///   whose backfill this migrator must NOT re-derive (B4).
/// * **`prd-04`** — soft-deleted, and it must still cross.
/// * **`prd-05`** — a non-ASCII name and a zero price.
const PRODUCT_CASES: &[(&str, &str, i64, Option<&str>, Option<&str>, Option<&str>)] = &[
    (
        "prd-01",
        "Tengely 40CrMo",
        125_000,
        Some("12.500000"),
        Some("3.000000"),
        Some("A-01-3"),
    ),
    (
        "prd-02",
        "Karima DN80",
        4_990,
        Some("-2.250000"),
        Some("10.000000"),
        Some("B-04-1"),
    ),
    ("prd-03", "Pre-ladder Widget", 1, None, None, None),
    (
        "prd-04",
        "Deleted Product",
        7_500,
        Some("0.000000"),
        Some("0.000000"),
        None,
    ),
    (
        "prd-05",
        "Csapágyház Ø120 — 100% acél",
        0,
        Some("999999.999999"),
        Some("0.000001"),
        Some("C-09-9"),
    ),
];

/// `(movement_id, product_id, qty_delta, reason, notes)`.
///
/// `qty_delta` is R2 today and crosses verbatim. The set is chosen so
/// `Σ qty_delta` is **not** the `stock_qty` cache on any product — the migrator
/// carries the cache rather than rebuilding it (§6.3 corrected), and a fixture
/// where the two agreed by accident could not tell the two behaviours apart.
const MOVEMENT_CASES: &[(&str, &str, &str, &str, Option<&str>)] = &[
    ("mvt-01", "prd-01", "10.000000", "receipt", Some("PO-1")),
    ("mvt-02", "prd-01", "-1.500000", "issue", None),
    ("mvt-03", "prd-02", "0.000001", "adjustment", Some("±")),
    ("mvt-04", "prd-05", "-0.250000", "scrap", None),
];

/// `(tenant, grade, on_hand, reserved, committed, consumed, uom, heat_lot)` —
/// **the rule-7 rows.** Every quantity here is an `f64` on DuckDB.
///
/// * `1.5` / `0.1` / `2.5` — values whose float representation is not the
///   decimal a naive `{:.6}` would print, and whose shortest round-trip
///   rendering is exact.
/// * `0.0` — the `DEFAULT 0` shape, which under R2 must store `'0'` and not the
///   INTEGER `0` a bare `DEFAULT 0` would have put there.
/// * a grade with **no heat lot** (all four S432 columns NULL) — the signal the
///   defense WO-start gate reads, so a backfill here would silently open it.
#[allow(clippy::type_complexity)]
const BALANCE_CASES: &[(&str, f64, f64, f64, f64, &str, Option<&str>)] = &[
    ("42CrMo4", 1.5, 0.1, 2.5, 0.0, "kg", Some("HEAT-2026-001")),
    ("AlMg3", 0.0, 0.0, 0.0, 0.0, "kg", None),
    ("1.4301", 1234.5, 12.25, 0.125, 100.0, "kg", Some("H-2")),
];

/// `(reservation_id, quote_id, grade, qty, state, qty_unit_kind)`.
const RESERVATION_CASES: &[(&str, &str, &str, f64, &str, Option<&str>)] = &[
    ("res-01", "q-1", "42CrMo4", 3.0, "reserved", Some("units")),
    ("res-02", "q-2", "1.4301", 0.75, "committed", Some("kg")),
    // pre-S275: `qty_unit_kind` is NULL and reads as `units`.
    ("res-03", "q-3", "AlMg3", 12.0, "released", None),
];

/// Seed a DEV-shaped DuckDB through the **real** `ensure_schema` functions, so
/// the SQLite side is compared against the schema the product actually builds
/// rather than against a hand-written copy of it.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");

    {
        let conn = duckdb::Connection::open(&db).unwrap();
        aberp::products::ensure_schema(&conn).unwrap();
        // The inventory ladder + `stock_movements`, through the crate's own
        // `ensure_schema` rather than a copy of its migration — the SQLite side
        // must be compared against the schema the product actually builds.
        aberp_inventory::ensure_schema(&conn).unwrap();
        aberp::material_inventory::ensure_schema(&conn).unwrap();

        for (id, name, price, stock, min, bin) in PRODUCT_CASES {
            let deleted_at = if *id == "prd-04" {
                Some("2026-07-30T10:00:00Z")
            } else {
                None
            };
            conn.execute(
                "INSERT INTO products
                   (id, tenant_id, name, unit_kind, unit_value, currency, unit_price_minor,
                    created_at, updated_at, deleted_at, stock_qty, min_stock, bin_location,
                    last_movement_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    id,
                    TENANT,
                    name,
                    "nav",
                    "PIECE",
                    "HUF",
                    price,
                    "2026-01-01T00:00:00Z",
                    "2026-07-01T00:00:00Z",
                    deleted_at,
                    stock,
                    min,
                    bin,
                    if stock.is_some() {
                        Some("2026-07-02T00:00:00Z")
                    } else {
                        None
                    },
                ],
            )
            .unwrap();
        }

        for (mid, pid, qty, reason, notes) in MOVEMENT_CASES {
            conn.execute(
                "INSERT INTO stock_movements
                   (movement_id, tenant_id, product_id, qty_delta, reason, ref_kind, ref_id,
                    at_iso8601, operator, idempotency_key, notes)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    mid,
                    TENANT,
                    pid,
                    qty,
                    reason,
                    Some("purchase_order"),
                    Some("po-1"),
                    "2026-07-02T08:00:00Z",
                    "operator@example.test",
                    format!("idem-{mid}"),
                    notes,
                ],
            )
            .unwrap();
        }

        for (grade, on_hand, reserved, committed, consumed, uom, heat) in BALANCE_CASES {
            conn.execute(
                "INSERT INTO inventory_balances
                   (tenant_id, material_grade, on_hand_qty, reserved_qty, committed_qty,
                    consumed_qty, unit_of_measure, last_updated, heat_lot_number,
                    mill_test_report_url, heat_assigned_at_utc, heat_assigned_by_operator)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    TENANT,
                    grade,
                    on_hand,
                    reserved,
                    committed,
                    consumed,
                    uom,
                    "2026-07-03T00:00:00Z",
                    heat,
                    heat.map(|_| "https://mtr.example.test/1.pdf"),
                    heat.map(|_| "2026-07-03T00:00:00Z"),
                    heat.map(|_| "operator@example.test"),
                ],
            )
            .unwrap();
        }

        for (rid, quote, grade, qty, state, kind) in RESERVATION_CASES {
            conn.execute(
                "INSERT INTO inventory_reservations
                   (reservation_id, tenant_id, quote_id, material_grade, qty, state,
                    created_at, transitioned_at, qty_unit_kind)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    rid,
                    TENANT,
                    quote,
                    grade,
                    qty,
                    state,
                    "2026-07-04T00:00:00Z",
                    "2026-07-05T00:00:00Z",
                    kind,
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
    assert_eq!(out.products.products, PRODUCT_CASES.len() as u64);
    assert_eq!(out.products.stock_movements, MOVEMENT_CASES.len() as u64);
    assert_eq!(out.products.inventory_balances, BALANCE_CASES.len() as u64);
    assert_eq!(
        out.products.inventory_reservations,
        RESERVATION_CASES.len() as u64
    );
    (dir, db, lite)
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
fn the_products_inventory_family_crosses_with_zero_drift() {
    let (_dir, db, lite) = crossed("family");

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "unexpected hard stops: {:?}",
        r.hard_stops
    );
    for want in [
        "products row count = 5",
        "stock_movements row count = 4",
        "inventory_balances row count = 3",
        "inventory_reservations row count = 3",
        "every products column round-trips with ZERO drift",
        "every stock_movements column round-trips with ZERO drift",
        "every inventory_balances column round-trips with ZERO drift",
        "every inventory_reservations column round-trips with ZERO drift",
        "Σ products.unit_price_minor",
        "Σ products.stock_qty",
        "Σ stock_movements.qty_delta",
        "typeof(products.unit_price_minor) = 'integer'",
        "typeof(products.stock_qty) = 'text'",
        "typeof(stock_movements.qty_delta) = 'text'",
        "typeof(inventory_balances.on_hand_qty) = 'text'",
        "typeof(inventory_reservations.qty) = 'text'",
    ] {
        assert!(
            r.checks.iter().any(|c| c.contains(want)),
            "the gate never ran a `{want}` check; it has {:?}",
            r.checks
        );
    }

    // --- products, against the fixture's own literals ---
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT id, name, unit_price_minor, stock_qty, min_stock, bin_location, deleted_at
             FROM products ORDER BY id ASC",
        )
        .unwrap();
    #[allow(clippy::type_complexity)]
    let got: Vec<(
        String,
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(got.len(), PRODUCT_CASES.len());
    for (got, want) in got.iter().zip(PRODUCT_CASES) {
        assert_eq!(got.0, want.0);
        assert_eq!(got.1, want.1, "the name must cross as UTF-8 verbatim");
        assert_eq!(
            got.2, want.2,
            "unit_price_minor is R1 money — an INTEGER count of minor units, never a float"
        );
        assert_eq!(
            got.3.as_deref(),
            want.3,
            "stock_qty must cross as the canonical decimal string VERBATIM — re-rendering it \
             would make the trailing-zero behaviour depend on when the row was written (§3.2 C)"
        );
        assert_eq!(got.4.as_deref(), want.4);
        assert_eq!(got.5.as_deref(), want.5);
    }
    assert_eq!(
        got.iter().filter(|g| g.6.is_some()).count(),
        1,
        "the soft-deleted product must cross rather than being quietly dropped"
    );
    let pre_ladder = got.iter().find(|g| g.0 == "prd-03").unwrap();
    assert_eq!(
        (pre_ladder.3.as_deref(), pre_ladder.4.as_deref()),
        (None, None),
        "the pre-ladder NULLs cross as NULLs: V001's backfill is DuckDB's job, and re-deriving \
         it here is the `verify the extraction against itself` shape B4 forbids"
    );

    // --- stock_movements ---
    let mut stmt = conn
        .prepare("SELECT movement_id, qty_delta, notes FROM stock_movements ORDER BY movement_id")
        .unwrap();
    let movements: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(movements.len(), MOVEMENT_CASES.len());
    for (got, want) in movements.iter().zip(MOVEMENT_CASES) {
        assert_eq!(got.0, want.0);
        assert_eq!(got.1, want.2, "qty_delta crosses verbatim");
        assert_eq!(got.2.as_deref(), want.4);
    }
}

// ---------------------------------------------------------------------------
// 2. THE RULE-7 RESOLUTION
// ---------------------------------------------------------------------------

/// The five columns that were `DOUBLE` are `TEXT` after the carry, holding the
/// canonical decimal string, and each one converts back to the identical `f64`.
///
/// This is the pin the whole commit exists for. Without it the conservative
/// reading of §3.2 E — carry the `DOUBLE`s as `REAL` — would produce a green
/// gate over a `STRICT` schema that declares a quantity to be a float in one
/// half of the product and exact in the other.
#[test]
fn the_five_double_quantity_columns_cross_as_exact_decimals_not_floats() {
    let (_dir, _db, lite) = crossed("rule7");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    // The declared type, read off the schema rather than inferred.
    for (table, col) in [
        ("inventory_balances", "on_hand_qty"),
        ("inventory_balances", "reserved_qty"),
        ("inventory_balances", "committed_qty"),
        ("inventory_balances", "consumed_qty"),
        ("inventory_reservations", "qty"),
    ] {
        let ty: String = conn
            .query_row(
                "SELECT type FROM pragma_table_info(?) WHERE name = ?",
                [table, col],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            ty, "TEXT",
            "{table}.{col} was DOUBLE on DuckDB; ADR-0108's rule-7 resolution brings it onto \
             stock_movements.qty_delta's exact representation, not onto REAL"
        );
    }

    // The stored strings, against what the f64 fixture values must canonicalise
    // to — and every one of them is exact, not rounded.
    let mut stmt = conn
        .prepare(
            "SELECT material_grade, on_hand_qty, reserved_qty, committed_qty, consumed_qty
             FROM inventory_balances ORDER BY tenant_id, material_grade",
        )
        .unwrap();
    let rows: Vec<(String, String, String, String, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    let mut sorted: Vec<_> = BALANCE_CASES.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    assert_eq!(rows.len(), sorted.len());
    for (got, want) in rows.iter().zip(&sorted) {
        assert_eq!(got.0, want.0);
        for (stored, source) in [
            (&got.1, want.1),
            (&got.2, want.2),
            (&got.3, want.3),
            (&got.4, want.4),
        ] {
            let expected = canonical_decimal_from_f64(source, &got.0, "fixture").unwrap();
            assert_eq!(
                *stored, expected,
                "{} must store the canonical decimal of {source}",
                got.0
            );
            assert_eq!(
                stored.parse::<f64>().unwrap(),
                source,
                "the stored string must convert back to the IDENTICAL f64 — that is what makes \
                 the rule-7 carry value-neutral rather than merely well-typed"
            );
        }
    }

    // `0.0` stores as the string `'0'`, not as the INTEGER 0 a bare
    // `DEFAULT 0` under STRICT would have left there.
    let zero: String = conn
        .query_row(
            "SELECT on_hand_qty FROM inventory_balances WHERE material_grade = 'AlMg3'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(zero, "0");

    // The no-heat-lot grade crossed with all four S432 columns NULL — a
    // backfill here would silently open the defense WO-start gate.
    let heat: Option<String> = conn
        .query_row(
            "SELECT heat_lot_number FROM inventory_balances WHERE material_grade = 'AlMg3'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(heat, None);
}

// ---------------------------------------------------------------------------
// 3. The property sweep
// ---------------------------------------------------------------------------

/// **Every migrated quantity round-trips, or the carry refuses loudly.**
///
/// A deterministic sweep rather than a random one: the generator is a fixed
/// xorshift over a fixed seed, so a failure is reproducible from the test name
/// alone and CI cannot go green-then-red on the same code. 4096 values across
/// the magnitudes a stocked quantity actually takes, plus every adversarial
/// value worth naming.
///
/// The property is a **disjunction**, and the second arm is the point: a value
/// that cannot be carried exactly must produce an `Err`, never a rounded `Ok`.
/// A test that only asserted the happy arm would pass on a carry that silently
/// rounded everything to six places.
#[test]
fn every_quantity_either_round_trips_exactly_or_is_refused() {
    let mut state: u64 = 0x2026_0801_0108_7c00;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut carried = 0usize;
    let mut refused = 0usize;

    // The adversarial table first, so a regression in the named cases is not
    // buried in the sweep's counters.
    let named: Vec<f64> = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        0.1,
        0.2,
        0.1 + 0.2,
        1.0 / 3.0,
        2.5,
        12.345678,
        999_999.999_999,
        1e6,
        1e-6,
        1e-7,
        f64::MIN_POSITIVE,
        f64::MAX,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];

    let sweep: Vec<f64> = (0..4096)
        .map(|i| {
            let r = next();
            // Quantities as an operator enters them: a signed integer part and
            // a scale-0..8 fractional part. Scales above 6 MUST be refused,
            // which is why the range deliberately overshoots the canonical one.
            let scale = (r % 9) as u32;
            let whole = ((r >> 8) % 1_000_000) as i64;
            let frac = ((r >> 32) % 10u64.pow(scale.max(1))) as i64;
            let sign = if i % 3 == 0 { -1.0 } else { 1.0 };
            let v = whole as f64 + (frac as f64) / 10f64.powi(scale as i32);
            sign * v
        })
        .collect();

    for v in named.iter().chain(sweep.iter()).copied() {
        match canonical_decimal_from_f64(v, "k", "c") {
            Ok(s) => {
                carried += 1;
                let back: f64 = s.parse().expect("a canonical decimal string parses as f64");
                assert_eq!(
                    back, v,
                    "{v} carried as {s:?} but reads back as {back} — the rule-7 carry must be \
                     value-neutral"
                );
                let scale = s.split_once('.').map_or(0, |(_, f)| f.len());
                assert!(
                    scale <= 6,
                    "{v} carried as {s:?} at scale {scale}, above the canonical quantity scale \
                     of 6 that DECIMAL(18,6) gives stock_movements.qty_delta"
                );
                assert!(
                    !s.contains('e') && !s.contains('E'),
                    "{v} carried as {s:?} — an exponent form is not a decimal string a SQL \
                     comparison or a later Decimal::from_str would read the same way"
                );
            }
            Err(e) => {
                refused += 1;
                let msg = e.to_string();
                assert!(
                    msg.contains("canonical quantity scale")
                        || msg.contains("no decimal representation")
                        || msg.contains("cannot represent"),
                    "a refusal must say WHY: {msg}"
                );
            }
        }
    }

    // Both arms must actually be exercised. A sweep where everything was
    // carried would prove nothing about the refusal, and vice versa.
    assert!(carried > 2_500, "only {carried} values carried");
    assert!(
        refused > 200,
        "only {refused} values were refused — the over-scale arm is not being reached, so the \
         'refuse rather than round' property is untested"
    );
}

// ---------------------------------------------------------------------------
// 4. Refuse rather than round, end to end
// ---------------------------------------------------------------------------

/// A `DOUBLE` that cannot be expressed at the canonical quantity scale **fails
/// the whole carry**, rather than being rounded into it.
///
/// `0.1 + 0.2` is the value the representation rules exist for: it is
/// `0.30000000000000004`, and a migrator that stored `"0.3"` would have moved a
/// stocked quantity — silently, once, in a direction nobody would ever look
/// for. It is also the value §3.1's own table uses to show that `STRICT` does
/// not protect an R2 column.
#[test]
fn a_double_that_cannot_be_carried_exactly_fails_the_migration() {
    let dir = scratch("refuse");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE inventory_balances SET on_hand_qty = ? WHERE material_grade = 'AlMg3'",
            duckdb::params![0.1_f64 + 0.2_f64],
        )
        .unwrap();
        conn.close().unwrap();
    }
    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");

    let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a quantity that cannot cross exactly must fail the migration");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("inventory_balances.on_hand_qty") && msg.contains("test/AlMg3"),
        "the refusal must name the column AND the row: {msg}"
    );
    assert!(
        msg.contains("ROUNDING a stocked quantity"),
        "the refusal must say what it is refusing to do: {msg}"
    );
}

// ---------------------------------------------------------------------------
// 5 + 6. STRICT + ensure_columns
// ---------------------------------------------------------------------------

/// `ensure_products_schema` lands both ladders and is idempotent (M8).
///
/// The counts are exact, not "at least": a ladder that added a column the
/// DuckDB build does not have is as much a divergence as one that added
/// nothing.
#[test]
fn ensure_products_schema_lands_both_ladders_and_is_idempotent() {
    let dir = scratch("schema");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    aberp::migrate_products::ensure_products_schema(&conn).unwrap();
    let cols = |t: &str| -> Vec<String> {
        let mut s = conn
            .prepare("SELECT name FROM pragma_table_info(?) ORDER BY name")
            .unwrap();
        s.query_map([t], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let snapshot = |c: &dyn Fn(&str) -> Vec<String>| -> Vec<Vec<String>> {
        FAMILY.iter().map(|t| c(t)).collect()
    };
    let before = snapshot(&cols);
    assert_eq!(before[0].len(), 14, "products: {:?}", before[0]);
    assert_eq!(before[1].len(), 11, "stock_movements: {:?}", before[1]);
    assert_eq!(before[2].len(), 12, "inventory_balances: {:?}", before[2]);
    assert_eq!(
        before[3].len(),
        9,
        "inventory_reservations: {:?}",
        before[3]
    );
    for ladder in ["stock_qty", "min_stock", "bin_location", "last_movement_at"] {
        assert!(
            before[0].iter().any(|c| c == ladder),
            "V001's ladder column {ladder} did not land — `ensure_columns` must PROVE the column \
             is there, not merely report that an ALTER ran"
        );
    }
    for ladder in [
        "heat_lot_number",
        "mill_test_report_url",
        "heat_assigned_at_utc",
        "heat_assigned_by_operator",
    ] {
        assert!(
            before[2].iter().any(|c| c == ladder),
            "S432's ladder column {ladder} did not land"
        );
    }
    assert!(before[3].iter().any(|c| c == "qty_unit_kind"));

    aberp::migrate_products::ensure_products_schema(&conn).unwrap();
    assert_eq!(before, snapshot(&cols), "a second run must change nothing");
}

/// `STRICT` refuses a float into `products.unit_price_minor` — the family's R1
/// money column.
///
/// The extended code is asserted rather than a substring: `SQLITE_CONSTRAINT`
/// is 19 and the `DATATYPE` sub-code is 12, so the extended code is
/// `19 | (12 << 8)` = 3091, and it exists **only** on a `STRICT` table. Drop the
/// `STRICT` suffix and the same statement succeeds silently.
#[test]
fn strict_refuses_a_float_into_the_money_column() {
    let dir = scratch("strict");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    aberp::migrate_products::ensure_products_schema(&conn).unwrap();

    let err = conn
        .execute(
            "INSERT INTO products
               (id, tenant_id, name, unit_kind, unit_value, currency, unit_price_minor,
                created_at, updated_at)
             VALUES ('x', 't', 'n', 'nav', 'PIECE', 'HUF', ?, 'c', 'u')",
            [1.5_f64],
        )
        .expect_err("STRICT must refuse a REAL into an INTEGER money column");
    match err {
        aberp_db::engine::Error::SqliteFailure(e, msg) => {
            assert_eq!(
                e.extended_code,
                19 | (12 << 8),
                "expected SQLITE_CONSTRAINT_DATATYPE, got {e:?}: {msg:?}"
            );
            assert!(
                msg.clone().unwrap_or_default().contains("unit_price_minor"),
                "the refusal must name the column: {msg:?}"
            );
        }
        other => panic!("expected a SQLite datatype constraint failure, got {other:?}"),
    }
}

/// **`STRICT` does NOT protect the rule-7 columns, and the `typeof` sweep is
/// what does.** §3.1's 2026-07-31 correction measured that a `REAL` bound into
/// a `TEXT` column converts losslessly and is accepted — and that `typeof()`
/// then reads `'text'` on it too.
///
/// So the guard on these five columns is not the declared type: it is that the
/// carry binds a `String` the migrator proved, and that no SQL arithmetic ever
/// touches them (T-8). This test pins the *narrow* thing `STRICT` does give —
/// the column is `TEXT`, so an INTEGER `0` bound into it converts and a later
/// `typeof` reads `'text'` — so that a reader does not take the gate's green
/// `typeof` line for more than it is worth.
#[test]
fn strict_does_not_protect_a_rule7_quantity_column_and_the_gate_says_which_guard_does() {
    let dir = scratch("r2unprotected");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    aberp::migrate_products::ensure_products_schema(&conn).unwrap();

    conn.execute(
        "INSERT INTO inventory_balances
           (tenant_id, material_grade, on_hand_qty, unit_of_measure, last_updated)
         VALUES ('t', 'g', ?, 'kg', 'now')",
        [0.1_f64 + 0.2_f64],
    )
    .expect("STRICT accepts a REAL into a TEXT column — it converts losslessly");

    let (stored, class): (String, String) = conn
        .query_row(
            "SELECT on_hand_qty, typeof(on_hand_qty) FROM inventory_balances",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        stored.parse::<f64>().unwrap(),
        0.1_f64 + 0.2_f64,
        "the float was stringified, not rejected — {stored:?}"
    );
    assert_eq!(
        class, "text",
        "the typeof sweep reads 'text' on a stringified FLOAT — which is exactly why R2's real \
         guards are the Rust-side bind and T-8, not the declared type"
    );
}

// ---------------------------------------------------------------------------
// 7 + 8. The gate's refusals
// ---------------------------------------------------------------------------

/// The silent-skip shape, **per table**: each of the four must be named when it
/// is on the DuckDB side and not on the SQLite one.
///
/// Mutation-shaped: this is what the gate does if a future edit drops one arm
/// of the carry from `carry_products`.
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

/// **The per-row equality arm goes red on a rule-7 quantity.** A gate that has
/// never been shown to fail is not a gate (ADR-0107 §4.1).
///
/// The mutation is the smallest one that matters: one quantity on one row moves
/// by a millionth. The row count and the `typeof` sweep are blind to it, and so
/// is any check that only compared totals per table — which is why the arm
/// compares per row over every column.
#[test]
fn a_single_changed_quantity_on_a_single_row_reds_the_gate() {
    let (_dir, db, lite) = crossed("drift-qty");
    assert!(
        reconcile(&db, &lite, TENANT).unwrap().hard_stops.is_empty(),
        "the fixture must be green before it is mutated"
    );

    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(
            "UPDATE inventory_balances SET on_hand_qty = '1.500001' \
             WHERE material_grade = '42CrMo4'",
        )
        .unwrap();
    }

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("inventory_balance test/42CrMo4: on_hand_qty")),
        "the gate must name the row AND the column; it reported {:?}",
        r.hard_stops
    );
    assert!(
        !r.checks
            .iter()
            .any(|c| c.contains("every inventory_balances column round-trips with ZERO drift")),
        "the ZERO-drift check must NOT be emitted alongside a drift hard stop"
    );
}

/// The same arm on `stock_movements.qty_delta` — the movement ledger the stock
/// cache is *defined* as the sum of.
///
/// The mutation is chosen to keep `Σ qty_delta` unchanged: `10.000000` and
/// `-1.500000` on `prd-01` are swapped for `8.500000` and `0.000000`. The row
/// count is unchanged, the `Σ` fold is unchanged, and the `typeof` sweep is
/// unchanged — so the per-row arm is the **only** thing between a rewritten
/// movement history and a green gate.
#[test]
fn a_qty_delta_rewrite_that_preserves_the_sum_still_reds_the_gate() {
    let (_dir, db, lite) = crossed("drift-sum");

    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(
            "UPDATE stock_movements SET qty_delta = '8.500000' WHERE movement_id = 'mvt-01';
             UPDATE stock_movements SET qty_delta = '0.000000' WHERE movement_id = 'mvt-02';",
        )
        .unwrap();
    }

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("Σ stock_movements.qty_delta")),
        "the Σ fold must still agree — that is what makes this mutation the interesting one: \
         {:?}",
        r.checks
    );
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("stock_movement mvt-01: qty_delta")),
        "the per-row arm must catch a rewritten movement the Σ fold cannot see; it reported {:?}",
        r.hard_stops
    );
}

/// A source with none of the family's tables is a legitimate shape, and the
/// gate says so out loud rather than staying silent.
#[test]
fn a_source_without_the_family_reports_the_absence_rather_than_staying_silent() {
    let dir = scratch("absent");
    let db = dir.join("aberp.duckdb");
    seed_ledger(&db);

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.products, Default::default());

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a source without products is legitimate: {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("products/inventory family absent on BOTH sides")),
        "the absence must be REPORTED, not silent; checks: {:?}",
        r.checks
    );
}
