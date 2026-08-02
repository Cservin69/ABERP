//! ADR-0108 Step 7 Part F — **the dispatch / shipment family crosses.**
//!
//! Parts B (partners), C (products/inventory), D (work-orders/BOM) and E (QA/QC)
//! are already on this branch, so this is **Part F**. Named rather than silently
//! renumbered — the same call Parts D and E made.
//!
//! Eleven pins, in the order they defend:
//!
//! 1. both tables cross and **no column drifts**, asserted against the fixture's
//!    own literals as well as through the gate;
//! 2. the three-part composite key really is composite — `unit_index = 0` exists
//!    under two different `wo_id`s and both rows survive, which a key built from
//!    two of the three components would have collapsed;
//! 3. `unit_index` crosses as `'integer'` (§3.2 F) with its value intact, and
//!    every other column as `'text'`;
//! 4. **the disjunction sweep** — every natural key in the source is either
//!    carried with a **byte-identical** round-trip or **refused loudly** naming
//!    the table and the key. Both arms are required to fire;
//! 5. the same disjunction proved through **real storage**: 256 generated rows
//!    seeded into a real DuckDB file, carried by the real migrator into a real
//!    SQLite file, and read back byte-for-byte — text *and* ordinal;
//! 6. a duplicate natural key **fails the whole carry** rather than crossing as
//!    a row nobody compares. `wo_part_marks` has no `PRIMARY KEY`, so this is
//!    reachable from a real DuckDB source and is seeded as one;
//! 7. `ensure_dispatch_schema` builds both tables, is idempotent, and declares
//!    every column with the §3.2 vocabulary;
//! 8. the gate **hard-stops** per table when a table was not carried;
//! 9. the per-row equality arm is shown to go red on **every mutable column of
//!    the composite-keyed table, one column at a time** — and separately on
//!    every mutable column of `dispatches`;
//! 10. a source with neither table is a legitimate shape and the gate says so
//!     out loud;
//! 11. a pre-S432 source with `dispatches` and no `wo_part_marks` still crosses,
//!     which is why presence is held per table.
//!
//! **This test pins no §3.4 fold, because this family owes none**, no R1/R2
//! money or quantity handling, because measurement says the family has neither,
//! and no M11-shaped `LOWER()`/`LIKE` refusal, because both patterns return zero
//! hits against both tables.

#![cfg(feature = "sqlite-engine")]

use std::path::{Path, PathBuf};

use aberp::migrate_dispatch::unique_natural_keys;
use aberp::migrate_to_sqlite::{migrate_families, reconcile, LedgerSource};
use aberp::premigration::run_snapshot;
use aberp_audit_ledger::{Actor, BinaryHash, EventKind, Ledger, TenantId};

const TENANT: &str = "test";

/// The family's two tables, in the order the migrator and the gate walk them.
const FAMILY: &[&str] = &["dispatches", "wo_part_marks"];

fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "aberp-adr0108-step7-dispatch-{tag}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// `(dsp_id, wo_id, partner_id, state, shipped, cancelled, notes)`.
///
/// * **`dsp-01`** — `drafted`: all six nullable columns are `NULL`, so the
///   NULL arm of every one of them is exercised.
/// * **`dsp-02`** — `shipped`: every nullable column populated, and `notes`
///   carries the accented Hungarian characters an ASCII `LOWER()` would fold
///   plus a literal `%` and `_`. There is **no** `LOWER()` or `LIKE` site in
///   this family (measured, zero hits), so this is not a T-12 pin — it is here
///   so that a T-12-shaped test has a value to key on if one ever becomes owed.
/// * **`dsp-03`** — `cancelled`: `cancelled_at` populated while `shipped_at`
///   stays `NULL`, the one combination `dsp-01` and `dsp-02` do not cover.
/// * **`dsp-04`** — `shipped` with `notes = ""`. **The empty string is a value**
///   and must cross as `""`, not as `NULL`: on SQLite the two are different
///   storage classes and the gate's `IS NOT NULL`-scoped `typeof` sweep cannot
///   tell them apart, so only the per-row arm can.
const DISPATCH_CASES: &[(&str, &str, &str, &str, bool, bool, Option<&str>)] = &[
    ("dsp-01", "wo-01", "prt-01", "drafted", false, false, None),
    (
        "dsp-02",
        "wo-02",
        "prt-02",
        "shipped",
        true,
        false,
        Some("Árvíztűrő tükörfúrógép — 100% ellenőrizve _ raklap"),
    ),
    ("dsp-03", "wo-03", "prt-01", "cancelled", false, true, None),
    (
        "dsp-04",
        "wo-04",
        "prt-03",
        "shipped",
        true,
        false,
        Some(""),
    ),
];

/// `(wo_id, unit_index, heat_lot)`.
///
/// **`unit_index` restarts at 0 for every WO**, so `wo-01#0` and `wo-02#0` both
/// exist — which is the point: the key is a three-part composite, and a migrator
/// that keyed on `unit_index` alone, or on `(tenant_id, unit_index)`, would pair
/// the wrong rows and the gate would still be green. Pin 2 asserts exactly that.
///
/// `wo-01#2` carries a `NULL` heat lot: a non-defense part is marked without
/// one, and the nullable column must cross as `NULL` rather than as `""`.
const MARK_CASES: &[(&str, i64, Option<&str>)] = &[
    ("wo-01", 0, Some("HL-2026-0001")),
    ("wo-01", 1, Some("HL-2026-0001")),
    ("wo-01", 2, None),
    ("wo-02", 0, Some("HL-2026-0002")),
    // A large ordinal: DuckDB's `INTEGER` is 32-bit while SQLite's is 64-bit, so
    // the carry widens. `i32::MAX` is the largest value the source can hold and
    // proves the widening is lossless rather than merely untested.
    ("wo-02", i32::MAX as i64, Some("HL-2026-0002")),
];

/// How many generated rows pin 5 pushes through real storage.
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

/// A `TEXT` value across the shapes a dispatch record actually holds — ULIDs,
/// carrier tracking numbers, accented operator notes — plus the ones that break
/// a naive bind: quotes, a literal `%`, a newline, and the empty string.
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
        _ => format!("HL-{}-{:04}", 2020 + (n % 10), n % 9999),
    }
}

/// Seed a DEV-shaped DuckDB through the **real** `ensure_schema`s, so the SQLite
/// side is compared against the schema the product actually builds rather than
/// against a hand-written copy of it.
///
/// Two of them, because this family has two owning modules: `aberp_dispatch`
/// builds `dispatches` (V001) and `aberp::part_marking` builds `wo_part_marks`.
fn seed(dir: &Path) -> PathBuf {
    let db = dir.join("aberp.duckdb");

    {
        let conn = duckdb::Connection::open(&db).unwrap();
        aberp_dispatch::ensure_schema(&conn).unwrap();
        aberp::part_marking::ensure_schema(&conn).unwrap();

        for (dsp_id, wo_id, partner_id, state, shipped, cancelled, notes) in DISPATCH_CASES {
            conn.execute(
                "INSERT INTO dispatches
                   (dsp_id, tenant_id, wo_id, partner_id, state, created_at, shipped_at,
                    cancelled_at, carrier_kind, tracking_number, spawned_invoice_id, notes)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    dsp_id,
                    TENANT,
                    wo_id,
                    partner_id,
                    state,
                    "2026-02-01T09:00:00Z",
                    shipped.then_some("2026-02-03T14:30:00Z"),
                    cancelled.then_some("2026-02-02T11:15:00Z"),
                    shipped.then_some("courier"),
                    shipped.then_some("GLS-4711-ÁÉÍ"),
                    shipped.then_some("inv-2026-0007"),
                    notes,
                ],
            )
            .unwrap();
        }

        for (wo_id, unit_index, heat_lot) in MARK_CASES {
            insert_mark(&conn, TENANT, wo_id, *unit_index, *heat_lot, None);
        }

        // The swept rows, on their own WO so the fixture's own cases stay
        // readable in a failure message.
        let mut state = 0x5EED_1108_D15A_u64 ^ 0x9E37_79B9_7F4A_7C15;
        for i in 0..SWEPT_ROWS {
            let heat = swept_text(i, &mut state);
            insert_mark(
                &conn,
                TENANT,
                "wo-sweep",
                i as i64,
                Some(&heat),
                Some(&swept_text(i + 3, &mut state)),
            );
        }

        conn.close().unwrap();
    }

    seed_ledger(&db);
    db
}

/// One `wo_part_marks` row. `serial` overrides the derived serial number so the
/// sweep can push a generated value through a second `TEXT` column.
fn insert_mark(
    conn: &duckdb::Connection,
    tenant: &str,
    wo_id: &str,
    unit_index: i64,
    heat_lot: Option<&str>,
    serial: Option<&str>,
) {
    let derived = format!("SN-{wo_id}-{unit_index:04}");
    conn.execute(
        "INSERT INTO wo_part_marks
           (tenant_id, wo_id, unit_index, part_uid, serial_number, data_matrix_payload,
            heat_lot_reference, marked_at_utc, marked_by_operator)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        duckdb::params![
            tenant,
            wo_id,
            unit_index,
            format!("UID-{wo_id}-{unit_index}"),
            serial.unwrap_or(&derived),
            format!("[)>06{wo_id}/{unit_index}"),
            heat_lot,
            "2026-02-01T10:00:00Z",
            "operator@example.test",
        ],
    )
    .unwrap();
}

/// The expected `serial_number` for a fixture row, so pin 1 compares SQLite
/// against the literal the fixture was built from rather than against DuckDB.
fn expected_serial(wo_id: &str, unit_index: i64) -> String {
    format!("SN-{wo_id}-{unit_index:04}")
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
    assert_eq!(out.dispatch.dispatches, DISPATCH_CASES.len() as u64);
    assert_eq!(
        out.dispatch.wo_part_marks,
        (MARK_CASES.len() + SWEPT_ROWS) as u64
    );
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

/// Both tables cross, the gate passes, and every column read back from SQLite is
/// the value DuckDB held.
///
/// The read-back is done here as well as inside the gate: the gate compares the
/// two sides against each other, whereas the assertions below compare SQLite
/// against the literal constants the fixture was built from — so two sides that
/// were wrong in the same way would still be caught.
#[test]
fn the_dispatch_family_crosses_with_zero_drift() {
    let (_dir, db, lite) = crossed("headline");

    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops.is_empty(),
        "the dispatch family must cross clean: {:?}",
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

    // --- dispatches, against the fixture's own literals ---
    for (dsp_id, wo_id, partner_id, state, shipped, cancelled, notes) in DISPATCH_CASES {
        let get = |col: &str| {
            sqlite_text(
                &lite,
                &format!("SELECT {col} FROM dispatches WHERE dsp_id = '{dsp_id}'"),
            )
        };
        assert_eq!(get("wo_id").as_deref(), Some(*wo_id));
        assert_eq!(get("partner_id").as_deref(), Some(*partner_id));
        assert_eq!(get("state").as_deref(), Some(*state));
        assert_eq!(get("created_at").as_deref(), Some("2026-02-01T09:00:00Z"));
        assert_eq!(get("shipped_at").is_some(), *shipped);
        assert_eq!(get("cancelled_at").is_some(), *cancelled);
        assert_eq!(get("tracking_number").is_some(), *shipped);
        // The empty string is a value, not a NULL. `dsp-04` is the row that
        // distinguishes them and the `typeof` sweep cannot.
        assert_eq!(get("notes").as_deref(), *notes, "notes on {dsp_id}");
    }

    // --- wo_part_marks, against the fixture's own literals ---
    for (wo_id, unit_index, heat_lot) in MARK_CASES {
        let get = |col: &str| {
            sqlite_text(
                &lite,
                &format!(
                    "SELECT {col} FROM wo_part_marks \
                     WHERE tenant_id = '{TENANT}' AND wo_id = '{wo_id}' \
                       AND unit_index = {unit_index}"
                ),
            )
        };
        assert_eq!(
            get("serial_number").as_deref(),
            Some(expected_serial(wo_id, *unit_index).as_str())
        );
        assert_eq!(
            get("part_uid").as_deref(),
            Some(&*format!("UID-{wo_id}-{unit_index}"))
        );
        assert_eq!(get("heat_lot_reference").as_deref(), *heat_lot);
        assert_eq!(
            get("marked_by_operator").as_deref(),
            Some("operator@example.test")
        );
    }
}

// ---------------------------------------------------------------------------
// 2. The composite key
// ---------------------------------------------------------------------------

/// **`unit_index = 0` exists under two different WOs and both rows survive.**
///
/// A migrator that keyed its `BTreeMap` on `unit_index` alone — or on any two of
/// the three components — would collapse them, and every other check in the set
/// would stay green: the row count would match, the `typeof` sweep would pass,
/// and the surviving row would be compared twice. This is the pin that says the
/// key really is all three.
#[test]
fn the_composite_key_really_is_composite() {
    let (_dir, _db, lite) = crossed("composite");

    let at = |wo: &str, unit: i64, col: &str| {
        sqlite_text(
            &lite,
            &format!(
                "SELECT {col} FROM wo_part_marks \
                 WHERE tenant_id = '{TENANT}' AND wo_id = '{wo}' AND unit_index = {unit}"
            ),
        )
    };

    assert_eq!(
        at("wo-01", 0, "part_uid").as_deref(),
        Some("UID-wo-01-0"),
        "wo-01 unit 0 must survive"
    );
    assert_eq!(
        at("wo-02", 0, "part_uid").as_deref(),
        Some("UID-wo-02-0"),
        "wo-02 unit 0 must survive ALONGSIDE wo-01 unit 0"
    );

    let zeros = sqlite_i64(
        &lite,
        "SELECT count(*) FROM wo_part_marks WHERE unit_index = 0",
    );
    assert_eq!(
        zeros, 3,
        "three rows carry unit_index 0 (wo-01, wo-02, wo-sweep); a two-component key would have \
         left fewer"
    );
}

// ---------------------------------------------------------------------------
// 3. §3.2 F — the family's one number
// ---------------------------------------------------------------------------

/// `unit_index` crosses as `'integer'` with its value intact; every other column
/// in the family crosses as `'text'`.
///
/// A `'text'` `unit_index` would order lexicographically — `'10' < '9'` — which
/// on a per-unit ordinal silently reorders a WO's marked units.
#[test]
fn the_ordinal_crosses_as_integer_and_everything_else_as_text() {
    let (_dir, db, lite) = crossed("typing");

    assert_eq!(
        sqlite_typeof(
            &lite,
            "wo_part_marks",
            "unit_index",
            &format!("wo_id = 'wo-02' AND unit_index = {}", i32::MAX)
        ),
        "integer"
    );
    assert_eq!(
        sqlite_i64(
            &lite,
            &format!(
                "SELECT unit_index FROM wo_part_marks WHERE wo_id = 'wo-02' \
                 AND unit_index = {}",
                i32::MAX
            )
        ),
        i32::MAX as i64,
        "DuckDB's INTEGER is 32-bit and SQLite's is 64-bit; the widening must be lossless"
    );
    assert_eq!(
        sqlite_typeof(&lite, "dispatches", "dsp_id", "dsp_id = 'dsp-01'"),
        "text"
    );
    assert_eq!(
        sqlite_typeof(&lite, "dispatches", "notes", "dsp_id = 'dsp-04'"),
        "text",
        "the empty string is TEXT, not NULL"
    );

    // And the gate says the same thing over EVERY row rather than the one this
    // test picked.
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(r
        .checks
        .iter()
        .any(|c| c.contains("typeof(wo_part_marks.unit_index) = 'integer'")));
    assert!(r
        .checks
        .iter()
        .any(|c| c.contains("Σ wo_part_marks.unit_index")));
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
/// only refusal in the family, and it is the only thing between a duplicate
/// composite key and a green gate over a row nobody compared.
///
/// The adversarial table below is measured, not asserted from the docs:
///
/// | input | arm | why |
/// |---|---|---|
/// | distinct keys | accept | the ordinary case |
/// | an adjacent duplicate | **refuse** | `ORDER BY` puts duplicates next to each other |
/// | a non-adjacent duplicate | **refuse** | a `BTreeSet`, not a peek at the previous row |
/// | keys differing only in `unit_index` | accept | the third component is load-bearing |
/// | keys differing only in `wo_id` | accept | the second component is load-bearing |
/// | keys differing only in `tenant_id` | accept | the first component is load-bearing |
/// | `""` vs a real key | accept | the empty key is a key, not a wildcard |
#[test]
fn every_carried_natural_key_either_round_trips_or_is_refused() {
    let mut accepted = 0usize;
    let mut refused = 0usize;

    let cases: &[(&[&str], bool)] = &[
        (&["a#b#0", "a#b#1", "a#c#0"], true),
        (&["a#b#0", "a#b#0"], false),
        (&["a#b#0", "a#b#1", "a#c#9", "a#b#0"], false),
        (&["a#b#0", "a#b#1", "a#b#2"], true),
        (&["a#b#0", "a#c#0", "a#d#0"], true),
        (&["a#b#0", "z#b#0"], true),
        (&["", "a#b#0"], true),
        (&["", ""], false),
    ];

    for (keys, ok) in cases {
        let owned: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
        match unique_natural_keys(&owned, "wo_part_marks") {
            Ok(()) => {
                assert!(ok, "{keys:?} must have been refused");
                accepted += 1;
            }
            Err(e) => {
                assert!(!ok, "{keys:?} must have been accepted, got {e}");
                let msg = e.to_string();
                assert!(msg.contains("wo_part_marks"), "must name the table: {msg}");
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
    assert!(accepted >= 5, "the accept arm must fire: {accepted}");
    assert!(refused >= 3, "the refuse arm must fire: {refused}");
}

/// The same disjunction proved through **real storage**: 256 generated rows
/// seeded into a real DuckDB file, carried by the real migrator into a real
/// SQLite file, and read back byte-for-byte.
///
/// This is the pin the unit test above cannot be: it exercises DuckDB's
/// `VARCHAR` read, the `ToSql` bind, `STRICT`'s acceptance, and SQLite's `TEXT`
/// read-back — four layers where a value could be normalised, truncated or
/// re-encoded without any in-memory check noticing.
#[test]
fn every_swept_row_survives_real_storage_byte_for_byte() {
    let (_dir, db, lite) = crossed("sweep");

    let duck: Vec<(i64, Option<String>, Option<String>)> = {
        let conn = duckdb::Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT unit_index, heat_lot_reference, serial_number FROM wo_part_marks \
                 WHERE wo_id = 'wo-sweep' ORDER BY unit_index ASC",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
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
    let mut stmt = conn
        .prepare(
            "SELECT unit_index, heat_lot_reference, serial_number FROM wo_part_marks \
             WHERE wo_id = 'wo-sweep' ORDER BY unit_index ASC",
        )
        .unwrap();
    let lite_rows: Vec<(i64, Option<String>, Option<String>)> = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(lite_rows.len(), SWEPT_ROWS);
    for (i, (d, l)) in duck.iter().zip(lite_rows.iter()).enumerate() {
        assert_eq!(d.0, l.0, "unit_index drifted on swept row {i}");
        assert_eq!(
            d.1.as_deref().map(str::as_bytes),
            l.1.as_deref().map(str::as_bytes),
            "heat_lot_reference is not byte-identical on swept row {i}: {:?} vs {:?}",
            d.1,
            l.1
        );
        assert_eq!(
            d.2.as_deref().map(str::as_bytes),
            l.2.as_deref().map(str::as_bytes),
            "serial_number is not byte-identical on swept row {i}: {:?} vs {:?}",
            d.2,
            l.2
        );
    }
    // The empty string really was swept, and it came back as `""` and not as
    // `NULL` — the distinction `STRICT` and the typeof sweep are both blind to.
    assert!(
        lite_rows.iter().any(|r| r.1.as_deref() == Some("")),
        "the sweep must include an empty-string heat lot"
    );
}

// ---------------------------------------------------------------------------
// 6. The refusal, through the real migrator
// ---------------------------------------------------------------------------

/// **A duplicate natural key fails the whole carry.**
///
/// Reachable from a real DuckDB source precisely because `wo_part_marks` has no
/// `PRIMARY KEY`, no `UNIQUE` and no index — this test inserts the duplicate the
/// DDL cannot stop, and asserts the migrator refuses rather than producing a
/// database whose row count matches and whose extra row is never compared.
#[test]
fn a_duplicate_natural_key_fails_the_migration() {
    let dir = scratch("dupkey");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        // Same (tenant_id, wo_id, unit_index) as the fixture's `wo-01#1`, with a
        // different part UID — two different parts claiming one unit slot.
        insert_mark(
            &conn,
            TENANT,
            "wo-01",
            1,
            Some("HL-2026-9999"),
            Some("SN-X"),
        );
        conn.close().unwrap();
    }

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let err = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table)
        .expect_err("a duplicate natural key must fail the migration");
    let msg = format!("{err:#}");
    assert!(msg.contains("wo_part_marks"), "must name the table: {msg}");
    assert!(
        msg.contains(&format!("{TENANT}#wo-01#1")),
        "must name the duplicated key: {msg}"
    );
    assert!(
        msg.contains("no PRIMARY KEY"),
        "must say why nothing stopped it: {msg}"
    );
}

// ---------------------------------------------------------------------------
// 7. The schema
// ---------------------------------------------------------------------------

/// `ensure_dispatch_schema` builds both tables, is idempotent, and declares
/// every column with the §3.2 vocabulary.
#[test]
fn ensure_dispatch_schema_builds_both_tables_and_is_idempotent() {
    let dir = scratch("schema");
    let lite = dir.join("aberp.sqlite");
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();

    aberp::migrate_dispatch::ensure_dispatch_schema(&conn).unwrap();
    // Twice: `CREATE TABLE IF NOT EXISTS` is the posture both source schemas
    // use, and a boot that re-runs it must be a no-op rather than an error.
    aberp::migrate_dispatch::ensure_dispatch_schema(&conn).unwrap();

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
    }

    // V001's two indexes carry across; `wo_part_marks` has none, matching its
    // source schema.
    for idx in [
        "dispatches_tenant_state_created_idx",
        "dispatches_tenant_wo_idx",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = ?",
                [idx],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{idx} must be created");
    }
    let marks_idx: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = \
             'wo_part_marks' AND sql IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        marks_idx, 0,
        "the DuckDB table has no index; creating one here would fork the two schemas"
    );

    // A STRICT table must refuse a float into the ordinal — the one typing
    // guarantee this family gets from the engine rather than from Rust.
    let bad = conn.execute(
        "INSERT INTO wo_part_marks (tenant_id, wo_id, unit_index, part_uid, serial_number, \
         data_matrix_payload, heat_lot_reference, marked_at_utc, marked_by_operator) \
         VALUES ('t', 'w', 1.5, 'u', 's', 'd', NULL, 'a', 'o')",
        [],
    );
    assert!(
        bad.is_err(),
        "STRICT must refuse a non-integral REAL in an INTEGER column"
    );
}

// ---------------------------------------------------------------------------
// 8 + 9. The gate's teeth
// ---------------------------------------------------------------------------

/// The gate **hard-stops** per table when a table was not carried.
///
/// Mutation-shaped: this is what the gate does if a future edit drops one arm of
/// the carry from `carry_dispatch`.
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

/// **A single changed column on a single row reds the gate — for EVERY mutable
/// column of the composite-keyed table, one column at a time.** A gate that has
/// never been shown to fail is not a gate (ADR-0107 §4.1).
///
/// `wo_part_marks` is the table that matters here: it is composite-keyed, it has
/// no fold over any of these columns, and the `typeof` sweep reads `'text'`
/// either way — so the per-row arm is the only thing between a rewritten
/// traceability record and a green gate. `tenant_id`, `wo_id` and `unit_index`
/// are excluded because they *are* the key; mutating one produces a missing-row
/// hard stop instead, which the last block below pins separately.
///
/// The mutation is applied to `wo-01#1` in every case, so the `Σ unit_index`
/// fold is provably **unchanged** and cannot be what caught it.
#[test]
fn a_single_changed_column_on_a_composite_keyed_row_reds_the_gate() {
    const MUTABLE: &[&str] = &[
        "part_uid",
        "serial_number",
        "data_matrix_payload",
        "heat_lot_reference",
        "marked_at_utc",
        "marked_by_operator",
    ];

    for col in MUTABLE {
        let (_dir, db, lite) = crossed("drift-mark");
        assert!(
            reconcile(&db, &lite, TENANT).unwrap().hard_stops.is_empty(),
            "the fixture must be green before {col} is mutated"
        );
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            conn.execute_batch(&format!(
                "UPDATE wo_part_marks SET {col} = 'TAMPERED' \
                 WHERE tenant_id = '{TENANT}' AND wo_id = 'wo-01' AND unit_index = 1"
            ))
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains(&format!("part mark {TENANT}#wo-01#1: {col}"))),
            "the gate must name the COMPOSITE key and the column {col}; it reported {:?}",
            r.hard_stops
        );
        assert!(
            r.checks
                .iter()
                .any(|c| c.contains("Σ wo_part_marks.unit_index")),
            "the Σ fold must still agree — that is what makes these mutations the interesting \
             ones: {:?}",
            r.checks
        );
        assert!(
            r.checks
                .iter()
                .any(|c| c.contains("typeof(wo_part_marks.heat_lot_reference) = 'text'")),
            "the typeof sweep is BLIND to this — which is why the per-row arm exists: {:?}",
            r.checks
        );
        assert!(
            !r.checks
                .iter()
                .any(|c| c.contains("every wo_part_marks column round-trips with ZERO drift")),
            "the ZERO-drift check must NOT be emitted alongside a drift hard stop"
        );
    }

    // And a mutation to a KEY component surfaces as a missing row rather than as
    // a column drift — the one shape the loop above cannot produce.
    let (_dir, db, lite) = crossed("drift-markkey");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch(&format!(
            "UPDATE wo_part_marks SET unit_index = 900 \
             WHERE tenant_id = '{TENANT}' AND wo_id = 'wo-01' AND unit_index = 1"
        ))
        .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains(&format!("part mark {TENANT}#wo-01#1 is missing"))),
        "a mutated key component must surface as a missing row: {:?}",
        r.hard_stops
    );
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("Σ wo_part_marks.unit_index")),
        "and the Σ fold catches it a second time, because the ordinal itself moved: {:?}",
        r.hard_stops
    );
}

/// The same, over every mutable column of `dispatches`.
///
/// Included rather than assumed-by-symmetry: `dispatches` has six **nullable**
/// columns, and a `NULL` → value drift is a different comparison from a value →
/// value one. Both directions are run below.
///
/// **Each column is mutated on a row where it actually holds a value**, and the
/// pairing is not cosmetic: `dsp-02` is `shipped`, so its `cancelled_at` is
/// `NULL` and a `SET cancelled_at = NULL` there is a no-op that would have made
/// this loop assert nothing on that column while still passing on the other ten.
/// `dsp-03` is the `cancelled` row and is the only one whose `cancelled_at` can
/// be cleared. (This is not hypothetical: the first draft of this test used
/// `dsp-02` for all eleven and the no-op is what it caught.)
#[test]
fn a_single_changed_column_on_a_dispatch_row_reds_the_gate() {
    /// `(column, row it is populated on)`.
    const MUTABLE: &[(&str, &str)] = &[
        ("tenant_id", "dsp-02"),
        ("wo_id", "dsp-02"),
        ("partner_id", "dsp-02"),
        ("state", "dsp-02"),
        ("created_at", "dsp-02"),
        ("shipped_at", "dsp-02"),
        // The ONLY row with a non-NULL `cancelled_at`.
        ("cancelled_at", "dsp-03"),
        ("carrier_kind", "dsp-02"),
        ("tracking_number", "dsp-02"),
        ("spawned_invoice_id", "dsp-02"),
        ("notes", "dsp-02"),
    ];
    const NOT_NULL: &[&str] = &["tenant_id", "wo_id", "partner_id", "state", "created_at"];

    for (col, row) in MUTABLE {
        let (_dir, db, lite) = crossed("drift-dsp-null");
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            // The nullable columns are cleared to NULL, the NOT NULL ones are
            // rewritten — so the value → NULL arm of the comparison runs six
            // times and the value → value arm five.
            let set = if NOT_NULL.contains(col) {
                "'TAMPERED'"
            } else {
                "NULL"
            };
            // The mutation must actually change a row; a no-op UPDATE would make
            // the assertion below vacuous.
            let before = sqlite_i64(
                &lite,
                &format!(
                    "SELECT count(*) FROM dispatches WHERE dsp_id = '{row}' AND {col} IS NOT NULL"
                ),
            );
            assert_eq!(before, 1, "{col} must be populated on {row} to be mutated");
            conn.execute_batch(&format!(
                "UPDATE dispatches SET {col} = {set} WHERE dsp_id = '{row}'"
            ))
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains(&format!("dispatch {row}: {col}"))),
            "the gate must name the row and the column {col}; it reported {:?}",
            r.hard_stops
        );
    }

    // The other direction: `dsp-01`'s NULLs become values. A comparison written
    // as "both non-null and different" would miss every one of these.
    for col in ["shipped_at", "cancelled_at", "carrier_kind", "notes"] {
        let (_dir, db, lite) = crossed("drift-dsp-value");
        {
            let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
            conn.execute_batch(&format!(
                "UPDATE dispatches SET {col} = 'INVENTED' WHERE dsp_id = 'dsp-01'"
            ))
            .unwrap();
        }
        let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
        assert!(
            r.hard_stops
                .iter()
                .any(|s| s.contains(&format!("dispatch dsp-01: {col}"))),
            "a NULL that became a value must red the gate on {col}: {:?}",
            r.hard_stops
        );
    }

    // And the empty string turning into a NULL — the drift `STRICT` and the
    // typeof sweep are both structurally blind to.
    let (_dir, db, lite) = crossed("drift-dsp-empty");
    {
        let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
        conn.execute_batch("UPDATE dispatches SET notes = NULL WHERE dsp_id = 'dsp-04'")
            .unwrap();
    }
    let r = reconcile(&db, &lite, TENANT).expect("reconcile runs");
    assert!(
        r.hard_stops
            .iter()
            .any(|s| s.contains("dispatch dsp-04: notes")),
        "\"\" → NULL must red the gate; nothing else in the set can see it: {:?}",
        r.hard_stops
    );
}

// ---------------------------------------------------------------------------
// 10 + 11. The legitimate partial shapes
// ---------------------------------------------------------------------------

/// A source with neither of the family's tables is a legitimate shape, and the
/// gate says so out loud rather than staying silent.
#[test]
fn a_source_without_the_family_reports_the_absence_rather_than_staying_silent() {
    let dir = scratch("absent");
    let db = dir.join("aberp.duckdb");
    seed_ledger(&db);

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.dispatch, Default::default());

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a source with no dispatches and no part marks is legitimate: {:?}",
        r.hard_stops
    );
    assert!(
        r.checks
            .iter()
            .any(|c| c.contains("dispatch family absent on BOTH sides")),
        "the absence must be REPORTED, not silent; checks: {:?}",
        r.checks
    );
}

/// **A pre-S432 source has `dispatches` and no `wo_part_marks`, and that is a
/// legitimate shape** — which is why presence is held per table rather than per
/// family.
///
/// Without this, a per-family answer would say "the family is present" and then
/// hard-stop on `wo_part_marks`, refusing to migrate a database whose only sin
/// is predating heat-lot traceability.
#[test]
fn a_pre_s432_source_without_part_marks_still_crosses() {
    let dir = scratch("premarks");
    let db = seed(&dir);
    {
        let conn = duckdb::Connection::open(&db).unwrap();
        conn.execute_batch("DROP TABLE wo_part_marks;").unwrap();
        conn.close().unwrap();
    }

    let snap = run_snapshot(&db, TENANT, None).unwrap();
    let lite = dir.join("aberp.sqlite");
    let out = migrate_families(&db, &lite, TENANT, &snap, LedgerSource::Table).expect("migrate");
    assert_eq!(out.dispatch.dispatches, DISPATCH_CASES.len() as u64);
    assert_eq!(out.dispatch.wo_part_marks, 0);

    let r = reconcile(&db, &lite, TENANT).expect("reconcile");
    assert!(
        r.hard_stops.is_empty(),
        "a pre-S432 source is legitimate, not an asymmetry: {:?}",
        r.hard_stops
    );

    // And the absent table really is absent, not silently created.
    let conn = aberp_db::sqlite::open_hardened(&lite).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'wo_part_marks'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        n, 0,
        "creating an empty wo_part_marks the source does not have would manufacture the \
         asymmetry the gate exists to detect"
    );
}
